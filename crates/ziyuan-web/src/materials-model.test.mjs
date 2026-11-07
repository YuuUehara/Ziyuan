import test from "node:test";
import assert from "node:assert/strict";
import { MaterialModel } from "./materials-model.mjs";

const clone = (value) => JSON.parse(JSON.stringify(value));
const revision = (digit) => digit.repeat(64);
const ORIGIN = "https://local.example";
const WORKSPACE = "a".repeat(64);

function material(source = "先生") {
  return {
    format: 2,
    terms: [{
      term_id: "t-1", source, category: "title", allowed: ["先生", "老师"],
      note: "备注", scope: { kind: "project" },
      evidence: [{ paragraph: "p1", quote: "先生", explanation: "称呼",
        stance: "project_fact", status: "confirmed" }], status: "confirmed",
    }],
    persons: [{
      person_id: "person-1", name: "老师", reading: "せんせい", aliases: ["先生"],
      terms: ["t-1"], address_rules: [{
        target: "person-1", speaker: { kind: "unknown" }, scope: { kind: "project" },
        term: "t-1", evidence: [],
      }], voice: { description: "庄重", examples: ["先生来了。"] },
      evidence: [], status: "tentative",
    }],
  };
}

function view(source = "先生", rev = revision("1"), workspace = WORKSPACE) {
  return {
    workspace_id: workspace, project_hash: revision("f"),
    project: {
      id: "book", metadata: { title: "书", source_language: "ja", target_language: "zh",
        provenance: { author: "", version: "", source: "", date: "", rights: "", scope: "" } },
      block_bytes: 1000, chapters: [{
        id: "chapter1", source_name: "", original_path: "", sha256: revision("0"),
        byte_len: 15, paragraphs: [{
          id: "p1", byte_start: 0, byte_end: 15, text: "先生来了。",
        }], blocks: [],
      }],
    },
    draft: rev === null ? null : { project: "book", revision: rev, material: material(source) },
  };
}

function storage() {
  const values = new Map();
  return {
    values, reads: [], fail: false, quota: false,
    getItem(key) { this.reads.push(key); return values.get(key) ?? null; },
    setItem(key, value) {
      if (this.fail || this.quota) throw new Error("QuotaExceededError");
      values.set(key, value);
    },
    removeItem(key) {
      if (this.fail) throw new Error("Storage blocked");
      values.delete(key);
    },
  };
}

function model(options = {}) {
  let id = 0;
  return new MaterialModel({
    origin: ORIGIN, project: "book", storage: storage(),
    requestId: () => `request-${++id}`, ...options,
  });
}

function receipt(rev = revision("2"), workspace = WORKSPACE) {
  return { workspace_id: workspace, revision: rev, content_hash: revision("c") };
}

test("GET supplies workspace identity before the only mirror read; recovery is explicit", () => {
  const session = storage();
  const original = model({ storage: session });
  assert.equal(session.reads.length, 0);
  original.receive(view());
  original.edit(["terms", 0, "source"], "本地");
  const next = model({ storage: session });
  assert.equal(session.reads.length, 1);
  next.receive(view());
  assert.equal(session.reads.length, 2);
  assert.equal(next.material.terms[0].source, "先生");
  assert.ok(next.recoveryChoice);
  next.restore();
  assert.equal(next.material.terms[0].source, "本地");
  assert.equal(next.dirty, true);
});

test("different workspaces, origins, projects and modules never fall back to old keys", () => {
  const session = storage();
  const first = model({ storage: session });
  first.receive(view());
  first.edit(["terms", 0, "source"], "隔离");
  for (const options of [
    { workspace: "b".repeat(64) }, { origin: "https://other.example" },
    { project: "other-book" }, { module: "other-module" },
  ]) {
    const other = model({ storage: session, ...options });
    const remote = view("先生", revision("1"), options.workspace ?? WORKSPACE);
    remote.project.id = options.project ?? "book";
    if (remote.draft) remote.draft.project = remote.project.id;
    other.receive(remote);
    assert.equal(other.recoveryChoice, null);
    assert.notEqual(other.mirrorKey, first.mirrorKey);
    assert.equal(other.restore(), false);
  }
  assert.equal(session.reads.length, 5);
});

test("a copied mirror must match every embedded identity field", () => {
  for (const field of ["origin", "workspace_id", "project", "module"]) {
    const session = storage();
    const first = model({ storage: session });
    first.receive(view());
    first.edit(["terms", 0, "source"], "本地");
    const copied = JSON.parse(session.values.get(first.mirrorKey));
    copied.identity[field] = "wrong";
    session.values.set(first.mirrorKey, JSON.stringify(copied));
    const next = model({ storage: session });
    next.receive(view());
    assert.equal(next.recoveryChoice, null);
    assert.equal(next.restore(), false);
  }
});

