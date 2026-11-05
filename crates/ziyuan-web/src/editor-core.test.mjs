// 人工译稿标记逻辑的单测：node --test crates/ziyuan-web/src/editor-core.test.mjs
// 口径核对：CJK 一字 1 码元 / 3 字节；emoji 2 码元 / 4 字节；
// e + U+0301 共 2 码元 / 3 字节，且同属一个字素。
import assert from "node:assert/strict";
import test from "node:test";
import {
  KIND_LABELS,
  addMark,
  clampGrapheme,
  graphemeBounds,
  graphemes,
  marksAt,
  paragraphFromWire,
  paragraphToWire,
  removeMark,
  runs,
  sameParagraph,
  spliceMarks,
  u16FromUtf8,
  u16ToUtf8,
  utf8Bytes,
  validateMarks,
} from "./editor-core.mjs";

const RANGE = "译文标记须为同一行内完整字素的非空范围";
const OVERLAP = "译文标记只支持不相交或同范围的不同种类，不支持重复、嵌套或交叉";

test("字节口径：CJK、emoji、组合符", () => {
  assert.equal(utf8Bytes("甲月"), 6);
  assert.equal(utf8Bytes("😀"), 4);
  assert.equal(utf8Bytes("e\u0301"), 3);
});

test("字素边界含组合符与 emoji", () => {
  assert.deepEqual(graphemeBounds(""), [0]);
  assert.deepEqual(graphemeBounds("甲月"), [0, 1, 2]);
  assert.deepEqual(graphemeBounds("e\u0301x"), [0, 2, 3]);
  assert.deepEqual(graphemeBounds("😀a"), [0, 2, 3]);
  // ZWJ 连字整体是一个字素。
  assert.deepEqual(graphemeBounds("👨‍👩‍👧a"), [0, 8, 9]);
});

test("字素切分与 unicode-segmentation 同一口径", () => {
  // 与 crates/ziyuan-app/src/translation/markup.rs 的
  // grapheme_boundaries_agree_with_web_editor 共用同一张表：
  // 前端用 ICU（Intl.Segmenter），服务端用 unicode-segmentation，两边必须一致。
  assert.deepEqual(graphemes("甲月"), ["甲", "月"]);
  assert.deepEqual(graphemes("e\u0301x"), ["e\u0301", "x"]);
  assert.deepEqual(graphemes("😀a"), ["😀", "a"]);
  assert.deepEqual(graphemes("👨‍👩‍👧a"), ["👨‍👩‍👧", "a"]);
  assert.deepEqual(graphemes(""), []);
});

test("码元到字节：非字素边界返回 null", () => {
  assert.equal(u16ToUtf8("甲月舟乙", 0), 0);
  assert.equal(u16ToUtf8("甲月舟乙", 2), 6);
  assert.equal(u16ToUtf8("甲月舟乙", 4), 12);
  assert.equal(u16ToUtf8("甲月舟乙", 5), null);
  assert.equal(u16ToUtf8("e\u0301x", 1), null);
  assert.equal(u16ToUtf8("e\u0301x", 2), 3);
  assert.equal(u16ToUtf8("😀a", 1), null);
  assert.equal(u16ToUtf8("😀a", 2), 4);
  assert.equal(u16ToUtf8("😀a", 3), 5);
});

test("字节回到码元：落在中间返回 null", () => {
  assert.equal(u16FromUtf8("甲月舟乙", 0), 0);
  assert.equal(u16FromUtf8("甲月舟乙", 6), 2);
  assert.equal(u16FromUtf8("甲月舟乙", 7), null);
  assert.equal(u16FromUtf8("e\u0301x", 3), 2);
  assert.equal(u16FromUtf8("e\u0301x", 1), null);
  assert.equal(u16FromUtf8("😀a", 5), 3);
  for (const text of ["甲月舟乙", "e\u0301x", "😀a", "👨‍👩‍👧a"]) {
    for (let i = 0; i <= text.length; i++) {
      const bytes = u16ToUtf8(text, i);
      if (bytes === null) continue;
      assert.equal(u16FromUtf8(text, bytes), i, `${JSON.stringify(text)} @${i}`);
    }
  }
});

test("选区端点吸附到字素边界", () => {
  assert.equal(clampGrapheme("e\u0301x", 1, "start"), 0);
  assert.equal(clampGrapheme("e\u0301x", 1, "end"), 2);
  assert.equal(clampGrapheme("e\u0301x", 2, "start"), 2);
  assert.equal(clampGrapheme("😀a", 1, "start"), 0);
  assert.equal(clampGrapheme("😀a", 1, "end"), 2);
  assert.equal(clampGrapheme("甲月", 0, "start"), 0);
  assert.equal(clampGrapheme("甲月", 99, "start"), 2);
  // 本身已在边界上时两个方向都不动。
  assert.equal(clampGrapheme("e\u0301x", 2, "start"), 2);
  assert.equal(clampGrapheme("e\u0301x", 2, "end"), 2);
  assert.equal(clampGrapheme("甲月", 1, "end"), 1);
  assert.equal(clampGrapheme("甲月", 1, "start"), 1);
});

