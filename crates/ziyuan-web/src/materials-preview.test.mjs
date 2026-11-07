import test from "node:test";
import assert from "node:assert/strict";
import { mountMaterialEditor } from "./materials-editor.mjs";
import { mountMaterialPreview } from "./materials-preview.mjs";

const clone = (value) => JSON.parse(JSON.stringify(value));
const hex = (digit) => digit.repeat(64);
const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

class FakeEvent {
  constructor(type, init = {}) {
    Object.assign(this, init);
    this.type = type;
    this.defaultPrevented = false;
  }
  preventDefault() { this.defaultPrevented = true; }
}

class FakeElement {
  constructor(ownerDocument, tagName) {
    this.ownerDocument = ownerDocument;
    this.tagName = tagName.toUpperCase();
    this.children = [];
    this.parentNode = null;
    this.dataset = {};
    this.attributes = new Map();
    this.listeners = new Map();
    this._text = "";
    this.value = "";
    this.checked = false;
    this.hidden = false;
    this.disabled = false;
    this.readOnly = false;
    this.type = "";
    this.className = "";
  }
  get textContent() { return this._text + this.children.map((child) => child.textContent).join(""); }
  set textContent(value) {
    for (const child of this.children) child.parentNode = null;
    this.children = [];
    this._text = String(value);
  }
  get innerHTML() { throw new Error("innerHTML is not available"); }
  set innerHTML(_) { throw new Error("innerHTML is not available"); }
  append(...nodes) {
    for (const node of nodes) {
      assert.ok(node instanceof FakeElement, "only elements may be appended");
      node.remove();
      node.parentNode = this;
      this.children.push(node);
    }
  }
  replaceChildren(...nodes) {
    for (const child of this.children) child.parentNode = null;
    this.children = [];
    this._text = "";
    this.append(...nodes);
  }
  remove() {
    if (!this.parentNode) return;
    const siblings = this.parentNode.children;
    siblings.splice(siblings.indexOf(this), 1);
    this.parentNode = null;
  }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  removeAttribute(name) { this.attributes.delete(name); }
  addEventListener(type, listener) {
    if (!this.listeners.has(type)) this.listeners.set(type, []);
    this.listeners.get(type).push(listener);
  }
  removeEventListener(type, listener) {
    const list = this.listeners.get(type) ?? [];
    if (list.includes(listener)) list.splice(list.indexOf(listener), 1);
  }
  focus() { this.ownerDocument.activeElement = this; }
  get isConnected() {
    let node = this;
    while (node.parentNode) node = node.parentNode;
    return node === this.ownerDocument.body;
  }
  dispatch(type, init = {}) {
    const event = new FakeEvent(type, { target: this, ...init });
    for (const listener of [...(this.listeners.get(type) ?? [])]) listener(event);
    return event;
  }
}

class FakeDocument {
  constructor() {
    this.activeElement = null;
    this.body = new FakeElement(this, "body");
  }
  createElement(tagName) { return new FakeElement(this, tagName); }
}

const all = (root, predicate) => {
  const found = [];
  const walk = (node) => { if (predicate(node)) found.push(node); node.children.forEach(walk); };
  walk(root);
  return found;
};
const one = (root, predicate, label) => {
  const found = all(root, predicate);
  assert.equal(found.length, 1, `expected exactly one ${label}, found ${found.length}`);
  return found[0];
};
const role = (root, name) => one(root, (node) => node.dataset.role === name, name);
const maybeRole = (root, name) => all(root, (node) => node.dataset.role === name)[0];
const choose = (node, value) => { node.value = value; node.dispatch("change"); };
const check = (node, value = true) => { node.checked = value; node.dispatch("change"); };
const click = (node) => node.dispatch("click");
const type = (node, value) => { node.value = value; node.dispatch("input"); };
const association = (root, kind, paragraph, id) => one(root, (node) =>
  node.dataset.association === kind && node.dataset.paragraph === paragraph && node.dataset.id === id,
`${kind} ${paragraph} ${id}`);
const reportEntries = (root, group, kind) => all(root, (node) =>
  node.dataset.reportGroup === group && node.dataset.reportKind === kind);

function storage() {
  const values = new Map();
  return {
    values,
    getItem(key) { return values.get(key) ?? null; },
    setItem(key, value) { values.set(key, value); },
    removeItem(key) { values.delete(key); },
  };
}

function position(paragraph) {
  return { chapter: `c0${Math.ceil(Number(paragraph.slice(1)) / 3)}`, paragraph };
}