test("restoring an old baseline is dirty and conflicted, not silently rebased", () => {
  const session = storage();
  const first = model({ storage: session });
  first.receive(view());
  first.edit(["terms", 0, "source"], "旧基线的输入");
  const next = model({ storage: session });
  next.receive(view("服务端新输入", revision("3")));
  next.restore();
  assert.equal(next.material.terms[0].source, "旧基线的输入");
  assert.equal(next.remoteRevision, revision("3"));
  assert.equal(next.conflict, true);
  assert.equal(next.dirty, true);
  assert.equal(next.prepareSave().expected_revision, revision("1"));
});

test("quota failure and oversized mirror preserve complete in-memory input and warn", () => {
  for (const quota of [true, false]) {
    const session = storage();
    const state = model({ storage: session, maxMirrorBytes: quota ? 1_000_000 : 100 });
    state.receive(view());
    session.fail = quota;
    const source = "长输入".repeat(1000);
    state.edit(["terms", 0, "source"], source);
    assert.equal(state.material.terms[0].source, source);
    assert.ok(state.mirrorError);
    assert.equal(state.dirty, true);
    assert.equal(state.prepareSave().material.terms[0].source, source);
  }
});

test("semantic validation errors do not disable saving", () => {
  const state = model();
  state.receive(view());
  state.edit(["persons", 0, "terms"], ["missing-term"]);
  state.validation = { valid: false, issues: [{
    path: ["persons", 0, "terms", 0], code: "missing_term", message: "术语不存在",
  }], content_hash: revision("c"), project_hash: revision("f") };
  assert.equal(state.canSave, true);
  assert.deepEqual(state.prepareSave().material, state.material);
});

test("a late save acknowledges only 旧输入, never overwriting the later 新输入", () => {
  const state = model();
  state.receive(view());
  state.edit(["terms", 0, "source"], "旧输入");
  const request = state.prepareSave();
  state.edit(["terms", 0, "source"], "新输入");
  assert.equal(request.material.terms[0].source, "旧输入");
  state.acceptSave(receipt(), request);
  assert.equal(state.material.terms[0].source, "新输入");
  assert.equal(state.savedSnapshot.terms[0].source, "旧输入");
  assert.equal(state.remoteRevision, revision("2"));
  assert.equal(state.dirty, true);
  assert.equal(state.prepareSave().expected_revision, revision("2"));
});

test("an unchanged failed save retries identical id and payload; edited input gets a new request", () => {
  const state = model();
  state.receive(view());
  state.edit(["terms", 0, "source"], "第一次");
  const request = state.prepareSave();
  assert.deepEqual(state.prepareSave(), request);
  assert.equal(request.request_id, "request-1");
  state.edit(["terms", 0, "source"], "第二次");
  const newer = state.prepareSave();
  assert.equal(newer.request_id, "request-2");
  assert.equal(newer.material.terms[0].source, "第二次");
  state.acceptSave(receipt(), request);
  assert.deepEqual(state.pendingSave, newer);
  assert.equal(state.material.terms[0].source, "第二次");
});

test("historical receipts request a server refresh without rolling baseline or input backwards", () => {
  let refreshes = 0;
  const state = model({ refresh: () => { refreshes++; } });
  state.receive(view());
  state.edit(["terms", 0, "source"], "旧输入");
  const request = state.prepareSave();
  state.receive(view("远端更新", revision("3")));
  state.edit(["terms", 0, "source"], "新输入");
  const baseline = clone(state.savedSnapshot);
  state.acceptSave(receipt(revision("2")), request);
  assert.equal(state.material.terms[0].source, "新输入");
  assert.deepEqual(state.savedSnapshot, baseline);
  assert.equal(state.remoteRevision, revision("3"));
  assert.equal(state.needsRefresh, true);
  assert.equal(refreshes, 1);
  state.receive(view("远端更新", revision("3")));
  assert.equal(state.material.terms[0].source, "新输入");
  assert.equal(state.dirty, true);
  assert.equal(state.needsRefresh, false);
});

test("a receipt for an older concurrent request cannot roll a newer acknowledgement back", () => {
  let refreshes = 0;
  const state = model({ refresh: () => refreshes++ });
  state.receive(view());
  state.edit(["terms", 0, "source"], "旧输入");
  const older = state.prepareSave();
  state.edit(["terms", 0, "source"], "新输入");
  const newer = state.prepareSave();
  state.acceptSave(receipt(revision("3")), newer);
  state.acceptSave(receipt(revision("2")), older);
  assert.equal(state.remoteRevision, revision("3"));
  assert.equal(state.savedSnapshot.terms[0].source, "新输入");
  assert.equal(state.material.terms[0].source, "新输入");
  assert.equal(refreshes, 1);
});

