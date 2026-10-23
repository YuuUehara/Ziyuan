//! Tokio 调度器：并发上限、单任务超时与整体撤销。
//!
//! 后端是同步的（模型 HTTP 调用会阻塞），因此每个任务在阻塞线程池中执行；
//! 取消是协作式的：调度器立即给出结论，后端在下一次检查 [`ExecutionHooks::is_cancelled`] 时停下。
use crate::{ExecutionHooks, WorkerBackend};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use ziyuan_core::{Agent, Edict, TaskId};

/// 可克隆的撤销信号；同步代码用 `is_cancelled` 轮询，异步代码 `await cancelled()`。
#[derive(Clone, Debug)]
pub struct CancelToken(Arc<watch::Sender<bool>>);

impl Default for CancelToken {
    fn default() -> Self {
        Self(Arc::new(watch::Sender::new(false)))
    }
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    pub async fn cancelled(&self) {
        // 发送端由 self 持有，等待不会因通道关闭而提前返回。
        let _ = self.0.subscribe().wait_for(|cancelled| *cancelled).await;
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// 同时执行的任务数上限（至少为 1）。
    pub max_concurrent: usize,
    /// 单个任务从开始执行起的时限。
    pub timeout: Duration,
}

/// 一件已指派的诏令。
#[derive(Clone, Debug)]
pub struct Job {
    pub actor: Agent,
    pub edict: Edict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    ToolInvoked {
        task: TaskId,
        tool: String,
        args_json: String,
        ok: bool,
    },
    /// 模型流式生成的文本片段，只用于实时显示。
    TextDelta { task: TaskId, text: String },
    /// 每个 Job 恰好一条。
    Finished { task: TaskId, outcome: Outcome },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Completed(String),
    Failed(String),
    /// 执行中被整体撤销。
    Cancelled,
    /// 撤销时尚在排队，从未开始执行。
    NotStarted,
}

pub struct Scheduler {
    backend: Arc<dyn WorkerBackend>,
    limits: Limits,
}

impl Scheduler {
    pub fn new(backend: Arc<dyn WorkerBackend>, limits: Limits) -> Self {
        Self { backend, limits }
    }

    /// 启动全部任务并立即返回进度通道。必须在 Tokio 运行时内调用。
    /// 任务按 `jobs` 顺序开始执行。`cancel` 触发后，执行中的任务记为 `Cancelled`，
    /// 尚未开始的记为 `NotStarted`。
    pub fn start(&self, jobs: Vec<Job>, cancel: &CancelToken) -> mpsc::UnboundedReceiver<Progress> {
        let (tx, rx) = mpsc::unbounded_channel();
        let permits = Arc::new(Semaphore::new(self.limits.max_concurrent.max(1)));
        let backend = self.backend.clone();
        let timeout = self.limits.timeout;
        let cancel = cancel.clone();
        // 单一派发者按顺序取得许可，保证先入队者先执行。
        tokio::spawn(async move {
            let mut jobs = jobs.into_iter();
            for job in jobs.by_ref() {
                let permit = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        let _ = tx.send(Progress::Finished { task: job.edict.id, outcome: Outcome::NotStarted });
                        break;
                    }
                    permit = permits.clone().acquire_owned() => permit.expect("信号量从不关闭"),
                };
                let task = job.edict.id;
                let run = run_job(
                    job,
                    permit,
                    backend.clone(),
                    cancel.clone(),
                    timeout,
                    tx.clone(),
                );
                let tx = tx.clone();
                tokio::spawn(async move {
                    let outcome = run.await;
                    let _ = tx.send(Progress::Finished { task, outcome });
                });
            }
            for job in jobs {
                let _ = tx.send(Progress::Finished {
                    task: job.edict.id,
                    outcome: Outcome::NotStarted,
                });
            }
        });
        rx
    }
}

async fn run_job(
    job: Job,
    permit: OwnedSemaphorePermit,
    backend: Arc<dyn WorkerBackend>,
    cancel: CancelToken,
    timeout: Duration,
    progress: mpsc::UnboundedSender<Progress>,
) -> Outcome {
    let hooks = TaskHooks {
        task: job.edict.id,
        cancel: CancelToken::new(),
        progress,
    };
    let task_cancel = hooks.cancel.clone();
    let handle = tokio::task::spawn_blocking(move || {
        // 许可随执行线程释放：超时后线程仍在收尾时，不会让并发数超过上限。
        let _permit = permit;
        backend.execute(&job.actor, &job.edict, &hooks)
    });
    tokio::select! {
        result = handle => match result {
            Ok(Ok(summary)) => Outcome::Completed(summary),
            Ok(Err(error)) => Outcome::Failed(error.to_string()),
            Err(join) => Outcome::Failed(format!("执行线程异常终止: {join}")),
        },
        _ = tokio::time::sleep(timeout) => {
            task_cancel.cancel();
            Outcome::Failed(format!("超时（{} 秒）", timeout.as_secs_f64()))
        }
        _ = cancel.cancelled() => {
            task_cancel.cancel();
            Outcome::Cancelled
        }
    }
}

struct TaskHooks {
    task: TaskId,
    cancel: CancelToken,
    progress: mpsc::UnboundedSender<Progress>,
}

