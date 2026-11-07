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
  await evaluate("document.querySelector('[data-hall=\\'wuying\\']').click()");
  await sleep(1200);
  console.error("DEBUG after login:", await evaluate("JSON.stringify({hall: location.hash, opts: document.querySelectorAll('#text-project option').length, state: typeof state})"));
  await waitJs("!!document.querySelector('#text-project') && document.querySelectorAll('#text-project option').length > 0", 20000, "登录后项目下拉");
  const project = await evaluate("document.querySelector('#text-project').value");
  if (!project) throw new Error("没有可选项目");
  await evaluate("const s = document.querySelector('#text-project'); s.value = " + JSON.stringify(project) + "; s.dispatchEvent(new Event('change', { bubbles: true }));");
  await sleep(1000);
  await waitJs("!!document.querySelector('#materials-load')", 10000, "本地资料入口");
  await evaluate("document.querySelector('#materials-load').click()");
  await waitJs("!!document.querySelector('#text-materials-editor .materials-editor')", 20000, "本地资料读取");
  const status = await evaluate("document.querySelector('#materials-status').textContent");
  await evaluate("document.querySelector('#materials-validate').click()");
  await waitJs("['校验通过','校验未通过','校验失败'].some((s) => document.querySelector('#materials-status').textContent.includes(s))", 20000, "校验结果");
  const validateStatus = await evaluate("document.querySelector('#materials-status').textContent");
  // 新增一条术语并保存，再预览当前块。
  await evaluate("document.querySelector('#text-materials-editor [data-role=\"add\"]').click()");
  await sleep(300);
  await evaluate(`(() => {
    const set = (selector, value) => {
      const input = document.querySelector(selector);
      if (!input) throw new Error('找不到字段 ' + selector);
      input.value = value;
      input.dispatchEvent(new Event('input', { bubbles: true }));
    };
    set('[data-field="term_id"]', 'browser-smoke-term');
    set('[data-field="source"]', '勇者');
    document.querySelector('[data-add="allowed"]').click();
  })()`);
  await sleep(300);
  await evaluate("(() => { const el = document.querySelector('[data-field=\"allowed.0\"]'); el.value = '勇者'; el.dispatchEvent(new Event('input', { bubbles: true })); })()");
  await evaluate("document.querySelector('#materials-save').click()");
  await waitJs("document.querySelector('#materials-status').textContent.includes('已保存') || document.querySelector('#materials-status').textContent.includes('保存失败')", 20000, "保存结果");
  const saveStatus = await evaluate("document.querySelector('#materials-status').textContent");
  await evaluate("document.querySelector('#materials-preview-run').click()");
  await waitJs("document.querySelector('#materials-status').textContent.includes('预览只表示') || document.querySelector('#materials-status').textContent.includes('预览失败')", 20000, "预览结果");
  const previewStatus = await evaluate("document.querySelector('#materials-status').textContent");
  const report = await evaluate("document.querySelector('#materials-preview').innerText.slice(0, 500)");
  await evaluate("document.querySelector('#materials-prepare').click()");
  await waitJs("document.querySelector('#materials-status').textContent.includes('已准备生产资料') || document.querySelector('#materials-status').textContent.includes('准备失败')", 20000, "准备生产资料");
  const prepareStatus = await evaluate("document.querySelector('#materials-status').textContent");
  await evaluate("renderTextNow()");
  await waitJs("document.querySelector('#text-n2b-hints').textContent.includes('术语：') || document.querySelector('#text-n2b-hints').textContent.includes('没有适用')", 20000, "段旁提示结果");
  const n2bHints = await evaluate("document.querySelector('#text-n2b-hints').textContent");
  // 焦点保持：在临时关联输入后刷新，稳定 DOM 不应重建该行或抢走焦点。
  await evaluate("(() => { const input = document.querySelector('#materials-associations input[data-field=\"persons\"]'); input.focus(); input.value = 'p1'; input.dispatchEvent(new Event('input', { bubbles: true })); })()");
  await evaluate("loadMaterialsView()");
  await sleep(900);
  await waitJs("!!document.querySelector('#text-materials-editor .materials-editor')", 20000, "刷新后编辑器");
  const focusKept = await evaluate("document.activeElement?.dataset?.field === 'persons' && document.activeElement.value === 'p1'");
  const states = await evaluate("JSON.stringify({ save: document.querySelector('#materials-save-state').textContent, session: document.querySelector('#materials-session-state').textContent, validate: document.querySelector('#materials-validate-state').textContent })");
  // 会话恢复：新增未保存术语 → 整页刷新 → 恢复按钮 → 点击恢复。
  await evaluate("document.querySelector('#text-materials-editor [data-role=\"add\"]').click()");
  await sleep(300);
  await evaluate("(() => { const set = (selector, value) => { const input = document.querySelector(selector); input.value = value; input.dispatchEvent(new Event('input', { bubbles: true })); }; set('[data-field=\"term_id\"]', 'browser-smoke-recover'); set('[data-field=\"source\"]', '恢复测试'); document.querySelector('[data-add=\"allowed\"]').click(); })()");
  await sleep(300);
  await evaluate("(() => { const el = document.querySelector('[data-field=\"allowed.0\"]'); el.value = '恢复测试'; el.dispatchEvent(new Event('input', { bubbles: true })); })()");
  await send("Page.reload", { ignoreCache: true });
  await sleep(1200);
  await waitJs("document.readyState === 'complete'", 15000, "刷新加载");
  await evaluate("document.querySelector('[data-hall=\"wuying\"]')?.click()");
  await sleep(900);
  await evaluate("document.querySelector('#materials-load')?.click()");
  await waitJs("!document.querySelector('#materials-restore').hidden", 20000, "恢复按钮");
  const restoreVisible = true;
  await evaluate("document.querySelector('#materials-restore').click()");
  await waitJs("document.querySelector('#materials-status').textContent.includes('已恢复会话暂存')", 15000, "恢复会话");
  const recovered = await evaluate("document.querySelector('[data-field=\"term_id\"]')?.value || ''");
  const recoveredContains = await evaluate("document.querySelector('#text-materials-editor').innerText.includes('browser-smoke-recover')");
  // 409：页面外保存空资料推进 revision，再在页面保存本地术语，应冲突并保留输入。
  await sleep(500);
  await evaluate("document.querySelector('#text-materials-editor [data-role=\"add\"]').click()");
  await sleep(300);
  await evaluate("(() => { const set = (selector, value) => { const input = document.querySelector(selector); input.value = value; input.dispatchEvent(new Event('input', { bubbles: true })); }; set('[data-field=\"term_id\"]', 'browser-smoke-term-2'); set('[data-field=\"source\"]', '勇者二'); document.querySelector('[data-add=\"allowed\"]').click(); })()");
  await sleep(300);
  await evaluate("(() => { const el = document.querySelector('[data-field=\"allowed.0\"]'); el.value = '勇者二'; el.dispatchEvent(new Event('input', { bubbles: true })); })()");
  const externalSave = await evaluate("(async () => { const snap = materialsEditor.previewState(); const response = await fetch('/api/text/' + encodeURIComponent(snap.project_id) + '/materials/draft', { method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({ workspace_id: snap.workspace_id, request_id: 'smoke-empty-' + Date.now(), expected_revision: snap.revision, material: { format: 2, terms: [], persons: [] } }) }); return { status: response.status, body: (await response.text()).slice(0, 300) }; })()");
  console.error('DEBUG external save:', JSON.stringify(externalSave));
  if (externalSave.status !== 200) throw new Error('外部保存失败 ' + externalSave.status);
  await evaluate("document.querySelector('#materials-save').click()");
  await waitJs("document.querySelector('#materials-status').textContent.includes('保存失败')", 20000, "409 保存失败");
  const conflictStatus = await evaluate("document.querySelector('#materials-status').textContent");
  const conflictKept = await evaluate("document.querySelector('[data-field=\"term_id\"]')?.value || ''");
  const discardVisible = await evaluate("!document.querySelector('#materials-discard').hidden");
  await evaluate("document.querySelector('#materials-discard').click()");
  await waitJs("document.querySelector('#materials-status').textContent.includes('已放弃本地')", 20000, "放弃本地");
  const afterDiscard = await evaluate("document.querySelector('#text-materials-editor').innerText.includes('browser-smoke-term-2')");
  const editor = await evaluate("!!document.querySelector('#text-materials-editor .materials-editor') || document.querySelector('#text-materials-editor').textContent");
  await evaluate("document.querySelector('#text-materials-panel').scrollIntoView({ block: 'center' })");
  await sleep(300);
  const shot = await send("Page.captureScreenshot", { format: "png" });
  fs.writeFileSync(out, Buffer.from(shot.data, "base64"));
  await send("Emulation.setDeviceMetricsOverride", { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await sleep(500);
  const narrow = await evaluate("JSON.stringify({ overflow: document.documentElement.scrollWidth > window.innerWidth + 1, saveVisible: !!document.querySelector('#materials-save')?.getBoundingClientRect().width, editor: !!document.querySelector('#text-materials-editor .materials-editor'), listWidth: document.querySelector('#materials-associations')?.getBoundingClientRect().width || 0 })");
  const narrowShot = await send("Page.captureScreenshot", { format: "png" });
  fs.writeFileSync(out.replace(/\.png$/, "-narrow.png"), Buffer.from(narrowShot.data, "base64"));
  console.log(JSON.stringify({ ok: true, project, status, validateStatus, saveStatus, prepareStatus, previewStatus, report, n2bHints, focusKept, states, restoreVisible, recovered, recoveredContains, narrow, externalSave, conflictStatus, conflictKept, discardVisible, afterDiscard, editor, screenshot: out }, null, 2));
} catch (error) {
  console.error("浏览器烟测失败：" + error.message);
  if (serverErr) console.error(serverErr.slice(-2000));
  process.exitCode = 1;
} finally {
  try { chrome?.kill(); } catch {}
  try { server.kill(); } catch {}
  await sleep(200);
}
