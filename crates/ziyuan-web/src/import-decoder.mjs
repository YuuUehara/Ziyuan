// Strict lossless multi-language text decoder for browser file imports.
// Shared between `/assets/import-decoder.mjs` and `node --test`.

export function hasIso2022JpEscape(bytes) {
  for (let i = 0; i + 2 < bytes.length; i++) {
    if (bytes[i] !== 0x1b || bytes[i + 1] !== 0x24) continue;
    if (bytes[i + 2] === 0x42 || bytes[i + 2] === 0x40) return true;
    if (i + 3 < bytes.length && bytes[i + 2] === 0x28 && bytes[i + 3] === 0x44) return true;
  }
  return false;
}

export function hasDisallowedControlByte(bytes) {
  for (let i = 0; i < bytes.length; i++) {
    const b = bytes[i];
    if ((b <= 0x08) || b === 0x0b || b === 0x0c || (b >= 0x0e && b <= 0x1f) || b === 0x7f) return true;
  }
  return false;
}

export function isUndefinedWindows1252(b) {
  return b === 0x81 || b === 0x8d || b === 0x8f || b === 0x90 || b === 0x9d;
}

export function isCommonWestern1252Letter(b) {
  return b === 0x8c || b === 0x9c || b === 0x9f
    || (b >= 0xc0 && b <= 0xcf) || (b >= 0xd1 && b <= 0xd6) || (b >= 0xd9 && b <= 0xdc)
    || (b >= 0xdf && b <= 0xef) || (b >= 0xf1 && b <= 0xf6) || (b >= 0xf9 && b <= 0xfc) || b === 0xff;
}

export function isCommonWestern1252Punct(b) {
  return b === 0x80 || b === 0x82 || b === 0x84 || b === 0x85 || b === 0x8b
    || (b >= 0x91 && b <= 0x97) || b === 0x9b || b === 0xa0 || b === 0xa3 || b === 0xa7
    || b === 0xa9 || b === 0xab || b === 0xad || b === 0xae || b === 0xb0 || b === 0xb2
    || b === 0xb3 || b === 0xb4 || b === 0xb7 || b === 0xba || b === 0xbb;
}

export function isPlausibleWindows1252(bytes) {
  if (!bytes.length || hasDisallowedControlByte(bytes)) return false;
  let nonAscii = 0, commonWestern = 0, hasAsciiAlnum = false;
  for (let i = 0; i < bytes.length; i++) {
    const b = bytes[i];
    if (isUndefinedWindows1252(b)) return false;
    if ((b >= 0x30 && b <= 0x39) || (b >= 0x41 && b <= 0x5a) || (b >= 0x61 && b <= 0x7a)) hasAsciiAlnum = true;
    else if (b >= 0x80) {
      nonAscii++;
      if (isCommonWestern1252Letter(b) || isCommonWestern1252Punct(b)) commonWestern++;
    }
  }
  return nonAscii > 0 && hasAsciiAlnum && nonAscii * 3 <= bytes.length * 2 && commonWestern * 4 >= nonAscii * 3;
}

const WIN1252_HIGH = [
  0x20ac, 0, 0x201a, 0x0192, 0x201e, 0x2026, 0x2020, 0x2021,
  0x02c6, 0x2030, 0x0160, 0x2039, 0x0152, 0, 0x017d, 0,
  0, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014,
  0x02dc, 0x2122, 0x0161, 0x203a, 0x0153, 0, 0x017e, 0x0178,
];

export function decodeWindows1252Strict(bytes) {
  if (!isPlausibleWindows1252(bytes)) return null;
  let out = "";
  for (let i = 0; i < bytes.length; i++) {
    const b = bytes[i];
    if (b >= 0x80 && b <= 0x9f) {
      const cp = WIN1252_HIGH[b - 0x80];
      if (!cp) return null;
      out += String.fromCharCode(cp);
    } else {
      out += String.fromCharCode(b);
    }
  }
  return out;
}

export function westernPlausibility(bytes) {
  let score = 0, prevHighNonPunct = false;
  for (let i = 0; i < bytes.length; i++) {
    const b = bytes[i];
    if (b < 0x80) {
      score += (b === 0x09 || b === 0x0a || b === 0x0d || (b >= 0x20 && b <= 0x7e)) ? 1 : -8;
      prevHighNonPunct = false;
    } else if (isCommonWestern1252Letter(b)) {
      score += prevHighNonPunct ? 1 : 2;
      prevHighNonPunct = true;
    } else if (isCommonWestern1252Punct(b)) {
      score += 1;
      prevHighNonPunct = false;
    } else {
      score += prevHighNonPunct ? -5 : -4;
      prevHighNonPunct = true;
    }
  }
  return score;
}

