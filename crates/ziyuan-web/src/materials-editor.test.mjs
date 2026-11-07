import test from "node:test";
import assert from "node:assert/strict";
import { mountMaterialEditor } from "./materials-editor.mjs";

const clone = (value) => JSON.parse(JSON.stringify(value));
const revision = (digit) => digit.repeat(64);

// Minimal DOM: just the APIs the editor is allowed to use. No bubbling, no innerHTML.
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
const field = (root, path) => one(root, (node) => node.dataset.field === path, `field ${path}`);
const maybeField = (root, path) => all(root, (node) => node.dataset.field === path)[0];
const toggle = (root, path) => one(root, (node) => node.dataset.toggle === path, `toggle ${path}`);
const control = (root, kind, path) => one(root, (node) => node.dataset[kind] === path, `${kind} ${path}`);
const roles = (root, role) => all(root, (node) => node.dataset.role === role);
const items = (root) => roles(root, "item");
const tab = (root, group) => one(root, (node) => node.dataset.role === "tab" && node.dataset.group === group, `tab ${group}`);
const add = (root) => one(root, (node) => node.dataset.role === "add", "add");
const deleteButton = (root) => one(root, (node) => node.dataset.action === "delete", "delete");
const options = (select) => select.children.map((option) => option.value);
const retained = (select) => select.children.filter((option) => "retained" in option.dataset);

const type = (node, value) => { node.value = value; node.dispatch("input"); };
const choose = (node, value) => { node.value = value; node.dispatch("change"); };
const check = (node, value) => { node.checked = value; node.dispatch("change"); };
const click = (node) => node.dispatch("click");

function storage() {
  const values = new Map();
  return {
    values,
    getItem(key) { return values.get(key) ?? null; },
    setItem(key, value) { values.set(key, value); },
    removeItem(key) { values.delete(key); },
  };
}

function fullMaterial() {
  return {
    format: 2,
    terms: [
      {
        term_id: "t-1", source: "先生", category: "title", allowed: ["先生", "老师"], note: "敬称",
        scope: { kind: "range", from: { chapter: "c1", paragraph: "p1" }, to: { chapter: "c2" } },
        available_from: { chapter: "c1", paragraph: "p2" },
        evidence: [{ paragraph: "p1", quote: "先生", explanation: "称呼",
          stance: "character_belief", status: "confirmed" }],
        status: "confirmed",
      },
      { term_id: "t-2", source: "王都", category: "place", allowed: [], scope: { kind: "project" },
        evidence: [], status: "tentative" },
    ],
    persons: [
      {
        person_id: "p-1", available_from: { chapter: "c1" }, name: "艾琳", reading: "えりん",
        aliases: ["艾"], terms: ["t-1", "missing-term"],
        address_rules: [
          { target: "p-2", speaker: { kind: "person", person_id: "p-1" },
            scope: { kind: "range", from: { chapter: "c1" }, to: { chapter: "c3", paragraph: "p9" } },
            term: "t-1", note: "私下",
            evidence: [{ paragraph: "p3", quote: "先生", explanation: "对话",
              stance: "narration", status: "tentative" }] },
          { target: "p-1", speaker: { kind: "narration" }, scope: { kind: "project" },
            term: "t-2", evidence: [] },
          { target: "ghost", speaker: { kind: "unknown" }, scope: { kind: "project" },
            term: "t-1", evidence: [] },
        ],
        voice: { description: "冷静", examples: ["嗯。"] },
        evidence: [{ paragraph: "p1", quote: "艾琳", explanation: "登场",
          stance: "project_fact", status: "confirmed" }],
        status: "confirmed",
      },
      { person_id: "p-2", name: "布兰", aliases: [], terms: [], address_rules: [],
        voice: { description: "" }, evidence: [], status: "tentative" },
    ],
  };
}

function view(material, rev = revision("1")) {
  return {
    workspace_id: "a".repeat(64), project_hash: revision("f"),
    project: { id: "book", chapters: [] },
    draft: { project: "book", revision: rev, material },
  };
}

function setup(material = fullMaterial(), { viewer = false, answer = false } = {}) {
  const doc = new FakeDocument();
  const host = doc.createElement("div");
  doc.body.append(host);
  const store = storage();
  const confirms = [];
  const ctx = { doc, host, store, confirms, answer };
  ctx.editor = mountMaterialEditor(host, {
    storage: store, viewer,
    confirm: (message) => { confirms.push(message); return ctx.answer; },
  });
  ctx.editor.update(view(clone(material)));
  ctx.mirror = () => {
    ctx.editor.flushMirror();
    assert.equal(store.values.size, 1, "a dirty editor keeps one session mirror");
    return JSON.parse([...store.values.values()][0]).material;
  };
  return ctx;
}

