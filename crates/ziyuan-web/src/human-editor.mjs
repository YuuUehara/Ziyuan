import { textLocation } from "./translation-workspace.mjs";
import {
  KIND_LABELS, utf8Bytes, graphemes, validateMarks, spliceMarks, addMark,
  removeMark, runs, paragraphsToWire, paragraphsFromWire, u16ToUtf8,
} from "./editor-core.mjs";

const STATE_LABELS = {dirty:"未保存",saving:"保存中",saved:"已保存",conflict:"冲突",invalid:"不合规"};
const copy = (v) => JSON.parse(JSON.stringify(v));
const equal = (a,b) => JSON.stringify(a) === JSON.stringify(b);
const escape = (v) => String(v ?? "").replace(/[&<>"']/g,c => ({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"})[c]);
const snapshot = (paragraphs) => JSON.stringify(paragraphs);
function requestId() {
  if(globalThis.crypto.randomUUID) return globalThis.crypto.randomUUID();
  const bytes=globalThis.crypto.getRandomValues(new Uint8Array(16));
  bytes[6]=(bytes[6]&15)|64; bytes[8]=(bytes[8]&63)|128;
  const hex=[...bytes].map(b=>b.toString(16).padStart(2,"0")).join("");
  return `${hex.slice(0,8)}-${hex.slice(8,12)}-${hex.slice(12,16)}-${hex.slice(16,20)}-${hex.slice(20)}`;
}

export function draftStats(paragraphs) {
  let bytes=0, marks=0, body=0, reading=0;
  const problems=[];
  for (const p of paragraphs) {
    body += graphemes(p.text).length; bytes += utf8Bytes(p.text); marks += p.marks.length;
    for (const m of p.marks) if(m.kind==="ruby") {
      reading += graphemes(m.reading || "").length; bytes += utf8Bytes(m.reading || "");
    }
    problems.push(...validateMarks(p.text,p.marks).map(e => `${textLocation(p.id)}：${e.message}`));
  }
  if(marks>4096) problems.push("整稿标记超过 4096 项");
  if(bytes>256*1024) problems.push("整稿正文与读音超过 256 KiB");
  return {bytes,marks,counts:{body,reading},problems};
}

export function previewHtml(paragraph) {
  return runs(paragraph).map(run => {
    let s=escape(run.text);
    if(run.strong) s=`<strong>${s}</strong>`;
    if(run.emphasis) s=`<em>${s}</em>`;
    if(run.emphasis_dots) s=`<span style="text-emphasis:filled dot">${s}</span>`;
    if(run.ruby!==null) s=`<ruby>${s}<rt>${escape(run.ruby)}</rt></ruby>`;
    return s;
  }).join("");
}

export function chapterProgress(progress) {
  if(!progress) return "";
  const line = c => `共 ${Number(c.total)||0} · 已起草 ${Number(c.drafted)||0} · 历史送审 ${Number(c.submitted)||0} · 准奏 ${Number(c.approved)||0} · 定稿 ${Number(c.finalized)||0} · 未起草 ${Number(c.not_started)||0}`;
  return `<p class="hint">起草＝至少一段非空；送审为历史记录，不代表当前修订已送审。</p>` +
    (progress.totals ? `<p>${line(progress.totals)}</p>` : "") +
    (progress.chapters||[]).map(c=>`<p class="hint">${escape(textLocation(c.chapter))}：${line(c)}</p>`).join("") +
    `<button type="button" data-view data-human-next ${progress.next_untranslated ? "" : "disabled"}>下一未起草${progress.next_untranslated ? "："+escape(textLocation(progress.next_untranslated)) : "：无"}</button>`;
}

