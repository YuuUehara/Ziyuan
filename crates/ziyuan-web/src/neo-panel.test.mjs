import test from "node:test";
import assert from "node:assert/strict";
import {
  NEO_SUBMODES,
  CLASSICAL_THEMES,
  NEO_EDICT_PRESETS,
  classifyTaskAttention,
  summarizeCourtForNeo,
  summarizeTranslationForNeo,
  renderNeoPanel,
} from "./neo-panel.mjs";

function makeFakeDoc() {
  const createNode = (tag) => ({
    tagName: String(tag).toUpperCase(),
    className: "",
    id: "",
    type: "",
    value: "",
    placeholder: "",
    checked: false,
    disabled: false,
    selected: false,
    title: "",
    textContent: "",
    dataset: {},
    style: {},
    attributes: {},
    listeners: {},
    children: [],
    get firstChild() {
      return this.children[0] || null;
    },
    appendChild(child) {
      if (child) this.children.push(child);
      return child;
    },
    append(...nodes) {
      for (const n of nodes) this.appendChild(n);
    },
    removeChild(child) {
      const idx = this.children.indexOf(child);
      if (idx >= 0) this.children.splice(idx, 1);
    },
    setAttribute(k, v) {
      this.attributes[k] = String(v);
    },
    addEventListener(ev, fn) {
      this.listeners[ev] = fn;
    },
  });
  const doc = {
    createElement(tag) {
      const n = createNode(tag);
      n.ownerDocument = doc;
      return n;
    },
    createTextNode(text) {
      return { tagName: "#TEXT", textContent: String(text), children: [] };
    },
  };
  return doc;
}

function collectText(node) {
  if (!node) return "";
  const own = node.textContent || "";
  const kids = (node.children || []).map(collectText).join(" ");
  return `${own} ${kids}`.trim();
}

function findNodes(node, pred, out = []) {
  if (!node) return out;
  if (pred(node)) out.push(node);
  for (const c of node.children || []) findNodes(c, pred, out);
  return out;
}

test("classical themes and Neo presets preserve imperial literary style", () => {
  assert.equal(NEO_SUBMODES.length, 4);
  assert.deepEqual(
    CLASSICAL_THEMES.map((t) => t.id),
    ["tianqing", "mo", "qinglv", "xiangzhi", "ye"],
  );
  assert.ok(NEO_EDICT_PRESETS.length >= 6);
  assert.ok(NEO_EDICT_PRESETS.some((p) => p.department === "works"));
  assert.ok(NEO_EDICT_PRESETS.some((p) => p.department === "justice"));
  const worksRepair = NEO_EDICT_PRESETS.find((p) => p.id === "works-repair");
  assert.ok(worksRepair);
  assert.match(worksRepair.text, /staged_diff/);
  assert.match(worksRepair.desc, /revert_staged/);
});

test("classifyTaskAttention and summarizeCourtForNeo compute attention breakdown accurately", () => {
  const state = {
    viewer: false,
    mode: { code: "cabinet", label: "内阁制" },
    retreat: false,
    running: false,
    watching: false,
    task_total: 4,
    open_total: 2,
    usage: { input: 1000, output: 500, cache_read: 400 },
    treasury: { limit: 5000, remaining: 3500 },
    officials: [
      { id: 1, name: "张居正", office: { code: "grand-secretary" }, purged: false },
      { id: 2, name: "宋应星", office: null, purged: false },
    ],
    vacancies: [{ office: "censor", impact: "左都御史空缺" }],
    departments: [{ code: "works", label: "工部", route: ["皇帝", "内阁首辅", "工部尚书"] }],
    tasks: [
      { id: 1, status: "pending", needs_approval: true, department: "工部", text: "巡检存储层" },
      {
        id: 2,
        status: "completed",
        needs_approval: false,
        changes: { state: "proposed" },
        department: "工部",
        text: "修补边界测试",
      },
      { id: 3, status: "completed", needs_approval: false, review: null, department: "刑部", text: "复核令牌" },
      { id: 4, status: "assigned", needs_approval: false, department: "礼部", text: "编纂实录" },
    ],
  };

  const summary = summarizeCourtForNeo(state);
  assert.equal(summary.attentionCount, 3);
  assert.equal(summary.approvalCount, 1);
  assert.equal(summary.changesCount, 1);
  assert.equal(summary.reviewCount, 1);
  assert.equal(summary.runningCount, 1);
  assert.equal(summary.usage.total, 1500);
  assert.equal(summary.usage.cacheHitPercent, 40);
  assert.equal(summary.treasury.usedPercent, 30);
  assert.equal(summary.appointedCount, 1);
  assert.equal(summary.totalOfficials, 2);

  // When in West Park retreat, completed tasks without proposed changes are auto-reviewed by Silijian.
  const retreatReview = classifyTaskAttention(
    { id: 3, status: "completed", review: null },
    true,
  );
  assert.equal(retreatReview.needsAttention, false);
});

