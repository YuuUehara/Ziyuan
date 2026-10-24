//! 治理引擎。职权由代码校验，不由 Agent 的角色提示词决定。
pub mod names;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path};
use ziyuan_core::{
    Agent, AgentId, CourtEvent, Department, Edict, GovernanceMode, Office, Seal, TaskId,
    TokenUsage, Verdict,
};

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
    /// 玉玺的查阅范围为空，或含绝对路径、`..` 等越出项目的路径。
    InvalidScope(String),
    DuplicateEdict(TaskId),
    UnknownEdict(TaskId),
    TaskFinished(TaskId),
    TaskNotAssigned(TaskId),
    WrongWorker {
        task: TaskId,
        worker: AgentId,
    },
    /// 只有已完成的诏令可以评定。
    NotCompleted(TaskId),
    AlreadyReviewed(TaskId),
    Vacant(Office),
    /// 已革职的官员永不叙用。
    Purged(AgentId),
    /// 只有内阁制下的诏令需要批红。
    ApprovalNotNeeded,
    AlreadyApproved(TaskId),
    /// 一键朱元璋只能在丞相制下使用。
    NoChancellery,
    /// 模型名为空或含空白。
    InvalidModel(String),
    /// 只有内阁制下才票拟。
    NoCabinet,
    /// 票拟只能拟派本部在任的尚书、侍郎或郎中。
    NotEligible {
        worker: AgentId,
        department: Department,
    },
    /// 诏令的查阅范围全在承办者官职的玉玺上限之外。
    SealExceedsCeiling(Office),
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
            Self::InvalidScope(path) if path.is_empty() => {
                f.write_str("玉玺必须至少给出一个查阅范围")
            }
            Self::InvalidScope(path) => {
                write!(f, "查阅范围必须是项目内的相对路径，且不含 `..`: {path}")
            }
            Self::DuplicateEdict(id) => write!(f, "重复诏令 ID: {}", id.0),
            Self::UnknownEdict(id) => write!(f, "未记录下达的诏令 #{}", id.0),
            Self::TaskFinished(id) => write!(f, "诏令 #{} 已结束，不能再变更", id.0),
            Self::TaskNotAssigned(id) => write!(f, "诏令 #{} 未在执行中", id.0),
            Self::WrongWorker { task, worker } => {
                write!(f, "诏令 #{} 并未派给官员 #{}", task.0, worker.0)
            }
            Self::NotCompleted(id) => write!(f, "诏令 #{} 尚未完成，无从评定", id.0),
            Self::AlreadyReviewed(id) => write!(f, "诏令 #{} 已评定过", id.0),
            Self::Vacant(Office::Chancellor) => {
                f.write_str("中书右丞相、左丞相俱缺。请先任命丞相，或改制。")
            }
            Self::Vacant(Office::FirstGrandSecretary) => {
                f.write_str("内阁首辅、次辅俱缺。请先 appoint 一名首辅。")
            }
            Self::Vacant(Office::Minister(d)) => {
                write!(f, "{0}尚书、{0}侍郎俱缺，无法承接诏令", d.label())
            }
            Self::Vacant(office) => write!(f, "{}空缺，无法承接诏令", office.label()),
            Self::Purged(id) => write!(f, "官员 #{} 已革职，永不叙用", id.0),
            Self::ApprovalNotNeeded => {
                f.write_str("现行制度无须批红：只有内阁制下的票拟须经皇帝批红")
            }
            Self::AlreadyApproved(id) => write!(f, "诏令 #{} 已批红或已在执行", id.0),
            Self::NoChancellery => f.write_str("现行制度不设中书省，已无丞相可废"),
            Self::InvalidModel(m) => write!(f, "模型名不能为空或含空白: {m:?}"),
            Self::NoCabinet => f.write_str("现行制度不设内阁，无从票拟"),
            Self::NotEligible { worker, department } => write!(
                f,
                "官员 #{} 不是{}在任官员，不能拟派",
                worker.0,
                department.label()
            ),
            Self::SealExceedsCeiling(office) => write!(
                f,
                "诏令的查阅范围全在{}的玉玺上限之外，不予承办",
                office.label()
            ),
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
    /// 革职削籍的官员。
    purged: BTreeSet<AgentId>,
    /// 按官职（`Office::code`）指定的承办模型与玉玺上限。
    office_models: BTreeMap<String, String>,
    office_ceilings: BTreeMap<String, Seal>,
    next_task_id: u64,
    next_agent_id: u64,
    tasks: BTreeMap<TaskId, Task>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub edict: Edict,
    pub status: TaskStatus,
    /// 执行中的工具调用次数与其中失败的次数（含越出玉玺的尝试）。
    pub tool_calls: u32,
    pub tool_failures: u32,
    /// 承办期间模型调用的累计用量。
    pub usage: TokenUsage,
    /// 皇帝的评定；只有已完成的诏令才有。
    pub review: Option<(Verdict, String)>,
    /// 皇帝是否已对内阁票拟批红（只在内阁制下要求）。
    pub approved: bool,
    /// 最近一次指派时承办所用的模型（演示后端与早期日志为 `None`）。
    pub model: Option<String>,
    /// 内阁票拟（只在内阁制下产生）。
    pub draft: Option<Draft>,
}

