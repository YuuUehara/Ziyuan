#!/usr/bin/env node
// 用真实页面检查三主题、五个页面、弹窗、长文本及 200% 显示下的横向越界。
// 用法: node scripts/browser-layout-smoke.mjs <自编样本目录> <ziyuan.exe> <截图.png> [端口]
import { spawn, execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const [root, bin, output, portArg] = process.argv.slice(2);
if (!root || !bin || !output) throw new Error("请提供自编样本目录、程序路径与截图路径");
const port = Number(portArg || 18095);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(fn, label) {
  const deadline = Date.now() + 20000;
  while (Date.now() < deadline) {
    const value = await fn();
    if (value) return value;
    await sleep(100);
  }
  throw new Error(label + "超时");
}
fs.mkdirSync(root, { recursive: true });
const cli = (args) => execFileSync(bin, args, { cwd: root, encoding: "utf8", windowsHide: true });
if (!fs.existsSync(path.join(root, ".ziyuan", "court.log"))) cli(["init"]);
if (!fs.existsSync(path.join(root, ".ziyuan", "text-projects"))) {
  const project = JSON.parse(cli(["text", "init", "--title", "自编排版样章——" + "LongProjectTitle".repeat(5), "--language", "ja", "--source", "自编排版样本", "--rights", "仅作界面检查", "--json"]));
  const source = path.join(root, "source.txt");
  fs.writeFileSync(source, "長い文章の表示を確かめる。".repeat(30) + "\n\nhttps://example.invalid/" + "long-path-segment".repeat(40) + "\n");
  cli(["text", "import", project.id, source]);
  cli(["edict", "--queue", "--dept", "works", "--tools", "read_file", "自编长诏令：" + "LongUnbrokenTaskName".repeat(30)]);
}
const server = spawn(bin, ["serve", "--bind", "127.0.0.1", "--port", String(port)], { cwd: root, windowsHide: true, stdio: "ignore" });
let chrome, socket;
const failures = [];
let cases = 0;
try {
  const base = "http://127.0.0.1:" + port;
  const token = await until(() => {
    const file = path.join(root, ".ziyuan", "web-token");
    return fs.existsSync(file) && fs.readFileSync(file, "utf8").trim();
  }, "服务令牌");
  await until(async () => { try { return (await fetch(base)).ok; } catch { return false; } }, "服务启动");
  const profile = fs.mkdtempSync(path.join(root, "chrome-"));
  chrome = spawn("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe", ["--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check", "--user-data-dir=" + profile, "--remote-debugging-port=0", "about:blank"], { windowsHide: true, stdio: "ignore" });
  const devtools = path.join(profile, "DevToolsActivePort");
  const debugPort = Number((await until(() => fs.existsSync(devtools) && fs.readFileSync(devtools, "utf8"), "浏览器启动")).split(/\r?\n/)[0]);
  const target = (await (await fetch(`http://127.0.0.1:${debugPort}/json`)).json()).find((item) => item.type === "page");
  socket = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
  const pending = new Map(); let sequence = 0;
  socket.onmessage = ({ data }) => {
    const message = JSON.parse(data), waiter = pending.get(message.id);
    if (!waiter) return;
    pending.delete(message.id);
    message.error ? waiter.reject(new Error(message.error.message)) : waiter.resolve(message.result);
  };
  const send = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++sequence; pending.set(id, { resolve, reject });
    socket.send(JSON.stringify({ id, method, params }));
  });
  const evaluate = async (expression) => {
    const result = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text);
    return result.result?.value;
  };
  const viewport = async (width) => {
    await send("Emulation.setDeviceMetricsOverride", { width, height: 1000, deviceScaleFactor: 1, mobile: false });
    await sleep(60);
  };
  const inspect = async (label) => {
    cases++;
    const result = await evaluate(`(() => {
      const width = document.documentElement.clientWidth;
      const dialogs = [...document.querySelectorAll('dialog[open]')];
      const clippedButtons = [...document.querySelectorAll('button')].filter(e => e.getClientRects().length && e.scrollWidth > e.clientWidth + 2).map(e => e.textContent.trim()).slice(0, 8);
      const context = document.createElement('canvas').getContext('2d');
      const crampedSelects = [...document.querySelectorAll('select')].filter(e => {
        if (!e.getClientRects().length || !e.selectedOptions.length) return false;
        const text = e.selectedOptions[0].textContent.trim();
        if (!text || text.length > 12) return false;
        const style = getComputedStyle(e);
        context.font = style.fontSize + ' ' + style.fontFamily;
        return e.clientWidth + 2 < context.measureText(text).width + parseFloat(style.paddingLeft) + parseFloat(style.paddingRight) + 16;
      }).map(e => ({id:e.id, text:e.selectedOptions[0].textContent.trim(), width:e.clientWidth})).slice(0, 8);
      const dialogOverflow = dialogs.some(e => e.getBoundingClientRect().left < -1 || e.getBoundingClientRect().right > width + 1 || e.scrollWidth > e.clientWidth + 2);
      const overflow = document.documentElement.scrollWidth > width + 1;
      const offenders = [...document.querySelectorAll('body *')].filter(e => e.getClientRects().length).map(e => ({ tag:e.tagName, id:e.id, cls:typeof e.className==='string'?e.className:'', right:Math.round(e.getBoundingClientRect().right), width:Math.round(e.getBoundingClientRect().width) })).filter(e => e.right > width + 1).slice(0, 10);
      return { width, scroll:document.documentElement.scrollWidth, overflow, dialogOverflow, clippedButtons, crampedSelects, offenders };
    })()`);
    if (result.overflow || result.dialogOverflow || result.clippedButtons.length || result.crampedSelects.length) {
      failures.push({ label, ...result });
      if (failures.length === 1) fs.writeFileSync(output, Buffer.from((await send("Page.captureScreenshot", { format: "png" })).data, "base64"));
    }
  };
  await send("Page.enable");
  await send("Page.navigate", { url: base });
  await until(() => evaluate("!!document.querySelector('#login-token')"), "登录页");
  await viewport(320);
  await evaluate("document.fonts.ready");
  await inspect("登录 / 320px");
  await evaluate(`document.querySelector('#login-token').value=${JSON.stringify(token)};document.querySelector('#login-form').requestSubmit()`);
  await until(() => evaluate("!!document.querySelector('#edict-new') && !document.querySelector('#locked').classList.contains('show')"), "入朝");
  await evaluate("location.hash='wuying';showHall()");
  await until(() => evaluate("document.querySelector('#text-project')?.options.length>0"), "译书项目");
  await evaluate("const project=document.querySelector('#text-project');project.value=project.options[0].value;project.dispatchEvent(new Event('change',{bubbles:true}))");
  await until(() => evaluate("textBlockId!==null && !!document.querySelector('#continuity-reader')"), "译书页面");
  await evaluate("document.querySelectorAll('#text-card details').forEach(e=>e.open=true);document.querySelector('#materials-load')?.click();document.querySelector('#continuity-reader [data-load]').click()");
  await until(() => evaluate("document.querySelectorAll('#continuity-reader .reading-row').length===2"), "长文通读");
  await sleep(300);
  // 如已挂载资料预览卡，附加长校验码；正文和长 URL 由前面的自编原文提供。
  await evaluate("document.querySelectorAll('.materials-preview pre').forEach(e=>e.textContent+='\\n'+ '0123456789abcdef'.repeat(32))");
  const themes = ["tianqing", "mo", "qinglv"], widths = [320, 390, 768, 1440];
  for (const theme of themes) {
    await evaluate(`setTheme(${JSON.stringify(theme)})`);
    for (const width of widths) {
      await viewport(width);
      for (const hall of ["fengtian", "huagai", "wenhua", "wuying", "qiju"]) {
        await evaluate(`location.hash=${JSON.stringify(hall)};showHall()`);
        await sleep(50);
        if (hall === "wuying") await evaluate("document.querySelectorAll('#text-card details').forEach(e=>e.open=true)");
        await inspect(`${theme} / ${width}px / ${hall}`);
      }
    }
  }
  await evaluate("location.hash='wuying';showHall()");
  for (const width of [320, 640]) {
    await viewport(width);
    for (const id of ["text-new-project-dialog", "material-wizard", "schedule-dialog"]) {
      if (id === "schedule-dialog") await evaluate("schedulingTask=state.tasks[0].id;document.querySelector('#schedule-nodes').replaceChildren();addScheduleNode();addScheduleNode()");
      await evaluate(`document.getElementById(${JSON.stringify(id)}).showModal()`);
      await inspect(`${id} / ${width}px`);
      await evaluate(`document.getElementById(${JSON.stringify(id)}).close()`);
    }
  }
  // 1440px窗口放大至200%时，页面获得720个CSS像素的可用宽度。
  await viewport(720);
  await inspect("1440px 窗口 / 200% 放大等效可用宽度");
  if (failures.length) {
    console.error(JSON.stringify({ ok: false, cases, failedCases: failures.length, failures: failures.slice(0, 12) }));
    process.exitCode = 1;
  } else {
    await viewport(390);
    await evaluate("document.querySelector('#continuity-reader').scrollIntoView({block:'start'})");
    fs.writeFileSync(output, Buffer.from((await send("Page.captureScreenshot", { format: "png" })).data, "base64"));
    console.log(JSON.stringify({ ok: true, cases, themes, widths, halls: 5, dialogs: 3, longText: true, zoomEquivalent: "200%" }));
  }
} catch (error) {
  console.error(error.message); process.exitCode = 1;
} finally {
  socket?.close(); chrome?.kill(); server.kill();
}