test("summarizeTranslationForNeo aggregates chapters and block categories", () => {
  const projects = [{ id: "novel-1", title: "浮生记", source_language: "ja", chapters: 1 }];
  const detail = {
    project: projects[0],
    outline: [
      {
        chapter: "ch-1",
        title: "第一章 雨夜",
        paragraphs: 12,
        blocks: ["b-1", "b-2", "b-3", "b-4"],
        block_states: [
          { block: "b-1", state: { state: "finalized" } },
          { block: "b-2", state: { state: "human_approved" } },
          { block: "b-3", state: { state: "human_draft" } },
          { block: "b-4", state: { state: "untranslated" } },
        ],
      },
    ],
  };
  const summary = summarizeTranslationForNeo(projects, detail);
  assert.equal(summary.activeId, "novel-1");
  assert.equal(summary.totalBlocks, 4);
  assert.equal(summary.finalizedBlocks, 1);
  assert.equal(summary.approvedBlocks, 1);
  assert.equal(summary.draftBlocks, 1);
  assert.equal(summary.untranslatedBlocks, 1);
  assert.equal(summary.completionPercent, 50);
});

test("renderNeoPanel renders all 4 submodes and respects viewer read-only mode", () => {
  const doc = makeFakeDoc();
  const root = doc.createElement("div");

  let switchedSubmode = null;
  let chosenTheme = null;
  let issuedPayload = null;

  const baseState = {
    viewer: false,
    mode: { code: "chancellor", label: "丞相制" },
    modes: [
      { code: "chancellor", label: "丞相制" },
      { code: "direct", label: "废相亲政" },
      { code: "cabinet", label: "内阁制" },
    ],
    tasks: [
      { id: 9, status: "pending", needs_approval: true, department: "工部", text: "查验号舍沙箱" },
    ],
    departments: [{ code: "works", label: "工部", route: ["皇帝", "丞相", "工部尚书"] }],
    officials: [],
    vacancies: [],
  };

  renderNeoPanel(
    root,
    { submode: "overview", state: baseState, theme: "tianqing" },
    {
      onSelectSubmode: (id) => {
        switchedSubmode = id;
      },
    },
  );
  const overviewText = collectText(root);
  assert.ok(overviewText.includes("垂拱枢机 · 御前听政台"));
  assert.ok(!overviewText.includes("Neo"));
  assert.ok(overviewText.includes("御案急递"));
  assert.ok(overviewText.includes("朱笔批红"));

  const quickTab = findNodes(root, (n) => n.dataset?.neoTab === "quick-edict")[0];
  assert.ok(quickTab);
  quickTab.listeners.click();
  assert.equal(switchedSubmode, "quick-edict");

  // Render quick-edict submode and trigger edict issue
  renderNeoPanel(
    root,
    { submode: "quick-edict", state: baseState, theme: "tianqing" },
    {
      onIssueEdict: (payload) => {
        issuedPayload = payload;
      },
    },
  );
  const runBtn = findNodes(root, (n) => n.id === "neo-issue-run")[0];
  assert.ok(runBtn);
  runBtn.listeners.click();
  assert.equal(issuedPayload.department, "works");
  assert.equal(issuedPayload.run, true);

  // Viewer mode hides mutating edict buttons in quick-edict
  renderNeoPanel(
    root,
    { submode: "quick-edict", state: { ...baseState, viewer: true }, theme: "tianqing" },
    {},
  );
  assert.equal(findNodes(root, (n) => n.id === "neo-issue-run").length, 0);

  // Render ministry-theme submode and switch theme to xiangzhi
  renderNeoPanel(
    root,
    { submode: "ministry-theme", state: baseState, theme: "tianqing" },
    {
      onSelectTheme: (id) => {
        chosenTheme = id;
      },
    },
  );
  const xiangzhiBtn = findNodes(root, (n) => n.dataset?.neoTheme === "xiangzhi")[0];
  assert.ok(xiangzhiBtn);
  xiangzhiBtn.listeners.click();
  assert.equal(chosenTheme, "xiangzhi");
});

