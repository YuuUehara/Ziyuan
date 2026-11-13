#!/usr/bin/env node
// 1.13 版本面板真实浏览器烟测：登录 → 译书 → 选项目 → 等快照 → 对比 → 截图（含窄屏）。
// 用法: node scripts/browser-versions-smoke.mjs <工作区> <ziyuan.exe> <输出.png> [端口]
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const root = process.argv[2];
const bin = process.argv[3];
const out = process.argv[4];
const port = Number(process.argv[5] || 18083);
if (!root || !bin || !out) {
  console.error("用法: node scripts/browser-versions-smoke.mjs <root> <ziyuan.exe> <out.png> [port]");
  process.exit(2);
}
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
const server = spawn(bin, ["serve", "--port", String(port), "--bind", "127.0.0.1"], { cwd: root, stdio: ["ignore", "pipe", "pipe"] });
let serverErr = "";
server.stderr.on("data", (d) => (serverErr += d.toString()));
let chrome;
try {
  const tokenPath = path.join(root, ".ziyuan", "web-token");
  const token = await waitFor(() => fs.existsSync(tokenPath) && fs.readFileSync(tokenPath, "utf8").trim(), 15000, "web-token");
  const base = "http://127.0.0.1:" + port;
  await waitFor(async () => (await fetch(base + "/")).status === 200, 15000, "Web 服务就绪");
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), "ziyuan-version-chrome-"));
  chrome = spawn("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe", [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--user-data-dir=" + profile, "--remote-debugging-port=0", "about:blank",
  ], { stdio: "ignore" });
  const active = path.join(profile, "DevToolsActivePort");
  const lines = (await waitFor(() => fs.existsSync(active) && fs.readFileSync(active, "utf8").trim(), 15000, "CDP 端口")).split(/\r?\n/);
  const cdpPort = Number(lines[0]);
  const targets = await waitFor(async () => (await (await fetch("http://127.0.0.1:" + cdpPort + "/json")).json()).find((t) => t.type === "page"), 10000, "页面 target");
  const ws = new WebSocket(targets.webSocketDebuggerUrl);
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
  const waitJs = (expression, timeout, label) => waitFor(async () => (await evaluate(expression)) || false, timeout, label);

  await send("Page.enable");
  await send("Runtime.enable");
  await send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 1000, deviceScaleFactor: 1, mobile: false });
  await send("Page.navigate", { url: base + "/" });
  await waitJs("document.readyState === 'complete'", 15000, "首页");
  await waitJs("!!document.querySelector('#login-token')", 10000, "登录表单");
  await evaluate("document.querySelector('#login-token').value = " + JSON.stringify(token) + "; document.querySelector('#login-form').requestSubmit();");
  await evaluate("document.querySelector('[data-hall=\\'translation\\']').click()");
  await waitJs("!!document.querySelector('#text-project') && document.querySelectorAll('#text-project option').length > 0", 20000, "项目下拉");
  await evaluate("(() => { const s = document.querySelector('#text-project'); s.value = s.options[0].value; s.dispatchEvent(new Event('change', { bubbles: true })); })()");
  await waitJs("document.querySelectorAll('#version-list option').length > 0 && document.querySelector('#version-list option').value !== ''", 20000, "版本列表");
  const listed = await evaluate("document.querySelector('#version-list').innerText");
  await evaluate("document.querySelector('[data-workspace-tab=manage]').click();document.querySelector('#version-compare-current').click()");
  await waitJs("document.querySelector('#version-diff').children.length > 0", 20000, "对比结果");
  const diff = await evaluate("document.querySelector('#version-diff').innerText.slice(0, 500)");
  const status = await evaluate("document.querySelector('#version-status').innerText");
  await evaluate("document.querySelector('#text-versions').scrollIntoView({ block: 'center' })");
  await sleep(300);
  const shot = await send("Page.captureScreenshot", { format: "png" });
  fs.writeFileSync(out, Buffer.from(shot.data, "base64"));
  await send("Emulation.setDeviceMetricsOverride", { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await sleep(400);
  await evaluate("document.querySelector('#text-versions').scrollIntoView({ block: 'center' })");
  await sleep(300);
  const narrow = await evaluate("JSON.stringify({ overflow: document.documentElement.scrollWidth > window.innerWidth + 1, panel: !!document.querySelector('#text-versions')?.getBoundingClientRect().width, diff: document.querySelector('#version-diff').children.length })");
  const narrowShot = await send("Page.captureScreenshot", { format: "png" });
  fs.writeFileSync(out.replace(/\.png$/, "-narrow.png"), Buffer.from(narrowShot.data, "base64"));
  const overflow = await evaluate("document.documentElement.scrollWidth > window.innerWidth + 1");
  console.log(JSON.stringify({ ok: true, listed, status, diff, narrow, overflow, screenshot: out }, null, 2));
} catch (error) {
  console.error("版本面板浏览器检查失败：" + error.message);
  if (serverErr) console.error(serverErr.slice(-2000));
  process.exitCode = 1;
} finally {
  try { chrome?.kill(); } catch {}
  try { server.kill(); } catch {}
  await sleep(200);
}
