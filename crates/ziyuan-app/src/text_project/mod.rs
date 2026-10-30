//! Standalone offline text projects. No Court, model, session or apply operations.
//!
//! The workspace and project directories must be trusted. Source handles, metadata and
//! bytes are checked before/after import, but portable std filesystem operations cannot
//! prevent a malicious process from swapping ancestor paths or changing/restoring a file
//! between observations. This is consistency checking, not an adversarial filesystem sandbox.
mod model;
mod split;
mod store;
mod validate;

pub use model::*;
pub use split::{chunk_paragraphs, split_paragraphs};
pub use store::TextProjects;
pub use validate::{IssueKind, ValidationIssue, ValidationReport};

pub const DEFAULT_BLOCK_BYTES: usize = 48 * 1024;
pub type Result<T> = std::result::Result<T, TextProjectError>;

#[derive(Debug)]
pub enum TextProjectError {
    Io(std::io::Error),
    Storage(ziyuan_storage::JsonFileError),
    Invalid(String),
    SourceChanged(String),
    Stale(String),
}
impl std::fmt::Display for TextProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::Storage(e) => e.fmt(f),
            Self::Invalid(s) => write!(f, "文本项目: {s}"),
            Self::SourceChanged(s) => write!(f, "导入期间原文已改变: {s}"),
            Self::Stale(s) => write!(f, "原文副本或项目数据已改变: {s}"),
        }
    }
}
impl std::error::Error for TextProjectError {}
impl From<std::io::Error> for TextProjectError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<ziyuan_storage::JsonFileError> for TextProjectError {
    fn from(e: ziyuan_storage::JsonFileError) -> Self {
        Self::Storage(e)
    }
}
impl From<serde_json::Error> for TextProjectError {
    fn from(e: serde_json::Error) -> Self {
        Self::Invalid(e.to_string())
    }
}
fn invalid(message: impl Into<String>) -> TextProjectError {
    TextProjectError::Invalid(message.into())
}