test("加标记：同范围不同种类可叠，重复与交叠拒绝", () => {
  const text = "甲月舟乙";
  let marks = addMark([], 1, 3, "ruby", "げつ");
  assert.deepEqual(marks, [{ start: 1, end: 3, kind: "ruby", reading: "げつ" }]);
  marks = addMark(marks, 1, 3, "emphasis_dots");
  assert.equal(marks.length, 2);
  assert.throws(() => addMark(marks, 1, 3, "ruby", "x"), /该范围已有同种标记/);
  assert.throws(() => addMark(marks, 0, 2, "strong"), new RegExp(OVERLAP));
  assert.throws(() => addMark(marks, 2, 4, "strong"), new RegExp(OVERLAP));
  // 嵌套也是交叠。
  marks = addMark([], 0, 4, "emphasis");
  assert.throws(() => addMark(marks, 1, 2, "strong"), new RegExp(OVERLAP));
  // 相邻不算交叠。
  assert.equal(addMark([{ start: 0, end: 2, kind: "strong" }], 2, 4, "emphasis").length, 2);
  assert.equal(removeMark(marks, 0).length, 0);
  assert.match(KIND_LABELS.ruby, /注音/);
});

test("加标记的入参守卫", () => {
  assert.throws(() => addMark([], 2, 2, "strong"), new RegExp(RANGE));
  assert.throws(() => addMark([], 3, 1, "strong"), new RegExp(RANGE));
  assert.throws(() => addMark([], 0, 1, "unknown"), /未知的标记种类/);
  assert.throws(() => addMark([], 0, 1, "ruby"), /注音须带读音/);
  assert.throws(() => addMark([], 0, 1, "ruby", "a\nb"), /注音读音不得含换行/);
});

test("本地预检报告畸形标记，不在排序时抛错", () => {
  for (const mark of [null, undefined, "bad", {}, { start: 0, end: 1, kind: "unknown" }]) {
    const problems = validateMarks("甲乙", [mark, { start: 1, end: 2, kind: "strong" }]);
    assert.ok(problems.some(p => p.index === 0));
  }
});

test("本地预检覆盖服务端全部规则", () => {
  const text = "e\u0301甲";
  // 组合符中间不是字素边界。
  assert.equal(validateMarks(text, [{ start: 1, end: 3, kind: "strong" }])[0].message, RANGE);
  // 空范围、越界、非整数。
  assert.equal(validateMarks("甲", [{ start: 0, end: 0, kind: "strong" }])[0].message, RANGE);
  assert.equal(validateMarks("甲", [{ start: 0, end: 2, kind: "strong" }])[0].message, RANGE);
  assert.equal(validateMarks("甲", [{ start: 0.5, end: 1, kind: "strong" }])[0].message, RANGE);
  // 跨行标记。
  const lines = "甲\n乙";
  assert.deepEqual(validateMarks(lines, [{ start: 0, end: 1, kind: "strong" }]), []);
  assert.equal(validateMarks(lines, [{ start: 0, end: 3, kind: "strong" }])[0].message, RANGE);
  // 读音含换行。
  assert.equal(validateMarks("甲", [{ start: 0, end: 1, kind: "ruby", reading: "a\nb" }])[0].message, "注音读音不得含换行");
  // 同范围不同种类合法；交叠非法。
  assert.deepEqual(
    validateMarks("甲月", [
      { start: 0, end: 2, kind: "ruby", reading: "げつ" },
      { start: 0, end: 2, kind: "emphasis_dots" },
      { start: 0, end: 2, kind: "strong" },
    ]),
    [],
  );
  assert.equal(
    validateMarks("甲月", [{ start: 0, end: 2, kind: "emphasis" }, { start: 1, end: 2, kind: "strong" }])[0].message,
    OVERLAP,
  );
  // 未知种类。
  assert.match(validateMarks("甲", [{ start: 0, end: 1, kind: "link" }])[0].message, /未知的标记种类/);
});

test("本地预检的两条总量上限", () => {
  const many = "甲".repeat(4097);
  const marks = Array.from({ length: 4097 }, (_, i) => ({ start: i, end: i + 1, kind: "strong" }));
  assert.match(validateMarks(many, marks).map((p) => p.message).join(), /超过 4096 项/);
  const long = "甲".repeat(90000);
  assert.match(validateMarks(long, []).map((p) => p.message).join(), /超过 256 KiB/);
  // 读音按字节计入：正文 261000 字节，剩 1144 字节；读音 400 字 = 1200 字节即超。
  const near = "甲".repeat(87000);
  const ruby = (length) => [{ start: 0, end: 1, kind: "ruby", reading: "あ".repeat(length) }];
  assert.equal(validateMarks(near, ruby(300)).length, 0);
  assert.match(validateMarks(near, ruby(400)).map((p) => p.message).join(), /超过 256 KiB/);
});