test("mount builds a stable host and selects entries with equal names, duplicate or empty IDs independently", () => {
  const material = { format: 2, persons: [], terms: [
    ["a", "同名"], ["b", "同名"], ["dup", "甲"], ["dup", "乙"], ["", "空一"], ["", "空二"],
  ].map(([term_id, source]) => ({ term_id, source, category: "common", allowed: [],
    scope: { kind: "project" }, evidence: [], status: "tentative" })) };
  const { host, editor, mirror } = setup(material);
  const root = host.children[0];
  assert.equal(host.children.length, 1);
  const listed = items(host);
  assert.equal(listed.length, 6);
  assert.match(listed[0].textContent, /a/);
  assert.match(listed[1].textContent, /b/);
  assert.match(listed[2].textContent, /重复/);
  assert.match(listed[3].textContent, /重复/);
  assert.match(listed[4].textContent, /空 ID/);

  click(items(host)[3]);
  assert.equal(field(host, "term_id").value, "dup");
  assert.equal(field(host, "source").value, "乙");
  assert.equal(items(host)[3].getAttribute("aria-selected"), "true");
  assert.equal(items(host)[2].getAttribute("aria-selected"), "false");
  type(field(host, "source"), "乙改");
  click(items(host)[5]);
  assert.equal(field(host, "source").value, "空二");
  type(field(host, "term_id"), "fresh");
  click(items(host)[1]);
  assert.equal(field(host, "source").value, "同名");
  assert.equal(field(host, "term_id").value, "b");

  assert.equal(host.children[0], root, "the editor root is never replaced");
  assert.equal(editor.hasDirty(), true);
  const saved = mirror();
  assert.deepEqual(saved.terms.map((term) => [term.term_id, term.source]), [
    ["a", "同名"], ["b", "同名"], ["dup", "甲"], ["dup", "乙改"], ["", "空一"], ["fresh", "空二"],
  ]);
});

