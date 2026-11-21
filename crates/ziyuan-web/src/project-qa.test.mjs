import test from "node:test";
import assert from "node:assert/strict";
import {
  QA_CODE_LABELS,
  qaCodeLabel,
  buildProjectQaSummaryChips,
  filterProjectQaIssues,
  renderProjectQaDashboard,
  createProjectQaController,
} from "./project-qa.mjs";

function fakeDoc() {
  const makeNode = (tag) => {
    const listeners = new Map();
    const attrs = new Map();
    const node = {
      tag,
      className: "",
      type: "",
      dataset: {},
      textContent: "",
      children: [],
      append(...items) {
        this.children.push(...items);
      },
      replaceChildren(...items) {
        this.children = [...items];
      },
      setAttribute(k, v) {
        attrs.set(k, String(v));
      },
      getAttribute(k) {
        return attrs.get(k);
      },
      addEventListener(evt, fn) {
        listeners.set(evt, fn);
      },
      click() {
        listeners.get("click")?.();
      },
    };
    return node;
  };
  return {
    createElement: makeNode,
    makeHost: () => makeNode("div"),
  };
}

function collectText(node) {
  if (!node) return "";
  return [node.textContent || "", ...(node.children || []).map(collectText)].join(" ").trim();
}

test("qaCodeLabel and buildProjectQaSummaryChips cover founded and warning states", () => {
  assert.equal(qaCodeLabel("source_script_leak"), QA_CODE_LABELS.source_script_leak);
  assert.equal(qaCodeLabel("custom_code"), "custom_code");

  const chipsFounded = buildProjectQaSummaryChips({
    founded: true,
    summary: {
      total_blocks: 10,
      translated_blocks: 6,
      untranslated_blocks: 4,
      finalized_blocks: 2,
      clean_blocks: 5,
      blocks_with_issues: 1,
      total_issues: 3,
    },
  });
  assert.ok(chipsFounded.some(([label]) => label === "已定稿 2"));
  assert.deepEqual(chipsFounded.at(-1), ["问题总数 3", true]);

  const chipsUnfounded = buildProjectQaSummaryChips({
    founded: false,
    summary: { total_blocks: 2, translated_blocks: 0, untranslated_blocks: 2 },
  });
  assert.ok(!chipsUnfounded.some(([label]) => label.startsWith("已定稿")));
});

test("renderProjectQaDashboard filters by code, preserves textContent XSS safety, and triggers navigate", () => {
  const doc = fakeDoc();
  const summaryHost = doc.makeHost();
  const filtersHost = doc.makeHost();
  const issuesHost = doc.makeHost();
  const report = {
    founded: true,
    summary: {
      total_blocks: 3,
      translated_blocks: 2,
      untranslated_blocks: 1,
      finalized_blocks: 1,
      clean_blocks: 1,
      blocks_with_issues: 1,
      total_issues: 2,
    },
    by_code: { source_script_leak: 1, punctuation_mismatch: 1 },
    issues: [
      {
        block: "ch001-b001",
        paragraph: "p001",
        code: "source_script_leak",
        message: "残留 <img src=x onerror=alert(1)>",
        source_excerpt: "雨の夜",
        target_excerpt: "雨の夜<script>",
      },
      {
        block: "ch001-b001",
        paragraph: "p002",
        code: "punctuation_mismatch",
        message: "标点未配对",
        source_excerpt: "「あ」",
        target_excerpt: "「啊",
      },
    ],
  };

  let activeFilter = "";
  const navigated = [];
  const rerender = () =>
    renderProjectQaDashboard({
      doc,
      report,
      filter: activeFilter,
      summaryHost,
      filtersHost,
      issuesHost,
      formatLocation: (id) => id,
      onFilterChange(code) {
        activeFilter = code;
        rerender();
      },
      onNavigate(target) {
        navigated.push(target);
      },
    });
  rerender();

  // 3 filter buttons: All + 2 codes
  assert.equal(filtersHost.children.length, 3);
  assert.equal(filtersHost.children[0].getAttribute("aria-pressed"), "true");
  assert.equal(issuesHost.children[0].children.length, 2);
  // XSS payload stayed pure textContent (no innerHTML parsing)
  const firstText = collectText(issuesHost.children[0].children[0]);
  assert.ok(firstText.includes("<img src=x onerror=alert(1)>"));

  // Click filter for punctuation_mismatch
  filtersHost.children[2].click();
  assert.equal(activeFilter, "punctuation_mismatch");
  assert.equal(issuesHost.children[0].children.length, 1);
  assert.equal(filterProjectQaIssues(report, "punctuation_mismatch").length, 1);

  // Click jump button on remaining issue
  const goBtn = issuesHost.children[0].children[0].children[0].children[1];
  assert.equal(goBtn.textContent, "定位编辑");
  assert.equal(goBtn.dataset.view, "");
  goBtn.click();
  assert.deepEqual(navigated, [{ block: "ch001-b001", paragraph: "p002", kind: "draft" }]);
});

test("createProjectQaController discards stale responses after project reset", async () => {
  const doc = fakeDoc();
  const statusEl = doc.makeHost();
  const summaryHost = doc.makeHost();
  const filtersHost = doc.makeHost();
  const issuesHost = doc.makeHost();

  let currentProject = "book-a";
  let resolveSlow;
  const ctrl = createProjectQaController({
    doc,
    getProjectId: () => currentProject,
    fetchReport: () => new Promise((resolve) => { resolveSlow = resolve; }),
    statusEl,
    summaryHost,
    filtersHost,
    issuesHost,
  });

  const pending = ctrl.run();
  assert.ok(statusEl.textContent.includes("正在汇总"));

  // Switch project before slow response arrives
  currentProject = "book-b";
  ctrl.reset("book-b");
  resolveSlow({
    founded: false,
    summary: { total_blocks: 1, translated_blocks: 1, checked_blocks: 1, untranslated_blocks: 0, clean_blocks: 1, blocks_with_issues: 0, total_issues: 0 },
    by_code: {},
    issues: [],
  });
  const res = await pending;
  assert.equal(res, null);
  assert.equal(ctrl.getState().project, "book-b");
  assert.equal(ctrl.getState().report, null);
  assert.equal(summaryHost.children.length, 0);
});
