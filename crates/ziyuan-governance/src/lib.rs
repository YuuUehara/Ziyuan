//! 治理引擎。职权由代码校验，不由 Agent 的角色提示词决定。
use std::collections::BTreeMap;
use std::fmt;
use ziyuan_core::{Agent, AgentId, CourtEvent, Department, Edict, GovernanceMode, Office, TaskId};

/// 违反朝廷规则的操作；回放与提议共用同一套规则。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GovernanceError {
    NotFounded,
    DuplicateAgent(AgentId),
    UnknownAgent(AgentId),
    EmptyName,
    AlreadyAppointed(AgentId),
    NotAppointed(AgentId),
    OfficeNotInMode(Office),
    SpecialityMismatch {
        name: String,
        department: Department,
    },
    OfficeOccupied(Office),
    SameMode(GovernanceMode),
    EmptyEdict,
    DuplicateEdict(TaskId),
    UnknownEdict(TaskId),
    TaskFinished(TaskId),
    TaskNotAssigned(TaskId),
    WrongWorker {
        task: TaskId,
        worker: AgentId,
    },
    Vacant(Office),
}
impl fmt::Display for GovernanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFounded => f.write_str("事件日志必须以 Founded 开始"),
            Self::DuplicateAgent(id) => write!(f, "重复官员 ID: {}", id.0),
            Self::UnknownAgent(id) => write!(f, "官员 #{} 不存在", id.0),
            Self::EmptyName => f.write_str("姓名不能为空"),
            Self::AlreadyAppointed(_) => {
                f.write_str("该官员已有官职。请先执行 dismiss，再重新任命。")
            }
            Self::NotAppointed(_) => f.write_str("该官员未任职"),
            Self::OfficeNotInMode(office) => write!(f, "现行制度不设{}", office.label()),
            Self::SpecialityMismatch { name, department } => {
                write!(f, "{name}的专长不属于{}", department.label())
            }
            Self::OfficeOccupied(office) => write!(f, "{}已有任职者", office.label()),
            Self::SameMode(_) => f.write_str("已经是该制度"),
            Self::EmptyEdict => f.write_str("诏令不能为空"),
            Self::DuplicateEdict(id) => write!(f, "重复诏令 ID: {}", id.0),
            Self::UnknownEdict(id) => write!(f, "未记录下达的诏令 #{}", id.0),
            Self::TaskFinished(id) => write!(f, "诏令 #{} 已结束，不能再变更", id.0),
            Self::TaskNotAssigned(id) => write!(f, "诏令 #{} 未在执行中", id.0),
            Self::WrongWorker { task, worker } => {
                write!(f, "诏令 #{} 并未派给官员 #{}", task.0, worker.0)
            }
            Self::Vacant(Office::Chancellor) => f.write_str("丞相空缺。请先任命丞相，或改制。"),
            Self::Vacant(Office::FirstGrandSecretary) => {
                f.write_str("内阁首辅空缺。请先 appoint 一名首辅。")
            }
            Self::Vacant(office) => write!(f, "{}空缺，无法承接诏令", office.label()),
        }
    }
}
impl std::error::Error for GovernanceError {}

/// 由事件重放得到的朝廷状态。字段私有：唯一的修改途径是 [`Court::apply`]，
/// 它与 `propose_*` 共用同一套规则，因此回放永远不会接受提议阶段会拒绝的事件。
#[derive(Clone, Debug)]
pub struct Court {
    mode: GovernanceMode,
    agents: BTreeMap<AgentId, Agent>,
    appointments: BTreeMap<AgentId, Office>,
    next_task_id: u64,
    next_agent_id: u64,
    tasks: BTreeMap<TaskId, Task>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub edict: Edict,
    pub status: TaskStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    /// 已入队，尚未指派。
    Pending,
    /// 已指派、尚未结束（执行中，或上次运行中断）。
    Assigned(AgentId),
    Completed {
        worker: AgentId,
        summary: String,
    },
    Failed {
        reason: String,
    },
    Cancelled,
}

impl TaskStatus {
    /// 未结束的任务可被（重新）执行或撤销。
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Pending | Self::Assigned(_))
    }
}

