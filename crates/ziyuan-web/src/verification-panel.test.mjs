import test from "node:test";
import assert from "node:assert/strict";
import {
  SEMANTIC_CATEGORIES,
  createInitialVerificationDraft,
  buildVerificationRequestBody,
} from "./verification-panel.mjs";

test("createInitialVerificationDraft populates all six classical verification categories", () => {
  const draft = createInitialVerificationDraft("fp-123");
  assert.equal(draft.fingerprint, "fp-123");
  assert.equal(draft.checks.length, 6);
  assert.deepEqual(
    draft.checks.map((c) => c.category),
    Object.keys(SEMANTIC_CATEGORIES),
  );
  assert.equal(draft.evidence.status, "tentative");
});

test("buildVerificationRequestBody validates fingerprint, task+quote pairing, and evidence IDs", () => {
  const draft = createInitialVerificationDraft("fp-v1");
  assert.throws(
    () => buildVerificationRequestBody({ paragraph: "p1", draft, viewFingerprint: "fp-v2" }),
    /材料或记录已更新/,
  );

  draft.checks[0].task = "12";
  draft.checks[0].quote = "";
  assert.throws(
    () => buildVerificationRequestBody({ paragraph: "p1", draft, viewFingerprint: "fp-v1" }),
    /请同时填写审校诏令号和该条意见的原句/,
  );

  draft.checks[0].quote = "审校短引";
  draft.checks[0].evidence = "1, abc";
  assert.throws(
    () => buildVerificationRequestBody({ paragraph: "p1", draft, viewFingerprint: "fp-v1" }),
    /词证编号须为正整数/,
  );

  draft.checks[0].evidence = "1, 3";
  draft.checks[0].disposition = "accepted";
  draft.checks[0].resolution = "resolved";
  draft.checks[0].note = "已核实主语一致";
  const body = buildVerificationRequestBody({
    paragraph: "c0001-p0001",
    draft,
    viewFingerprint: "fp-v1",
  });
  assert.equal(body.paragraph, "c0001-p0001");
  assert.equal(body.fingerprint, "fp-v1");
  assert.deepEqual(body.checks[0].advice, [{ task: 12, quote: "审校短引" }]);
  assert.deepEqual(body.checks[0].evidence, [1, 3]);
});
