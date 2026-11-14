//! 译稿版本快照：草稿、准奏、定稿三种状态自动留存，可人工补存与比较，并有保留期限。
//!
//! 快照只写 `.ziyuan/text-projects/<项目>/versions/<块>/`，从不改 `zh.json`、草稿或事件簿；
//! 它既不是批准凭据，也不是导出资格。自动快照是辅助材料：写入失败由调用方忽略，绝不回滚
//! 已经完成的准奏或定稿。
use super::{
    Result, TargetMark, Translations, find_block, invalid, pretty_json, read_approved_target,
};
use crate::text_project::{Project, PromptBlock};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;

/// 快照文件格式版本；旧的手工快照没有 `format` 字段，按第一版纯文本次要格式读取。
pub const VERSION_FILE_FORMAT: u32 = 1;
/// 每种自动状态默认保留的最新份数；人工快照不受自动清理影响。
pub const AUTO_KEEP_PER_STATE: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionState {
    Draft,
    Approved,
    Finalized,
    Manual,
}

impl VersionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Approved => "approved",
            Self::Finalized => "finalized",
            Self::Manual => "manual",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Draft => "草稿",
            Self::Approved => "准奏",
            Self::Finalized => "定稿",
            Self::Manual => "手工",
        }
    }
    pub fn is_auto(self) -> bool {
        !matches!(self, Self::Manual)
    }
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "draft" => Self::Draft,
            "approved" => Self::Approved,
            "finalized" => Self::Finalized,
            "manual" => Self::Manual,
            _ => return None,
        })
    }
}

/// 一份快照的身份。目录里所有可读快照都按时间从早到晚排序。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VersionInfo {
    pub id: String,
    pub state: VersionState,
    /// 人工标签；自动快照为空。
    pub label: String,
    pub created_ms: u64,
    /// 快照正文（规范化段落）的 SHA-256。
    pub hash: String,
}

/// 自动快照的结果；`draft_changes` 是与最新草稿快照的逐段比较。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AutoSnapshot {
    pub info: VersionInfo,
    /// 是否存在可比较的草稿快照。
    pub compared_with_draft: bool,
    pub draft_changes: Vec<super::VersionChange>,
}

/// 快照正文：只保留逐段 id、译文与译文标记，不复制原文或上下文。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionFile {
    pub format: u32,
    pub state: VersionState,
    pub block: String,
    pub created_ms: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    pub paragraphs: Vec<VersionParagraph>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionParagraph {
    pub id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<TargetMark>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PruneReport {
    /// 预览中本会删除、或本次确实删除的快照 ID（按时间从早到晚）。
    pub candidates: Vec<String>,
    pub deleted: Vec<String>,
    pub kept: usize,
    /// 为真时只预览，不动文件。
    pub dry_run: bool,
    pub include_manual: bool,
    pub keep_per_state: usize,
}

fn safe_label(label: &str) -> String {
    label
        .chars()
        .filter_map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                Some(c)
            } else if c.is_whitespace() {
                Some('_')
            } else {
                None
            }
        })
        .take(40)
        .collect()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 文件名 `<毫秒>-<状态>[-<标签>].json`；旧快照为 `<毫秒>-<标签>.json`。
fn parse_id(id: &str) -> Option<(u64, VersionState, String)> {
    let stem = id.strip_suffix(".json")?;
    let (stamp, rest) = stem.split_once('-')?;
    let created = stamp.parse().ok()?;
    let (state, label) = match rest.split_once('-') {
        Some((head, tail)) if VersionState::parse(head).is_some() => {
            (VersionState::parse(head)?, tail.to_owned())
        }
        _ => (
            VersionState::parse(rest).unwrap_or(VersionState::Manual),
            if VersionState::parse(rest).is_some() {
                String::new()
            } else {
                rest.to_owned()
            },
        ),
    };
    Some((created, state, label))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 160
        && id.ends_with(".json")
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        && !id.contains("..")
}

fn version_file(block: &str, state: VersionState, label: &str, created_ms: u64) -> VersionFile {
    VersionFile {
        format: VERSION_FILE_FORMAT,
        state,
        block: block.to_owned(),
        created_ms,
        label: label.to_owned(),
        paragraphs: Vec::new(),
    }
}

