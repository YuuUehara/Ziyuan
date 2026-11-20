//! UTF-8 与日文 JIS 编码（Shift_JIS、ISO-2022-JP、EUC-JP）的严格解码。
//!
//! 绝不做有损替换（不产生 U+FFFD）；有效 UTF-8 始终零拷贝原样返回。
use std::borrow::Cow;

pub const UNSUPPORTED_TEXT_ENCODING: &str =
    "仅接受 UTF-8 或日文 JIS（Shift_JIS／ISO-2022-JP／EUC-JP）文本";

/// 识别出的文本编码。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextEncoding {
    Utf8,
    ShiftJis,
    Iso2022Jp,
    EucJp,
}

impl TextEncoding {
    pub fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::ShiftJis => "Shift_JIS",
            Self::Iso2022Jp => "ISO-2022-JP",
            Self::EucJp => "EUC-JP",
        }
    }
}

/// 将导入的纯文本字节按 UTF-8 或日文 JIS 编码严格解码为 Unicode 字符串。
///
/// 规则：
/// 1. 带 UTF-8 BOM（`EF BB BF`）时只按 UTF-8 校验，损坏时不猜测 JIS；
/// 2. 含 ISO-2022-JP 转义序列（`ESC $ B` / `ESC $ @` / `ESC $ ( D`）时按 ISO-2022-JP 严格解码；
/// 3. 合法 UTF-8 零拷贝原样返回；
/// 4. 非 UTF-8 时分别按 Shift_JIS（CP932）与 EUC-JP 严格解码，两者均合法时按日文字符特征择优；
/// 5. 全部失败或含任意非法字节序列时报错拒绝。
pub fn decode_text_bytes(bytes: &[u8]) -> Result<(Cow<'_, str>, TextEncoding), &'static str> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return std::str::from_utf8(bytes)
            .map(|s| (Cow::Borrowed(s), TextEncoding::Utf8))
            .map_err(|_| UNSUPPORTED_TEXT_ENCODING);
    }

    if has_iso_2022_jp_escape(bytes) {
        return encoding_rs::ISO_2022_JP
            .decode_without_bom_handling_and_without_replacement(bytes)
            .map(|s| (s, TextEncoding::Iso2022Jp))
            .ok_or(UNSUPPORTED_TEXT_ENCODING);
    }

    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok((Cow::Borrowed(text), TextEncoding::Utf8));
    }

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

fn has_iso_2022_jp_escape(bytes: &[u8]) -> bool {
    bytes.windows(3).any(|w| w == b"\x1b$B" || w == b"\x1b$@")
        || bytes.windows(4).any(|w| w == b"\x1b$(D")
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

        // 带 UTF-8 BOM 但后续字节非法时必须拒绝，不猜 JIS。
        let broken_bom = [0xEF, 0xBB, 0xBF, 0x82, 0xA0];
        assert!(decode_text_bytes(&broken_bom).is_err());
    }

    #[test]
    fn decodes_shift_jis_including_aozora_ruby_and_cp932() {
        let sample = "｜東京《とうきょう》の空①。\r\n吾輩は猫である。";
        let (encoded, _, had_errors) = encoding_rs::SHIFT_JIS.encode(sample);
        assert!(!had_errors);
        let (decoded, enc) = decode_text_bytes(&encoded).unwrap();
        assert_eq!(enc, TextEncoding::ShiftJis);
        assert_eq!(decoded, sample);
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
    fn rejects_invalid_binary_and_truncated_jis_sequences() {
        assert!(decode_text_bytes(&[0xFF]).is_err());
        assert!(decode_text_bytes(&[0x82]).is_err()); // 截断的 Shift_JIS 首字节
        assert!(decode_text_bytes(&[0xA4, 0xFF]).is_err()); // 非法尾字节
        assert!(decode_text_bytes(b"\x1b$B\x7f\x1b(B").is_err()); // 损坏的 ISO-2022-JP
    }
}
