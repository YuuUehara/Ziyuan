import test from "node:test";
import assert from "node:assert/strict";
import { decodeImportedText, decodeWindows1252Strict, hasIso2022JpEscape } from "./import-decoder.mjs";

test("decodes UTF-8 and UTF-8 BOM cleanly without stripping BOM bytes", () => {
  const plain = new TextEncoder().encode("第一章 紫垣\n");
  assert.deepEqual(decodeImportedText(plain, "ch1.txt"), { text: "第一章 紫垣\n", encoding: "UTF-8" });
  const bom = new Uint8Array([0xef, 0xbb, 0xbf, ...plain]);
  const res = decodeImportedText(bom, "ch1.txt");
  assert.equal(res.encoding, "UTF-8");
  assert.ok(res.text.startsWith("\ufeff第一章"));
});

test("rejects UTF-16 BOM and disallowed control bytes", () => {
  assert.throws(() => decodeImportedText(new Uint8Array([0xff, 0xfe, 0x41, 0x00]), "u16.txt"), /UTF-8/);
  assert.throws(() => decodeImportedText(new Uint8Array([0xfe, 0xff, 0x00, 0x41]), "u16be.txt"), /UTF-8/);
  assert.throws(() => decodeImportedText(new Uint8Array([0x82, 0xa0, 0x00]), "nul.txt", "ja"), /UTF-8/);
});

test("decodes Japanese Shift_JIS, EUC-JP, and ISO-2022-JP", () => {
  // Shift_JIS "あいう" = 0x82A0 0x82A2 0x82A4
  const sjis = new Uint8Array([0x82, 0xa0, 0x82, 0xa2, 0x82, 0xa4]);
  assert.deepEqual(decodeImportedText(sjis, "sjis.txt", "ja"), { text: "あいう", encoding: "Shift_JIS" });

  // EUC-JP "あいう" = 0xA4A2 0xA4A4 0xA4A6
  const euc = new Uint8Array([0xa4, 0xa2, 0xa4, 0xa4, 0xa4, 0xa6]);
  assert.deepEqual(decodeImportedText(euc, "euc.txt", "ja"), { text: "あいう", encoding: "EUC-JP" });

  // ISO-2022-JP ESC $ B + 0x24 0x22 ("あ") + ESC ( B
  const iso = new Uint8Array([0x1b, 0x24, 0x42, 0x24, 0x22, 0x1b, 0x28, 0x42]);
  assert.equal(hasIso2022JpEscape(iso), true);
  assert.deepEqual(decodeImportedText(iso, "iso.txt", "ja"), { text: "あ", encoding: "ISO-2022-JP" });
});

test("decodes Korean EUC-KR and Western Windows-1252 and rejects undefined C1 bytes", () => {
  // EUC-KR "안녕" = 0xBEC8 0xB3E7
  const kr = new Uint8Array([0xbe, 0xc8, 0xb3, 0xe7]);
  assert.deepEqual(decodeImportedText(kr, "kr.txt", "ko"), { text: "안녕", encoding: "EUC-KR" });

  // Windows-1252 "Café — déjà" = C a f 0xE9 ' ' 0x97 ' ' d 0xE9 j 0xE0
  const fr = new Uint8Array([0x43, 0x61, 0x66, 0xe9, 0x20, 0x97, 0x20, 0x64, 0xe9, 0x6a, 0xe0]);
  assert.deepEqual(decodeImportedText(fr, "fr.txt", "fr"), { text: "Café — déjà", encoding: "Windows-1252" });

  // Undefined Windows-1252 byte 0x81 must fail closed
  const badC1 = new Uint8Array([0x43, 0x61, 0x66, 0x81, 0x20, 0x74, 0x65, 0x73, 0x74]);
  assert.equal(decodeWindows1252Strict(badC1), null);
  assert.throws(() => decodeImportedText(badC1, "bad.txt", "de"), /UTF-8/);
});