/** One block, independent of the active page selection. UI and Node tests share this state machine. */
export class HumanDraft {
  constructor(project,block,source,storage) {
    this.project=project; this.block=block; this.source=copy(source); this.storage=storage;
    this.key=`ziyuan-human:${encodeURIComponent(project)}/${encodeURIComponent(block)}`;
    this.paragraphs=source.map(p=>({id:p.id,text:"",marks:[]}));
    this.revision=null; this.basis=null; this.remote=null; this.initialized=false;
    this.saved=snapshot(this.paragraphs); this.busy=false; this.composing=false;
    this.conflict=false; this.conflictCurrent=null; this.message=""; this.viewer=false;
    this.undo=[]; this.redo=[]; this.pendingSave=null; this.recovery=null; this.epoch=0;
    try { this.recovery=JSON.parse(storage?.getItem(this.key)||"null"); } catch {}
  }
  get dirty() { return snapshot(this.paragraphs)!==this.saved; }
  get stats() { return draftStats(this.paragraphs); }
  get state() {
    if(this.busy) return "saving";
    if(this.conflict) return "conflict";
    if(this.composing) return this.dirty ? "dirty" : "saved";
    if(this.stats.problems.length) return "invalid";
    return this.dirty ? "dirty" : "saved";
  }
  get locked() {
    return this.viewer || !this.initialized || !!this.remote?.human?.finalized ||
      ["finalized","human_finalized"].includes(this.remote?.status?.state?.state);
  }
  get editable() { return !this.locked && !this.recovery; }
  receive(view,epoch=this.epoch) {
    if(epoch!==this.epoch || this.busy || this.composing) return false;
    this.remote=copy(view);
    if(!this.initialized) { this.install(view); return true; }
    const changed=this.revision!==view.revision || !equal(this.basis,view.basis);
    if(changed && !this.dirty && !this.conflict && !this.recovery) {
      // Nothing local to lose: follow the disk (e.g. approval just landed zh.json and moved the basis).
      const message=this.message; this.install(view); this.message=view.message||message; return true;
    }
    if(changed) {
      this.conflict=true;
      this.message="已保存的稿件或参考材料有变化。请选择载入最新稿件，或保留页面中的文字，核对后重新保存。";
    }
    return false;
  }
  install(view) {
    this.epoch++;
    this.paragraphs=view.draft ? paragraphsFromWire(view.draft.paragraphs) : this.source.map(p=>({id:p.id,text:"",marks:[]}));
    this.basis=copy(view.basis); this.revision=view.revision ?? null;
    this.saved=snapshot(this.paragraphs); this.initialized=true; this.conflict=false;
    this.undo=[]; this.redo=[]; this.pendingSave=null; this.message=view.message||"";
    // A mirror identical to the disk draft holds nothing to recover.
    if(this.recovery && snapshot(this.recovery.paragraphs)===this.saved && this.recovery.revision===this.revision) this.dismissRecovery();
  }
  mirror() {
    if(this.composing || this.viewer || this.recovery) return;
    try {
      this.storage?.setItem(this.key,JSON.stringify({paragraphs:this.paragraphs,basis:this.basis,revision:this.revision}));
    } catch { this.message="浏览器暂存失败，可能是空间或权限不足。请先保存草稿，再关闭页面。"; }
  }
  restore() {
    if(!this.recovery || this.locked || this.composing || this.busy) return false;
    const r=this.recovery;
    if(!Array.isArray(r.paragraphs) || r.paragraphs.length!==this.source.length ||
      r.paragraphs.some((p,i)=>!p || p.id!==this.source[i].id || typeof p.text!=="string" || !Array.isArray(p.marks) ||
        p.marks.some(m=>!m || typeof m!=="object")) || !r.basis || typeof r.basis!=="object") {
      this.message="页面暂存的段落与当前原文不一致，无法恢复。"; return false;
    }
    this.paragraphs=copy(r.paragraphs);
    this.conflict=r.revision!==this.revision || !equal(r.basis,this.basis);
    this.basis=copy(r.basis); this.revision=r.revision; this.undo=[]; this.redo=[];
    this.recovery=null; this.mirror(); return true;
  }
  dismissRecovery() { this.recovery=null; try { this.storage?.removeItem(this.key); } catch {} }
  loadDisk() {
    if(this.busy || this.composing || !this.remote) return false;
    this.install(this.remote); this.dismissRecovery(); return true;
  }
  keepLocal() {
    if(!this.editable || this.busy || this.composing || !this.remote) return false;
    this.basis=copy(this.remote.basis); this.revision=this.remote.revision ?? null;
    this.conflict=false; this.conflictCurrent=null; this.pendingSave=null;
    // Even identical text needs saving when rebased onto changed source material.
    this.saved=""; this.message="已保留页面中的文字并载入最新参考材料，请核对后重新保存，再送审。"; this.mirror(); return true;
  }
  beginComposition() { this.composing=true; }
  endComposition(index,text) { this.composing=false; this.editText(index,text); }
  editText(index,text) {
    if(this.composing || !this.editable) return false;
    const p=this.paragraphs[index], old=p.text;
    if(old===text) return false;
    let start=0,endOld=old.length,endNew=text.length;
    while(start<endOld && start<endNew && old[start]===text[start]) start++;
    while(endOld>start && endNew>start && old[endOld-1]===text[endNew-1]) { endOld--;endNew--; }
    p.marks=spliceMarks(p.marks,start,endOld,endNew-start); p.text=text;
    this.undo=[]; this.redo=[]; this.pendingSave=null; this.mirror(); return true;
  }
  changeMarks(index,marks) {
    if(!this.editable || this.composing || this.busy) return false;
    this.undo.push(this.paragraphs.map(p=>copy(p.marks))); if(this.undo.length>100) this.undo.shift();
    this.redo=[]; this.paragraphs[index].marks=marks; this.pendingSave=null; this.mirror(); return true;
  }
  mark(index,start,end,kind,reading) {
    const p=this.paragraphs[index];
    const marks=addMark(p.marks,start,end,kind,reading);
    const issues=validateMarks(p.text,marks);
    if(issues.length) throw new Error(issues[0].message);
    const changed=this.changeMarks(index,marks);
    if(changed) this.message="";
    return changed;
  }
  remove(index,mark) { return this.changeMarks(index,removeMark(this.paragraphs[index].marks,mark)); }
  history(from,to) {
    if(!this.editable || this.composing || this.busy || !from.length) return false;
    to.push(this.paragraphs.map(p=>copy(p.marks)));
    from.pop().forEach((marks,i)=>this.paragraphs[i].marks=marks);
    this.pendingSave=null; this.mirror(); return true;
  }
  undoMarks() { return this.history(this.undo,this.redo); }
  redoMarks() { return this.history(this.redo,this.undo); }
  get base() { return `/text/${encodeURIComponent(this.project)}/blocks/${encodeURIComponent(this.block)}/human`; }
  fail(error) {
    this.message=error.body?.message || error.message;
    if(error.status===409) {
      this.conflict=true; this.conflictCurrent=error.body?.current ?? error.current ?? null;
      this.pendingSave=null;
    }
  }
  async save(api) {
    if(!this.editable || this.busy || this.composing || this.conflict || this.stats.problems.length) return false;
    const sent=snapshot(this.paragraphs);
    if(!this.pendingSave || this.pendingSave.sent!==sent) this.pendingSave={
      sent, body:{request_id:requestId(),expected_revision:this.revision,basis:copy(this.basis),paragraphs:paragraphsToWire(this.paragraphs)},
    };
    const request=this.pendingSave; this.busy=true; this.epoch++;
    try {
      const receipt=await api(this.base+"/draft",request.body);
      this.revision=receipt.revision; this.saved=sent; this.pendingSave=null;
      this.message=receipt.message||"已保存人工草稿";
      if(!this.dirty) { try { this.storage?.removeItem(this.key); } catch {} } else this.mirror();
      return true;
    } catch(e) { this.fail(e); return false; } finally { this.busy=false; }
  }
  async action(action,api) {
    if(!["submit","approve","finalize"].includes(action) || !this.editable || this.busy || this.composing ||
      this.conflict || this.dirty || this.stats.problems.length || !this.revision) return false;
    this.busy=true; this.epoch++;
    try {
      const receipt=await api(this.base+"/"+action,{});
      this.message=receipt.message||"操作完成，已按当前稿件核对状态。"; return true;
    } catch(e) { this.fail(e); return false; } finally { this.busy=false; }
  }
}

