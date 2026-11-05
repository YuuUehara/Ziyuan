// 人工译稿标记编辑器的纯逻辑：不碰 DOM，浏览器与 Node 共用。
// 口径：界面里一律用 UTF-16 码元偏移（选区、textarea），落盘前转 UTF-8 字节偏移。
// 服务端 crates/ziyuan-app/src/translation/markup.rs 会按同一规则复核。

const encoder = new TextEncoder();
const SEGMENTER =
  typeof Intl !== "undefined" && typeof Intl.Segmenter === "function"
    ? new Intl.Segmenter("zh", { granularity: "grapheme" })
    : null;

/** 标记种类的展示名。 */
export const KIND_LABELS = {
  ruby: "注音",
  emphasis_dots: "着重号",
  strong: "粗体",
  emphasis: "斜体",
};

/** 服务端排序用的种类次序，用来复现同一套配对检查。 */
const KIND_ORDER = { ruby: 0, emphasis_dots: 1, strong: 2, emphasis: 3 };

const RANGE_MESSAGE = "译文标记须为同一行内完整字素的非空范围";
const OVERLAP_MESSAGE = "译文标记只支持不相交或同范围的不同种类，不支持重复、嵌套或交叉";

/** UTF-8 字节数（不依赖 DOM）。 */
export function utf8Bytes(text) {
  return encoder.encode(String(text)).length;
}

/** 每个字素的 UTF-16 起点，末尾补上总长。空串返回 [0]。 */
export function graphemeBounds(text) {
  const value = String(text);
  if (!value.length) return [0];
  const out = [];
  if (SEGMENTER) {
    for (const part of SEGMENTER.segment(value)) out.push(part.index);
  } else {
    // 没有 Intl.Segmenter 时退到按码点切分：组合符会被当成独立字素，
    // 此时前端只允许在码点边界加标记，服务端仍会拒绝不合规的范围。
    let i = 0;
    for (const ch of value) {
      out.push(i);
      i += ch.length;
    }
  }
  if (out[out.length - 1] !== value.length) out.push(value.length);
  return out;
}

/** 字素序列（字数统计与边界检查共用）。 */
export function graphemes(text) {
  const value = String(text);
  if (!value.length) return [];
  if (SEGMENTER) return [...SEGMENTER.segment(value)].map((part) => part.segment);
  return [...value];
}

/** UTF-16 码元偏移 → UTF-8 字节偏移；不在字素边界返回 null。 */
export function u16ToUtf8(text, index) {
  const value = String(text);
  if (!Number.isInteger(index) || index < 0 || index > value.length) return null;
  if (!graphemeBounds(value).includes(index)) return null;
  return utf8Bytes(value.slice(0, index));
}

/** UTF-8 字节偏移 → UTF-16 码元偏移；不是字素边界返回 null。 */
export function u16FromUtf8(text, bytes) {
  const value = String(text);
  if (!Number.isInteger(bytes) || bytes < 0) return null;
  if (bytes === 0) return 0;
  let seen = 0;
  let previous = 0;
  for (const bound of graphemeBounds(value)) {
    if (bound === 0) continue;
    seen += utf8Bytes(value.slice(previous, bound));
    if (seen === bytes) return bound;
    if (seen > bytes) return null;
    previous = bound;
  }
  return null;
}

/** 把任意偏移吸附到字素边界：dir 为 "start"（向前）或 "end"（向后）。 */
export function clampGrapheme(text, index, dir = "start") {
  const value = String(text);
  const bounds = graphemeBounds(value);
  const target = Math.max(0, Math.min(value.length, Number(index) || 0));
  if (bounds.includes(target)) return target;
  if (dir === "end") return bounds.find((b) => b >= target) ?? value.length;
  let low = 0;
  for (const b of bounds) if (b <= target) low = b;
  return low;
}

function isKind(kind) {
  return Object.prototype.hasOwnProperty.call(KIND_ORDER, kind);
}

function kindOrder(kind) {
  return KIND_ORDER[kind] ?? 99;
}