impl Court {
    pub fn replay(events: &[CourtEvent]) -> Result<Self, GovernanceError> {
        let mode = match events.first() {
            Some(CourtEvent::Founded(mode)) => *mode,
            _ => return Err(GovernanceError::NotFounded),
        };
        let mut court = Self {
            mode,
            agents: BTreeMap::new(),
            appointments: BTreeMap::new(),
            next_task_id: 1,
            next_agent_id: 1,
            tasks: BTreeMap::new(),
        };
        for event in events {
            court.apply(event)?;
        }
        Ok(court)
    }

    pub fn apply(&mut self, event: &CourtEvent) -> Result<(), GovernanceError> {
        match event {
            CourtEvent::Founded(mode) => self.mode = *mode,
            CourtEvent::AgentRegistered(agent) => {
                if agent.name.trim().is_empty() {
                    return Err(GovernanceError::EmptyName);
                }
                if self.agents.contains_key(&agent.id) {
                    return Err(GovernanceError::DuplicateAgent(agent.id));
                }
                self.next_agent_id = self.next_agent_id.max(agent.id.0 + 1);
                self.agents.insert(agent.id, agent.clone());
            }
            CourtEvent::OfficialAppointed { agent, office } => {
                self.propose_appointment(*agent, *office)?;
                self.appointments.insert(*agent, *office);
            }
            CourtEvent::OfficialDismissed { agent } => {
                self.propose_dismissal(*agent)?;
                self.appointments.remove(agent);
            }
            CourtEvent::InstitutionReformed(mode) => {
                self.propose_reform(*mode)?;
                // 改制时自动撤销最高协调职务；部门官员保留。
                self.appointments.retain(|_, office| {
                    !matches!(office, Office::Chancellor | Office::FirstGrandSecretary)
                });
                self.mode = *mode;
            }
            CourtEvent::EdictIssued(edict) => {
                if edict.text.trim().is_empty() {
                    return Err(GovernanceError::EmptyEdict);
                }
                if self.tasks.contains_key(&edict.id) {
                    return Err(GovernanceError::DuplicateEdict(edict.id));
                }
                self.next_task_id = self.next_task_id.max(edict.id.0 + 1);
                self.tasks.insert(
                    edict.id,
                    Task {
                        edict: edict.clone(),
                        status: TaskStatus::Pending,
                    },
                );
            }
            CourtEvent::TaskAssigned { task, worker } => {
                self.open_task(*task)?;
                if !self.agents.contains_key(worker) {
                    return Err(GovernanceError::UnknownAgent(*worker));
                }
                self.set_status(*task, TaskStatus::Assigned(*worker));
            }
            CourtEvent::ToolInvoked { task, .. } => {
                if !matches!(self.open_task(*task)?, TaskStatus::Assigned(_)) {
                    return Err(GovernanceError::TaskNotAssigned(*task));
                }
            }
            CourtEvent::TaskCompleted {
                task,
                worker,
                summary,
            } => {
                // v0.1 的日志没有 TaskAssigned，直接从 Pending 完成。
                match self.open_task(*task)? {
                    TaskStatus::Assigned(assigned) if assigned != worker => {
                        return Err(GovernanceError::WrongWorker {
                            task: *task,
                            worker: *worker,
                        });
                    }
                    _ => {}
                }
                if !self.agents.contains_key(worker) {
                    return Err(GovernanceError::UnknownAgent(*worker));
                }
                self.set_status(
                    *task,
                    TaskStatus::Completed {
                        worker: *worker,
                        summary: summary.clone(),
                    },
                );
            }
            CourtEvent::TaskFailed { task, reason } => {
                self.open_task(*task)?;
                self.set_status(
                    *task,
                    TaskStatus::Failed {
                        reason: reason.clone(),
                    },
                );
            }
            CourtEvent::TaskCancelled { task } => {
                self.propose_cancellation(*task)?;
                self.set_status(*task, TaskStatus::Cancelled);
            }
        }
        Ok(())
    }

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

    pub fn task(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(&id)
    }

    /// 按 ID 升序列出全部诏令。
    pub fn tasks(&self) -> impl Iterator<Item = &Task> {
        self.tasks.values()
    }

