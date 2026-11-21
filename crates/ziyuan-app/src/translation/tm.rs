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

    /// 段旁实时译文记忆推荐（纯只读）：按当前段原文在本地 `tm.json` 中检索最相近的参考译文。
    ///
    /// 绝不写盘，绝不自动覆盖译稿；同块同段自身条目会被自动跳过。
    pub fn suggest_tm_for_paragraph(
        &self,
        project: &str,
        block: &str,
        paragraph: &str,
        limit: usize,
    ) -> Result<ParagraphTmSuggestions> {
        if !(1..=10).contains(&limit) {
            return Err(invalid("推荐条数须在 1 至 10 之间"));
        }
        let p = self.project(project)?;
        let (_, _, prompt) = super::find_block(&p, block)?;
        let source_para = prompt
            .paragraphs
            .iter()
            .find(|para| para.id == paragraph)
            .ok_or_else(|| invalid("段号不属于当前块"))?;
        if source_para.text.trim().is_empty() {
            return Ok(ParagraphTmSuggestions {
                project: project.into(),
                block: block.into(),
                paragraph: paragraph.into(),
                source: source_para.text.clone(),
                suggestions: Vec::new(),
            });
        }
        let entries = self.tm_list(project)?;
        let mut hits = Vec::new();
        for entry in entries {
            if entry.block == block && entry.paragraph == paragraph {
                continue;
            }
            let score = score_tm_similarity(&source_para.text, &entry.source);
            if score >= 25 {
                hits.push(TmSuggestion {
                    source: entry.source,
                    target: entry.target,
                    score,
                    origin: "tm".into(),
                    origin_block: (!entry.block.is_empty()).then_some(entry.block),
                    origin_paragraph: (!entry.paragraph.is_empty()).then_some(entry.paragraph),
                    tm_id: Some(entry.id),
                });
            }
        }
        let suggestions = merge_tm_suggestions(hits, Vec::new(), limit);
        Ok(ParagraphTmSuggestions {
            project: project.into(),
            block: block.into(),
            paragraph: paragraph.into(),
            source: source_para.text.clone(),
            suggestions,
        })
    }
}

/// 单条段旁译文记忆推荐（只读参考，不构成自动替换或指令）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TmSuggestion {
    pub source: String,
    pub target: String,
    /// 0..=100 相似度分值（仅用于排序与展示，不代表语义等价）。
    pub score: u32,
    /// `"tm"`（本地翻译记忆）或 `"memory"`（定稿向量记忆）。
    pub origin: String,
    pub origin_block: Option<String>,
    pub origin_paragraph: Option<String>,
    pub tm_id: Option<String>,
}

/// 指定段落的实时译文记忆推荐结果（纯只读）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ParagraphTmSuggestions {
    pub project: String,
    pub block: String,
    pub paragraph: String,
    pub source: String,
    pub suggestions: Vec<TmSuggestion>,
}

fn normalize_for_tm(text: &str) -> String {
    let mut out = String::new();
    let mut prev_ws = false;
    for ch in text.trim().chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            prev_ws = false;
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
        }
    }
    out
}

/// 确定性字符双元组 Dice 相似度（0..=100），兼顾中日韩与拉丁语段。
pub fn score_tm_similarity(query: &str, candidate: &str) -> u32 {
    let a = normalize_for_tm(query);
    let b = normalize_for_tm(candidate);
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    if a == b {
        return 100;
    }
    let ac: Vec<char> = a.chars().collect();
    let bc: Vec<char> = b.chars().collect();
    let dice = if ac.len() < 2 || bc.len() < 2 {
        let mut set_b: std::collections::BTreeMap<char, usize> = std::collections::BTreeMap::new();
        for &c in &bc {
            *set_b.entry(c).or_default() += 1;
        }
        let mut overlap = 0usize;
        for &c in &ac {
            if let Some(n) = set_b.get_mut(&c)
                && *n > 0
            {
                *n -= 1;
                overlap += 1;
            }
        }
        ((2 * overlap * 100) / (ac.len() + bc.len())).min(99) as u32
    } else {
        let mut counts: std::collections::BTreeMap<(char, char), usize> =
            std::collections::BTreeMap::new();
        for w in bc.windows(2) {
            *counts.entry((w[0], w[1])).or_default() += 1;
        }
        let mut overlap = 0usize;
        for w in ac.windows(2) {
            if let Some(n) = counts.get_mut(&(w[0], w[1]))
                && *n > 0
            {
                *n -= 1;
                overlap += 1;
            }
        }
        let total = (ac.len() - 1) + (bc.len() - 1);
        ((2 * overlap * 100) / total).min(99) as u32
    };
    let shorter = ac.len().min(bc.len());
    let longer = ac.len().max(bc.len());
    let substr_bonus = if shorter >= 2 && (a.contains(&b) || b.contains(&a)) {
        (55 + (35 * shorter) / longer).min(96) as u32
    } else {
        0
    };
    dice.max(substr_bonus).min(99)
}

