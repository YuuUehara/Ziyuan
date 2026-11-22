import test from "node:test";
import assert from "node:assert/strict";
import {
  QA_CODE_LABELS,
  qaCodeLabel,
  buildProjectQaSummaryChips,
  filterProjectQaIssues,
  renderProjectQaDashboard,
  createProjectQaController,
  createQaNavigator,
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

test("renderProjectQaDashboard renders ignore/restore controls and hides them for viewer", () => {
  const doc = fakeDoc();
  const report = {
    founded: false,
    summary: {
      total_blocks: 1,
      translated_blocks: 1,
      untranslated_blocks: 0,
      clean_blocks: 0,
      blocks_with_issues: 1,
      total_issues: 1,
      ignored_issues: 1,
    },
    by_code: { punctuation_mismatch: 1 },
    issues: [
      { block: "c0001-b0001", paragraph: "c0001-p0001", code: "punctuation_mismatch", message: "标点异常" },
    ],
    ignores: [
      { block: "c0001-b0001", paragraph: "c0001-p0002", code: "source_script_leak", note: "刻意保留" },
    ],
  };

  const chips = buildProjectQaSummaryChips(report);
  assert.ok(chips.some(([label]) => label === "已忽略 1"));

  const ignoredCalls = [];
  const restoredCalls = [];
  const summaryHost = doc.makeHost();
  const filtersHost = doc.makeHost();
  const issuesHost = doc.makeHost();

  renderProjectQaDashboard({
    doc,
    report,
    viewer: false,
    summaryHost,
    filtersHost,
    issuesHost,
    onIgnore: (i) => ignoredCalls.push(i),
    onUnignore: (e) => restoredCalls.push(e),
  });

  // Issue row has title, 定位编辑, 标记忽略
  const issueHead = issuesHost.children[0].children[0].children[0];
  assert.equal(issueHead.children.length, 3);
  assert.equal(issueHead.children[2].textContent, "标记忽略");
  issueHead.children[2].click();
  assert.equal(ignoredCalls.length, 1);
  assert.equal(ignoredCalls[0].code, "punctuation_mismatch");

  // Ignored details box has 恢复检查 button
  const details = issuesHost.children[1];
  assert.equal(details.tag, "details");
  const restoreBtn = details.children[1].children[0].children[0].children[1];
  assert.equal(restoreBtn.textContent, "恢复检查");
  restoreBtn.click();
  assert.equal(restoredCalls.length, 1);
  assert.equal(restoredCalls[0].code, "source_script_leak");

  // Viewer mode hides both 标记忽略 and 恢复检查
  renderProjectQaDashboard({
    doc,
    report,
    viewer: true,
    summaryHost,
    filtersHost,
    issuesHost,
    onIgnore: () => {},
    onUnignore: () => {},
  });
  assert.equal(issuesHost.children[0].children[0].children[0].children.length, 2);
  assert.equal(issuesHost.children[1].children[1].children[0].children[0].children.length, 1);
});

test("createQaNavigator steps prev/next cyclically, ignores current item, and closes when empty", async () => {
  const doc = fakeDoc();
  const host = doc.makeHost();
  host.hidden = true;
  const navigated = [];
  const ignored = [];

  const nav = createQaNavigator({
    host,
    doc,
    formatLocation: (id) => id,
    onNavigate: (t) => navigated.push(t),
    onIgnore: async (item) => { ignored.push(item.code); },
    isViewer: () => false,
  });

  const list = [
    { block: "b1", paragraph: "p1", code: "number_mismatch", message: "数字不符" },
    { block: "b2", paragraph: "p2", code: "punctuation_mismatch", message: "标点异常" },
  ];
  nav.start(list, 0);
  assert.equal(nav.isActive(), true);
  assert.equal(host.hidden, false);
  assert.ok(collectText(host).includes("QA 1/2"));

  nav.next();
  assert.equal(nav.getState().index, 1);
  assert.deepEqual(navigated.at(-1), { block: "b2", paragraph: "p2", kind: "draft" });

  // Wrap around to 0
  nav.next();
  assert.equal(nav.getState().index, 0);

  // Wrap backward to 1
  nav.prev();
  assert.equal(nav.getState().index, 1);

  // Ignore current removes it and jumps to remaining item
  await nav.ignoreCurrent();
  assert.deepEqual(ignored, ["punctuation_mismatch"]);
  assert.equal(nav.getState().total, 1);
  assert.equal(nav.getState().current.code, "number_mismatch");

  // Ignoring last item closes bar
  await nav.ignoreCurrent();
  assert.equal(nav.isActive(), false);
  assert.equal(host.hidden, true);
});
