//! 诏令：下诏、路由、评定、准奏与撤销。

use crate::offices::validate_edict_seal;
use crate::personnel::Pick;
use crate::{Court, GovernanceError, Task, TaskStatus};
use serde::{Deserialize, Serialize};
use ziyuan_core::{
    Agent, AgentId, CourtEvent, Department, Edict, FileChange, GovernanceMode, Office, Seal,
    TaskId, Verdict,
};

/// 呈报的文件改动及皇帝的处置。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changes {
    pub files: Vec<FileChange>,
    pub state: ChangeState,
    /// 逐段准奏时驳回的段号（从 1 起，跨文件连续编号）。
    pub skipped: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeState {
    /// 待皇帝落盘或驳回。
    Proposed,
    /// 已写入项目。
    Applied,
    /// 驳回，未写入。
    Discarded,
}

/// 查阅范围只能是项目内的相对路径。路径是否存在、是否经符号链接逃逸，由运行时在构造工具时检查。
/// 诏令的玉玺：在 [`validate_seal`] 之外，写文件与跑测试只能授予工部。
/// （官职上限不受此限：上限只会收窄诏令的玉玺。）
pub(crate) fn check_criteria(criteria: &[String]) -> Result<(), GovernanceError> {
    let bad = |why: &str| Err(GovernanceError::Template(why.into()));
    if criteria.is_empty() || criteria.len() > 8 {
        return bad("验收标准须为 1–8 条");
    }
    if criteria
        .iter()
        .any(|c| c.trim().is_empty() || c.chars().count() > 200 || c.contains(['\n', '\r']))
    {
        return bad("每条验收标准须为单行、1–200 字");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub path: Vec<String>,
    pub worker: AgentId,
}

/// 六部官职所属的部门。
pub(crate) fn department_of(office: Office) -> Option<Department> {
    match office {
        Office::Minister(d) | Office::ViceMinister(d) | Office::Clerk(d) => Some(d),
        _ => None,
    }
}

impl Court {
    /// 八百里加急：诏令须未结束且尚未加急。
    pub fn propose_expedite(&self, task: TaskId) -> Result<CourtEvent, GovernanceError> {
        self.open_task(task)?;
        if self.tasks[&task].urgent {
            return Err(GovernanceError::Military(format!(
                "诏令 #{} 已是八百里加急",
                task.0
            )));
        }
        Ok(CourtEvent::EdictExpedited { task })
    }

    /// 皇帝评定一件已完成的诏令；每件只能评定一次。
    pub fn propose_review(
        &self,
        task: TaskId,
        verdict: Verdict,
        note: String,
    ) -> Result<CourtEvent, GovernanceError> {
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        if !matches!(entry.status, TaskStatus::Completed { .. }) {
            return Err(GovernanceError::NotCompleted(task));
        }
        if entry.review.is_some() {
            return Err(GovernanceError::AlreadyReviewed(task));
        }
        Ok(CourtEvent::TaskReviewed {
            task,
            verdict,
            note,
        })
    }

    /// 皇帝处置已完成诏令呈报的改动：`apply` 为真是落盘（准奏），否则驳回。
    /// 实际写入文件由调用方在提交事件前完成。
    pub fn propose_changes_decision(
        &self,
        task: TaskId,
        apply: bool,
    ) -> Result<CourtEvent, GovernanceError> {
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        if !matches!(entry.status, TaskStatus::Completed { .. }) {
            return Err(GovernanceError::NotCompleted(task));
        }
        match &entry.changes {
            Some(c) if c.state == ChangeState::Proposed => Ok(if apply {
                CourtEvent::ChangesApplied {
                    task,
                    skipped: Vec::new(),
                }
            } else {
                CourtEvent::ChangesDiscarded { task }
            }),
            _ => Err(GovernanceError::NoPendingChanges(task)),
        }
    }

    /// 逐段准奏：驳回 `skipped` 中的段（从 1 起，跨文件连续编号），其余写入项目。
    /// 段号须在范围内；不能全部驳回（那是驳回改动）。
    pub fn propose_application(
        &self,
        task: TaskId,
        mut skipped: Vec<u32>,
    ) -> Result<CourtEvent, GovernanceError> {
        self.propose_changes_decision(task, true)?;
        let files = &self.tasks[&task]
            .changes
            .as_ref()
            .expect("已确认有待决改动")
            .files;
        let total: usize = files.iter().map(|f| f.hunks().len()).sum();
        skipped.sort_unstable();
        skipped.dedup();
        let bad = |why: String| Err(GovernanceError::Changes(why));
        if let Some(n) = skipped.iter().find(|&&n| n == 0 || n as usize > total) {
            return bad(format!("没有第 {n} 段：改动共 {total} 段"));
        }
        if !skipped.is_empty() && skipped.len() == total {
            return bad("各段都驳回了，请直接驳回改动".into());
        }
        Ok(CourtEvent::ChangesApplied { task, skipped })
    }

    pub fn propose_cancellation(&self, task: TaskId) -> Result<CourtEvent, GovernanceError> {
        self.open_task(task)?;
        Ok(CourtEvent::TaskCancelled { task })
    }

    pub fn new_edict(
        &self,
        department: Department,
        text: String,
        seal: Seal,
    ) -> Result<Edict, GovernanceError> {
        if text.trim().is_empty() {
            return Err(GovernanceError::EmptyEdict);
        }
        validate_edict_seal(department, &seal)?;
        Ok(Edict {
            id: TaskId(self.next_task_id),
            department,
            text,
            seal,
        })
    }

    /// 路由说明是可观察性日志，不是给 Worker 的权限凭证。
    ///
    /// 皇帝 →（中书省或内阁）→ 本部尚书 → 郎中。正职空缺时由副职署理，在路线中注明。
    pub fn route(&self, edict: &Edict) -> Result<Route, GovernanceError> {
        self.route_to(edict, Pick::Default)
    }

    /// 诏令的派工路线：子诏令派给分办时指定的官员；已分办的原诏令由尚书汇总；内阁制下若已
    /// 票拟，派给拟派者。指定者已不在本部任职时按旧例。
    pub fn route_task(&self, task: &Task) -> Result<Route, GovernanceError> {
        let pick = if let Some(worker) = task.assignee {
            Pick::Named(worker, "（分办）")
        } else if task.split.is_some() {
            Pick::Head
        } else if let Some(draft) = task
            .draft
            .as_ref()
            .filter(|_| self.mode == GovernanceMode::Cabinet)
        {
            Pick::Named(draft.worker, "（票拟）")
        } else {
            Pick::Default
        };
        self.route_to(&task.edict, pick)
    }

    pub(crate) fn route_to(&self, edict: &Edict, pick: Pick) -> Result<Route, GovernanceError> {
        let mut path = vec!["皇帝".to_owned()];
        let step = |(agent, office): (&Agent, Office), head: Office| {
            let acting = if office == head { "" } else { "（署理）" };
            format!("{}·{}{acting}", office.label(), agent.name)
        };
        let coordinator = match self.mode {
            GovernanceMode::Chancellor => Some((Office::Chancellor, Office::ViceChancellor)),
            GovernanceMode::Cabinet => {
                Some((Office::FirstGrandSecretary, Office::SecondGrandSecretary))
            }
            GovernanceMode::DirectRule => None,
        };
        if let Some((head, deputy)) = coordinator {
            let found = self
                .head_or_deputy(head, deputy)
                .ok_or(GovernanceError::Vacant(head))?;
            path.push(step(found, head));
        }
        let department = edict.department;
        let head = Office::Minister(department);
        let found = self
            .head_or_deputy(head, Office::ViceMinister(department))
            .ok_or(GovernanceError::Vacant(head))?;
        path.push(step(found, head));
        let minister = found.0;
        let staff = self.draft_candidates(department);
        if matches!(pick, Pick::Head) {
            path.last_mut()
                .expect("至少有尚书一步")
                .push_str("（汇总）");
            return Ok(Route {
                path,
                worker: minister.id,
            });
        }
        let (drafted, mark) = match pick {
            Pick::Named(id, mark) => (staff.iter().find(|(a, _)| a.id == id), mark),
            _ => (None, ""),
        };
        // 未票拟时按任务量均衡派给郎中；本部无郎中则由尚书（或署理的侍郎）亲办。
        let (worker, office) = drafted.copied().unwrap_or_else(|| {
            staff
                .iter()
                .filter(|(_, office)| *office == Office::Clerk(department))
                .min_by_key(|(candidate, _)| (self.workload(candidate.id), candidate.id))
                .copied()
                .unwrap_or(found)
        });
        let mark = if drafted.is_some() { mark } else { "" };
        if worker.id != minister.id {
            path.push(format!("{}·{}{mark}", office.label(), worker.name));
        } else if drafted.is_some() {
            let last = path.last_mut().expect("至少有尚书一步");
            last.push_str(mark);
        }
        Ok(Route {
            path,
            worker: worker.id,
        })
    }
}