impl ExecutionHooks for TaskHooks {
    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    fn text_delta(&self, text: &str) {
        let _ = self.progress.send(Progress::TextDelta {
            task: self.task,
            text: text.into(),
        });
    }

    fn tool_invoked(&self, tool: &str, args_json: &str, ok: bool) {
        // 任务已结束、接收方已关闭时丢弃。
        let _ = self.progress.send(Progress::ToolInvoked {
            task: self.task,
            tool: tool.into(),
            args_json: args_json.into(),
            ok,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BoxError;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::thread;
    use ziyuan_core::{AgentId, Department, Seal};

    fn job(id: u64) -> Job {
        Job {
            actor: Agent {
                id: AgentId(id),
                name: format!("官{id}"),
                speciality: Department::Works,
            },
            edict: Edict {
                id: TaskId(id),
                department: Department::Works,
                text: format!("任务{id}"),
                seal: Seal::default(),
            },
        }
    }

    async fn collect(mut rx: mpsc::UnboundedReceiver<Progress>) -> Vec<Progress> {
        let mut all = Vec::new();
        while let Some(p) = rx.recv().await {
            all.push(p);
        }
        all
    }

    fn outcomes(progress: &[Progress]) -> Vec<(u64, Outcome)> {
        let mut out: Vec<_> = progress
            .iter()
            .filter_map(|p| match p {
                Progress::Finished { task, outcome } => Some((task.0, outcome.clone())),
                _ => None,
            })
            .collect();
        out.sort_by_key(|(id, _)| *id);
        out
    }

    /// 记录同时执行的最大任务数；每次调用一次工具。
    #[derive(Default)]
    struct Busy {
        running: AtomicUsize,
        peak: AtomicUsize,
    }
    impl WorkerBackend for Busy {
        fn execute(
            &self,
            actor: &Agent,
            _: &Edict,
            hooks: &dyn ExecutionHooks,
        ) -> Result<String, BoxError> {
            let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(40));
            hooks.tool_invoked("echo", "{}", actor.id.0.is_multiple_of(2));
            self.running.fetch_sub(1, Ordering::SeqCst);
            Ok(format!("done {}", actor.id.0))
        }
    }

    /// 一直工作，直到被要求停止；记录是否观察到撤销。
    #[derive(Default)]
    struct UntilCancelled {
        saw_cancel: AtomicBool,
    }
    impl WorkerBackend for UntilCancelled {
        fn execute(
            &self,
            _: &Agent,
            _: &Edict,
            hooks: &dyn ExecutionHooks,
        ) -> Result<String, BoxError> {
            while !hooks.is_cancelled() {
                thread::sleep(Duration::from_millis(5));
            }
            self.saw_cancel.store(true, Ordering::SeqCst);
            Err("stopped".into())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn respects_concurrency_limit_and_reports_tools() {
        let backend = Arc::new(Busy::default());
        let scheduler = Scheduler::new(
            backend.clone(),
            Limits {
                max_concurrent: 2,
                timeout: Duration::from_secs(10),
            },
        );
        let progress =
            collect(scheduler.start((1..=6).map(job).collect(), &CancelToken::new())).await;

        assert_eq!(backend.peak.load(Ordering::SeqCst), 2);
        let expected: Vec<_> = (1..=6)
            .map(|id| (id, Outcome::Completed(format!("done {id}"))))
            .collect();
        assert_eq!(outcomes(&progress), expected);
        // 工具审计先于该任务的 Finished 送达。
        for id in 1..=6u64 {
            let tool = progress.iter().position(|p| {
                matches!(p, Progress::ToolInvoked { task, ok, .. } if task.0 == id && *ok == (id % 2 == 0))
            });
            let done = progress
                .iter()
                .position(|p| matches!(p, Progress::Finished { task, .. } if task.0 == id));
            assert!(tool.unwrap() < done.unwrap(), "任务 {id}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn timeout_fails_task_and_signals_backend() {
        let backend = Arc::new(UntilCancelled::default());
        let scheduler = Scheduler::new(
            backend.clone(),
            Limits {
                max_concurrent: 1,
                timeout: Duration::from_millis(50),
            },
        );
        let progress = collect(scheduler.start(vec![job(1)], &CancelToken::new())).await;
        assert_eq!(
            outcomes(&progress),
            [(1, Outcome::Failed("超时（0.05 秒）".into()))]
        );
        // 通道在后端线程退出后才关闭，此时它必然已看到撤销信号。
        assert!(backend.saw_cancel.load(Ordering::SeqCst));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_stops_running_and_skips_queued() {
        let scheduler = Scheduler::new(
            Arc::new(UntilCancelled::default()),
            Limits {
                max_concurrent: 1,
                timeout: Duration::from_secs(10),
            },
        );
        let cancel = CancelToken::new();
        let rx = scheduler.start(vec![job(1), job(2), job(3)], &cancel);
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        assert_eq!(
            outcomes(&collect(rx).await),
            [
                (1, Outcome::Cancelled),
                (2, Outcome::NotStarted),
                (3, Outcome::NotStarted)
            ]
        );
    }
}
