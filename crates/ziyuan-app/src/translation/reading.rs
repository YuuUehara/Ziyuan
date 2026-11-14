use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadingParagraph {
    pub chapter: String,
    pub title: String,
    pub block: String,
    pub paragraph: String,
    pub source: String,
    pub source_inlines: Vec<crate::text_project::Inline>,
    pub translation: Option<String>,
    pub marks: Vec<TargetMark>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ReadingView {
    pub project: String,
    pub fingerprint: String,
    pub paragraphs: Vec<ReadingParagraph>,
    pub warnings: Vec<String>,
}
pub(crate) struct Materials {
    pub anchor_index: BTreeMap<String, usize>,
    pub reading: ReadingView,
    pub block_hashes: BTreeMap<String, String>,
    pub context_hash: String,
}
impl Translations {
    pub(crate) fn reading_materials(&self, project: &str) -> Result<Materials> {
        let p = self.project(project)?;
        self.reading_materials_for(&p)
    }
    pub(crate) fn reading_materials_for(
        &self,
        p: &crate::text_project::Project,
    ) -> Result<Materials> {
        let project = p.id.as_str();
        let mut paragraphs = Vec::new();
        let mut block_hashes = BTreeMap::new();
        let mut warnings = Vec::new();
        let mut total = 0usize;
        for chapter in &p.chapters {
            for block in &chapter.blocks {
                let bytes = self.read_translation(project, &block.block_id)?;
                total += bytes.as_ref().map_or(0, Vec::len);
                if total > 256 * 1024 * 1024 {
                    return Err(invalid("通读材料超过256 MiB，请按卷拆分项目"));
                }
                block_hashes.insert(
                    block.block_id.clone(),
                    sha256_hex(bytes.as_deref().unwrap_or(b"")),
                );
                let mut targets = BTreeMap::new();
                if let Some(bytes) = &bytes {
                    let aligned = match read_approved_target(bytes, &block.block_id) {
                        Ok(ApprovedTarget::Plain(target))
                            if target
                                .paragraphs
                                .iter()
                                .map(|p| &p.id)
                                .eq(block.paragraphs.iter().map(|p| &p.id)) =>
                        {
                            for p in target.paragraphs {
                                targets.insert(p.id, (p.text, Vec::new()));
                            }
                            true
                        }
                        Ok(ApprovedTarget::Marked(target))
                            if target
                                .paragraphs
                                .iter()
                                .map(|p| &p.id)
                                .eq(block.paragraphs.iter().map(|p| &p.id)) =>
                        {
                            for p in target.paragraphs {
                                targets.insert(p.id, (p.text, p.marks));
                            }
                            true
                        }
                        _ => false,
                    };
                    if !aligned {
                        warnings.push(format!(
                            "{} 的译稿结构无效，未作为通读译文展示",
                            block.block_id
                        ));
                    }
                }
                for source in &block.paragraphs {
                    let target = targets.remove(&source.id);
                    let (translation, marks) =
                        target.map_or((None, Vec::new()), |(s, m)| (Some(s), m));
                    paragraphs.push(ReadingParagraph {
                        chapter: chapter.id.clone(),
                        title: chapter
                            .parsing
                            .as_ref()
                            .map_or(&chapter.source_name, |p| &p.title)
                            .clone(),
                        block: block.block_id.clone(),
                        paragraph: source.id.clone(),
                        source: source.text.clone(),
                        source_inlines: source.inlines.clone(),
                        translation,
                        marks,
                    });
                }
            }
        }
        let context_hash = sha256_hex(&serde_json::to_vec(&self.current_context(project)?)?);
        let fingerprint = sha256_hex(&serde_json::to_vec(&(&p, &block_hashes, &context_hash))?);
        let anchor_index = paragraphs
            .iter()
            .enumerate()
            .map(|(i, p)| (p.paragraph.clone(), i))
            .collect();
        Ok(Materials {
            anchor_index,
            reading: ReadingView {
                project: project.into(),
                fingerprint,
                paragraphs,
                warnings,
            },
            block_hashes,
            context_hash,
        })
    }
    pub fn read_chapter(&self, project: &str, chapter: Option<&str>) -> Result<ReadingView> {
        let p = self.project(project)?;
        let mut result = self.reading_materials_for(&p)?.reading;
        if let Some(chapter) = chapter {
            if !p.chapters.iter().any(|c| c.id == chapter) {
                return Err(invalid("章节不存在"));
            }
            result.paragraphs.retain(|p| p.chapter == chapter);
        }
        if result.fingerprint != self.reading_materials(project)?.reading.fingerprint {
            return Err(invalid("读取期间材料变化，请重试"));
        }
        Ok(result)
    }
}