test("entry keys are independent of editable ids and never leak into material payload", () => {
  const state = model();
  state.receive(view());
  const termKey = state.entryKeys.terms[0];
  const personKey = state.entryKeys.persons[0];
  state.edit(["terms", 0, "term_id"], "renamed");
  state.edit(["persons", 0, "person_id"], "renamed");
  state.edit(["terms", 1], material().terms[0]);
  assert.equal(state.entryKeys.terms[0], termKey);
  assert.equal(state.entryKeys.persons[0], personKey);
  assert.notEqual(state.entryKeys.terms[1], termKey);
  assert.deepEqual(Object.keys(state.prepareSave()).sort(),
    ["expected_revision", "material", "request_id", "workspace_id"]);
  assert.deepEqual(state.prepareSave().material, state.material);
});

test("mirror stores only durable form state and restores entry keys and pending retry", () => {
  const session = storage();
  const state = model({ storage: session });
  state.receive(view());
  state.edit(["terms", 0, "source"], "本地");
  state.preview = { report: "temporary-report" };
  state.associations = [{ paragraph: "temporary-association" }];
  const request = state.prepareSave();
  const serialized = session.values.get(state.mirrorKey);
  assert.ok(!serialized.includes("temporary-"));
  const next = model({ storage: session });
  next.receive(view());
  next.restore();
  assert.deepEqual(next.entryKeys, state.entryKeys);
  assert.deepEqual(next.prepareSave(), request);
  assert.equal(next.preview, null);
});

test("edits invalidate validation and preview and advance the local epoch", () => {
  const state = model();
  state.receive(view());
  const before = state.localEpoch;
  state.validation = { valid: true, issues: [] };
  state.preview = { report: "stale" };
  state.edit(["persons", 0, "voice", "examples"], ["新例子"]);
  assert.equal(state.localEpoch, before + 1);
  assert.equal(state.validation, null);
  assert.equal(state.preview, null);
  state.preview = { report: "another" };
  state.invalidatePreview();
  assert.equal(state.preview, null);
});

test("confirmed discard adopts latest server material and removes the mirror", () => {
  const session = storage();
  const state = model({ storage: session });
  state.receive(view());
  state.edit(["terms", 0, "source"], "本地");
  state.receive(view("最新服务端", revision("3")));
  state.discardLocalConfirmed();
  assert.equal(state.material.terms[0].source, "最新服务端");
  assert.equal(state.dirty, false);
  assert.equal(state.conflict, false);
  assert.equal(state.pendingSave, null);
  assert.equal(session.values.has(state.mirrorKey), false);
});

test("missing or mismatched GET identity and foreign receipts cannot change state", () => {
  const session = storage();
  const state = model({ storage: session });
  const invalid = view();
  delete invalid.workspace_id;
  assert.throws(() => state.receive(invalid));
  assert.equal(session.reads.length, 0);
  assert.equal(state.canSave, false);
  assert.throws(() => state.prepareSave());
  state.receive(view());
  state.edit(["terms", 0, "source"], "保留");
  const request = state.prepareSave();
  assert.equal(state.acceptSave(receipt(revision("2"), "b".repeat(64)), request), false);
  assert.equal(state.remoteRevision, revision("1"));
  const wrongProject = view();
  wrongProject.project.id = "other";
  assert.throws(() => state.receive(wrongProject));
  assert.equal(state.material.terms[0].source, "保留");
});

test("absent drafts start as a complete empty material and save with a null baseline", () => {
  const state = model();
  state.receive(view("", null));
  assert.deepEqual(state.material, { format: 2, terms: [], persons: [] });
  state.edit(["terms"], material().terms);
  assert.equal(state.prepareSave().expected_revision, null);
});

test("corrupt and unavailable storage warn without preventing edits or saves", () => {
  const session = storage();
  const state = model({ storage: session });
  state.receive(view());
  session.values.set(state.mirrorKey, "{bad JSON");
  const next = model({ storage: session });
  next.receive(view());
  assert.equal(next.recoveryChoice, null);
  assert.ok(next.mirrorError);
  next.edit(["terms", 0, "source"], "可保存");
  assert.equal(next.prepareSave().material.terms[0].source, "可保存");
  const unavailable = model({ storage: null });
  unavailable.receive(view());
  unavailable.edit(["terms", 0, "source"], "仍保留");
  assert.ok(unavailable.mirrorError);
  assert.equal(unavailable.material.terms[0].source, "仍保留");
});

