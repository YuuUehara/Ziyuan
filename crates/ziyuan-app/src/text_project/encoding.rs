//! UTF-8 与受支持旧编码（日文 JIS：Shift_JIS／ISO-2022-JP／EUC-JP；韩文 EUC-KR／CP949；
//! 西欧 Windows-1252／ISO-8859-1）的严格解码。
//!
//! 绝不做有损替换（不产生 U+FFFD）；有效 UTF-8 始终零拷贝原样返回。
use std::borrow::Cow;

pub const UNSUPPORTED_TEXT_ENCODING: &str = "仅接受 UTF-8 或受支持旧编码（JIS：Shift_JIS／ISO-2022-JP／EUC-JP；韩文 EUC-KR／CP949；西欧 Windows-1252／ISO-8859-1）文本";

/// 识别出的文本编码。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextEncoding {
    Utf8,
    ShiftJis,
    Iso2022Jp,
    EucJp,
    EucKr,
    Windows1252,
}

impl TextEncoding {
    pub fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::ShiftJis => "Shift_JIS",
            Self::Iso2022Jp => "ISO-2022-JP",
            Self::EucJp => "EUC-JP",
            Self::EucKr => "EUC-KR",
            Self::Windows1252 => "Windows-1252",
        }
    }
}

/// 将导入的纯文本字节按 UTF-8 或受支持旧编码严格解码为 Unicode 字符串（无语种提示）。
pub fn decode_text_bytes(bytes: &[u8]) -> Result<(Cow<'_, str>, TextEncoding), &'static str> {
    decode_text_bytes_for_language(bytes, None)
}

/// 将导入的纯文本字节按 UTF-8 或对应语种的受支持旧编码严格解码为 Unicode 字符串。
///
/// 规则：
/// 1. 带 UTF-8 BOM（`EF BB BF`）时只按 UTF-8 校验，损坏时不猜测旧编码；
/// 2. 带 UTF-16 BOM（`FF FE` / `FE FF`）时直接拒绝；
/// 3. 含 ISO-2022-JP 转义序列（`ESC $ B` / `ESC $ @` / `ESC $ ( D`）且语种为 `ja` 或未指定时按 ISO-2022-JP 严格解码；
/// 4. 合法 UTF-8 零拷贝原样返回；
/// 5. 非 UTF-8 含非法控制字节（如 NUL）时拒绝；
/// 6. 已知原文语种时只按该语种旧编码族严格解码（`ja` → Shift_JIS／EUC-JP；`ko` → EUC-KR／CP949；`en`/`fr`/`de` → Windows-1252／ISO-8859-1），
///    未指定语种时在全部受支持旧编码中严格解码并按语言字符特征择优；
/// 7. 全部失败或含任意非法字节序列时报错拒绝。
pub fn decode_text_bytes_for_language<'a>(
    bytes: &'a [u8],
    source_language: Option<&str>,
) -> Result<(Cow<'a, str>, TextEncoding), &'static str> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return std::str::from_utf8(bytes)
            .map(|s| (Cow::Borrowed(s), TextEncoding::Utf8))
            .map_err(|_| UNSUPPORTED_TEXT_ENCODING);
    }

    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return Err(UNSUPPORTED_TEXT_ENCODING);
    }

    if matches!(source_language, None | Some("ja")) && has_iso_2022_jp_escape(bytes) {
        return encoding_rs::ISO_2022_JP
            .decode_without_bom_handling_and_without_replacement(bytes)
            .map(|s| (s, TextEncoding::Iso2022Jp))
            .ok_or(UNSUPPORTED_TEXT_ENCODING);
    }

    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok((Cow::Borrowed(text), TextEncoding::Utf8));
    }

    if has_disallowed_control_byte(bytes) {
        return Err(UNSUPPORTED_TEXT_ENCODING);
    }

    match source_language {
        Some("ja") => decode_japanese_legacy(bytes),
        Some("ko") => decode_korean_legacy(bytes),
        Some("en" | "fr" | "de") => decode_western_legacy(bytes),
        _ => decode_any_legacy(bytes),
    }
}

