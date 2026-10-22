//! 应用服务层：CLI 与未来的 UI 共用的用例入口。
//!
//! 每次状态变更都遵循同一流程：治理层提议事件 → 在 `Court` 副本上校验 → 写入事件簿 →
//! 更新内存状态。因此被拒绝的操作不会留下任何事件，写入的事件也一定能被回放。
//!
//! 诏令分两步：[`Session::issue`] 入队（路由不通则拒绝、不留记录），
//! [`Session::run`] 指派并交给 [`Scheduler`] 并发执行，逐条记录进度与结局。
use std::fmt;
use ziyuan_core::{AgentId, CourtEvent, Department, Edict, GovernanceMode, Office, TaskId};
use ziyuan_governance::{Court, GovernanceError, TaskStatus, founding_events};
use ziyuan_runtime::{CancelToken, Job, Outcome, Progress, Scheduler};
use ziyuan_storage::{EventStore, StorageError};

#[derive(Debug)]
pub enum AppError {
    NotFounded,
    AlreadyFounded,
    Governance(GovernanceError),
    Storage(StorageError),
}
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFounded => f.write_str("朝廷尚未建立，请先在项目目录执行 `ziyuan init`"),
            Self::AlreadyFounded => f.write_str("朝廷已存在，拒绝覆盖日志"),
            Self::Governance(e) => e.fmt(f),
            Self::Storage(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotFounded | Self::AlreadyFounded => None,
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
}

/// 一个已打开的朝廷：事件簿与由它重放出的当前状态。
pub struct Session<S: EventStore> {
    store: S,
    court: Court,
    history: Vec<CourtEvent>,
}

