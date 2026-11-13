#!/usr/bin/env node
// 观政（只读）浏览器验证：serve --viewer，用 viewer token 登录，确认保存被拒、编辑器只读。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const args = new Map();
for (let i = 2; i < process.argv.length; i += 2) args.set(process.argv[i], process.argv[i + 1]);
const root = args.get("--root");
const bin = args.get("--bin");
const port = Number(args.get("--port") || 18110);
const out = args.get("--out") || path.join(os.tmpdir(), "ziyuan-viewer-smoke.png");
if (!root || !bin) { console.error("用法: node scripts/browser-viewer-smoke.mjs --root <工作区> --bin <ziyuan.exe> [--port] [--out]"); process.exit(2); }

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function waitFor(fn, timeout = 20000, label = "条件") {
  const end = Date.now() + timeout;
  let last;
  while (Date.now() < end) {
    try { const value = await fn(); if (value) return value; } catch (e) { last = e; }
    await sleep(150);
  }
  throw new Error(label + " 超时" + (last ? "：" + last.message : ""));
}

const server = spawn(bin, ["serve", "--port", String(port), "--bind", "127.0.0.1", "--viewer"], { cwd: root, stdio: ["ignore", "pipe", "pipe"] });
let serverErr = "";
server.stderr.on("data", (d) => { serverErr += d.toString(); });
let chrome;
try {
  const tokenPath = path.join(root, ".ziyuan", "web-viewer-token");
  const token = await waitFor(() => fs.existsSync(tokenPath) && fs.readFileSync(tokenPath, "utf8").trim(), 15000, "viewer token");
  const base = "http://127.0.0.1:" + port;
  await waitFor(async () => (await fetch(base + "/")).status === 200, 15000, "Web 服务就绪");

  const profile = fs.mkdtempSync(path.join(os.tmpdir(), "ziyuan-viewer-chrome-"));
  chrome = spawn("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe", [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--user-data-dir=" + profile, "--remote-debugging-port=0", "about:blank",
  ], { stdio: "ignore" });
  const active = path.join(profile, "DevToolsActivePort");
  const lines = (await waitFor(() => fs.existsSync(active) && fs.readFileSync(active, "utf8").trim(), 15000, "Chrome CDP 端口")).split(/\r?\n/);
  const cdpPort = Number(lines[0]);
  const target = await waitFor(async () => (await (await fetch("http://127.0.0.1:" + cdpPort + "/json")).json()).find((t) => t.type === "page"), 10000, "页面 target");
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  const pending = new Map();
  let nextId = 1;
  ws.onmessage = (event) => {
    const message = JSON.parse(event.data);
    if (message.id && pending.has(message.id)) {
      const { resolve, reject } = pending.get(message.id);
      pending.delete(message.id);
      message.error ? reject(new Error(message.error.message)) : resolve(message.result);
    }
  };
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  const send = (method, params = {}) => new Promise((resolve, reject) => {
    const id = nextId++;
    pending.set(id, { resolve, reject });
    ws.send(JSON.stringify({ id, method, params }));
  });
  const evaluate = async (expression) => {
    const result = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text || "页面脚本异常");
    return result.result?.value;
  };
  const waitJs = (expression, timeout, label) => waitFor(async () => !!(await evaluate(expression)), timeout, label);

  await send("Page.enable");
  await send("Runtime.enable");
  await send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 1000, deviceScaleFactor: 1, mobile: false });
  await send("Page.navigate", { url: base + "/" });
  await waitJs("document.readyState === 'complete'", 15000, "首页加载");
  await waitJs("!!document.querySelector('#login-token')", 10000, "登录表单");
  await evaluate("document.querySelector('#login-token').value = " + JSON.stringify(token) + "; document.querySelector('#login-form').requestSubmit();");
  await sleep(900);
  await evaluate("document.querySelector('[data-hall=\"translation\"]').click()");
  await sleep(900);
  await waitJs("document.querySelectorAll('#text-project option').length > 0", 20000, "项目下拉");
  await evaluate("document.querySelector('[data-workspace-tab=materials]').click();document.querySelector('#materials-load').click()");
  await waitJs("!!document.querySelector('#text-materials-editor .materials-editor')", 20000, "编辑器挂载");

  const viewerState = await evaluate("JSON.stringify({ viewer: !!state?.viewer, addHidden: !!document.querySelector('#text-materials-editor [data-role=\"add\"]')?.hidden, readOnly: [...document.querySelectorAll('#text-materials-editor input')].every((i) => i.readOnly || i.disabled), savePresent: !!document.querySelector('#materials-save') })");
  await evaluate("document.querySelector('#materials-save').click()");
  await sleep(400);
  const viewerSave = await evaluate("document.querySelector('#materials-status').textContent");
  const shot = await send("Page.captureScreenshot", { format: "png" });
  fs.writeFileSync(out, Buffer.from(shot.data, "base64"));
  console.log(JSON.stringify({ ok: true, viewerState, viewerSave, screenshot: out }, null, 2));
} catch (error) {
  console.error("观政烟测失败：" + error.message);
  if (serverErr) console.error(serverErr.slice(-2000));
  process.exitCode = 1;
} finally {
  try { chrome?.kill(); } catch {}
  try { server.kill(); } catch {}
  await sleep(200);
}
