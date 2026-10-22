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
    AlreadyCompleted(TaskId),
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
            Self::AlreadyCompleted(id) => write!(f, "诏令 #{} 已完成，拒绝重复记功", id.0),
            Self::Vacant(Office::Chancellor) => f.write_str("丞相空缺。请先任命丞相，或改制。"),
            Self::Vacant(Office::FirstGrandSecretary) => {
                f.write_str("内阁首辅空缺。请先 appoint 一名首辅。")
            }
            Self::Vacant(office) => write!(f, "{}空缺，无法承接诏令", office.label()),
        }
    }
}
impl std::error::Error for GovernanceError {}

#[derive(Clone, Debug)]
pub struct Court {
    pub mode: GovernanceMode,
    pub agents: BTreeMap<AgentId, Agent>,
    pub appointments: BTreeMap<AgentId, Office>,
    pub next_task_id: u64,
    pub next_agent_id: u64,
    pub issued: BTreeMap<TaskId, Edict>,
    pub completions: BTreeMap<TaskId, (AgentId, String)>,
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
            issued: BTreeMap::new(),
            completions: BTreeMap::new(),
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
                self.appointments.remove(agent);
            }
            CourtEvent::InstitutionReformed(mode) => {
                // 改制时自动撤销最高协调职务；部门官员保留。
                self.appointments.retain(|_, office| {
                    !matches!(office, Office::Chancellor | Office::FirstGrandSecretary)
                });
                self.mode = *mode;
            }
            CourtEvent::EdictIssued(edict) => {
                if self.issued.contains_key(&edict.id) {
                    return Err(GovernanceError::DuplicateEdict(edict.id));
                }
                self.next_task_id = self.next_task_id.max(edict.id.0 + 1);
                self.issued.insert(edict.id, edict.clone());
            }
            CourtEvent::TaskCompleted {
                task,
                worker,
                summary,
            } => {
                if !self.issued.contains_key(task) {
                    return Err(GovernanceError::UnknownEdict(*task));
                }
                if !self.agents.contains_key(worker) {
                    return Err(GovernanceError::UnknownAgent(*worker));
                }
                if self.completions.contains_key(task) {
                    return Err(GovernanceError::AlreadyCompleted(*task));
                }
                self.completions.insert(*task, (*worker, summary.clone()));
            }
        }
        Ok(())
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

    pub fn appointed_as(&self, office: Office) -> Option<&Agent> {
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
        // 目前只按已记录任务量均衡派工；未来可接入可靠的考绩评分。
        let worker = self
            .appointments
            .iter()
            .filter(|(_, office)| **office == Office::Clerk(edict.department))
            .filter_map(|(id, _)| self.agents.get(id))
            .min_by_key(|candidate| (self.recorded_completions(candidate.id), candidate.id))
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
        self.completions
            .values()
            .filter(|(worker, _)| *worker == agent)
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