test("every schema field renders and round-trips through the registered handlers", () => {
  const { host, mirror } = setup();
  const expected = fullMaterial();

  assert.equal(field(host, "term_id").value, "t-1");
  assert.equal(field(host, "source").value, "先生");
  assert.equal(field(host, "category").value, "title");
  assert.equal(field(host, "allowed.0").value, "先生");
  assert.equal(field(host, "allowed.1").value, "老师");
  assert.equal(toggle(host, "note").checked, true);
  assert.equal(field(host, "note").value, "敬称");
  assert.equal(field(host, "scope.kind").value, "range");
  assert.equal(field(host, "scope.from.chapter").value, "c1");
  assert.equal(field(host, "scope.from.paragraph").value, "p1");
  assert.equal(field(host, "scope.to.chapter").value, "c2");
  assert.equal(toggle(host, "scope.to.paragraph").checked, false);
  assert.equal(maybeField(host, "scope.to.paragraph"), undefined);
  assert.equal(toggle(host, "available_from").checked, true);
  assert.equal(field(host, "available_from.chapter").value, "c1");
  assert.equal(field(host, "available_from.paragraph").value, "p2");
  assert.equal(field(host, "evidence.0.paragraph").value, "p1");
  assert.equal(field(host, "evidence.0.quote").value, "先生");
  assert.equal(field(host, "evidence.0.explanation").value, "称呼");
  assert.equal(field(host, "evidence.0.stance").value, "character_belief");
  assert.equal(field(host, "evidence.0.status").value, "confirmed");
  assert.equal(field(host, "status").value, "confirmed");

  type(field(host, "term_id"), "t-1x"); expected.terms[0].term_id = "t-1x";
  type(field(host, "source"), "先生们"); expected.terms[0].source = "先生们";
  choose(field(host, "category"), "culture"); expected.terms[0].category = "culture";
  type(field(host, "allowed.1"), "师"); expected.terms[0].allowed[1] = "师";
  click(control(host, "add", "allowed")); expected.terms[0].allowed.push("");
  type(field(host, "allowed.2"), "导师"); expected.terms[0].allowed[2] = "导师";
  type(field(host, "note"), "敬称（改）"); expected.terms[0].note = "敬称（改）";
  type(field(host, "scope.from.paragraph"), "p1b"); expected.terms[0].scope.from.paragraph = "p1b";
  type(field(host, "scope.to.chapter"), "c2b"); expected.terms[0].scope.to.chapter = "c2b";
  check(toggle(host, "scope.to.paragraph"), true); expected.terms[0].scope.to.paragraph = "";
  type(field(host, "scope.to.paragraph"), "p7"); expected.terms[0].scope.to.paragraph = "p7";
  type(field(host, "available_from.chapter"), "c0"); expected.terms[0].available_from.chapter = "c0";
  type(field(host, "evidence.0.quote"), "先生！"); expected.terms[0].evidence[0].quote = "先生！";
  type(field(host, "evidence.0.explanation"), "称呼语"); expected.terms[0].evidence[0].explanation = "称呼语";
  type(field(host, "evidence.0.paragraph"), "p4"); expected.terms[0].evidence[0].paragraph = "p4";
  choose(field(host, "evidence.0.stance"), "project_fact"); expected.terms[0].evidence[0].stance = "project_fact";
  choose(field(host, "evidence.0.status"), "tentative"); expected.terms[0].evidence[0].status = "tentative";
  choose(field(host, "status"), "tentative"); expected.terms[0].status = "tentative";

  click(items(host)[1]);
  assert.equal(toggle(host, "note").checked, false);
  assert.equal(toggle(host, "available_from").checked, false);
  assert.equal(field(host, "scope.kind").value, "project");
  assert.equal(maybeField(host, "scope.from.chapter"), undefined);

  click(tab(host, "persons"));
  assert.equal(field(host, "person_id").value, "p-1");
  assert.equal(field(host, "name").value, "艾琳");
  assert.equal(field(host, "reading").value, "えりん");
  assert.equal(field(host, "available_from.chapter").value, "c1");
  assert.equal(toggle(host, "available_from.paragraph").checked, false);
  assert.equal(field(host, "aliases.0").value, "艾");
  assert.equal(field(host, "terms.0").value, "t-1", "renaming a term never rewrites references");
  assert.deepEqual(retained(field(host, "terms.0")).map((option) => option.value), ["t-1"]);
  assert.equal(field(host, "terms.1").value, "missing-term");
  assert.equal(field(host, "address_rules.0.target").value, "p-2");
  assert.equal(field(host, "address_rules.0.speaker.kind").value, "person");
  assert.equal(field(host, "address_rules.0.speaker.person_id").value, "p-1");
  assert.equal(field(host, "address_rules.0.scope.from.chapter").value, "c1");
  assert.equal(field(host, "address_rules.0.scope.to.paragraph").value, "p9");
  assert.equal(field(host, "address_rules.0.term").value, "t-1");
  assert.equal(field(host, "address_rules.0.note").value, "私下");
  assert.equal(field(host, "address_rules.0.evidence.0.stance").value, "narration");
  assert.equal(field(host, "address_rules.1.speaker.kind").value, "narration");
  assert.equal(maybeField(host, "address_rules.1.speaker.person_id"), undefined);
  assert.equal(field(host, "address_rules.2.speaker.kind").value, "unknown");
  assert.equal(field(host, "address_rules.2.target").value, "ghost");
  assert.equal(field(host, "voice.description").value, "冷静");
  assert.equal(field(host, "voice.examples.0").value, "嗯。");
  assert.equal(field(host, "evidence.0.quote").value, "艾琳");
  assert.equal(field(host, "status").value, "confirmed");

  type(field(host, "reading"), "エリン"); expected.persons[0].reading = "エリン";
  type(field(host, "aliases.0"), "小艾"); expected.persons[0].aliases[0] = "小艾";
  choose(field(host, "terms.0"), "t-2"); expected.persons[0].terms[0] = "t-2";
  choose(field(host, "address_rules.0.speaker.person_id"), "p-2");
  expected.persons[0].address_rules[0].speaker.person_id = "p-2";
  choose(field(host, "address_rules.1.speaker.kind"), "person");
  expected.persons[0].address_rules[1].speaker = { kind: "person", person_id: "" };
  choose(field(host, "address_rules.1.speaker.person_id"), "p-1");
  expected.persons[0].address_rules[1].speaker.person_id = "p-1";
  choose(field(host, "address_rules.1.target"), "p-2"); expected.persons[0].address_rules[1].target = "p-2";
  choose(field(host, "address_rules.1.term"), "t-2");
  type(field(host, "address_rules.0.evidence.0.explanation"), "改");
  expected.persons[0].address_rules[0].evidence[0].explanation = "改";
  type(field(host, "address_rules.0.scope.to.chapter"), "c4");
  expected.persons[0].address_rules[0].scope.to.chapter = "c4";
  type(field(host, "voice.description"), "冷静克制"); expected.persons[0].voice.description = "冷静克制";
  type(field(host, "voice.examples.0"), "嗯……"); expected.persons[0].voice.examples[0] = "嗯……";
  click(control(host, "add", "address_rules.2.evidence"));
  expected.persons[0].address_rules[2].evidence.push({ paragraph: "", quote: "", explanation: "",
    stance: "project_fact", status: "tentative" });
  type(field(host, "address_rules.2.evidence.0.quote"), "引");
  expected.persons[0].address_rules[2].evidence[0].quote = "引";
  choose(field(host, "evidence.0.stance"), "character_belief");
  expected.persons[0].evidence[0].stance = "character_belief";

  const saved = mirror();
  assert.deepEqual(saved.terms[0], expected.terms[0]);
  assert.deepEqual(saved.terms[1], expected.terms[1]);
  assert.deepEqual(saved.persons, expected.persons);
  assert.deepEqual(saved, { ...expected, terms: saved.terms });
  assert.deepEqual(Object.keys(saved.terms[0]), Object.keys(expected.terms[0]));
});

