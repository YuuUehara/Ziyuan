use crate::text_project::TextProjectError;
use crate::translation::{StructuredGlossary, TranslationError};
use serde::{Deserialize, Serialize};
use std::{fmt, io};
use ziyuan_storage::{JsonFileError, WorkspaceError};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialSave {
    pub request_id: String,
    pub expected_revision: Option<String>,
    pub material: StructuredGlossary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialDraft {
    pub project: String,
    pub revision: String,
    pub material: StructuredGlossary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialReceipt {
    pub revision: String,
    pub content_hash: String,
}

/// A field name or array index relative to the material root (not a parsed error message);
/// save-request fields such as `request_id` appear as a single top-level field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MaterialPathSegment {
    Field(String),
    Index(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialIssueCode {
    Shape,
    Required,
    Id,
    Reference,
    Position,
    Evidence,
    Limit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialIssue {
    pub path: Vec<MaterialPathSegment>,
    pub code: MaterialIssueCode,
    pub message: String,
}

impl fmt::Display for MaterialIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for MaterialIssue {}

/// Bound to the exact compact material and serialized verified project.
/// This is neither a save receipt nor approval; at most the first issue is returned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialValidation {
    pub valid: bool,
    pub issues: Vec<MaterialIssue>,
    pub content_hash: String,
    pub project_hash: String,
}

/// Separate from human translation draft errors. Preview variants reserve their
/// shared protocol vocabulary without implementing the operation.
#[derive(Debug)]
pub enum MaterialError {
    Issue(MaterialIssue),
    Io(io::Error),
    Storage(JsonFileError),
    Workspace(WorkspaceError),
    Project(TextProjectError),
    Conflict { current: Option<String> },
    RequestConflict,
    ProjectChanged,
}

impl fmt::Display for MaterialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Issue(issue) => issue.fmt(f),
            Self::Io(error) => write!(f, "资料文件读写失败：{error}"),
            Self::Storage(error) => error.fmt(f),
            Self::Workspace(error) => error.fmt(f),
            Self::Project(error) => error.fmt(f),
            Self::Conflict { current } => write!(
                f,
                "资料草稿修订冲突，现行修订为 {current:?}；请保留输入并重新读取"
            ),
            Self::RequestConflict => f.write_str("资料草稿请求身份已被不同内容使用"),
            Self::ProjectChanged => f.write_str("资料所依据的项目已变化，请重新读取并核对"),
        }
    }
}
impl std::error::Error for MaterialError {}
impl From<MaterialIssue> for MaterialError {
    fn from(issue: MaterialIssue) -> Self {
        Self::Issue(issue)
    }
}
impl From<io::Error> for MaterialError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<JsonFileError> for MaterialError {
    fn from(error: JsonFileError) -> Self {
        Self::Storage(error)
    }
}
impl From<WorkspaceError> for MaterialError {
    fn from(error: WorkspaceError) -> Self {
        Self::Workspace(error)
    }
}
/// Shared path helpers report plain-path refusals as `Invalid`; those surface as
/// `InvalidInput` I/O errors rather than being mistaken for material issues.
impl From<TranslationError> for MaterialError {
    fn from(error: TranslationError) -> Self {
        match error {
            TranslationError::Text(error) => Self::Project(error),
            TranslationError::Io(error) => Self::Io(error),
            TranslationError::Storage(error) => Self::Storage(error),
            other => Self::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                other.to_string(),
            )),
        }
    }
}
impl From<TextProjectError> for MaterialError {
    fn from(error: TextProjectError) -> Self {
        Self::Project(error)
    }
}
