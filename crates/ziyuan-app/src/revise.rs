//! 打回再改一轮：携带上轮改动 diff、都察院逐段意见、皇帝批语与补充指示，
//! 一键下发后续工部写诏令，不新增朝廷事件类型。

use std::collections::BTreeSet;
use std::path::Path;
use ziyuan_core::{Capability, CensorVerdict, Department, Edict, Seal, TaskId, Verdict};
use ziyuan_governance::{ChangeState, GovernanceError, Task, TaskStatus};
use ziyuan_storage::EventStore;

use crate::censorate::{HunkVerdict, parse_hunk_reviews, total_hunks};
use crate::edicts::render_diffs;
use crate::plan::{collapse_scope, nearest_existing_parent, push_unique, validate_scope_override};
use crate::{AppError, Session, truncate};

const MAX_EDICT_CHARS: usize = 800;
const MAX_DIFF_CHARS: usize = 3500;
const MAX_CENSURE_CHARS: usize = 1200;
const MAX_REVIEW_CHARS: usize = 600;

/// 从已完成的工部代码诏令中提炼出的「打回再改一轮」预填方案。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviseDraft {
    /// 原诏令 ID。
    pub source_task: TaskId,
    /// 延续的有效查阅范围（盘上已删的路径已收拢到最近已存在父目录）。
    pub scope: Vec<String>,
    /// 默认是否保留 `run_tests`（沿用原诏令设定）。
    pub run_tests: bool,
    /// 原诏令是否带有暂存／已处理改动。
    pub has_changes: bool,
    /// 上轮被逐段驳回或御史建议驳回的段号（1-based，升序去重）。
    pub rejected_hunks: Vec<u32>,
    /// 原诏令是否已有都察院复核意见。
    pub has_censure: bool,
    /// 原诏令是否已有皇帝评定意见。
    pub has_review: bool,
}

/// 一件诏令是否可一键打回修订：须为已完成、非译书、工部且带写权限或改动的诏令。
pub fn can_revise_task(task: &Task) -> bool {
    matches!(task.status, TaskStatus::Completed { .. })
        && task.translation.is_none()
        && task.edict.department == Department::Works
        && (task.changes.is_some()
            || task
                .edict
                .seal
                .capabilities
                .contains(&Capability::WriteFile))
}

/// 为已完成的工部代码诏令构造修订草案（只读计算，不修改事件簿）。
pub fn draft_revision(task: &Task, root: &Path) -> Result<ReviseDraft, AppError> {
    if !matches!(task.status, TaskStatus::Completed { .. }) {
        return Err(GovernanceError::Court(format!(
            "诏令 #{} 尚未完成，不能打回修订",
            task.edict.id.0
        ))
        .into());
    }
    if task.translation.is_some() {
        return Err(GovernanceError::Court(format!(
            "诏令 #{} 为译书诏令，请经 `ziyuan text revise` 流程修订",
            task.edict.id.0
        ))
        .into());
    }
    if task.edict.department != Department::Works
        || (task.changes.is_none()
            && !task
                .edict
                .seal
                .capabilities
                .contains(&Capability::WriteFile))
    {
        return Err(GovernanceError::Court(format!(
            "诏令 #{} 不是工部写任务，不能直接打回修订（只读条陈请改用 `--from-plan`）",
            task.edict.id.0
        ))
        .into());
    }

    let mut scope = Vec::new();
    for raw in &task.edict.seal.scope {
        if root.join(raw).exists() {
            push_unique(&mut scope, raw.clone());
        } else {
            let parent = nearest_existing_parent(root, raw);
            push_unique(&mut scope, parent);
        }
    }
    let scope = collapse_scope(scope);
    let scope = if scope.is_empty() {
        vec![".".to_owned()]
    } else {
        scope
    };

    let mut rejected = BTreeSet::new();
    if let Some(changes) = &task.changes {
        for &n in &changes.skipped {
            rejected.insert(n);
        }
        if let Some(censure) = &task.censure {
            let total = total_hunks(&changes.files);
            for (hunk, hr) in parse_hunk_reviews(&censure.note, total) {
                if hr.verdict == HunkVerdict::Reject {
                    rejected.insert(hunk);
                }
            }
        }
    }

    Ok(ReviseDraft {
        source_task: task.edict.id,
        scope,
        run_tests: task.edict.seal.capabilities.contains(&Capability::RunTests),
        has_changes: task.changes.is_some(),
        rejected_hunks: rejected.into_iter().collect(),
        has_censure: task.censure.is_some(),
        has_review: task.review.is_some(),
    })
}