/// 把 `zh.json` 的批准目标（纯文本或带标记）转成快照正文。
fn from_target(
    block: &str,
    state: VersionState,
    bytes: &[u8],
    created_ms: u64,
) -> Result<VersionFile> {
    let target = read_approved_target(bytes, block)?;
    let mut file = version_file(block, state, "", created_ms);
    match target {
        super::ApprovedTarget::Plain(prompt) => {
            file.paragraphs = prompt
                .paragraphs
                .into_iter()
                .map(|p| VersionParagraph {
                    id: p.id,
                    text: p.text,
                    marks: Vec::new(),
                })
                .collect();
        }
        super::ApprovedTarget::Marked(candidate) => {
            file.paragraphs = candidate
                .paragraphs
                .into_iter()
                .map(|p| VersionParagraph {
                    id: p.id,
                    text: p.text,
                    marks: p.marks,
                })
                .collect();
        }
    }
    Ok(file)
}

fn from_prompt(
    block: &str,
    state: VersionState,
    prompt: &PromptBlock,
    created_ms: u64,
) -> VersionFile {
    let mut file = version_file(block, state, "", created_ms);
    file.paragraphs = prompt
        .paragraphs
        .iter()
        .map(|p| VersionParagraph {
            id: p.id.clone(),
            text: p.text.clone(),
            marks: Vec::new(),
        })
        .collect();
    file
}

fn content_hash(file: &VersionFile) -> Result<String> {
    Ok(super::sha256_hex(&serde_json::to_vec(&file.paragraphs)?))
}

impl VersionFile {
    pub(crate) fn into_parts(self) -> Vec<(String, String, Vec<TargetMark>)> {
        self.paragraphs
            .into_iter()
            .map(|p| (p.id, p.text, p.marks))
            .collect()
    }
}

/// 读取旧的第一版手工快照（原始 `PromptBlock` JSON）。
fn legacy_prompt(bytes: &[u8], block: &str) -> Result<PromptBlock> {
    let prompt: PromptBlock = serde_json::from_slice(bytes)
        .map_err(|e| invalid(format!("快照不是有效 PromptBlock JSON：{e}")))?;
    if prompt.block_id != block {
        return Err(invalid(format!(
            "快照块 ID {} 与目标 {block} 不符",
            prompt.block_id
        )));
    }
    Ok(prompt)
}

pub(crate) fn parse_snapshot(bytes: &[u8], block: &str) -> Result<VersionFile> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| invalid(format!("快照不是有效 JSON：{e}")))?;
    if value.get("format").is_none() {
        let prompt = legacy_prompt(bytes, block)?;
        return Ok(from_prompt(block, VersionState::Manual, &prompt, 0));
    }
    let file: VersionFile =
        serde_json::from_slice(bytes).map_err(|e| invalid(format!("快照格式不对：{e}")))?;
    if file.format != VERSION_FILE_FORMAT {
        return Err(invalid(format!(
            "快照格式第 {} 版本版读不了，请升级紫垣",
            file.format
        )));
    }
    if file.block != block {
        return Err(invalid(format!(
            "快照块 ID {} 与目标 {block} 不符",
            file.block
        )));
    }
    Ok(file)
}

impl Translations {
    fn versions_dir_in(&self, project: &Project, block: &str) -> Result<PathBuf> {
        // 先核对项目与块确实存在，再拿它们作路径成分。
        find_block(project, block)?;
        Ok(self
            .root
            .join(".ziyuan")
            .join("text-projects")
            .join(&project.id)
            .join("versions")
            .join(block))
    }

    fn version_path_in(&self, project: &Project, block: &str, id: &str) -> Result<PathBuf> {
        if !valid_id(id) {
            return Err(invalid("版本 ID 不合法"));
        }
        Ok(self.versions_dir_in(project, block)?.join(id))
    }

    fn version_path(&self, project: &str, block: &str, id: &str) -> Result<PathBuf> {
        let p = self.project(project)?;
        self.version_path_in(&p, block, id)
    }

    /// 列出快照，按时间从早到晚；损坏或未知文件跳过，不阻止其余快照可读。
    pub fn list_translation_versions(
        &self,
        project: &str,
        block: &str,
    ) -> Result<Vec<VersionInfo>> {
        let p = self.project(project)?;
        self.list_translation_versions_in(&p, block)
    }