test("reordering and removing entries retains independent keys, including across unchanged GETs", () => {
  const state = model();
  state.receive(view());
  state.edit(["terms", 1], material("第二条").terms[0]);
  const [firstKey, secondKey] = state.entryKeys.terms;
  const [first, second] = state.material.terms;
  state.edit(["terms"], [second, first]);
  assert.deepEqual(state.entryKeys.terms, [secondKey, firstKey]);
  state.edit(["terms"], [state.material.terms[1]]);
  assert.deepEqual(state.entryKeys.terms, [firstKey]);
  state.acceptSave(receipt(), state.prepareSave());
  const keys = clone(state.entryKeys);
  state.receive(view("先生", revision("2")));
  assert.deepEqual(state.entryKeys, keys);
});

test("recovered newer input whose pending request targets a superseded revision is rebuilt", () => {
  const session = storage();
  const state = model({ storage: session });
  state.receive(view());
  state.edit(["terms", 0, "source"], "旧输入");
  const older = state.prepareSave();
  state.edit(["terms", 0, "source"], "新输入");
  const newer = state.prepareSave();
  state.acceptSave(receipt(), older);
  let id = 0;
  const next = model({ storage: session, requestId: () => `reload-${++id}` });
  next.receive(view("旧输入", revision("2")));
  assert.equal(next.restore(), true);
  assert.equal(next.material.terms[0].source, "新输入");
  assert.equal(next.dirty, true);
  assert.equal(next.conflict, false);
  const retry = next.prepareSave();
  assert.notEqual(retry.request_id, newer.request_id);
  assert.equal(retry.expected_revision, revision("2"));
  assert.equal(retry.material.terms[0].source, "新输入");
  assert.deepEqual(next.prepareSave(), retry);
});

test("an unchanged pending request is rebuilt against the baseline an older receipt committed", () => {
  const state = model();
  state.receive(view());
  state.edit(["terms", 0, "source"], "旧输入");
  const older = state.prepareSave();
  state.edit(["terms", 0, "source"], "新输入");
  const newer = state.prepareSave();
  assert.equal(state.acceptSave(receipt(revision("2")), older), true);
  const retry = state.prepareSave();
  assert.notEqual(retry.request_id, newer.request_id);
  assert.equal(retry.expected_revision, revision("2"));
  assert.equal(retry.material.terms[0].source, "新输入");
  assert.deepEqual(state.prepareSave(), retry);
  assert.equal(state.acceptSave(receipt(revision("3")), retry), true);
  assert.equal(state.dirty, false);
  assert.equal(state.remoteRevision, revision("3"));
});

test("a GET of the just-submitted material before its receipt adopts the newer baseline", () => {
  let refreshes = 0;
  const state = model({ refresh: () => refreshes++ });
  state.receive(view());
  state.edit(["terms", 0, "source"], "已提交");
  const request = state.prepareSave();
  state.receive(view("已提交", revision("3")));
  assert.equal(state.conflict, false);
  assert.equal(state.dirty, false);
  assert.equal(state.pendingSave, null);
  assert.equal(state.acceptSave(receipt(revision("3")), request), true);
  assert.equal(state.remoteRevision, revision("3"));
  assert.equal(state.needsRefresh, false);
  assert.equal(refreshes, 0);
});

test("a GET of the submitted snapshot keeps later typing dirty without endless conflict or refresh", () => {
  let refreshes = 0;
  const state = model({ refresh: () => refreshes++ });
  state.receive(view());
  state.edit(["terms", 0, "source"], "已提交");
  const request = state.prepareSave();
  state.edit(["terms", 0, "source"], "提交后");
  for (let poll = 0; poll < 2; poll++) {
    state.receive(view("已提交", revision("3")));
    assert.equal(state.material.terms[0].source, "提交后");
    assert.equal(state.savedSnapshot.terms[0].source, "已提交");
    assert.equal(state.remoteRevision, revision("3"));
    assert.equal(state.conflict, false);
    assert.equal(state.dirty, true);
  }
  assert.equal(state.acceptSave(receipt(revision("3")), request), true);
  assert.equal(state.needsRefresh, false);
  assert.equal(refreshes, 0);
  assert.equal(state.material.terms[0].source, "提交后");
  const next = state.prepareSave();
  assert.equal(next.expected_revision, revision("3"));
  assert.equal(next.material.terms[0].source, "提交后");
});

test("restoring a pending request whose commit succeeded but receipt was lost needs no discard", () => {
  for (const typedAfter of [false, true]) {
    const session = storage();
    const state = model({ storage: session });
    state.receive(view());
    state.edit(["terms", 0, "source"], "已提交");
    state.prepareSave();
    if (typedAfter) state.edit(["terms", 0, "source"], "提交后");
    let id = 0;
    const next = model({ storage: session, requestId: () => `reload-${++id}` });
    next.receive(view("已提交", revision("2")));
    assert.equal(next.restore(), true);
    assert.equal(next.conflict, false);
    assert.equal(next.remoteRevision, revision("2"));
    assert.equal(next.savedSnapshot.terms[0].source, "已提交");
    assert.equal(next.pendingSave, null);
    assert.equal(next.dirty, typedAfter);
    assert.equal(next.material.terms[0].source, typedAfter ? "提交后" : "已提交");
    if (typedAfter) assert.equal(next.prepareSave().expected_revision, revision("2"));
  }
});