    /// 未结束的诏令，按 ID 升序。
    pub fn open_tasks(&self) -> Vec<TaskId> {
        self.tasks
            .iter()
            .filter(|(_, t)| t.status.is_open())
            .map(|(id, _)| *id)
            .collect()
    }

    fn open_task(&self, id: TaskId) -> Result<&TaskStatus, GovernanceError> {
        let task = self
            .tasks
            .get(&id)
            .ok_or(GovernanceError::UnknownEdict(id))?;
        if !task.status.is_open() {
            return Err(GovernanceError::TaskFinished(id));
        }
        Ok(&task.status)
    }

    fn set_status(&mut self, id: TaskId, status: TaskStatus) {
        if let Some(task) = self.tasks.get_mut(&id) {
            task.status = status;
        }
    }

    pub fn propose_cancellation(&self, task: TaskId) -> Result<CourtEvent, GovernanceError> {
        self.open_task(task)?;
        Ok(CourtEvent::TaskCancelled { task })
    }

    /// 招募新官员，登记身份但不自动授职。
    pub fn propose_recruitment(
        &self,
        name: String,
        speciality: Department,
    ) -> Result<CourtEvent, GovernanceError> {
        if name.trim().is_empty() {
            return Err(GovernanceError::EmptyName);
        }
        Ok(CourtEvent::AgentRegistered(Agent {
            id: AgentId(self.next_agent_id),
            name,
            speciality,
        }))
    }

    /// 皇帝的任命：当前 MVP 只有 CLI 皇帝可以调用此入口。
    pub fn propose_appointment(
        &self,
        agent: AgentId,
        office: Office,
    ) -> Result<CourtEvent, GovernanceError> {
        let person = self
            .agents
            .get(&agent)
            .ok_or(GovernanceError::UnknownAgent(agent))?;
        if self.appointments.contains_key(&agent) {
            return Err(GovernanceError::AlreadyAppointed(agent));
        }
        match office {
            Office::Chancellor if self.mode != GovernanceMode::Chancellor => {
                return Err(GovernanceError::OfficeNotInMode(office));
            }
            Office::FirstGrandSecretary if self.mode != GovernanceMode::Cabinet => {
                return Err(GovernanceError::OfficeNotInMode(office));
            }
            Office::Minister(d) | Office::Clerk(d) if d != person.speciality => {
                return Err(GovernanceError::SpecialityMismatch {
                    name: person.name.clone(),
                    department: d,
                });
            }
            _ => {}
        }
        if !matches!(office, Office::Clerk(_)) && self.appointments.values().any(|o| *o == office) {
            return Err(GovernanceError::OfficeOccupied(office));
        }
        Ok(CourtEvent::OfficialAppointed { agent, office })
    }

    pub fn propose_dismissal(&self, agent: AgentId) -> Result<CourtEvent, GovernanceError> {
        if !self.appointments.contains_key(&agent) {
            return Err(GovernanceError::NotAppointed(agent));
        }
        Ok(CourtEvent::OfficialDismissed { agent })
    }

    pub fn propose_reform(&self, mode: GovernanceMode) -> Result<CourtEvent, GovernanceError> {
        if self.mode == mode {
            return Err(GovernanceError::SameMode(mode));
        }
        Ok(CourtEvent::InstitutionReformed(mode))
    }

    pub fn new_edict(
        &self,
        department: Department,
        text: String,
    ) -> Result<Edict, GovernanceError> {
        if text.trim().is_empty() {
            return Err(GovernanceError::EmptyEdict);
        }
        Ok(Edict {
            id: TaskId(self.next_task_id),
            department,
            text,
        })
    }

    fn appointed_as(&self, office: Office) -> Option<&Agent> {
        self.appointments.iter().find_map(|(id, o)| {
            if *o == office {
                self.agents.get(id)
            } else {
                None
            }
        })
    }

