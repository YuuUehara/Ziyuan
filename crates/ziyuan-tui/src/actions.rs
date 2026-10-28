//! 需要选人或执行的操作：分办、廷议、复核、科举、廷推、票拟与执行。

use ziyuan_app::{EXECUTOR_BUSY, RunUpdate, Session};
use ziyuan_core::{AgentId, Office, TaskId};
use ziyuan_runtime::{CancelToken, Scheduler};
use ziyuan_storage::EventLog;

use crate::App;
use crate::app::Message;
use crate::render::speech_line;

impl App {
    /// 在阻塞线程中请尚书分办（会等待模型返回），结果经消息送回。
    pub(crate) fn start_split(&mut self, task: TaskId) {
        self.info(format!("诏令 #{} 尚书分办中…", task.0));
        let root = self.config.root.clone();
        let backend = self.config.backend.clone();
        let sender = self.sender.clone();
        tokio::task::spawn_blocking(move || {
            let result = Session::open(EventLog::new(&root))
                .and_then(|mut s| s.plan_split(task, backend.as_ref()))
                .map(|subtasks| subtasks.len())
                .map_err(|e| e.to_string());
            let _ = sender.send(Message::Split(task, result));
        });
    }

    /// 在阻塞线程中廷议（默认人选，两轮；会等待全部发言），发言与结果经消息送回。
    pub(crate) fn start_debate(&mut self, task: TaskId) {
        if self.debating.contains_key(&task) {
            return;
        }
        self.info(format!("诏令 #{} 廷议中…", task.0));
        self.debating.insert(task, Vec::new());
        let root = self.config.root.clone();
        let backend = self.config.backend.clone();
        let sender = self.sender.clone();
        tokio::task::spawn_blocking(move || {
            let result = Session::open(EventLog::new(&root))
                .and_then(|mut s| {
                    s.debate(task, &[], 2, backend.as_ref(), |agent, speech| {
                        let line = speech_line(&agent.name, agent.id, speech);
                        let _ = sender.send(Message::Speech(task, line));
                    })
                })
                .map_err(|e| e.to_string());
            let _ = sender.send(Message::Debated(task, result));
        });
    }

    /// 在阻塞线程中请都察院复核（会等待御史的模型返回），结果经消息送回。
    pub(crate) fn start_censor(&mut self, task: TaskId) {
        self.info(format!("诏令 #{} 都察院复核中…", task.0));
        let root = self.config.root.clone();
        let backend = self.config.backend.clone();
        let sender = self.sender.clone();
        tokio::task::spawn_blocking(move || {
            let result = Session::open(EventLog::new(&root))
                .and_then(|mut s| s.censor(task, &root, backend.as_ref()))
                .map(|v| v.label())
                .map_err(|e| e.to_string());
            let _ = sender.send(Message::Censured(task, result));
        });
    }

    /// 在阻塞线程中请刑部复勘密报（会等待模型），结果经消息送回。
    pub(crate) fn start_recheck(&mut self, task: TaskId) {
        self.info(format!("诏令 #{} 刑部复勘中…", task.0));
        let root = self.config.root.clone();
        let backend = self.config.backend.clone();
        let sender = self.sender.clone();
        tokio::task::spawn_blocking(move || {
            let result = Session::open(EventLog::new(&root))
                .and_then(|mut s| s.recheck(task, backend.as_ref()))
                .map(|r| {
                    let upheld = r.iter().filter(|(_, u)| *u).count();
                    (r.len() - upheld, upheld)
                })
                .map_err(|e| e.to_string());
            let _ = sender.send(Message::Rechecked(task, result));
        });
    }

    /// 在阻塞线程中让官员应试 `.ziyuan/exam/` 下的考题（会等待模型作答），结果经消息送回。
    pub(crate) fn start_exam(&mut self, agent: AgentId) {
        self.info(format!("官员 #{} 应试中…", agent.0));
        let root = self.config.root.clone();
        let backend = self.config.backend.clone();
        let sender = self.sender.clone();
        tokio::task::spawn_blocking(move || {
            let mut scores = Vec::new();
            let result = Session::open(EventLog::new(&root))
                .and_then(|mut s| {
                    s.exam(agent, &root, backend.as_ref(), |paper, score| {
                        scores.push(format!("「{}」{score} 分", paper.name));
                    })
                })
                .map(|_| scores)
                .map_err(|e| e.to_string());
            let _ = sender.send(Message::Examined(agent, result));
        });
    }