/// 组装打回再改一轮的工部诏令正文：携带原诏令、上轮改动摘录、都察院意见、皇帝评定与本次指示。
pub fn revision_edict_text(source: &Task, scope: &[String], note: &str) -> String {
    let mut out = format!(
        "打回修订诏令 #{}（延续范围：{}）：\n原诏令：{}",
        source.edict.id.0,
        scope.join("、"),
        truncate(source.edict.text.trim(), MAX_EDICT_CHARS),
    );

    if let Some(changes) = &source.changes {
        let state_label = match changes.state {
            ChangeState::Proposed => "待准奏（尚未落盘）".to_owned(),
            ChangeState::Discarded => "已驳回（未落盘）".to_owned(),
            ChangeState::Applied if changes.skipped.is_empty() => "已全部准奏落盘".to_owned(),
            ChangeState::Applied => format!(
                "已部分准奏（驳回第 {} 段，其余已落盘）",
                changes
                    .skipped
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join("、")
            ),
        };
        let files: Vec<String> = changes
            .files
            .iter()
            .map(|f| {
                let kind = if f.deleted {
                    "删除"
                } else if f.before.is_none() {
                    "新建"
                } else {
                    "改写"
                };
                format!("{}（{}）", f.path, kind)
            })
            .collect();
        out.push_str(&format!(
            "\n\n上轮呈报改动（{state_label}；涉及：{}）：",
            files.join("、")
        ));
        let diff = render_diffs(&changes.files).join("\n");
        if !diff.trim().is_empty() {
            out.push_str(&format!(
                "\n```diff\n{}\n```",
                truncate(diff.trim(), MAX_DIFF_CHARS)
            ));
        }
    }

    if let Some(censure) = &source.censure {
        let verdict = match censure.verdict {
            CensorVerdict::Endorsed => "背书",
            CensorVerdict::Impeached => "弹劾",
        };
        out.push_str(&format!(
            "\n\n都察院复核意见（{verdict}）：\n{}",
            truncate(censure.note.trim(), MAX_CENSURE_CHARS)
        ));
        if let Some(changes) = &source.changes {
            let total = total_hunks(&changes.files);
            let flagged: Vec<String> = parse_hunk_reviews(&censure.note, total)
                .into_iter()
                .filter(|(_, r)| r.verdict != HunkVerdict::Ok)
                .map(|(hunk, r)| format!("- 第 {hunk} 段（{}）：{}", r.verdict.label(), r.note))
                .collect();
            if !flagged.is_empty() {
                out.push_str(&format!("\n重点逐段意见：\n{}", flagged.join("\n")));
            }
        }
    }

    if let Some((verdict, review_note)) = &source.review {
        let verdict = match verdict {
            Verdict::Accepted => "采纳",
            Verdict::Rejected => "驳回",
        };
        let review_note = review_note.trim();
        if review_note.is_empty() {
            out.push_str(&format!("\n\n皇帝上轮评定：{verdict}"));
        } else {
            out.push_str(&format!(
                "\n\n皇帝上轮评定（{verdict}）：{}",
                truncate(review_note, MAX_REVIEW_CHARS)
            ));
        }
    }

    let note = note.trim();
    if !note.is_empty() {
        out.push_str(&format!("\n\n本次修订指示：{note}"));
    }

    out.push_str(
        "\n\n办理要求：请先用只读工具核对当前项目磁盘上的实际文件内容（勿假定上轮暂存改动已落盘），按上述反馈完成修订并重新呈报；改动仍须暂存并经皇帝逐段准奏才落盘。",
    );
    out
}

impl<S: EventStore> Session<S> {
    /// 打回已完成的工部诏令再改一轮：携带上轮 diff、都察院意见与皇帝批语下发新工部诏令。
    ///
    /// - `scope_override` 为 `None` 或空时沿用原诏令范围（已删路径自动收拢到最近已存在父目录）；
    /// - `run_tests` 为 `None` 时沿用原诏令是否授予 `run_tests`，`Some(bool)` 显式覆盖；
    /// - 不新增朝廷事件类型，仍只记一条 `EdictIssued`，改动仍须暂存与逐段准奏。
    pub fn issue_revision(
        &mut self,
        source_task: TaskId,
        root: &Path,
        scope_override: Option<Vec<String>>,
        run_tests: Option<bool>,
        note: String,
    ) -> Result<(Edict, ReviseDraft), AppError> {
        self.reload()?;
        let entry = self
            .court
            .task(source_task)
            .ok_or(GovernanceError::UnknownEdict(source_task))?
            .clone();
        let mut draft = draft_revision(&entry, root)?;
        if let Some(custom) = scope_override
            && !custom.is_empty()
        {
            draft.scope = validate_scope_override(root, custom)?;
        }
        let want_tests = run_tests.unwrap_or(draft.run_tests);
        draft.run_tests = want_tests;

        let mut capabilities: BTreeSet<Capability> = entry.edict.seal.capabilities.clone();
        capabilities.insert(Capability::ReadFile);
        capabilities.insert(Capability::SearchCode);
        capabilities.insert(Capability::WriteFile);
        if want_tests {
            capabilities.insert(Capability::RunTests);
        } else {
            capabilities.remove(&Capability::RunTests);
        }

        let seal = Seal {
            capabilities,
            scope: draft.scope.clone(),
            mcp: entry.edict.seal.mcp.clone(),
        };
        let text = revision_edict_text(&entry, &draft.scope, &note);
        let edict = self.issue(Department::Works, text, seal)?;
        Ok((edict, draft))
    }
}
