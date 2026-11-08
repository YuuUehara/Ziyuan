//! 译稿比较：现行译稿或人工草稿对某份快照，逐段给出增、删、改与标记变化。
use super::versions::parse_snapshot;
use super::{Result, TargetMark, Translations, invalid, read_approved_target};
use serde::Serialize;

/// 逐段差异。`kind` 为 `added`、`removed`、`changed` 或 `markup`（文字相同、标记不同）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranslationChange {
    pub paragraph: String,
    pub kind: &'static str,
    pub old: Option<String>,
    pub new: Option<String>,
}

pub(crate) type VersionParagraphs = Vec<(String, String, Vec<TargetMark>)>;

pub(crate) fn compare_paragraphs(
    old: VersionParagraphs,
    new: VersionParagraphs,
) -> Vec<TranslationChange> {
    use std::collections::BTreeMap;
    let old_map: BTreeMap<&str, (&str, &[TargetMark])> = old
        .iter()
        .map(|(id, text, marks)| (id.as_str(), (text.as_str(), marks.as_slice())))
        .collect();
    let new_map: BTreeMap<&str, (&str, &[TargetMark])> = new
        .iter()
        .map(|(id, text, marks)| (id.as_str(), (text.as_str(), marks.as_slice())))
        .collect();
    let mut out = Vec::new();
    for (id, (text, marks)) in &old_map {
        match new_map.get(id) {
            None => out.push(TranslationChange {
                paragraph: (*id).into(),
                kind: "removed",
                old: Some((*text).into()),
                new: None,
            }),
            Some((new_text, _)) if new_text != text => out.push(TranslationChange {
                paragraph: (*id).into(),
                kind: "changed",
                old: Some((*text).into()),
                new: Some((*new_text).into()),
            }),
            Some((_, new_marks)) if *new_marks != *marks => out.push(TranslationChange {
                paragraph: (*id).into(),
                kind: "markup",
                old: None,
                new: None,
            }),
            _ => {}
        }
    }
    for (id, (text, _)) in &new_map {
        if !old_map.contains_key(id) {
            out.push(TranslationChange {
                paragraph: (*id).into(),
                kind: "added",
                old: None,
                new: Some((*text).into()),
            });
        }
    }
    out.sort_by(|a, b| a.paragraph.cmp(&b.paragraph));
    out
}

fn target_paragraphs(bytes: &[u8], block: &str) -> Result<VersionParagraphs> {
    let mut out = Vec::new();
    match read_approved_target(bytes, block)? {
        super::ApprovedTarget::Plain(prompt) => {
            out.extend(
                prompt
                    .paragraphs
                    .into_iter()
                    .map(|p| (p.id, p.text, Vec::new())),
            );
        }
        super::ApprovedTarget::Marked(candidate) => {
            out.extend(
                candidate
                    .paragraphs
                    .into_iter()
                    .map(|p| (p.id, p.text, p.marks)),
            );
        }
    }
    Ok(out)
}

impl Translations {
    /// 现行 `zh.json` 对某份快照。
    pub fn compare_translation(
        &self,
        project: &str,
        block: &str,
        old_bytes: &[u8],
    ) -> Result<Vec<TranslationChange>> {
        let old = parse_snapshot(old_bytes, block)?.into_parts();
        let Some(current) = self.read_translation(project, block)? else {
            return Err(invalid("当前块没有译稿"));
        };
        let new = target_paragraphs(&current, block)?;
        Ok(compare_paragraphs(old, new))
    }

    /// 现行人工草稿对某份快照。没有草稿时拒绝，不拿现行译稿冒充草稿。
    pub fn compare_draft(
        &self,
        project: &str,
        block: &str,
        old_bytes: &[u8],
    ) -> Result<Vec<TranslationChange>> {
        let old = parse_snapshot(old_bytes, block)?.into_parts();
        let new = self.draft_paragraphs(project, block)?;
        Ok(compare_paragraphs(old, new))
    }

    pub(crate) fn draft_paragraphs(&self, project: &str, block: &str) -> Result<VersionParagraphs> {
        let draft = self
            .peek_human_draft(project, block)?
            .draft
            .ok_or_else(|| invalid("当前块尚无人工草稿，不能与草稿比较"))?;
        Ok(draft
            .paragraphs
            .into_iter()
            .map(|p| (p.id, p.text, p.marks))
            .collect())
    }

    /// 按快照 ID 比较，`against` 为 `current`（现行译稿）或 `draft`（人工草稿）。
    pub fn compare_version(
        &self,
        project: &str,
        block: &str,
        id: &str,
        against_draft: bool,
    ) -> Result<Vec<TranslationChange>> {
        let old = self.read_translation_version(project, block, id)?;
        if against_draft {
            self.compare_draft(project, block, &old)
        } else {
            self.compare_translation(project, block, &old)
        }
    }
}