test("summarizeTranslationForNeo reads real top-level statuses array from /api/text/{project}", () => {
  const summary = summarizeTranslationForNeo(
    [{ id: "proj-1", title: "东都风物志", source_language: "ja", chapters: 2 }],
    {
      project: { id: "proj-1", title: "东都风物志", source_language: "ja", chapters: 2 },
      outline: [
        { chapter: "c0001", title: "第一章", blocks: ["c0001-b0001"] },
        { chapter: "c0002", title: "第二章", blocks: ["c0002-b0001", "c0002-b0002"] },
      ],
      statuses: [
        { block: "c0001-b0001", state: { state: "human_finalized" }, label: "已人工定稿" },
        { block: "c0002-b0001", state: { state: "human_approved" }, label: "已人工准奏" },
        { block: "c0002-b0002", state: { state: "not_started" }, label: "未开始" },
      ],
    },
  );
  assert.equal(summary.finalizedBlocks, 1);
  assert.equal(summary.approvedBlocks, 1);
  assert.equal(summary.untranslatedBlocks, 1);
  assert.equal(summary.completionPercent, 67);
  assert.equal(summary.chapters[0].blocks[0].label, "已人工定稿");
  assert.equal(summary.chapters[1].blocks[0].category, "approved");
});

test("translation-neo submode renders usage strip and opens project usage ledger", () => {
  const doc = makeFakeDoc();
  const root = doc.createElement("div");
  let openedUsage = false;

  renderNeoPanel(
    root,
    {
      submode: "translation-neo",
      state: { viewer: false, mode: { code: "cabinet", label: "内阁制" } },
      textProjects: [{ id: "proj-1", title: "东京梦华录", source_language: "ja", chapters: 1 }],
      textProject: {
        project: { id: "proj-1", title: "东京梦华录", source_language: "ja", chapters: 1 },
        outline: [{ chapter: "c0001", title: "第一章", blocks: ["c0001-b0001"] }],
        statuses: [{ block: "c0001-b0001", state: { state: "finalized" }, label: "已定稿" }],
      },
      textUsage: {
        project: "proj-1",
        compactText: "关联诏令 2 件 · 已记录 3,200 tokens · USD 0.0120（估算非账单）",
      },
    },
    {
      onOpenProjectUsage: () => {
        openedUsage = true;
      },
    },
  );

  assert.match(collectText(root), /度支账本/);
  assert.match(collectText(root), /已记录 3,200 tokens/);
  const usageBtn = findNodes(root, (n) => n.tagName === "BUTTON" && n.textContent === "译书度支账本")[0];
  assert.ok(usageBtn);
  usageBtn.listeners.click();
  assert.equal(openedUsage, true);
});