fn decode_japanese_legacy(bytes: &[u8]) -> Result<(Cow<'_, str>, TextEncoding), &'static str> {
    let sjis = encoding_rs::SHIFT_JIS.decode_without_bom_handling_and_without_replacement(bytes);
    let euc = encoding_rs::EUC_JP.decode_without_bom_handling_and_without_replacement(bytes);

    match (sjis, euc) {
        (Some(s), None) => Ok((s, TextEncoding::ShiftJis)),
        (None, Some(e)) => Ok((e, TextEncoding::EucJp)),
        (Some(s), Some(e)) => {
            if japanese_plausibility(&e) > japanese_plausibility(&s) {
                Ok((e, TextEncoding::EucJp))
            } else {
                Ok((s, TextEncoding::ShiftJis))
            }
        }
        (None, None) => Err(UNSUPPORTED_TEXT_ENCODING),
    }
}

fn decode_korean_legacy(bytes: &[u8]) -> Result<(Cow<'_, str>, TextEncoding), &'static str> {
    encoding_rs::EUC_KR
        .decode_without_bom_handling_and_without_replacement(bytes)
        .map(|s| (s, TextEncoding::EucKr))
        .ok_or(UNSUPPORTED_TEXT_ENCODING)
}

fn decode_western_legacy(bytes: &[u8]) -> Result<(Cow<'_, str>, TextEncoding), &'static str> {
    if !is_plausible_windows_1252(bytes) {
        return Err(UNSUPPORTED_TEXT_ENCODING);
    }
    encoding_rs::WINDOWS_1252
        .decode_without_bom_handling_and_without_replacement(bytes)
        .map(|s| (s, TextEncoding::Windows1252))
        .ok_or(UNSUPPORTED_TEXT_ENCODING)
}

fn decode_any_legacy<'a>(bytes: &'a [u8]) -> Result<(Cow<'a, str>, TextEncoding), &'static str> {
    let mut best: Option<(i64, Cow<'a, str>, TextEncoding)> = None;
    let mut consider = |score: i64, text: Cow<'a, str>, enc: TextEncoding| {
        if best
            .as_ref()
            .is_none_or(|(best_score, _, _)| score > *best_score)
        {
            best = Some((score, text, enc));
        }
    };

    if let Some(s) =
        encoding_rs::SHIFT_JIS.decode_without_bom_handling_and_without_replacement(bytes)
    {
        consider(japanese_plausibility(&s), s, TextEncoding::ShiftJis);
    }
    if let Some(e) = encoding_rs::EUC_JP.decode_without_bom_handling_and_without_replacement(bytes)
    {
        consider(japanese_plausibility(&e), e, TextEncoding::EucJp);
    }
    if let Some(k) = encoding_rs::EUC_KR.decode_without_bom_handling_and_without_replacement(bytes)
    {
        consider(korean_plausibility(bytes), k, TextEncoding::EucKr);
    }
    if is_plausible_windows_1252(bytes)
        && let Some(w) =
            encoding_rs::WINDOWS_1252.decode_without_bom_handling_and_without_replacement(bytes)
    {
        consider(western_plausibility(bytes), w, TextEncoding::Windows1252);
    }

    best.map(|(_, text, enc)| (text, enc))
        .ok_or(UNSUPPORTED_TEXT_ENCODING)
}

fn has_iso_2022_jp_escape(bytes: &[u8]) -> bool {
    bytes.windows(3).any(|w| w == b"\x1b$B" || w == b"\x1b$@")
        || bytes.windows(4).any(|w| w == b"\x1b$(D")
}

fn has_disallowed_control_byte(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .any(|&b| matches!(b, 0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F | 0x7F))
}

fn is_undefined_windows_1252(b: u8) -> bool {
    matches!(b, 0x81 | 0x8D | 0x8F | 0x90 | 0x9D)
}

fn is_common_western_1252_letter(b: u8) -> bool {
    matches!(
        b,
        0x8C | 0x9C
            | 0x9F
            | 0xC0..=0xCF
            | 0xD1..=0xD6
            | 0xD9..=0xDC
            | 0xDF..=0xEF
            | 0xF1..=0xF6
            | 0xF9..=0xFC
            | 0xFF
    )
}

fn is_common_western_1252_punct(b: u8) -> bool {
    matches!(
        b,
        0x80 | 0x82 | 0x84 | 0x85 | 0x8B | 0x91
            ..=0x97
                | 0x9B
                | 0xA0
                | 0xA3
                | 0xA7
                | 0xA9
                | 0xAB
                | 0xAD
                | 0xAE
                | 0xB0
                | 0xB2
                | 0xB3
                | 0xB4
                | 0xB7
                | 0xBA
                | 0xBB
    )
}

fn is_plausible_windows_1252(bytes: &[u8]) -> bool {
    if bytes.is_empty() || has_disallowed_control_byte(bytes) {
        return false;
    }
    let mut non_ascii = 0usize;
    let mut common_western = 0usize;
    let mut has_ascii_alnum = false;
    for &b in bytes {
        if is_undefined_windows_1252(b) {
            return false;
        }
        if b.is_ascii_alphanumeric() {
            has_ascii_alnum = true;
        } else if b >= 0x80 {
            non_ascii += 1;
            if is_common_western_1252_letter(b) || is_common_western_1252_punct(b) {
                common_western += 1;
            }
        }
    }
    non_ascii > 0
        && has_ascii_alnum
        && non_ascii * 3 <= bytes.len() * 2
        && common_western * 4 >= non_ascii * 3
}

fn western_plausibility(bytes: &[u8]) -> i64 {
    let mut score = 0i64;
    let mut prev_high_non_punct = false;
    for &b in bytes {
        if b < 0x80 {
            score += match b {
                0x09 | 0x0A | 0x0D | 0x20..=0x7E => 1,
                _ => -8,
            };
            prev_high_non_punct = false;
        } else if is_common_western_1252_letter(b) {
            score += 2;
            if prev_high_non_punct {
                score -= 1;
            }
            prev_high_non_punct = true;
        } else if is_common_western_1252_punct(b) {
            score += 1;
            prev_high_non_punct = false;
        } else {
            score -= 4;
            if prev_high_non_punct {
                score -= 1;
            }
            prev_high_non_punct = true;
        }
    }
    score
}

fn korean_plausibility(bytes: &[u8]) -> i64 {
    let mut score = 0i64;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 {
            score += match b {
                0x09 | 0x0A | 0x0D | 0x20..=0x7E => 1,
                _ => -8,
            };
            i += 1;
            continue;
        }
        if i + 1 >= bytes.len() {
            return i64::MIN / 2;
        }
        let trail = bytes[i + 1];
        score += match (b, trail) {
            // KS X 1001 常用韩文音节（2,350 字，绝大多数韩文文本落在此区间）
            (0xB0..=0xC8, 0xA1..=0xFE) => 3,
            // KS X 1001 全角标点与符号
            (0xA1..=0xA3, 0xA1..=0xFE) => 2,
            // KS X 1001 汉字
            (0xCA..=0xFD, 0xA1..=0xFE) => 1,
            // 兼容字母／特殊符号（EUC-JP 假名 0xA4/0xA5 误按 EUC-KR 解码会落在这里）
            (0xA4..=0xAC, 0xA1..=0xFE) => 0,
            // CP949 扩展生僻韩文音节（Shift_JIS 汉字 0x88..=0x9F 也重叠于此）
            _ => -1,
        };
        i += 2;
    }
    score
}

fn japanese_plausibility(text: &str) -> i64 {
    let mut score = 0i64;
    for ch in text.chars() {
        let code = ch as u32;
        score += match code {
            // 全角平假名、片假名
            0x3040..=0x30FF => 4,
            // CJK 标点与全角符号
            0x3000..=0x303F | 0xFF01..=0xFF60 => 3,
            // 常用汉字
            0x4E00..=0x9FFF | 0x3400..=0x4DBF => 2,
            // 常见 ASCII 可见字符与空白
            0x09 | 0x0A | 0x0D | 0x20..=0x7E => 1,
            // 半角片假名（EUC-JP 假名误按 Shift_JIS 解码常成串落入此区间）
            0xFF61..=0xFF9F => -4,
            // 控制字符
            0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F | 0x7F..=0x9F => -8,
            _ => 0,
        };
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_utf8_zero_copy_with_and_without_bom() {
        let plain = "吾輩は猫である。\n".as_bytes();
        let (text, enc) = decode_text_bytes(plain).unwrap();
        assert_eq!(enc, TextEncoding::Utf8);
        assert!(matches!(text, Cow::Borrowed(_)));
        assert_eq!(text, "吾輩は猫である。\n");

        let with_bom = "\u{feff}第一章\n".as_bytes();
        let (text, enc) = decode_text_bytes(with_bom).unwrap();
        assert_eq!(enc, TextEncoding::Utf8);
        assert_eq!(text, "\u{feff}第一章\n");

        // 带 UTF-8 BOM 但后续字节非法时必须拒绝，不猜旧编码。
        let broken_bom = [0xEF, 0xBB, 0xBF, 0x82, 0xA0];
        assert!(decode_text_bytes(&broken_bom).is_err());
        assert!(decode_text_bytes_for_language(&broken_bom, Some("fr")).is_err());
    }

    #[test]
    fn decodes_shift_jis_including_aozora_ruby_and_cp932() {
        let sample = "｜東京《とうきょう》の空①。\r\n吾輩は猫である。";
        let (encoded, _, had_errors) = encoding_rs::SHIFT_JIS.encode(sample);
        assert!(!had_errors);
        let (decoded, enc) = decode_text_bytes(&encoded).unwrap();
        assert_eq!(enc, TextEncoding::ShiftJis);
        assert_eq!(decoded, sample);

        // 短纯汉字 Shift_JIS（0x90 0xD4 0x8C 0xBB）在无语种提示下也不能被 CP949 抢走。
        let (short_sjis, _, had_errors) = encoding_rs::SHIFT_JIS.encode("赤現");
        assert!(!had_errors);
        let (decoded, enc) = decode_text_bytes(&short_sjis).unwrap();
        assert_eq!(enc, TextEncoding::ShiftJis);
        assert_eq!(decoded, "赤現");
    }

    #[test]
    fn decodes_iso_2022_jp_even_though_all_bytes_are_7bit() {
        let sample = "第一章\n吾輩は猫である。名前はまだ無い。\n";
        let (encoded, _, had_errors) = encoding_rs::ISO_2022_JP.encode(sample);
        assert!(!had_errors);
        assert!(std::str::from_utf8(&encoded).is_ok());
        let (decoded, enc) = decode_text_bytes(&encoded).unwrap();
        assert_eq!(enc, TextEncoding::Iso2022Jp);
        assert_eq!(decoded, sample);
    }

    #[test]
    fn decodes_euc_jp_including_bytes_also_valid_in_shift_jis() {
        // 这一段纯假名与标点在 EUC-JP 中的所有字节都落在 0xA1..=0xDF，按 Shift_JIS 也可无替换地解为半角片假名。
        let sample = "あいうえお、かきくけこ。\nさしすせそたちつてとなにぬねの。";
        let (encoded, _, had_errors) = encoding_rs::EUC_JP.encode(sample);
        assert!(!had_errors);
        assert!(
            encoding_rs::SHIFT_JIS
                .decode_without_bom_handling_and_without_replacement(&encoded)
                .is_some()
        );
        let (decoded, enc) = decode_text_bytes(&encoded).unwrap();
        assert_eq!(enc, TextEncoding::EucJp);
        assert_eq!(decoded, sample);
    }

    #[test]
    fn decodes_korean_euc_kr_and_cp949_extended_syllables() {
        let sample =
            "제1장\n안녕하세요. 한국어 소설입니다.\n뷁 같은 확장 완성형 글자도 복원됩니다.";
        let (encoded, _, had_errors) = encoding_rs::EUC_KR.encode(sample);
        assert!(!had_errors);
        let (decoded, enc) = decode_text_bytes_for_language(&encoded, Some("ko")).unwrap();
        assert_eq!(enc, TextEncoding::EucKr);
        assert_eq!(decoded, sample);

        let (decoded_auto, enc_auto) = decode_text_bytes(&encoded).unwrap();
        assert_eq!(enc_auto, TextEncoding::EucKr);
        assert_eq!(decoded_auto, sample);

        // 即使仅含单个 CP949 扩展生僻字，在 ko 语种上下文下也准确按 EUC-KR 解码。
        let (cp949_only, _, had_errors) = encoding_rs::EUC_KR.encode("뷁");
        assert!(!had_errors);
        let (decoded_only, enc_only) =
            decode_text_bytes_for_language(&cp949_only, Some("ko")).unwrap();
        assert_eq!(enc_only, TextEncoding::EucKr);
        assert_eq!(decoded_only, "뷁");
    }

    #[test]
    fn decodes_western_windows_1252_for_french_german_and_english() {
        let french = "« L’œuvre naïve d’un élève à côté du café… 10 € »\n";
        let (fr_bytes, _, had_errors) = encoding_rs::WINDOWS_1252.encode(french);
        assert!(!had_errors);
        for lang in [Some("fr"), None] {
            let (decoded, enc) = decode_text_bytes_for_language(&fr_bytes, lang).unwrap();
            assert_eq!(enc, TextEncoding::Windows1252);
            assert_eq!(decoded, french);
        }

        let german = "Über die Straße: „Grüße aus München — Äpfel und Öl!“\n";
        let (de_bytes, _, had_errors) = encoding_rs::WINDOWS_1252.encode(german);
        assert!(!had_errors);
        for lang in [Some("de"), None] {
            let (decoded, enc) = decode_text_bytes_for_language(&de_bytes, lang).unwrap();
            assert_eq!(enc, TextEncoding::Windows1252);
            assert_eq!(decoded, german);
        }

        let english = "“Hello,” she said—it’s £5.\n";
        let (en_bytes, _, had_errors) = encoding_rs::WINDOWS_1252.encode(english);
        assert!(!had_errors);
        let (decoded, enc) = decode_text_bytes_for_language(&en_bytes, Some("en")).unwrap();
        assert_eq!(enc, TextEncoding::Windows1252);
        assert_eq!(decoded, english);
    }

    #[test]
    fn rejects_invalid_binary_utf16_gbk_and_truncated_sequences() {
        for lang in [
            None,
            Some("ja"),
            Some("ko"),
            Some("fr"),
            Some("de"),
            Some("en"),
        ] {
            assert!(decode_text_bytes_for_language(&[0xFF], lang).is_err());
            assert!(decode_text_bytes_for_language(&[0x82], lang).is_err());
            assert!(decode_text_bytes_for_language(&[0xA4, 0xFF], lang).is_err());
            assert!(decode_text_bytes_for_language(&[0xFF, 0xFE, b'a', 0], lang).is_err());
            assert!(decode_text_bytes_for_language(&[0xFE, 0xFF, 0, b'a'], lang).is_err());
            assert!(decode_text_bytes_for_language(b"hello\x00\xe9", lang).is_err());
        }
        assert!(decode_text_bytes(b"\x1b$B\x7f\x1b(B").is_err()); // 损坏的 ISO-2022-JP
        assert!(decode_text_bytes_for_language(b"Cafe\x81", Some("fr")).is_err()); // Windows-1252 未定义 C1 字节
        // GBK 中文在西欧语种下不能被误当 Windows-1252 接收。
        let (gbk, _, _) = encoding_rs::GBK.encode("第1章 你好世界，这是中文测试。");
        assert!(decode_text_bytes_for_language(&gbk, Some("fr")).is_err());
        assert!(decode_text_bytes_for_language(&gbk, Some("de")).is_err());
    }
}