test("文本改动后标记跟着原来的字", () => {
  const ruby = { start: 0, end: 3, kind: "ruby", reading: "x" };
  // 起点插入：整条后移，字不变。
  assert.deepEqual(spliceMarks([ruby], 0, 0, 1), [{ start: 1, end: 4, kind: "ruby", reading: "x" }]);
  // 终点插入：不动。
  assert.deepEqual(spliceMarks([ruby], 3, 3, 1), [ruby]);
  // 内部插入：扩张。
  assert.deepEqual(spliceMarks([ruby], 1, 1, 1), [{ start: 0, end: 4, kind: "ruby", reading: "x" }]);
  // 整条删除。
  assert.deepEqual(spliceMarks([ruby], 0, 3, 0), []);
  // 内部删除。
  assert.deepEqual(spliceMarks([ruby], 1, 2, 0), [{ start: 0, end: 2, kind: "ruby", reading: "x" }]);
  // 删掉开头。
  assert.deepEqual(spliceMarks([ruby], 0, 1, 0), [{ start: 0, end: 2, kind: "ruby", reading: "x" }]);
  // 删掉结尾。
  assert.deepEqual(spliceMarks([ruby], 2, 3, 0), [{ start: 0, end: 2, kind: "ruby", reading: "x" }]);
  // 等长替换覆盖开头。
  assert.deepEqual(spliceMarks([ruby], 0, 2, 2), [{ start: 2, end: 3, kind: "ruby", reading: "x" }]);
  // 标记之后改动不动它。
  assert.deepEqual(spliceMarks([ruby], 3, 4, 6), [ruby]);
  // 标记之前改动整条平移。
  assert.deepEqual(spliceMarks([{ start: 5, end: 8, kind: "strong" }], 0, 2, 0), [{ start: 3, end: 6, kind: "strong" }]);
  // 两条标记，改动横跨两者。
  const both = [
    { start: 0, end: 2, kind: "strong" },
    { start: 3, end: 5, kind: "emphasis" },
  ];
  // 删掉两条标记之间的字：前一条不动，后一条左移。
  assert.deepEqual(spliceMarks(both, 2, 3, 0), [
    { start: 0, end: 2, kind: "strong" },
    { start: 2, end: 4, kind: "emphasis" },
  ]);
  assert.deepEqual(spliceMarks(both, 1, 4, 0), [
    { start: 0, end: 1, kind: "strong" },
    { start: 1, end: 2, kind: "emphasis" },
  ]);
});

test("落盘往返：注音与着重同范围叠加", () => {
  const paragraph = {
    id: "c0001-p0001",
    text: "甲月舟乙",
    marks: [
      { start: 1, end: 3, kind: "ruby", reading: "げつ" },
      { start: 1, end: 3, kind: "emphasis_dots" },
    ],
  };
  const wire = paragraphToWire(paragraph);
  assert.deepEqual(wire, {
    id: "c0001-p0001",
    text: "甲月舟乙",
    marks: [
      { start: 3, end: 9, kind: { type: "ruby", reading: "げつ" } },
      { start: 3, end: 9, kind: { type: "emphasis_dots" } },
    ],
  });
  assert.deepEqual(paragraphFromWire(wire), paragraph);
  // 落盘前必须落在字素边界。
  assert.throws(
    () => paragraphToWire({ id: "x", text: "e\u0301甲", marks: [{ start: 1, end: 3, kind: "strong" }] }),
    new RegExp(RANGE),
  );
  // 服务端给的偏移不合规时明确报错，不静默改数据。
  assert.throws(
    () => paragraphFromWire({ id: "x", text: "e\u0301甲", marks: [{ start: 1, end: 3, kind: { type: "strong" } }] }),
    new RegExp(RANGE),
  );
  assert.throws(
    () => paragraphFromWire({ id: "x", text: "甲", marks: [{ start: 0, end: 3, kind: { type: "link" } }] }),
    /未知的标记种类/,
  );
});

test("渲染片段：按端点切分并合并同范围标记", () => {
  const paragraph = {
    text: "甲月舟乙",
    marks: [
      { start: 1, end: 3, kind: "ruby", reading: "げつ" },
      { start: 1, end: 3, kind: "emphasis_dots" },
      { start: 3, end: 4, kind: "strong" },
    ],
  };
  assert.deepEqual(runs(paragraph).map((r) => [r.text, r.ruby, r.emphasis_dots, r.strong]), [
    ["甲", null, false, false],
    ["月舟", "げつ", true, false],
    ["乙", null, false, true],
  ]);
  assert.deepEqual(runs({ text: "", marks: [] }), []);
  assert.equal(marksAt(paragraph.marks, 1).length, 2);
  assert.equal(marksAt(paragraph.marks, 0).length, 0);
});

test("未保存判定按内容而非引用", () => {
  const base = { id: "p", text: "甲", marks: [{ start: 0, end: 1, kind: "strong" }] };
  assert.equal(sameParagraph(base, { ...base, marks: [...base.marks] }), true);
  assert.equal(sameParagraph(base, { ...base, text: "乙" }), false);
  assert.equal(sameParagraph(base, { ...base, marks: [] }), false);
  assert.equal(sameParagraph(base, { ...base, marks: [{ start: 0, end: 1, kind: "emphasis" }] }), false);
  assert.equal(sameParagraph(null, base), false);
});
