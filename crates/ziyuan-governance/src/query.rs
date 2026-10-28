//! 朝廷状态的查询：官员、诏令、实录、用量与政绩。

use crate::edicts::department_of;
use crate::personnel::Candidate;
use crate::{Court, GovernanceError, ServiceRecord, Task, TaskStatus};
use ziyuan_core::{
    Agent, AgentId, CensorVerdict, GovernanceMode, Office, Record, TaskId, TokenUsage, Verdict,
    default_styles,
};

impl Court {
    pub fn mode(&self) -> GovernanceMode {
        self.mode
    }

    pub fn agent(&self, id: AgentId) -> Option<&Agent> {
        self.agents.get(&id)
    }

    /// 按 ID 升序列出全部在册官员。
    pub fn agents(&self) -> impl Iterator<Item = &Agent> {
        self.agents.values()
    }

    /// 官员的字：皇帝改过的以事件簿为准，否则取 [`default_styles`]。
    pub fn courtesy_name(&self, id: AgentId) -> String {
        self.courtesy_names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| default_styles(id).0)
    }

    /// 官员的号：同 [`Court::courtesy_name`]。
    pub fn art_name(&self, id: AgentId) -> String {
        self.art_names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| default_styles(id).1)
    }

    pub fn office_of(&self, id: AgentId) -> Option<Office> {
        self.appointments.get(&id).copied()
    }

    pub fn appointed_count(&self) -> usize {
        self.appointments.len()
    }

    pub fn completed_count(&self) -> usize {
        self.tasks
            .values()
            .filter(|t| matches!(t.status, TaskStatus::Completed { .. }))
            .count()
    }

    /// 全部诏令的模型 token 用量。
    pub fn total_usage(&self) -> TokenUsage {
        let mut total = TokenUsage::default();
        for task in self.tasks.values() {
            total += task.usage;
        }
        total += self.exam_usage();
        total += self.nomination_usage;
        total
    }

    /// 科举与殿试所用的 token（不属于任何诏令）。
    pub fn exam_usage(&self) -> TokenUsage {
        let mut total = TokenUsage::default();
        for exam in &self.exams {
            total += exam.usage;
        }
        for answer in self.palace_exams.iter().flat_map(|p| &p.answers) {
            total += answer.usage;
        }
        total
    }

    pub fn task(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(&id)
    }

    /// 按 ID 升序列出全部诏令。
    pub fn tasks(&self) -> impl DoubleEndedIterator<Item = &Task> {
        self.tasks.values()
    }

    /// 翰林院实录：已完成的原诏令（子诏令随原诏令归档），按 ID 升序。
    pub fn records(&self) -> Vec<Record> {
        self.tasks
            .values()
            .filter(|t| t.parent.is_none())
            .filter_map(|t| {
                let TaskStatus::Completed { worker, summary } = &t.status else {
                    return None;
                };
                let mut verdict = Vec::new();
                if let Some((v, note)) = &t.review {
                    verdict.push(if note.is_empty() {
                        v.label().to_owned()
                    } else {
                        format!("{}：{note}", v.label())
                    });
                }
                if let Some(c) = &t.censure {
                    verdict.push(format!("都察院{}", c.verdict.label()));
                }
                Some(Record {
                    task: t.edict.id,
                    department: t.edict.department,
                    edict: t.edict.text.clone(),
                    worker: *worker,
                    handler: self
                        .agent(*worker)
                        .map(|a| a.name.clone())
                        .unwrap_or_default(),
                    summary: summary.clone(),
                    verdict: if verdict.is_empty() {
                        "待评定".into()
                    } else {
                        verdict.join("；")
                    },
                })
            })
            .collect()
    }

    /// 未结束的诏令，按 ID 升序。
    pub fn open_tasks(&self) -> Vec<TaskId> {
        self.tasks
            .iter()
            .filter(|(_, t)| t.status.is_open())
            .map(|(id, _)| *id)
            .collect()
    }

    /// 内阁制下尚未批红的待办诏令：须先批红才能执行。子诏令随原诏令批红，自身不批红。
    pub fn needs_approval(&self, task: &Task) -> bool {
        self.mode == GovernanceMode::Cabinet
            && !task.approved
            && task.status == TaskStatus::Pending
            && task.parent.is_none()
    }

    /// 已分办的原诏令中尚未结束的子诏令数。
    pub fn open_subtasks(&self, task: &Task) -> usize {
        task.split.as_ref().map_or(0, |split| {
            split
                .subtasks
                .iter()
                .filter(|id| self.tasks.get(id).is_some_and(|t| t.status.is_open()))
                .count()
        })
    }

    /// 现在就可以执行的诏令：未结束，（内阁制下）已批红；子诏令须原诏令已批红，
    /// 已分办的原诏令须子诏令全部结束（届时由尚书汇总）。按 ID 升序。
    pub fn runnable_tasks(&self) -> Vec<TaskId> {
        let mut tasks = self
            .tasks
            .values()
            .filter(|t| t.status.is_open() && !self.needs_approval(t))
            .filter(|t| {
                t.parent
                    .and_then(|p| self.tasks.get(&p))
                    .is_none_or(|p| !self.needs_approval(p))
            })
            .filter(|t| self.open_subtasks(t) == 0)
            .collect::<Vec<_>>();
        // 兵部：八百里加急插队先行，其余按诏令先后（排序稳定）。
        tasks.sort_by_key(|t| !t.urgent);
        tasks.into_iter().map(|t| t.edict.id).collect()
    }

    pub(crate) fn open_task(&self, id: TaskId) -> Result<&TaskStatus, GovernanceError> {
        let task = self
            .tasks
            .get(&id)
            .ok_or(GovernanceError::UnknownEdict(id))?;
        if !task.status.is_open() {
            return Err(GovernanceError::TaskFinished(id));
        }
        Ok(&task.status)
    }

    pub(crate) fn set_status(&mut self, id: TaskId, status: TaskStatus) {
        if let Some(task) = self.tasks.get_mut(&id) {
            task.status = status;
        }
    }

    /// 吏部考绩：完全由事件簿计算的履历。质量只采信皇帝的评定。
    pub fn service_record(&self, agent: AgentId) -> ServiceRecord {
        let mut record = ServiceRecord::default();
        for task in self.tasks.values() {
            if task.handler() != Some(agent) {
                continue;
            }
            record.tool_calls += task.tool_calls;
            record.usage += task.usage;
            record.tool_failures += task.tool_failures;
            match (&task.status, &task.review) {
                (TaskStatus::Completed { .. }, Some((Verdict::Accepted, _))) => {
                    record.accepted += 1
                }
                (TaskStatus::Completed { .. }, Some((Verdict::Rejected, _))) => {
                    record.rejected += 1
                }
                (TaskStatus::Completed { .. }, None) => record.unreviewed += 1,
                (TaskStatus::Failed { .. }, _) => record.failed += 1,
                _ => {}
            }
            if let Some(censure) = &task.censure {
                match censure.verdict {
                    CensorVerdict::Endorsed => record.endorsed += 1,
                    CensorVerdict::Impeached => record.impeached += 1,
                }
                record.verified += u32::from(censure.evidence.verified());
            }
        }
        for exam in self.exams.iter().filter(|e| e.agent == agent) {
            record.exams += 1;
            record.exams_passed += u32::from(exam.passed());
        }
        let crowned = self
            .palace_exams
            .iter()
            .filter(|p| p.zhuangyuan == Some(agent));
        record.zhuangyuan = crowned.count() as u32;
        record.nominated = self.nomination_votes.get(&agent).copied().unwrap_or(0);
        record.caned = self.canings.get(&agent).map_or(0, |c| c.times);
        record
    }

    /// 吏部荐举：列出可任 `office` 的官员（专长与制度均符合），按已评定的履历排序。
    /// 仅供参考，不产生事件；任命仍须皇帝 `appoint`。
    pub fn recommend(&self, office: Office) -> Result<Vec<Candidate<'_>>, GovernanceError> {
        if !office.exists_in(self.mode) {
            return Err(GovernanceError::OfficeNotInMode(office));
        }
        let mut candidates: Vec<_> = self
            .agents
            .values()
            .filter(|agent| !self.purged.contains(&agent.id))
            .filter(|agent| department_of(office).is_none_or(|d| agent.speciality == d))
            .map(|agent| Candidate {
                agent,
                current: self.office_of(agent.id),
                record: self.service_record(agent.id),
            })
            .collect();
        candidates.sort_by_key(|c| {
            (
                std::cmp::Reverse(c.record.merit()),
                c.record.failed,
                c.record.tool_failures,
                c.agent.id,
            )
        });
        Ok(candidates)
    }

    pub(crate) fn appointed_as(&self, office: Office) -> Option<&Agent> {
        self.appointments.iter().find_map(|(id, o)| {
            if *o == office {
                self.agents.get(id)
            } else {
                None
            }
        })
    }

    /// 正职在任则为正职，否则由副职署理；两者俱缺时为 `None`。返回 (官员, 实际官职)。
    pub fn head_or_deputy(&self, head: Office, deputy: Office) -> Option<(&Agent, Office)> {
        self.appointed_as(head)
            .map(|a| (a, head))
            .or_else(|| self.appointed_as(deputy).map(|a| (a, deputy)))
    }

    pub fn recorded_completions(&self, agent: AgentId) -> usize {
        self.tasks
            .values()
            .filter(|t| matches!(t.status, TaskStatus::Completed { worker, .. } if worker == agent))
            .count()
    }

    /// 派工负载：已完成与执行中的任务数。并发派工时，先派出的任务也计入。
    pub(crate) fn workload(&self, agent: AgentId) -> usize {
        self.tasks
            .values()
            .filter(|t| match t.status {
                TaskStatus::Assigned(worker) | TaskStatus::Completed { worker, .. } => {
                    worker == agent
                }
                _ => false,
            })
            .count()
    }
}