/** 单条标记的范围问题；无问题返回 null。 */
function rangeProblem(text, bounds, mark) {
  if (!mark || typeof mark !== "object") return RANGE_MESSAGE;
  if (!isKind(mark.kind)) return `未知的标记种类：${String(mark.kind)}`;
  if (!Number.isInteger(mark.start) || !Number.isInteger(mark.end)) return RANGE_MESSAGE;
  if (mark.start < 0 || mark.end > text.length || mark.start >= mark.end) return RANGE_MESSAGE;
  if (!bounds.has(mark.start) || !bounds.has(mark.end)) return RANGE_MESSAGE;
  if (/[\n\r]/.test(text.slice(mark.start, mark.end))) return RANGE_MESSAGE;
  return null;
}

/**
 * 与服务端同规则的本地预检：返回 [{index, message}]，index 为 -1 表示整稿问题。
 * 前端据此禁用保存，服务端仍会复核。
 */
export function validateMarks(text, marks) {
  const value = String(text);
  if (!Array.isArray(marks)) return [{ index: -1, message: "标记须为数组" }];
  const problems = [];
  if (marks.length > 4096) {
    problems.push({ index: -1, message: `人工草稿标记超过 4096 项（现 ${marks.length} 项）` });
  }
  const bounds = new Set(graphemeBounds(value));
  let bytes = utf8Bytes(value);
  marks.forEach((mark, index) => {
    const problem = rangeProblem(value, bounds, mark);
    if (problem) problems.push({ index, message: problem });
    if (mark && mark.kind === "ruby") {
      const reading = typeof mark.reading === "string" ? mark.reading : "";
      bytes += utf8Bytes(reading);
      if (/[\n\r]/.test(reading)) problems.push({ index, message: "注音读音不得含换行" });
    }
  });
  if (bytes > 256 * 1024) {
    problems.push({ index: -1, message: `人工草稿正文与读音合计超过 256 KiB（现 ${bytes} 字节）` });
  }
  const sorted = marks
    .map((mark, index) => ({ mark, index }))
    .filter(({ mark }) => !rangeProblem(value, bounds, mark))
    .sort((a, b) =>
      a.mark.start - b.mark.start ||
      a.mark.end - b.mark.end ||
      kindOrder(a.mark.kind) - kindOrder(b.mark.kind));
  for (let i = 1; i < sorted.length; i++) {
    const a = sorted[i - 1].mark;
    const b = sorted[i].mark;
    const sameRange = a.start === b.start && a.end === b.end;
    if (b.start < a.end && !(sameRange && a.kind !== b.kind)) {
      problems.push({ index: sorted[i].index, message: OVERLAP_MESSAGE });
    }
  }
  return problems;
}

/**
 * 文本被 [from, to) 换成 insertedLength 个码元后，平移、裁剪或删除标记。
 * 标记始终跟着原来的字：恰在标记起点插入会把标记整体后移（不扩张），
 * 恰在终点插入不动，标记内部插入才扩张；整条被删则去掉标记。
 */
export function spliceMarks(marks, from, to, insertedLength) {
  const delta = insertedLength - (to - from);
  const out = [];
  for (const mark of marks) {
    const next = spliceOne(mark, from, to, insertedLength, delta);
    if (next) out.push(next);
  }
  return out;
}

function spliceOne(mark, from, to, insertedLength, delta) {
  // 全在标记之前：整条平移。
  if (to <= mark.start) return { ...mark, start: mark.start + delta, end: mark.end + delta };
  // 全在标记之后：不动。
  if (from >= mark.end) return mark;
  // 标记被整个盖住。
  if (from <= mark.start && to >= mark.end) return null;
  // 删掉了标记的开头：剩下的尾巴接着插入的文字之后。
  if (from <= mark.start) {
    const start = from + insertedLength;
    const end = mark.end + delta;
    return end > start ? { ...mark, start, end } : null;
  }
  // 删掉了标记的结尾。
  if (to >= mark.end) return { ...mark, end: from };
  // 改在标记内部。
  const end = mark.end + delta;
  return end > mark.start ? { ...mark, end } : null;
}

/** 加一条标记。重复、嵌套、交叉一律抛错，文案与服务端一致。 */
export function addMark(marks, start, end, kind, reading) {
  if (!isKind(kind)) throw new Error(`未知的标记种类：${String(kind)}`);
  if (!Number.isInteger(start) || !Number.isInteger(end) || start >= end) {
    throw new Error(RANGE_MESSAGE);
  }
  if (kind === "ruby") {
    if (typeof reading !== "string") throw new Error("注音须带读音");
    if (/[\n\r]/.test(reading)) throw new Error("注音读音不得含换行");
  }
  for (const mark of marks) {
    const sameRange = mark.start === start && mark.end === end;
    if (sameRange && mark.kind === kind) throw new Error("该范围已有同种标记");
    const overlaps = start < mark.end && mark.start < end;
    if (overlaps && !sameRange) throw new Error(OVERLAP_MESSAGE);
  }
  const next = { start, end, kind };
  if (kind === "ruby") next.reading = reading;
  return [...marks, next];
}

