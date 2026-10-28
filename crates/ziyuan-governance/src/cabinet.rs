//! 内阁：票拟、批红、廷议与分办。

use crate::edicts::department_of;
use crate::{Court, GovernanceError, TaskStatus};
use serde::{Deserialize, Serialize};
use ziyuan_core::{
    Agent, AgentId, CourtEvent, Department, GovernanceMode, Office, Speech, Subtask, TaskId,
    TokenUsage,
};

/// 分办方案。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Split {
    pub subtasks: Vec<TaskId>,
    /// 尚书的拆分说明，或「御定分办」。
    pub note: String,
    pub model: Option<String>,
    /// 拆分所用的 token（已计入 `Task::usage`）。
    pub usage: TokenUsage,
}

/// 内阁票拟的结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    /// 拟派的承办者。
    pub worker: AgentId,
    /// 票拟意见（首辅所写，或「御笔改票」）。
    pub note: String,
    /// 最近一次由模型票拟时所用的模型。
    pub model: Option<String>,
    /// 历次票拟的累计用量（已计入 `Task::usage`）。
    pub usage: TokenUsage,
}

impl Court {
    /// 分办：把待办诏令 `task` 拆给本部官员，`plan` 为 (承办者, 子诏令内容)。
    pub fn propose_split(
        &self,
        task: TaskId,
        plan: Vec<(AgentId, String)>,
        note: String,
        model: Option<String>,
        usage: TokenUsage,
    ) -> Result<CourtEvent, GovernanceError> {
        self.check_split(task, &plan)?;
        let subtasks = plan
            .into_iter()
            .zip(self.next_task_id..)
            .map(|((worker, text), id)| Subtask {
                id: TaskId(id),
                worker,
                text,
            })
            .collect();
        Ok(CourtEvent::EdictSplit {
            task,
            subtasks,
            note,
            model,
            usage,
        })
    }

    pub(crate) fn check_split(
        &self,
        task: TaskId,
        plan: &[(AgentId, String)],
    ) -> Result<(), GovernanceError> {
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        if entry.status != TaskStatus::Pending || entry.split.is_some() || entry.parent.is_some() {
            return Err(GovernanceError::CannotSplit(task));
        }
        if !(2..=8).contains(&plan.len()) {
            return Err(GovernanceError::InvalidSplit(format!(
                "须分为 2–8 件，实为 {} 件",
                plan.len()
            )));
        }
        if plan.iter().any(|(_, text)| text.trim().is_empty()) {
            return Err(GovernanceError::InvalidSplit("子诏令内容不能为空".into()));
        }
        let department = entry.edict.department;
        let staff = self.draft_candidates(department);
        if let Some((worker, _)) = plan
            .iter()
            .find(|(w, _)| !staff.iter().any(|(a, _)| a.id == *w))
        {
            return Err(GovernanceError::NotEligible {
                worker: *worker,
                department,
            });
        }
        Ok(())
    }

    /// 批红：内阁制下，诏令经内阁票拟（派工路线）后须皇帝批红才能执行。
    pub fn propose_approval(&self, task: TaskId) -> Result<CourtEvent, GovernanceError> {
        if self.mode != GovernanceMode::Cabinet {
            return Err(GovernanceError::ApprovalNotNeeded);
        }
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        if !entry.status.is_open() {
            return Err(GovernanceError::TaskFinished(task));
        }
        if !self.needs_approval(entry) {
            return Err(GovernanceError::AlreadyApproved(task));
        }
        Ok(CourtEvent::EdictApproved { task })
    }

    /// 票拟：内阁制下、批红之前，拟派本部在任官员承办。
    pub fn propose_draft(
        &self,
        task: TaskId,
        worker: AgentId,
        note: String,
        model: Option<String>,
        usage: TokenUsage,
    ) -> Result<CourtEvent, GovernanceError> {
        if self.mode != GovernanceMode::Cabinet {
            return Err(GovernanceError::NoCabinet);
        }
        // 诏令须存在、未结束且尚未批红。
        self.propose_approval(task)?;
        let department = self.tasks[&task].edict.department;
        if !self
            .draft_candidates(department)
            .iter()
            .any(|(a, _)| a.id == worker)
        {
            return Err(GovernanceError::NotEligible { worker, department });
        }
        Ok(CourtEvent::EdictDrafted {
            task,
            worker,
            note,
            model,
            usage,
        })
    }

    /// 可拟派承办 `department` 诏令的官员：本部在任尚书、侍郎、郎中（依次排列）。
    pub fn draft_candidates(&self, department: Department) -> Vec<(&Agent, Office)> {
        let mut staff: Vec<_> = self
            .appointments
            .iter()
            .filter(|(_, office)| department_of(**office) == Some(department))
            .filter_map(|(id, office)| Some((self.agents.get(id)?, *office)))
            .collect();
        staff.sort_by_key(|(agent, office)| {
            let rank = match office {
                Office::Minister(_) => 0,
                Office::ViceMinister(_) => 1,
                _ => 2,
            };
            (rank, agent.id)
        });
        staff
    }

    /// 本部长官：尚书，空缺则由侍郎署理。
    pub fn department_head(&self, department: Department) -> Option<(&Agent, Office)> {
        self.head_or_deputy(
            Office::Minister(department),
            Office::ViceMinister(department),
        )
    }

    /// 现行制度下的协调者（正职，空缺则副职）及其官职；废相亲政时为 `None`。
    pub fn coordinator(&self) -> Option<(&Agent, Office)> {
        match self.mode {
            GovernanceMode::Chancellor => {
                self.head_or_deputy(Office::Chancellor, Office::ViceChancellor)
            }
            GovernanceMode::Cabinet => {
                self.head_or_deputy(Office::FirstGrandSecretary, Office::SecondGrandSecretary)
            }
            GovernanceMode::DirectRule => None,
        }
    }

    /// 廷议可否：诏令待办，发言者在发言时担任所记官职。
    pub fn propose_debate(&self, task: TaskId, speeches: &[Speech]) -> Result<(), GovernanceError> {
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        if entry.status != TaskStatus::Pending {
            return Err(GovernanceError::CannotDebate(task));
        }
        if speeches.is_empty() {
            return Err(GovernanceError::InvalidDebate("没有发言".into()));
        }
        for speech in speeches {
            if self.appointments.get(&speech.speaker) != Some(&speech.office) {
                return Err(GovernanceError::NotAppointed(speech.speaker));
            }
        }
        Ok(())
    }

    /// 默认的廷议人选（至多四位）：协调者（丞相或首辅）、本部长官、左都御史，不足再由本部官员补上。
    pub fn debate_candidates(&self, department: Department) -> Vec<(&Agent, Office)> {
        let mut chosen: Vec<(&Agent, Office)> = Vec::new();
        let censor = self.censor().map(|a| (a, Office::Censor));
        let staff = self.draft_candidates(department);
        let picks = [self.coordinator(), self.department_head(department), censor];
        for pick in picks.into_iter().flatten().chain(staff) {
            if chosen.len() < 4 && chosen.iter().all(|(a, _)| a.id != pick.0.id) {
                chosen.push(pick);
            }
        }
        chosen
    }
}