// Independent hand-written N0 input. No preview result is used to construct it.
function n0Material() {
  const terms = [
    ["ren", "蓮", "莲", "p02", "p09", "confirmed", "person_name"],
    ["lantern-keeper", "灯守", "守灯人", "p04", "p09", "confirmed", "person_name"],
    ["mio", "美緒", "美绪", "p07", "p09", "confirmed", "person_name"],
    ["sister", "姉さん", "姐姐", "p05", "p05", "confirmed", "common"],
    ["moon-lantern", "月舟", "月舟", "p08", "p09", "tentative", "common"],
  ].map(([term_id, source, allowed, from, to, status, category]) => ({
    term_id, source, allowed: [allowed], category,
    available_from: position(from),
    scope: { kind: "range", from: position(from), to: position(to) },
    evidence: [], status,
  }));
  const persons = [
    ["woman", "未明身份的女人", ["lantern-keeper", "mio"], "p01"],
    ["ren-captain", "蓮", ["ren"], "p02"],
    ["ren-healer", "蓮", ["ren"], "p03"],
    ["brother", "弟", [], "p05"],
    ["supervisor", "監督", [], "p06"],
  ].map(([person_id, name, terms, from]) => ({
    person_id, name, aliases: [], terms, available_from: position(from), address_rules: [],
    voice: { description: "自创样本的人工说明" }, evidence: [], status: "confirmed",
  }));
  persons[0].address_rules = [{
    target: "woman", speaker: { kind: "person", person_id: "brother" },
    scope: { kind: "range", from: position("p05"), to: position("p05") },
    term: "sister", evidence: [],
  }];
  return { format: 2, terms, persons };
}

function n0Project(id = "n0") {
  const rows = [
    ["c01", "b01", [["p01", "港では、名を明かさない女が灯台を見ていた。"],
      ["p02", "船長の蓮は言った。「今夜は出ない。」"]]],
    ["c01", "b02", [["p03", "薬師の蓮は、船長とは別人だった。"]]],
    ["c02", "b03", [["p04", "皆はその女を「灯守」と呼んだ。"],
      ["p05", "弟は灯守を「姉さん」と呼んだ。"], ["p06", "監督は灯守に言った。「灯守さん、こちらへ。」"]]],
    ["c03", "b04", [["p07", "昨日のことを思い出した。女の本名は美緒だった。"],
      ["p08", "この町で「月舟」は祭りの灯籠を指す。船ではない。"]]],
    ["c03", "b05", [["p09", "蓮は月舟を持って港へ戻った。"]]],
  ];
  const chapters = [];
  for (const [chapter, block_id, paragraphs] of rows) {
    let target = chapters.find((item) => item.id === chapter);
    if (!target) {
      target = { id: chapter, blocks: [], paragraphs: [] };
      chapters.push(target);
    }
    const mapped = paragraphs.map(([paragraph, text]) => ({ id: paragraph, text }));
    target.blocks.push({ block_id, paragraphs: mapped });
    target.paragraphs.push(...mapped);
  }
  return { id, chapters };
}

function n0View(material = n0Material(), { project = "n0", workspace = hex("a"), projectHash = hex("f"), revision = "r1" } = {}) {
  return {
    workspace_id: workspace, project_hash: projectHash, project: n0Project(project),
    draft: { project, revision, material },
  };
}

function selection({ terms = [], persons = [], address_rules = [] } = {}) {
  return { terms, persons, address_rules };
}
const term = (term_id, status, paragraphs) => ({ term_id, status, paragraphs });
const person = (person_id, status, paragraphs) => ({ person_id, status, paragraphs });

// Fixed N0 expectations copied by hand from preview.rs, never generated by JS input.
function n0Report(block = "b03") {
  if (block === "b03") return {
    project: "n0", block,
    applicable: selection({
      terms: [term("ren", "confirmed", ["p02", "p03", "p04", "p05", "p06", "p07", "p08", "p09"]),
        term("lantern-keeper", "confirmed", ["p04", "p05", "p06"])],
      persons: [person("woman", "confirmed", ["p04"])],
    }),
    automatic: selection({
      terms: [term("ren", "confirmed", ["p04", "p05", "p06"]),
        term("lantern-keeper", "confirmed", ["p04", "p05", "p06"])],
    }),
    diagnostics: [],
  };
  return {
    project: "n0", block,
    applicable: selection({
      terms: [term("moon-lantern", "tentative", ["p08", "p09"])],
      persons: [person("woman", "confirmed", ["p07"]),
        person("ren-captain", "confirmed", ["p07"]), person("ren-healer", "confirmed", ["p07"])],
    }),
    automatic: selection({
      persons: [person("woman", "confirmed", ["p07"]),
        person("ren-captain", "confirmed", ["p07"]), person("ren-healer", "confirmed", ["p07"])],
    }),
    diagnostics: [],
  };
}

