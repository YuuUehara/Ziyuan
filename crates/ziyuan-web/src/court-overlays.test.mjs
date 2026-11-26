import test from "node:test";
import assert from "node:assert/strict";
import { mountCourtOverlays } from "./court-overlays.mjs";
import * as paletteMod from "./command-palette.mjs";
import * as neoMod from "./neo-panel.mjs";
import * as quickMod from "./quick-glossary.mjs";

function makeFakeEnv() {
  const byId = new Map();
  const docListeners = {};
  const doc = {
    activeElement: null,
    documentElement: { dataset: { theme: "tianqing" } },
    body: null,
    createElement(tag) {
      return makeEl(tag);
    },
    createTextNode(text) {
      return { tagName: "#TEXT", textContent: String(text), children: [] };
    },
    getElementById(id) {
      return byId.get(id) || null;
    },
    addEventListener(ev, fn) {
      docListeners[ev] = fn;
    },
    listeners: docListeners,
  };

  function makeEl(tag, id = "") {
    const classes = new Set();
    const node = {
      ownerDocument: doc,
      tagName: String(tag).toUpperCase(),
      id,
      className: "",
      value: "",
      hidden: false,
      checked: false,
      textContent: "",
      dataset: {},
      style: {},
      attributes: {},
      listeners: {},
      children: [],
      get firstChild() {
        return this.children[0] || null;
      },
      classList: {
        toggle(cls, force) {
          const on = force === undefined ? !classes.has(cls) : Boolean(force);
          if (on) classes.add(cls);
          else classes.delete(cls);
          node.className = [...classes].join(" ");
        },
        contains(cls) {
          return classes.has(cls);
        },
      },
      appendChild(c) {
        if (c) this.children.push(c);
        return c;
      },
      append(...nodes) {
        for (const n of nodes) this.appendChild(n);
      },
      removeChild(c) {
        const idx = this.children.indexOf(c);
        if (idx >= 0) this.children.splice(idx, 1);
      },
      setAttribute(k, v) {
        this.attributes[k] = String(v);
      },
      addEventListener(ev, fn) {
        this.listeners[ev] = fn;
      },
      contains(target) {
        if (target === this) return true;
        return this.children.some((c) => c.contains?.(target));
      },
      focus() {
        doc.activeElement = this;
      },
      showModal() {
        this.open = true;
      },
      close() {
        this.open = false;
      },
    };
    if (id) byId.set(id, node);
    return node;
  }

  doc.body = makeEl("body");

  for (const id of [
    "cmd-palette",
    "cmd-palette-input",
    "cmd-palette-list",
    "cmd-palette-btn",
    "cmd-palette-close",
    "doctor-modal",
    "doctor-body",
    "doctor-open-btn",
    "doctor-close-btn",
    "doctor-refresh-btn",
    "neo-mode-btn",
    "neo-panel-root",
    "quick-glossary-dialog",
    "quick-glossary-title",
    "quick-term-fields",
    "quick-person-fields",
    "quick-glossary-status",
    "quick-term-source",
    "quick-term-target",
    "quick-term-category",
    "quick-term-note",
    "quick-person-name",
    "quick-person-alias",
    "quick-person-voice",
    "quick-term-btn",
    "quick-person-btn",
    "quick-glossary-submit",
    "quick-glossary-cancel",
    "human-toggle-note",
  ]) {
    const el = makeEl("div", id);
    if (id === "cmd-palette" || id === "doctor-modal") el.hidden = true;
  }

  const store = new Map();
  const storage = {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => store.set(k, String(v)),
  };
  const win = {
    location: { hash: "#fengtian" },
    confirm: () => true,
    getSelection: () => ({ toString: () => "" }),
  };

  return { doc, win, storage, byId };
}