    /// 按已校验的 [`Project`] 列出快照。
    pub fn list_translation_versions_in(
        &self,
        project: &Project,
        block: &str,
    ) -> Result<Vec<VersionInfo>> {
        let dir = self.versions_dir_in(project, block)?;
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if !valid_id(&name) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let Ok(bytes) = std::fs::read(entry.path()) else {
                continue;
            };
            let Ok(file) = parse_snapshot(&bytes, block) else {
                continue;
            };
            let Ok(hash) = content_hash(&file) else {
                continue;
            };
            let (created_ms, _, fallback_label) = match parse_id(&name) {
                Some(parts) => parts,
                None => continue,
            };
            let label = if file.label.is_empty() {
                fallback_label
            } else {
                file.label.clone()
            };
            out.push(VersionInfo {
                id: name,
                state: file.state,
                label,
                created_ms,
                hash,
            });
        }
        out.sort_by(|a, b| (a.created_ms, &a.id).cmp(&(b.created_ms, &b.id)));
        Ok(out)
    }

    fn write_snapshot(
        &self,
        project: &str,
        block: &str,
        file: &VersionFile,
    ) -> Result<VersionInfo> {
        // 目录成分先由项目与块核对过，拒绝符号链接与非普通目录。
        let dir = super::ensure_plain_dirs(
            &self.root,
            &[".ziyuan", "text-projects", project, "versions", block],
        )?;
        let label = safe_label(&file.label);
        let suffix = if label.is_empty() {
            String::new()
        } else {
            format!("-{label}")
        };
        let mut stamp = now_ms();
        let path = loop {
            let id = format!("{stamp}-{}{suffix}.json", file.state.as_str());
            let path = dir.join(&id);
            match std::fs::symlink_metadata(&path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => break path,
                Err(e) => return Err(e.into()),
                Ok(_) => stamp += 1,
            }
        };
        let id = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| invalid("版本文件名不合法"))?
            .to_owned();
        let (_, state, _) = parse_id(&id).ok_or_else(|| invalid("版本 ID 不合法"))?;
        let mut stored = file.clone();
        stored.created_ms = stamp;
        let bytes = pretty_json(&stored)?;
        super::write_plain(&path, &bytes)?;
        Ok(VersionInfo {
            id,
            state,
            label: file.label.clone(),
            created_ms: stamp,
            hash: content_hash(&stored)?,
        })
    }

    /// 人工保存当前 `zh.json` 为快照。没有译稿时拒绝，绝不拿草稿冒充。
    pub fn snapshot_translation(
        &self,
        project: &str,
        block: &str,
        label: &str,
    ) -> Result<VersionInfo> {
        find_block(&self.project(project)?, block)?;
        let bytes = self
            .read_translation(project, block)?
            .ok_or_else(|| invalid("当前块没有译稿，不能存快照"))?;
        let mut file = from_target(block, VersionState::Manual, &bytes, now_ms())?;
        file.label = if label.trim().is_empty() {
            "manual".into()
        } else {
            label.trim().to_owned()
        };
        self.write_snapshot(project, block, &file)
    }

    /// 同状态且正文未变时不重复写；返回现有或新写的快照身份。
    fn store_if_changed(
        &self,
        project: &str,
        block: &str,
        file: VersionFile,
    ) -> Result<Option<VersionInfo>> {
        let hash = content_hash(&file)?;
        let existing = self.list_translation_versions(project, block)?;
        if let Some(last) = existing
            .iter()
            .rev()
            .find(|info| info.state == file.state)
            .filter(|info| info.hash == hash)
        {
            return Ok(Some(last.clone()));
        }
        let info = self.write_snapshot(project, block, &file)?;
        // 自动快照按状态保留最新数份；人工快照一律保留。
        if info.state.is_auto() {
            let _ = self.prune_versions(project, block, AUTO_KEEP_PER_STATE, false, true);
        }
        Ok(Some(info))
    }

    /// 把一段译稿正文（`PromptBlock` JSON）存为指定状态的快照；用于模型准奏前的候选稿。
    pub(crate) fn snapshot_translation_content(
        &self,
        project: &str,
        block: &str,
        bytes: &[u8],
        state: VersionState,
    ) -> Result<Option<VersionInfo>> {
        find_block(&self.project(project)?, block)?;
        let prompt = legacy_prompt(bytes, block)?;
        let file = from_prompt(block, state, &prompt, now_ms());
        self.store_if_changed(project, block, file)
    }

    /// 自动快照：草稿取人工草稿，准奏／定稿取磁盘上的 `zh.json`。
    /// 同状态且正文未变时不重复写。
    pub(crate) fn snapshot_state(
        &self,
        project: &str,
        block: &str,
        state: VersionState,
    ) -> Result<Option<VersionInfo>> {
        let p = self.project(project)?;
        self.snapshot_state_in(&p, block, state)
    }

    pub(crate) fn snapshot_state_in(
        &self,
        project: &Project,
        block: &str,
        state: VersionState,
    ) -> Result<Option<VersionInfo>> {
        let file = match state {
            VersionState::Draft => {
                let Some(draft) = self
                    .peek_human_draft_in(project, block)?
                    .draft
                    .filter(|d| d.paragraphs.iter().any(|p| !p.text.trim().is_empty()))
                else {
                    return Ok(None);
                };
                let mut file = version_file(block, state, "", now_ms());
                file.paragraphs = draft
                    .paragraphs
                    .into_iter()
                    .map(|p| VersionParagraph {
                        id: p.id,
                        text: p.text,
                        marks: p.marks,
                    })
                    .collect();
                file
            }
            _ => {
                let Some(bytes) = self.read_translation(&project.id, block)? else {
                    return Ok(None);
                };
                from_target(block, state, &bytes, now_ms())?
            }
        };
        self.store_if_changed(&project.id, block, file)
    }

    /// 自动快照并附上与最新草稿快照的比较。写入失败返回 `None`，不影响已完成的操作。
    pub fn auto_snapshot(
        &self,
        project: &str,
        block: &str,
        state: VersionState,
    ) -> Option<AutoSnapshot> {
        let info = self.snapshot_state(project, block, state).ok().flatten()?;
        let versions = self.list_translation_versions(project, block).ok()?;
        let draft = versions
            .iter()
            .rev()
            .find(|candidate| candidate.state == VersionState::Draft)?;
        let old = self
            .read_translation_version(project, block, &draft.id)
            .ok()?;
        let old_file = parse_snapshot(&old, block).ok()?;
        let new = self
            .read_translation_version(project, block, &info.id)
            .ok()?;
        let new_file = parse_snapshot(&new, block).ok()?;
        let draft_changes =
            super::compare::compare_paragraphs(old_file.into_parts(), new_file.into_parts());
        Some(AutoSnapshot {
            info,
            compared_with_draft: true,
            draft_changes,
        })
    }

    /// 读取一份快照的原始字节；仍按普通文件与安全 ID 校验。
    pub fn read_translation_version(
        &self,
        project: &str,
        block: &str,
        id: &str,
    ) -> Result<Vec<u8>> {
        let path = self.version_path(project, block, id)?;
        super::read_plain_opt(&path)?
            .ok_or_else(|| invalid(format!("没有版本 {id}，请先运行 ziyuan text version list")))
    }

    /// 按已校验的 [`Project`] 读取一份快照的原始字节。
    pub fn read_translation_version_in(
        &self,
        project: &Project,
        block: &str,
        id: &str,
    ) -> Result<Vec<u8>> {
        let path = self.version_path_in(project, block, id)?;
        super::read_plain_opt(&path)?
            .ok_or_else(|| invalid(format!("没有版本 {id}，请先运行 ziyuan text version list")))
    }

    /// 版本清理：每个状态保留最新 `keep` 份；默认不动人工快照。
    /// `apply` 为假时只预览。绝不删除未知文件、符号链接或全部快照。
    pub fn prune_versions(
        &self,
        project: &str,
        block: &str,
        keep: usize,
        include_manual: bool,
        apply: bool,
    ) -> Result<PruneReport> {
        let keep = keep.max(1);
        let versions = self.list_translation_versions(project, block)?;
        let mut keep_ids = std::collections::BTreeSet::new();
        for state in [
            VersionState::Draft,
            VersionState::Approved,
            VersionState::Finalized,
            VersionState::Manual,
        ] {
            if state == VersionState::Manual && !include_manual {
                for info in versions.iter().filter(|i| i.state == VersionState::Manual) {
                    keep_ids.insert(info.id.clone());
                }
                continue;
            }
            for info in versions
                .iter()
                .filter(|i| i.state == state)
                .rev()
                .take(keep)
            {
                keep_ids.insert(info.id.clone());
            }
        }
        let mut candidates: Vec<String> = versions
            .iter()
            .filter(|info| !keep_ids.contains(&info.id))
            .map(|info| info.id.clone())
            .collect();
        candidates.sort_by_key(|id| parse_id(id).map(|p| p.0).unwrap_or(0));
        let mut deleted = Vec::new();
        if apply {
            for id in &candidates {
                let path = self.version_path(project, block, id)?;
                match std::fs::symlink_metadata(&path) {
                    Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {}
                    _ => continue,
                }
                std::fs::remove_file(&path)?;
                deleted.push(id.clone());
            }
        }
        let kept = versions.len()
            - if apply {
                deleted.len()
            } else {
                candidates.len()
            };
        Ok(PruneReport {
            candidates,
            deleted,
            kept,
            dry_run: !apply,
            include_manual,
            keep_per_state: keep,
        })
    }

    /// 是否有任何快照；供 Web 端省一次目录读取。
    pub fn has_versions(&self, project: &str, block: &str) -> Result<bool> {
        Ok(!self.list_translation_versions(project, block)?.is_empty())
    }
}
