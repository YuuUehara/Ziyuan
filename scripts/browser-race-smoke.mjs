#!/usr/bin/env node
// 双页面同基线保存竞争：A、B 读取同一草稿，A 先保存，B 以旧 revision 保存应 409。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const args = new Map();
for (let i = 2; i < process.argv.length; i += 2) args.set(process.argv[i], process.argv[i + 1]);
const root = args.get("--root");
const bin = args.get("--bin");
const port = Number(args.get("--port") || 18120);
const out = args.get("--out") || path.join(os.tmpdir(), "ziyuan-race-smoke.png");
if (!root || !bin) { console.error("用法: node scripts/browser-race-smoke.mjs --root <工作区> --bin <ziyuan.exe> [--port] [--out]"); process.exit(2); }

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
async function connect(wsUrl) {
  const ws = new WebSocket(wsUrl);
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
  return { send, evaluate };
}
try {
  const tokenPath = path.join(root, ".ziyuan", "web-token");
  const token = await waitFor(() => fs.existsSync(tokenPath) && fs.readFileSync(tokenPath, "utf8").trim(), 15000, "web-token");
  const base = "http://127.0.0.1:" + port;
  await waitFor(async () => (await fetch(base + "/")).status === 200, 15000, "Web 服务就绪");

  const profile = fs.mkdtempSync(path.join(os.tmpdir(), "ziyuan-race-chrome-"));
  chrome = spawn("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe", [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--user-data-dir=" + profile, "--remote-debugging-port=0", "about:blank",
  ], { stdio: "ignore" });
  const active = path.join(profile, "DevToolsActivePort");
  const lines = (await waitFor(() => fs.existsSync(active) && fs.readFileSync(active, "utf8").trim(), 15000, "Chrome CDP 端口")).split(/\r?\n/);
  const cdpPort = Number(lines[0]);
  const targets = await waitFor(async () => (await (await fetch("http://127.0.0.1:" + cdpPort + "/json")).json()), 10000, "Chrome targets");
  const pageA = targets.find((t) => t.type === "page");
  const a = await connect(pageA.webSocketDebuggerUrl);
  await a.send("Page.enable");
  await a.send("Runtime.enable");
  await a.send("Page.navigate", { url: base + "/" });
  await waitFor(async () => !!(await a.evaluate("document.readyState === 'complete'")), 15000, "A 首页");
  await waitFor(async () => !!(await a.evaluate("!!document.querySelector('#login-token')")), 10000, "A 登录");
  await a.evaluate("document.querySelector('#login-token').value = " + JSON.stringify(token) + "; document.querySelector('#login-form').requestSubmit();");
  await sleep(1200);
  await a.evaluate("document.querySelector('[data-hall=\"wuying\"]').click()");
  await sleep(800);
  await waitFor(async () => !!(await a.evaluate("document.querySelectorAll('#text-project option').length > 0")), 20000, "A 项目");
  await a.evaluate("document.querySelector('#materials-load').click()");
  await waitFor(async () => !!(await a.evaluate("!!document.querySelector('#text-materials-editor .materials-editor')")), 20000, "A 编辑器");

  // B：同 profile 新页面，cookie 共享。
  const created = await (await fetch("http://127.0.0.1:" + cdpPort + "/json/new?about:blank", { method: "PUT" })).json();
  const b = await connect(created.webSocketDebuggerUrl);
  await b.send("Page.enable");
  await b.send("Runtime.enable");
  await b.send("Page.navigate", { url: base + "/" });
  await waitFor(async () => !!(await b.evaluate("document.readyState === 'complete'")), 15000, "B 首页");
  await sleep(1200);
  await b.evaluate("document.querySelector('[data-hall=\"wuying\"]').click()");
  await sleep(800);
  await waitFor(async () => !!(await b.evaluate("document.querySelectorAll('#text-project option').length > 0")), 20000, "B 项目");
  await b.evaluate("document.querySelector('#materials-load').click()");
  await waitFor(async () => !!(await b.evaluate("!!document.querySelector('#text-materials-editor .materials-editor')")), 20000, "B 编辑器");

  const add = async (page, id, source) => {
    await page.evaluate("document.querySelector('#text-materials-editor [data-role=\"add\"]').click()");
    await sleep(300);
    await page.evaluate("(() => { const set = (s, v) => { const i = document.querySelector(s); i.value = v; i.dispatchEvent(new Event('input', { bubbles: true })); }; set('[data-field=\"term_id\"]', " + JSON.stringify(id) + "); set('[data-field=\"source\"]', " + JSON.stringify(source) + "); document.querySelector('[data-add=\"allowed\"]').click(); })()");
    await sleep(300);
    await page.evaluate("(() => { const el = document.querySelector('[data-field=\"allowed.0\"]'); el.value = " + JSON.stringify(source) + "; el.dispatchEvent(new Event('input', { bubbles: true })); })()");
  };
  await add(a, "race-a", "甲");
  await a.evaluate("document.querySelector('#materials-save').click()");
  await waitFor(async () => !!(await a.evaluate("document.querySelector('#materials-status').textContent.includes('已保存') || document.querySelector('#materials-status').textContent.includes('保存失败')")), 20000, "A 保存");
  const aSave = await a.evaluate("document.querySelector('#materials-status').textContent");

  await add(b, "race-b", "乙");
  await b.evaluate("document.querySelector('#materials-save').click()");
  await waitFor(async () => !!(await b.evaluate("document.querySelector('#materials-status').textContent.includes('保存失败') || document.querySelector('#materials-status').textContent.includes('已保存')")), 20000, "B 保存");
  const bSave = await b.evaluate("document.querySelector('#materials-status').textContent");
  const bKept = await b.evaluate("document.querySelector('[data-field=\"term_id\"]')?.value || ''");

  await a.evaluate("document.querySelector('#materials-refresh').click()");
  await sleep(800);
  const aTerms = await a.evaluate("document.querySelector('#text-materials-editor').innerText");
  const shot = await b.send("Page.captureScreenshot", { format: "png" });
  fs.writeFileSync(out, Buffer.from(shot.data, "base64"));
  console.log(JSON.stringify({ ok: true, aSave, bSave, bKept, aHasA: aTerms.includes("race-a"), aHasB: aTerms.includes("race-b"), screenshot: out }, null, 2));
} catch (error) {
  console.error("双页面竞争烟测失败：" + error.message);
  if (serverErr) console.error(serverErr.slice(-2000));
  process.exitCode = 1;
} finally {
  try { chrome?.kill(); } catch {}
  try { server.kill(); } catch {}
  await sleep(200);
}
