import { textLocation } from "./translation-workspace.mjs";
import { mountIssues } from "./continuity-issues.mjs";
import { mountReading } from "./continuity-reading.mjs";
import { mountSearch } from "./continuity-search.mjs";
import { mountMemoryExchange } from "./continuity-memory.mjs";
import { mountDocuments } from "./continuity-document.mjs";
async function readError(response, fallback) {
  const data = await response.json().catch(() => ({}));
  return data.error || data.message || `${fallback}（HTTP ${response.status}）`;
}
export function mount({ api, getSelection, getChapters, refresh, navigate }) {
  const host = document.createElement("details");host.className="continuity-tools";host.dataset.workspaceTool="backup";
  host.innerHTML = `<summary>备份与恢复</summary><p class="hint">完整备份包含全部译书项目和共用朝廷记录，其中可能有代码任务正文；登录令牌和模型凭据配置不会打包。恢复时会在当前工作区内新建目录，请勿把这份备份直接当作单本书的分享包。</p><div class="row write"><button type="button" data-preview>预览完整备份</button><button type="button" data-download disabled>确认下载</button></div><div class="row write"><input type="file" accept=".tar" data-file aria-label="备份文件"><input data-destination placeholder="恢复到的新目录名，如 books-restored" aria-label="恢复目录"><button type="button" data-check>校验备份</button><button type="button" data-restore disabled>确认恢复</button></div><p data-status role="status"></p><div class="continuity-result" data-result></div>`;
  document.querySelector("#text-card > .body").prepend(host);
  const el = (key) => host.querySelector(`[data-${key}]`);
  let preview, checked, upload;
  const show = (value) => renderBackup(el("result"), value);
  const run = async (task) => { try { el("status").textContent = "处理中…"; await task(); el("status").textContent = "完成"; } catch (e) { el("status").textContent = e.message; } };
  el("preview").onclick = () => run(async () => { preview = await api("/text/backup", {}); show(preview); el("download").disabled = false; });
  el("download").onclick = () => run(async () => {
    const response = await fetch("api/text/backup", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ fingerprint: preview.fingerprint }) });
    if (!response.ok) { el("download").disabled = true; throw new Error(await readError(response, "下载备份失败")); }
    const url = URL.createObjectURL(await response.blob()); const a = document.createElement("a"); a.href = url; a.download = "ziyuan-workspace.tar"; a.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
  });
  const send = async (query = "") => {
    const response = await fetch("api/text/restore" + query, { method: "POST", body: upload });
    if (!response.ok) throw new Error(await readError(response, "恢复备份失败"));
    const value = await response.json().catch(() => null);
    if (!value) throw new Error("服务器返回了无效的恢复响应");
    return value;
  };
  el("file").onchange = () => { checked = null; el("restore").disabled = true; };
  el("check").onclick = () => run(async () => { const file = el("file").files[0]; if (!file) throw new Error("请选择备份文件"); if (file.size > 256 * 1024 * 1024) throw new Error("备份超过 256 MiB，请检查是否选错文件"); upload = await file.arrayBuffer(); const result = await send(); if(el("file").files[0]!==file)throw new Error("备份文件已变，请重新校验"); checked=result; show(checked); el("restore").disabled = false; });
  el("restore").onclick = () => run(async () => { if (!checked) throw new Error("请先校验备份"); show(await send(`?destination=${encodeURIComponent(el("destination").value)}&fingerprint=${encodeURIComponent(checked.fingerprint)}`)); el("restore").disabled = true; });
  const updates=document.createElement("details");updates.className="continuity-tools";updates.dataset.workspaceTool="source-update";
  updates.innerHTML=`<summary>追加与修订原文</summary><p class="hint">可选 TXT、Markdown（支持 UTF-8 与常见旧编码：日语 JIS／韩语 EUC-KR／西欧 Windows-1252）、DOCX 或 EPUB。TXT 和 Markdown 不超过 8 MiB，DOCX 和 EPUB 不超过 16 MiB。压缩包里最多 4096 个文件，每个解开后不超过 32 MiB。默认追加新章；勾选「修订当前章」则改当前块所在的那一章。先预览，再确认。旧原件和旧译都保留。修订时段落数和所属的块必须与原来一样，拆段、合段或重排会被拒绝。</p><div class="row write"><input type="file" accept=".txt,.md,.markdown,.docx,.epub" data-source aria-label="更新原文"><label><input type="checkbox" data-revise>修订当前章</label><button data-preview type="button">预览原文变化</button><button data-apply type="button" disabled>确认更新原文</button></div><p data-status role="status"></p><div class="continuity-result" data-result></div>`;
  host.after(updates);
  const issues=mountIssues(updates,{api,getSelection,navigate});
  const reading=mountReading(issues,{api,getSelection,getChapters,navigate});
  const search=mountSearch(reading,{api,getSelection,navigate,refresh});
  const memory=mountMemoryExchange(search,{api,getSelection});
  const documents=mountDocuments(memory,{getSelection,refresh});
  issues.dataset.workspaceTool="issues";reading.dataset.workspaceTool="reading";search.dataset.workspaceTool="search";memory.dataset.workspaceTool="tmx";
  const u=(name)=>updates.querySelector(`[data-${name}]`);
  let proposed;
  const change=async(task)=>{try{u("status").textContent="处理中…";await task();u("status").textContent="完成";}catch(e){u("status").textContent=e.message;u("apply").disabled=true;}};
  u("source").onchange=u("revise").onchange=()=>{proposed=null;u("apply").disabled=true;};
  u("preview").onclick=()=>change(async()=>{
    const {project,block,sourceLanguage}=getSelection(); if(!project)throw new Error("请先选择项目");
    const file=u("source").files[0];if(!file)throw new Error("请选择 TXT、Markdown、DOCX 或 EPUB 文件");
    const binary=/\.(docx|epub)$/i.test(file.name);
    if(file.size>(binary?16:8)*1024*1024)throw new Error(binary?"原件超过 16 MiB":"原文超过 8 MiB");
    if(u("revise").checked&&!block)throw new Error("请选择要修订章节中的一个块");
    const bytes=new Uint8Array(await file.arrayBuffer());
    const input=sourceUpdateRequest(file.name,bytes,u("revise").checked?block.split("-")[0]:null,sourceLanguage||null);
    proposed=null;u("apply").disabled=true;
    const preview=await api(`/text/${encodeURIComponent(project)}/source-update`,input);if(project!==getSelection().project||u("source").files[0]!==file||u("revise").checked!==!!input.chapter)throw new Error("项目或原文选项已变，请重新预览");proposed={project,input,preview};renderChanges(u("result"),preview);u("apply").disabled=preview.blockers.length>0;
  });
  u("apply").onclick=()=>change(async()=>{if(!proposed||proposed.project!==getSelection().project)throw new Error("项目已变，请重新预览");await api(`/text/${encodeURIComponent(proposed.project)}/source-update`,{...proposed.input,fingerprint:proposed.preview.fingerprint});proposed=null;u("apply").disabled=true;await refresh();});
}
function hasIso2022JpEscape(bytes) {
  for (let i = 0; i + 2 < bytes.length; i++) {
    if (bytes[i] !== 0x1b || bytes[i + 1] !== 0x24) continue;
    if (bytes[i + 2] === 0x42 || bytes[i + 2] === 0x40) return true;
    if (i + 3 < bytes.length && bytes[i + 2] === 0x28 && bytes[i + 3] === 0x44) return true;
  }
  return false;
}
function hasDisallowedControlByte(bytes) {
  for (let i = 0; i < bytes.length; i++) {
    const b = bytes[i];
    if ((b <= 0x08) || b === 0x0b || b === 0x0c || (b >= 0x0e && b <= 0x1f) || b === 0x7f) return true;
  }
  return false;
}
function isUndefinedWindows1252(b) {
  return b === 0x81 || b === 0x8d || b === 0x8f || b === 0x90 || b === 0x9d;
}
function isCommonWestern1252Letter(b) {
  return b === 0x8c || b === 0x9c || b === 0x9f
    || (b >= 0xc0 && b <= 0xcf) || (b >= 0xd1 && b <= 0xd6) || (b >= 0xd9 && b <= 0xdc)
    || (b >= 0xdf && b <= 0xef) || (b >= 0xf1 && b <= 0xf6) || (b >= 0xf9 && b <= 0xfc) || b === 0xff;
}
function isCommonWestern1252Punct(b) {
  return b === 0x80 || b === 0x82 || b === 0x84 || b === 0x85 || b === 0x8b
    || (b >= 0x91 && b <= 0x97) || b === 0x9b || b === 0xa0 || b === 0xa3 || b === 0xa7
    || b === 0xa9 || b === 0xab || b === 0xad || b === 0xae || b === 0xb0 || b === 0xb2
    || b === 0xb3 || b === 0xb4 || b === 0xb7 || b === 0xba || b === 0xbb;
}
function isPlausibleWindows1252(bytes) {
  if (!bytes.length || hasDisallowedControlByte(bytes)) return false;
  let nonAscii = 0, commonWestern = 0, hasAsciiAlnum = false;
  for (let i = 0; i < bytes.length; i++) {
    const b = bytes[i];
    if (isUndefinedWindows1252(b)) return false;
    if ((b >= 0x30 && b <= 0x39) || (b >= 0x41 && b <= 0x5a) || (b >= 0x61 && b <= 0x7a)) hasAsciiAlnum = true;
    else if (b >= 0x80) {
      nonAscii++;
      if (isCommonWestern1252Letter(b) || isCommonWestern1252Punct(b)) commonWestern++;
    }
  }
  return nonAscii > 0 && hasAsciiAlnum && nonAscii * 3 <= bytes.length * 2 && commonWestern * 4 >= nonAscii * 3;
}
const WIN1252_HIGH = [
  0x20ac, 0, 0x201a, 0x0192, 0x201e, 0x2026, 0x2020, 0x2021,
  0x02c6, 0x2030, 0x0160, 0x2039, 0x0152, 0, 0x017d, 0,
  0, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014,
  0x02dc, 0x2122, 0x0161, 0x203a, 0x0153, 0, 0x017e, 0x0178,
];
function decodeWindows1252Strict(bytes) {
  if (!isPlausibleWindows1252(bytes)) return null;
  let out = "";
  for (let i = 0; i < bytes.length; i++) {
    const b = bytes[i];
    if (b >= 0x80 && b <= 0x9f) {
      const cp = WIN1252_HIGH[b - 0x80];
      if (!cp) return null;
      out += String.fromCharCode(cp);
    } else {
      out += String.fromCharCode(b);
    }
  }
  return out;
}
function westernPlausibility(bytes) {
  let score = 0, prevHighNonPunct = false;
  for (let i = 0; i < bytes.length; i++) {
    const b = bytes[i];
    if (b < 0x80) {
      score += (b === 0x09 || b === 0x0a || b === 0x0d || (b >= 0x20 && b <= 0x7e)) ? 1 : -8;
      prevHighNonPunct = false;
    } else if (isCommonWestern1252Letter(b)) {
      score += prevHighNonPunct ? 1 : 2;
      prevHighNonPunct = true;
    } else if (isCommonWestern1252Punct(b)) {
      score += 1;
      prevHighNonPunct = false;
    } else {
      score += prevHighNonPunct ? -5 : -4;
      prevHighNonPunct = true;
    }
  }
  return score;
}
function koreanPlausibility(bytes) {
  let score = 0;
  for (let i = 0; i < bytes.length;) {
    const b = bytes[i];
    if (b < 0x80) {
      score += (b === 0x09 || b === 0x0a || b === 0x0d || (b >= 0x20 && b <= 0x7e)) ? 1 : -8;
      i++;
      continue;
    }
    if (i + 1 >= bytes.length) return -1e9;
    const t = bytes[i + 1];
    if (b >= 0xb0 && b <= 0xc8 && t >= 0xa1 && t <= 0xfe) score += 3;
    else if (b >= 0xa1 && b <= 0xa3 && t >= 0xa1 && t <= 0xfe) score += 2;
    else if (b >= 0xca && b <= 0xfd && t >= 0xa1 && t <= 0xfe) score += 1;
    else if (b >= 0xa4 && b <= 0xac && t >= 0xa1 && t <= 0xfe) score += 0;
    else score -= 1;
    i += 2;
  }
  return score;
}
function japanesePlausibility(text) {
  let score = 0;
  for (let i = 0; i < text.length; i++) {
    const code = text.charCodeAt(i);
    if ((code >= 0x3040 && code <= 0x309f) || (code >= 0x30a0 && code <= 0x30ff)) score += 4;
    else if ((code >= 0x3000 && code <= 0x303f) || (code >= 0xff01 && code <= 0xff60)) score += 3;
    else if ((code >= 0x4e00 && code <= 0x9fff) || (code >= 0x3400 && code <= 0x4dbf)) score += 2;
    else if (code === 0x09 || code === 0x0a || code === 0x0d || (code >= 0x20 && code <= 0x7e)) score += 1;
    else if (code >= 0xff61 && code <= 0xff9f) score -= 4;
    else if (code < 0x20 || (code >= 0x7f && code <= 0x9f)) score -= 8;
  }
  return score;
}
export function decodeTextBytes(buffer, label = "文件", sourceLanguage = null) {
  const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer);
  const bad = () => new Error(`「${label || "文件"}」不是有效的 UTF-8 或受支持旧编码（日语 JIS：Shift_JIS／ISO-2022-JP／EUC-JP；韩语 EUC-KR／CP949；西欧 Windows-1252／ISO-8859-1）文本`);
  if (bytes.length >= 3 && bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf) {
    try { return { text: new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes), encoding: "UTF-8" }; }
    catch { throw bad(); }
  }
  if (bytes.length >= 2 && ((bytes[0] === 0xff && bytes[1] === 0xfe) || (bytes[0] === 0xfe && bytes[1] === 0xff))) {
    throw bad();
  }
  if ((!sourceLanguage || sourceLanguage === "ja") && hasIso2022JpEscape(bytes)) {
    try { return { text: new TextDecoder("iso-2022-jp", { fatal: true }).decode(bytes), encoding: "ISO-2022-JP" }; }
    catch { throw bad(); }
  }
  try { return { text: new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes), encoding: "UTF-8" }; } catch {}
  if (hasDisallowedControlByte(bytes)) throw bad();
  const tryDecode = (name) => { try { const out = new TextDecoder(name, { fatal: true }).decode(bytes); return /[\u007f-\u009f]/.test(out) ? null : out; } catch { return null; } };
  if (sourceLanguage === "ja") {
    const sjis = tryDecode("shift_jis"), euc = tryDecode("euc-jp");
    if (sjis !== null && euc === null) return { text: sjis, encoding: "Shift_JIS" };
    if (euc !== null && sjis === null) return { text: euc, encoding: "EUC-JP" };
    if (sjis !== null && euc !== null) {
      return japanesePlausibility(euc) > japanesePlausibility(sjis)
        ? { text: euc, encoding: "EUC-JP" }
        : { text: sjis, encoding: "Shift_JIS" };
    }
    throw bad();
  }
  if (sourceLanguage === "ko") {
    const ko = tryDecode("euc-kr");
    if (ko !== null) return { text: ko, encoding: "EUC-KR" };
    throw bad();
  }
  if (sourceLanguage === "en" || sourceLanguage === "fr" || sourceLanguage === "de" || sourceLanguage === "es" || sourceLanguage === "it") {
    const w = decodeWindows1252Strict(bytes);
    if (w !== null) return { text: w, encoding: "Windows-1252" };
    throw bad();
  }
  let best = null;
  const consider = (score, text, encoding) => { if (!best || score > best.score) best = { score, text, encoding }; };
  const sjis = tryDecode("shift_jis");
  if (sjis !== null) consider(japanesePlausibility(sjis), sjis, "Shift_JIS");
  const euc = tryDecode("euc-jp");
  if (euc !== null) consider(japanesePlausibility(euc), euc, "EUC-JP");
  const ko = tryDecode("euc-kr");
  if (ko !== null) consider(koreanPlausibility(bytes), ko, "EUC-KR");
  const w = decodeWindows1252Strict(bytes);
  if (w !== null) consider(westernPlausibility(bytes), w, "Windows-1252");
  if (best) return { text: best.text, encoding: best.encoding };
  throw bad();
}
export function sourceUpdateRequest(name, bytes, chapter, sourceLanguage = null) {
  const binary = /\.(docx|epub)$/i.test(name);
  if (!binary) {
    const { text } = decodeTextBytes(bytes, name || "文件", sourceLanguage);
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
    row.append(text("h4",`${change.before===null?"新增":change.after===null?"删除":"修订"} · ${textLocation(change.paragraph)}`));
    const columns=document.createElement("div");columns.className="continuity-columns";
    for(const [label,value] of [["更新前的原文",change.before],["更新后的原文",change.after]]) {const column=document.createElement("div");column.append(text("p",label,"hint"),text("p",value??"—"));columns.append(column);}row.append(columns);host.append(row);
  }
}
