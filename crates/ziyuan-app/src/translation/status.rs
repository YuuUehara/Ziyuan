//! 每块的译书状态：由事件回放与磁盘实际译稿得出（纯计算，便于单测），及续做计划。
use super::{chapter_rank, sha256_hex};
use crate::edicts::selected_files;
use serde::Serialize;
use std::collections::BTreeMap;
use ziyuan_core::{CourtEvent, TaskId, TranslationKind};
use ziyuan_governance::{ChangeState, Court, Signoff, Task, TaskStatus};

/// 下一步：已准奏初译后先审校，审校齐了再修订。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NextStep {
    Review,
    Revise,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum StaleReason {
    /// 原文与下诏时不同。
    SourceChanged,
    /// 上下文已批准新版本。
    ContextChanged { used: u32, current: u32 },
    /// 此前各章的前情摘要有变（随上下文新版本而来）。
    SummaryChanged { chapters: Vec<String> },
    /// 磁盘上的 `zh.json` 与已准奏（或已定稿）的不同，如被人手改过。
    TranslationEdited,
    /// 已准奏的 `zh.json` 不见了。
    TranslationMissing,
}

impl StaleReason {
    pub fn describe(&self) -> String {
        match self {
            Self::SourceChanged => "原文已变".into(),
            Self::ContextChanged { used, current } => {
                format!("上下文已更新（所用 v{used}，现行 v{current}）")
            }
            Self::SummaryChanged { chapters } => {
                format!("前情摘要有变：{}", chapters.join("、"))
            }
            Self::TranslationEdited => "磁盘上的译稿与已准奏或已定稿的不同".into(),
            Self::TranslationMissing => "已准奏的译稿不见了".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BlockState {
    /// 未开始。
    NotStarted,
    /// 执行中：有关联诏令待办或已指派。
    Running { tasks: Vec<u64> },
    /// 待准奏：最近一件初译或修订已完成、改动待决。
    AwaitingApproval { task: u64 },
    /// 已准奏，待审校或待修订（含御览退回）。
    Applied { task: u64, next: NextStep },
    /// 待御览：已准奏修订稿，尚未定稿。
    AwaitingYulan { task: u64 },
    /// 已定稿。
    Finalized { task: u64, translation_hash: String },
    /// 失败（含改动被驳回、未呈报译稿）；尚无已准奏译稿。
    Failed { task: u64, reason: String },
    /// 撤销；尚无已准奏译稿。
    Cancelled { task: u64 },
    /// 版本失效。
    Stale {
        task: u64,
        reasons: Vec<StaleReason>,
    },
    /// 不确定：须先对账，不猜。
    Uncertain { reason: String },
}

impl BlockState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::NotStarted => "未开始",
            Self::Running { .. } => "执行中",
            Self::AwaitingApproval { .. } => "待准奏",
            Self::Applied {
                next: NextStep::Review,
                ..
            } => "已准奏（待审校）",
            Self::Applied {
                next: NextStep::Revise,
                ..
            } => "已准奏（待修订）",
            Self::AwaitingYulan { .. } => "待御览",
            Self::Finalized { .. } => "已定稿",
            Self::Failed { .. } => "失败",
            Self::Cancelled { .. } => "已撤销",
            Self::Stale { .. } => "版本失效",
            Self::Uncertain { .. } => "不确定",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BlockStatus {
    pub block: String,
    pub chapter: String,
    pub state: BlockState,
    /// 关联到此块的全部诏令，升序。
    pub tasks: Vec<u64>,
    /// 补充说明（如退回理由、其后一件修订未成）。
    pub note: Option<String>,
}

/// 一块的磁盘现状。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockFacts {
    pub block: String,
    pub chapter: String,
    /// 现行原文哈希（见模块说明）。
    pub source_hash: String,
    /// 磁盘上 `zh.json` 的哈希；不存在为 `None`。
    pub disk_hash: Option<String>,
    /// 该块现行朱批条目的哈希（[`Translations::block_zhupi_hash`]）。
    pub zhupi_hash: String,
}

/// 上下文现状：现行版本，及各版本的前情摘要哈希（章 ID → 哈希；版本 0 为空）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextFacts {
    pub current: u32,
    pub summaries: BTreeMap<u32, BTreeMap<String, String>>,
}

/// 已准奏改动中译稿的最终内容（逐段取舍之后）。
/// 修订未改动 `zh.json`（如只补建议）时，译稿仍是此前最近一件已准奏的译文。
pub(crate) fn applied_output(court: &Court, task: &Task) -> Option<String> {
    let link = task.translation.as_ref()?;
    let changes = task.changes.as_ref()?;
    if changes.state != ChangeState::Applied {
        return None;
    }
    if let Some(file) = selected_files(&changes.files, &changes.skipped)
        .into_iter()
        .find(|f| f.path == link.output_path)
    {
        return Some(file.after);
    }
    previous_output(court, task)
}

/// `task` 之前最近一件已准奏的初译或修订所写的译稿。
pub(crate) fn previous_output(court: &Court, task: &Task) -> Option<String> {
    let link = task.translation.as_ref()?;
    court
        .translation_tasks(&link.project, &link.block)
        .filter(|t| t.edict.id < task.edict.id && writes(t))
        .filter(|t| t.changes.as_ref().map(|c| c.state) == Some(ChangeState::Applied))
        .last()
        .and_then(|t| applied_output(court, t))
}

pub(crate) fn writes(task: &Task) -> bool {
    task.translation.as_ref().is_some_and(|l| l.kind.writes())
}

fn kind(task: &Task) -> Option<TranslationKind> {
    task.translation.as_ref().map(|l| l.kind)
}

/// 最近一次御览（`ChangesApplied{after}` 之后的）：定稿或退回。
fn signoff_after(
    history: &[CourtEvent],
    project: &str,
    block: &str,
    applied: TaskId,
) -> Option<Signoff> {
    let start = history
        .iter()
        .rposition(|e| matches!(e, CourtEvent::ChangesApplied { task, .. } if *task == applied))?;
    history[start + 1..].iter().rev().find_map(|e| match e {
        CourtEvent::TranslationFinalized {
            project: p,
            block: b,
            translation_hash,
            context_version,
            zhupi_version,
        } if p == project && b == block => Some(Signoff::Finalized {
            translation_hash: translation_hash.clone(),
            context_version: *context_version,
            zhupi_version: *zhupi_version,
        }),
        CourtEvent::TranslationReopened {
            project: p,
            block: b,
            reason,
        } if p == project && b == block => Some(Signoff::Reopened {
            reason: reason.clone(),
        }),
        _ => None,
    })
}

fn summary_changes(context: &ContextFacts, used: u32, chapter: &str) -> Vec<String> {
    let empty = BTreeMap::new();
    let old = context.summaries.get(&used).unwrap_or(&empty);
    let new = context.summaries.get(&context.current).unwrap_or(&empty);
    let rank = chapter_rank(chapter);
    let mut chapters: Vec<String> = old
        .keys()
        .chain(new.keys())
        .filter(|c| chapter_rank(c) < rank && old.get(*c) != new.get(*c))
        .cloned()
        .collect();
    chapters.sort();
    chapters.dedup();
    chapters
}

fn failure(task: &Task) -> Option<BlockState> {
    let id = task.edict.id.0;
    Some(match &task.status {
        TaskStatus::Failed { reason, .. } => BlockState::Failed {
            task: id,
            reason: reason.clone(),
        },
        TaskStatus::Cancelled => BlockState::Cancelled { task: id },
        TaskStatus::Completed { .. } => match task.changes.as_ref().map(|c| c.state) {
            Some(ChangeState::Discarded) => BlockState::Failed {
                task: id,
                reason: "改动已驳回".into(),
            },
            None => BlockState::Failed {
                task: id,
                reason: "未呈报译稿改动".into(),
            },
            Some(_) => return None,
        },
        TaskStatus::Pending | TaskStatus::Assigned(_) => return None,
    })
}

fn one_block(
    court: &Court,
    history: &[CourtEvent],
    project: &str,
    facts: &BlockFacts,
    context: &ContextFacts,
) -> BlockStatus {
    let tasks: Vec<&Task> = court.translation_tasks(project, &facts.block).collect();
    let ids: Vec<u64> = tasks.iter().map(|t| t.edict.id.0).collect();
    let status = |state, note| BlockStatus {
        block: facts.block.clone(),
        chapter: facts.chapter.clone(),
        state,
        tasks: ids.clone(),
        note,
    };
    let open: Vec<u64> = tasks
        .iter()
        .filter(|t| t.status.is_open())
        .map(|t| t.edict.id.0)
        .collect();
    if !open.is_empty() {
        return status(BlockState::Running { tasks: open }, None);
    }
    let writers: Vec<&Task> = tasks.iter().copied().filter(|t| writes(t)).collect();
    let latest = writers.last().copied();
    if let Some(w) = latest
        && matches!(w.status, TaskStatus::Completed { .. })
        && w.changes.as_ref().map(|c| c.state) == Some(ChangeState::Proposed)
    {
        return status(BlockState::AwaitingApproval { task: w.edict.id.0 }, None);
    }
    let applied = writers
        .iter()
        .rev()
        .copied()
        .find(|t| t.changes.as_ref().map(|c| c.state) == Some(ChangeState::Applied));
    let Some(applied) = applied else {
        if facts.disk_hash.is_some() {
            return status(
                BlockState::Uncertain {
                    reason: "磁盘上已有 zh.json，但没有已准奏的译书诏令；请先核对".into(),
                },
                None,
            );
        }
        let state = latest.and_then(failure).unwrap_or(BlockState::NotStarted);
        return status(state, None);
    };
    let link = applied.translation.as_ref().expect("已确认有关联");
    let id = applied.edict.id.0;
    let mut note = latest
        .filter(|w| w.edict.id != applied.edict.id)
        .and_then(|w| failure(w).map(|f| (w, f)))
        .map(|(w, f)| {
            format!(
                "其后的{} #{} 未成（{}）",
                kind(w).map_or("", |k| k.label()),
                w.edict.id.0,
                f.label()
            )
        });
    let Some(applied_bytes) = applied_output(court, applied) else {
        return status(
            BlockState::Uncertain {
                reason: format!("诏令 #{id} 已准奏的改动中没有译稿"),
            },
            note,
        );
    };
    let applied_hash = sha256_hex(applied_bytes.as_bytes());
    let signoff = signoff_after(history, project, &facts.block, applied.edict.id);

    let mut reasons = Vec::new();
    if facts.source_hash != link.source_hash {
        reasons.push(StaleReason::SourceChanged);
    }
    if context.current != link.context_version {
        reasons.push(StaleReason::ContextChanged {
            used: link.context_version,
            current: context.current,
        });
        let chapters = summary_changes(context, link.context_version, &facts.chapter);
        if !chapters.is_empty() {
            reasons.push(StaleReason::SummaryChanged { chapters });
        }
    }
    let expected = match &signoff {
        Some(Signoff::Finalized {
            translation_hash, ..
        }) => translation_hash.as_str(),
        _ => applied_hash.as_str(),
    };
    match &facts.disk_hash {
        None => reasons.push(StaleReason::TranslationMissing),
        Some(h) if h != expected => reasons.push(StaleReason::TranslationEdited),
        Some(_) => {}
    }
    if !reasons.is_empty() {
        return status(BlockState::Stale { task: id, reasons }, note);
    }
    let state = match signoff {
        Some(Signoff::Finalized {
            translation_hash, ..
        }) => BlockState::Finalized {
            task: id,
            translation_hash,
        },
        Some(Signoff::Reopened { reason }) => {
            note = Some(match note {
                Some(n) => format!("退回重修：{reason}；{n}"),
                None => format!("退回重修：{reason}"),
            });
            BlockState::Applied {
                task: id,
                next: NextStep::Revise,
            }
        }
        None if link.kind == TranslationKind::Revise => {
            if facts.zhupi_hash != link.zhupi_hash {
                note = Some(match note {
                    Some(n) => format!("朱批自下诏后有变，御览前须再下修订；{n}"),
                    None => "朱批自下诏后有变，御览前须再下修订".into(),
                });
            }
            BlockState::AwaitingYulan { task: id }
        }
        None => {
            let reviewed = |k: TranslationKind| {
                tasks.iter().any(|t| {
                    t.edict.id > applied.edict.id
                        && kind(t) == Some(k)
                        && matches!(t.status, TaskStatus::Completed { .. })
                })
            };
            let next = if reviewed(TranslationKind::ReviewFidelity)
                && reviewed(TranslationKind::ReviewStyle)
            {
                NextStep::Revise
            } else {
                NextStep::Review
            };
            BlockState::Applied { task: id, next }
        }
    };
    status(state, note)
}

/// 各块状态，与 `blocks` 同序。`history` 为事件簿全部事件（用于判定御览与准奏的先后）。
pub fn block_states(
    court: &Court,
    history: &[CourtEvent],
    project: &str,
    blocks: &[BlockFacts],
    context: &ContextFacts,
) -> Vec<BlockStatus> {
    blocks
        .iter()
        .map(|f| one_block(court, history, project, f, context))
        .collect()
}

/// 续做的取舍：默认只为未开始的块新下初译；失败/撤销、版本失效须明示才重做。
/// 执行中、待准奏、不确定的块从不新下。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResumeOptions {
    pub include_failed: bool,
    pub include_stale: bool,
}

/// 将新下初译的块，按 `statuses` 的次序。
pub fn resume_plan(statuses: &[BlockStatus], options: ResumeOptions) -> Vec<String> {
    statuses
        .iter()
        .filter(|s| match s.state {
            BlockState::NotStarted => true,
            BlockState::Failed { .. } | BlockState::Cancelled { .. } => options.include_failed,
            BlockState::Stale { .. } => options.include_stale,
            _ => false,
        })
        .map(|s| s.block.clone())
        .collect()
}