/** Persistent DOM per block: polling never replaces textareas, nor intercepts native undo/input. */
export function mountHumanEditor(host,{api,isViewer,onNext,onChanged,storage,confirm=globalThis.confirm}) {
  if(storage===undefined) { try { storage=globalThis.sessionStorage; } catch { storage=null; } }
  const entries=new Map();
  let active=null, latestContext=null, deferred, progressRequest=0;
  const progress=host.ownerDocument.createElement("div"), stage=host.ownerDocument.createElement("div");
  host.replaceChildren(progress,stage);
  function notify(entry) {
    const d=entry.draft;
    if(d.composing) return;
    const stats=d.stats;
    entry.status.textContent=`${STATE_LABELS[d.state]} · 修订 ${d.revision ?? "尚无"} · 正文字符 ${stats.counts.body} · 读音字符 ${stats.counts.reading} · ${stats.marks}/4096 标记 · ${stats.bytes}/262144 字节`;
    entry.message.textContent=[
      !d.storage && !d.viewer ? "浏览器无法暂存编辑内容，请在离开页面前保存草稿。" : "",
      d.recovery && !d.viewer ? "发现本块上次留在页面中的暂存内容，请先选择恢复或忽略，再继续编辑。" : "",
      d.message,...stats.problems,
    ].filter(Boolean).join("\n");
    entry.recovery.hidden=!d.recovery || d.viewer;
    entry.conflict.hidden=!d.conflict || d.viewer;
    for(const [action,button] of entry.buttons) {
      button.hidden=d.viewer;
      const workflow=["submit","approve","finalize"].includes(action);
      button.disabled=!d.editable || d.busy || d.composing ||
        (workflow && (d.dirty||d.conflict||!!stats.problems.length||!d.revision)) ||
        (action==="save" && (d.conflict||!!stats.problems.length)) ||
        (action==="undo" && !d.undo.length) || (action==="redo" && !d.redo.length);
    }
    entry.root.querySelectorAll("textarea").forEach(t=>{ t.readOnly=!d.editable; });
    entry.root.querySelectorAll("[data-mark], [data-remove]").forEach(b=>{
      b.hidden=d.viewer; b.disabled=!d.editable||d.busy;
    });
    entry.refresh.disabled=d.busy;
  }
  function details(entry) {
    const d=entry.draft;
    if(d.composing) return;
    entry.rows.forEach((row,i)=>{
      const p=d.paragraphs[i];
      if(!p) return;
      row.preview.innerHTML=previewHtml(p);
      row.table.innerHTML=`<thead><tr><th>标记</th><th>UTF-8 字节 [起,止)</th><th>读音</th><th></th></tr></thead><tbody>`+
        p.marks.map((m,j)=>`<tr><td>${escape(KIND_LABELS[m.kind])}</td><td>[${u16ToUtf8(p.text,m.start) ?? "无效"}, ${u16ToUtf8(p.text,m.end) ?? "无效"})</td><td>${escape(m.reading||"")}</td><td>${d.viewer ? "" : `<button type="button" class="small" data-remove="${j}">删除</button>`}</td></tr>`).join("")+"</tbody>";
    });
    notify(entry);
  }
  function syncText(entry) {
    // Only explicit initial load/recovery/disk replacement changes values. Ordinary input never does.
    if(entry.draft.composing) return;
    const sameIds=entry.rows.length===entry.draft.paragraphs.length &&
      entry.rows.every((r,i)=>r.id===entry.draft.paragraphs[i].id);
    if(!sameIds) buildRows(entry);
    entry.rows.forEach((row,i)=>{
      if(row.textarea.value!==entry.draft.paragraphs[i].text) row.textarea.value=entry.draft.paragraphs[i].text;
    });
    details(entry);
  }
  function button(label,action,parent) {
    const b=host.ownerDocument.createElement("button"); b.type="button"; b.className="small";
    b.textContent=label; if(action) b.dataset.action=action; parent.append(b); return b;
  }
  function buildRows(entry) {
    entry.paras.replaceChildren(); entry.rows=[];
    for(const [i,p] of entry.draft.paragraphs.entries()) {
      const section=host.ownerDocument.createElement("section"); section.className="human-paragraph";
      section.innerHTML=`<h4 title="内部编号：${escape(p.id)}">${escape(textLocation(p.id))}</h4><div class="human-bilingual"><div><b>原文</b><p class="text-para">${escape(entry.draft.source.find(s=>s.id===p.id)?.text||"（原文已变，请核对段落）")}</p></div><label>人工译文<textarea aria-label="${escape(textLocation(p.id))} 人工译文"></textarea></label></div><div class="actions human-mark-tools"></div><p class="human-preview text-para" aria-label="标记预览"></p><div class="table-wrap"><table aria-label="标记结构表"></table></div>`;
      const textarea=section.querySelector("textarea"), tools=section.querySelector(".human-mark-tools");
      for(const [kind,label] of Object.entries(KIND_LABELS)) {
        const b=button(label,null,tools); b.dataset.mark=kind;
        b.addEventListener("click",()=>{
          try {
            const {selectionStart:start,selectionEnd:end}=textarea;
            const reading=kind==="ruby" ? globalThis.prompt("所选文字的读音（单行）","") : undefined;
            if(reading===null) return;
            entry.draft.mark(i,start,end,kind,reading); details(entry); textarea.focus();
          } catch(e) { entry.draft.message=e.message; notify(entry); }
        });
      }
      textarea.addEventListener("compositionstart",()=>{ entry.draft.beginComposition(); });
      textarea.addEventListener("input",event=>{
        if(event.isComposing || entry.draft.composing) return;
        if(entry.draft.editText(i,textarea.value)) details(entry);
      });
      textarea.addEventListener("compositionend",()=>{
        entry.draft.endComposition(i,textarea.value); details(entry);
        if(deferred!==undefined) { const next=deferred; deferred=undefined; update(next); }
      });
      section.querySelector("table").addEventListener("click",event=>{
        const b=event.target.closest("[data-remove]"); if(!b) return;
        entry.draft.remove(i,Number(b.dataset.remove)); details(entry);
      });
      entry.rows.push({id:p.id,textarea,source:section.querySelector(".text-para"),preview:section.querySelector(".human-preview"),table:section.querySelector("table")});
      entry.paras.append(section);
    }
  }
  function create(context) {
    const d=new HumanDraft(context.project,context.block,context.source,storage);
    const root=host.ownerDocument.createElement("div");
    root.innerHTML=`<h3>人工译稿 · ${escape(textLocation(context.block))}</h3><p class="hint">逐段编辑译文，选中文字后可加注音、着重或粗斜体。正文用浏览器的撤销功能；标记可单独撤销，修改正文后会清空标记撤销记录。完成后依次保存、送审、准奏，再御览定稿，每一步都会核对当前稿件。</p><p role="status" class="human-status"></p><p class="human-message hint" role="status"></p><div class="human-recovery actions" hidden></div><div class="human-conflict actions" hidden></div><div class="human-actions actions"></div><div class="human-paragraphs"></div>`;
    const entry={draft:d,root,rows:[],buttons:new Map(),request:0,
      status:root.querySelector(".human-status"),message:root.querySelector(".human-message"),
      recovery:root.querySelector(".human-recovery"),conflict:root.querySelector(".human-conflict"),paras:root.querySelector(".human-paragraphs")};
    button("恢复页面暂存（恢复后仍须保存）","restore",entry.recovery);
    button("忽略页面暂存","dismiss",entry.recovery);
    button("载入已保存稿件（替换页面内容）","disk",entry.conflict);
    button("确认保留本地，取最新基准","rebase",entry.conflict);
    const actions=root.querySelector(".human-actions");
    for(const [action,label] of [["save","保存人工草稿"],["submit","送审当前已保存修订"],["approve","人工准奏"],["finalize","人工御览定稿"],["undo","撤销标记"],["redo","重做标记"]]) {
      entry.buttons.set(action,button(label,action,actions));
    }
    entry.refresh=button("刷新人工状态","refresh",actions); entry.refresh.dataset.view="";
    root.addEventListener("click",async event=>{
      const action=event.target.closest("[data-action]")?.dataset.action;
      if(!action || d.busy || d.composing) return;
      d.viewer=!!isViewer();
      if(action==="refresh") { await pull(entry); return; }
      if(d.viewer) return;
      if(action==="restore") { if(confirm("用页面暂存替换当前编辑内容？恢复后仍须点击保存，才会写入草稿。")) { d.restore(); syncText(entry); } }
      else if(action==="dismiss") { d.dismissRecovery(); notify(entry); }
      else if(action==="disk" || action==="rebase") {
        if(!confirm(action==="disk" ? "丢弃页面中尚未保存的编辑，载入最新稿件？" : "保留页面全文，并载入最新参考材料？请先核对其他窗口的改动；再次保存会用页面内容替换最新草稿。")) return;
        // Resolve from a fresh GET, never from 409's possibly revision-only current.
        if(!await pull(entry)) return;
        if(action==="disk") d.loadDisk(); else d.keepLocal();
        syncText(entry);
      } else if(action==="undo" || action==="redo") {
        action==="undo" ? d.undoMarks() : d.redoMarks(); details(entry);
      } else if(action==="save" || ["submit","approve","finalize"].includes(action)) {
        if(action!=="save" && !confirm(`确定${{submit:"送审当前已保存修订",approve:"准奏当前已送审修订",finalize:"人工御览定稿"}[action]}？未保存或未送审的修订不会自动提交。`)) return;
        const promise=action==="save" ? d.save(api) : d.action(action,api);
        notify(entry); const ok=await promise; notify(entry);
        if(ok) { await pull(entry); onChanged?.(d.project,d.block); }
      }
    });
    buildRows(entry); syncText(entry); return entry;
  }
  async function pull(entry) {
    const d=entry.draft;
    if(d.busy || d.composing) return false;
    const request=++entry.request, epoch=d.epoch;
    try {
      const view=await api(d.base);
      if(request!==entry.request || epoch!==d.epoch || d.busy || d.composing) return false;
      const installed=d.receive(view,epoch);
      if(installed) syncText(entry); else notify(entry);
      return true;
    } catch(e) { if(request===entry.request) { d.message=e.body?.message||e.message; notify(entry); } return false; }
  }
  async function loadProgress(project) {
    const request=++progressRequest;
    try {
      const result=await api(`/text/${encodeURIComponent(project)}/human/progress`);
      if(request!==progressRequest || latestContext?.project!==project) return;
      progress.innerHTML=chapterProgress(result);
      progress.querySelector("[data-human-next]")?.addEventListener("click",()=>onNext?.(result.next_untranslated));
    } catch(e) { if(request===progressRequest) progress.textContent="人工进度读取失败："+e.message; }
  }
  function update(context) {
    if(active?.draft.composing) { deferred=context; return; }
    latestContext=context;
    if(!context?.project || !context?.block) { active=null; stage.replaceChildren(); progress.replaceChildren(); ++progressRequest; return; }
    const key=JSON.stringify([context.project,context.block]);
    let entry=entries.get(key);
    if(!entry) { entry=create(context); entries.set(key,entry); }
    entry.draft.viewer=!!isViewer();
    entry.draft.source=copy(context.source);
    entry.rows.forEach(row=>{
      row.source.textContent=context.source.find(p=>p.id===row.id)?.text || "（原文段落已变，请重新载入并核对）";
    });
    // Retain the exact node (and textarea undo state) across polling and block navigation.
    if(active!==entry) { active=entry; stage.replaceChildren(entry.root); }
    notify(entry); pull(entry); loadProgress(context.project);
  }
  return {update,hasDirty:()=>[...entries.values()].some(e=>e.draft.dirty||e.draft.composing)};
}
