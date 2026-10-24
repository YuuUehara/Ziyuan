//! 应用服务层：CLI 与未来的 UI 共用的用例入口。
//!
//! 每次状态变更都遵循同一流程：治理层提议事件 → 在 `Court` 副本上校验 → 写入事件簿 →
//! 更新内存状态。因此被拒绝的操作不会留下任何事件，写入的事件也一定能被回放。
//!
//! 诏令分两步：[`Session::issue`] 入队（路由不通则拒绝、不留记录），
//! [`Session::run`] 指派并交给 [`Scheduler`] 并发执行，逐条记录进度与结局。
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use ziyuan_core::{
    AgentId, CensorVerdict, CourtEvent, Department, Edict, Evidence, FileChange, GovernanceMode,
    Office, Price, Seal, TaskId, TokenUsage, Verdict,
};
use ziyuan_governance::names::NameGenerator;
use ziyuan_governance::{
    ChangeState, Court, GovernanceError, ServiceRecord, Task, TaskStatus, founding_events,
};
use ziyuan_runtime::{CancelToken, Job, Outcome, Progress, Scheduler, WorkerBackend};
use ziyuan_storage::{EventStore, StorageError};

#[derive(Debug)]
pub enum AppError {
    NotFounded,
    AlreadyFounded,
    Governance(GovernanceError),
    Storage(StorageError),
    /// 落盘失败（如文件自呈报以来已被改动）；事件簿不变。
    Apply(String),
}
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFounded => f.write_str("朝廷尚未建立，请先在项目目录执行 `ziyuan init`"),
            Self::AlreadyFounded => f.write_str("朝廷已存在，拒绝覆盖日志"),
            Self::Governance(e) => e.fmt(f),
            Self::Storage(e) => e.fmt(f),
            Self::Apply(e) => write!(f, "落盘失败：{e}"),
        }
    }
}
impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotFounded | Self::AlreadyFounded | Self::Apply(_) => None,
            Self::Governance(e) => Some(e),
            Self::Storage(e) => Some(e),
        }
    }
}
impl From<GovernanceError> for AppError {
    fn from(e: GovernanceError) -> Self {
        Self::Governance(e)
    }
}
impl From<StorageError> for AppError {
    fn from(e: StorageError) -> Self {
        Self::Storage(e)
    }
}

/// 一件诏令在本次运行中的结局。
#[derive(Clone, Debug)]
pub struct TaskReport {
    pub edict: Edict,
    /// 派工路线；运行时路由失败则为空。
    pub path: Vec<String>,
    pub outcome: Outcome,
    /// 该诏令承办至今累计的模型 token 用量（含以往运行，不含内阁票拟）。
    pub usage: TokenUsage,
    /// 最近一次承办所用的模型。
    pub model: Option<String>,
}