impl<S: EventStore> Session<S> {
    /// 在空事件簿上建立默认朝廷（丞相制）。
    pub fn found(mut store: S) -> Result<Self, AppError> {
        if !store.load()?.is_empty() {
            return Err(AppError::AlreadyFounded);
        }
        let history = founding_events();
        let court = Court::replay(&history)?;
        store.append(&history)?;
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

    /// 招募新官员（不授官职），返回其 ID。
    pub fn recruit(&mut self, name: String, speciality: Department) -> Result<AgentId, AppError> {
        let event = self.court.propose_recruitment(name, speciality)?;
        let CourtEvent::AgentRegistered(agent) = &event else {
            unreachable!("propose_recruitment 只产生 AgentRegistered");
        };
        let id = agent.id;
        self.commit(vec![event])?;
        Ok(id)
    }

    pub fn appoint(&mut self, agent: AgentId, office: Office) -> Result<(), AppError> {
        let event = self.court.propose_appointment(agent, office)?;
        self.commit(vec![event])
    }

    pub fn dismiss(&mut self, agent: AgentId) -> Result<(), AppError> {
        let event = self.court.propose_dismissal(agent)?;
        self.commit(vec![event])
    }

    pub fn reform(&mut self, mode: GovernanceMode) -> Result<(), AppError> {
        let event = self.court.propose_reform(mode)?;
        self.commit(vec![event])
    }

    /// 下诏入队。现行制度下无法路由（如职位空缺）时拒绝，不留记录。
    pub fn issue(&mut self, department: Department, text: String) -> Result<Edict, AppError> {
        let edict = self.court.new_edict(department, text)?;
        self.court.route(&edict)?;
        self.commit(vec![CourtEvent::EdictIssued(edict.clone())])?;
        Ok(edict)
    }

    /// 撤销尚未结束的诏令。
    pub fn cancel(&mut self, task: TaskId) -> Result<(), AppError> {
        let event = self.court.propose_cancellation(task)?;
        self.commit(vec![event])
    }

    /// 按当前制度逐件指派 `tasks`，交给调度器并发执行，并在每件结束时调用 `on_report`。
    ///
    /// 结局都会记入事件簿：完成、失败（含运行时路由失败与超时）、撤销。
    /// `cancel` 触发时尚未开始的诏令保持未结束，可下次再运行。
    pub async fn run(
        &mut self,
        tasks: &[TaskId],
        scheduler: &Scheduler,
        cancel: &CancelToken,
        mut on_report: impl FnMut(&TaskReport),
    ) -> Result<(), AppError> {
        let mut jobs = Vec::new();
        let mut running = Vec::new();
        for &task in tasks {
            let edict = self
                .court
                .task(task)
                .ok_or(GovernanceError::UnknownEdict(task))?
                .edict
                .clone();
            match self.court.route(&edict) {
                Ok(route) => {
                    self.commit(vec![CourtEvent::TaskAssigned {
                        task,
                        worker: route.worker,
                    }])?;
                    let actor = self
                        .court
                        .agent(route.worker)
                        .expect("路由只指向在册官员")
                        .clone();
                    jobs.push(Job {
                        actor,
                        edict: edict.clone(),
                    });
                    running.push((edict, route.path));
                }
                Err(error) => {
                    let reason = error.to_string();
                    self.commit(vec![CourtEvent::TaskFailed {
                        task,
                        reason: reason.clone(),
                    }])?;
                    on_report(&TaskReport {
                        edict,
                        path: Vec::new(),
                        outcome: Outcome::Failed(reason),
                    });
                }
            }
        }

        let mut progress = scheduler.start(jobs, cancel);
        while !running.is_empty() {
            let Some(update) = progress.recv().await else {
                break;
            };
            match update {
                Progress::ToolInvoked {
                    task,
                    tool,
                    args_json,
                    ok,
                } => {
                    // 超时/撤销后，后端线程仍可能报告工具调用；此时任务已结束，不再记录。
                    if matches!(
                        self.court.task(task).map(|t| &t.status),
                        Some(TaskStatus::Assigned(_))
                    ) {
                        self.commit(vec![CourtEvent::ToolInvoked {
                            task,
                            tool,
                            args_json,
                            ok,
                        }])?;
                    }
                }
                Progress::Finished { task, outcome } => {
                    let Some(index) = running.iter().position(|(e, _)| e.id == task) else {
                        continue;
                    };
                    let (edict, path) = running.swap_remove(index);
                    let event = match &outcome {
                        Outcome::Completed(summary) => Some(CourtEvent::TaskCompleted {
                            task,
                            worker: match self.court.task(task).map(|t| &t.status) {
                                Some(TaskStatus::Assigned(worker)) => *worker,
                                _ => unreachable!("执行中的诏令必已指派"),
                            },
                            summary: summary.clone(),
                        }),
                        Outcome::Failed(reason) => Some(CourtEvent::TaskFailed {
                            task,
                            reason: reason.clone(),
                        }),
                        Outcome::Cancelled => Some(CourtEvent::TaskCancelled { task }),
                        Outcome::NotStarted => None,
                    };
                    if let Some(event) = event {
                        self.commit(vec![event])?;
                    }
                    on_report(&TaskReport {
                        edict,
                        path,
                        outcome,
                    });
                }
            }
        }
        Ok(())
    }

    fn commit(&mut self, events: Vec<CourtEvent>) -> Result<(), AppError> {
        let mut next = self.court.clone();
        for event in &events {
            next.apply(event)?;
        }
        self.store.append(&events)?;
        self.court = next;
        self.history.extend(events);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use ziyuan_core::Agent;
    use ziyuan_runtime::{BoxError, DemoBackend, ExecutionHooks, Limits, WorkerBackend};
    use ziyuan_storage::MemoryEventLog;

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
            .run(&tasks, scheduler, &CancelToken::new(), |r| {
                reports.push(r.clone())
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
    fn unroutable_edict_records_nothing() {
        let mut store = MemoryEventLog::default();
        let mut session = Session::found(&mut store).unwrap();
        session.reform(GovernanceMode::Cabinet).unwrap();
        let before = session.history().len();
        let result = session.issue(Department::Works, "无首辅".into());
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
        let id = session.recruit("新工".into(), Department::Works).unwrap();
        session
            .appoint(id, Office::Clerk(Department::Works))
            .unwrap();
        session.issue(Department::Works, "一".into()).unwrap();
        let second = session.issue(Department::Works, "二".into()).unwrap();

        let reports = run_all(&mut session, &scheduler(DemoBackend)).await;
        assert!(
            reports
                .iter()
                .all(|r| matches!(r.outcome, Outcome::Completed(_)))
        );
        // 两件同时指派：第一件给陈工（#4），第二件应计入在途负载而给新官员。
        assert!(reports[0].path.last().unwrap().contains("陈工"));
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

    /// 调用一次工具后失败。
    struct Broken;
    impl WorkerBackend for Broken {
        fn execute(
            &self,
            _: &Agent,
            _: &Edict,
            hooks: &dyn ExecutionHooks,
        ) -> Result<String, BoxError> {
            hooks.tool_invoked("read_file", r#"{"path":"x"}"#, false);
            Err("模型不可用".into())
        }
    }

    #[tokio::test]
    async fn failures_cancellations_and_tool_audit_are_recorded() {
        let mut store = MemoryEventLog::default();
        let mut session = Session::found(&mut store).unwrap();
        let failing = session.issue(Department::Works, "会失败".into()).unwrap();
        let cancelled = session.issue(Department::Works, "撤销".into()).unwrap();
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
                    worker: AgentId(4)
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
}
