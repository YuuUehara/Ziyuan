//! Editable local translation memory. Adopting a hit is an explicit operator action;
//! entries never replace translations automatically.
use super::{Result, Translations, invalid};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TmEntry {
    pub id: String,
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub block: String,
    #[serde(default)]
    pub paragraph: String,
    #[serde(default)]
    pub adopted: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TmBook {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<TmEntry>,
}
fn one() -> u32 {
    1
}

impl Translations {
    fn tm_path(&self, project: &str) -> PathBuf {
        self.root
            .join(".ziyuan")
            .join("text-projects")
            .join(project)
            .join("tm.json")
    }

    fn load_tm(&self, project: &str) -> Result<TmBook> {
        let path = self.tm_path(project);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| invalid(format!("tm.json 格式不对：{e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TmBook {
                version: 1,
                entries: Vec::new(),
            }),
            Err(e) => Err(invalid(format!("读取 tm.json 失败：{e}"))),
        }
    }

    fn save_tm(&self, project: &str, book: &TmBook) -> Result<()> {
        let path = self.tm_path(project);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| invalid(format!("创建 tm.json 目录失败：{e}")))?;
        }
        let bytes = serde_json::to_vec_pretty(book).map_err(|e| invalid(e.to_string()))?;
        std::fs::write(&path, [bytes.as_slice(), b"\n"].concat())
            .map_err(|e| invalid(format!("写入 tm.json 失败：{e}")))
    }

    pub fn tm_list(&self, project: &str) -> Result<Vec<TmEntry>> {
        Ok(self.load_tm(project)?.entries)
    }

    pub fn tm_add(&self, project: &str, source: &str, target: &str) -> Result<TmEntry> {
        if source.trim().is_empty() || target.trim().is_empty() {
            return Err(invalid("原文与译文都不能为空"));
        }
        let mut book = self.load_tm(project)?;
        if let Some(existing) = book
            .entries
            .iter()
            .find(|e| e.source == source && e.target == target)
        {
            return Ok(existing.clone());
        }
        let next = book
            .entries
            .iter()
            .filter_map(|e| e.id.strip_prefix("tm-").and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0)
            + 1;
        let entry = TmEntry {
            id: format!("tm-{next}"),
            source: source.to_owned(),
            target: target.to_owned(),
            block: String::new(),
            paragraph: String::new(),
            adopted: 0,
        };
        book.entries.push(entry.clone());
        self.save_tm(project, &book)?;
        Ok(entry)
    }

    pub fn tm_update(
        &self,
        project: &str,
        id: &str,
        source: &str,
        target: &str,
    ) -> Result<TmEntry> {
        if source.trim().is_empty() || target.trim().is_empty() {
            return Err(invalid("原文与译文都不能为空"));
        }
        let mut book = self.load_tm(project)?;
        let entry = book
            .entries
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| invalid("没有这条翻译记忆"))?;
        entry.source = source.to_owned();
        entry.target = target.to_owned();
        let out = entry.clone();
        self.save_tm(project, &book)?;
        Ok(out)
    }

    pub fn tm_delete(&self, project: &str, id: &str) -> Result<()> {
        let mut book = self.load_tm(project)?;
        let before = book.entries.len();
        book.entries.retain(|e| e.id != id);
        if book.entries.len() == before {
            return Err(invalid("没有这条翻译记忆"));
        }
        self.save_tm(project, &book)
    }

    pub fn tm_adopt(&self, project: &str, id: &str) -> Result<TmEntry> {
        let mut book = self.load_tm(project)?;
        let entry = book
            .entries
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| invalid("没有这条翻译记忆"))?;
        entry.adopted += 1;
        let out = entry.clone();
        self.save_tm(project, &book)?;
        Ok(out)
    }

    /// Add source/target pairs from existing translations; duplicates are skipped.
    pub fn tm_build(&self, project: &str) -> Result<usize> {
        let project_file = self.project(project)?;
        let mut added = 0;
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
                    if source_para.text.trim().is_empty() || target_para.text.trim().is_empty() {
                        continue;
                    }
                    let before = self.tm_list(project)?.len();
                    let entry = self.tm_add(project, &source_para.text, &target_para.text)?;
                    if entry.block.is_empty() {
                        let mut book = self.load_tm(project)?;
                        if let Some(e) = book.entries.iter_mut().find(|e| e.id == entry.id) {
                            e.block = block.block_id.clone();
                            e.paragraph = source_para.id.clone();
                            self.save_tm(project, &book)?;
                        }
                    }
                    if self.tm_list(project)?.len() > before {
                        added += 1;
                    }
                }
            }
        }
        Ok(added)
    }
}