/// 内阁票拟的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// `worker` 为失败时的承办官员；指派前（如路由失败）为 `None`。
    Failed {
        worker: Option<AgentId>,
        reason: String,
    },
    Cancelled,
}

impl Task {
    /// 经手此诏令的官员（执行中、完成或失败时）；撤销不算经手。
    pub fn handler(&self) -> Option<AgentId> {
        match &self.status {
            TaskStatus::Assigned(worker) | TaskStatus::Completed { worker, .. } => Some(*worker),
            TaskStatus::Failed { worker, .. } => *worker,
            // 撤销不计入：可能是皇帝改了主意，不代表官员表现。
            TaskStatus::Pending | TaskStatus::Cancelled => None,
        }
    }
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
            purged: BTreeSet::new(),
            office_models: BTreeMap::new(),
            office_ceilings: BTreeMap::new(),
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
            CourtEvent::OfficialPurged { agent } => {
                self.propose_purge(*agent)?;
                self.appointments.remove(agent);
                self.purged.insert(*agent);
            }
            CourtEvent::EdictDrafted {
                task,
                worker,
                note,
                model,
                usage,
            } => {
                self.propose_draft(*task, *worker, note.clone(), model.clone(), *usage)?;
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                entry.usage += *usage;
                let (mut total, mut model) = (*usage, model.clone());
                if let Some(previous) = &entry.draft {
                    total += previous.usage;
                    model = model.or_else(|| previous.model.clone());
                }
                entry.draft = Some(Draft {
                    worker: *worker,
                    note: note.clone(),
                    model,
                    usage: total,
                });
            }
            CourtEvent::EdictApproved { task } => {
                self.propose_approval(*task)?;
                self.tasks.get_mut(task).expect("已确认存在").approved = true;
            }
            CourtEvent::InstitutionReformed(mode) => {
                self.propose_reform(*mode)?;
                // 改制时撤销旧制度协调机构的官职；六部官员保留（品级随制度变化）。
                self.appointments
                    .retain(|_, office| office.mode().is_none());
                self.mode = *mode;
            }
            CourtEvent::EdictIssued(edict) => {
                if edict.text.trim().is_empty() {
                    return Err(GovernanceError::EmptyEdict);
                }
                validate_seal(&edict.seal)?;
                if self.tasks.contains_key(&edict.id) {
                    return Err(GovernanceError::DuplicateEdict(edict.id));
                }
                self.next_task_id = self.next_task_id.max(edict.id.0 + 1);
                self.tasks.insert(
                    edict.id,
                    Task {
                        edict: edict.clone(),
                        status: TaskStatus::Pending,
                        tool_calls: 0,
                        tool_failures: 0,
                        usage: TokenUsage::default(),
                        review: None,
                        approved: false,
                        model: None,
                        draft: None,
                    },
                );
            }
            CourtEvent::TaskAssigned {
                task,
                worker,
                model,
            } => {
                self.open_task(*task)?;
                if !self.agents.contains_key(worker) {
                    return Err(GovernanceError::UnknownAgent(*worker));
                }
                self.set_status(*task, TaskStatus::Assigned(*worker));
                self.tasks.get_mut(task).expect("已确认存在").model = model.clone();
            }
            CourtEvent::OfficeModelSet { office, model } => {
                self.propose_office_model(*office, model.clone())?;
                match model {
                    Some(m) => self.office_models.insert(office.code(), m.clone()),
                    None => self.office_models.remove(&office.code()),
                };
            }
            CourtEvent::OfficeCeilingSet { office, ceiling } => {
                self.propose_office_ceiling(*office, ceiling.clone())?;
                match ceiling {
                    Some(c) => self.office_ceilings.insert(office.code(), c.clone()),
                    None => self.office_ceilings.remove(&office.code()),
                };
            }
            CourtEvent::ToolInvoked { task, ok, .. } => {
                if !matches!(self.open_task(*task)?, TaskStatus::Assigned(_)) {
                    return Err(GovernanceError::TaskNotAssigned(*task));
                }
                let entry = self.tasks.get_mut(task).expect("open_task 已确认存在");
                entry.tool_calls += 1;
                entry.tool_failures += u32::from(!ok);
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
                let worker = match self.open_task(*task)? {
                    TaskStatus::Assigned(worker) => Some(*worker),
                    _ => None,
                };
                self.set_status(
                    *task,
                    TaskStatus::Failed {
                        worker,
                        reason: reason.clone(),
                    },
                );
            }
            CourtEvent::ModelUsage { task, usage } => {
                let entry = self
                    .tasks
                    .get_mut(task)
                    .ok_or(GovernanceError::UnknownEdict(*task))?;
                if entry.status == TaskStatus::Pending {
                    return Err(GovernanceError::TaskNotAssigned(*task));
                }
                entry.usage += *usage;
            }
            CourtEvent::TaskReviewed {
                task,
                verdict,
                note,
            } => {
                self.propose_review(*task, *verdict, note.clone())?;
                let entry = self.tasks.get_mut(task).expect("propose_review 已确认存在");
                entry.review = Some((*verdict, note.clone()));
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

    /// 全部诏令的模型 token 用量。
    pub fn total_usage(&self) -> TokenUsage {
        let mut total = TokenUsage::default();
        for task in self.tasks.values() {
            total += task.usage;
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

    /// 未结束的诏令，按 ID 升序。
    pub fn open_tasks(&self) -> Vec<TaskId> {
        self.tasks
            .iter()
            .filter(|(_, t)| t.status.is_open())
            .map(|(id, _)| *id)
            .collect()
    }

    /// 内阁制下尚未批红的待办诏令：须先批红才能执行。
    pub fn needs_approval(&self, task: &Task) -> bool {
        self.mode == GovernanceMode::Cabinet && !task.approved && task.status == TaskStatus::Pending
    }

    /// 现在就可以执行的诏令：未结束，且（内阁制下）已批红。按 ID 升序。
    pub fn runnable_tasks(&self) -> Vec<TaskId> {
        self.tasks
            .values()
            .filter(|t| t.status.is_open() && !self.needs_approval(t))
            .map(|t| t.edict.id)
            .collect()
    }

    /// 为官职指定承办模型；`None` 恢复默认。
    pub fn propose_office_model(
        &self,
        office: Office,
        model: Option<String>,
    ) -> Result<CourtEvent, GovernanceError> {
        if let Some(m) = &model
            && (m.is_empty() || m.chars().any(char::is_whitespace))
        {
            return Err(GovernanceError::InvalidModel(m.clone()));
        }
        Ok(CourtEvent::OfficeModelSet { office, model })
    }

    /// 为官职设定玉玺上限；`None` 取消上限。
    pub fn propose_office_ceiling(
        &self,
        office: Office,
        ceiling: Option<Seal>,
    ) -> Result<CourtEvent, GovernanceError> {
        if let Some(seal) = &ceiling {
            validate_seal(seal)?;
        }
        Ok(CourtEvent::OfficeCeilingSet { office, ceiling })
    }

    /// 官职指定的承办模型；未指定时用默认模型。
    pub fn office_model(&self, office: Office) -> Option<&str> {
        self.office_models.get(&office.code()).map(String::as_str)
    }

    pub fn office_ceiling(&self, office: Office) -> Option<&Seal> {
        self.office_ceilings.get(&office.code())
    }

    /// `agent` 承办诏令时所用的模型（按其现任官职）；`None` 表示默认模型。
    pub fn model_of(&self, agent: AgentId) -> Option<&str> {
        self.office_of(agent).and_then(|o| self.office_model(o))
    }

    /// `agent` 承办 `edict` 时实际可用的玉玺：受其现任官职上限约束。
    /// 诏令的查阅范围与上限毫无交集时返回 `Err(SealExceedsCeiling)`。
    pub fn effective_seal(&self, agent: AgentId, edict: &Edict) -> Result<Seal, GovernanceError> {
        let Some(office) = self.office_of(agent) else {
            return Ok(edict.seal.clone());
        };
        match self.office_ceiling(office) {
            None => Ok(edict.seal.clone()),
            Some(ceiling) => edict
                .seal
                .within(ceiling)
                .ok_or(GovernanceError::SealExceedsCeiling(office)),
        }
    }

    pub fn is_purged(&self, agent: AgentId) -> bool {
        self.purged.contains(&agent)
    }

    /// 革职：除去官职并削去仕籍，此后不得再任官。皇帝可革任何在籍官员。
    pub fn propose_purge(&self, agent: AgentId) -> Result<CourtEvent, GovernanceError> {
        if !self.agents.contains_key(&agent) {
            return Err(GovernanceError::UnknownAgent(agent));
        }
        if self.purged.contains(&agent) {
            return Err(GovernanceError::Purged(agent));
        }
        Ok(CourtEvent::OfficialPurged { agent })
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

    /// 一键朱元璋：革去中书省全部丞相，罢中书省，改为废相亲政。
    pub fn propose_abolition(&self) -> Result<Vec<CourtEvent>, GovernanceError> {
        if self.mode != GovernanceMode::Chancellor {
            return Err(GovernanceError::NoChancellery);
        }
        let mut events: Vec<_> = self
            .appointments
            .iter()
            .filter(|(_, office)| office.mode() == Some(GovernanceMode::Chancellor))
            .map(|(&agent, _)| CourtEvent::OfficialPurged { agent })
            .collect();
        events.push(CourtEvent::InstitutionReformed(GovernanceMode::DirectRule));
        Ok(events)
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
        if self.purged.contains(&agent) {
            return Err(GovernanceError::Purged(agent));
        }
        if self.appointments.contains_key(&agent) {
            return Err(GovernanceError::AlreadyAppointed(agent));
        }
        if !office.exists_in(self.mode) {
            return Err(GovernanceError::OfficeNotInMode(office));
        }
        if let Some(d) = department_of(office)
            && d != person.speciality
        {
            return Err(GovernanceError::SpecialityMismatch {
                name: person.name.clone(),
                department: d,
            });
        }
        if office.is_unique() && self.appointments.values().any(|o| *o == office) {
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
        seal: Seal,
    ) -> Result<Edict, GovernanceError> {
        if text.trim().is_empty() {
            return Err(GovernanceError::EmptyEdict);
        }
        validate_seal(&seal)?;
        Ok(Edict {
            id: TaskId(self.next_task_id),
            department,
            text,
            seal,
        })
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
        }
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
                std::cmp::Reverse(c.record.net_accepted()),
                c.record.failed,
                c.record.tool_failures,
                c.agent.id,
            )
        });
        Ok(candidates)
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

    /// 正职在任则为正职，否则由副职署理；两者俱缺时为 `None`。返回 (官员, 实际官职)。
    fn head_or_deputy(&self, head: Office, deputy: Office) -> Option<(&Agent, Office)> {
        self.appointed_as(head)
            .map(|a| (a, head))
            .or_else(|| self.appointed_as(deputy).map(|a| (a, deputy)))
    }

    /// 路由说明是可观察性日志，不是给 Worker 的权限凭证。
    ///
    /// 皇帝 →（中书省或内阁）→ 本部尚书 → 郎中。正职空缺时由副职署理，在路线中注明。
    pub fn route(&self, edict: &Edict) -> Result<Route, GovernanceError> {
        self.route_to(edict, None)
    }

    /// 诏令的派工路线：内阁制下若已票拟且拟派者仍在任，派给拟派者，否则按旧例。
    pub fn route_task(&self, task: &Task) -> Result<Route, GovernanceError> {
        let drafted = task
            .draft
            .as_ref()
            .filter(|_| self.mode == GovernanceMode::Cabinet)
            .map(|d| d.worker);
        self.route_to(&task.edict, drafted)
    }

    fn route_to(&self, edict: &Edict, drafted: Option<AgentId>) -> Result<Route, GovernanceError> {
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
        let drafted = drafted.and_then(|id| staff.iter().find(|(a, _)| a.id == id));
        // 未票拟时按任务量均衡派给郎中；本部无郎中则由尚书（或署理的侍郎）亲办。
        let (worker, office) = drafted.copied().unwrap_or_else(|| {
            staff
                .iter()
                .filter(|(_, office)| *office == Office::Clerk(department))
                .min_by_key(|(candidate, _)| (self.workload(candidate.id), candidate.id))
                .copied()
                .unwrap_or(found)
        });
        let mark = if drafted.is_some() {
            "（票拟）"
        } else {
            ""
        };
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

/// 查阅范围只能是项目内的相对路径。路径是否存在、是否经符号链接逃逸，由运行时在构造工具时检查。
fn validate_seal(seal: &Seal) -> Result<(), GovernanceError> {
    if seal.scope.is_empty() {
        return Err(GovernanceError::InvalidScope(String::new()));
    }
    for path in &seal.scope {
        let escapes = Path::new(path)
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir));
        if path.trim().is_empty() || escapes {
            return Err(GovernanceError::InvalidScope(path.clone()));
        }
    }
    Ok(())
}

/// 一位官员经手诏令的统计。`unreviewed` 不计入评价：未经皇帝评定的回奏不算政绩。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServiceRecord {
    pub accepted: u32,
    pub rejected: u32,
    pub unreviewed: u32,
    /// 承办时失败（含超时）。
    pub failed: u32,
    pub tool_calls: u32,
    /// 失败的工具调用，含越出玉玺的尝试。
    pub tool_failures: u32,
    /// 经手诏令（含失败）的模型 token 用量。
    pub usage: TokenUsage,
}

impl ServiceRecord {
    /// 荐举排序的主要依据：采纳数减驳回数。
    pub fn net_accepted(&self) -> i64 {
        i64::from(self.accepted) - i64::from(self.rejected)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate<'a> {
    pub agent: &'a Agent,
    /// 现任官职；荐举不会自动罢免或调任。
    pub current: Option<Office>,
    pub record: ServiceRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub path: Vec<String>,
    pub worker: AgentId,
}

/// 六部官职所属的部门。
fn department_of(office: Office) -> Option<Department> {
    match office {
        Office::Minister(d) | Office::ViceMinister(d) | Office::Clerk(d) => Some(d),
        _ => None,
    }
}

/// 最小开局（丞相制）：只有工部与吏部有人，其余四部留空待扩展。官员姓名由 `seed` 随机生成。
pub fn founding_events(seed: u64) -> Vec<CourtEvent> {
    use CourtEvent::*;
    use Department::*;
    let mut generator = names::NameGenerator::new(seed);
    let mut taken: Vec<String> = Vec::new();
    let recruits = [Personnel, Works, Personnel, Works, Personnel, Personnel]
        .into_iter()
        .zip(1..)
        .map(|(speciality, id)| {
            let name = generator.name(|n| taken.iter().any(|t| t == n));
            taken.push(name.clone());
            (id, name, speciality)
        })
        .collect::<Vec<_>>();
    let mut events = vec![Founded(GovernanceMode::Chancellor)];
    for (id, name, speciality) in recruits {
        events.push(AgentRegistered(Agent {
            id: AgentId(id),
            name,
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
        let mut court = Court::replay(&founding_events(1)).unwrap();
        let edict = court
            .new_edict(Department::Works, "检查源码".into(), Seal::default())
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
    fn deputies_act_for_vacant_heads_and_ranks_follow_the_mode() {
        let mut court = Court::replay(&founding_events(1)).unwrap();
        let works = |court: &Court| {
            let edict = court
                .new_edict(Department::Works, "查".into(), Seal::default())
                .unwrap();
            court.route(&edict).map(|r| r.path)
        };
        let apply =
            |court: &mut Court, propose: &dyn Fn(&Court) -> Result<CourtEvent, GovernanceError>| {
                let event = propose(court).unwrap();
                court.apply(&event).unwrap()
            };
        // 丞相制：右丞相（#1）罢免后，左丞相署理；尚书（#2）罢免后，侍郎署理。
        apply(&mut court, &|c| c.propose_dismissal(AgentId(1)));
        assert_eq!(
            works(&court).unwrap_err(),
            GovernanceError::Vacant(Office::Chancellor)
        );
        apply(&mut court, &|c| {
            c.propose_appointment(AgentId(1), Office::ViceChancellor)
        });
        apply(&mut court, &|c| c.propose_dismissal(AgentId(2)));
        assert_eq!(
            works(&court).unwrap_err(),
            GovernanceError::Vacant(Office::Minister(Department::Works))
        );
        apply(&mut court, &|c| {
            c.propose_appointment(AgentId(2), Office::ViceMinister(Department::Works))
        });
        let path = works(&court).unwrap();
        let names: Vec<String> = court.agents().map(|a| a.name.clone()).collect();
        let name = |id: usize| names[id - 1].clone();
        assert_eq!(
            path,
            [
                "皇帝".to_owned(),
                format!("中书左丞相·{}（署理）", name(1)),
                format!("工部侍郎·{}（署理）", name(2)),
                format!("工部郎中·{}", name(4)),
            ]
        );
        let minister = Office::Minister(Department::Works);
        assert_eq!(minister.rank(court.mode()), "正三品");

        // 废相亲政：中书省官职随之撤销，六部官职保留并升为明制品级；内阁官职不可任命。
        apply(&mut court, &|c| {
            c.propose_reform(GovernanceMode::DirectRule)
        });
        assert_eq!(court.office_of(AgentId(1)), None);
        assert_eq!(
            court.office_of(AgentId(2)),
            Some(Office::ViceMinister(Department::Works))
        );
        assert_eq!(minister.rank(court.mode()), "正二品");
        assert_eq!(works(&court).unwrap().len(), 3);
        assert_eq!(
            court.propose_appointment(AgentId(1), Office::SecondGrandSecretary),
            Err(GovernanceError::OfficeNotInMode(
                Office::SecondGrandSecretary
            ))
        );
        // 内阁制：次辅在首辅空缺时署理。
        apply(&mut court, &|c| c.propose_reform(GovernanceMode::Cabinet));
        apply(&mut court, &|c| {
            c.propose_appointment(AgentId(1), Office::SecondGrandSecretary)
        });
        assert_eq!(
            works(&court).unwrap()[1],
            format!("内阁次辅·{}（署理）", name(1))
        );
    }
    #[test]
    fn abolition_purges_chancellors_and_cabinet_edicts_need_approval() {
        let mut court = Court::replay(&founding_events(1)).unwrap();
        let apply = |events: Vec<CourtEvent>, court: &mut Court| {
            for e in events {
                court.apply(&e).unwrap();
            }
        };
        // 一键朱元璋：右丞相 #1 被革职，改为亲政；被革者不能再任官，也不入荐举。
        apply(court.propose_abolition().unwrap(), &mut court);
        assert_eq!(court.mode(), GovernanceMode::DirectRule);
        assert!(court.is_purged(AgentId(1)) && court.office_of(AgentId(1)).is_none());
        assert!(!court.is_purged(AgentId(2)));
        assert_eq!(
            court.propose_abolition(),
            Err(GovernanceError::NoChancellery)
        );
        apply(
            vec![court.propose_reform(GovernanceMode::Cabinet).unwrap()],
            &mut court,
        );
        assert_eq!(
            court.propose_appointment(AgentId(1), Office::FirstGrandSecretary),
            Err(GovernanceError::Purged(AgentId(1)))
        );
        assert!(
            court
                .recommend(Office::FirstGrandSecretary)
                .unwrap()
                .iter()
                .all(|c| c.agent.id != AgentId(1))
        );
        assert_eq!(
            court.propose_purge(AgentId(1)),
            Err(GovernanceError::Purged(AgentId(1)))
        );

        // 内阁制：未批红的诏令不可执行；批红一次后可执行，不能重复批红。
        apply(
            vec![court.propose_dismissal(AgentId(6)).unwrap()],
            &mut court,
        );
        apply(
            vec![
                court
                    .propose_appointment(AgentId(6), Office::FirstGrandSecretary)
                    .unwrap(),
            ],
            &mut court,
        );
        let edict = court
            .new_edict(Department::Works, "查".into(), Seal::default())
            .unwrap();
        let id = edict.id;
        apply(vec![CourtEvent::EdictIssued(edict)], &mut court);
        assert!(court.runnable_tasks().is_empty());
        assert_eq!(court.open_tasks(), [id]);
        // 票拟只能拟派本部在任官员；拟派尚书后，派工改给尚书（路线注明票拟），不再按旧例给郎中。
        let none = TokenUsage::default();
        assert_eq!(
            court.propose_draft(id, AgentId(3), String::new(), None, none),
            Err(GovernanceError::NotEligible {
                worker: AgentId(3),
                department: Department::Works
            })
        );
        assert_eq!(
            court.route(&court.task(id).unwrap().edict).unwrap().worker,
            AgentId(4)
        );
        let used = TokenUsage {
            input: 100,
            output: 10,
        };
        apply(
            vec![
                court
                    .propose_draft(id, AgentId(2), "尚书亲办".into(), Some("pro".into()), used)
                    .unwrap(),
            ],
            &mut court,
        );
        let route = court.route_task(court.task(id).unwrap()).unwrap();
        assert_eq!(route.worker, AgentId(2));
        assert!(
            route.path.last().unwrap().ends_with("（票拟）"),
            "{:?}",
            route.path
        );
        let task = court.task(id).unwrap();
        assert_eq!(
            (task.usage, task.draft.as_ref().unwrap().usage),
            (used, used)
        );
        apply(vec![court.propose_approval(id).unwrap()], &mut court);
        assert_eq!(court.runnable_tasks(), [id]);
        assert_eq!(
            court.propose_approval(id),
            Err(GovernanceError::AlreadyApproved(id))
        );
        // 其他制度不批红，诏令直接可执行。
        apply(
            vec![court.propose_reform(GovernanceMode::DirectRule).unwrap()],
            &mut court,
        );
        assert_eq!(
            court.propose_approval(id),
            Err(GovernanceError::ApprovalNotNeeded)
        );
    }
    #[test]
    fn cannot_create_two_chancellors() {
        let court = Court::replay(&founding_events(1)).unwrap();
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
        let mut events = founding_events(1);
        events.push(CourtEvent::OfficialAppointed {
            agent: AgentId(2),
            office: Office::Minister(Department::Works),
        });
        assert!(Court::replay(&events).is_err());
    }
    #[test]
    fn replay_applies_same_rules_as_proposals() {
        let mut events = founding_events(1);
        events.push(CourtEvent::OfficialDismissed { agent: AgentId(7) });
        assert_eq!(
            Court::replay(&events).unwrap_err(),
            GovernanceError::NotAppointed(AgentId(7))
        );
        let mut events = founding_events(1);
        events.push(CourtEvent::InstitutionReformed(GovernanceMode::Chancellor));
        assert!(Court::replay(&events).is_err());
    }
    #[test]
    fn libu_counts_only_reviewed_results_and_ranks_by_them() {
        let mut events = founding_events(1);
        events.push(CourtEvent::AgentRegistered(Agent {
            id: AgentId(7),
            name: "新工".into(),
            speciality: Department::Works,
        }));
        let mut next = 1;
        // (承办者, 是否完成, 评定, 失败的工具调用数)
        let mut handle = |worker: u64, ok: bool, verdict: Option<Verdict>, bad_tools: u32| {
            let task = TaskId(next);
            next += 1;
            events.push(CourtEvent::EdictIssued(Edict {
                id: task,
                department: Department::Works,
                text: "查".into(),
                seal: Seal::default(),
            }));
            events.push(CourtEvent::TaskAssigned {
                task,
                worker: AgentId(worker),
                model: None,
            });
            events.push(CourtEvent::ModelUsage {
                task,
                usage: TokenUsage {
                    input: 100,
                    output: 7,
                },
            });
            for _ in 0..bad_tools {
                events.push(CourtEvent::ToolInvoked {
                    task,
                    tool: "read_file".into(),
                    args_json: "{}".into(),
                    ok: false,
                });
            }
            events.push(if ok {
                CourtEvent::TaskCompleted {
                    task,
                    worker: AgentId(worker),
                    summary: "ok".into(),
                }
            } else {
                CourtEvent::TaskFailed {
                    task,
                    reason: "超时".into(),
                }
            });
            if let Some(verdict) = verdict {
                events.push(CourtEvent::TaskReviewed {
                    task,
                    verdict,
                    note: String::new(),
                });
            }
        };
        // 工部郎中 #4 完成数最多，但多为驳回或未评定；新工（#7）完成少但被采纳。
        handle(4, true, Some(Verdict::Rejected), 0);
        handle(4, true, None, 0);
        handle(4, true, None, 0);
        handle(4, false, None, 2);
        handle(7, true, Some(Verdict::Accepted), 1);
        let court = Court::replay(&events).unwrap();

        assert_eq!(
            court.service_record(AgentId(4)),
            ServiceRecord {
                accepted: 0,
                rejected: 1,
                unreviewed: 2,
                failed: 1,
                tool_calls: 2,
                tool_failures: 2,
                // 4 件（含失败的一件）各 100/7。
                usage: TokenUsage {
                    input: 400,
                    output: 28,
                },
            }
        );
        assert_eq!(court.total_usage().total(), 5 * 107);
        let ranked: Vec<_> = court
            .recommend(Office::Clerk(Department::Works))
            .unwrap()
            .iter()
            .map(|c| c.agent.id.0)
            .collect();
        // 新工居首；工部尚书 #2（无履历）在被驳回过的 #4 之前；吏部官员不入选。
        assert_eq!(ranked, [7, 2, 4]);
        assert!(court.recommend(Office::FirstGrandSecretary).is_err());

        // 只能评定已完成且未评定过的诏令。
        assert_eq!(
            court.propose_review(TaskId(4), Verdict::Accepted, String::new()),
            Err(GovernanceError::NotCompleted(TaskId(4)))
        );
        assert_eq!(
            court.propose_review(TaskId(5), Verdict::Rejected, String::new()),
            Err(GovernanceError::AlreadyReviewed(TaskId(5)))
        );
        assert!(
            court
                .propose_review(TaskId(2), Verdict::Accepted, "好".into())
                .is_ok()
        );
    }
    #[test]
    fn seal_scope_must_stay_inside_project() {
        let court = Court::replay(&founding_events(1)).unwrap();
        let seal = |scope: &[&str]| Seal {
            capabilities: Default::default(),
            scope: scope.iter().map(|s| s.to_string()).collect(),
        };
        let issue = |scope: &[&str]| court.new_edict(Department::Works, "查".into(), seal(scope));
        assert!(issue(&["."]).is_ok());
        assert!(issue(&["./crates/ziyuan-storage", "README.md"]).is_ok());
        for bad in ["/etc", "../secret", "src/../../x", " "] {
            assert_eq!(
                issue(&["src", bad]).unwrap_err(),
                GovernanceError::InvalidScope(bad.into()),
                "{bad}"
            );
        }
        assert!(issue(&[]).is_err());
        // 回放同样拒绝：手写的日志也不能绕过。
        let mut events = founding_events(1);
        events.push(CourtEvent::EdictIssued(Edict {
            id: TaskId(1),
            department: Department::Works,
            text: "查".into(),
            seal: seal(&["/"]),
        }));
        assert!(Court::replay(&events).is_err());
    }
    #[test]
    fn task_lifecycle_is_enforced_on_replay() {
        let task = TaskId(1);
        let mut base = founding_events(1);
        base.push(CourtEvent::EdictIssued(Edict {
            id: task,
            department: Department::Works,
            text: "查".into(),
            seal: Seal::default(),
        }));
        let with = |extra: &[CourtEvent]| {
            let mut events = base.clone();
            events.extend_from_slice(extra);
            Court::replay(&events)
        };
        let assigned = CourtEvent::TaskAssigned {
            task,
            worker: AgentId(4),
            model: None,
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

        let usage = CourtEvent::ModelUsage {
            task,
            usage: TokenUsage {
                input: 1,
                output: 1,
            },
        };
        assert_eq!(
            with(std::slice::from_ref(&usage)).unwrap_err(),
            GovernanceError::TaskNotAssigned(task)
        );
        // 结束后仍可补记用量（超时后才返回的调用同样消耗了 token）。
        assert!(with(&[assigned.clone(), CourtEvent::TaskCancelled { task }, usage]).is_ok());
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
        let mut court = Court::replay(&founding_events(1)).unwrap();
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
        let edict = court
            .new_edict(Department::Works, "测试".into(), Seal::default())
            .unwrap();
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
        let mut court = Court::replay(&founding_events(1)).unwrap();
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