/** 去掉第 index 条标记。 */
export function removeMark(marks, index) {
  return marks.filter((_, i) => i !== index);
}

/** 覆盖某个码元位置的标记。 */
export function marksAt(marks, position) {
  return marks.filter((mark) => mark.start <= position && position < mark.end);
}

/**
 * 渲染用的行内片段：按所有标记端点切分，每段带上活动标记。
 * ruby 为 null 或读音字符串；其余三种为布尔。
 */
export function runs(paragraph) {
  const text = String(paragraph?.text ?? "");
  const marks = Array.isArray(paragraph?.marks) ? paragraph.marks : [];
  const points = new Set([0, text.length]);
  for (const mark of marks) {
    if (mark.start > 0 && mark.start < text.length) points.add(mark.start);
    if (mark.end > 0 && mark.end < text.length) points.add(mark.end);
  }
  const sorted = [...points].sort((a, b) => a - b);
  const out = [];
  for (let i = 0; i + 1 < sorted.length; i++) {
    const start = sorted[i];
    const end = sorted[i + 1];
    const run = { start, end, text: text.slice(start, end), ruby: null, emphasis_dots: false, strong: false, emphasis: false };
    for (const mark of marks) {
      if (mark.start > start || mark.end < end) continue;
      if (mark.kind === "ruby") run.ruby = mark.reading ?? "";
      else run[mark.kind] = true;
    }
    out.push(run);
  }
  return out;
}

/** 界面模型（码元）→ 落盘模型（UTF-8 字节）。偏移不在字素边界时抛错。 */
export function paragraphToWire(paragraph) {
  const text = String(paragraph?.text ?? "");
  const marks = Array.isArray(paragraph?.marks) ? paragraph.marks : [];
  return {
    id: paragraph.id,
    text,
    marks: marks.map((mark) => {
      const start = u16ToUtf8(text, mark.start);
      const end = u16ToUtf8(text, mark.end);
      if (start === null || end === null) throw new Error(RANGE_MESSAGE);
      const kind = { type: mark.kind };
      if (mark.kind === "ruby") kind.reading = mark.reading ?? "";
      return { start, end, kind };
    }),
  };
}

/** 落盘模型（UTF-8 字节）→ 界面模型（码元）。 */
export function paragraphFromWire(paragraph) {
  const text = String(paragraph?.text ?? "");
  const wire = Array.isArray(paragraph?.marks) ? paragraph.marks : [];
  return {
    id: paragraph.id,
    text,
    marks: wire.map((mark) => {
      const start = u16FromUtf8(text, mark.start);
      const end = u16FromUtf8(text, mark.end);
      if (start === null || end === null) throw new Error(RANGE_MESSAGE);
      const kind = mark.kind?.type;
      if (!isKind(kind)) throw new Error(`未知的标记种类：${String(kind)}`);
      const next = { start, end, kind };
      if (kind === "ruby") next.reading = mark.kind.reading ?? "";
      return next;
    }),
  };
}

/** 整稿转换。 */
export function paragraphsToWire(paragraphs) {
  return paragraphs.map(paragraphToWire);
}

export function paragraphsFromWire(paragraphs) {
  return paragraphs.map(paragraphFromWire);
}

/** 段落是否有与初始不同之处（用于“未保存”判定）。 */
export function sameParagraph(a, b) {
  if (!a || !b) return false;
  if (String(a.text ?? "") !== String(b.text ?? "")) return false;
  const left = Array.isArray(a.marks) ? a.marks : [];
  const right = Array.isArray(b.marks) ? b.marks : [];
  if (left.length !== right.length) return false;
  const key = (mark) => `${mark.start}:${mark.end}:${mark.kind}:${mark.reading ?? ""}`;
  const l = left.map(key).sort();
  const r = right.map(key).sort();
  return l.every((value, i) => value === r[i]);
}
