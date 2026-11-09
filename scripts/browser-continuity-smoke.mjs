#!/usr/bin/env node
// 1.13 版本面板真实浏览器烟测：登录 → 武英殿 → 选项目 → 等快照 → 对比 → 截图（含窄屏）。
// 用法: node scripts/browser-versions-smoke.mjs <工作区> <ziyuan.exe> <输出.png> [端口]
import { spawn, execFileSync } from "node:child_process";
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
fs.mkdirSync(root,{recursive:true});
if(!fs.existsSync(path.join(root,".ziyuan","court.log")))execFileSync(bin,["init"],{cwd:root});
if(!fs.existsSync(path.join(root,'.ziyuan','text-projects'))){
 const p=JSON.parse(execFileSync(bin,['text','init','--title','连续工作样本','--language','ja','--source','self','--rights','self','--json'],{cwd:root,encoding:'utf8'}));
 const file=path.join(root,'source.txt');fs.writeFileSync(file,'勇者は城へ。\n\n夜だった。\n');execFileSync(bin,['text','import',p.id,file],{cwd:root});
}
const server = spawn(bin, ["serve", "--port", String(port), "--bind", "127.0.0.1"], { cwd: root, windowsHide:true, stdio: ["ignore", "pipe", "pipe"] });
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
  ], {windowsHide:true, stdio:"ignore"});
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
  await evaluate("document.querySelector('[data-hall=\\'wuying\\']').click()");
  await waitJs("!!document.querySelector('#text-project') && document.querySelectorAll('#text-project option').length > 0", 20000, "项目下拉");
  await evaluate("(() => { const s = document.querySelector('#text-project'); s.value = s.options[0].value; s.dispatchEvent(new Event('change', { bubbles: true })); })()");

  await waitJs("textBlockId !== null && !!document.querySelector('#continuity-reader')",20000,"工作区模块");
  await evaluate("document.querySelector('#continuity-reader').open=true; document.querySelector('#continuity-reader [data-load]').click()");
  await waitJs("document.querySelectorAll('#continuity-reader .reading-row').length===2",20000,"双语通读");
  await evaluate("(() => {const d=[...document.querySelectorAll('details')].find(d=>d.querySelector('summary')?.textContent==='跨章待查');d.open=true;d.querySelector('[data-load]').click();})()");
  await waitJs("[...document.querySelectorAll('details')].find(d=>d.querySelector('summary')?.textContent==='跨章待查')?.querySelector('[data-source]')?.options.length===2",20000,"待查段落");
  await evaluate("(() => {const d=[...document.querySelectorAll('details')].find(d=>d.querySelector('summary')?.textContent==='跨章待查');d.querySelector('[data-rule]').value='称谓';d.querySelector('[data-body]').value='浏览器自编待查';d.querySelector('[data-related]').options[1].selected=true;d.querySelector('[data-add]').click();})()");
  await waitJs("[...document.querySelectorAll('details')].some(d=>d.querySelector('[data-list]')?.textContent.includes('浏览器自编待查'))",20000,"登记待查");
  await evaluate("document.querySelector('#continuity-reader [data-check]').click()");
  await waitJs("document.querySelector('#continuity-reader [data-report]').textContent.includes('暂不能导出')",20000,"交稿门槛");
  await evaluate("(() => {const d=[...document.querySelectorAll('details')].find(d=>d.querySelector('summary')?.textContent==='备份与恢复');d.open=true;d.querySelector('[data-preview]').click();})()");
  await waitJs("[...document.querySelectorAll('details')].some(d=>d.querySelector('[data-download]')?.disabled===false)",20000,"备份预览");
  await evaluate("document.querySelector('#continuity-reader').scrollIntoView({block:'start'})");
  fs.writeFileSync(out,Buffer.from((await send('Page.captureScreenshot',{format:'png'})).data,'base64'));
  await send('Emulation.setDeviceMetricsOverride',{width:390,height:844,deviceScaleFactor:1,mobile:false});await sleep(300);
  await evaluate("document.querySelector('#continuity-reader').scrollIntoView({block:'start'})");
  const overflow=await evaluate('window.innerWidth!==390 || document.documentElement.scrollWidth>391');
  fs.writeFileSync(out.replace(/\.png$/,'-narrow.png'),Buffer.from((await send('Page.captureScreenshot',{format:'png'})).data,'base64'));
  if(overflow){console.error(JSON.stringify(await evaluate("({inner:innerWidth,scroll:document.documentElement.scrollWidth,bad:[...document.querySelectorAll('body *')].map(e=>({tag:e.tagName,id:e.id,cls:e.className,left:e.getBoundingClientRect().left,right:e.getBoundingClientRect().right,width:e.getBoundingClientRect().width})).filter(e=>e.width>0&&(e.right>391||e.left<0)).slice(0,12)})")));throw new Error('窄屏横向溢出');}for(const theme of ['tianqing','mo','qinglv']){for(const width of [390,768,1440]){await send('Emulation.setDeviceMetricsOverride',{width,height:900,deviceScaleFactor:1,mobile:false});await evaluate('setTheme('+JSON.stringify(theme)+')');await sleep(100);const bad=await evaluate('document.documentElement.scrollWidth>window.innerWidth+1');if(bad)throw new Error(theme+' / '+width+'px 溢出');}}console.log(JSON.stringify({ok:true,reading:true,issues:true,preflight:true,backup:true,overflow,themes:3,widths:[390,768,1440]}));
}catch(error){console.error(error.message);if(serverErr)console.error(serverErr.slice(-1200));process.exitCode=1;}finally{try{chrome?.kill();}catch{}try{server.kill();}catch{}}