test("a reverted edit survives a GET of its still-unconfirmed submission", () => {
  let refreshes = 0;
  const state = model({ refresh: () => refreshes++ });
  state.receive(view());
  state.edit(["terms", 0, "source"], "已提交");
  const request = state.prepareSave();
  state.edit(["terms", 0, "source"], "先生");
  assert.equal(state.dirty, false);
  for (let poll = 0; poll < 2; poll++) {
    state.receive(view("已提交", revision("3")));
    assert.equal(state.material.terms[0].source, "先生");
    assert.equal(state.savedSnapshot.terms[0].source, "已提交");
    assert.equal(state.remoteRevision, revision("3"));
    assert.equal(state.conflict, false);
    assert.equal(state.dirty, true);
  }
  assert.equal(state.acceptSave(receipt(revision("3")), request), true);
  assert.equal(state.material.terms[0].source, "先生");
  assert.equal(refreshes, 0);
  const next = state.prepareSave();
  assert.notEqual(next.request_id, request.request_id);
  assert.equal(next.expected_revision, revision("3"));
  assert.equal(next.material.terms[0].source, "先生");
});

test("a clean model adopts server changes once no submission can still commit on its baseline", () => {
  const state = model();
  state.receive(view());
  state.edit(["terms", 0, "source"], "已提交");
  const request = state.prepareSave();
  state.acceptSave(receipt(revision("2")), request);
  state.receive(view("他处修改", revision("3")));
  assert.equal(state.material.terms[0].source, "他处修改");
  assert.equal(state.dirty, false);
  const unchanged = model();
  unchanged.receive(view());
  unchanged.prepareSave();
  unchanged.receive(view("他处修改", revision("3")));
  assert.equal(unchanged.material.terms[0].source, "他处修改");
  assert.equal(unchanged.conflict, false);
});

test("a failed later mirror write removes the stale mirror so reload does not offer it", () => {
  for (const failure of ["quota", "oversize"]) {
    const session = storage();
    const state = model({ storage: session, maxMirrorBytes: 20_000 });
    state.receive(view());
    state.edit(["terms", 0, "source"], "较早输入");
    assert.ok(session.values.has(state.mirrorKey));
    if (failure === "quota") session.quota = true;
    const source = failure === "quota" ? "配额失败" : "长输入".repeat(5000);
    state.edit(["terms", 0, "source"], source);
    assert.equal(state.material.terms[0].source, source);
    assert.ok(state.mirrorError);
    assert.equal(session.values.has(state.mirrorKey), false);
    const next = model({ storage: session });
    next.receive(view());
    assert.equal(next.recoveryChoice, null);
    assert.equal(next.restore(), false);
  }
});

test("an unresolved recovery mirror is not overwritten by new typing or save preparation", () => {
  const session = storage();
  const original = model({ storage: session });
  original.receive(view());
  original.edit(["terms", 0, "source"], "待恢复");
  const savedMirror = session.values.get(original.mirrorKey);
  const next = model({ storage: session });
  next.receive(view());
  next.edit(["terms", 0, "source"], "刚输入");
  next.prepareSave();
  assert.equal(next.material.terms[0].source, "刚输入");
  assert.equal(session.values.get(next.mirrorKey), savedMirror);
  assert.equal(next.restore(), true);
  assert.equal(next.material.terms[0].source, "待恢复");
});

test("default request generation works on HTTP without randomUUID", () => {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis.crypto, "randomUUID");
  Object.defineProperty(globalThis.crypto, "randomUUID", { configurable: true, value: undefined });
  try {
    const state = model({ requestId: undefined });
    state.receive(view());
    state.edit(["terms", 0, "source"], "HTTP");
    const request = state.prepareSave();
    assert.match(request.request_id, /^[a-zA-Z0-9_-]{1,128}$/);
    assert.deepEqual(state.prepareSave(), request);
  } finally {
    if (descriptor) Object.defineProperty(globalThis.crypto, "randomUUID", descriptor);
    else delete globalThis.crypto.randomUUID;
  }
});

