#!/usr/bin/env node
// 本地资料 HTTP 性能烟测：5000 条术语接近 1MiB，记录 view/save/validate/preview 耗时。
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const args = new Map();
for (let i = 2; i < process.argv.length; i += 2) args.set(process.argv[i], process.argv[i + 1]);
const root = args.get("--root");
const bin = args.get("--bin");
const port = Number(args.get("--port") || 18130);
if (!root || !bin) { console.error("用法: node scripts/materials-bench.mjs --root <工作区> --bin <ziyuan.exe> [--port]"); process.exit(2); }
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function waitFor(fn, timeout = 20000, label = "条件") {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    try { const v = await fn(); if (v) return v; } catch {}
    await sleep(150);
  }
  throw new Error(label + " 超时");
}
const server = spawn(bin, ["serve", "--port", String(port), "--bind", "127.0.0.1"], { cwd: root, stdio: ["ignore", "pipe", "pipe"] });
let serverErr = "";
server.stderr.on("data", (d) => { serverErr += d.toString(); });
try {
  const tokenPath = path.join(root, ".ziyuan", "web-token");
  const token = await waitFor(() => fs.existsSync(tokenPath) && fs.readFileSync(tokenPath, "utf8").trim(), 15000, "web-token");
  const base = "http://127.0.0.1:" + port;
  await waitFor(async () => (await fetch(base + "/")).status === 200, 15000, "服务");
  const login = await fetch(base + "/login", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ token }) });
  const cookie = (login.headers.getSetCookie?.()[0] ?? login.headers.get("set-cookie") ?? "").split(";")[0];
  if (!cookie || !login.ok) throw new Error("登录失败 " + login.status);
  const headers = { Cookie: cookie, "Content-Type": "application/json" };
  const timed = async (label, fn) => { const t = performance.now(); const result = await fn(); return { label, ms: Math.round(performance.now() - t), result }; };
  const view = await timed("view", async () => (await (await fetch(base + "/api/text", { headers })).json()));
  const projects = Array.isArray(view.result) ? view.result : view.result.projects;
  const project = projects[0].id ?? projects[0];
  const materialsView = await (await fetch(base + "/api/text/" + encodeURIComponent(project) + "/materials", { headers })).json();
  const block = materialsView.project.chapters[0].blocks[0].block_id;
  const workspaceId = materialsView.workspace_id;
  const terms = [];
  for (let i = 0; i < 5000; i++) {
    const id = "bench-" + String(i).padStart(5, "0");
    terms.push({ term_id: id, source: "术语" + String(i).padStart(5, "0"), category: "common", allowed: ["译法"], scope: { kind: "project" }, evidence: [], status: "tentative" });
  }
  const material = { format: 2, terms, persons: [] };
  const bytes = Buffer.byteLength(JSON.stringify(material));
  const body = (requestId, revision) => JSON.stringify({ workspace_id: workspaceId, request_id: requestId, expected_revision: revision, material });
  const view2 = await timed("view-after", async () => (await (await fetch(base + "/api/text/" + encodeURIComponent(project) + "/materials", { headers })).json()));
  const save = await timed("save-5000", async () => {
    const response = await fetch(base + "/api/text/" + encodeURIComponent(project) + "/materials/draft", { method: "POST", headers, body: body("bench-save", null) });
    return { status: response.status, text: (await response.text()).slice(0, 200) };
  });
  const revision = (() => { try { return JSON.parse(save.result.text).revision; } catch { return null; } })();
  const validate = await timed("validate-5000", async () => {
    const response = await fetch(base + "/api/text/" + encodeURIComponent(project) + "/materials/validate", { method: "POST", headers, body: JSON.stringify({ workspace_id: workspaceId, material }) });
    return { status: response.status, text: (await response.text()).slice(0, 200) };
  });
  const preview = await timed("preview-5000", async () => {
    const response = await fetch(base + "/api/text/" + encodeURIComponent(project) + "/materials/preview", { method: "POST", headers, body: JSON.stringify({ workspace_id: workspaceId, material, block, associations: [] }) });
    return { status: response.status, text: (await response.text()).slice(0, 200) };
  });
  console.log(JSON.stringify({ ok: true, project, block, bytes, terms: terms.length, saveStatus: save.result.status, revision, validateStatus: validate.result.status, previewStatus: preview.result.status, timings: [view, view2, save, validate, preview].map(({ label, ms }) => ({ label, ms })) }, null, 2));
} catch (error) {
  console.error("性能烟测失败：" + error.message);
  if (serverErr) console.error(serverErr.slice(-1500));
  process.exitCode = 1;
} finally {
  try { server.kill(); } catch {}
  await sleep(200);
}