test("mountCourtOverlays wires Neo layout prefs, Ctrl+K command palette, and Alt+N toggle", async () => {
  const { doc, win, storage, byId } = makeFakeEnv();
  let currentHall = "fengtian";
  const themes = [];

  const ctrl = mountCourtOverlays({
    doc,
    win,
    storage,
    getById: (id) => byId.get(id),
    api: async () => ({}),
    act: async (p) => p,
    toast: () => {},
    showHall: () => {
      currentHall = win.location.hash.slice(1) || "fengtian";
    },
    setTheme: (t) => {
      doc.documentElement.dataset.theme = t;
      themes.push(t);
    },
    statusLabel: { pending: "待办" },
    getState: () => ({ viewer: false, mode: { code: "cabinet", label: "内阁制" }, tasks: [] }),
    getHall: () => currentHall,
    getPreviousClassicHall: () => "wenhua",
    getTextState: () => ({ textProjectId: "proj-1", textProjects: [] }),
    loadNeoModule: async () => neoMod,
    loadPaletteModule: async () => paletteMod,
    loadQuickGlossaryModule: async () => quickMod,
  });

  // Default layout class: neo-wusilan enabled, neo-compact disabled
  assert.equal(doc.body.classList.contains("neo-wusilan"), true);
  assert.equal(doc.body.classList.contains("neo-compact"), false);

  // Alt+N toggles into #neo and back to previousClassicHall
  doc.listeners.keydown({ key: "n", altKey: true, ctrlKey: false, metaKey: false, preventDefault() {} });
  assert.equal(win.location.hash, "#neo");
  assert.equal(currentHall, "neo");

  await ctrl.ensureNeoPanel();
  assert.ok(byId.get("neo-panel-root").children.length > 0);

  doc.listeners.keydown({ key: "n", altKey: true, ctrlKey: false, metaKey: false, preventDefault() {} });
  assert.equal(win.location.hash, "#wenhua");

  // Ctrl+K opens command palette and populates filtered items safely
  doc.listeners.keydown({ key: "k", ctrlKey: true, altKey: false, metaKey: false, preventDefault() {} });
  assert.equal(byId.get("cmd-palette").hidden, false);
  await new Promise((r) => setTimeout(r, 10));
  assert.ok(byId.get("cmd-palette-list").children.length > 0);

  // Escape closes open command palette
  doc.listeners.keydown({ key: "Escape" });
  assert.equal(byId.get("cmd-palette").hidden, true);
});

test("mountCourtOverlays quick glossary dialog validates and submits to local draft", async () => {
  const { doc, win, storage, byId } = makeFakeEnv();
  const apiCalls = [];

  const ctrl = mountCourtOverlays({
    doc,
    win,
    storage,
    getById: (id) => byId.get(id),
    api: async (path, body) => {
      apiCalls.push({ path, body });
      return { kind: "term", key: "魔導書", created: true };
    },
    act: async (p) => p,
    getState: () => ({ viewer: false }),
    getTextState: () => ({ textProjectId: "proj-abc" }),
    loadQuickGlossaryModule: async () => quickMod,
  });

  await ctrl.openQuickGlossaryDialog("term");
  assert.equal(byId.get("quick-glossary-dialog").open, true);

  // Empty submit triggers validation message without calling API
  await byId.get("quick-glossary-submit").listeners.click();
  assert.equal(apiCalls.length, 0);
  assert.match(byId.get("quick-glossary-status").textContent, /原文词条/);

  byId.get("quick-term-source").value = "魔導書";
  byId.get("quick-term-target").value = "魔导书";
  byId.get("quick-term-category").value = "setting";
  await byId.get("quick-glossary-submit").listeners.click();

  assert.equal(apiCalls.length, 1);
  assert.equal(apiCalls[0].path, "/text/proj-abc/materials/quick-add");
  assert.equal(apiCalls[0].body.source, "魔導書");
  assert.equal(byId.get("quick-glossary-dialog").open, false);
  assert.ok(byId.get("human-toggle-note").textContent.length > 0);
});