test("successful save clears mirror and duplicate receipts do not invalidate newer edits", () => {
  const session = storage();
  const state = model({ storage: session });
  state.receive(view());
  state.edit(["terms", 0, "source"], "保存");
  const request = state.prepareSave();
  assert.throws(() => { request.material.terms[0].source = "篡改"; }, TypeError);
  state.acceptSave(receipt(), request);
  assert.equal(state.dirty, false);
  assert.equal(session.values.has(state.mirrorKey), false);
  state.edit(["terms", 0, "source"], "后续");
  assert.equal(state.acceptSave(receipt(), request), false);
  assert.equal(state.material.terms[0].source, "后续");
  assert.equal(state.needsRefresh, false);
});

test("server field order and an added optional field still reconcile the just-saved content", () => {
  let refreshes = 0;
  const state = model({ refresh: () => refreshes++ });
  state.receive(view());
  state.edit(["terms", 0, "available_from"], { chapter: "chapter1" });
  state.edit(["persons", 0, "available_from"], { chapter: "chapter1", paragraph: "p1" });
  const request = state.prepareSave();
  const local = request.material;
  const { term_id, source, category, allowed, note, scope, available_from, evidence, status } =
    local.terms[0];
  const person = local.persons[0];
  const serverOrder = view("先生", revision("2"));
  serverOrder.draft.material = {
    format: 2,
    terms: [{ term_id, source, category, allowed, note, scope, available_from, evidence, status }],
    persons: [{
      person_id: person.person_id, available_from: person.available_from, name: person.name,
      reading: person.reading, aliases: person.aliases, terms: person.terms,
      address_rules: person.address_rules, voice: person.voice,
      evidence: person.evidence, status: person.status,
    }],
  };
  assert.notEqual(JSON.stringify(serverOrder.draft.material), JSON.stringify(local));
  state.receive(serverOrder);
  assert.equal(state.conflict, false);
  assert.equal(state.dirty, false);
  assert.equal(state.pendingSave, null);
  assert.equal(state.acceptSave(receipt(revision("2")), request), true);
  assert.equal(refreshes, 0);
  state.edit(["terms", 0, "note"], "后续");
  assert.equal(state.prepareSave().expected_revision, revision("2"));
});

test("a baseline rebased by GET is persisted so reload restores it without a false conflict", () => {
  const session = storage();
  const state = model({ storage: session });
  state.receive(view());
  state.edit(["terms", 0, "source"], "X");
  state.prepareSave();
  state.edit(["terms", 0, "source"], "Y");
  state.prepareSave();
  state.receive(view("X", revision("2")));
  assert.equal(state.conflict, false);
  let id = 0;
  const next = model({ storage: session, requestId: () => `reload-${++id}` });
  next.receive(view("X", revision("2")));
  assert.equal(next.restore(), true);
  assert.equal(next.conflict, false);
  assert.equal(next.savedSnapshot.terms[0].source, "X");
  assert.equal(next.material.terms[0].source, "Y");
  assert.equal(next.dirty, true);
  const retry = next.prepareSave();
  assert.equal(retry.expected_revision, revision("2"));
  assert.equal(retry.material.terms[0].source, "Y");
});

test("a GET rebase does not overwrite an unresolved recovery mirror", () => {
  const session = storage();
  const original = model({ storage: session });
  original.receive(view());
  original.edit(["terms", 0, "source"], "待恢复");
  const savedMirror = session.values.get(original.mirrorKey);
  const next = model({ storage: session });
  next.receive(view());
  next.edit(["terms", 0, "source"], "刚输入");
  next.prepareSave();
  next.receive(view("刚输入", revision("2")));
  assert.equal(next.conflict, false);
  assert.equal(session.values.get(next.mirrorKey), savedMirror);
  assert.equal(next.restore(), true);
  assert.equal(next.material.terms[0].source, "待恢复");
});

test("a GET begun before a successful save cannot roll the acknowledged state back", () => {
  let refreshes = 0;
  const state = model({ refresh: () => refreshes++ });
  assert.notEqual(state.receive(view(), state.beginReceive()), false);
  state.edit(["terms", 0, "source"], "X");
  const request = state.prepareSave();
  const stale = state.beginReceive();
  assert.equal(state.acceptSave(receipt(revision("2")), request), true);
  assert.equal(state.receive(view(), stale), false);
  assert.equal(state.material.terms[0].source, "X");
  assert.equal(state.savedSnapshot.terms[0].source, "X");
  assert.equal(state.remoteRevision, revision("2"));
  assert.equal(state.dirty, false);
  assert.equal(state.needsRefresh, true);
  assert.equal(refreshes, 1);
  assert.notEqual(state.receive(view("X", revision("2")), state.beginReceive()), false);
  assert.equal(state.needsRefresh, false);
  assert.equal(state.material.terms[0].source, "X");
});