    /// 路由说明是可观察性日志，不是给 Worker 的权限凭证。
    pub fn route(&self, edict: &Edict) -> Result<Route, GovernanceError> {
        let mut path = vec!["皇帝".to_owned()];
        match self.mode {
            GovernanceMode::Chancellor => {
                let agent = self
                    .appointed_as(Office::Chancellor)
                    .ok_or(GovernanceError::Vacant(Office::Chancellor))?;
                path.push(format!("丞相·{}", agent.name));
            }
            GovernanceMode::Cabinet => {
                let agent = self
                    .appointed_as(Office::FirstGrandSecretary)
                    .ok_or(GovernanceError::Vacant(Office::FirstGrandSecretary))?;
                path.push(format!("首辅·{}", agent.name));
            }
            GovernanceMode::DirectRule => {}
        }
        let minister = self
            .appointed_as(Office::Minister(edict.department))
            .ok_or(GovernanceError::Vacant(Office::Minister(edict.department)))?;
        path.push(format!(
            "{}尚书·{}",
            edict.department.label(),
            minister.name
        ));
        // 目前只按任务量均衡派工；未来可接入可靠的考绩评分。
        let worker = self
            .appointments
            .iter()
            .filter(|(_, office)| **office == Office::Clerk(edict.department))
            .filter_map(|(id, _)| self.agents.get(id))
            .min_by_key(|candidate| (self.workload(candidate.id), candidate.id))
            .unwrap_or(minister);
        if worker.id != minister.id {
            path.push(format!("{}属官·{}", edict.department.label(), worker.name));
        }
        Ok(Route {
            path,
            worker: worker.id,
        })
    }

    pub fn recorded_completions(&self, agent: AgentId) -> usize {
        self.tasks
            .values()
            .filter(|t| matches!(t.status, TaskStatus::Completed { worker, .. } if worker == agent))
            .count()
    }