/// [`Session::run`] 执行期间的实时进度。
#[derive(Clone, Copy, Debug)]
pub enum RunUpdate<'a> {
    /// 诏令已指派给承办官员，开始执行（已记入事件簿）。
    Assigned(TaskId),
    /// 承办官员（模型）流式生成的文本片段；只用于显示，不写入事件簿。
    Text { task: TaskId, text: &'a str },
    /// 一件诏令在本次运行中的结局（每件恰好一次）。
    Finished(&'a TaskReport),
}

/// 一个已打开的朝廷：事件簿与由它重放出的当前状态。
pub struct Session<S: EventStore> {
    store: S,
    court: Court,
    history: Vec<CourtEvent>,
}

impl<S: EventStore> Session<S> {
    /// 在空事件簿上建立默认朝廷（丞相制）。并发 init 只有一个成功。
    pub fn found(mut store: S) -> Result<Self, AppError> {
        let history = founding_events(random_seed());
        let court = Court::replay(&history)?;
        match store.append(0, &history) {
            Err(StorageError::Conflict { .. }) => return Err(AppError::AlreadyFounded),
            result => result?,
        }
        Ok(Self {
            store,
            court,
            history,
        })
    }

    /// 打开已有朝廷并重放其事件簿。
    pub fn open(store: S) -> Result<Self, AppError> {
        let history = store.load()?;
        if history.is_empty() {
            return Err(AppError::NotFounded);
        }
        let court = Court::replay(&history)?;
        Ok(Self {
            store,
            court,
            history,
        })
    }

    pub fn court(&self) -> &Court {
        &self.court
    }

    pub fn history(&self) -> &[CourtEvent] {
        &self.history
    }

    /// 重新加载事件簿，看到其他进程（或会话）写入的变更。
    pub fn reload(&mut self) -> Result<(), AppError> {
        let history = self.store.load()?;
        self.court = Court::replay(&history)?;
        self.history = history;
        Ok(())
    }

    /// 招募新官员（不授官职），返回其 ID。`name` 为 `None` 时随机取一个未被占用的姓名。
    pub fn recruit(
        &mut self,
        name: Option<String>,
        speciality: Department,
    ) -> Result<AgentId, AppError> {
        let seed = random_seed();
        let committed = self.commit(|court| {
            let name = name.clone().unwrap_or_else(|| {
                NameGenerator::new(seed).name(|n| court.agents().any(|a| a.name == n))
            });
            Ok(vec![court.propose_recruitment(name, speciality)?])
        })?;
        match committed.first() {
            Some(CourtEvent::AgentRegistered(agent)) => Ok(agent.id),
            _ => unreachable!("propose_recruitment 只产生 AgentRegistered"),
        }
    }

    pub fn appoint(&mut self, agent: AgentId, office: Office) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_appointment(agent, office)?]))?;
        Ok(())
    }

    pub fn dismiss(&mut self, agent: AgentId) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_dismissal(agent)?]))?;
        Ok(())
    }

    /// 改制，返回史官对这次改制的记述（见 [`reform_note`]）。
    pub fn reform(&mut self, mode: GovernanceMode) -> Result<&'static str, AppError> {
        let mut from = self.court.mode();
        self.commit(|court| {
            from = court.mode();
            Ok(vec![court.propose_reform(mode)?])
        })?;
        Ok(reform_note(from, mode))
    }

    /// 革职：除去官职、削去仕籍，永不叙用。
    pub fn purge(&mut self, agent: AgentId) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_purge(agent)?]))?;
        Ok(())
    }

    /// 内阁制下对内阁票拟批红，诏令方可执行。
    pub fn approve(&mut self, task: TaskId) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_approval(task)?]))?;
        Ok(())
    }

    /// 内阁票拟：首辅（以其官职所配模型，否则默认模型）读诏令，从本部在任官员中拟定承办人，
    /// 附票拟意见记入事件簿。模型不可用、回复未指明合格人选时，按旧例（任务量均衡）拟派并在
    /// 意见中注明。阻塞直到模型返回。返回拟派的官员。
    pub fn draft(
        &mut self,
        task: TaskId,
        backend: &dyn WorkerBackend,
    ) -> Result<AgentId, AppError> {
        let court = &self.court;
        let entry = court
            .task(task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        // 旧例人选；同时校验制度、诏令状态与内阁、本部是否有人。
        let fallback = court.route(&entry.edict)?.worker;
        court.propose_draft(task, fallback, String::new(), None, TokenUsage::default())?;
        let (secretary, office) = court.coordinator().expect("路由已确认内阁有人");
        let candidates = court.draft_candidates(entry.edict.department);
        let model = court
            .model_of(secretary.id)
            .or(backend.model_name())
            .map(str::to_owned);
        let system = format!(
            "你是紫垣朝廷{}{}，为皇帝票拟：从候选官员中拟定一位承办下面这件诏令，并简述理由。\n\
             只能从候选名单中选。回复第一行必须是「拟派：#官员ID」，其后用一两句话说明理由，不超过 100 字。",
            office.label(),
            secretary.name
        );
        let mut user = format!(
            "诏令（{}）：{}\n玉玺：{}\n候选：\n",
            entry.edict.department.label(),
            entry.edict.text,
            entry.edict.seal
        );
        for (agent, office) in &candidates {
            user.push_str(&format!(
                "#{} {} {}，{}\n",
                agent.id.0,
                agent.name,
                office.label(),
                describe_record(&court.service_record(agent.id))
            ));
        }
        let (worker, note, model, usage) = match backend.complete(model.as_deref(), &system, &user)
        {
            Ok((reply, usage)) => {
                let reply: String = reply.trim().chars().take(600).collect();
                match parse_draft(&reply).filter(|id| candidates.iter().any(|(a, _)| a.id == *id)) {
                    Some(id) => (id, reply, model, usage),
                    None => (
                        fallback,
                        format!("票拟未指明合格人选，按旧例拟派。原票：{reply}"),
                        model,
                        usage,
                    ),
                }
            }
            Err(e) => (
                fallback,
                format!("内阁未能票拟（{e}），按旧例拟派。"),
                None,
                TokenUsage::default(),
            ),
        };
        self.commit(|court| {
            Ok(vec![court.propose_draft(
                task,
                worker,
                note.clone(),
                model.clone(),
                usage,
            )?])
        })?;
        Ok(worker)
    }

    /// 都察院复核一件已完成的诏令：先机械核验证据（回奏引用的文件与行号是否存在于 `root`
    /// 下的项目、最近一次 `run_tests` 是否通过），再请左都御史（其官职所配模型，否则默认模型）
    /// 据此背书或弹劾（核验结果另计入考绩的「实证」，不随御史结论改变）。模型不可用（含演示后端）或回复无结论时，
    /// 按证据例：有据可查则背书，否则弹劾。阻塞直到模型返回。
    pub fn censor(
        &mut self,
        task: TaskId,
        root: &Path,
        backend: &dyn WorkerBackend,
    ) -> Result<CensorVerdict, AppError> {
        self.reload()?;
        let court = &self.court;
        let censor = court
            .censor()
            .ok_or(GovernanceError::Vacant(Office::Censor))?
            .clone();
        court.propose_censure_check(task, censor.id)?;
        let entry = court.task(task).expect("已确认存在");
        let (evidence, findings) = check_evidence(root, entry, &self.history);
        let model = court
            .model_of(censor.id)
            .or(backend.model_name())
            .map(str::to_owned);
        let handler = entry
            .handler()
            .and_then(|id| court.agent(id))
            .map_or("?".to_owned(), |a| format!("#{} {}", a.id.0, a.name));
        let summary = match &entry.status {
            TaskStatus::Completed { summary, .. } => summary.as_str(),
            _ => "",
        };
        let system = format!(
            "你是紫垣朝廷都察院左都御史{}，独立复核官员的回奏，风闻亦须核实。\n\
             依据诏令、回奏、工具调用记录与证据核验，判断回奏是否属实、是否办成了诏令所命之事。\n\
             回复第一行必须是「背书」或「弹劾」二字之一，其后用不超过 150 字说明理由。\n\
             证据核验由程序完成、确凿可信：查无实据的引用要分辨是捏造，还是官员如实说明其不存在；\
             测试未通过而回奏称已办成者，应予弹劾。",
            censor.name
        );
        let mut user = format!(
            "诏令 #{}（{}）：{}\n玉玺：{}\n承办：{handler}\n\n回奏：\n{}\n\n工具调用：\n",
            task.0,
            entry.edict.department.label(),
            entry.edict.text,
            entry.edict.seal,
            truncate(summary, 6000),
        );
        for (tool, args, ok) in tool_calls(&self.history, task) {
            let mark = if ok { "成功" } else { "失败" };
            user.push_str(&format!("- {tool} {}（{mark}）\n", truncate(&args, 200)));
        }
        user.push_str(&format!("\n证据核验：{evidence}\n"));
        for line in &findings {
            user.push_str(&format!("- {line}\n"));
        }
        if let Some(changes) = &entry.changes {
            let diff: String = changes.files.iter().map(render_diff).collect();
            user.push_str(&format!("\n呈报改动：\n{}\n", truncate(&diff, 6000)));
        }
        let by_evidence = if evidence.verified() {
            CensorVerdict::Endorsed
        } else {
            CensorVerdict::Impeached
        };
        let (verdict, note, model, usage) = match backend.complete(model.as_deref(), &system, &user)
        {
            Ok((reply, usage)) => {
                let reply = truncate(reply.trim(), 600);
                match parse_censure(&reply) {
                    Some(verdict) => (verdict, reply, model, usage),
                    None => (
                        by_evidence,
                        format!(
                            "御史未下结论，按证据例{}。原文：{reply}",
                            by_evidence.label()
                        ),
                        model,
                        usage,
                    ),
                }
            }
            Err(e) => (
                by_evidence,
                format!(
                    "都察院未能以模型复核（{e}），按证据例{}：{evidence}。",
                    by_evidence.label()
                ),
                None,
                TokenUsage::default(),
            ),
        };
        self.commit(|court| {
            court.propose_censure_check(task, censor.id)?;
            Ok(vec![CourtEvent::CensorReported {
                task,
                censor: censor.id,
                verdict,
                note: note.clone(),
                evidence,
                model: model.clone(),
                usage,
            }])
        })?;
        Ok(verdict)
    }

    /// 御笔改票：皇帝改定拟派的承办人（仍须批红）。
    pub fn redraft(&mut self, task: TaskId, worker: AgentId, note: String) -> Result<(), AppError> {
        let note = if note.trim().is_empty() {
            "御笔改票".to_owned()
        } else {
            format!("御笔改票：{}", note.trim())
        };
        self.commit(|court| {
            Ok(vec![court.propose_draft(
                task,
                worker,
                note.clone(),
                None,
                TokenUsage::default(),
            )?])
        })?;
        Ok(())
    }

    /// 为官职指定承办模型；`None` 恢复默认。
    pub fn set_office_model(
        &mut self,
        office: Office,
        model: Option<String>,
    ) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_office_model(office, model.clone())?]))?;
        Ok(())
    }

    /// 为官职设定玉玺上限；`None` 取消上限。
    pub fn set_office_ceiling(
        &mut self,
        office: Office,
        ceiling: Option<Seal>,
    ) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_office_ceiling(office, ceiling.clone())?]))?;
        Ok(())
    }

    /// 一键朱元璋：罢中书省，株连百官（全部在任官员革职削籍），改为废相亲政。
    /// 返回株连的文字记述（丞相在前，各附原官职）。
    pub fn abolish_chancellery(&mut self) -> Result<String, AppError> {
        let mut offices = BTreeMap::new();
        let committed = self.commit(|court| {
            offices = court
                .agents()
                .filter_map(|a| Some((a.id, court.office_of(a.id)?)))
                .collect();
            court.propose_abolition()
        })?;
        let mut purged: Vec<_> = committed
            .iter()
            .filter_map(|e| match e {
                CourtEvent::OfficialPurged { agent } => Some((*agent, offices.get(agent)?)),
                _ => None,
            })
            .collect();
        if purged.is_empty() {
            return Ok("朝中本无在任官员。".into());
        }
        purged.sort_by_key(|(_, office)| office.mode() != Some(GovernanceMode::Chancellor));
        let names: Vec<_> = purged
            .iter()
            .map(|(id, office)| {
                let name = self.court.agent(*id).map_or("?", |a| a.name.as_str());
                format!("{}{name}", office.label())
            })
            .collect();
        Ok(format!(
            "株连所及，凡 {} 人：{}，一并革职削籍，永不叙用。六部为之一空，请重新招募任官。",
            names.len(),
            names.join("、")
        ))
    }

    /// 下诏入队。现行制度下无法路由（如职位空缺）时拒绝，不留记录。
    /// `seal` 是承办官员的权限令牌（玉玺），随诏令记入事件簿。
    pub fn issue(
        &mut self,
        department: Department,
        text: String,
        seal: Seal,
    ) -> Result<Edict, AppError> {
        let committed = self.commit(|court| {
            let edict = court.new_edict(department, text.clone(), seal.clone())?;
            court.route(&edict)?;
            Ok(vec![CourtEvent::EdictIssued(edict)])
        })?;
        match committed.into_iter().next() {
            Some(CourtEvent::EdictIssued(edict)) => Ok(edict),
            _ => unreachable!("issue 只产生 EdictIssued"),
        }
    }

    /// 皇帝评定一件已完成的诏令。吏部考绩只采信这些评定。
    pub fn review(&mut self, task: TaskId, verdict: Verdict, note: String) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_review(task, verdict, note.clone())?]))?;
        Ok(())
    }

    /// 准奏：把诏令呈报的改动写入 `root` 下的项目，再记入事件簿。写入前逐一核对文件
    /// 自呈报以来未被改动，否则一处也不写、不留记录。
    pub fn apply_changes(&mut self, task: TaskId, root: &Path) -> Result<(), AppError> {
        self.reload()?;
        self.court.propose_changes_decision(task, true)?;
        let files = &self.court.task(task).expect("已确认存在");
        let files = &files.changes.as_ref().expect("已确认有待决改动").files;
        ziyuan_runtime::tools::apply_changes(root, files)
            .map_err(|e| AppError::Apply(e.to_string()))?;
        self.commit(|court| Ok(vec![court.propose_changes_decision(task, true)?]))?;
        Ok(())
    }

    /// 驳回诏令呈报的改动：不写入项目。
    pub fn discard_changes(&mut self, task: TaskId) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_changes_decision(task, false)?]))?;
        Ok(())
    }

    /// 撤销尚未结束的诏令。
    ///
    /// 若该诏令正由另一进程执行，执行不会被打断，但其结果会被作废、不再记录。
    pub fn cancel(&mut self, task: TaskId) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_cancellation(task)?]))?;
        Ok(())
    }

    /// 按当前制度逐件指派 `tasks`，交给调度器并发执行，并经 `on_update` 报告流式文本与每件的结局。
    ///
    /// 结局都会记入事件簿：完成、失败（含运行时路由失败与超时）、撤销。
    /// `cancel` 触发时尚未开始的诏令保持未结束，可下次再运行。
    /// 期间已被其他进程撤销的诏令会被跳过；执行中被撤销的，结果作废并报告为撤销。
    pub async fn run(
        &mut self,
        tasks: &[TaskId],
        scheduler: &Scheduler,
        cancel: &CancelToken,
        mut on_update: impl FnMut(RunUpdate<'_>),
    ) -> Result<(), AppError> {
        let mut jobs = Vec::new();
        let mut running = Vec::new();
        let default_model = scheduler.backend().model_name().map(str::to_owned);
        for &task in tasks {
            let mut path = Vec::new();
            let mut sealed = None;
            let committed = self.commit(|court| {
                let entry = court
                    .task(task)
                    .ok_or(GovernanceError::UnknownEdict(task))?;
                // 已结束，或内阁制下尚未批红：跳过。
                if !entry.status.is_open() || court.needs_approval(entry) {
                    return Ok(Vec::new());
                }
                // 玉玺受承办者官职的上限约束；毫无交集则不予承办。
                // 内阁制下按票拟派工（拟派者已不在任则按旧例）。
                let assigned = court.route_task(entry).and_then(|route| {
                    let seal = court.effective_seal(route.worker, &entry.edict)?;
                    Ok((route, seal))
                });
                Ok(vec![match assigned {
                    Ok((mut route, seal)) => {
                        if seal != entry.edict.seal {
                            let office = court.office_of(route.worker).expect("有上限即有官职");
                            route
                                .path
                                .push(format!("玉玺受{}上限所限：{seal}", office.label()));
                        }
                        path = route.path;
                        sealed = Some(seal);
                        CourtEvent::TaskAssigned {
                            task,
                            worker: route.worker,
                            // 演示后端不调用模型：不记官职指定的模型，以免误导花费估算。
                            model: default_model.as_ref().map(|default| {
                                court.model_of(route.worker).unwrap_or(default).to_owned()
                            }),
                        }
                    }
                    Err(error) => CourtEvent::TaskFailed {
                        task,
                        reason: error.to_string(),
                    },
                }])
            })?;
            let Some(event) = committed.into_iter().next() else {
                continue;
            };
            let edict = self.court.task(task).expect("刚提交的诏令").edict.clone();
            match event {
                CourtEvent::TaskAssigned { worker, model, .. } => {
                    let actor = self
                        .court
                        .agent(worker)
                        .expect("路由只指向在册官员")
                        .clone();
                    jobs.push(Job {
                        actor,
                        edict: Edict {
                            seal: sealed.take().expect("指派时已算出玉玺"),
                            ..edict.clone()
                        },
                        model,
                    });
                    running.push((edict, path));
                    on_update(RunUpdate::Assigned(task));
                }
                CourtEvent::TaskFailed { reason, .. } => {
                    on_update(RunUpdate::Finished(&TaskReport {
                        edict,
                        path: Vec::new(),
                        outcome: Outcome::Failed(reason),
                        usage: TokenUsage::default(),
                        model: None,
                    }))
                }
                _ => unreachable!("指派只产生 TaskAssigned 或 TaskFailed"),
            }
        }

        let mut progress = scheduler.start(jobs, cancel);
        while !running.is_empty() {
            let Some(update) = progress.recv().await else {
                break;
            };
            match update {
                Progress::Usage { task, usage } => {
                    // 结束后才返回的调用同样消耗了 token，照记不误。
                    self.commit(|court| {
                        Ok(match court.task(task).map(|t| &t.status) {
                            Some(TaskStatus::Pending) | None => Vec::new(),
                            Some(_) => vec![CourtEvent::ModelUsage { task, usage }],
                        })
                    })?;
                }
                Progress::Changes { task, changes } => {
                    // 超时/撤销后才呈报的改动作废。
                    self.commit(|court| {
                        Ok(match court.task(task).map(|t| &t.status) {
                            Some(TaskStatus::Assigned(_)) => vec![CourtEvent::ChangesProposed {
                                task,
                                changes: changes.clone(),
                            }],
                            _ => Vec::new(),
                        })
                    })?;
                }
                Progress::TextDelta { task, text } => {
                    // 超时/撤销后仍在收尾的线程可能继续输出；只转发仍在执行的诏令。
                    if running.iter().any(|(e, _)| e.id == task) {
                        on_update(RunUpdate::Text { task, text: &text });
                    }
                }
                Progress::ToolInvoked {
                    task,
                    tool,
                    args_json,
                    ok,
                } => {
                    // 超时/撤销后，后端线程仍可能报告工具调用；此时任务已结束，不再记录。
                    self.commit(|court| {
                        Ok(match court.task(task).map(|t| &t.status) {
                            Some(TaskStatus::Assigned(_)) => vec![CourtEvent::ToolInvoked {
                                task,
                                tool: tool.clone(),
                                args_json: args_json.clone(),
                                ok,
                            }],
                            _ => Vec::new(),
                        })
                    })?;
                }
                Progress::Finished { task, outcome } => {
                    let Some(index) = running.iter().position(|(e, _)| e.id == task) else {
                        continue;
                    };
                    let (edict, path) = running.swap_remove(index);
                    let committed = self.commit(|court| {
                        let Some(TaskStatus::Assigned(worker)) =
                            court.task(task).map(|t| &t.status)
                        else {
                            return Ok(Vec::new());
                        };
                        Ok(match &outcome {
                            Outcome::Completed(summary) => vec![CourtEvent::TaskCompleted {
                                task,
                                worker: *worker,
                                summary: summary.clone(),
                            }],
                            Outcome::Failed(reason) => vec![CourtEvent::TaskFailed {
                                task,
                                reason: reason.clone(),
                            }],
                            Outcome::Cancelled => vec![CourtEvent::TaskCancelled { task }],
                            Outcome::NotStarted => Vec::new(),
                        })
                    })?;
                    // 执行期间被其他进程撤销：结果作废。
                    let outcome = if committed.is_empty() && outcome != Outcome::NotStarted {
                        Outcome::Cancelled
                    } else {
                        outcome
                    };
                    let entry = self.court.task(task);
                    on_update(RunUpdate::Finished(&TaskReport {
                        edict,
                        path,
                        outcome,
                        usage: entry.map(execution_usage).unwrap_or_default(),
                        model: entry.and_then(|t| t.model.clone()),
                    }));
                }
            }
        }
        Ok(())
    }

    /// 反复执行全部可执行的诏令（内阁制下须已批红），直到队列清空：执行期间新入队（或新批红）的
    /// 诏令在本轮结束后接着执行。`cancel` 触发、或一轮过后只剩未开始的诏令时停止。
    pub async fn run_open(
        &mut self,
        scheduler: &Scheduler,
        cancel: &CancelToken,
        mut on_update: impl FnMut(RunUpdate<'_>),
    ) -> Result<(), AppError> {
        let mut previous = Vec::new();
        loop {
            self.reload()?;
            let tasks = self.court.runnable_tasks();
            if tasks.is_empty() || tasks == previous || cancel.is_cancelled() {
                return Ok(());
            }
            self.run(&tasks, scheduler, cancel, &mut on_update).await?;
            previous = tasks;
        }
    }

    /// 由 `propose` 基于当前状态生成事件，校验后写入。事件簿若已被其他进程更新，
    /// 则重新加载并重新生成，因此 ID 分配与规则检查总是基于最新状态。返回实际写入的事件。
    fn commit(
        &mut self,
        mut propose: impl FnMut(&Court) -> Result<Vec<CourtEvent>, GovernanceError>,
    ) -> Result<Vec<CourtEvent>, AppError> {
        loop {
            let events = propose(&self.court)?;
            if events.is_empty() {
                return Ok(events);
            }
            let mut next = self.court.clone();
            for event in &events {
                next.apply(event)?;
            }
            match self.store.append(self.history.len(), &events) {
                Ok(()) => {
                    self.court = next;
                    self.history.extend(events.iter().cloned());
                    return Ok(events);
                }
                Err(StorageError::Conflict { .. }) => {
                    self.history = self.store.load()?;
                    self.court = Court::replay(&self.history)?;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// 每次调用都不同的随机种子（标准库的 `RandomState` 由系统随机数初始化）。
fn random_seed() -> u64 {
    use std::hash::{BuildHasher, RandomState};
    RandomState::new().hash_one(std::time::SystemTime::now())
}

// ---- 呈现：CLI、Web 与 TUI 共用的中文描述 ----

/// 承办所用的 token：诏令累计用量减去内阁票拟的部分。
fn execution_usage(task: &Task) -> TokenUsage {
    let drafting = task
        .draft
        .as_ref()
        .map_or_else(TokenUsage::default, |d| d.usage);
    let censoring = task
        .censure
        .as_ref()
        .map_or_else(TokenUsage::default, |c| c.usage);
    TokenUsage {
        input: task
            .usage
            .input
            .saturating_sub(drafting.input + censoring.input),
        output: task
            .usage
            .output
            .saturating_sub(drafting.output + censoring.output),
    }
}

/// 按各诏令实际所用的模型估算花费，如「约 0.0123 元」。`default_model` 用于未记下模型的早期诏令；
/// 查不到单价的诏令不计入。全部查不到（或没有用量）时返回 `None`。
pub fn estimate_cost<'a>(
    tasks: impl IntoIterator<Item = &'a Task>,
    default_model: Option<&str>,
    price: impl Fn(&str) -> Option<Price>,
) -> Option<String> {
    let mut total = None::<(f64, String)>;
    for task in tasks {
        // 票拟、承办与复核可能用不同模型，分别计价。
        let (drafting, draft_model) = task
            .draft
            .as_ref()
            .map_or((TokenUsage::default(), None), |d| {
                (d.usage, d.model.as_deref())
            });
        let (censoring, censor_model) = task
            .censure
            .as_ref()
            .map_or((TokenUsage::default(), None), |c| {
                (c.usage, c.model.as_deref())
            });
        let executing = execution_usage(task);
        for (usage, model) in [
            (executing, task.model.as_deref()),
            (drafting, draft_model),
            (censoring, censor_model),
        ] {
            if usage.total() == 0 {
                continue;
            }
            let Some(p) = model.or(default_model).and_then(&price) else {
                continue;
            };
            let entry = total.get_or_insert((0.0, p.currency.clone()));
            entry.0 += p.cost(usage);
        }
    }
    total.map(|(cost, currency)| format!("约 {cost:.4} {currency}"))
}

/// 从票拟回复中取拟派的官员 ID：「拟派」之后（没有则全文）的第一个 `#数字`。
fn parse_draft(reply: &str) -> Option<AgentId> {
    let after = reply.find("拟派").map_or(reply, |i| &reply[i..]);
    let (start, mark) = after
        .char_indices()
        .find(|(_, c)| matches!(c, '#' | '＃'))?;
    let digits: String = after[start + mark.len_utf8()..]
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().map(AgentId)
}

/// 一处改动的统一 diff（每段前后各留 3 行上下文）。
pub fn render_diff(change: &FileChange) -> String {
    const CONTEXT: usize = 3;
    let old: Vec<&str> = change
        .before
        .as_deref()
        .map_or(Vec::new(), |b| b.lines().collect());
    let new: Vec<&str> = change.after.lines().collect();
    // (标记, 原文行号, 内容)：' ' 不变，'-' 删除，'+' 新增。
    let mut ops: Vec<(char, usize, &str)> = Vec::new();
    if old.len().saturating_mul(new.len()) > 4_000_000 {
        // 太大时不求最小差异：整段删去、整段新增。
        ops.extend(old.iter().enumerate().map(|(i, l)| ('-', i, *l)));
        ops.extend(new.iter().map(|l| ('+', old.len(), *l)));
    } else {
        // 最长公共子序列：lcs[i][j] 为 old[i..] 与 new[j..] 的公共行数。
        let (n, m) = (old.len(), new.len());
        let mut lcs = vec![vec![0u32; m + 1]; n + 1];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[i][j] = if old[i] == new[j] {
                    lcs[i + 1][j + 1] + 1
                } else {
                    lcs[i + 1][j].max(lcs[i][j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && old[i] == new[j] {
                ops.push((' ', i, old[i]));
                (i, j) = (i + 1, j + 1);
            } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
                ops.push(('-', i, old[i]));
                i += 1;
            } else {
                ops.push(('+', i, new[j]));
                j += 1;
            }
        }
    }
    let changed: Vec<usize> = (0..ops.len()).filter(|&k| ops[k].0 != ' ').collect();
    let shown = |k: usize| changed.iter().any(|&c| c.abs_diff(k) <= CONTEXT);
    let mut out = match &change.before {
        Some(_) => format!("--- a/{0}\n+++ b/{0}\n", change.path),
        None => format!("--- /dev/null\n+++ b/{}\n", change.path),
    };
    let mut previous = None;
    for (k, (mark, line, text)) in ops.iter().enumerate() {
        if !shown(k) {
            continue;
        }
        if previous != k.checked_sub(1) || previous.is_none() {
            out.push_str(&format!("@@ 原第 {} 行 @@\n", line + 1));
        }
        out.push_str(&format!("{mark}{text}\n"));
        previous = Some(k);
    }
    out
}

/// 一键朱元璋时的史官记述。
pub const ABOLITION_NOTE: &str = "洪武十三年正月，丞相胡惟庸以谋反伏诛，株连者三万余人。即日罢中书省，升六部，天子亲裁庶务。——一键朱元璋：丞相，从此不再是问题。";

/// 史官对一次改制的记述（埋了些历史梗）。
pub fn reform_note(from: GovernanceMode, to: GovernanceMode) -> &'static str {
    use GovernanceMode::*;
    match (from, to) {
        (Chancellor, DirectRule) => {
            "洪武十三年，罢中书省，析其权归六部。太祖垂训：「以后嗣君毋得议置丞相，臣下敢以此请者，置之重典。」"
        }
        (_, Cabinet) => {
            "建文四年六月，燕军渡江，金川门开。「侄儿，四叔来看你了。」新君命解缙、杨士奇等入直文渊阁，参预机务——内阁自此始。今后诏令须经内阁票拟、皇帝批红，方可施行。"
        }
        (Cabinet, Chancellor) => {
            "正统十四年，土木堡之变，天子北狩。瓦剌铁骑南下叩关，中书省竟随之复设——大元，杀回来了。"
        }
        (Cabinet, DirectRule) => {
            "崇祯：「朕非亡国之君，诸臣皆亡国之臣。」十七年间换了五十位大学士，这回索性一个不要，事必躬亲。"
        }
        (DirectRule, Chancellor) => {
            "《皇明祖训》：「以后子孙做皇帝时，并不许立丞相。臣下敢有奏请设立者，文武群臣即时劾奏，将犯人凌迟，全家处死。」太祖的棺材板，快压不住了。"
        }
        _ => "",
    }
}

/// 默认玉玺不额外显示，保持输出简洁。
pub fn seal_note(seal: &Seal) -> String {
    if *seal == Seal::default() {
        String::new()
    } else {
        format!("〔玉玺 {seal}〕")
    }
}

pub fn describe_record(r: &ServiceRecord) -> String {
    format!(
        "采纳 {}，驳回 {}，失败 {}，待评定 {}；御史背书 {}，弹劾 {}；实证 {}",
        r.accepted, r.rejected, r.failed, r.unreviewed, r.endorsed, r.impeached, r.verified
    )
}

/// 考成等第：采纳、背书、实证为功，驳回、弹劾、失败为过。
pub fn kaocheng_grade(r: &ServiceRecord) -> &'static str {
    let handled = r.accepted + r.rejected + r.unreviewed + r.failed;
    let score = r.merit() - i64::from(r.failed);
    match () {
        _ if handled == 0 => "无事可考",
        _ if score >= 2 => "称职",
        _ if score < 0 => "不及",
        _ => "平常",
    }
}

/// 考成簿（仿万历初张居正考成法：立限考事，以事责人）：在任官员逐一列出功过与等第。
/// 只是呈现，不入事件簿，也不自动黜陟。
pub fn kaocheng_report(court: &Court) -> String {
    let mut out = String::from(match court.mode() {
        GovernanceMode::Cabinet => {
            "考成簿　万历元年，首辅张居正上《请稽查章奏随事考成以修实政疏》：立限考事，以事责人，月有考，岁有稽。"
        }
        GovernanceMode::DirectRule => {
            "考成簿　考成法本是内阁首辅张居正所行；今已废相，无人代劳，只好由天子亲自考成。"
        }
        GovernanceMode::Chancellor => {
            "考成簿　元无考成之法，今借明制，由中书省代为考成——丞相考百官，谁来考丞相？"
        }
    });
    out.push('\n');
    let mut secretary_failed = false;
    let mut any = false;
    for agent in court.agents() {
        let Some(office) = court.office_of(agent.id) else {
            continue;
        };
        any = true;
        let record = court.service_record(agent.id);
        let grade = kaocheng_grade(&record);
        secretary_failed |= office == Office::FirstGrandSecretary && grade == "不及";
        out.push_str(&format!(
            "#{} {}（{}）：{} —— {grade}\n",
            agent.id.0,
            agent.name,
            office.label(),
            describe_record(&record)
        ));
    }
    if !any {
        out.push_str("朝中无一人在任，无事可考。\n");
    }
    if secretary_failed {
        out.push_str("首辅亦考成不及。万历十年张居正卒，次年即被抄家，考成法随之而废。\n");
    }
    out.push_str("称职者可擢，不及者宜黜；然黜陟出自圣裁，考成簿不罢免任何人。");
    out
}

/// 截取前 `max` 个字符，超出时注明。
fn truncate(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().nth(max).is_some() {
        out.push_str("……（下略）");
    }
    out
}

/// 诏令最近一次指派以来的工具调用：(工具, 参数, 成败)。
fn tool_calls(history: &[CourtEvent], task: TaskId) -> Vec<(String, String, bool)> {
    let start = history
        .iter()
        .rposition(|e| matches!(e, CourtEvent::TaskAssigned { task: t, .. } if *t == task))
        .unwrap_or(0);
    history[start..]
        .iter()
        .filter_map(|e| match e {
            CourtEvent::ToolInvoked {
                task: t,
                tool,
                args_json,
                ok,
            } if *t == task => Some((tool.clone(), args_json.clone(), *ok)),
            _ => None,
        })
        .collect()
}

/// 机械核验回奏的证据，返回汇总与逐条结果。
///
/// 引用：回奏中形如 `path:行` 或 `path:起-止` 的文件位置，行号在范围内即属实。不带行号的
/// 路径只在查无此文件时计入（捏造）；只含 `/`、不带扩展名的路径不存在时不计，以免把
/// 「和/或」之类误作引用。
/// 呈报待决的改动按改动后的内容核对。测试：最近一次 `run_tests` 的成败。
fn check_evidence(root: &Path, task: &Task, history: &[CourtEvent]) -> (Evidence, Vec<String>) {
    let summary = match &task.status {
        TaskStatus::Completed { summary, .. } => summary.as_str(),
        _ => "",
    };
    let staged = |path: &str| {
        let changes = task.changes.as_ref()?;
        if changes.state != ChangeState::Proposed {
            return None;
        }
        let file = changes.files.iter().find(|f| f.path == path)?;
        Some(file.after.clone())
    };
    let mut evidence = Evidence::default();
    let mut findings = Vec::new();
    for (path, lines) in citations(summary) {
        let target = root.join(&path);
        let label = match lines {
            Some((a, b)) if a == b => format!("{path}:{a}"),
            Some((a, b)) => format!("{path}:{a}-{b}"),
            None => path.clone(),
        };
        let escapes = Path::new(&path).components().any(|c| {
            !matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        });
        let has_extension = Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.len() >= 2 && e.starts_with(|c: char| c.is_ascii_alphabetic()));
        let content = if escapes {
            None
        } else {
            staged(&path).or_else(|| {
                let meta = std::fs::metadata(&target).ok()?;
                if meta.is_dir() {
                    return Some(String::new());
                }
                (meta.len() <= 4 << 20).then(|| std::fs::read_to_string(&target).ok())?
            })
        };
        let found = match (&content, lines) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(text), Some((_, end))) => end <= text.lines().count(),
        };
        if !found && !has_extension && !escapes {
            continue;
        }
        // 只提到确实存在的文件而不给行号，证明不了什么，不计为证据（捏造的文件名仍算查无实据）。
        if found && lines.is_none() {
            findings.push(format!("{label} 存在（未给行号，不计为证据）"));
            continue;
        }
        evidence.cited += 1;
        if found {
            findings.push(format!("{label} 属实"));
        } else {
            evidence.missing += 1;
            let why = if content.is_some() {
                "行号越界"
            } else {
                "文件不存在"
            };
            findings.push(format!("{label} 查无实据（{why}）"));
        }
    }
    evidence.tests = tool_calls(history, task.edict.id)
        .into_iter()
        .rev()
        .find(|(tool, _, _)| tool == ziyuan_core::Capability::RunTests.code())
        .map(|(_, _, ok)| ok);
    (evidence, findings)
}

/// 回奏中引用的文件位置（去重，最多 50 处）：(相对路径, 行号范围)。
fn citations(text: &str) -> Vec<(String, Option<(usize, usize)>)> {
    const DENIED: &[&str] = &[".ziyuan", ".git", ".jj"];
    let mut out: Vec<(String, Option<(usize, usize)>)> = Vec::new();
    let tokens = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | ':' | '-')));
    for token in tokens {
        // 只去掉句末的标点；开头的 `.`（`./`、`../`、`.env`）有意义。
        let token = token
            .trim_end_matches(['.', ':', '-'])
            .trim_start_matches('-');
        let (path, rest) = token.split_once(':').unwrap_or((token, ""));
        let path = path.trim_end_matches('/').trim_start_matches("./");
        let lines = match rest.split_once('-') {
            _ if rest.is_empty() => None,
            Some((a, b)) => a.parse().ok().zip(b.parse().ok()),
            None => rest.parse().ok().map(|n| (n, n)),
        };
        let file_like = path.contains('/') || {
            let ext = path.rsplit_once('.');
            ext.is_some_and(|(stem, ext)| {
                !stem.is_empty()
                    && ext.len() >= 2
                    && ext.starts_with(|c: char| c.is_ascii_alphabetic())
            })
        };
        let denied = path
            .split('/')
            .any(|p| DENIED.contains(&p) || p.starts_with(".env"));
        if path.is_empty()
            || !file_like
            || denied
            || path.starts_with('/')
            || path.starts_with('-')
            || !path.contains(|c: char| c.is_ascii_alphabetic())
            || lines.is_some_and(|(a, b): (usize, usize)| a == 0 || b < a)
        {
            continue;
        }
        let entry = (path.to_owned(), lines);
        if !out.contains(&entry) {
            out.push(entry);
            if out.len() == 50 {
                break;
            }
        }
    }
    out
}

