//! 应用服务层：CLI 与未来的 UI 共用的用例入口。
//!
//! 每次状态变更都遵循同一流程：治理层提议事件 → 在 `Court` 副本上校验 → 写入事件簿 →
//! 更新内存状态。因此被拒绝的操作不会留下任何事件，写入的事件也一定能被回放。
//!
//! 诏令分两步：[`Session::issue`] 入队（路由不通则拒绝、不留记录），
//! [`Session::run`] 指派并交给 [`Scheduler`] 并发执行，逐条记录进度与结局。
use std::fmt;
use ziyuan_core::{
    AgentId, CourtEvent, Department, Edict, GovernanceMode, Office, Seal, TaskId, Verdict,
};
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

/// [`Session::run`] 执行期间的实时进度。
#[derive(Clone, Copy, Debug)]
pub enum RunUpdate<'a> {
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
        let history = founding_events();
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

    /// 招募新官员（不授官职），返回其 ID。
    pub fn recruit(&mut self, name: String, speciality: Department) -> Result<AgentId, AppError> {
        let committed =
            self.commit(|court| Ok(vec![court.propose_recruitment(name.clone(), speciality)?]))?;
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

    pub fn reform(&mut self, mode: GovernanceMode) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_reform(mode)?]))?;
        Ok(())
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
        for &task in tasks {
            let mut path = Vec::new();
            let committed = self.commit(|court| {
                let entry = court
                    .task(task)
                    .ok_or(GovernanceError::UnknownEdict(task))?;
                if !entry.status.is_open() {
                    return Ok(Vec::new());
                }
                Ok(vec![match court.route(&entry.edict) {
                    Ok(route) => {
                        path = route.path;
                        CourtEvent::TaskAssigned {
                            task,
                            worker: route.worker,
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
                CourtEvent::TaskAssigned { worker, .. } => {
                    let actor = self
                        .court
                        .agent(worker)
                        .expect("路由只指向在册官员")
                        .clone();
                    jobs.push(Job {
                        actor,
                        edict: edict.clone(),
                    });
                    running.push((edict, path));
                }
                CourtEvent::TaskFailed { reason, .. } => {
                    on_update(RunUpdate::Finished(&TaskReport {
                        edict,
                        path: Vec::new(),
                        outcome: Outcome::Failed(reason),
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
                    on_update(RunUpdate::Finished(&TaskReport {
                        edict,
                        path,
                        outcome,
                    }));
                }
            }
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use ziyuan_core::Agent;
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
        let id = session.recruit("新工".into(), Department::Works).unwrap();
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

    /// 分两段流式输出后完成。
    struct Streaming;
    impl WorkerBackend for Streaming {
        fn execute(
            &self,
            _: &Agent,
            _: &Edict,
            hooks: &dyn ExecutionHooks,
        ) -> Result<String, BoxError> {
            hooks.text_delta("先");
            hooks.text_delta("后");
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
                        RunUpdate::Text { task, text } => format!("{}:{text}", task.0),
                        RunUpdate::Finished(r) => format!("{}:done", r.edict.id.0),
                    })
                },
            )
            .await
            .unwrap();
        assert_eq!(seen, ["1:先", "1:后", "1:done"]);
        // 只多了指派与完成两条事件，流式片段不入事件簿。
        assert_eq!(session.history().len(), before + 2);
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
        let recruited = a.recruit("新工".into(), Department::Works).unwrap();
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
        fn execute(
            &self,
            _: &Agent,
            edict: &Edict,
            _: &dyn ExecutionHooks,
        ) -> Result<String, BoxError> {
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
