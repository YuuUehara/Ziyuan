//! Target-only annotations: byte ranges in the translator's text, never source offsets.
use super::{DraftParagraph, Result, invalid};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, mem::discriminant};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetMark {
    pub start: usize,
    pub end: usize,
    pub kind: TargetMarkKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TargetMarkKind {
    /// The base is text[start..end]. Empty readings are recoverable draft input.
    Ruby {
        reading: String,
    },
    // Empty struct variants ensure serde rejects extra fields on these kinds too.
    EmphasisDots {},
    Strong {},
    Emphasis {},
}

pub(super) fn validate(paragraphs: &[DraftParagraph]) -> Result<()> {
    if paragraphs.iter().map(|p| p.marks.len()).sum::<usize>() > 4096 {
        return Err(invalid("人工草稿标记超过 4096 项"));
    }
    let mut bytes = paragraphs.iter().map(|p| p.text.len()).sum::<usize>();
    if bytes > 256 * 1024 {
        return Err(invalid("人工草稿正文与读音合计超过 256 KiB"));
    }
    for paragraph in paragraphs {
        if paragraph.marks.is_empty() {
            continue;
        }
        let boundaries: BTreeSet<_> = paragraph
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain([paragraph.text.len()])
            .collect();
        for mark in &paragraph.marks {
            if mark.start >= mark.end
                || !boundaries.contains(&mark.start)
                || !boundaries.contains(&mark.end)
                || paragraph.text[mark.start..mark.end].contains(['\n', '\r'])
            {
                return Err(invalid("译文标记须为同一行内完整字素的非空范围"));
            }
            if let TargetMarkKind::Ruby { reading } = &mark.kind {
                if reading.contains(['\n', '\r']) {
                    return Err(invalid("注音读音不得含换行"));
                }
                bytes += reading.len();
            }
        }
        // Only disjoint ranges or different kinds on the exact same range are
        // supported. Sorting for validation never changes the saved mark order.
        let mut sorted: Vec<_> = paragraph.marks.iter().collect();
        sorted.sort_by_key(|m| {
            (
                m.start,
                m.end,
                match m.kind {
                    TargetMarkKind::Ruby { .. } => 0,
                    TargetMarkKind::EmphasisDots {} => 1,
                    TargetMarkKind::Strong {} => 2,
                    TargetMarkKind::Emphasis {} => 3,
                },
            )
        });
        for pair in sorted.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if b.start < a.end
                && ((a.start, a.end) != (b.start, b.end)
                    || discriminant(&a.kind) == discriminant(&b.kind))
            {
                return Err(invalid(
                    "译文标记只支持不相交或同范围的不同种类，不支持重复、嵌套或交叉",
                ));
            }
        }
    }
    if bytes > 256 * 1024 {
        return Err(invalid("人工草稿正文与读音合计超过 256 KiB"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use unicode_segmentation::UnicodeSegmentation;

    /// 与 `crates/ziyuan-web/src/editor-core.test.mjs` 共用同一张字素表：
    /// 前端按 ICU（`Intl.Segmenter`）取边界与换算 UTF-8 字节，服务端按
    /// `unicode-segmentation`（UAX #29）复核。两边切分不同的话，界面加出的标记
    /// 会被服务端拒绝，用户只能看到 400，所以这里固定住这几种容易分歧的写法。
    #[test]
    fn grapheme_boundaries_agree_with_web_editor() {
        for (text, expected) in [
            ("甲月", vec!["甲", "月"]),
            ("e\u{301}x", vec!["e\u{301}", "x"]),
            ("😀a", vec!["😀", "a"]),
            ("👨‍👩‍👧a", vec!["👨‍👩‍👧", "a"]),
        ] {
            assert_eq!(
                text.graphemes(true).collect::<Vec<_>>(),
                expected,
                "{text:?}"
            );
        }
    }
}
