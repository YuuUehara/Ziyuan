import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { HumanDraft, draftStats, previewHtml, chapterProgress, summarizeParagraphIds, mountHumanEditor } from "./human-editor.mjs";

const source = [{ id: "p1", text: "原文" }];
const remote = (revision = null, text = "") => ({
  basis: { source_hash: "s", context_hash: "c" }, revision,
  draft: revision ? { revision, paragraphs: [{ id: "p1", text, marks: [] }] } : null,
  human: { approved: false, finalized: false, stale: false },
});
const storage = () => {
  const map = new Map();
  return { getItem: (k) => map.get(k) ?? null, setItem: (k, v) => map.set(k, v), removeItem: (k) => map.delete(k) };
};

test("entry exports the same mount and model used by the page", () => {
  assert.equal(typeof mountHumanEditor, "function");
});
test("page imports the actual editor, uses a persistent host and retains API conflict bodies", async () => {
  const html=await readFile(new URL("./index.html",import.meta.url),"utf8");
  assert.match(html,/import\(`\/assets\/human-editor\.mjs/);
  assert.ok(!html.includes('<script type="module">'), "page script must stay classic so a failed module load cannot break login");
  assert.match(html,/id="text-human-editor"/);
  assert.match(html,/id="text-model-drawer"/);
  assert.match(html,/<summary>逐段勘校（六事核对、审校参考与词证登记）<\/summary>/);
  assert.ok(!html.includes("<summary>展开逐段勘校"), "collapsible summary should not include redundant 展开 prefix");
  assert.match(html,/\.human-mark-tools\s*\{/);
  assert.match(html,/error\.body = data/);
  assert.match(html,/error\.current = data\.current/);
  assert.ok(!html.includes("Web 尚未开放人工"));
  assert.match(html,/humanEditor\.update\(/);
  assert.match(html,/定稿可选 TXT、Markdown 或 HTML/);
  assert.match(html,/value="html"[^>]*>HTML（保留文学标记）/);
  assert.match(html,/格式与编排只用于导出定稿/);
  assert.match(html,/取回原件固定按归档字节、逐章请求/);
  assert.match(html,/kind === "original" \? "txt" : d\.format/);
  assert.match(html,/kind === "original" \? "chapters" : d\.layout/);
  assert.ok(!html.includes("格式与编排不影响原件"));
});
test("whole draft limits include readings and every paragraph", () => {
  const paragraphs = ["a", "b"].map((id) => ({ id, text: "字", marks: Array.from({ length: 2049 }, () => ({start:0,end:1,kind:"strong"})) }));
  assert.match(draftStats(paragraphs).problems.join(), /4096/);
  const big = [{ id:"p", text:"a".repeat(131073), marks:[{start:0,end:1,kind:"ruby",reading:"b".repeat(131072)}] }];
  assert.match(draftStats(big).problems.join(), /256/);
  assert.deepEqual(draftStats([{ id:"p",text:"👩‍👩‍👦", marks:[{start:0,end:8,kind:"ruby",reading:"かな"}]}]).counts, {body:1,reading:2});
});
test("safe preview escapes both ruby body and reading", () => {
  const html = previewHtml({text:"<",marks:[{start:0,end:1,kind:"ruby",reading:"<img onerror=x>"}]});
  assert.ok(html.includes("&lt;")); assert.ok(!html.includes("<img"));
});
test("mirrors are scoped and recovery is explicit", () => {
  const store = storage(), a = new HumanDraft("a","b",source,store);
  a.receive(remote()); a.editText(0,"本地");
  const again = new HumanDraft("a","b",source,store); again.receive(remote("1","磁盘"));
  assert.equal(again.paragraphs[0].text,"磁盘"); assert.ok(again.recovery);
  assert.equal(new HumanDraft("a","other",source,store).recovery,null);
  again.restore(); assert.equal(again.paragraphs[0].text,"本地"); assert.equal(again.state,"conflict");
});
test("composition never validates or mirrors until its end", () => {
  const store = storage(), d = new HumanDraft("p","b",source,store); d.receive(remote());
  d.beginComposition(); d.editText(0,"中");
  assert.equal(store.getItem(d.key),null); assert.equal(d.paragraphs[0].text,"");
  d.endComposition(0,"中文");
  assert.equal(d.paragraphs[0].text,"中文"); assert.ok(store.getItem(d.key)); assert.equal(d.composing,false);
});
test("mark undo is bounded and text changes clear both histories", () => {
  const d = new HumanDraft("p","b",source,storage()); d.receive(remote("1","文字"));
  d.mark(0,0,1,"strong"); assert.equal(d.paragraphs[0].marks.length,1);
  d.undoMarks(); assert.equal(d.paragraphs[0].marks.length,0);
  d.redoMarks(); assert.equal(d.paragraphs[0].marks.length,1);
  for(let i=0;i<105;i++) { d.remove(0,0); d.mark(0,0,1,"strong"); }
  assert.equal(d.undo.length,100);
  d.editText(0,"新文字"); assert.equal(d.undo.length,0); assert.equal(d.redo.length,0);
});
test("polling keeps local text and basis; explicit rebase is required", () => {
  const d = new HumanDraft("p","b",source,storage()); d.receive(remote("1","旧"));
  d.editText(0,"本地"); const basis = d.basis;
  d.receive({...remote("2","外部"),basis:{source_hash:"new"}});
  assert.equal(d.state,"conflict"); assert.equal(d.revision,"1"); assert.deepEqual(d.basis,basis);
  d.keepLocal(); assert.equal(d.revision,"2"); assert.equal(d.paragraphs[0].text,"本地");
  d.loadDisk(); assert.equal(d.paragraphs[0].text,"外部");
});
test("busy, dirty, viewer and finalized block workflow writes", async () => {
  const d = new HumanDraft("p","b",source,storage()); d.receive(remote("1","译文"));
  const calls = []; const api = async (path,body) => { calls.push([path,body]); return { revision:"2" }; };
  d.editText(0,"未保存"); assert.equal(await d.action("submit",api),false);
  d.viewer=true; assert.equal(await d.save(api),false); d.viewer=false;
  d.remote.human.finalized=true; assert.equal(await d.action("approve",api),false);
  assert.equal(await d.save(api),false); assert.equal(calls.length,0);
});
test("save snapshots remain dirty if edited while request is pending and do not cross blocks", async () => {
  const d = new HumanDraft("project","block",source,storage()); d.receive(remote()); d.editText(0,"一");
  let finish; const pending = d.save(async(path,body) => { assert.match(path,/project\/blocks\/block\/human\/draft$/); assert.equal(body.expected_revision,null); return new Promise(r => finish=r); });
  assert.equal(d.state,"saving"); assert.equal(await d.save(() => assert.fail()),false);
  d.editText(0,"二"); finish({revision:"1"}); await pending;
  assert.equal(d.revision,"1"); assert.equal(d.state,"dirty"); assert.equal(d.paragraphs[0].text,"二");
});
test("409 retains current and message; disk/local resolution is never automatic", async () => {
  const d = new HumanDraft("p","b",source,storage()); d.receive(remote("1","旧")); d.editText(0,"本地");
  const current = remote("2","磁盘");
  await d.save(async() => { const e = new Error("revision_conflict"); e.status=409; e.body={current,message:"修订已改变"}; throw e; });
  assert.equal(d.state,"conflict"); assert.equal(d.paragraphs[0].text,"本地"); assert.equal(d.message,"修订已改变");
  assert.equal(d.revision,"1"); assert.deepEqual(d.conflictCurrent,current);
});
test("progress labels drafted versus historical submitted, collapses multi-chapter details, and avoids duplicating single-chapter totals", () => {
  const html = chapterProgress({chapters:[{chapter:"一",total:4,drafted:2,submitted:3,approved:1,finalized:0,not_started:2}],next_untranslated:"b"});
  assert.match(html,/已起草 2/); assert.match(html,/历史送审 3/); assert.match(html,/下一未起草/);
  const single = chapterProgress({totals:{total:19,drafted:1,submitted:0,approved:0,finalized:0,not_started:18},chapters:[{chapter:"c0001",total:19,drafted:1,submitted:0,approved:0,finalized:0,not_started:18}],next_untranslated:"c0001-b0002"});
  assert.ok(!single.includes("第1章："), "single chapter should not repeat identical totals line");
  assert.ok(!single.includes("各章进度"));
  const multi = chapterProgress({totals:{total:10,drafted:2,submitted:0,approved:0,finalized:0,not_started:8},chapters:[{chapter:"c0001",total:5,drafted:2,submitted:0,approved:0,finalized:0,not_started:3},{chapter:"c0002",total:5,drafted:0,submitted:0,approved:0,finalized:0,not_started:5}],next_untranslated:"c0001-b0003"});
  assert.match(multi, /各章进度（2 章）/);
  assert.match(multi, /第1章：共 5/);
  assert.match(multi, /第2章：共 5/);
});
test("GET started before a successful write cannot roll the state back", async () => {
  const d=new HumanDraft("p","b",source,storage()); d.receive(remote("1","旧"));
  const epoch=d.epoch; d.editText(0,"新");
  await d.save(async()=>({revision:"2"}));
  d.receive(remote("1","旧"),epoch);
  assert.equal(d.state,"saved"); assert.equal(d.remote.revision,"1");
  assert.equal(d.revision,"2"); assert.equal(d.conflict,false);
});
test("malformed recovery is rejected without altering current text", () => {
  const store=storage(), d=new HumanDraft("p","b",source,store); d.receive(remote("1","磁盘"));
  d.recovery={basis:{},revision:"1",paragraphs:[{id:"p1",text:"坏镜像",marks:[null,null]}]};
  assert.equal(d.restore(),false); assert.equal(d.paragraphs[0].text,"磁盘");
});
test("pending recovery cannot be silently overwritten by typing or saving", async () => {
  const store=storage(), first=new HumanDraft("p","b",source,store); first.receive(remote()); first.editText(0,"待恢复");
  const d=new HumanDraft("p","b",source,store); d.receive(remote("1","磁盘"));
  const mirror=store.getItem(d.key);
  assert.equal(d.editText(0,"覆盖"),false);
  assert.equal(await d.save(()=>assert.fail("must first choose recovery")),false);
  assert.equal(store.getItem(d.key),mirror);
  d.dismissRecovery(); assert.equal(d.editText(0,"明确忽略后编辑"),true);
});
test("saving also works on HTTP origins without crypto.randomUUID", async () => {
  const descriptor=Object.getOwnPropertyDescriptor(globalThis,"crypto");
  const crypto=globalThis.crypto;
  Object.defineProperty(globalThis,"crypto",{configurable:true,value:{getRandomValues:crypto.getRandomValues.bind(crypto)}});
  try {
    const d=new HumanDraft("p","b",source,storage()); d.receive(remote());
    assert.equal(await d.save(async(path,body)=>{assert.match(body.request_id,/^[a-zA-Z0-9-]+$/);return {revision:"1"};}),true);
  } finally { Object.defineProperty(globalThis,"crypto",descriptor); }
});

test("a clean editor follows disk changes (e.g. approval landing zh.json) without a false conflict", () => {
  const d = new HumanDraft("p","b",source,storage()); d.receive(remote("1","译文"));
  d.receive({...remote("1","译文"),basis:{source_hash:"s",context_hash:"c",translation_hash:"new"}});
  assert.equal(d.state,"saved"); assert.equal(d.conflict,false);
  assert.deepEqual(d.basis,{source_hash:"s",context_hash:"c",translation_hash:"new"});
  d.receive(remote("2","外部"));
  assert.equal(d.conflict,false); assert.equal(d.paragraphs[0].text,"外部"); assert.equal(d.revision,"2");
});
test("a successful mark clears a stale error message", () => {
  const d = new HumanDraft("p","b",source,storage()); d.receive(remote("1","文字"));
  d.message="旧错误"; d.mark(0,0,1,"strong"); assert.equal(d.message,"");
});

test("a successful save clears the session mirror and an identical mirror is not offered for recovery", async () => {
  const store = storage(), d = new HumanDraft("p","b",source,store); d.receive(remote());
  d.editText(0,"已存"); assert.ok(store.getItem(d.key));
  await d.save(async()=>({revision:"1"}));
  assert.equal(store.getItem(d.key), null);
  store.setItem(d.key, JSON.stringify({paragraphs:[{id:"p1",text:"已存",marks:[]}],basis:remote().basis,revision:"1"}));
  const again = new HumanDraft("p","b",source,store); again.receive(remote("1","已存"));
  assert.equal(again.recovery, null); assert.equal(store.getItem(again.key), null);
});

test("mounted editor hides empty mark details, labels initial state, and gates workflow buttons by submitted/approved", async () => {
  const makeEl = (tag, doc) => {
    const node = {
      tagName: tag.toUpperCase(), className: "", hidden: false, disabled: false, readOnly: false,
      value: "", textContent: "", _innerHTML: "", dataset: {}, attributes: {}, children: [],
      listeners: new Map(), parentElement: null, ownerDocument: doc,
      get innerHTML() { return this._innerHTML; },
      set innerHTML(v) {
        this._innerHTML = String(v);
        if (this.className === "human-paragraph") {
          const tx = makeEl("textarea", doc);
          const src = makeEl("p", doc); src.className = "text-para";
          const tools = makeEl("div", doc); tools.className = "actions human-mark-tools";
          const preview = makeEl("p", doc); preview.className = "human-preview text-para";
          const tableWrap = makeEl("div", doc); tableWrap.className = "table-wrap";
          const table = makeEl("table", doc);
          tableWrap.append(table);
          this.children = [src, tx, tools, preview, tableWrap];
          this._map = { textarea: tx, ".human-mark-tools": tools, table, ".text-para": src, ".human-preview": preview, ".table-wrap": tableWrap };
        } else if (this._innerHTML.includes("human-paragraphs")) {
          const status = makeEl("p", doc); status.className = "human-status";
          const msg = makeEl("p", doc); msg.className = "human-message hint";
          const rec = makeEl("div", doc); rec.className = "human-recovery actions";
          const conf = makeEl("div", doc); conf.className = "human-conflict actions";
          const acts = makeEl("div", doc); acts.className = "human-actions actions";
          const paras = makeEl("div", doc); paras.className = "human-paragraphs";
          this.children = [status, msg, rec, conf, acts, paras];
          this._map = { ".human-status": status, ".human-message": msg, ".human-recovery": rec, ".human-conflict": conf, ".human-actions": acts, ".human-paragraphs": paras };
        }
      },
      setAttribute(k, v) { this.attributes[k] = v; },
      append(...items) { items.forEach((i) => { i.parentElement = this; this.children.push(i); }); },
      replaceChildren(...items) { this.children = []; this.append(...items); },
      addEventListener(type, fn) {
        if (!this.listeners.has(type)) this.listeners.set(type, []);
        this.listeners.get(type).push(fn);
      },
      querySelector(sel) { return this._map?.[sel] || null; },
      querySelectorAll(sel) {
        const out = [];
        const walk = (n) => {
          for (const c of n.children || []) {
            if (sel === "textarea" && c.tagName === "TEXTAREA") out.push(c);
            if (sel.includes("[data-mark]") && (c.dataset.mark || c.dataset.remove !== undefined)) out.push(c);
            walk(c);
          }
        };
        walk(this);
        return out;
      },
      closest() { return null; },
      focus() {},
    };
    return node;
  };
  const doc = {}; doc.createElement = (tag) => makeEl(tag, doc);
  const host = makeEl("div", doc);
  let resolvePull;
  const pullPromise = new Promise((r) => { resolvePull = r; });
  let apiCalls = 0;
  const api = async (path, body) => {
    apiCalls++;
    if (apiCalls === 1) return pullPromise;
    if (path.endsWith("/draft")) return { revision: "rev-2" };
    return { basis: { source_hash: "s", context_hash: "c" }, revision: "rev-1", draft: { revision: "rev-1", paragraphs: [{ id: "p1", text: "译文", marks: [] }] }, human: { submitted: true, approved: false, finalized: false, stale: false } };
  };
  const editor = mountHumanEditor(host, { api, isViewer: () => false, storage: storage(), confirm: () => true });
  editor.update({ project: "p", block: "b1", source: [{ id: "p1", text: "原文" }] });
  const stage = host.children[1];
  const root = stage.children[0];
  assert.match(root.querySelector(".human-status").textContent, /^正在读取人工草稿…/);
  const paraSection = root.querySelector(".human-paragraphs").children[0];
  assert.equal(paraSection.querySelector(".human-preview").hidden, true);
  assert.equal(paraSection.querySelector(".table-wrap").hidden, true);

  resolvePull({ ...remote(null, ""), human: { submitted: false, approved: false, finalized: false, stale: false } });
  await new Promise((r) => setTimeout(r, 0));
  assert.match(root.querySelector(".human-status").textContent, /^尚无草稿 · 正文字符 0/);

  const btns = Object.fromEntries(root.querySelector(".human-actions").children.map((b) => [b.dataset.action, b]));
  assert.equal(btns.submit.disabled, true);
  assert.equal(btns.approve.disabled, true);
  assert.equal(btns.finalize.disabled, true);

  apiCalls = 99; // subsequent pulls return submitted:true
  editor.update({ project: "p", block: "b2", source: [{ id: "p1", text: "原文" }] });
  await new Promise((r) => setTimeout(r, 0));
  const root2 = stage.children[0];
  const btns2 = Object.fromEntries(root2.querySelector(".human-actions").children.map((b) => [b.dataset.action, b]));
  assert.match(root2.querySelector(".human-status").textContent, /^已保存 · 修订 rev-1/);
  assert.equal(btns2.submit.disabled, true, "already submitted revision disables submit");
  assert.equal(btns2.approve.disabled, false, "submitted revision enables approve");
  assert.equal(btns2.finalize.disabled, true, "unapproved revision disables finalize");
});

test("summarizeParagraphIds condenses contiguous ranges and submit blocks empty paragraphs before confirm", async () => {
  const ids = Array.from({ length: 12 }, (_, i) => `c0001-p${String(i + 3).padStart(4, "0")}`);
  assert.equal(summarizeParagraphIds(ids), "第1章 · 第3～14段");
  assert.equal(summarizeParagraphIds(["c0001-p0002", "c0001-p0004", "c0001-p0005"]), "第1章 · 第2段、第4～5段");

  let focused = null;
  let confirmCount = 0;
  let submitPosts = 0;
  const makeEl = (tag, doc) => {
    const node = {
      tagName: tag.toUpperCase(), className: "", hidden: false, disabled: false, readOnly: false,
      value: "", textContent: "", _innerHTML: "", dataset: {}, attributes: {}, children: [],
      listeners: new Map(), parentElement: null, ownerDocument: doc,
      get innerHTML() { return this._innerHTML; },
      set innerHTML(v) {
        this._innerHTML = String(v);
        if (this.className === "human-paragraph") {
          const tx = makeEl("textarea", doc);
          const pid = this.dataset.humanPara;
          tx.focus = () => { focused = pid; };
          const src = makeEl("p", doc); src.className = "text-para";
          const tools = makeEl("div", doc); tools.className = "actions human-mark-tools";
          const preview = makeEl("p", doc); preview.className = "human-preview text-para";
          const tableWrap = makeEl("div", doc); tableWrap.className = "table-wrap";
          const table = makeEl("table", doc);
          tableWrap.append(table);
          this.children = [src, tx, tools, preview, tableWrap];
          this._map = { textarea: tx, ".human-mark-tools": tools, table, ".text-para": src, ".human-preview": preview, ".table-wrap": tableWrap };
        } else if (this._innerHTML.includes("human-paragraphs")) {
          const status = makeEl("p", doc); status.className = "human-status";
          const msg = makeEl("p", doc); msg.className = "human-message hint";
          const rec = makeEl("div", doc); rec.className = "human-recovery actions";
          const conf = makeEl("div", doc); conf.className = "human-conflict actions";
          const acts = makeEl("div", doc); acts.className = "human-actions actions";
          const paras = makeEl("div", doc); paras.className = "human-paragraphs";
          this.children = [status, msg, rec, conf, acts, paras];
          this._map = { ".human-status": status, ".human-message": msg, ".human-recovery": rec, ".human-conflict": conf, ".human-actions": acts, ".human-paragraphs": paras };
        }
      },
      setAttribute(k, v) { this.attributes[k] = v; },
      append(...items) { items.forEach((i) => { i.parentElement = this; this.children.push(i); }); },
      replaceChildren(...items) { this.children = []; this.append(...items); },
      addEventListener(type, fn) {
        if (!this.listeners.has(type)) this.listeners.set(type, []);
        this.listeners.get(type).push(fn);
      },
      querySelector(sel) { return this._map?.[sel] || null; },
      querySelectorAll(sel) {
        const out = [];
        const walk = (n) => {
          for (const c of n.children || []) {
            if (sel === "textarea" && c.tagName === "TEXTAREA") out.push(c);
            if (sel.includes("[data-mark]") && (c.dataset.mark || c.dataset.remove !== undefined)) out.push(c);
            walk(c);
          }
        };
        walk(this);
        return out;
      },
      closest(sel) {
        if (sel === "[data-action]" && this.dataset?.action) return this;
        return null;
      },
      focus() {},
    };
    return node;
  };
  const doc = {}; doc.createElement = (tag) => makeEl(tag, doc);
  const host = makeEl("div", doc);
  const src = [
    { id: "c0001-p0001", text: "原文1" },
    { id: "c0001-p0002", text: "原文2" },
    { id: "c0001-p0003", text: "原文3" },
  ];
  const api = async (path) => {
    if (path.endsWith("/submit")) { submitPosts++; return {}; }
    if (path.endsWith("/human/progress")) return null;
    return {
      basis: { source_hash: "s", context_hash: "c" },
      revision: "rev-1",
      draft: {
        revision: "rev-1",
        paragraphs: [
          { id: "c0001-p0001", text: "译1", marks: [] },
          { id: "c0001-p0002", text: "", marks: [] },
          { id: "c0001-p0003", text: "   ", marks: [] },
        ],
      },
      human: { submitted: false, approved: false, finalized: false, stale: false },
    };
  };
  const editor = mountHumanEditor(host, {
    api,
    isViewer: () => false,
    storage: storage(),
    confirm: () => { confirmCount++; return true; },
  });
  editor.update({ project: "p", block: "c0001-b0001", source: src });
  await new Promise((r) => setTimeout(r, 0));

  const root = host.children[1].children[0];
  const submitBtn = root.querySelector(".human-actions").children.find((b) => b.dataset.action === "submit");
  for (const fn of root.listeners.get("click") || []) {
    await fn({ target: submitBtn });
  }
  const msg = root.querySelector(".human-message");
  assert.equal(confirmCount, 0);
  assert.equal(submitPosts, 0);
  assert.equal(msg.dataset.tone, "error");
  assert.match(msg.textContent, /还有 2 段尚未填写译文（第1章 · 第2～3段）/);
  assert.equal(focused, "c0001-p0002");
});
