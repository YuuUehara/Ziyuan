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
    pub(super) fn lock_local_translation(
        &self,
        project: &str,
    ) -> Result<ziyuan_storage::WriteGuard> {
        self.project(project)?;
        super::check_plain_dirs(self.root(), &[".ziyuan", "text-projects", project])?;
        let workspace =
            ziyuan_storage::Workspace::open(self.root()).map_err(|e| invalid(e.to_string()))?;
        let guard = workspace
            .lock(ziyuan_storage::LockScope::Workspace)
            .map_err(|e| invalid(e.to_string()))?;
        workspace
            .ensure_writable()
            .map_err(|e| invalid(e.to_string()))?;
        Ok(guard)
    }
    pub(super) fn read_local_translation_file(
        &self,
        path: &std::path::Path,
    ) -> Result<Option<Vec<u8>>> {
        use std::io::Read;
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > 32 * 1024 * 1024
        {
            return Err(invalid("本地资料须为32 MiB以内普通文件"));
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err(invalid("本地资料过大"));
        }
        Ok(Some(bytes))
    }
    pub(super) fn check_local_comment_anchor(
        &self,
        project: &str,
        block: &str,
        paragraph: &str,
    ) -> Result<()> {
        let p = self.project(project)?;
        let (_, _, source) = super::find_block(&p, block)?;
        if !paragraph.is_empty() && !source.paragraphs.iter().any(|p| p.id == paragraph) {
            return Err(invalid("评论段号不属于当前块"));
        }
        Ok(())
    }
    fn tm_path(&self, project: &str) -> Result<PathBuf> {
        self.project(project)?;
        Ok(
            super::check_plain_dirs(self.root(), &[".ziyuan", "text-projects", project])?
                .join("tm.json"),
        )
    }
    fn load_tm(&self, project: &str) -> Result<TmBook> {
        let path = self.tm_path(project)?;
        let book: TmBook = match self.read_local_translation_file(&path)? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| invalid(format!("tm.json 格式不对：{e}"))),
            None => Ok(TmBook {
                version: 1,
                entries: Vec::new(),
            }),
        }?;
        if book.version != 1 || book.entries.len() > 10000 {
            return Err(invalid("翻译记忆版本或数量不支持"));
        }
        Ok(book)
    }

    fn save_tm(&self, project: &str, book: &TmBook) -> Result<()> {
        if book.entries.len() > 10000 {
            return Err(invalid("本地记录超过10000条上限"));
        }
        let path = self.tm_path(project)?;
        let bytes = serde_json::to_vec_pretty(book)?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err(invalid("本地记录超过32 MiB"));
        }
        super::write_plain(&path, &[bytes.as_slice(), b"\n"].concat())
    }

    pub fn tm_list(&self, project: &str) -> Result<Vec<TmEntry>> {
        Ok(self.load_tm(project)?.entries)
    }

    /// One workspace lock, one atomic publication; used by explicit exchange imports.
    pub(crate) fn tm_merge_checked(
        &self,
        project: &str,
        pairs: &[(String, String)],
        expected: &[TmEntry],
    ) -> Result<usize> {
        self.tm_merge_internal(project, pairs, Some(expected))
    }
    fn tm_merge_internal(
        &self,
        project: &str,
        pairs: &[(String, String)],
        expected: Option<&[TmEntry]>,
    ) -> Result<usize> {
        let _guard = self.lock_local_translation(project)?;
        if pairs.len() > 10000 {
            return Err(invalid("导入记忆超过10000条"));
        }
        let mut book = self.load_tm(project)?;
        if expected.is_some_and(|entries| entries != book.entries) {
            return Err(invalid("记忆在预览后已变化，请重新预览"));
        }
        let mut keys: std::collections::BTreeSet<_> = book
            .entries
            .iter()
            .map(|e| (e.source.clone(), e.target.clone()))
            .collect();
        let mut next = book
            .entries
            .iter()
            .filter_map(|e| e.id.strip_prefix("tm-").and_then(|s| s.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        let mut added = 0;
        for (source, target) in pairs {
            Self::check_tm_pair(source, target)?;
            if keys.insert((source.clone(), target.clone())) {
                next = next.checked_add(1).ok_or_else(|| invalid("记忆序号溢出"))?;
                book.entries.push(TmEntry {
                    id: format!("tm-{next}"),
                    source: source.clone(),
                    target: target.clone(),
                    block: String::new(),
                    paragraph: String::new(),
                    adopted: 0,
                });
                added += 1;
            }
        }
        if added > 0 {
            self.save_tm(project, &book)?;
        }
        Ok(added)
    }
    fn check_tm_pair(source: &str, target: &str) -> Result<()> {
        if source.trim().is_empty()
            || target.trim().is_empty()
            || source.len() > 256 * 1024
            || target.len() > 256 * 1024
        {
            return Err(invalid("记忆原文与译文须非空且各不超过256 KiB"));
        }
        Ok(())
    }

    pub fn tm_add(&self, project: &str, source: &str, target: &str) -> Result<TmEntry> {
        let _guard = self.lock_local_translation(project)?;
        self.tm_add_locked(project, source, target)
    }

    fn tm_add_locked(&self, project: &str, source: &str, target: &str) -> Result<TmEntry> {
        Self::check_tm_pair(source, target)?;
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
            .checked_add(1)
            .ok_or_else(|| invalid("记忆序号溢出"))?;
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
        let _guard = self.lock_local_translation(project)?;
        Self::check_tm_pair(source, target)?;
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
        let _guard = self.lock_local_translation(project)?;
        let mut book = self.load_tm(project)?;
        let before = book.entries.len();
        book.entries.retain(|e| e.id != id);
        if book.entries.len() == before {
            return Err(invalid("没有这条翻译记忆"));
        }
        self.save_tm(project, &book)
    }

    pub fn tm_adopt(&self, project: &str, id: &str) -> Result<TmEntry> {
        let _guard = self.lock_local_translation(project)?;
        let mut book = self.load_tm(project)?;
        let entry = book
            .entries
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or_else(|| invalid("没有这条翻译记忆"))?;
        entry.adopted = entry
            .adopted
            .checked_add(1)
            .ok_or_else(|| invalid("采纳次数溢出"))?;
        let out = entry.clone();
        self.save_tm(project, &book)?;
        Ok(out)
    }

    /// Add source/target pairs from existing translations; duplicates are skipped.
    pub fn tm_build(&self, project: &str) -> Result<usize> {
        let _guard = self.lock_local_translation(project)?;
        let project_file = self.project(project)?;
        let session = match crate::Session::open(ziyuan_storage::EventLog::new(self.root())) {
            Ok(session) => session,
            Err(crate::AppError::NotFounded) => return Ok(0),
            Err(error) => return Err(invalid(error.to_string())),
        };
        let finished: std::collections::BTreeSet<_> = session
            .translation_status(self.root(), project)
            .map_err(|e| invalid(e.to_string()))?
            .into_iter()
            .filter(|b| {
                matches!(
                    b.state,
                    super::BlockState::Finalized { .. } | super::BlockState::HumanFinalized { .. }
                )
            })
            .map(|b| b.block)
            .collect();
        let mut book = self.load_tm(project)?;
        let mut next = book
            .entries
            .iter()
            .filter_map(|e| e.id.strip_prefix("tm-").and_then(|s| s.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        let mut keys: std::collections::BTreeSet<_> = book
            .entries
            .iter()
            .map(|e| (e.source.clone(), e.target.clone()))
            .collect();
        let mut added = 0;
        for chapter in &project_file.chapters {
            for block in &chapter.blocks {
                if !finished.contains(&block.block_id) {
                    continue;
                }
                let Some(bytes) = self.read_translation(project, &block.block_id)? else {
                    continue;
                };
                let pairs = match super::read_approved_target(&bytes, &block.block_id)? {
                    super::ApprovedTarget::Plain(b) => b
                        .paragraphs
                        .into_iter()
                        .map(|p| (p.id, p.text))
                        .collect::<Vec<_>>(),
                    super::ApprovedTarget::Marked(b) => {
                        b.paragraphs.into_iter().map(|p| (p.id, p.text)).collect()
                    }
                };
                if pairs.len() != block.paragraphs.len()
                    || !pairs
                        .iter()
                        .map(|(id, _)| id)
                        .eq(block.paragraphs.iter().map(|p| &p.id))
                {
                    return Err(invalid("译稿段数或段号不符，不能建库"));
                }
                for ((_, target), source) in pairs.into_iter().zip(&block.paragraphs) {
                    if source.text.trim().is_empty() || target.trim().is_empty() {
                        continue;
                    }
                    Self::check_tm_pair(&source.text, &target)?;
                    if keys.insert((source.text.clone(), target.clone())) {
                        next = next.checked_add(1).ok_or_else(|| invalid("记忆序号溢出"))?;
                        book.entries.push(TmEntry {
                            id: format!("tm-{next}"),
                            source: source.text.clone(),
                            target,
                            block: block.block_id.clone(),
                            paragraph: source.id.clone(),
                            adopted: 0,
                        });
                        added += 1;
                    }
                }
            }
        }
        if added > 0 {
            self.save_tm(project, &book)?;
        }
        Ok(added)
    }
}
