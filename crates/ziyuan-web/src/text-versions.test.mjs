import test from "node:test";
import assert from "node:assert/strict";
import {
  versionStateLabel,
  formatVersionOptionLabel,
  formatVersionDiffChange,
  renderVersionDiffView,
  createTextVersionsController,
} from "./text-versions.mjs";

function makeDoc() {
  const byId = new Map();
  function makeEl(tag, id = "") {
    const listeners = new Map();
    const el = {
      tagName: tag.toUpperCase(),
      id,
      value: "",
      disabled: false,
      textContent: "",
      children: [],
      append(...nodes) {
        for (const n of nodes) this.children.push(n);
        if (this.tagName === "SELECT" && !this.value && this.children.length) {
          this.value = this.children[0].value;
        }
      },
      replaceChildren(...nodes) {
        this.children = [];
        this.value = "";
        this.append(...nodes);
      },
      addEventListener(type, fn) {
        const arr = listeners.get(type) || [];
        arr.push(fn);
        listeners.set(type, arr);
      },
      trigger(type) {
        for (const fn of listeners.get(type) || []) fn({});
      },
    };
    if (id) byId.set(id, el);
    return el;
  }
  return {
    byId,
    makeEl,
    createElement: (tag) => makeEl(tag),
    getElementById: (id) => byId.get(id) || null,
  };
}

test("versionStateLabel and formatVersionDiffChange render readable Chinese labels safely", () => {
  assert.equal(versionStateLabel("draft"), "草稿");
  assert.equal(versionStateLabel("finalized"), "定稿");
  assert.equal(versionStateLabel("manual"), "手工");
  const opt = formatVersionOptionLabel({ state: "approved", label: "初审", created_ms: 0 });
  assert.match(opt, /^准奏 · 初审 · /);

  assert.equal(
    formatVersionDiffChange(
      { paragraph: "c0001-p0002", kind: "changed", old: "旧译", new: "新译" },
      () => "第1章 · 第2段",
    ),
    "第1章 · 第2段 · 改写：旧译 → 新译",
  );
  assert.equal(
    formatVersionDiffChange({ paragraph: "c0001-p0003", kind: "markup" }, (id) => id),
    "c0001-p0003 · 标记",
  );
});

test("renderVersionDiffView uses textContent without HTML parsing", () => {
  const doc = makeDoc();
  const host = doc.makeEl("div", "version-diff");
  const status = doc.makeEl("p", "version-status");
  renderVersionDiffView({
    doc,
    host,
    statusEl: status,
    result: {
      against: "draft",
      changes: [{ paragraph: "p1", kind: "added", new: "<img src=x onerror=alert(1)>" }],
    },
  });
  assert.match(status.textContent, /1 处差异（对比草稿/);
  assert.equal(host.children.length, 1);
  assert.equal(host.children[0].textContent, "p1 · 新增：<img src=x onerror=alert(1)>");
});

test("createTextVersionsController reverses list order, enforces viewer read-only, and confirms prune", async () => {
  const doc = makeDoc();
  doc.makeEl("select", "version-list");
  doc.makeEl("p", "version-status");
  doc.makeEl("div", "version-diff");
  doc.makeEl("button", "version-compare-current");
  doc.makeEl("button", "version-compare-draft");
  doc.makeEl("button", "version-save");
  doc.makeEl("button", "version-prune");

  let viewer = true;
  const calls = [];
  const ctrl = createTextVersionsController({
    doc,
    getScope: () => ({ project: "proj-1", block: "c0001-b0001" }),
    isViewer: () => viewer,
    apiCall: async (path, body) => {
      calls.push([path, body]);
      if (path.includes("/versions?t=")) {
        return {
          versions: [
            { id: "v1", state: "draft", created_ms: 1000 },
            { id: "v2", state: "approved", created_ms: 2000 },
          ],
        };
      }
      if (path.endsWith("/versions/prune") && body?.dry_run) {
        return { candidates: ["v1"] };
      }
      if (path.endsWith("/versions/prune") && !body?.dry_run) {
        return { deleted: ["v1"], kept: 1 };
      }
      return {};
    },
    confirmAction: () => true,
  });

  await ctrl.load();
  const list = doc.byId.get("version-list");
  assert.equal(list.children.length, 2);
  assert.equal(list.children[0].value, "v2");
  assert.equal(doc.byId.get("version-compare-current").disabled, false);
  assert.equal(doc.byId.get("version-save").disabled, true);
  assert.equal(doc.byId.get("version-prune").disabled, true);

  viewer = false;
  ctrl.renderButtons();
  assert.equal(doc.byId.get("version-save").disabled, false);
  await ctrl.prune();
  assert.match(doc.byId.get("version-status").textContent, /^共 2 份/);
  assert.ok(calls.some(([p, b]) => p.endsWith("/versions/prune") && b?.dry_run === false));
});
