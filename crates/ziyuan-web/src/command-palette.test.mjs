import test from "node:test";
import assert from "node:assert/strict";
import { buildPaletteSpecs, filterPaletteItems } from "./command-palette.mjs";

test("viewer mode hides mutating edict and run actions while keeping read-only exports", () => {
  const emperor = buildPaletteSpecs({ viewer: false });
  const viewer = buildPaletteSpecs({ viewer: true });
  assert.ok(emperor.some((i) => i.id === "op:edict"));
  assert.ok(emperor.some((i) => i.id === "op:run"));
  assert.ok(!viewer.some((i) => i.id === "op:edict"));
  assert.ok(!viewer.some((i) => i.id === "op:run"));
  assert.ok(viewer.some((i) => i.id === "export:csv"));
  assert.ok(viewer.some((i) => i.id === "export:json"));
});

test("multi-term Chinese and English fuzzy filter matches halls, exports, and tasks", () => {
  const items = buildPaletteSpecs({
    viewer: false,
    statusLabel: { Completed: "已完成" },
    tasks: [
      { id: 7, status: "Completed", department: "works", text: "修复 Windows\n命名管道测试" },
      { id: 8, status: "Completed", department: "rites", text: "校对第一章译文" },
    ],
  });
  assert.equal(filterPaletteItems(items, "csv 度支").length, 1);
  assert.equal(filterPaletteItems(items, "csv 度支")[0].id, "export:csv");

  const taskHits = filterPaletteItems(items, "#7 works");
  assert.equal(taskHits.length, 1);
  assert.equal(taskHits[0].action.taskId, 7);
  assert.equal(taskHits[0].label, "诏令 #7 · 修复 Windows 命名管道测试");

  assert.equal(filterPaletteItems(items, "nonexistent-xyz").length, 0);
});

test("caps task entries to 20 items", () => {
  const tasks = Array.from({ length: 30 }, (_, i) => ({ id: i + 1, status: "Pending", text: `任务 ${i + 1}` }));
  const items = buildPaletteSpecs({ viewer: true, tasks });
  const taskItems = items.filter((i) => i.id.startsWith("task:"));
  assert.equal(taskItems.length, 20);
});
