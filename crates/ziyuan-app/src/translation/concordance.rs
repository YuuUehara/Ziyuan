//! Local bilingual concordance over existing translations; no model, no network.
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConcordanceHit {
    pub project: String,
    pub block: String,
    pub paragraph: String,
    pub source: String,
    pub target: String,
    pub score: f64,
}

fn bigrams(text: &str) -> BTreeMap<String, usize> {
    let chars: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    let mut map = BTreeMap::new();
    if chars.len() < 2 {
        for c in chars {
            *map.entry(c.to_string()).or_default() += 1;
        }
        return map;
    }
    for pair in chars.windows(2) {
        let key: String = pair.iter().collect();
        *map.entry(key).or_default() += 1;
    }
    map
}

fn similarity(query: &str, text: &str) -> f64 {
    if query.trim().is_empty() || text.trim().is_empty() {
        return 0.0;
    }
    if text.contains(query) || query.contains(text) {
        return 1.0;
    }
    let a = bigrams(query);
    let b = bigrams(text);
    let total: usize = a.values().sum::<usize>() + b.values().sum::<usize>();
    if total == 0 {
        return 0.0;
    }
    let overlap: usize = a
        .iter()
        .map(|(key, count)| count.min(b.get(key).unwrap_or(&0)))
        .sum();
    (2.0 * overlap as f64) / total as f64
}

impl Translations {
    /// Search the current project's existing translations by source-text similarity.
    pub fn concordance(
        &self,
        project: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ConcordanceHit>> {
        if query.trim().is_empty() {
            return Err(invalid("检索文字不能为空"));
        }
        if !(1..=50).contains(&limit) {
            return Err(invalid("检索条数须在 1 到 50 之间"));
        }
        let project_file = self.project(project)?;
        let mut hits = Vec::new();
        for chapter in &project_file.chapters {
            for block in &chapter.blocks {
                let Some(bytes) = self.read_translation(project, &block.block_id)? else {
                    continue;
                };
                let Ok(target) = serde_json::from_slice::<crate::text_project::PromptBlock>(&bytes)
                else {
                    continue;
                };
                for (source_para, target_para) in block.paragraphs.iter().zip(&target.paragraphs) {
                    let score = similarity(query, &source_para.text);
                    if score > 0.0 {
                        hits.push(ConcordanceHit {
                            project: project.to_owned(),
                            block: block.block_id.clone(),
                            paragraph: source_para.id.clone(),
                            source: source_para.text.clone(),
                            target: target_para.text.clone(),
                            score,
                        });
                    }
                }
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.block.cmp(&b.block))
        });
        hits.truncate(limit);
        Ok(hits)
    }
}
use super::{Result, Translations, invalid};
