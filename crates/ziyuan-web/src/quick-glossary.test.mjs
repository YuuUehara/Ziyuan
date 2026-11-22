import test from "node:test";
import assert from "node:assert/strict";
import {
  TERM_CATEGORIES,
  extractSelectionText,
  validateQuickMaterialFields,
  buildQuickMaterialPayload,
  formatQuickMaterialReceipt,
} from "./quick-glossary.mjs";

test("extractSelectionText reads textarea selection or window selection safely", () => {
  const textarea = { value: "前往王都的旅途", selectionStart: 2, selectionEnd: 4 };
  assert.equal(extractSelectionText({}, textarea), "王都");

  const doc = {
    activeElement: null,
    defaultView: { getSelection: () => ({ toString: () => "  艾琳  " }) },
  };
  assert.equal(extractSelectionText(doc), "艾琳");
  assert.equal(extractSelectionText({}), "");
});

test("validateQuickMaterialFields and buildQuickMaterialPayload enforce required fields", () => {
  assert.ok(TERM_CATEGORIES.some((c) => c.value === "place"));
  assert.equal(validateQuickMaterialFields("term", { source: "", target: "王城" }).ok, false);
  assert.equal(validateQuickMaterialFields("term", { source: "王都", target: "" }).ok, false);
  assert.equal(validateQuickMaterialFields("term", { source: "王都", target: "王城" }).ok, true);

  assert.deepEqual(
    buildQuickMaterialPayload("term", { source: " 王都 ", target: " 王城 ", category: "place", note: "  " }),
    { kind: "term", source: "王都", target: "王城", category: "place" },
  );

  assert.equal(validateQuickMaterialFields("person", { name: "  " }).ok, false);
  assert.deepEqual(
    buildQuickMaterialPayload("person", { name: " 艾琳 ", alias: "琳儿", voice: "沉稳" }),
    { kind: "person", name: "艾琳", alias: "琳儿", voice: "沉稳" },
  );
});

test("formatQuickMaterialReceipt distinguishes new vs merged entries and reminds draft-only boundary", () => {
  const s1 = formatQuickMaterialReceipt({ kind: "term", id: "term-quick-001", updated_existing: false, revision: "r1" });
  assert.ok(s1.includes("已收录新术语") && s1.includes("尚未批准"));

  const s2 = formatQuickMaterialReceipt({ kind: "term", id: "term-quick-001", updated_existing: true, revision: "r2" });
  assert.ok(s2.includes("已合并到现有术语"));

  const s3 = formatQuickMaterialReceipt({ kind: "person", id: "person-quick-001", updated_existing: false, revision: "r3" });
  assert.ok(s3.includes("已收录新人物"));
});
