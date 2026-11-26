import test from "node:test";
import assert from "node:assert/strict";
import {
  formatMaterialAdvice,
  formatMaterialsDiffReport,
  parseMaterialsAssociations,
  renderBlockQaIssues,
  renderTermProposalsPanel,
} from "./materials-collab.mjs";

test("formatMaterialAdvice appends actionable next-step guidance on conflicts and missing draft", () => {
  assert.match(formatMaterialAdvice("修订冲突"), /重新读取服务器资料/);
  assert.match(formatMaterialAdvice("尚无本地资料草稿"), /先新增一条术语或人物/);
  assert.equal(formatMaterialAdvice("普通提示"), "普通提示");
});

test("formatMaterialsDiffReport summarizes term and person changes with Chinese field labels", () => {
  const empty = formatMaterialsDiffReport({
    draft_revision: "rev-1",
    approved_version: 2,
    approved_versions: [1, 2],
    terms: [],
    persons: [],
  });
  assert.match(empty, /草稿与已批准资料一致/);

  const changed = formatMaterialsDiffReport({
    draft_revision: "rev-2",
    approved_version: 1,
    approved_versions: [1],
    terms: [{ id: "t-1", kind: "changed", fields: ["allowed", "scope"] }],
    persons: [{ id: "p-1", kind: "added", fields: [] }],
  });
  assert.match(changed, /摘要：改动 1 · 新增 1/);
  assert.match(changed, /t-1：改动（允许译法、范围）/);
  assert.match(changed, /p-1：新增/);
});

test("parseMaterialsAssociations normalizes narration, unknown, and person speaker tags", () => {
  const map = new Map([
    ["p1", { persons: "p_a, p_b", targets: "p_b", speaker: "p_a" }],
    ["p2", { persons: "", targets: "", speaker: "narration" }],
    ["p3", { persons: " ", targets: "", speaker: "" }],
  ]);
  const out = parseMaterialsAssociations(map);
  assert.deepEqual(out, [
    {
      paragraph: "p1",
      persons: ["p_a", "p_b"],
      targets: ["p_b"],
      speaker: { kind: "person", person_id: "p_a" },
    },
    {
      paragraph: "p2",
      persons: [],
      targets: [],
      speaker: { kind: "narration" },
    },
  ]);
});

test("renderBlockQaIssues filters by code and wires paragraph navigation safely", () => {
  const navigated = [];
  const doc = {
    createElement(tag) {
      return {
        tagName: tag.toUpperCase(),
        className: "",
        type: "",
        textContent: "",
        children: [],
        listeners: {},
        append(...nodes) {
          this.children.push(...nodes);
        },
        replaceChildren(...nodes) {
          this.children = [...nodes];
        },
        addEventListener(ev, fn) {
          this.listeners[ev] = fn;
        },
      };
    },
  };
  const host = doc.createElement("div");
  renderBlockQaIssues({
    doc,
    host,
    issues: [
      { code: "source_script_leak", paragraph: "c0001-p0001", message: "残留假名" },
      { code: "number_mismatch", paragraph: "c0001-p0002", message: "数字不符" },
    ],
    filter: "source_script_leak",
    formatLocation: () => "第1章 · 第1段",
    onNavigate: (p) => navigated.push(p),
  });
  assert.equal(host.children.length, 1);
  assert.match(host.children[0].children[1].textContent, /源语字符残留（第1章 · 第1段）：残留假名/);
  host.children[0].children[2].listeners.click();
  assert.deepEqual(navigated, ["c0001-p0001"]);
});

test("renderTermProposalsPanel supports single/batch quick-add into draft and respects viewer read-only", async () => {
  const makeDoc = () => ({
    createElement(tag) {
      return {
        tagName: tag.toUpperCase(),
        className: "",
        type: "",
        value: "",
        placeholder: "",
        disabled: false,
        hidden: false,
        selected: false,
        title: "",
        textContent: "",
        dataset: {},
        attributes: {},
        children: [],
        listeners: {},
        setAttribute(k, v) {
          this.attributes[k] = v;
        },
        append(...nodes) {
          for (const n of nodes) {
            if (n) this.children.push(n);
          }
          if (this.tagName === "SELECT" && !this.value) {
            const sel = this.children.find((c) => c.selected) || this.children[0];
            if (sel) this.value = sel.value;
          }
        },
        replaceChildren(...nodes) {
          this.children = [];
          this.append(...nodes);
        },
        addEventListener(ev, fn) {
          this.listeners[ev] = fn;
        },
      };
    },
  });

  const findNodes = (node, pred, out = []) => {
    if (!node) return out;
    if (pred(node)) out.push(node);
    for (const c of node.children || []) findNodes(c, pred, out);
    return out;
  };

  const doc = makeDoc();
  const host = doc.createElement("div");
  const statusEl = doc.createElement("p");
  const calls = [];
  const jumped = [];
  let mergedReloads = 0;

  const sampleReport = {
    project: "novel-1",
    block: "c0001-b0001",
    path: "translations/novel-1/blocks/c0001-b0001/proposals/term-candidates.json",
    written: true,
    candidates: [
      { source: "アルカディア", count: 3, reason: "片假名专名候选", paragraphs: ["c0001-p0001", "c0001-p0004"] },
      { source: "星辰の塔", count: 2, reason: "引号短语候选", paragraphs: ["c0001-p0002"] },
    ],
  };

  // 1. Viewer mode: no merge inputs or write buttons, paragraph jump works
  renderTermProposalsPanel({
    doc,
    host,
    statusEl,
    report: { ...sampleReport, written: false },
    project: "novel-1",
    viewer: true,
    formatLocation: (id) => `段:${id}`,
    onNavigate: (p) => jumped.push(p),
  });
  assert.equal(findNodes(host, (n) => n.tagName === "INPUT").length, 0);
  const viewerJumpBtns = findNodes(host, (n) => n.tagName === "BUTTON");
  assert.equal(viewerJumpBtns.length, 3);
  viewerJumpBtns[0].listeners.click();
  assert.deepEqual(jumped, ["c0001-p0001"]);

  // 2. Write mode: fill translations and batch merge into quick-add
  renderTermProposalsPanel({
    doc,
    host,
    statusEl,
    report: sampleReport,
    project: "novel-1",
    viewer: false,
    formatLocation: (id) => `段:${id}`,
    apiCall: async (url, body) => {
      calls.push([url, body]);
      return { id: `term-quick-00${calls.length}`, updated_existing: false };
    },
    onMerged: async () => {
      mergedReloads += 1;
    },
  });

  const inputs = findNodes(host, (n) => n.tagName === "INPUT" && n.placeholder.includes("认可译法"));
  assert.equal(inputs.length, 2);
  inputs[0].value = "阿卡迪亚";
  inputs[1].value = "星辰之塔";

  const batchBtn = findNodes(host, (n) => n.tagName === "BUTTON" && n.textContent.includes("批量收入"))[0];
  await batchBtn.listeners.click();

  assert.equal(calls.length, 2);
  assert.equal(calls[0][0], "/text/novel-1/materials/quick-add");
  assert.equal(calls[0][1].source, "アルカディア");
  assert.equal(calls[0][1].target, "阿卡迪亚");
  assert.equal(calls[0][1].category, "person_name");
  assert.equal(calls[1][1].source, "星辰の塔");
  assert.equal(calls[1][1].target, "星辰之塔");
  assert.equal(mergedReloads, 1);
  assert.match(statusEl.textContent, /已批量收入 2 条候选术语/);
});
