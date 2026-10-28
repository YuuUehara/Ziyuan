//! 紫垣核心领域模型：官员、官职、诏令与朝廷事件。
mod agent;
mod changes;
mod event;
mod office;
mod records;
mod review;
mod seal;
mod usage;

pub use agent::{Agent, Persona};
pub use changes::{FileChange, Hunk};
pub use event::{CourtEvent, Edict, Nomination, Speech, Subtask, Template};
pub use office::{BudgetScope, Department, GovernanceMode, Office};
pub use records::{Record, find_records, search_records};
pub use review::{
    CHANGELOG, CensorVerdict, Evidence, REPORT_SECTIONS, Threat, Verdict, missing_sections,
};
pub use seal::{Capability, Seal, mcp_grant_covers, valid_mcp_grant};
pub use usage::{Price, TokenUsage};

use serde::{Deserialize, Serialize};
use std::fmt;

/// 字符串到领域类型的解析错误（CLI 参数、事件簿字段）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    UnknownDepartment(String),
    UnknownMode(String),
    UnknownOffice(String),
    MalformedOffice(String),
    UnknownCapability(String),
    UnknownVerdict(String),
    UnknownThreat(String),
}
impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownDepartment(s) => write!(f, "未知部门: {s}"),
            Self::UnknownMode(s) => write!(f, "未知制度: {s}（可用：chancellor/direct/cabinet）"),
            Self::UnknownOffice(s) => write!(f, "未知官职: {s}"),
            Self::MalformedOffice(s) => write!(f, "官职格式无效: {s}"),
            Self::UnknownVerdict(s) => write!(f, "未知评定: {s}（可用：accept/reject）"),
            Self::UnknownThreat(s) => write!(f, "未知可疑类别: {s}"),
            Self::UnknownCapability(s) => {
                write!(
                    f,
                    "未知工具权限: {s}（可用：read_file、search_code、write_file、run_tests）"
                )
            }
        }
    }
}
impl std::error::Error for ParseError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AgentId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TaskId(pub u64);

#[cfg(test)]
mod tests;
