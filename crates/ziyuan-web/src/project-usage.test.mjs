import test from "node:test";
import assert from "node:assert/strict";
import {
  summarizeProjectUsage,
  formatProjectUsageStatus,
  renderProjectUsageLedger,
  createProjectUsageController,
} from "./project-usage.mjs";

function makeNode(tag) {
  return {
    tagName: String(tag).toUpperCase(),
    className: "",
    type: "",
    title: "",
    textContent: "",
    dataset: {},
    listeners: {},
    children: [],
    append(...items) {
      for (const item of items) {
        if (item) this.children.push(item);
      }
    },
    replaceChildren(...items) {
      this.children = [];
      this.append(...items);
    },
    addEventListener(ev, fn) {
      this.listeners[ev] = fn;
    },
  };
}

function makeDoc() {
  return { createElement: (tag) => makeNode(tag) };
}

function collectText(node) {
  if (!node) return "";
  return [node.textContent || "", ...(node.children || []).map(collectText)]
    .filter(Boolean)
    .join(" ");
}

function findNodes(node, pred, out = []) {
  if (!node) return out;
  if (pred(node)) out.push(node);
  for (const child of node.children || []) findNodes(child, pred, out);
  return out;
}

test("summarizeProjectUsage preserves unknown usage and separates currencies", () => {
  const unknownReport = {
    founded: true,
    project: "novel-a",
    totals: {
      tasks: 2,
      statuses: { completed: 2 },
      recorded_usage: null,
      tasks_without_execution_usage: 2,
      estimates: {},
      unpriced_usage: { input: 0, output: 0, cache_read: 0 },
    },
    stages: [],
    tasks: [],
    notes: [],
  };
  const s1 = summarizeProjectUsage(unknownReport);
  assert.equal(s1.hasRecorded, false);
  assert.equal(s1.recordedTotal, null);
  assert.match(s1.compactText, /用量未知/);
  assert.doesNotMatch(s1.compactText, /已记录 0 tokens/);

  const multiCurrency = {
    founded: true,
    project: "novel-a",
    totals: {
      tasks: 3,
      statuses: { completed: 3 },
      recorded_usage: { input: 1200, output: 800, cache_read: 300 },
      tasks_without_execution_usage: 0,
      estimates: { CNY: 0.045, USD: 0.0125 },
      unpriced_usage: { input: 100, output: 50, cache_read: 0 },
    },
    stages: [],
    tasks: [{ task: 7, usage_reconciled: false }],
    notes: [],
  };
  const s2 = summarizeProjectUsage(multiCurrency);
  assert.equal(s2.recordedTotal, 2000);
  assert.equal(s2.estimates.length, 2);
  assert.equal(s2.estimates[0].currency, "CNY");
  assert.equal(s2.estimates[1].currency, "USD");
  assert.equal(s2.unpricedTotal, 150);
  assert.equal(s2.unreconciledCount, 1);
  assert.match(formatProjectUsageStatus( { founded: false } ), /尚未建朝/);
});

test("renderProjectUsageLedger renders four stages and invokes task/block navigation callbacks", () => {
  const doc = makeDoc();
  const summaryHost = makeNode("div");
  const stagesHost = makeNode("div");
  const tasksHost = makeNode("div");
  const notesHost = makeNode("div");
  const actions = [];

  renderProjectUsageLedger({
    doc,
    report: {
      founded: true,
      project: "book-1",
      totals: {
        tasks: 2,
        statuses: { completed: 1, failed: 1 },
        recorded_usage: { input: 1000, output: 500, cache_read: 200 },
        tasks_without_execution_usage: 1,
        estimates: { USD: 0.0095 },
        unpriced_usage: { input: 200, output: 100, cache_read: 0 },
      },
      stages: [
        { kind: "translate", totals: { tasks: 1, recorded_usage: { input: 800, output: 400, cache_read: 200 }, estimates: { USD: 0.008 }, tool_calls: 2, tool_failures: 0 } },
        { kind: "fidelity_review", totals: { tasks: 1, recorded_usage: null, tasks_without_execution_usage: 1, estimates: {} } },
        { kind: "style_review", totals: { tasks: 0, estimates: {} } },
        { kind: "revise", totals: { tasks: 0, estimates: {} } },
      ],
      tasks: [
        {
          task: 12,
          block: "c0001-b0001",
          kind: "translate",
          status: "completed",
          attempts: 1,
          budget: 4000,
          recorded_execution_usage: { input: 800, output: 400, cache_read: 200 },
          usage_reconciled: true,
          charges: [
            {
              purpose: "execution",
              model: "gpt-5.4",
              usage: { input: 800, output: 400, cache_read: 200 },
              rate: { input_per_million: 2.5, output_per_million: 10, currency: "USD", source: "内置参考单价" },
              estimated_cost: 0.006,
            },
          ],
        },
      ],
      notes: ["缺失 usage 为未知，不按零计算。"],
    },
    summaryHost,
    stagesHost,
    tasksHost,
    notesHost,
    formatLocation: (id) => (id === "c0001-b0001" ? "第1章 · 译稿第1组" : id),
    onInspectTask: (id) => actions.push(["task", id]),
    onOpenBlock: (blk) => actions.push(["block", blk]),
  });

  assert.equal(stagesHost.children.length, 4);
  assert.match(collectText(stagesHost), /初译/);
  assert.match(collectText(stagesHost), /忠实审校/);
  assert.match(collectText(stagesHost), /用量未知/);
  assert.match(collectText(summaryHost), /非账单/);
  assert.match(collectText(notesHost), /未报告用量视为未知/);

  const buttons = findNodes(tasksHost, (n) => n.tagName === "BUTTON");
  assert.equal(buttons.length, 2);
  buttons[0].listeners.click();
  buttons[1].listeners.click();
  assert.deepEqual(actions, [
    ["task", 12],
    ["block", "c0001-b0001"],
  ]);
});

test("createProjectUsageController ignores stale project responses", async () => {
  const doc = makeDoc();
  const statusEl = makeNode("p");
  const summaryHost = makeNode("div");
  const stagesHost = makeNode("div");
  const tasksHost = makeNode("div");
  const notesHost = makeNode("div");
  let activeProject = "p1";
  let resolveP1;

  const ctrl = createProjectUsageController({
    doc,
    getProjectId: () => activeProject,
    fetchUsage: (proj) =>
      new Promise((resolve) => {
        if (proj === "p1") resolveP1 = resolve;
        else resolve({ founded: true, project: proj, totals: { tasks: 0 }, stages: [], tasks: [], notes: [] });
      }),
    statusEl,
    summaryHost,
    stagesHost,
    tasksHost,
    notesHost,
  });

  const p1Promise = ctrl.run();
  activeProject = "p2";
  ctrl.reset("p2");
  resolveP1({ founded: true, project: "p1", totals: { tasks: 5 }, stages: [], tasks: [], notes: [] });
  const stale = await p1Promise;
  assert.equal(stale, null);
  assert.equal(ctrl.getState().project, "p2");
  assert.equal(ctrl.getState().report, null);
});
