//! 执行：指派官员、交给调度器并发执行，以及常朝。

use std::path::PathBuf;
use std::sync::Arc;
use ziyuan_core::{CensorVerdict, CourtEvent, Edict, TaskId, Threat, TokenUsage, Verdict};
use ziyuan_governance::{GovernanceError, TaskStatus};
use ziyuan_runtime::{
    CancelToken, Job, Outcome, Progress, Scheduler, Treasury, WorkerBackend, guard,
};
use ziyuan_storage::EventStore;

use crate::cabinet::{criteria_note, debate_minutes, summary_prompt};
use crate::edicts::PROXY_REVIEW_NOTE;
use crate::reports::execution_usage;
use crate::time::now;
use crate::{AppError, Session, truncate};

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

/// 常朝（[`Session::watch`]）的进展。
pub enum WatchUpdate<'a> {
    /// 执行中的进展，同 [`Session::run`]。
    Run(RunUpdate<'a>),
    /// 办完的诏令已由都察院自动复核。
    Censured {
        task: TaskId,
        verdict: CensorVerdict,
    },
    /// 提示（不中断常朝）：未能自动复核、国库告罄待加额等。
    Notice(String),
    /// 史官记：待评定的回奏积压过多。
    Neglect(String),
}

/// 常朝的设定。
pub struct WatchOptions {
    /// 两次查看队列的间隔。
    pub poll: std::time::Duration,
    /// 办完的诏令是否自动请都察院复核（`root` 为核验引用所据的项目目录）。
    pub censor: Option<PathBuf>,
}

/// 待评定的回奏达到此数时，史官记万历故事。
pub const NEGLECT_THRESHOLD: usize = 10;

/// 执行者锁被占（另一个进程正在执行诏令）时各前端的提示。
pub const EXECUTOR_BUSY: &str =
    "军情紧急，另有将领在外：另一个 ziyuan 进程正在执行诏令，请待其结束";