test("out-of-order GET responses keep the newest applied view", () => {
  const state = model();
  state.receive(view());
  const older = state.beginReceive();
  const newer = state.beginReceive();
  assert.notEqual(state.receive(view("最新", revision("3")), newer), false);
  assert.equal(state.receive(view("先生", revision("1")), older), false);
  assert.equal(state.material.terms[0].source, "最新");
  assert.equal(state.remoteRevision, revision("3"));
  const first = state.beginReceive();
  const second = state.beginReceive();
  assert.notEqual(state.receive(view("最新", revision("3")), first), false);
  assert.notEqual(state.receive(view("更新", revision("4")), second), false);
  assert.equal(state.remoteRevision, revision("4"));
});

test("tickets from another workspace, generation or model are rejected", () => {
  const state = model();
  state.receive(view());
  const fromA = state.beginReceive();
  state.receive(view("先生", revision("1"), "b".repeat(64)));
  assert.equal(state.receive(view(), fromA), false);
  assert.equal(state.workspaceId, "b".repeat(64));
  const beforeDiscard = state.beginReceive();
  state.discardLocalConfirmed();
  assert.equal(state.receive(view("先生", revision("1"), "b".repeat(64)), beforeDiscard), false);
  const other = model();
  assert.equal(state.receive(view("先生", revision("1"), "b".repeat(64)), other.beginReceive()), false);
  assert.equal(state.receive(view("先生", revision("1"), "b".repeat(64)),
    { ...state.beginReceive() }), false);
  assert.equal(state.workspaceId, "b".repeat(64));
});

test("clearing an optional field to null reconciles with a GET that omits it", () => {
  let refreshes = 0;
  const state = model({ refresh: () => refreshes++ });
  state.receive(view());
  assert.equal(state.material.terms[0].note, "备注");
  state.edit(["terms", 0, "note"], null);
  state.edit(["persons", 0, "reading"], null);
  state.edit(["persons", 0, "voice", "examples"], null);
  const request = state.prepareSave();
  state.edit(["terms", 0, "source"], "提交后");
  const omitted = view("先生", revision("3"));
  delete omitted.draft.material.terms[0].note;
  delete omitted.draft.material.persons[0].reading;
  delete omitted.draft.material.persons[0].voice.examples;
  for (let poll = 0; poll < 2; poll++) {
    state.receive(omitted);
    assert.equal(state.material.terms[0].source, "提交后");
    assert.equal(state.remoteRevision, revision("3"));
    assert.equal(state.conflict, false);
    assert.equal(state.dirty, true);
    assert.equal(state.pendingSave, null);
  }
  assert.equal(state.acceptSave(receipt(revision("3")), request), true);
  assert.equal(state.needsRefresh, false);
  assert.equal(refreshes, 0);
  const next = state.prepareSave();
  assert.equal(next.expected_revision, revision("3"));
  assert.equal(next.material.terms[0].source, "提交后");
  state.edit(["terms", 0, "source"], "先生");
  assert.equal(state.dirty, false);
});

test("nulls in required fields, nested required paragraphs and arrays stay distinct", () => {
  for (const [path, value] of [
    [["terms", 0, "allowed"], ["先生", "老师", null]],
    [["terms", 0, "evidence", 0, "paragraph"], null],
    [["persons", 0, "voice", "description"], null],
  ]) {
    const state = model();
    state.receive(view());
    state.edit(path, value);
    state.prepareSave();
    assert.equal(state.dirty, true);
    state.receive(view("先生", revision("2")));
    assert.equal(state.conflict, true);
  }
  const position = model();
  position.receive(view());
  position.edit(["terms", 0, "available_from"], { chapter: "chapter1", paragraph: null });
  position.prepareSave();
  const remote = view("先生", revision("2"));
  remote.draft.material.terms[0].available_from = { chapter: "chapter1" };
  position.receive(remote);
  assert.equal(position.conflict, false);
  assert.equal(position.dirty, false);
});

test("range scope endpoints with null paragraphs reconcile with a GET that omits them", () => {
  for (const location of ["term", "rule"]) {
    let refreshes = 0;
    const state = model({ refresh: () => refreshes++ });
    state.receive(view());
    const range = { kind: "range", from: { chapter: "chapter1", paragraph: null },
      to: { chapter: "chapter1", paragraph: null } };
    const path = location === "term" ? ["terms", 0, "scope"]
      : ["persons", 0, "address_rules", 0, "scope"];
    state.edit(path, range);
    const request = state.prepareSave();
    state.edit(["terms", 0, "source"], "提交后");
    const omitted = view("先生", revision("3"));
    const target = location === "term" ? omitted.draft.material.terms[0]
      : omitted.draft.material.persons[0].address_rules[0];
    target.scope = { kind: "range", from: { chapter: "chapter1" }, to: { chapter: "chapter1" } };
    for (let poll = 0; poll < 2; poll++) {
      state.receive(omitted);
      assert.equal(state.conflict, false, location);
      assert.equal(state.pendingSave, null, location);
      assert.equal(state.dirty, true, location);
      assert.equal(state.material.terms[0].source, "提交后", location);
      assert.equal(state.remoteRevision, revision("3"), location);
    }
    assert.equal(state.acceptSave(receipt(revision("3")), request), true, location);
    assert.equal(refreshes, 0, location);
    const next = state.prepareSave();
    assert.equal(next.expected_revision, revision("3"), location);
    assert.equal(next.material.terms[0].source, "提交后", location);
  }
});

