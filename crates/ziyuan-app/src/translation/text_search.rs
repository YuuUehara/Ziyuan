//! Exact local lookup and explicit, optimistic draft-only replacement.
use super::*;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize)]
pub struct TextSearchHit {
    pub chapter: String,
    pub block: String,
    pub paragraph: String,
    pub kind: String,
    pub text: String,
    pub version: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct TextSearch {
    pub hits: Vec<TextSearchHit>,
    pub truncated: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftReplace {
    pub block: String,
    pub paragraph: String,
    pub from: String,
    pub to: String,
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub confirm: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct DraftReplacePreview {
    pub fingerprint: String,
    pub before: String,
    pub after: String,
    pub occurrences: usize,
    pub saved: bool,
}
impl Translations {
    pub fn search_text(&self, project: &str, query: &str, scope: &str) -> Result<TextSearch> {
        if query.trim().is_empty()
            || query.len() > 4000
            || !matches!(
                scope,
                "all" | "source" | "translation" | "draft" | "history"
            )
        {
            return Err(invalid("检索文字须非空且不超过4000字节；范围须明确"));
        }
        let material = self.reading_materials(project)?;
        let mut hits = Vec::new();
        let mut truncated = false;
        let mut push = |chapter: &str,
                        block: &str,
                        paragraph: &str,
                        kind: &str,
                        text: &str,
                        version: Option<String>| {
            if text.contains(query) {
                if hits.len() >= 200 {
                    truncated = true;
                } else {
                    hits.push(TextSearchHit {
                        chapter: chapter.into(),
                        block: block.into(),
                        paragraph: paragraph.into(),
                        kind: kind.into(),
                        text: text.into(),
                        version,
                    });
                }
            }
        };
        for p in &material.reading.paragraphs {
            if matches!(scope, "all" | "source") {
                push(
                    &p.chapter,
                    &p.block,
                    &p.paragraph,
                    "source",
                    &p.source,
                    None,
                );
            }
            if matches!(scope, "all" | "translation")
                && let Some(target) = &p.translation
            {
                push(
                    &p.chapter,
                    &p.block,
                    &p.paragraph,
                    "translation",
                    target,
                    None,
                );
            }
        }
        if matches!(scope, "all" | "draft" | "history") {
            let p = self.project(project)?;
            for chapter in &p.chapters {
                for block in &chapter.blocks {
                    if matches!(scope, "all" | "draft")
                        && let Some(draft) = self.peek_human_draft(project, &block.block_id)?.draft
                    {
                        for paragraph in &draft.paragraphs {
                            push(
                                &chapter.id,
                                &block.block_id,
                                &paragraph.id,
                                "draft",
                                &paragraph.text,
                                Some(draft.revision.clone()),
                            );
                        }
                    }
                    if matches!(scope, "all" | "history") {
                        for version in self.list_translation_versions(project, &block.block_id)? {
                            let bytes = self.read_translation_version(
                                project,
                                &block.block_id,
                                &version.id,
                            )?;
                            let saved = super::versions::parse_snapshot(&bytes, &block.block_id)?;
                            for paragraph in saved.paragraphs {
                                push(
                                    &chapter.id,
                                    &block.block_id,
                                    &paragraph.id,
                                    "history",
                                    &paragraph.text,
                                    Some(version.id.clone()),
                                );
                            }
                        }
                    }
                }
            }
        }
        if material.reading.fingerprint != self.reading_materials(project)?.reading.fingerprint {
            return Err(invalid("检索期间原译文已变化，请重试"));
        }
        Ok(TextSearch { hits, truncated })
    }
    pub fn replace_draft_text(
        &self,
        project: &str,
        input: &DraftReplace,
    ) -> Result<DraftReplacePreview> {
        if input.from.is_empty() || input.from.len() > 4000 || input.to.len() > 16000 {
            return Err(invalid(
                "替换原文须非空；查找最多4000字节、替换最多16000字节",
            ));
        }
        let view = self.peek_human_draft(project, &input.block)?;
        let mut draft = view
            .draft
            .ok_or_else(|| invalid("请先保存人工草稿；替换不改批准译文"))?;
        if draft.basis != view.basis {
            return Err(invalid("草稿所据材料已变，请先核对并重新保存"));
        }
        let fingerprint = sha256_hex(&serde_json::to_vec(&(
            project,
            &draft,
            &input.paragraph,
            &input.from,
            &input.to,
        ))?);
        let paragraph = draft
            .paragraphs
            .iter_mut()
            .find(|p| p.id == input.paragraph)
            .ok_or_else(|| invalid("段落不在当前人工草稿中"))?;
        let edits: Vec<_> = paragraph
            .text
            .match_indices(&input.from)
            .map(|(start, _)| (start, start + input.from.len()))
            .collect();
        if edits.is_empty() {
            return Err(invalid("本段没有匹配文字"));
        }
        if paragraph.marks.iter().any(|m| {
            edits
                .iter()
                .any(|(start, end)| *start < m.end && *end > m.start)
        }) {
            return Err(invalid(
                "替换与文学标记范围相交，请在编辑器中核对标记后修改",
            ));
        }
        let before = paragraph.text.clone();
        let after = before.replace(&input.from, &input.to);
        let shift = input.to.len() as isize - input.from.len() as isize;
        for mark in &mut paragraph.marks {
            let start_shift =
                edits.iter().filter(|(_, end)| *end <= mark.start).count() as isize * shift;
            let end_shift =
                edits.iter().filter(|(_, end)| *end <= mark.end).count() as isize * shift;
            mark.start = mark
                .start
                .checked_add_signed(start_shift)
                .ok_or_else(|| invalid("标记位移溢出"))?;
            mark.end = mark
                .end
                .checked_add_signed(end_shift)
                .ok_or_else(|| invalid("标记位移溢出"))?;
        }
        paragraph.text = after.clone();
        super::markup::validate(&draft.paragraphs)?;
        if input.confirm {
            if input.fingerprint.as_deref() != Some(&fingerprint) {
                return Err(invalid("须先预览，且草稿不得自预览后变化"));
            }
            self.save_human_draft(
                project,
                &input.block,
                &DraftSave {
                    request_id: format!("replace-{}", &fingerprint[..32]),
                    expected_revision: Some(draft.revision),
                    basis: view.basis,
                    paragraphs: draft.paragraphs,
                },
            )?;
        }
        Ok(DraftReplacePreview {
            fingerprint,
            before,
            after,
            occurrences: edits.len(),
            saved: input.confirm,
        })
    }
}