impl<S: EventStore> Session<S> {
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
        // 修道期间，司礼监先把待批红的诏令代批红。
        self.commit(|court| Ok(court.proxy_approvals(Some(tasks))))?;
        // 国库告罄：有可执行的诏令也不开工，留在队列待追加度支。
        let court = &self.court;
        let runnable = tasks.iter().filter_map(|&t| court.task(t)).any(|t| {
            t.status.is_open() && !court.needs_approval(t) && court.blocked_by(t).is_none()
        });
        if runnable {
            court.check_treasury(0)?;
        }
        let treasury = court
            .treasury_remaining()
            .map(|left| Arc::new(Treasury::new(left)));
        // 翰林院实录：本次开工前已结的诏令，供承办者以 search_records 查阅前例。
        let records = Arc::new(court.records());
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
                // 已结束、尚未批红或依赖未成：跳过。显式 run(task IDs) 也不得越过 DAG 门。
                if !entry.status.is_open()
                    || court.needs_approval(entry)
                    || court.blocked_by(entry).is_some()
                {
                    return Ok(Vec::new());
                }
                // 排程限束：期限已过或尝试次数已尽，不再承办，径记失败。
                if let Some(dag) = &entry.dag {
                    if let Some(deadline) = dag.deadline
                        && now() > deadline
                    {
                        return Ok(vec![CourtEvent::TaskFailed {
                            task,
                            reason: "排程期限已过，不再承办".into(),
                        }]);
                    }
                    if let Some(limit) = dag.attempts
                        && entry.attempts >= limit
                    {
                        return Ok(vec![CourtEvent::TaskFailed {
                            task,
                            reason: format!("排程尝试次数已尽（上限 {limit}），不再承办"),
                        }]);
                    }
                }
                // 玉玺受承办者官职的上限约束；毫无交集则不予承办。
                // 内阁制下按票拟派工（拟派者已不在任则按旧例）。
                // 本件度支（含承办者官职的每件限额）已用尽：不予承办。
                let assigned = court.route_task(entry).and_then(|route| {
                    let seal = court.effective_seal(route.worker, &entry.edict)?;
                    court.check_task_funds(entry, Some(route.worker), 0)?;
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
                    let mut seal = sealed.take().expect("指派时已算出玉玺");
                    let entry = self.court.task(task).expect("刚提交的诏令");
                    let mut text = if entry.split.is_some() {
                        // 汇总只核对、不动土木：收回写文件与跑测试之权。
                        seal.capabilities.retain(|c| !c.is_works_only());
                        summary_prompt(&self.court, entry)
                    } else {
                        match debate_minutes(&self.court, entry) {
                            Some(minutes) => format!("{}\n\n{minutes}", edict.text),
                            None => edict.text.clone(),
                        }
                    };
                    if let Some(note) = criteria_note(entry) {
                        text = format!("{text}\n\n{note}");
                    }
                    // 排程：上游子诏令的回奏与约定产出附入承办提示（依赖已完成方可调度）。
                    if let Some(dag) = &entry.dag {
                        if !dag.dependencies.is_empty() {
                            let nonce = guard::nonce();
                            let mut upstream = format!(
                                "上游子诏令的回奏（本件排在它们之后办理，可查可用）：\n{}\n",
                                guard::warning("上游回奏", &nonce)
                            );
                            for dep in &dag.dependencies {
                                if let Some(TaskStatus::Completed { summary, .. }) =
                                    self.court.task(*dep).map(|t| &t.status)
                                {
                                    upstream.push_str(&format!(
                                        "\n## 子诏令 #{}：\n{}\n",
                                        dep.0,
                                        guard::fence(&truncate(summary, 3000), &nonce)
                                    ));
                                }
                            }
                            text = format!("{text}\n\n{upstream}");
                        }
                        if let Some(artifact) = &dag.artifact {
                            text = format!("{text}\n\n约定产出：{artifact}");
                        }
                    }
                    let budget = self
                        .court
                        .task_limit(entry, Some(worker))
                        .map(|limit| limit.saturating_sub(entry.usage.total()));
                    jobs.push(Job {
                        budget,
                        treasury: treasury.clone(),
                        urgent: entry.urgent,
                        records: records.clone(),
                        actor,
                        edict: Edict {
                            seal,
                            text,
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
                Progress::Usage { task, usage, trace } => {
                    // 结束后才返回的调用同样消耗了 token，照记不误。
                    self.commit(|court| {
                        Ok(match court.task(task).map(|t| &t.status) {
                            Some(TaskStatus::Pending) | None => Vec::new(),
                            Some(_) => vec![CourtEvent::ModelUsage {
                                task,
                                usage,
                                trace: trace.clone(),
                            }],
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
                Progress::Suspicion {
                    task,
                    source,
                    threat,
                    excerpt,
                } => {
                    // 锦衣卫密报：只记执行中诏令的。
                    self.commit(|court| {
                        Ok(match court.task(task).map(|t| &t.status) {
                            Some(TaskStatus::Assigned(_)) => vec![CourtEvent::SuspicionRaised {
                                task,
                                source: source.clone(),
                                threat,
                                excerpt: excerpt.clone(),
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
                                args_json: ziyuan_runtime::tools::audit_args(&tool, &args_json),
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
                            Outcome::Completed(summary) => {
                                let mut events = vec![CourtEvent::TaskCompleted {
                                    task,
                                    worker: *worker,
                                    summary: summary.as_str().into(),
                                }];
                                // 刑部勘问回奏：有无劫持、索取机密、越权的文字。
                                let findings = guard::scan(summary, &Threat::ALL);
                                let flagged = !findings.is_empty()
                                    || court
                                        .task(task)
                                        .is_some_and(|t| t.standing_suspicions().next().is_some());
                                events.extend(findings.into_iter().map(|f| {
                                    CourtEvent::SuspicionRaised {
                                        task,
                                        source: "report".into(),
                                        threat: f.threat,
                                        excerpt: f.excerpt,
                                    }
                                }));
                                // 修道期间回奏无人御览，司礼监照准；但有密报的，司礼监不敢擅专。
                                if court.in_retreat() && !flagged {
                                    events.push(CourtEvent::TaskReviewed {
                                        task,
                                        verdict: Verdict::Accepted,
                                        note: PROXY_REVIEW_NOTE.into(),
                                    });
                                }
                                events
                            }
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
            self.commit(|court| Ok(court.proxy_approvals(None)))?;
            let tasks = self.court.runnable_tasks();
            if tasks.is_empty() || tasks == previous || cancel.is_cancelled() {
                return Ok(());
            }
            self.run(&tasks, scheduler, cancel, &mut on_update).await?;
            previous = tasks;
        }
    }

    /// 常朝：反复执行可执行的诏令（其他进程经 `post`、`edict --queue` 入队或批红的也算），
    /// 直到 `cancel` 触发。办完的诏令（子诏令除外，由汇总的原诏令代表）可自动请都察院复核；
    /// 采纳与落盘仍须皇帝亲自处置。国库告罄、御史空缺只提示，不中断。复核会阻塞到模型返回。
    pub async fn watch(
        &mut self,
        scheduler: &Scheduler,
        backend: &dyn WorkerBackend,
        cancel: &CancelToken,
        options: &WatchOptions,
        mut on_update: impl FnMut(WatchUpdate<'_>),
    ) -> Result<(), AppError> {
        let (mut broke, mut no_censor, mut neglected) = (false, false, false);
        while !cancel.is_cancelled() {
            let mut done = Vec::new();
            let ran = self
                .run_open(scheduler, cancel, |update| {
                    if let RunUpdate::Finished(report) = &update
                        && matches!(report.outcome, Outcome::Completed(_))
                    {
                        done.push(report.edict.id);
                    }
                    on_update(WatchUpdate::Run(update));
                })
                .await;
            match ran {
                Ok(()) => broke = false,
                Err(AppError::Governance(e @ GovernanceError::TreasuryExhausted(_))) => {
                    if !broke {
                        on_update(WatchUpdate::Notice(format!("{e}；待加额后续办")));
                    }
                    broke = true;
                }
                Err(e) => return Err(e),
            }
            if let Some(root) = &options.censor {
                for task in done {
                    if cancel.is_cancelled() {
                        break;
                    }
                    if self.court.task(task).is_some_and(|t| t.parent.is_some()) {
                        continue;
                    }
                    if self.court.censor().is_none() {
                        if !no_censor {
                            on_update(WatchUpdate::Notice(
                                "左都御史空缺，办完的诏令不自动复核".into(),
                            ));
                        }
                        no_censor = true;
                        continue;
                    }
                    no_censor = false;
                    match self.censor(task, root, backend) {
                        Ok(verdict) => on_update(WatchUpdate::Censured { task, verdict }),
                        Err(e) => on_update(WatchUpdate::Notice(format!(
                            "诏令 #{} 未能自动复核：{e}",
                            task.0
                        ))),
                    }
                }
            }
            let waiting = self
                .court
                .tasks()
                .filter(|t| matches!(t.status, TaskStatus::Completed { .. }))
                .filter(|t| t.review.is_none() && t.parent.is_none())
                .count();
            if waiting >= NEGLECT_THRESHOLD && !neglected {
                on_update(WatchUpdate::Neglect(format!(
                    "常朝不辍，百官回奏已有 {waiting} 件留中不发（`ziyuan review` 评定）。\n\
                     ——万历中年以后，帝深居不出，章奏多留中不报，阁臣请见经年不得，\
                     史称「三十年不上朝」；而六部照常办事，国事日非。"
                )));
            }
            neglected = waiting >= NEGLECT_THRESHOLD;
            tokio::select! {
                _ = tokio::time::sleep(options.poll) => {}
                _ = cancel.cancelled() => {}
            }
        }
        Ok(())
    }
}
