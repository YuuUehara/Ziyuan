//! 户部：预算与花费核算。

use crate::{Court, GovernanceError, Task};
use ziyuan_core::{AgentId, BudgetScope, CourtEvent};

impl Court {
    /// 设定度支：须为正数；诏令须尚未结束。
    pub fn propose_budget(
        &self,
        scope: BudgetScope,
        tokens: Option<u64>,
    ) -> Result<CourtEvent, GovernanceError> {
        if tokens == Some(0) {
            return Err(GovernanceError::InvalidBudget);
        }
        if let BudgetScope::Task(task) = scope {
            self.open_task(task)?;
        }
        Ok(CourtEvent::BudgetSet { scope, tokens })
    }

    pub fn budget(&self, scope: BudgetScope) -> Option<u64> {
        match scope {
            BudgetScope::Court => self.court_budget,
            BudgetScope::Office(office) => self.office_budgets.get(&office.code()).copied(),
            BudgetScope::Task(task) => self.tasks.get(&task)?.budget,
        }
    }

    /// 朝廷度支的余额；未设总额时为 `None`。
    pub fn treasury_remaining(&self) -> Option<u64> {
        let limit = self.court_budget?;
        Some(limit.saturating_sub(self.total_usage().total()))
    }

    /// 国库已告罄（计入尚未记账的 `pending` tokens）则报错。
    pub fn check_treasury(&self, pending: u64) -> Result<(), GovernanceError> {
        match (self.court_budget, self.treasury_remaining()) {
            (Some(limit), Some(left)) if left <= pending => {
                Err(GovernanceError::TreasuryExhausted(limit))
            }
            _ => Ok(()),
        }
    }

    /// 为 `task` 再调用模型（票拟、分办、廷议、复核）之前：国库与本件度支都须有余。
    pub fn check_funds(&self, task: &Task, pending: u64) -> Result<(), GovernanceError> {
        self.check_treasury(pending)?;
        self.check_task_funds(task, None, pending)
    }

    /// `task` 由 `worker` 承办时的度支：本件限额与承办者官职的每件限额中较小者。
    pub fn task_limit(&self, task: &Task, worker: Option<AgentId>) -> Option<u64> {
        let office = worker
            .and_then(|w| self.office_of(w))
            .and_then(|o| self.budget(BudgetScope::Office(o)));
        [task.budget, office].into_iter().flatten().min()
    }

    /// 诏令已用尽 `task_limit`（计入尚未记账的 `pending` tokens）则报错。
    pub fn check_task_funds(
        &self,
        task: &Task,
        worker: Option<AgentId>,
        pending: u64,
    ) -> Result<(), GovernanceError> {
        match self.task_limit(task, worker) {
            Some(limit) if task.usage.total() + pending >= limit => {
                Err(GovernanceError::TaskOverBudget {
                    task: task.edict.id,
                    limit,
                })
            }
            _ => Ok(()),
        }
    }
}