    /// 在阻塞线程中请九卿廷推（会等待模型作答），各票与得票经消息送回。
    pub(crate) fn start_nomination(&mut self, office: Office) {
        self.info(format!("九卿会推{}中…", office.label()));
        let root = self.config.root.clone();
        let backend = self.config.backend.clone();
        let sender = self.sender.clone();
        tokio::task::spawn_blocking(move || {
            let mut lines = vec![format!("廷推{}：", office.label())];
            let result = Session::open(EventLog::new(&root))
                .and_then(|mut s| {
                    s.nominate(office, backend.as_ref(), |voter, vote| {
                        lines.push(format!(
                            "{} 举 #{}：{}",
                            voter.name, vote.candidate.0, vote.note
                        ));
                    })?;
                    let court = s.court();
                    let tally: Vec<String> = court
                        .nomination_tally(office)
                        .into_iter()
                        .map(|(id, n)| {
                            let name = court.agent(id).map_or("", |a| a.name.as_str());
                            format!("#{} {name} {n} 票", id.0)
                        })
                        .collect();
                    lines.push(format!("得票：{}", tally.join("、")));
                    Ok(lines.join("\n"))
                })
                .map_err(|e| e.to_string());
            let _ = sender.send(Message::Nominated(office, result));
        });
    }

    /// 在阻塞线程中请内阁票拟（会等待模型返回），结果经消息送回。
    pub(crate) fn start_draft(&mut self, task: TaskId) {
        self.info(format!("诏令 #{} 内阁票拟中…", task.0));
        let root = self.config.root.clone();
        let backend = self.config.backend.clone();
        let sender = self.sender.clone();
        tokio::task::spawn_blocking(move || {
            let result = Session::open(EventLog::new(&root))
                .and_then(|mut s| s.draft(task, backend.as_ref()))
                .map_err(|e| e.to_string());
            let _ = sender.send(Message::Drafted(task, result));
        });
    }

    /// 在后台执行全部未结束的诏令（含执行期间新入队的）。
    pub(crate) fn start_run(&mut self) {
        if self.run.is_some() {
            return self.error("已在执行中");
        }
        let lock = match EventLog::new(&self.config.root).try_lock_executor() {
            Ok(Some(lock)) => lock,
            Ok(None) => return self.error(EXECUTOR_BUSY),
            Err(e) => return self.error(e.to_string()),
        };
        if self.court().runnable_tasks().is_empty() {
            return if self.court().open_tasks().is_empty() {
                self.info("没有未结束的诏令")
            } else {
                self.error("诏令尚待批红：内阁制下须先按 b 朱笔批红")
            };
        }
        let cancel = CancelToken::new();
        self.run = Some(cancel.clone());
        self.info("执行中…（按 s 停止）");
        let scheduler = Scheduler::new(self.config.backend.clone(), self.config.limits);
        let root = self.config.root.clone();
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let _lock = lock;
            let result = async {
                let mut session = Session::open(EventLog::new(&root))?;
                session
                    .run_open(&scheduler, &cancel, |update| {
                        let _ = sender.send(match update {
                            RunUpdate::Assigned(task) => Message::Assigned(task),
                            RunUpdate::Text { task, text } => Message::Text(task, text.to_owned()),
                            RunUpdate::Finished(report) => Message::Finished {
                                task: report.edict.id,
                                outcome: report.outcome.clone(),
                            },
                        });
                    })
                    .await
            }
            .await;
            let _ = sender.send(Message::RunDone(result.err().map(|e| e.to_string())));
        });
    }

    pub(crate) fn stop_run(&mut self) {
        match &self.run {
            Some(cancel) => {
                cancel.cancel();
                self.info("正在停止：撤销执行中的诏令，排队中的保留");
            }
            None => self.error("当前没有执行"),
        }
    }

    // ---- 绘制 ----
}