function response(block = "b03", projectHash = hex("f")) {
  return {
    binding: { content_hash: hex("c"), project_hash: projectHash,
      block_hash: hex("b"), associations_hash: hex("d") },
    report: n0Report(block),
  };
}

function setup({
  material = n0Material(), view = n0View(material), viewer = false,
  getView, runPreview,
} = {}) {
  const doc = new FakeDocument();
  const editorHost = doc.createElement("div");
  const host = doc.createElement("div");
  doc.body.append(editorHost, host);
  const store = storage();
  const requests = [];
  const gets = [];
  const editor = mountMaterialEditor(editorHost, { storage: store, viewer });
  editor.update(clone(view));
  const preview = mountMaterialPreview(host, {
    editor, viewer,
    getView: getView ?? (async () => { gets.push("get"); return clone(view); }),
    runPreview: runPreview ?? (async (request) => {
      requests.push(clone(request));
      return response(request.block, view.project_hash);
    }),
  });
  return { doc, editorHost, host, editor, preview, store, requests, gets, view };
}

async function runAt(ctx, block) {
  choose(role(ctx.host, "block"), block);
  const result = await ctx.preview.run();
  assert.equal(result, true);
}

test("local card is complete and fixed-warning N0 report keeps applicable and automatic independent", async () => {
  const ctx = setup();
  const card = role(ctx.host, "material-card");
  assert.deepEqual(JSON.parse(card.textContent), n0Material());
  assert.match(role(ctx.host, "local-warning").textContent, /完整本地资料/);
  assert.match(role(ctx.host, "local-warning").textContent, /不会批准|不是批准/);
  await runAt(ctx, "b03");

  assert.deepEqual(reportEntries(ctx.host, "applicable", "persons").map((node) => node.dataset.id), ["woman"]);
  assert.deepEqual(reportEntries(ctx.host, "automatic", "persons").map((node) => node.dataset.id), []);
  assert.deepEqual(reportEntries(ctx.host, "applicable", "terms").map((node) => node.dataset.id),
    ["ren", "lantern-keeper"]);
  const report = role(ctx.host, "identity-report").textContent;
  for (const secret of ["未明身份的女人", "蓮", "美緒", "美绪", "守灯人", "自创样本的人工说明"]) {
    assert.equal(report.includes(secret), false, `identity report leaked ${secret}`);
  }
  assert.match(role(ctx.host, "preview-project-hash").textContent, new RegExp(hex("f")));
  assert.match(role(ctx.host, "preview-warning").textContent, /本次输入|外部磁盘/);
  ctx.editor.flushMirror();
  assert.equal(ctx.store.values.size, 0, "preview and associations never enter clean session mirrors");
});

test("range controls use concrete block and paragraph IDs and four-state speakers are not conflated", async () => {
  const ctx = setup();
  choose(role(ctx.host, "block"), "b04");
  assert.deepEqual(all(role(ctx.host, "paragraphs"), (node) => node.dataset.paragraph)
    .map((node) => [node.dataset.paragraph, node.textContent]), [
    ["p07", "p07 · 昨日のことを思い出した。女の本名は美緒だった。"],
    ["p08", "p08 · この町で「月舟」は祭りの灯籠を指す。船ではない。"],
  ]);
  for (const id of ["woman", "ren-captain", "ren-healer"]) {
    check(association(ctx.host, "persons", "p07", id));
  }
  check(association(ctx.host, "targets", "p07", "woman"));
  const p07Speaker = one(ctx.host, (node) => node.dataset.role === "speaker" && node.dataset.paragraph === "p07", "p07 speaker");
  const p08Speaker = one(ctx.host, (node) => node.dataset.role === "speaker" && node.dataset.paragraph === "p08", "p08 speaker");
  assert.deepEqual(p07Speaker.children.map((node) => node.value), ["", "person", "narration", "unknown"]);
  choose(p07Speaker, "person");
  choose(one(ctx.host, (node) => node.dataset.role === "speaker-person" && node.dataset.paragraph === "p07", "speaker person"), "brother");
  choose(p08Speaker, "unknown");
  assert.equal(await ctx.preview.run(), true);
  assert.equal(ctx.requests.length, 1);
  assert.deepEqual(ctx.requests[0], {
    workspace_id: hex("a"), material: n0Material(), block: "b04",
    associations: [
      { paragraph: "p07", persons: ["woman", "ren-captain", "ren-healer"], targets: ["woman"],
        speaker: { kind: "person", person_id: "brother" } },
      { paragraph: "p08", persons: [], targets: [], speaker: { kind: "unknown" } },
    ],
  });
  assert.deepEqual(reportEntries(ctx.host, "automatic", "persons").map((node) => node.dataset.id),
    ["woman", "ren-captain", "ren-healer"]);
  const moon = reportEntries(ctx.host, "applicable", "terms").find((node) => node.dataset.id === "moon-lantern");
  assert.match(moon.textContent, /tentative/);
  assert.match(moon.textContent, /p08, p09/);
  assert.equal(reportEntries(ctx.host, "automatic", "terms").some((node) => node.dataset.id === "moon-lantern"), false);
  assert.equal(Object.hasOwn(ctx.requests[0].associations[0], "speaker"), true);
  assert.equal(ctx.requests[0].associations[1].speaker.kind, "unknown");
});

