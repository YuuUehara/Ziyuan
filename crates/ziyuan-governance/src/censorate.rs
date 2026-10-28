//! 都察院与刑部：复核回奏、密报复勘与平反。

use crate::{Court, GovernanceError, Task, TaskStatus};
use serde::{Deserialize, Serialize};
use ziyuan_core::{
    Agent, AgentId, CensorVerdict, Department, Evidence, Office, TaskId, Threat, TokenUsage,
};

/// 一处检出（见 `CourtEvent::SuspicionRaised`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suspicion {
    pub source: String,
    pub threat: Threat,
    pub excerpt: String,
    /// 刑部复勘（见 `CourtEvent::SuspicionReviewed`）。
    pub review: Option<SuspicionReview>,
    /// 皇帝已平反：不再妨碍代批。
    pub acquitted: bool,
}

/// 刑部复勘的意见（用量已计入 `Task::usage`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuspicionReview {
    pub reviewer: AgentId,
    /// 维持原检；否则判为误报。
    pub upheld: bool,
    pub note: String,
    pub model: Option<String>,
    pub usage: TokenUsage,
}

impl Suspicion {
    /// 驿传入队时对诏令内容的检出所记的来源。
    pub const COURIER: &'static str = "courier";

    /// 检出者：外来文书（工具输出、驿传）归锦衣卫，回奏归刑部。
    pub fn bureau(&self) -> &'static str {
        if self.source.starts_with("tool:") || self.source == Self::COURIER {
            "锦衣卫"
        } else {
            "刑部"
        }
    }

    /// 检出之处：工具名、「驿传」或「回奏」。
    pub fn origin(&self) -> &str {
        match self.source.strip_prefix("tool:") {
            Some(tool) => tool,
            None if self.source == Self::COURIER => "驿传",
            None => "回奏",
        }
    }
}

/// 都察院复核的结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Censure {
    pub censor: AgentId,
    pub verdict: CensorVerdict,
    pub note: String,
    pub evidence: Evidence,
    pub model: Option<String>,
    /// 复核所用的 token（已计入 `Task::usage`）。
    pub usage: TokenUsage,
}

impl Court {
    /// 诏令的第 `index` 处检出（从 0 起）。
    pub(crate) fn suspicion(
        &self,
        task: TaskId,
        index: u32,
    ) -> Result<(&Task, &Suspicion), GovernanceError> {
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        let found = entry.suspicions.get(index as usize).ok_or_else(|| {
            GovernanceError::Suspicion(format!(
                "诏令 #{} 没有第 {} 处检出（共 {} 处）",
                task.0,
                index + 1,
                entry.suspicions.len()
            ))
        })?;
        Ok((entry, found))
    }

    /// 刑部复勘可否：检出存在且未复勘、未平反，`reviewer` 现任刑部官职且未经手此诏令。
    pub fn propose_recheck(
        &self,
        task: TaskId,
        index: u32,
        reviewer: AgentId,
    ) -> Result<(), GovernanceError> {
        let (entry, found) = self.suspicion(task, index)?;
        if found.review.is_some() || found.acquitted {
            return Err(GovernanceError::Suspicion(format!(
                "诏令 #{} 第 {} 处检出已复勘或已平反",
                task.0,
                index + 1
            )));
        }
        let justice = matches!(
            self.appointments.get(&reviewer),
            Some(
                Office::Minister(Department::Justice)
                    | Office::ViceMinister(Department::Justice)
                    | Office::Clerk(Department::Justice)
            )
        );
        if !justice {
            return Err(GovernanceError::Suspicion(format!(
                "官员 #{} 不是现任刑部官员",
                reviewer.0
            )));
        }
        if entry.handler() == Some(reviewer) {
            return Err(GovernanceError::SelfReview(reviewer));
        }
        Ok(())
    }

    /// 复勘此诏令检出的刑部官员：尚书（空缺则侍郎），回避经手者时取刑部郎中。
    pub fn recheck_official(&self, task: TaskId) -> Option<&Agent> {
        let handler = self.tasks.get(&task).and_then(Task::handler);
        let justice = [
            Office::Minister(Department::Justice),
            Office::ViceMinister(Department::Justice),
            Office::Clerk(Department::Justice),
        ];
        justice.into_iter().find_map(|office| {
            self.appointments
                .iter()
                .find(|(id, o)| **o == office && Some(**id) != handler)
                .and_then(|(id, _)| self.agents.get(id))
        })
    }

    /// 平反可否：检出存在且未平反，皇帝在朝。
    pub fn propose_acquittal(&self, task: TaskId, index: u32) -> Result<(), GovernanceError> {
        if self.retreat {
            return Err(GovernanceError::InRetreat);
        }
        let (_, found) = self.suspicion(task, index)?;
        if found.acquitted {
            return Err(GovernanceError::Suspicion(format!(
                "诏令 #{} 第 {} 处检出已平反",
                task.0,
                index + 1
            )));
        }
        Ok(())
    }

    /// 现任左都御史。
    pub fn censor(&self) -> Option<&Agent> {
        self.appointed_as(Office::Censor)
    }

    /// 都察院可否由 `censor` 复核 `task`：诏令已完成且未复核，`censor` 为现任左都御史且未经手此诏令。
    pub fn propose_censure_check(
        &self,
        task: TaskId,
        censor: AgentId,
    ) -> Result<(), GovernanceError> {
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        if !matches!(entry.status, TaskStatus::Completed { .. }) {
            return Err(GovernanceError::NotCompleted(task));
        }
        if entry.censure.is_some() {
            return Err(GovernanceError::AlreadyCensured(task));
        }
        if self.appointments.get(&censor) != Some(&Office::Censor) {
            return Err(GovernanceError::NotCensor(censor));
        }
        if entry.handler() == Some(censor) {
            return Err(GovernanceError::SelfReview(censor));
        }
        Ok(())
    }
}
