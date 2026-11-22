// Quick term/person capture helpers for the translation draft editor.
// Shared between `/assets/quick-glossary.mjs` and `node --test`.

export const TERM_CATEGORIES = Object.freeze([
  { value: "common", label: "通用" },
  { value: "person_name", label: "人名" },
  { value: "place", label: "地名" },
  { value: "organization", label: "组织" },
  { value: "title", label: "官职 / 头衔" },
  { value: "culture", label: "文化专词" },
]);

export function extractSelectionText(doc = globalThis.document, activeEl = null) {
  const el = activeEl || doc?.activeElement || null;
  if (
    el &&
    typeof el.value === "string" &&
    typeof el.selectionStart === "number" &&
    typeof el.selectionEnd === "number" &&
    el.selectionEnd > el.selectionStart
  ) {
    return el.value.slice(el.selectionStart, el.selectionEnd).trim().slice(0, 120);
  }
  const sel = doc?.defaultView?.getSelection?.() || globalThis.getSelection?.();
  if (sel && typeof sel.toString === "function") {
    return sel.toString().trim().slice(0, 120);
  }
  return "";
}

export function validateQuickMaterialFields(kind, fields = {}) {
  if (kind === "term") {
    const source = String(fields.source || "").trim();
    const target = String(fields.target || "").trim();
    if (!source) return { ok: false, error: "请填写原文词条。" };
    if (!target) return { ok: false, error: "请填写译法。" };
    return { ok: true, error: "" };
  }
  if (kind === "person") {
    const name = String(fields.name || "").trim();
    if (!name) return { ok: false, error: "请填写人物姓名。" };
    return { ok: true, error: "" };
  }
  return { ok: false, error: "未知资料类型。" };
}

export function buildQuickMaterialPayload(kind, fields = {}) {
  if (kind === "term") {
    const payload = {
      kind: "term",
      source: String(fields.source || "").trim(),
      target: String(fields.target || "").trim(),
    };
    const category = String(fields.category || "").trim();
    if (category) payload.category = category;
    const note = String(fields.note || "").trim();
    if (note) payload.note = note;
    return payload;
  }
  if (kind === "person") {
    const payload = {
      kind: "person",
      name: String(fields.name || "").trim(),
    };
    const alias = String(fields.alias || "").trim();
    if (alias) payload.alias = alias;
    const voice = String(fields.voice || "").trim();
    if (voice) payload.voice = voice;
    return payload;
  }
  throw new Error("未知资料类型：" + String(kind));
}

export function formatQuickMaterialReceipt(receipt) {
  if (!receipt) return "已写入本地资料草稿。";
  const noun = receipt.kind === "person" ? "人物" : "术语";
  const verb = receipt.updated_existing ? "已合并到现有" : "已收录新";
  return `${verb}${noun}（${receipt.id}，草稿修订 ${receipt.revision}）。仅写入本地资料草稿，尚未批准生效。`;
}