test("optional values toggle off by omission and restore their prior value", () => {
  const { host, mirror } = setup();
  check(toggle(host, "note"), false);
  assert.equal(maybeField(host, "note"), undefined);
  check(toggle(host, "available_from"), false);
  assert.equal(field(host, "term_id").value, "t-1", "removing an entry field keeps the selection");
  choose(field(host, "scope.kind"), "project");
  assert.equal(maybeField(host, "scope.from.chapter"), undefined);
  let saved = mirror();
  assert.equal(Object.hasOwn(saved.terms[0], "note"), false);
  assert.equal(Object.hasOwn(saved.terms[0], "available_from"), false);
  assert.deepEqual(saved.terms[0].scope, { kind: "project" });
  assert.equal(JSON.stringify(saved).includes("null"), false, "absent options are never null");

  check(toggle(host, "note"), true);
  assert.equal(field(host, "note").value, "敬称");
  check(toggle(host, "available_from"), true);
  choose(field(host, "scope.kind"), "range");
  assert.equal(field(host, "scope.from.paragraph").value, "p1");
  check(toggle(host, "available_from.paragraph"), false);
  saved = mirror();
  assert.deepEqual(saved.terms[0].available_from, { chapter: "c1" });
  assert.deepEqual(saved.terms[0].scope, fullMaterial().terms[0].scope);
  assert.equal(saved.terms[0].note, "敬称");

  click(tab(host, "persons"));
  check(toggle(host, "reading"), false);
  check(toggle(host, "voice.examples"), false);
  check(toggle(host, "address_rules.0.note"), false);
  choose(field(host, "address_rules.0.speaker.kind"), "unknown");
  saved = mirror();
  assert.equal(Object.hasOwn(saved.persons[0], "reading"), false);
  assert.deepEqual(saved.persons[0].voice, { description: "冷静" });
  assert.equal(Object.hasOwn(saved.persons[0].address_rules[0], "note"), false);
  assert.deepEqual(saved.persons[0].address_rules[0].speaker, { kind: "unknown" });
  choose(field(host, "address_rules.0.speaker.kind"), "person");
  check(toggle(host, "voice.examples"), true);
  saved = mirror();
  assert.deepEqual(saved.persons[0].address_rules[0].speaker, { kind: "person", person_id: "p-1" });
  assert.deepEqual(saved.persons[0].voice.examples, ["嗯。"]);
  assert.deepEqual(saved.persons[1], fullMaterial().persons[1], "required empty values are preserved");
});