test("unspecified speaker is omitted and refresh never restores temporary associations", async () => {
  const ctx = setup();
  choose(role(ctx.host, "block"), "b04");
  check(association(ctx.host, "persons", "p07", "woman"));
  const speaker = one(ctx.host, (node) => node.dataset.role === "speaker" && node.dataset.paragraph === "p07", "speaker");
  choose(speaker, "unknown");
  choose(speaker, "");
  assert.equal(await ctx.preview.run(), true);
  assert.deepEqual(ctx.requests[0].associations, [
    { paragraph: "p07", persons: ["woman"], targets: [] },
    { paragraph: "p08", persons: [], targets: [] },
  ]);
  assert.equal(Object.hasOwn(ctx.requests[0].associations[0], "speaker"), false);
  assert.equal(association(ctx.host, "persons", "p07", "woman").checked, true);
  assert.equal(await ctx.preview.refresh(), true);
  assert.equal(association(ctx.host, "persons", "p07", "woman").checked, false);
  assert.equal(maybeRole(ctx.host, "identity-report"), undefined);
  assert.equal(ctx.gets.length, 2, "manual run re-reads GET and explicit refresh reads it again");
});

test("viewer can run temporary previews without modifying or mirroring server material", async () => {
  const ctx = setup({ viewer: true });
  assert.equal(role(ctx.host, "run").disabled, false);
  choose(role(ctx.host, "block"), "b04");
  check(association(ctx.host, "persons", "p07", "woman"));
  assert.equal(await ctx.preview.run(), true);
  assert.equal(ctx.requests.length, 1);
  assert.deepEqual(ctx.requests[0].material, n0Material());
  assert.deepEqual(JSON.parse(role(ctx.host, "material-card").textContent), n0Material());
  assert.equal(ctx.editor.hasDirty(), false);
  ctx.editor.flushMirror();
  assert.equal(ctx.store.values.size, 0);
});

test("two interleaved responses cannot let the older request overwrite the newer report", async () => {
  const pending = [];
  const ctx = setup({
    runPreview: (request) => new Promise((resolve) => pending.push({ request, resolve })),
  });
  choose(role(ctx.host, "block"), "b03");
  const first = ctx.preview.run();
  await tick();
  choose(role(ctx.host, "block"), "b04");
  const second = ctx.preview.run();
  await tick();
  assert.equal(pending.length, 2);
  pending[1].resolve(response("b04"));
  assert.equal(await second, true);
  pending[0].resolve(response("b03"));
  assert.equal(await first, false);
  assert.equal(role(ctx.host, "identity-report").dataset.block, "b04");
  assert.deepEqual(reportEntries(ctx.host, "automatic", "persons").map((node) => node.dataset.id),
    ["woman", "ren-captain", "ren-healer"]);
});

test("editing material while running invalidates the response and rule indices stay bound to current input", async () => {
  let resolve;
  const ctx = setup({ runPreview: () => new Promise((done) => { resolve = done; }) });
  choose(role(ctx.host, "block"), "b03");
  const running = ctx.preview.run();
  await tick();
  const changed = n0Material();
  changed.persons[0].address_rules[0].term = "ren";
  const result = response("b03");
  result.report.applicable.address_rules = [{ person_id: "woman", rule_index: 0,
    person_status: "confirmed", term_status: "confirmed", paragraphs: ["p05"] }];
  type(one(ctx.editorHost, (node) => node.dataset.field === "source", "editor source"), "蓮（改）");
  assert.equal(maybeRole(ctx.host, "identity-report"), undefined);
  assert.match(role(ctx.host, "preview-status").textContent, /失效/);
  resolve(result);
  assert.equal(await running, false);
  assert.equal(maybeRole(ctx.host, "identity-report"), undefined);
  assert.equal(ctx.editor.hasDirty(), true);
});