export function koreanPlausibility(bytes) {
  let score = 0;
  for (let i = 0; i < bytes.length;) {
    const b = bytes[i];
    if (b < 0x80) {
      score += (b === 0x09 || b === 0x0a || b === 0x0d || (b >= 0x20 && b <= 0x7e)) ? 1 : -8;
      i++;
      continue;
    }
    if (i + 1 >= bytes.length) return -1e9;
    const t = bytes[i + 1];
    if (b >= 0xb0 && b <= 0xc8 && t >= 0xa1 && t <= 0xfe) score += 3;
    else if (b >= 0xa1 && b <= 0xa3 && t >= 0xa1 && t <= 0xfe) score += 2;
    else if (b >= 0xca && b <= 0xfd && t >= 0xa1 && t <= 0xfe) score += 1;
    else if (b >= 0xa4 && b <= 0xac && t >= 0xa1 && t <= 0xfe) score += 0;
    else score -= 1;
    i += 2;
  }
  return score;
}

export function japanesePlausibility(text) {
  let score = 0;
  for (let i = 0; i < text.length; i++) {
    const code = text.charCodeAt(i);
    if ((code >= 0x3040 && code <= 0x309f) || (code >= 0x30a0 && code <= 0x30ff)) score += 4;
    else if ((code >= 0x3000 && code <= 0x303f) || (code >= 0xff01 && code <= 0xff60)) score += 3;
    else if ((code >= 0x4e00 && code <= 0x9fff) || (code >= 0x3400 && code <= 0x4dbf)) score += 2;
    else if (code === 0x09 || code === 0x0a || code === 0x0d || (code >= 0x20 && code <= 0x7e)) score += 1;
    else if (code >= 0xff61 && code <= 0xff9f) score -= 4;
    else if (code < 0x20 || (code >= 0x7f && code <= 0x9f)) score -= 8;
  }
  return score;
}

export function decodeImportedText(buffer, label = "文件", sourceLanguage = null) {
  const bytes = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer);
  const bad = () => new Error(`「${label || "文件"}」不是有效的 UTF-8 或受支持旧编码（日语 JIS：Shift_JIS／ISO-2022-JP／EUC-JP；韩语 EUC-KR／CP949；西欧 Windows-1252／ISO-8859-1）文本`);
  if (bytes.length >= 3 && bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf) {
    try { return { text: new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes), encoding: "UTF-8" }; }
    catch { throw bad(); }
  }
  if (bytes.length >= 2 && ((bytes[0] === 0xff && bytes[1] === 0xfe) || (bytes[0] === 0xfe && bytes[1] === 0xff))) {
    throw bad();
  }
  if ((!sourceLanguage || sourceLanguage === "ja") && hasIso2022JpEscape(bytes)) {
    try { return { text: new TextDecoder("iso-2022-jp", { fatal: true }).decode(bytes), encoding: "ISO-2022-JP" }; }
    catch { throw bad(); }
  }
  try { return { text: new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes), encoding: "UTF-8" }; } catch {}
  if (hasDisallowedControlByte(bytes)) throw bad();
  const tryDecode = (name) => { try { const out = new TextDecoder(name, { fatal: true }).decode(bytes); return /[-]/.test(out) ? null : out; } catch { return null; } };
  if (sourceLanguage === "ja") {
    const sjis = tryDecode("shift_jis"), euc = tryDecode("euc-jp");
    if (sjis !== null && euc === null) return { text: sjis, encoding: "Shift_JIS" };
    if (euc !== null && sjis === null) return { text: euc, encoding: "EUC-JP" };
    if (sjis !== null && euc !== null) {
      return japanesePlausibility(euc) > japanesePlausibility(sjis)
        ? { text: euc, encoding: "EUC-JP" }
        : { text: sjis, encoding: "Shift_JIS" };
    }
    throw bad();
  }
  if (sourceLanguage === "ko") {
    const ko = tryDecode("euc-kr");
    if (ko !== null) return { text: ko, encoding: "EUC-KR" };
    throw bad();
  }
  if (sourceLanguage === "en" || sourceLanguage === "fr" || sourceLanguage === "de" || sourceLanguage === "es" || sourceLanguage === "it") {
    const w = decodeWindows1252Strict(bytes);
    if (w !== null) return { text: w, encoding: "Windows-1252" };
    throw bad();
  }
  let best = null;
  const consider = (score, text, encoding) => { if (!best || score > best.score) best = { score, text, encoding }; };
  const sjis = tryDecode("shift_jis");
  if (sjis !== null) consider(japanesePlausibility(sjis), sjis, "Shift_JIS");
  const euc = tryDecode("euc-jp");
  if (euc !== null) consider(japanesePlausibility(euc), euc, "EUC-JP");
  const ko = tryDecode("euc-kr");
  if (ko !== null) consider(koreanPlausibility(bytes), ko, "EUC-KR");
  const w = decodeWindows1252Strict(bytes);
  if (w !== null) consider(westernPlausibility(bytes), w, "Windows-1252");
  if (best) return { text: best.text, encoding: best.encoding };
  throw bad();
}