test("viewer mode renders read-only controls and handlers never change input", () => {
  let readOnly = true;
  const { host, editor, store, confirms } = setup(fullMaterial(), { viewer: () => readOnly });
  const source = field(host, "source");
  assert.equal(source.readOnly, true);
  assert.equal(field(host, "category").disabled, true);
  assert.equal(toggle(host, "note").disabled, true);
  assert.equal(add(host).hidden, true);
  assert.equal(deleteButton(host).hidden, true);
  assert.equal(control(host, "add", "allowed").hidden, true);
  assert.equal(control(host, "remove", "allowed.0").hidden, true);

  type(source, "改");
  assert.equal(source.value, "先生");
  choose(field(host, "category"), "place");
  assert.equal(field(host, "category").value, "title");
  check(toggle(host, "note"), false);
  assert.equal(toggle(host, "note").checked, true);
  click(add(host));
  click(deleteButton(host));
  click(control(host, "remove", "allowed.0"));
  assert.equal(items(host).length, 2);
  assert.deepEqual(confirms, []);
  assert.equal(editor.hasDirty(), false);
  editor.flushMirror();
  assert.equal(store.values.size, 0);

  readOnly = false;
  editor.update(view(fullMaterial()));
  assert.equal(field(host, "source").readOnly, false);
  assert.equal(add(host).hidden, false);
});

test("deletion confirms with reference locations, cancels cleanly and never cascades", () => {
  const ctx = setup();
  const { host, editor, confirms } = ctx;
  click(deleteButton(host));
  assert.equal(confirms.length, 1);
  assert.match(confirms[0], /t-1/);
  assert.match(confirms[0], /persons\[0\]\.terms\[0\]/);
  assert.match(confirms[0], /persons\[0\]\.address_rules\[0\]\.term/);
  assert.match(confirms[0], /persons\[0\]\.address_rules\[2\]\.term/);
  assert.equal(editor.hasDirty(), false);
  assert.equal(items(host).length, 2);
  assert.equal(field(host, "term_id").value, "t-1");

  ctx.answer = true;
  click(deleteButton(host));
  assert.equal(items(host).length, 1);
  let saved = ctx.mirror();
  assert.deepEqual(saved.terms, [fullMaterial().terms[1]]);
  assert.deepEqual(saved.persons, fullMaterial().persons, "references are left for the user to fix");

  click(tab(host, "persons"));
  click(items(host)[1]);
  click(deleteButton(host));
  assert.match(confirms[2], /persons\[0\]\.address_rules\[0\]\.target/);
  saved = ctx.mirror();
  assert.deepEqual(saved.persons, [fullMaterial().persons[0]]);
});

test("composition defers commits and remote updates without replacing the active field", () => {
  const { host, doc, editor, store, mirror } = setup();
  const source = field(host, "source");
  source.focus();
  source.dispatch("compositionstart");
  source.value = "先生ｓ";
  source.dispatch("input", { isComposing: true });
  assert.equal(editor.hasDirty(), true, "an active composition counts as unsaved input");
  assert.equal(store.values.size, 0, "composition input stays local");

  const remote = fullMaterial();
  remote.terms[0].source = "远端";
  assert.equal(editor.update(view(remote, revision("2"))), false);
  assert.equal(field(host, "source"), source);
  assert.equal(source.value, "先生ｓ");
  assert.equal(doc.activeElement, source);

  source.value = "先生さん";
  source.dispatch("compositionend");
  assert.equal(field(host, "source"), source, "the deferred update keeps the edited field");
  assert.equal(source.value, "先生さん");
  assert.equal(mirror().terms[0].source, "先生さん");

  editor.update(view(remote, revision("2")));
  assert.equal(field(host, "source"), source);
  assert.equal(doc.activeElement, source);
});

test("an unchanged update keeps form nodes and focus", () => {
  const { host, doc, editor } = setup();
  const quote = field(host, "evidence.0.quote");
  quote.focus();
  editor.update(view(fullMaterial()));
  assert.equal(field(host, "evidence.0.quote"), quote);
  assert.equal(doc.activeElement, quote);
});

test("switching tabs and entries keeps input, and keyboard navigation moves focus", () => {
  const { host, doc, mirror } = setup();
  type(field(host, "source"), "改");
  click(tab(host, "persons"));
  type(field(host, "name"), "新名");
  click(tab(host, "terms"));
  assert.equal(field(host, "source").value, "改");
  click(tab(host, "persons"));
  assert.equal(field(host, "name").value, "新名");
  click(tab(host, "terms"));

  const first = items(host)[0];
  first.focus();
  const event = first.dispatch("keydown", { key: "ArrowDown" });
  assert.equal(event.defaultPrevented, true);
  assert.equal(doc.activeElement, items(host)[1]);
  assert.equal(field(host, "term_id").value, "t-2");
  items(host)[1].dispatch("keydown", { key: "ArrowUp" });
  assert.equal(doc.activeElement, items(host)[0]);
  assert.equal(field(host, "term_id").value, "t-1");

  const termsTab = tab(host, "terms");
  termsTab.focus();
  termsTab.dispatch("keydown", { key: "ArrowRight" });
  assert.equal(doc.activeElement, tab(host, "persons"));
  assert.equal(field(host, "person_id").value, "p-1");
  assert.equal(tab(host, "persons").getAttribute("aria-selected"), "true");

  const name = field(host, "name");
  name.dispatch("compositionstart");
  name.value = "组合";
  click(tab(host, "terms"));
  assert.equal(mirror().persons[0].name, "组合");
  assert.equal(mirror().terms[0].source, "改");
});

