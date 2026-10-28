//! 应用服务层：CLI 与未来的 UI 共用的用例入口。
//!
//! 每次状态变更都遵循同一流程：治理层提议事件 → 在 `Court` 副本上校验 → 写入事件簿 →
//! 更新内存状态。因此被拒绝的操作不会留下任何事件，写入的事件也一定能被回放。
//!
//! 诏令分两步：[`Session::issue`] 入队（路由不通则拒绝、不留记录），
//! [`Session::run`] 指派并交给 [`Scheduler`](ziyuan_runtime::Scheduler) 并发执行，逐条记录进度与结局。
mod cabinet;
mod censorate;
mod courier;
mod edicts;
mod exam;
mod history;
mod huidian;
mod personnel;
mod reports;
mod run;
mod session;
mod time;
mod vcs;

pub use cabinet::{criteria_note, debate_minutes};
pub use censorate::{ACQUITTAL_NOTE, suspicion_status};
pub use courier::{PullReport, changelog_commits};
pub use edicts::{
    CHANGELOG_NOTE, PROXY_REVIEW_NOTE, changelog_text, render_diffs, reply_text, seal_note,
    selected_files, skipped_note,
};
pub use exam::{ExamPaper, exam_papers};
pub use history::{HistoryEntry, HistoryFilter, describe_event, history_entries};
pub use huidian::HUIDIAN_EVERY;
pub use personnel::{ABOLITION_NOTE, appointment_note, demotion_note, reform_note};
pub use reports::{describe_record, duzhi_report, estimate_cost, kaocheng_grade, kaocheng_report};
pub use run::{EXECUTOR_BUSY, NEGLECT_THRESHOLD, RunUpdate, TaskReport, WatchOptions, WatchUpdate};
pub use time::{format_time, parse_time, tz_offset};
pub use vcs::{VcsCommit, commit_files, commit_message};

use std::fmt;
use std::sync::OnceLock;
use ziyuan_core::CourtEvent;
use ziyuan_governance::{Court, GovernanceError};
use ziyuan_storage::{EventStore, StorageError};

#[derive(Debug)]
pub enum AppError {
    NotFounded,
    AlreadyFounded,
    Governance(GovernanceError),
    Storage(StorageError),
    /// 落盘失败（如文件自呈报以来已被改动）；事件簿不变。
    Apply(String),
    /// 外界（GitHub、git）出错；事件簿不变。
    External(String),
}
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFounded => f.write_str("朝廷尚未建立，请先在项目目录执行 `ziyuan init`"),
            Self::AlreadyFounded => f.write_str("朝廷已存在，拒绝覆盖日志"),
            Self::Governance(e) => e.fmt(f),
            Self::Storage(e) => e.fmt(f),
            Self::Apply(e) => write!(f, "落盘失败：{e}"),
            Self::External(e) => f.write_str(e),
        }
    }
}
impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotFounded | Self::AlreadyFounded | Self::Apply(_) | Self::External(_) => None,
            Self::Governance(e) => Some(e),
            Self::Storage(e) => Some(e),
        }
    }
}
impl From<GovernanceError> for AppError {
    fn from(e: GovernanceError) -> Self {
        Self::Governance(e)
    }
}
impl From<StorageError> for AppError {
    fn from(e: StorageError) -> Self {
        Self::Storage(e)
    }
}

/// 一个已打开的朝廷：由会典与其后的事件（或整本事件簿）重放出的当前状态。
/// 全部事件只在起居注、复核等用到时才读入（见 [`Session::history`]）。
pub struct Session<S: EventStore> {
    store: S,
    court: Court,
    /// 事件簿中已应用的事件条数。
    len: usize,
    /// 最近一条时辰（`CourtEvent::Clock`），据此决定提交时是否另记。
    clock: Option<u64>,
    /// 全部事件，读入后随提交与重载追加。
    history: OnceLock<Vec<CourtEvent>>,
    /// 最近一部会典涵盖的事件条数。
    huidian_at: usize,
    /// 史官记：修会典或弃会典时的记述，由前端取走显示（不入事件簿）。
    chronicle: Option<String>,
}

fn truncate(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().nth(max).is_some() {
        out.push_str("……（下略）");
    }
    out
}

#[cfg(test)]
mod tests;
