//! 翰林院译书：导出材料、上下文快照、朱批、准奏门控、御览与断点续做。
//!
//! 用户可见的材料在 `translations/<项目>/` 下：`context/` 是用户编辑的上下文草稿，
//! `<块>/` 是每次下诏前由程序导出的只读材料（`source.json`、`context.json`、`neighbors.json`，
//! 修订时另有 `zhupi.json`、`reviews.json`），模型只可写 `zh.json` 与 `proposals/`。
//! 已批准的上下文快照与朱批存于 `.ziyuan/text-projects/<项目>/` 下。
//!
//! 哈希一律为 SHA-256 小写十六进制（无前缀）：原文哈希是块 JSON（与项目内
//! `blocks/<块>.json` 逐字节相同，即 [`PromptBlock::serialized`]）的哈希；
//! 译稿哈希是 `zh.json` 文件字节的哈希。
mod context;
mod export;
mod gate;
mod session;
mod status;
mod zhupi;

#[cfg(test)]
mod tests;

pub use context::{
    ContextManifest, ContextSnapshot, GLOSSARY_FILE, GlossaryEntry, ManifestFile, STYLE_FILE,
    SUMMARIES_DIR, parse_glossary,
};
pub use export::Exported;
pub use session::{ResumeOutcome, block_source_hash};
pub use status::{
    BlockFacts, BlockState, BlockStatus, ContextFacts, NextStep, ResumeOptions, StaleReason,
    block_states, resume_plan,
};
pub use zhupi::{BlockZhupi, ZhupiNote};

pub(crate) use gate::check_gate;

use crate::AppError;
use crate::text_project::{Project, PromptBlock, TextProjectError, TextProjects};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use ziyuan_governance::GovernanceError;
use ziyuan_storage::JsonFileError;

pub type Result<T> = std::result::Result<T, TranslationError>;

#[derive(Debug)]
pub enum TranslationError {
    Text(TextProjectError),
    Io(io::Error),
    Storage(JsonFileError),
    Invalid(String),
}
impl fmt::Display for TranslationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(e) => e.fmt(f),
            Self::Io(e) => write!(f, "文件读写失败：{e}"),
            Self::Storage(e) => e.fmt(f),
            Self::Invalid(s) => f.write_str(s),
        }
    }
}
impl std::error::Error for TranslationError {}
impl From<TextProjectError> for TranslationError {
    fn from(e: TextProjectError) -> Self {
        Self::Text(e)
    }
}
impl From<io::Error> for TranslationError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<JsonFileError> for TranslationError {
    fn from(e: JsonFileError) -> Self {
        Self::Storage(e)
    }
}
impl From<serde_json::Error> for TranslationError {
    fn from(e: serde_json::Error) -> Self {
        Self::Invalid(e.to_string())
    }
}
/// 译书的前置条件与材料错误都在事件簿之外：磁盘与存储故障照实报，其余归为翰林院不准（不留记录）。
impl From<TranslationError> for AppError {
    fn from(e: TranslationError) -> Self {
        match e {
            TranslationError::Io(e) => AppError::Storage(ziyuan_storage::StorageError::Io(e)),
            TranslationError::Storage(JsonFileError::Io(e)) => {
                AppError::Storage(ziyuan_storage::StorageError::Io(e))
            }
            other => AppError::Governance(GovernanceError::Translation(other.to_string())),
        }
    }
}

pub(crate) fn invalid(message: impl Into<String>) -> TranslationError {
    TranslationError::Invalid(message.into())
}

/// 译书材料与上下文草稿：`root` 是用户的项目目录（不是 `.ziyuan`）。不需要朝廷即可使用。
#[derive(Clone, Debug)]
pub struct Translations {
    root: PathBuf,
    texts: TextProjects,
}