test("long lists are paginated instead of rendering every nested form", () => {
  const terms = Array.from({ length: 5000 }, (_, i) => ({
    term_id: `t-${i}`, source: `词${i}`, category: "common", allowed: [], scope: { kind: "project" },
    evidence: Array.from({ length: 30 }, (_, j) => ({ paragraph: `p${j}`, quote: `q${j}`,
      explanation: "", stance: "project_fact", status: "tentative" })), status: "tentative",
  }));
  const { host, doc } = setup({ format: 2, terms, persons: [] });
  assert.equal(items(host).length, 50);
  assert.ok(all(host, () => true).length < 1500);
  assert.equal(all(host, (node) => /^evidence\.\d+\.quote$/.test(node.dataset.field ?? "")).length, 10);
  click(control(host, "next", "evidence"));
  assert.equal(field(host, "evidence.10.quote").value, "q10");
  assert.equal(maybeField(host, "evidence.0.quote"), undefined);

  click(one(host, (node) => node.dataset.role === "next-page", "next page"));
  assert.match(items(host)[0].textContent, /t-50/);
  click(one(host, (node) => node.dataset.role === "prev-page", "previous page"));
  const last = items(host)[49];
  last.focus();
  last.dispatch("keydown", { key: "ArrowDown" });
  assert.match(doc.activeElement.textContent, /t-50/);
  assert.equal(field(host, "term_id").value, "t-50");
});

test("new entries start tentative and nothing is sorted, deduplicated or confirmed", () => {
  const { host, doc, mirror } = setup();
  click(add(host));
  assert.equal(items(host).length, 3);
  assert.equal(doc.activeElement, field(host, "term_id"));
  click(control(host, "add", "evidence"));
  type(field(host, "source"), "王都");
  click(tab(host, "persons"));
  click(add(host));
  type(field(host, "person_id"), "p-1");
  const saved = mirror();
  assert.deepEqual(saved.terms.slice(0, 2), fullMaterial().terms);
  assert.deepEqual(saved.terms[2], { term_id: "", source: "王都", category: "common", allowed: [],
    scope: { kind: "project" }, evidence: [{ paragraph: "", quote: "", explanation: "",
      stance: "project_fact", status: "tentative" }], status: "tentative" });
  assert.deepEqual(saved.persons.map((person) => person.person_id), ["p-1", "p-2", "p-1"]);
  assert.equal(saved.persons[2].status, "tentative");
});

test("invalid enum and reference values keep explicit retained options", () => {
  const material = fullMaterial();
  material.terms[0].category = "weird";
  material.terms[0].evidence[0].stance = "rumor";
  const { host, mirror } = setup(material);
  const category = field(host, "category");
  assert.equal(category.value, "weird");
  assert.deepEqual(retained(category).map((option) => option.value), ["weird"]);
  assert.ok(options(category).includes("common"));
  assert.equal(field(host, "evidence.0.stance").value, "rumor");
  type(field(host, "source"), "先生2");
  assert.equal(mirror().terms[0].category, "weird");
  assert.equal(mirror().terms[0].evidence[0].stance, "rumor");

  click(tab(host, "persons"));
  const missing = field(host, "terms.1");
  assert.deepEqual(retained(missing).map((option) => option.value), ["missing-term"]);
  assert.deepEqual(retained(field(host, "terms.0")), []);
  const ghost = field(host, "address_rules.2.target");
  assert.deepEqual(retained(ghost).map((option) => option.value), ["ghost"]);
  assert.ok(options(ghost).includes("p-1") && options(ghost).includes("p-2"));
});

test("dispose detaches the editor and later events are ignored", () => {
  const { host, editor, store } = setup();
  const source = field(host, "source");
  editor.dispose();
  assert.equal(host.children.length, 0);
  type(source, "改");
  assert.equal(editor.hasDirty(), false);
  assert.equal(editor.update(view(fullMaterial())), false);
  assert.equal(store.values.size, 0);
});
