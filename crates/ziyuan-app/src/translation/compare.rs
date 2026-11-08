//! Compare the current translation against a saved PromptBlock JSON (for example an older export).
use super::{Result, Translations, invalid};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TranslationChange {
    pub paragraph: String,
    pub kind: &'static str,
    pub old: Option<String>,
    pub new: Option<String>,
}

impl Translations {
    pub fn compare_translation(&self, project: &str, block: &str, old_bytes: &[u8]) -> Result<Vec<TranslationChange>> {
        let old = serde_json::from_slice::<crate::text_project::PromptBlock>(old_bytes)
            .map_err(|e| invalid(format!("旧译稿不是有效 PromptBlock JSON：{e}")))?;
        let Some(current_bytes) = self.read_translation(project, block)? else {
            return Err(invalid("当前块没有译稿"));
        };
        let current = serde_json::from_slice::<crate::text_project::PromptBlock>(&current_bytes)
            .map_err(|e| invalid(format!("当前译稿解析失败：{e}")))?;
        let old_map: std::collections::BTreeMap<&str, &str> =
            old.paragraphs.iter().map(|p| (p.id.as_str(), p.text.as_str())).collect();
        let new_map: std::collections::BTreeMap<&str, &str> =
            current.paragraphs.iter().map(|p| (p.id.as_str(), p.text.as_str())).collect();
        let mut out = Vec::new();
        for (id, text) in &old_map {
            match new_map.get(id) {
                None => out.push(TranslationChange { paragraph: (*id).into(), kind: "removed", old: Some((*text).into()), new: None }),
                Some(new) if new != text => out.push(TranslationChange { paragraph: (*id).into(), kind: "changed", old: Some((*text).into()), new: Some((*new).into()) }),
                _ => {}
            }
        }
        for (id, text) in &new_map {
            if !old_map.contains_key(id) {
                out.push(TranslationChange { paragraph: (*id).into(), kind: "added", old: None, new: Some((*text).into()) });
            }
        }
        out.sort_by(|a, b| a.paragraph.cmp(&b.paragraph));
        Ok(out)
    }
}