impl Translations {
    pub fn new(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        Self {
            texts: TextProjects::new(&root),
            root,
        }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn texts(&self) -> &TextProjects {
        &self.texts
    }
    /// 校验过的文本项目（原文与块副本均与记录相符）。
    pub fn project(&self, project: &str) -> Result<Project> {
        Ok(self.texts.show(project)?)
    }
    /// `.ziyuan/text-projects/<项目>/` 目录。
    pub(crate) fn store_dir(&self, project: &str) -> Result<PathBuf> {
        Ok(self.texts.project_dir(project)?)
    }
    /// 块材料目录 `translations/<项目>/<块>/` 的绝对路径（不建目录）。
    pub fn block_dir(&self, project: &str, block: &str) -> PathBuf {
        self.root.join(block_dir_rel(project, block))
    }
    /// 磁盘上的译稿 `zh.json`；不存在为 `None`，符号链接或非普通文件报错。
    pub fn read_translation(&self, project: &str, block: &str) -> Result<Option<Vec<u8>>> {
        read_plain_opt(&self.root.join(output_rel(project, block)))
    }
}

/// 块材料目录（项目内相对路径，`/` 分隔）。
pub fn block_dir_rel(project: &str, block: &str) -> String {
    format!("translations/{project}/{block}")
}
/// 译稿 `zh.json` 的项目内相对路径。
pub fn output_rel(project: &str, block: &str) -> String {
    format!("translations/{project}/{block}/zh.json")
}
/// 建议目录 `proposals/` 的项目内相对路径前缀（含末尾 `/`）。
pub fn proposals_rel(project: &str, block: &str) -> String {
    format!("translations/{project}/{block}/proposals/")
}
/// 上下文草稿目录（项目内相对路径）。
pub fn context_dir_rel(project: &str) -> String {
    format!("translations/{project}/context")
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 全书按章、块顺序排列的块：(章 ID, 块)。
pub(crate) fn all_blocks(project: &Project) -> Vec<(&str, &PromptBlock)> {
    project
        .chapters
        .iter()
        .flat_map(|c| c.blocks.iter().map(move |b| (c.id.as_str(), b)))
        .collect()
}

/// 块在全书中的位置与所属章；不属于本项目时报错。
pub(crate) fn find_block<'a>(
    project: &'a Project,
    block: &str,
) -> Result<(usize, &'a str, &'a PromptBlock)> {
    all_blocks(project)
        .into_iter()
        .enumerate()
        .find(|(_, (_, b))| b.block_id == block)
        .map(|(i, (c, b))| (i, c, b))
        .ok_or_else(|| invalid(format!("项目 {} 没有块 {block}", project.id)))
}

/// 自 `root` 起逐级确认（缺则建）普通目录，拒绝符号链接与非目录，返回最末一级。
pub(crate) fn ensure_plain_dirs(root: &Path, parts: &[&str]) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for part in parts {
        if part.is_empty() || *part == "." || *part == ".." || part.contains(['/', '\\']) {
            return Err(invalid(format!("路径成分不合法：{part:?}")));
        }
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(invalid(format!(
                    "{} 不是普通目录（拒绝符号链接）",
                    path.display()
                )));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => match fs::create_dir(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let m = fs::symlink_metadata(&path)?;
                    if !m.is_dir() || m.file_type().is_symlink() {
                        return Err(invalid(format!("{} 不是普通目录", path.display())));
                    }
                }
                Err(e) => return Err(e.into()),
            },
            Err(e) => return Err(e.into()),
        }
    }
    Ok(path)
}

/// 读普通文件；不存在为 `None`，符号链接或非普通文件报错。
pub(crate) fn read_plain_opt(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
        Ok(m) if !m.is_file() || m.file_type().is_symlink() => Err(invalid(format!(
            "{} 不是普通文件（拒绝符号链接）",
            path.display()
        ))),
        Ok(_) => Ok(Some(fs::read(path)?)),
    }
}

/// 写普通文件（已存在的须为普通文件）；内容未变时不写。
/// 先写同目录临时文件、刷盘，再改名覆盖：崩溃不会留下写了一半的草稿。
pub(crate) fn write_plain(path: &Path, bytes: &[u8]) -> Result<()> {
    if read_plain_opt(path)?.as_deref() == Some(bytes) {
        return Ok(());
    }
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().ok_or_else(|| invalid("路径没有上级目录"))?;
    let name = path.file_name().ok_or_else(|| invalid("路径没有文件名"))?;
    let (tmp, mut file) = loop {
        let mut tmp_name = std::ffi::OsString::from(".");
        tmp_name.push(name);
        tmp_name.push(format!(
            ".{}.{}.tmp",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let tmp = dir.join(tmp_name);
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(file) => break (tmp, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    };
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    match written.and_then(|()| fs::rename(&tmp, path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e.into())
        }
    }
}

/// 草稿目录的排他锁：读改写期间不让另一个进程同时改。超出 [`LOCK_TIMEOUT`] 即报错。
pub(crate) struct DirLock {
    _file: fs::File,
}

const LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(10);

impl DirLock {
    /// 锁 `dir/.lock`；锁文件本身留在目录里（无内容）。
    pub(crate) fn acquire(dir: &Path) -> Result<Self> {
        let path = dir.join(".lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        let started = std::time::Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(fs::TryLockError::WouldBlock) => {}
                Err(fs::TryLockError::Error(e)) => return Err(e.into()),
            }
            let waited = started.elapsed();
            if waited >= LOCK_TIMEOUT {
                return Err(invalid(format!(
                    "{} 被占用超过 {} 秒",
                    path.display(),
                    LOCK_TIMEOUT.as_secs()
                )));
            }
            std::thread::sleep(LOCK_POLL.min(LOCK_TIMEOUT - waited));
        }
    }
}

/// 章 ID 的排序键：取 `c` 后的数字；不合格式的按最大，排在最后。
pub(crate) fn chapter_rank(id: &str) -> u64 {
    id.strip_prefix('c')
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(u64::MAX)
}

/// 读 UTF-8 文本（不存在为 `None`），超过 `limit` 字节报错。
pub(crate) fn read_text_opt(path: &Path, what: &str, limit: usize) -> Result<Option<String>> {
    let Some(bytes) = read_plain_opt(path)? else {
        return Ok(None);
    };
    if bytes.len() > limit {
        return Err(invalid(format!("{what} 超过 {limit} 字节上限")));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| invalid(format!("{what} 不是 UTF-8 文本")))
}

pub(crate) fn pretty_json(value: &impl serde::Serialize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}
