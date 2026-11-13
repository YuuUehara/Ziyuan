import { mountIssues } from "./continuity-issues.mjs";
import { mountReading } from "./continuity-reading.mjs";
import { mountSearch } from "./continuity-search.mjs";
import { mountMemoryExchange } from "./continuity-memory.mjs";
import { mountDocuments } from "./continuity-document.mjs";
export function mount({ api, getSelection, refresh, navigate }) {
  const host = document.createElement("details");host.className="continuity-tools";
  host.innerHTML = `<summary>备份与恢复</summary><p class="hint">完整备份包含全部译书项目和共用朝廷记录，其中可能有代码任务正文；登录令牌和模型凭据配置不会打包。恢复时会在当前工作区内新建目录，请勿把这份备份直接当作单本书的分享包。</p><div class="row write"><button type="button" data-preview>预览完整备份</button><button type="button" data-download disabled>确认下载</button></div><div class="row write"><input type="file" accept=".tar" data-file aria-label="备份文件"><input data-destination placeholder="新目录名，如 books-restored（英文）" aria-label="恢复目录"><button type="button" data-check>校验备份</button><button type="button" data-restore disabled>确认恢复</button></div><p data-status role="status"></p><div class="continuity-result" data-result></div>`;
  document.querySelector("#text-card > .body").prepend(host);
  const el = (key) => host.querySelector(`[data-${key}]`);
  let preview, checked, upload;
  const show = (value) => renderBackup(el("result"), value);
  const run = async (task) => { try { el("status").textContent = "处理中…"; await task(); el("status").textContent = "完成"; } catch (e) { el("status").textContent = e.message; } };
  el("preview").onclick = () => run(async () => { preview = await api("/text/backup", {}); show(preview); el("download").disabled = false; });
  el("download").onclick = () => run(async () => {
    const response = await fetch("api/text/backup", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ fingerprint: preview.fingerprint }) });
    if (!response.ok) { el("download").disabled = true; throw new Error((await response.json()).error); }
    const url = URL.createObjectURL(await response.blob()); const a = document.createElement("a"); a.href = url; a.download = "ziyuan-workspace.tar"; a.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
  });
  const send = async (query = "") => {
    const response = await fetch("api/text/restore" + query, { method: "POST", body: upload });
    const value = await response.json(); if (!response.ok) throw new Error(value.error); return value;
  };
  el("file").onchange = () => { checked = null; el("restore").disabled = true; };
  el("check").onclick = () => run(async () => { const file = el("file").files[0]; if (!file) throw new Error("请选择备份文件"); if (file.size > 256 * 1024 * 1024) throw new Error("备份超过 256 MiB，请检查是否选错文件"); upload = await file.arrayBuffer(); const result = await send(); if(el("file").files[0]!==file)throw new Error("备份文件已变，请重新校验"); checked=result; show(checked); el("restore").disabled = false; });
  el("restore").onclick = () => run(async () => { if (!checked) throw new Error("请先校验备份"); show(await send(`?destination=${encodeURIComponent(el("destination").value)}&fingerprint=${encodeURIComponent(checked.fingerprint)}`)); el("restore").disabled = true; });
  const updates=document.createElement("details");updates.className="continuity-tools";
  updates.innerHTML=`<summary>追加与修订原文</summary><p class="hint">选取 TXT、Markdown、DOCX 或 EPUB，默认追加新章；勾选「修订当前章」则更新当前块所在章。请先预览差异，再确认更新，旧原件和旧译都会保留。段落拆合、重新分块或破坏已批准引用的改动会被拒绝。</p><div class="row write"><input type="file" accept=".txt,.md,.markdown,.docx,.epub" data-source aria-label="更新原文"><label><input type="checkbox" data-revise>修订当前章</label><button data-preview type="button">预览原文变化</button><button data-apply type="button" disabled>确认更新原文</button></div><p data-status role="status"></p><div class="continuity-result" data-result></div>`;
  host.after(updates);
  const issues=mountIssues(updates,{api,getSelection,navigate});
  const reading=mountReading(issues,{api,getSelection,navigate});
  const search=mountSearch(reading,{api,getSelection,navigate,refresh});
  const memory=mountMemoryExchange(search,{api,getSelection});
  mountDocuments(memory,{getSelection,refresh});
  const u=(name)=>updates.querySelector(`[data-${name}]`);
  let proposed;
  const change=async(task)=>{try{u("status").textContent="处理中…";await task();u("status").textContent="完成";}catch(e){u("status").textContent=e.message;u("apply").disabled=true;}};
  u("source").onchange=u("revise").onchange=()=>{proposed=null;u("apply").disabled=true;};
  u("preview").onclick=()=>change(async()=>{
    const {project,block}=getSelection(); if(!project)throw new Error("请先选择项目");
    const file=u("source").files[0];if(!file)throw new Error("请选择 TXT、Markdown、DOCX 或 EPUB 文件");
    const binary=/\.(docx|epub)$/i.test(file.name);
    if(file.size>(binary?16:8)*1024*1024)throw new Error(binary?"原件超过 16 MiB":"原文超过 8 MiB，请选用大小符合要求的文件");
    if(u("revise").checked&&!block)throw new Error("请选择要修订章节中的一个块");
    const bytes=new Uint8Array(await file.arrayBuffer());
    const input=sourceUpdateRequest(file.name,bytes,u("revise").checked?block.split("-")[0]:null);
    proposed=null;u("apply").disabled=true;
    const preview=await api(`/text/${encodeURIComponent(project)}/source-update`,input);if(project!==getSelection().project||u("source").files[0]!==file||u("revise").checked!==!!input.chapter)throw new Error("项目或原文选项已变，请重新预览");proposed={project,input,preview};renderChanges(u("result"),preview);u("apply").disabled=preview.blockers.length>0;
  });
  u("apply").onclick=()=>change(async()=>{if(!proposed||proposed.project!==getSelection().project)throw new Error("项目已变，请重新预览");await api(`/text/${encodeURIComponent(proposed.project)}/source-update`,{...proposed.input,fingerprint:proposed.preview.fingerprint});proposed=null;u("apply").disabled=true;await refresh();});
}
export function sourceUpdateRequest(name, bytes, chapter) {
  const binary = /\.(docx|epub)$/i.test(name);
  if (!binary) {
    const text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
    return { chapter, name, text };
  }
  let binaryText = "";
  const chunk = 0x8000;
  for (let index = 0; index < bytes.length; index += chunk) {
    binaryText += String.fromCharCode(...bytes.subarray(index, index + chunk));
  }
  return { chapter, name, text: "", package_base64: btoa(binaryText) };
}
export function backupSummary(value) {
  const manifest=value.manifest||value;
  return {projects:manifest.projects?.length||0,files:manifest.files?.length||0,bytes:(manifest.files||[]).reduce((sum,file)=>sum+Number(file.bytes||0),0)};
}
function text(tag,value,className) {const node=document.createElement(tag);node.textContent=value;if(className)node.className=className;return node;}
function renderBackup(host,value) {
  host.replaceChildren();const manifest=value.manifest||value;const summary=backupSummary(value);
  host.append(text("p",value.restored?"已恢复到新目录："+value.restored:`共 ${summary.projects} 个项目、${summary.files} 个文件，${(summary.bytes/1024/1024).toFixed(2)} MiB。`));
  if(value.scope)host.append(text("p",value.scope,"hint"));
  const details=document.createElement("details");details.append(text("summary","查看备份文件清单"));
  const list=document.createElement("ul");for(const file of manifest.files||[])list.append(text("li",`${file.path}（${file.bytes} 字节）`));details.append(list);host.append(details);
}
function renderChanges(host,preview) {
  host.replaceChildren();host.append(text("p",`原文变化 ${preview.changes.length} 段，影响 ${preview.changed_blocks.length} 块。`));
  for(const message of preview.blockers||[])host.append(text("p",message,"text-problems"));
  for(const message of preview.warnings||[])host.append(text("p",message,"hint"));
  for(const change of preview.changes) {
    const row=document.createElement("article");row.className="continuity-change";
    row.append(text("h4",`${change.before===null?"新增":change.after===null?"删除":"修订"} · ${change.paragraph}`));
    const columns=document.createElement("div");columns.className="continuity-columns";
    for(const [label,value] of [["更新前的原文",change.before],["更新后的原文",change.after]]) {const column=document.createElement("div");column.append(text("p",label,"hint"),text("p",value??"—"));columns.append(column);}row.append(columns);host.append(row);
  }
}