    /// 派工负载：已完成与执行中的任务数。并发派工时，先派出的任务也计入。
    fn workload(&self, agent: AgentId) -> usize {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub path: Vec<String>,
    pub worker: AgentId,
}

/// 最小开局：只有工部与吏部有人，其余四部留空待扩展。
pub fn founding_events() -> Vec<CourtEvent> {
    use CourtEvent::*;
    use Department::*;
    let recruits = [
        (1, "魏衡", Personnel),
        (2, "沈衡", Works),
        (3, "许文", Personnel),
        (4, "陈工", Works),
        (5, "顾吏", Personnel),
        (6, "陆诚", Personnel),
    ];
    let mut events = vec![Founded(GovernanceMode::Chancellor)];
    for (id, name, speciality) in recruits {
        events.push(AgentRegistered(Agent {
            id: AgentId(id),
            name: name.into(),
            speciality,
        }));
    }
    events.extend([
        OfficialAppointed {
            agent: AgentId(1),
            office: Office::Chancellor,
        },
        OfficialAppointed {
            agent: AgentId(2),
            office: Office::Minister(Works),
        },
        OfficialAppointed {
            agent: AgentId(3),
            office: Office::Minister(Personnel),
        },
        OfficialAppointed {
            agent: AgentId(4),
            office: Office::Clerk(Works),
        },
        OfficialAppointed {
            agent: AgentId(5),
            office: Office::Clerk(Personnel),
        },
        OfficialAppointed {
            agent: AgentId(6),
            office: Office::Clerk(Personnel),
        },
    ]);
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reform_changes_route() {
        let mut court = Court::replay(&founding_events()).unwrap();
        let edict = court
            .new_edict(Department::Works, "检查源码".into())
            .unwrap();
        assert!(
            court
                .route(&edict)
                .unwrap()
                .path
                .iter()
                .any(|p| p.contains("丞相"))
        );
        court
            .apply(&court.propose_reform(GovernanceMode::DirectRule).unwrap())
            .unwrap();
        assert!(
            !court
                .route(&edict)
                .unwrap()
                .path
                .iter()
                .any(|p| p.contains("丞相"))
        );
        court
            .apply(&court.propose_reform(GovernanceMode::Cabinet).unwrap())
            .unwrap();
        assert!(court.route(&edict).is_err());
        court
            .apply(&court.propose_dismissal(AgentId(6)).unwrap())
            .unwrap();
        court
            .apply(
                &court
                    .propose_appointment(AgentId(6), Office::FirstGrandSecretary)
                    .unwrap(),
            )
            .unwrap();
        assert!(
            court
                .route(&edict)
                .unwrap()
                .path
                .iter()
                .any(|p| p.contains("首辅"))
        );
    }
    #[test]
    fn cannot_create_two_chancellors() {
        let court = Court::replay(&founding_events()).unwrap();
        let mut court = court;
        court
            .apply(&court.propose_dismissal(AgentId(6)).unwrap())
            .unwrap();
        assert!(
            court
                .propose_appointment(AgentId(6), Office::Chancellor)
                .is_err()
        );
    }
    #[test]
    fn cannot_replay_duplicate_appointment() {
        let mut events = founding_events();
        events.push(CourtEvent::OfficialAppointed {
            agent: AgentId(2),
            office: Office::Minister(Department::Works),
        });
        assert!(Court::replay(&events).is_err());
    }
    #[test]
    fn replay_applies_same_rules_as_proposals() {
        let mut events = founding_events();
        events.push(CourtEvent::OfficialDismissed { agent: AgentId(7) });
        assert_eq!(
            Court::replay(&events).unwrap_err(),
            GovernanceError::NotAppointed(AgentId(7))
        );
        let mut events = founding_events();
        events.push(CourtEvent::InstitutionReformed(GovernanceMode::Chancellor));
        assert!(Court::replay(&events).is_err());
    }
    #[test]
    fn task_lifecycle_is_enforced_on_replay() {
        let task = TaskId(1);
        let mut base = founding_events();
        base.push(CourtEvent::EdictIssued(Edict {
            id: task,
            department: Department::Works,
            text: "查".into(),
        }));
        let with = |extra: &[CourtEvent]| {
            let mut events = base.clone();
            events.extend_from_slice(extra);
            Court::replay(&events)
        };
        let assigned = CourtEvent::TaskAssigned {
            task,
            worker: AgentId(4),
        };
        let tool = CourtEvent::ToolInvoked {
            task,
            tool: "read_file".into(),
            args_json: "{}".into(),
            ok: true,
        };
        let done_by = |worker| CourtEvent::TaskCompleted {
            task,
            worker: AgentId(worker),
            summary: "ok".into(),
        };

        // v0.1 日志：未指派即完成，仍可回放。
        assert!(with(&[done_by(4)]).is_ok());
        assert!(with(&[assigned.clone(), tool.clone(), done_by(4)]).is_ok());
        assert_eq!(
            with(&[assigned.clone(), done_by(2)]).unwrap_err(),
            GovernanceError::WrongWorker {
                task,
                worker: AgentId(2)
            }
        );
        assert_eq!(
            with(std::slice::from_ref(&tool)).unwrap_err(),
            GovernanceError::TaskNotAssigned(task)
        );
        assert_eq!(
            with(&[assigned, CourtEvent::TaskCancelled { task }, tool]).unwrap_err(),
            GovernanceError::TaskFinished(task)
        );
        let court = with(&[CourtEvent::TaskFailed {
            task,
            reason: "x".into(),
        }])
        .unwrap();
        assert!(court.open_tasks().is_empty());
        assert!(court.propose_cancellation(task).is_err());
    }
    #[test]
    fn recruitment_enables_dynamic_worker_pool() {
        let mut court = Court::replay(&founding_events()).unwrap();
        let event = court
            .propose_recruitment("新工".into(), Department::Works)
            .unwrap();
        court.apply(&event).unwrap();
        assert!(court.agents.contains_key(&AgentId(7)));
        court
            .apply(
                &court
                    .propose_appointment(AgentId(7), Office::Clerk(Department::Works))
                    .unwrap(),
            )
            .unwrap();
        let edict = court.new_edict(Department::Works, "测试".into()).unwrap();
        assert_eq!(court.route(&edict).unwrap().worker, AgentId(4));
        let completed = [
            CourtEvent::EdictIssued(edict.clone()),
            CourtEvent::TaskCompleted {
                task: edict.id,
                worker: AgentId(4),
                summary: "demo".into(),
            },
        ];
        for e in &completed {
            court.apply(e).unwrap();
        }
        assert_eq!(court.route(&edict).unwrap().worker, AgentId(7));
    }
    #[test]
    fn appoint_validates_department() {
        let mut court = Court::replay(&founding_events()).unwrap();
        court
            .apply(&court.propose_dismissal(AgentId(4)).unwrap())
            .unwrap();
        assert!(
            court
                .propose_appointment(AgentId(4), Office::Clerk(Department::Personnel))
                .is_err()
        );
    }
}