test("a clean GET at a new baseline clears a superseded pending request and its mirror", () => {
  const session = storage();
  const state = model({ storage: session });
  state.receive(view());
  state.prepareSave();
  assert.ok(session.values.has(state.mirrorKey));
  state.receive(view("他处修改", revision("3")));
  assert.equal(state.material.terms[0].source, "他处修改");
  assert.equal(state.dirty, false);
  assert.equal(state.pendingSave, null);
  assert.equal(session.values.has(state.mirrorKey), false);
  const next = model({ storage: session });
  next.receive(view("他处修改", revision("3")));
  assert.equal(next.recoveryChoice, null);
  assert.equal(next.restore(), false);
  assert.equal(next.conflict, false);
  assert.equal(next.material.terms[0].source, "他处修改");
});

test("a clean GET at the same baseline keeps a still-valid pending retry", () => {
  const session = storage();
  const state = model({ storage: session });
  state.receive(view());
  const request = state.prepareSave();
  state.receive(view());
  assert.deepEqual(state.pendingSave, request);
  assert.deepEqual(state.prepareSave(), request);
  assert.equal(JSON.parse(session.values.get(state.mirrorKey)).pendingSave.request_id,
    request.request_id);
});

test("a clean GET does not overwrite an unresolved recovery mirror", () => {
  const session = storage();
  const original = model({ storage: session });
  original.receive(view());
  original.edit(["terms", 0, "source"], "待恢复");
  const savedMirror = session.values.get(original.mirrorKey);
  const next = model({ storage: session });
  next.receive(view());
  next.receive(view("他处修改", revision("3")));
  assert.equal(session.values.get(next.mirrorKey), savedMirror);
  assert.equal(next.restore(), true);
  assert.equal(next.material.terms[0].source, "待恢复");
});

test("reverting to a rebased baseline lets a later clean GET adopt server state", () => {
  const session = storage();
  const state = model({ storage: session });
  state.receive(view());
  state.edit(["terms", 0, "source"], "X");
  state.prepareSave();
  state.edit(["terms", 0, "source"], "Y");
  state.receive(view("X", revision("2")));
  state.edit(["terms", 0, "source"], "X");
  assert.equal(state.dirty, false);
  state.receive(view("Z", revision("3")));
  assert.equal(state.material.terms[0].source, "Z");
  assert.equal(state.conflict, false);
  assert.equal(state.pendingSave, null);
  assert.equal(session.values.has(state.mirrorKey), false);
});

test("a duplicate GET preserves the result epoch, validation and preview", () => {
  for (const dirty of [false, true]) {
    const state = model();
    state.receive(view());
    if (dirty) state.edit(["terms", 0, "source"], "本地");
    const validation = { valid: true, issues: [] };
    const preview = { report: "current" };
    state.validation = validation;
    state.preview = preview;
    const epoch = state.localEpoch;
    state.receive(view());
    state.receive(view(), state.beginReceive());
    assert.equal(state.localEpoch, epoch, `dirty=${dirty}`);
    assert.equal(state.validation, validation, `dirty=${dirty}`);
    assert.equal(state.preview, preview, `dirty=${dirty}`);
    assert.equal(state.dirty, dirty);
  }
});

test("a relevant GET change advances the epoch and invalidates results", () => {
  const variants = {
    revision: () => view("先生", revision("2")),
    material: () => view("他处修改", revision("1")),
    project_hash: () => ({ ...view(), project_hash: revision("e") }),
    workspace: () => view("先生", revision("1"), "b".repeat(64)),
  };
  for (const [name, next] of Object.entries(variants)) {
    for (const dirty of [false, true]) {
      const state = model();
      state.receive(view());
      if (dirty) state.edit(["terms", 0, "source"], "本地");
      state.validation = { valid: true, issues: [] };
      state.preview = { report: "stale" };
      const epoch = state.localEpoch;
      state.receive(next());
      assert.ok(state.localEpoch > epoch, `${name} dirty=${dirty}`);
      assert.equal(state.validation, null, `${name} dirty=${dirty}`);
      assert.equal(state.preview, null, `${name} dirty=${dirty}`);
    }
  }
});