/// 合并并去重多来源（本地 `tm.json` 与定稿 `ziyuan-memory`）的译文记忆推荐。
pub fn merge_tm_suggestions(
    base: Vec<TmSuggestion>,
    extra: Vec<TmSuggestion>,
    limit: usize,
) -> Vec<TmSuggestion> {
    let mut best: std::collections::BTreeMap<(String, String), TmSuggestion> =
        std::collections::BTreeMap::new();
    for item in base.into_iter().chain(extra) {
        let key = (item.source.clone(), item.target.clone());
        match best.get(&key) {
            Some(existing) if existing.score >= item.score => {}
            _ => {
                best.insert(key, item);
            }
        }
    }
    let mut out: Vec<TmSuggestion> = best.into_values().collect();
    out.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.origin.cmp(&b.origin))
            .then_with(|| a.tm_id.cmp(&b.tm_id))
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.target.cmp(&b.target))
    });
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggest_tm_for_paragraph_scores_ranks_excludes_self_and_merges() {
        let dir = std::env::temp_dir().join(format!(
            "ziyuan-tm-suggest-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let tp = crate::text_project::TextProjects::new(&dir);
        let meta = tp
            .create(crate::text_project::NewProject {
                title: "译文记忆推荐测试".into(),
                source_language: "ja".into(),
                target_language: "zh-CN".into(),
                provenance: crate::text_project::Provenance {
                    source: "local".into(),
                    rights: "owned".into(),
                    author: String::new(),
                    version: String::new(),
                    date: String::new(),
                    scope: String::new(),
                },
            })
            .unwrap();
        let ch = dir.join("ch1.txt");
        std::fs::write(
            &ch,
            "王都の夜はとても静かだった。

別の話。",
        )
        .unwrap();
        let project = tp.import(&meta.id, &[ch]).unwrap();
        let block = &project.chapters[0].blocks[0];
        let p0 = &block.paragraphs[0].id;
        let t = Translations::new(&dir);

        t.tm_add(
            &meta.id,
            "王都の夜はとても静かだった。",
            "王都的夜晚格外宁静。",
        )
        .unwrap();
        t.tm_add(&meta.id, "王都の夜は静かだった。", "王都之夜很安静。")
            .unwrap();
        t.tm_add(
            &meta.id,
            "Completely unrelated sentence.",
            "完全无关的句子。",
        )
        .unwrap();
        let mut book = t.load_tm(&meta.id).unwrap();
        book.entries.push(TmEntry {
            id: "tm-self".into(),
            source: "王都の夜はとても静かだった。".into(),
            target: "自引用不应出现。".into(),
            block: block.block_id.clone(),
            paragraph: p0.clone(),
            adopted: 0,
        });
        t.save_tm(&meta.id, &book).unwrap();

        let res = t
            .suggest_tm_for_paragraph(&meta.id, &block.block_id, p0, 5)
            .unwrap();
        assert_eq!(res.suggestions.len(), 2);
        assert_eq!(res.suggestions[0].score, 100);
        assert_eq!(res.suggestions[0].target, "王都的夜晚格外宁静。");
        assert!(res.suggestions[1].score >= 50 && res.suggestions[1].score < 100);
        assert!(
            res.suggestions
                .iter()
                .all(|s| s.target != "自引用不应出现。")
        );

        let merged = merge_tm_suggestions(
            res.suggestions.clone(),
            vec![
                TmSuggestion {
                    source: "王都の夜は静かだった。".into(),
                    target: "王都之夜很安静。".into(),
                    score: 95,
                    origin: "memory".into(),
                    origin_block: Some("c0001-b0002".into()),
                    origin_paragraph: Some("c0001-p0009".into()),
                    tm_id: None,
                },
                TmSuggestion {
                    source: "王都の夜".into(),
                    target: "王都之夜".into(),
                    score: 60,
                    origin: "memory".into(),
                    origin_block: None,
                    origin_paragraph: None,
                    tm_id: None,
                },
            ],
            2,
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].score, 100);
        assert_eq!(merged[1].score, 95);
        assert_eq!(merged[1].origin, "memory");

        assert!(
            t.suggest_tm_for_paragraph(&meta.id, &block.block_id, "no-such-para", 5)
                .is_err()
        );
        assert!(
            t.suggest_tm_for_paragraph(&meta.id, &block.block_id, p0, 0)
                .is_err()
        );
        assert!(
            t.suggest_tm_for_paragraph(&meta.id, &block.block_id, p0, 11)
                .is_err()
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
