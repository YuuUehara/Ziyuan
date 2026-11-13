#!/usr/bin/env node
// 真实浏览器烟测：启动 ziyuan serve，用本机 Chrome 走 CDP，登录后打开“本地资料”入口并截图。
// 零 npm 依赖；只用 Node 内置 fetch / WebSocket 与系统 Chrome。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const args = new Map();
for (let i = 2; i < process.argv.length; i += 2) args.set(process.argv[i], process.argv[i + 1]);
const root = args.get("--root");
const bin = args.get("--bin");
const port = Number(args.get("--port") || 18080);
const out = args.get("--out") || path.join(os.tmpdir(), "ziyuan-browser-smoke.png");
if (!root || !bin) { console.error("用法: node scripts/browser-smoke.mjs --root <工作区> --bin <ziyuan.exe> [--port 18080] [--out shot.png]"); process.exit(2); }

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
server.stderr.on("data", (d) => { serverErr += d.toString(); });
let chrome;
try {
  const tokenPath = path.join(root, ".ziyuan", "web-token");
  const token = await waitFor(() => fs.existsSync(tokenPath) && fs.readFileSync(tokenPath, "utf8").trim(), 15000, "web-token 文件");
  const base = "http://127.0.0.1:" + port;
  await waitFor(async () => (await fetch(base + "/")).status === 200, 15000, "Web 服务就绪");

  const profile = fs.mkdtempSync(path.join(os.tmpdir(), "ziyuan-chrome-"));
  chrome = spawn("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe", [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--user-data-dir=" + profile, "--remote-debugging-port=0", "about:blank",
  ], { stdio: "ignore" });
  const active = path.join(profile, "DevToolsActivePort");
  const lines = (await waitFor(() => fs.existsSync(active) && fs.readFileSync(active, "utf8").trim(), 15000, "Chrome CDP 端口")).split(/\r?\n/);
  const cdpPort = Number(lines[0]);
  const targets = await waitFor(async () => (await (await fetch("http://127.0.0.1:" + cdpPort + "/json")).json()).find((t) => t.type === "page"), 10000, "Chrome 页面 target");
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
  const waitJs = (expression, timeout, label) => waitFor(async () => {
    const value = await evaluate(expression);
    return value || false;
  }, timeout, label);

  await send("Page.enable");
  await send("Runtime.enable");
  await send("Emulation.setDeviceMetricsOverride", { width: 1440, height: 1000, deviceScaleFactor: 1, mobile: false });
  await send("Page.navigate", { url: base + "/" });
  await waitJs("document.readyState === 'complete'", 15000, "首页加载");

  // 登录：令牌输入框只读由服务器种 Cookie；页面脚本不经手 token，这里模拟用户填写。
  await waitJs("!!document.querySelector('#login-token')", 10000, "登录表单");
  await evaluate("document.querySelector('#login-token').value = " + JSON.stringify(token) + "; document.querySelector('#login-form').requestSubmit();");
  await evaluate("document.querySelector('[data-hall=\\'translation\\']').click()");
  await sleep(1200);
  console.error("DEBUG after login:", await evaluate("JSON.stringify({hall: location.hash, opts: document.querySelectorAll('#text-project option').length, state: typeof state})"));
  await waitJs("!!document.querySelector('#text-project') && document.querySelectorAll('#text-project option').length > 0", 20000, "登录后项目下拉");
  const assocCheck = await evaluate(`(async () => {
    const r = await fetch("/api/text/" + encodeURIComponent(textProjectId) + "/blocks/" + encodeURIComponent(textBlockId) + "/verification/associations?associations=" + encodeURIComponent("[]"));
    return JSON.stringify({ status: r.status, url: r.url });
  })()`);
  const project = await evaluate("document.querySelector('#text-project').value");
  if (!project) throw new Error("没有可选项目");
  await evaluate("const s = document.querySelector('#text-project'); s.value = " + JSON.stringify(project) + "; s.dispatchEvent(new Event('change', { bubbles: true }));");
  await sleep(1000);
  const projectId = await evaluate("document.querySelector('#text-project').value");
  const blockId = await evaluate("textBlockId");
  const view = await evaluate(`(async () => {
    const v = await api("/text/" + encodeURIComponent(textProjectId) + "/materials");
    const material = {
      format: 2,
      terms: [{ term_id: "t-1", source: "夜", category: "common", allowed: ["勇士"], scope: { kind: "project" }, evidence: [], status: "confirmed" }],
      persons: [{
        person_id: "p_1", name: "夜", aliases: ["夜晚"], terms: [],
        address_rules: [{ target: "p_1", speaker: { kind: "person", person_id: "p_1" }, scope: { kind: "project" }, term: "t-1", evidence: [] }],
        voice: { description: "庄重" }, evidence: [], status: "confirmed"
      }]
    };
    await api("/text/" + encodeURIComponent(textProjectId) + "/materials/draft", {
      workspace_id: v.workspace_id, request_id: "assoc-e2e-" + Date.now(), expected_revision: v.draft ? v.draft.revision : null, material
    });
    await api("/text/" + encodeURIComponent(textProjectId) + "/materials/prepare", {});
    await api("/text/" + encodeURIComponent(textProjectId) + "/context/approve", {});
    return JSON.stringify({ workspace: v.workspace_id, block: textBlockId });
  })()`);
  const blockView = await evaluate(`(async () => JSON.stringify(await api("/text/" + encodeURIComponent(textProjectId) + "/blocks/" + encodeURIComponent(textBlockId))))()`);
  const source = JSON.parse(blockView).source.paragraphs;
  const zh = { schema_version: 1, block_id: blockId, paragraphs: source.map((p) => ({ id: p.id, text: "译文没有允许译法。" })) };
  const blockDir = path.join(root, "translations", projectId, blockId);
  fs.mkdirSync(blockDir, { recursive: true });
  fs.writeFileSync(path.join(blockDir, "zh.json"), JSON.stringify(zh, null, 2) + "\n");
  await send("Page.navigate", { url: base + "/#wuying/text/" + projectId + "/" + blockId });
  await waitJs("document.readyState === 'complete'", 15000, "重载");
  await sleep(2000);
  await evaluate("document.querySelector('[data-workspace-tab=materials]').click();document.querySelector('#materials-load').click()");
  await waitJs("!!document.querySelector('#text-materials-editor .materials-editor')", 20000, "资料读取");
  await evaluate(`(() => {
    const set = (field, value) => {
      const input = document.querySelector('[data-field="' + field + '"]');
      if (!input) throw new Error('missing ' + field);
      input.value = value;
      input.dispatchEvent(new Event('input', { bubbles: true }));
    };
    set('persons', 'p_1'); set('targets', 'p_1'); set('speaker', 'p_1');
  })()`);
  await waitJs("!!document.querySelector('#verification-reload')", 20000, "复核面板");
  await evaluate("document.querySelector('#verification-reload').click()");
  await waitJs("!!verificationView", 15000, "复核视图");
  await waitJs("Array.isArray(verificationView.hints) && verificationView.hints.some((h) => h.message.includes('第二版称谓'))", 20000, "称谓提示");
  const hints = await evaluate("JSON.stringify(verificationView.hints.map((h) => h.message))");
  console.log(JSON.stringify({ ok: true, projectId, blockId, prepare: JSON.parse(view).workspace ? true : false, hints: JSON.parse(hints) }));
} catch (e) {
  console.error(e.stack || e.message);
  process.exitCode = 1;
} finally {
  try { chrome?.kill(); } catch {}
  try { server.kill(); } catch {}
}