/// 御史回复中最先出现的「背书」或「弹劾」。
fn parse_censure(reply: &str) -> Option<CensorVerdict> {
    let endorse = reply.find("背书");
    let impeach = reply.find("弹劾");
    match (endorse, impeach) {
        (Some(e), Some(i)) if i < e => Some(CensorVerdict::Impeached),
        (Some(_), _) => Some(CensorVerdict::Endorsed),
        (None, Some(_)) => Some(CensorVerdict::Impeached),
        (None, None) => None,
    }
}

pub fn describe_event(event: &CourtEvent) -> String {
    match event {
        CourtEvent::Founded(m) => format!("开国：{}", m.label()),
        CourtEvent::AgentRegistered(a) => format!("登记官员 #{} {}", a.id.0, a.name),
        CourtEvent::OfficialAppointed { agent, office } => {
            format!("任命 #{} 为{}", agent.0, office.label())
        }
        CourtEvent::OfficialDismissed { agent } => format!("罢免官员 #{}", agent.0),
        CourtEvent::OfficialPurged { agent } => format!("革职 #{}，削去仕籍", agent.0),
        CourtEvent::EdictApproved { task } => format!("诏令 #{} 批红", task.0),
        CourtEvent::EdictDrafted {
            task,
            worker,
            model,
            ..
        } => match model {
            Some(model) => format!(
                "诏令 #{} 内阁票拟：拟派 #{}（模型 {model}）",
                task.0, worker.0
            ),
            None => format!("诏令 #{} 票拟：拟派 #{}", task.0, worker.0),
        },
        CourtEvent::InstitutionReformed(m) => format!("改制：{}", m.label()),
        CourtEvent::EdictIssued(e) => {
            format!("诏令 #{}：{}{}", e.id.0, e.text, seal_note(&e.seal))
        }
        CourtEvent::TaskAssigned {
            task,
            worker,
            model,
        } => match model {
            Some(model) => format!("诏令 #{} 派给 #{}（模型 {model}）", task.0, worker.0),
            None => format!("诏令 #{} 派给 #{}", task.0, worker.0),
        },
        CourtEvent::OfficeModelSet { office, model } => match model {
            Some(model) => format!("{}改用模型 {model}", office.label()),
            None => format!("{}恢复默认模型", office.label()),
        },
        CourtEvent::OfficeCeilingSet { office, ceiling } => match ceiling {
            Some(seal) => format!("{}玉玺上限：{seal}", office.label()),
            None => format!("{}取消玉玺上限", office.label()),
        },
        CourtEvent::ToolInvoked {
            task,
            tool,
            args_json,
            ok,
        } => format!(
            "诏令 #{} 调用 {tool} {args_json}（{}）",
            task.0,
            if *ok { "成功" } else { "失败" }
        ),
        CourtEvent::TaskCompleted { task, worker, .. } => {
            format!("诏令 #{} 由 #{} 回奏", task.0, worker.0)
        }
        CourtEvent::TaskFailed { task, reason } => format!("诏令 #{} 失败：{reason}", task.0),
        CourtEvent::TaskCancelled { task } => format!("诏令 #{} 撤销", task.0),
        CourtEvent::ChangesProposed { task, changes } => {
            let paths: Vec<_> = changes.iter().map(|c| c.path.as_str()).collect();
            format!("诏令 #{} 呈报改动：{}", task.0, paths.join("、"))
        }
        CourtEvent::ChangesApplied { task } => format!("诏令 #{} 改动准奏落盘", task.0),
        CourtEvent::CensorReported {
            task,
            censor,
            verdict,
            evidence,
            ..
        } => format!(
            "都察院 #{} 复核诏令 #{}：{}（{evidence}）",
            censor.0,
            task.0,
            verdict.label()
        ),
        CourtEvent::ChangesDiscarded { task } => format!("诏令 #{} 改动驳回", task.0),
        CourtEvent::ModelUsage { task, usage } => format!("诏令 #{} 模型用量：{usage}", task.0),
        CourtEvent::TaskReviewed {
            task,
            verdict,
            note,
        } => {
            let note = if note.is_empty() {
                String::new()
            } else {
                format!("：{note}")
            };
            format!("诏令 #{} 评定为{}{note}", task.0, verdict.label())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use ziyuan_core::Capability;
    use ziyuan_runtime::{BoxError, DemoBackend, ExecutionHooks, Limits, WorkerBackend};
    use ziyuan_storage::{EventLog, MemoryEventLog};

    fn scheduler(backend: impl WorkerBackend + 'static) -> Scheduler {
        Scheduler::new(
            Arc::new(backend),
            Limits {
                max_concurrent: 4,
                timeout: Duration::from_secs(10),
            },
        )
    }

    async fn run_all<S: EventStore>(
        session: &mut Session<S>,
        scheduler: &Scheduler,
    ) -> Vec<TaskReport> {
        let tasks = session.court().open_tasks();
        let mut reports = Vec::new();
        session
            .run(&tasks, scheduler, &CancelToken::new(), |update| {
                if let RunUpdate::Finished(r) = update {
                    reports.push(r.clone())
                }
            })
            .await
            .unwrap();
        reports.sort_by_key(|r| r.edict.id);
        reports
    }

    #[test]
    fn found_and_open_guard_the_store() {
        let mut store = MemoryEventLog::default();
        assert!(matches!(
            Session::open(&mut store),
            Err(AppError::NotFounded)
        ));
        Session::found(&mut store).unwrap();
        assert!(matches!(
            Session::found(&mut store),
            Err(AppError::AlreadyFounded)
        ));
    }

    #[test]
    fn recruits_without_a_name_get_a_fresh_random_one() {
        let mut store = MemoryEventLog::default();
        let mut session = Session::found(&mut store).unwrap();
        for _ in 0..20 {
            session.recruit(None, Department::Works).unwrap();
        }
        let names: std::collections::BTreeSet<_> =
            session.court().agents().map(|a| a.name.clone()).collect();
        assert_eq!(names.len(), 26, "开国六人与新招二十人姓名各不相同");
        // 姓名记入事件簿，重放后不变。
        let before: Vec<_> = session.court().agents().cloned().collect();
        drop(session);
        let reopened = Session::open(&mut store).unwrap();
        assert_eq!(
            reopened.court().agents().cloned().collect::<Vec<_>>(),
            before
        );
    }

    #[tokio::test]
    async fn cabinet_runs_only_approved_edicts_and_reforms_leave_notes() {
        let mut store = MemoryEventLog::default();
        let mut session = Session::found(&mut store).unwrap();
        session.reform(GovernanceMode::DirectRule).unwrap();
        assert!(
            session
                .reform(GovernanceMode::Cabinet)
                .unwrap()
                .contains("四叔")
        );
        session.dismiss(AgentId(6)).unwrap();
        session
            .appoint(AgentId(6), Office::FirstGrandSecretary)
            .unwrap();
        let first = session
            .issue(Department::Works, "甲".into(), Seal::default())
            .unwrap();
        let second = session
            .issue(Department::Works, "乙".into(), Seal::default())
            .unwrap();
        session.approve(second.id).unwrap();
        let reports = run_all(&mut session, &scheduler(DemoBackend)).await;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].edict.id, second.id);
        assert!(session.court().task(first.id).unwrap().status.is_open());
        // 内阁改回丞相：土木堡之变。
        assert!(
            session
                .reform(GovernanceMode::Chancellor)
                .unwrap()
                .contains("大元，杀回来了")
        );
        // 一键朱元璋：在任官员全部株连，白身幸免；丞相排在名单之首。
        let commoner = session.recruit(None, Department::Works).unwrap();
        session.appoint(AgentId(6), Office::Chancellor).unwrap();
        let summary = session.abolish_chancellery().unwrap();
        assert!(summary.contains("凡 5 人：中书右丞相"), "{summary}");
        let court = session.court();
        assert_eq!(court.mode(), GovernanceMode::DirectRule);
        assert!(court.agents().all(|a| court.office_of(a.id).is_none()));
        assert!(court.is_purged(AgentId(2)) && !court.is_purged(commoner));
    }

    #[test]
    fn unroutable_edict_records_nothing() {
        let mut store = MemoryEventLog::default();
        let mut session = Session::found(&mut store).unwrap();
        session.reform(GovernanceMode::Cabinet).unwrap();
        let before = session.history().len();
        let result = session.issue(Department::Works, "无首辅".into(), Seal::default());
        assert!(matches!(
            result,
            Err(AppError::Governance(GovernanceError::Vacant(
                Office::FirstGrandSecretary
            )))
        ));
        assert_eq!(session.history().len(), before);
        drop(session);
        assert_eq!(store.load().unwrap().len(), before);
    }

    #[tokio::test]
    async fn concurrent_run_balances_clerks_and_replays() {
        let mut store = MemoryEventLog::default();
        let mut session = Session::found(&mut store).unwrap();
        let id = session
            .recruit(Some("新工".into()), Department::Works)
            .unwrap();
        session
            .appoint(id, Office::Clerk(Department::Works))
            .unwrap();
        session
            .issue(Department::Works, "一".into(), Seal::default())
            .unwrap();
        let second = session
            .issue(Department::Works, "二".into(), Seal::default())
            .unwrap();

        let reports = run_all(&mut session, &scheduler(DemoBackend)).await;
        assert!(
            reports
                .iter()
                .all(|r| matches!(r.outcome, Outcome::Completed(_)))
        );
        // 两件同时指派：第一件给原有郎中（#4），第二件应计入在途负载而给新官员。
        let clerk = &session.court().agent(AgentId(4)).unwrap().name;
        assert!(reports[0].path.last().unwrap().contains(clerk.as_str()));
        assert!(reports[1].path.last().unwrap().contains("新工"));
        drop(session);

        let reopened = Session::open(&mut store).unwrap();
        let court = reopened.court();
        assert!(court.open_tasks().is_empty());
        assert_eq!(court.recorded_completions(AgentId(4)), 1);
        assert_eq!(court.recorded_completions(id), 1);
        assert!(matches!(
            court.task(second.id).unwrap().status,
            TaskStatus::Completed { worker, .. } if worker == id
        ));
    }

    /// 分两段流式输出后完成。
    struct Streaming;
    impl WorkerBackend for Streaming {
        fn execute(&self, _job: &Job, hooks: &dyn ExecutionHooks) -> Result<String, BoxError> {
            hooks.text_delta("先");
            hooks.model_usage(TokenUsage {
                input: 10,
                output: 1,
            });
            hooks.text_delta("后");
            hooks.model_usage(TokenUsage {
                input: 20,
                output: 2,
            });
            Ok("先后".into())
        }
    }

    #[tokio::test]
    async fn streamed_text_arrives_before_finish_and_is_not_persisted() {
        let mut store = MemoryEventLog::default();
        let mut session = Session::found(&mut store).unwrap();
        let edict = session
            .issue(Department::Works, "流".into(), Seal::default())
            .unwrap();
        let before = session.history().len();
        let mut seen = Vec::new();
        session
            .run(
                &[edict.id],
                &scheduler(Streaming),
                &CancelToken::new(),
                |u| {
                    seen.push(match u {
                        RunUpdate::Assigned(task) => format!("{}:assigned", task.0),
                        RunUpdate::Text { task, text } => format!("{}:{text}", task.0),
                        RunUpdate::Finished(r) => {
                            format!("{}:done {}", r.edict.id.0, r.usage.total())
                        }
                    })
                },
            )
            .await
            .unwrap();
        assert_eq!(seen, ["1:assigned", "1:先", "1:后", "1:done 33"]);
        // 指派、两次用量与完成；流式片段不入事件簿。
        assert_eq!(session.history().len(), before + 4);
        drop(session);
        let reopened = Session::open(&mut store).unwrap();
        assert_eq!(
            reopened.court().task(edict.id).unwrap().usage,
            TokenUsage {
                input: 30,
                output: 3
            }
        );
    }

    /// 记录收到的模型与玉玺；默认模型为 flash。
    #[derive(Default)]
    struct Recording(std::sync::Mutex<Vec<(Option<String>, Seal)>>);
    impl WorkerBackend for Recording {
        fn execute(&self, job: &Job, _: &dyn ExecutionHooks) -> Result<String, BoxError> {
            let mut seen = self.0.lock().unwrap();
            seen.push((job.model.clone(), job.edict.seal.clone()));
            Ok("办毕".into())
        }
        fn model_name(&self) -> Option<&str> {
            Some("flash")
        }
    }

    #[tokio::test]
    async fn office_model_and_ceiling_shape_each_job() {
        let mut store = MemoryEventLog::default();
        let mut session = Session::found(&mut store).unwrap();
        let clerk = Office::Clerk(Department::Works);
        let ceiling = Seal {
            capabilities: [Capability::SearchCode].into(),
            scope: vec!["src".into()],
        };
        session.set_office_model(clerk, Some("pro".into())).unwrap();
        session
            .set_office_ceiling(clerk, Some(ceiling.clone()))
            .unwrap();
        assert!(session.set_office_model(clerk, Some("a b".into())).is_err());

        let backend = Arc::new(Recording::default());
        let scheduler = Scheduler::new(
            backend.clone(),
            Limits {
                max_concurrent: 1,
                timeout: Duration::from_secs(10),
            },
        );
        let wide = session
            .issue(Department::Works, "全查".into(), Seal::default())
            .unwrap();
        let outside = Seal {
            scope: vec!["docs".into()],
            ..Seal::default()
        };
        let denied = session
            .issue(Department::Works, "越界".into(), outside)
            .unwrap();
        let reports = run_all(&mut session, &scheduler).await;
        // 默认玉玺收窄为郎中的上限，模型用郎中指定的 pro；范围全在上限外的诏令不执行。
        assert_eq!(
            *backend.0.lock().unwrap(),
            [(Some("pro".to_owned()), ceiling)]
        );
        assert!(reports[0].path.last().unwrap().contains("工部郎中上限"));
        assert!(matches!(&reports[1].outcome, Outcome::Failed(r) if r.contains("上限之外")));
        assert_eq!(
            session.court().task(wide.id).unwrap().model.as_deref(),
            Some("pro")
        );
        assert!(matches!(
            session.court().task(denied.id).unwrap().status,
            TaskStatus::Failed { worker: None, .. }
        ));

        // 恢复默认后记下的是后端默认模型；取消上限后玉玺不再收窄。
        session.set_office_model(clerk, None).unwrap();
        session.set_office_ceiling(clerk, None).unwrap();
        let open = session
            .issue(Department::Works, "再查".into(), Seal::default())
            .unwrap();
        run_all(&mut session, &scheduler).await;
        assert_eq!(
            backend.0.lock().unwrap()[1],
            (Some("flash".to_owned()), Seal::default())
        );
        drop(session);
        let reopened = Session::open(&mut store).unwrap();
        assert_eq!(
            reopened.court().task(open.id).unwrap().model.as_deref(),
            Some("flash")
        );
        assert_eq!(reopened.court().office_model(clerk), None);
    }

    /// 票拟时按 `reply` 作答；执行时记下承办者。
    struct Cabinet(String, std::sync::Mutex<Vec<AgentId>>);
    impl WorkerBackend for Cabinet {
        fn execute(&self, job: &Job, _: &dyn ExecutionHooks) -> Result<String, BoxError> {
            self.1.lock().unwrap().push(job.actor.id);
            Ok("办毕".into())
        }
        fn model_name(&self) -> Option<&str> {
            Some("flash")
        }
        fn complete(
            &self,
            model: Option<&str>,
            system: &str,
            user: &str,
        ) -> Result<(String, TokenUsage), BoxError> {
            assert_eq!(model, Some("pro"), "首辅用其官职所配模型");
            assert!(
                system.contains("内阁首辅") && user.contains("#7 新工 工部郎中"),
                "{user}"
            );
            Ok((
                self.0.clone(),
                TokenUsage {
                    input: 50,
                    output: 5,
                },
            ))
        }
    }

    #[tokio::test]
    async fn cabinet_drafts_pick_the_worker_and_fall_back_when_unusable() {
        let mut store = MemoryEventLog::default();
        let mut session = Session::found(&mut store).unwrap();
        session.reform(GovernanceMode::Cabinet).unwrap();
        session.dismiss(AgentId(6)).unwrap();
        session
            .appoint(AgentId(6), Office::FirstGrandSecretary)
            .unwrap();
        session
            .set_office_model(Office::FirstGrandSecretary, Some("pro".into()))
            .unwrap();
        let new = session
            .recruit(Some("新工".into()), Department::Works)
            .unwrap();
        session
            .appoint(new, Office::Clerk(Department::Works))
            .unwrap();
        let issue = |s: &mut Session<_>, text: &str| {
            s.issue(Department::Works, text.into(), Seal::default())
                .unwrap()
                .id
        };
        let picked = issue(&mut session, "甲");
        let bogus = issue(&mut session, "乙");

        // 旧例会派给 #4；首辅拟派 #7，执行时以票拟为准。
        let cabinet = Cabinet("拟派：#7\n新工熟悉此事。".into(), Default::default());
        assert_eq!(session.draft(picked, &cabinet).unwrap(), new);
        // 拟派了非本部官员（#3 吏部尚书）：按旧例拟派，意见中注明。
        let wrong = Cabinet("拟派：#3".into(), Default::default());
        assert_eq!(session.draft(bogus, &wrong).unwrap(), AgentId(4));
        let draft = session.court().task(bogus).unwrap().draft.clone().unwrap();
        assert!(
            draft.note.starts_with("票拟未指明合格人选"),
            "{}",
            draft.note
        );
        // 演示后端不调用模型：按旧例，不记模型与用量。
        let demo = issue(&mut session, "丙");
        session.draft(demo, &DemoBackend).unwrap();
        assert_eq!(
            session
                .court()
                .task(demo)
                .unwrap()
                .draft
                .as_ref()
                .unwrap()
                .model,
            None
        );
        // 御笔改票后仍须批红；不能改派给他部官员。
        assert!(session.redraft(demo, AgentId(3), String::new()).is_err());
        session
            .redraft(demo, AgentId(2), "尚书亲办".into())
            .unwrap();
        for task in [picked, bogus, demo] {
            session.approve(task).unwrap();
        }
        assert!(
            session.redraft(demo, AgentId(4), String::new()).is_err(),
            "批红后不能改票"
        );

        let scheduler = Scheduler::new(
            Arc::new(Cabinet(String::new(), Default::default())),
            Limits {
                max_concurrent: 1,
                timeout: Duration::from_secs(10),
            },
        );
        let reports = run_all(&mut session, &scheduler).await;
        let worker_of = |id| match &session.court().task(id).unwrap().status {
            TaskStatus::Completed { worker, .. } => *worker,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            [worker_of(picked), worker_of(bogus), worker_of(demo)],
            [new, AgentId(4), AgentId(2)]
        );
        assert!(reports[0].path.last().unwrap().ends_with("（票拟）"));
        // 票拟用量计入诏令，花费按票拟模型（pro）与承办模型分别计价。
        let price = |m: &str| {
            Some(Price {
                input_per_million: if m == "pro" { 1_000_000.0 } else { 0.0 },
                output_per_million: 0.0,
                currency: "元".into(),
            })
        };
        let task = session.court().task(picked).unwrap();
        assert_eq!(task.usage.total(), 55);
        assert_eq!(
            estimate_cost([task], None, price).as_deref(),
            Some("约 50.0000 元")
        );
    }

    #[test]
    fn draft_reply_parsing() {
        assert_eq!(parse_draft("拟派：#12\n理由"), Some(AgentId(12)));
        assert_eq!(parse_draft("参考 #3 之例，拟派：＃ 5"), Some(AgentId(5)));
        assert_eq!(parse_draft("建议 #8 承办"), Some(AgentId(8)));
        assert_eq!(parse_draft("无人可派"), None);
    }

    /// 把诏令文本当作路径，写入 "new\nkeep\n" 后回奏。
    struct Writer;
    impl ziyuan_runtime::agent_loop::ModelProvider for Writer {
        fn next_turn(
            &self,
            messages: &[ziyuan_runtime::agent_loop::Message],
            _tools: &[ziyuan_runtime::agent_loop::ToolSpec],
            _hooks: &dyn ExecutionHooks,
        ) -> Result<ziyuan_runtime::agent_loop::ModelTurn, BoxError> {
            use ziyuan_runtime::agent_loop::{Message, ModelTurn, ToolRequest};
            if messages
                .iter()
                .any(|m| matches!(m, Message::ToolOutput { .. }))
            {
                return Ok(ModelTurn::Final("已改".into()));
            }
            let path = messages.iter().find_map(|m| match m {
                Message::User(text) => Some(text.clone()),
                _ => None,
            });
            Ok(ModelTurn::Calls {
                calls: vec![ToolRequest {
                    id: "1".into(),
                    name: "write_file".into(),
                    args_json: serde_json::json!({ "path": path, "content": "new\nkeep\n" })
                        .to_string(),
                }],
                text: String::new(),
                reasoning: String::new(),
            })
        }
    }

    #[tokio::test]
    async fn works_changes_are_proposed_then_applied_or_discarded_by_the_emperor() {
        let root = std::env::temp_dir().join(format!("ziyuan-app-changes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("README.md"), "old\nkeep\n").unwrap();
        let mut store = MemoryEventLog::default();
        let mut session = Session::found(&mut store).unwrap();
        let write = Seal {
            capabilities: [Capability::ReadFile, Capability::WriteFile].into(),
            scope: vec![".".into()],
        };
        // 动土木之权唯工部可授。
        assert!(matches!(
            session.issue(Department::Revenue, "README.md".into(), write.clone()),
            Err(AppError::Governance(GovernanceError::WorksOnly(
                Capability::WriteFile
            )))
        ));
        let edit = session
            .issue(Department::Works, "README.md".into(), write.clone())
            .unwrap()
            .id;
        let create = session
            .issue(Department::Works, "docs/新.md".into(), write)
            .unwrap()
            .id;
        let backend = ziyuan_runtime::AgentBackend::new(Box::new(Writer), root.clone(), 4);
        run_all(&mut session, &scheduler(backend)).await;

        // 执行后项目不变；改动在回奏之前呈报。
        assert_eq!(
            std::fs::read_to_string(root.join("README.md")).unwrap(),
            "old\nkeep\n"
        );
        let history = session.history();
        let position = |f: &dyn Fn(&CourtEvent) -> bool| history.iter().position(f).unwrap();
        assert!(
            position(&|e| matches!(e, CourtEvent::ChangesProposed { task, .. } if *task == edit))
                < position(
                    &|e| matches!(e, CourtEvent::TaskCompleted { task, .. } if *task == edit)
                )
        );
        let changes = session.court().task(edit).unwrap().changes.clone().unwrap();
        assert_eq!(changes.state, ziyuan_governance::ChangeState::Proposed);
        let diff = render_diff(&changes.files[0]);
        assert!(
            diff.contains("-old\n+new\n") && diff.contains("--- a/README.md"),
            "{diff}"
        );
        assert!(diff.ends_with("+new\n keep\n"), "{diff}");

        session.apply_changes(edit, &root).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("README.md")).unwrap(),
            "new\nkeep\n"
        );
        assert!(matches!(
            session.apply_changes(edit, &root),
            Err(AppError::Governance(GovernanceError::NoPendingChanges(_)))
        ));
        session.discard_changes(create).unwrap();
        assert!(!root.join("docs").exists());
        let state = session
            .court()
            .task(create)
            .unwrap()
            .changes
            .as_ref()
            .unwrap()
            .state;
        assert_eq!(state, ziyuan_governance::ChangeState::Discarded);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// 回奏引用若干文件位置并报告测试通过；复核时以 `censor_reply` 作答（`None` 则不可用）。
    struct Cited {
        censor_reply: Option<&'static str>,
    }
    impl WorkerBackend for Cited {
        fn execute(&self, _job: &Job, hooks: &dyn ExecutionHooks) -> Result<String, BoxError> {
            hooks.tool_invoked("run_tests", "{}", false);
            hooks.tool_invoked("run_tests", "{}", true);
            Ok(
                "见 `src/lib.rs:2`、src/lib.rs:9 与 ./missing.rs，通读 src/lib.rs；和/或 e.g. v0.2.0、std::fs、.env"
                    .into(),
            )
        }
        fn complete(
            &self,
            _model: Option<&str>,
            _system: &str,
            user: &str,
        ) -> Result<(String, TokenUsage), BoxError> {
            assert!(user.contains("src/lib.rs:9 查无实据（行号越界）"), "{user}");
            let reply = self.censor_reply.ok_or("无模型")?;
            Ok((
                reply.into(),
                TokenUsage {
                    input: 10,
                    output: 2,
                },
            ))
        }
    }

    #[tokio::test]
    async fn censorate_checks_evidence_and_kaocheng_grades_officials() {
        let root = std::env::temp_dir().join(format!("ziyuan-app-censor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "a\nb\nc\n").unwrap();
        let mut store = MemoryEventLog::default();
        let mut session = Session::found(&mut store).unwrap();
        for text in ["一", "二"] {
            session
                .issue(Department::Works, text.into(), Seal::default())
                .unwrap();
        }
        let endorse = Cited {
            censor_reply: Some("经查属实，予以背书。\n然 missing.rs 并不存在，不宜弹劾。"),
        };
        run_all(&mut session, &scheduler(Cited { censor_reply: None })).await;
        let handler = session.court().task(TaskId(1)).unwrap().handler().unwrap();

        assert!(matches!(
            session.censor(TaskId(1), &root, &endorse),
            Err(AppError::Governance(GovernanceError::Vacant(
                Office::Censor
            )))
        ));
        let censor = session.recruit(None, Department::Justice).unwrap();
        session.appoint(censor, Office::Censor).unwrap();
        // 计入三处（越界的行号与不存在的文件各一处查无实据；只提文件名不计），最近一次测试通过。
        assert_eq!(
            session.censor(TaskId(1), &root, &endorse).unwrap(),
            CensorVerdict::Endorsed
        );
        let censure = session
            .court()
            .task(TaskId(1))
            .unwrap()
            .censure
            .clone()
            .unwrap();
        let expected = Evidence {
            cited: 3,
            missing: 2,
            tests: Some(true),
        };
        assert_eq!((censure.evidence, censure.usage.total()), (expected, 12));
        // 模型不可用：按证据例——引用有查无实据者，弹劾。
        let unavailable = Cited { censor_reply: None };
        assert_eq!(
            session.censor(TaskId(2), &root, &unavailable).unwrap(),
            CensorVerdict::Impeached
        );
        assert!(
            session
                .court()
                .task(TaskId(2))
                .unwrap()
                .censure
                .as_ref()
                .unwrap()
                .note
                .contains("按证据例弹劾")
        );
        assert!(matches!(
            session.censor(TaskId(2), &root, &unavailable),
            Err(AppError::Governance(GovernanceError::AlreadyCensured(_)))
        ));
        let record = session.court().service_record(handler);
        assert_eq!(
            (record.endorsed, record.impeached, record.verified),
            (1, 1, 0)
        );
        // 背书 1 − 弹劾 1，两件待评定：平常。复核的 token 不算承办用量。
        assert_eq!(kaocheng_grade(&record), "平常");
        assert_eq!(
            execution_usage(session.court().task(TaskId(1)).unwrap()).total(),
            0
        );
        let report = kaocheng_report(session.court());
        assert!(
            report.contains("谁来考丞相") && report.contains("—— 平常"),
            "{report}"
        );
        assert!(
            report.contains("左都御史") && report.contains("—— 无事可考"),
            "{report}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn citation_parsing_and_censure_verdicts() {
        let found = citations(
            "改了 crates/a/lib.rs:12-20，见 README.md；Cargo.toml:3，和/或 e.g. 0.3.0 v0.2 https://x.io/a lib.rs:0 ../x.rs /etc/passwd .git/config",
        );
        let paths: Vec<_> = found.iter().map(|(p, l)| (p.as_str(), *l)).collect();
        assert_eq!(
            paths,
            [
                ("crates/a/lib.rs", Some((12, 20))),
                ("README.md", None),
                ("Cargo.toml", Some((3, 3))),
                ("../x.rs", None),
            ]
        );
        assert_eq!(
            parse_censure("背书。此前有人欲弹劾"),
            Some(CensorVerdict::Endorsed)
        );
        assert_eq!(
            parse_censure("臣以为当弹劾，不当背书"),
            Some(CensorVerdict::Impeached)
        );
        assert_eq!(parse_censure("待查"), None);
    }

    /// 调用一次工具后失败。
    struct Broken;
    impl WorkerBackend for Broken {
        fn execute(&self, _job: &Job, hooks: &dyn ExecutionHooks) -> Result<String, BoxError> {
            hooks.tool_invoked("read_file", r#"{"path":"x"}"#, false);
            Err("模型不可用".into())
        }
    }

    #[tokio::test]
    async fn failures_cancellations_and_tool_audit_are_recorded() {
        let mut store = MemoryEventLog::default();
        let mut session = Session::found(&mut store).unwrap();
        let failing = session
            .issue(Department::Works, "会失败".into(), Seal::default())
            .unwrap();
        let cancelled = session
            .issue(Department::Works, "撤销".into(), Seal::default())
            .unwrap();
        session.cancel(cancelled.id).unwrap();
        assert!(session.cancel(cancelled.id).is_err(), "不能重复撤销");

        let reports = run_all(&mut session, &scheduler(Broken)).await;
        assert_eq!(reports.len(), 1, "已撤销的诏令不再运行");
        assert_eq!(reports[0].outcome, Outcome::Failed("模型不可用".into()));
        drop(session);

        let reopened = Session::open(&mut store).unwrap();
        let court = reopened.court();
        assert_eq!(
            court.task(failing.id).unwrap().status,
            TaskStatus::Failed {
                worker: Some(AgentId(4)),
                reason: "模型不可用".into()
            }
        );
        assert_eq!(
            court.task(cancelled.id).unwrap().status,
            TaskStatus::Cancelled
        );
        let tail: Vec<_> = reopened
            .history()
            .iter()
            .rev()
            .take(3)
            .rev()
            .cloned()
            .collect();
        assert_eq!(
            tail,
            [
                CourtEvent::TaskAssigned {
                    task: failing.id,
                    worker: AgentId(4),
                    model: None,
                },
                CourtEvent::ToolInvoked {
                    task: failing.id,
                    tool: "read_file".into(),
                    args_json: r#"{"path":"x"}"#.into(),
                    ok: false
                },
                CourtEvent::TaskFailed {
                    task: failing.id,
                    reason: "模型不可用".into()
                },
            ]
        );
    }

    fn temp_root(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ziyuan-app-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn stale_session_reproposes_on_fresh_state() {
        let root = temp_root("stale");
        let mut a = Session::found(EventLog::new(&root)).unwrap();
        let mut b = Session::open(EventLog::new(&root)).unwrap();
        let first = a
            .issue(Department::Works, "甲".into(), Seal::default())
            .unwrap();
        // b 尚未看到 #1；若不重新提议，会再次分配 #1 并写坏事件簿。
        let second = b
            .issue(Department::Works, "乙".into(), Seal::default())
            .unwrap();
        assert_eq!((first.id, second.id), (TaskId(1), TaskId(2)));
        let recruited = a.recruit(Some("新工".into()), Department::Works).unwrap();
        assert_eq!(recruited, AgentId(7));

        let reopened = Session::open(EventLog::new(&root)).unwrap();
        assert_eq!(reopened.court().open_tasks(), [TaskId(1), TaskId(2)]);
        assert!(reopened.court().agent(recruited).is_some());
        assert!(matches!(
            Session::found(EventLog::new(&root)),
            Err(AppError::AlreadyFounded)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// 执行期间，另一个进程（此处为另一个 Session）撤销了这件诏令。
    struct CancelledElsewhere(std::path::PathBuf);
    impl WorkerBackend for CancelledElsewhere {
        fn execute(&self, job: &Job, _: &dyn ExecutionHooks) -> Result<String, BoxError> {
            let (_, edict) = (&job.actor, &job.edict);
            let mut other = Session::open(EventLog::new(&self.0))?;
            other.cancel(edict.id)?;
            Ok("本不该被记录".into())
        }
    }

    #[tokio::test]
    async fn result_of_task_cancelled_elsewhere_is_discarded() {
        let root = temp_root("cancel");
        let mut session = Session::found(EventLog::new(&root)).unwrap();
        let edict = session
            .issue(Department::Works, "查".into(), Seal::default())
            .unwrap();
        let reports = run_all(&mut session, &scheduler(CancelledElsewhere(root.clone()))).await;
        assert_eq!(reports[0].outcome, Outcome::Cancelled);

        let reopened = Session::open(EventLog::new(&root)).unwrap();
        assert_eq!(
            reopened.court().task(edict.id).unwrap().status,
            TaskStatus::Cancelled
        );
        assert!(
            !reopened
                .history()
                .iter()
                .any(|e| matches!(e, CourtEvent::TaskCompleted { .. }))
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