test("project and workspace switches invalidate late results and temporary input", async () => {
  let resolve;
  const ctx = setup({ runPreview: () => new Promise((done) => { resolve = done; }) });
  choose(role(ctx.host, "block"), "b03");
  check(association(ctx.host, "persons", "p04", "woman"));
  const running = ctx.preview.run();
  await tick();
  const other = n0View(n0Material(), { project: "other", workspace: hex("e"), projectHash: hex("1") });
  ctx.preview.update(other);
  assert.equal(role(ctx.host, "block").value, "b01");
  assert.equal(association(ctx.host, "persons", "p01", "woman").checked, false);
  resolve(response("b03"));
  assert.equal(await running, false);
  assert.equal(maybeRole(ctx.host, "identity-report"), undefined);
  assert.equal(JSON.parse(role(ctx.host, "material-card").textContent).terms[0].term_id, "ren");
});

test("GET failure on entry, focus, or manual run clears success and never posts", async () => {
  let fail = false;
  let calls = 0;
  const ctx = setup({
    getView: async () => { calls++; if (fail) throw new Error("disk unavailable"); return n0View(); },
  });
  await runAt(ctx, "b03");
  fail = true;
  assert.equal(await ctx.preview.enter(), false);
  assert.equal(maybeRole(ctx.host, "identity-report"), undefined);
  assert.match(role(ctx.host, "preview-status").textContent, /disk unavailable/);
  assert.equal(await ctx.preview.focus(), false);
  assert.equal(await ctx.preview.run(), false);
  assert.equal(calls, 4, "initial run, entry, focus, and manual run each re-read GET");
});

test("project hash changes invalidate old reports and invalid responses never reuse success", async () => {
  const results = [];
  let current = n0View();
  const ctx = setup({ getView: async () => clone(current), runPreview: async () => results.shift() });
  results.push(response("b03"));
  await runAt(ctx, "b03");
  const changed = n0View(n0Material(), { projectHash: hex("2"), revision: "r2" });
  current = changed;
  ctx.preview.update(changed);
  assert.equal(maybeRole(ctx.host, "identity-report"), undefined);
  assert.match(role(ctx.host, "preview-status").textContent, /失效/);

  const invalid = [
    { ...response("b03", hex("2")), report: { ...n0Report("b03"), project: "spoofed" } },
    { ...response("b03", hex("2")), report: { ...n0Report("b03"), block: "b04" } },
    { ...response("b03", hex("2")), binding: { ...response("b03", hex("2")).binding, project_hash: hex("f") } },
    (() => { const value = response("b03", hex("2")); value.report.applicable.terms[0].name = "蓮"; return value; })(),
    (() => { const value = response("b03", hex("2")); value.report.diagnostics = [{ text: "正文" }]; return value; })(),
    null,
  ];
  for (const value of invalid) {
    results.push(value, response("b03", hex("2")));
    assert.equal(await ctx.preview.run(), false);
    assert.equal(maybeRole(ctx.host, "identity-report"), undefined);
    assert.match(role(ctx.host, "preview-status").textContent, /无效|不匹配/);
    assert.equal(await ctx.preview.run(), true, "a valid later response must still render");
    assert.equal(role(ctx.host, "identity-report").dataset.block, "b03");
  }
});

test("safe DOM rendering shows XSS strings as text without creating executable elements", async () => {
  const malicious = "<img src=x onerror=globalThis.pwned=true><script>globalThis.pwned=true</script>";
  const material = n0Material();
  material.terms[0].source = malicious;
  material.persons[0].name = malicious;
  const result = response("b03");
  result.report.applicable.persons[0].person_id = malicious;
  const ctx = setup({ material, view: n0View(material), runPreview: async () => result });
  await runAt(ctx, "b03");
  assert.match(role(ctx.host, "material-card").textContent, /<img src=x/);
  assert.match(role(ctx.host, "identity-report").textContent, /<script>/);
  assert.equal(all(ctx.host, (node) => ["IMG", "SCRIPT"].includes(node.tagName)).length, 0);
  assert.equal(globalThis.pwned, undefined);
});

test("dispose detaches editor subscription and ignores a late response", async () => {
  let resolve;
  const ctx = setup({ runPreview: () => new Promise((done) => { resolve = done; }) });
  const running = ctx.preview.run();
  await tick();
  ctx.preview.dispose();
  type(one(ctx.editorHost, (node) => node.dataset.field === "source", "editor source"), "after dispose");
  resolve(response("b01"));
  assert.equal(await running, false);
  assert.equal(ctx.host.children.length, 0);
});
