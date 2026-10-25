//! Tokio 调度器：并发上限、单任务超时与整体撤销。
//!
//! 后端是同步的（模型 HTTP 调用会阻塞），因此每个任务在阻塞线程池中执行；
//! 取消是协作式的：调度器立即给出结论，后端在下一次检查 [`ExecutionHooks::is_cancelled`] 时停下。
use crate::{ExecutionHooks, WorkerBackend};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use ziyuan_core::{Agent, Edict, FileChange, TaskId, Threat, TokenUsage};

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
    /// 玉玺已受承办者官职的上限约束。
    pub edict: Edict,
    /// 承办者官职指定的模型；`None` 用后端默认模型。
    pub model: Option<String>,
    /// 本次执行可用的 tokens（度支余额）；用尽即停，结局为「度支超限」。`None` 不限。
    /// 每次模型调用结束后才知道用量，因此最多超出一次调用。
    pub budget: Option<u64>,
    /// 国库余额（tokens），同一次执行的各件共用；用尽后执行中的停支，未开始的不再开始
    /// （结局为 `NotStarted`，诏令留在队列）。`None` 不限。
    pub treasury: Option<Arc<AtomicI64>>,
    /// 八百里加急：时限为常规的两倍（入队次序由调用方排定，加急在前、先得空位）。
    pub urgent: bool,
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
    /// 一次模型调用的 token 用量。
    Usage { task: TaskId, usage: TokenUsage },
    /// 承办官员呈报的暂存改动；总在该任务的 `Finished` 之前（除非已超时或被撤销）。
    Changes {
        task: TaskId,
        changes: Vec<FileChange>,
    },
    /// 锦衣卫在外来文书中的检出。
    Suspicion {
        task: TaskId,
        source: String,
        threat: Threat,
        excerpt: String,
    },
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

    pub fn backend(&self) -> &dyn WorkerBackend {
        self.backend.as_ref()
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
    let treasury = job.treasury.clone();
    let timeout = if job.urgent { timeout * 2 } else { timeout };
    if treasury
        .as_ref()
        .is_some_and(|t| t.load(Ordering::SeqCst) <= 0)
    {
        return Outcome::NotStarted;
    }
    let spent = Arc::new(AtomicU64::new(0));
    let budget = job.budget;
    let hooks = TaskHooks {
        task: job.edict.id,
        cancel: CancelToken::new(),
        progress,
        budget,
        spent: spent.clone(),
        treasury: treasury.clone(),
    };
    let task_cancel = hooks.cancel.clone();
    // 用到额度即停支；`strict` 时须超出才算（恰好用完而办成的照常采信）。
    let over_budget = move |strict: bool| {
        let reached = |left: i64| if strict { left < 0 } else { left <= 0 };
        if let Some(limit) = budget {
            let left = limit as i64 - spent.load(Ordering::SeqCst) as i64;
            if reached(left) {
                return Some(Outcome::Failed(format!(
                    "度支超限：本次可用 {limit} tokens 已用尽，户部停支"
                )));
            }
        }
        if treasury
            .as_ref()
            .is_some_and(|t| reached(t.load(Ordering::SeqCst)))
        {
            return Some(Outcome::Failed("国库告罄：朝廷度支已用尽，户部停支".into()));
        }
        None
    };
    let handle = tokio::task::spawn_blocking(move || {
        // 许可随执行线程释放：超时后线程仍在收尾时，不会让并发数超过上限。
        let _permit = permit;
        backend.execute(&job, &hooks)
    });
    tokio::select! {
        result = handle => match result {
            // 超支后即使模型答完也不采信；因停支而中断的，结局记为度支超限。
            Ok(Ok(summary)) => over_budget(true).unwrap_or(Outcome::Completed(summary)),
            Ok(Err(_)) if over_budget(false).is_some() => over_budget(false).expect("已停支"),
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
    budget: Option<u64>,
    /// 本次执行已用的 tokens。
    spent: Arc<AtomicU64>,
    treasury: Option<Arc<AtomicI64>>,
}

impl ExecutionHooks for TaskHooks {
    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    fn model_usage(&self, usage: TokenUsage) {
        let _ = self.progress.send(Progress::Usage {
            task: self.task,
            usage,
        });
        let spent = self.spent.fetch_add(usage.total(), Ordering::SeqCst) + usage.total();
        let total = usage.total() as i64;
        let left = self
            .treasury
            .as_ref()
            .map(|t| t.fetch_sub(total, Ordering::SeqCst) - total);
        if self.budget.is_some_and(|limit| spent >= limit) || left.is_some_and(|l| l <= 0) {
            self.cancel.cancel();
        }
    }

    fn token_allowance(&self) -> Option<u64> {
        let own = self
            .budget
            .map(|limit| limit.saturating_sub(self.spent.load(Ordering::SeqCst)));
        let treasury = self
            .treasury
            .as_ref()
            .map(|t| t.load(Ordering::SeqCst).max(0) as u64);
        match (own, treasury) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn text_delta(&self, text: &str) {
        let _ = self.progress.send(Progress::TextDelta {
            task: self.task,
            text: text.into(),
        });
    }

    fn changes_proposed(&self, changes: Vec<FileChange>) {
        let _ = self.progress.send(Progress::Changes {
            task: self.task,
            changes,
        });
    }

    fn suspicion(&self, source: &str, threat: Threat, excerpt: &str) {
        let _ = self.progress.send(Progress::Suspicion {
            task: self.task,
            source: source.into(),
            threat,
            excerpt: excerpt.into(),
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
            model: None,
            budget: None,
            treasury: None,
            urgent: false,
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
        fn execute(&self, job: &Job, hooks: &dyn ExecutionHooks) -> Result<String, BoxError> {
            let (actor, _) = (&job.actor, &job.edict);
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
        fn execute(&self, _job: &Job, hooks: &dyn ExecutionHooks) -> Result<String, BoxError> {
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
                max_concurrent: 2,
                timeout: Duration::from_millis(50),
            },
        );
        // 八百里加急的时限加倍。
        let urgent = Job {
            urgent: true,
            ..job(2)
        };
        let progress = collect(scheduler.start(vec![job(1), urgent], &CancelToken::new())).await;
        assert_eq!(
            outcomes(&progress),
            [
                (1, Outcome::Failed("超时（0.05 秒）".into())),
                (2, Outcome::Failed("超时（0.1 秒）".into()))
            ]
        );
        // 通道在后端线程退出后才关闭，此时它必然已看到撤销信号。
        assert!(backend.saw_cancel.load(Ordering::SeqCst));
    }

    /// 调用模型 `calls` 次（每次 40 tokens），被要求停止时中断。
    struct Spender {
        calls: usize,
    }
    impl WorkerBackend for Spender {
        fn execute(&self, job: &Job, hooks: &dyn ExecutionHooks) -> Result<String, BoxError> {
            for _ in 0..self.calls {
                if hooks.is_cancelled() {
                    return Err("stopped".into());
                }
                hooks.model_usage(TokenUsage {
                    input: 30,
                    output: 10,
                });
            }
            Ok(format!("done {}", job.edict.id.0))
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn budget_stops_a_job_once_spent() {
        let scheduler = Scheduler::new(
            Arc::new(Spender { calls: 5 }),
            Limits {
                max_concurrent: 1,
                timeout: Duration::from_secs(10),
            },
        );
        let budgeted = |id, budget| Job { budget, ..job(id) };
        // 100：第三次调用后（120）停支；200：恰好用完而办成；199：办成但超支，不采信。
        let jobs = vec![
            budgeted(1, Some(100)),
            budgeted(2, Some(200)),
            budgeted(3, Some(199)),
            budgeted(4, None),
        ];
        let progress = collect(scheduler.start(jobs, &CancelToken::new())).await;
        let over = |limit| {
            Outcome::Failed(format!(
                "度支超限：本次可用 {limit} tokens 已用尽，户部停支"
            ))
        };
        assert_eq!(
            outcomes(&progress),
            [
                (1, over(100)),
                (2, Outcome::Completed("done 2".into())),
                (3, over(199)),
                (4, Outcome::Completed("done 4".into())),
            ]
        );
        let calls = |id| {
            progress
                .iter()
                .filter(|p| matches!(p, Progress::Usage { task, .. } if task.0 == id))
                .count()
        };
        assert_eq!((calls(1), calls(2), calls(4)), (3, 5, 5));

        // 国库 150，两件共用：第一件用尽后停支，第二件不再开始。
        let treasury = Arc::new(AtomicI64::new(150));
        let shared = |id| Job {
            treasury: Some(treasury.clone()),
            ..job(id)
        };
        let progress =
            collect(scheduler.start(vec![shared(1), shared(2)], &CancelToken::new())).await;
        assert_eq!(
            outcomes(&progress),
            [
                (
                    1,
                    Outcome::Failed("国库告罄：朝廷度支已用尽，户部停支".into())
                ),
                (2, Outcome::NotStarted),
            ]
        );
        assert_eq!(treasury.load(Ordering::SeqCst), -10);
    }

    /// 花 40 tokens 后回报余额。
    struct Allowance;
    impl WorkerBackend for Allowance {
        fn execute(&self, _job: &Job, hooks: &dyn ExecutionHooks) -> Result<String, BoxError> {
            let before = hooks.token_allowance();
            hooks.model_usage(TokenUsage {
                input: 30,
                output: 10,
            });
            Ok(format!("{before:?} {:?}", hooks.token_allowance()))
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn allowance_is_the_smaller_of_task_budget_and_treasury() {
        let scheduler = Scheduler::new(
            Arc::new(Allowance),
            Limits {
                max_concurrent: 1,
                timeout: Duration::from_secs(10),
            },
        );
        let treasury = Arc::new(AtomicI64::new(500));
        let jobs = vec![
            Job {
                budget: Some(100),
                treasury: Some(treasury.clone()),
                ..job(1)
            },
            Job {
                budget: Some(9000),
                treasury: Some(treasury.clone()),
                ..job(2)
            },
            job(3),
        ];
        let progress = collect(scheduler.start(jobs, &CancelToken::new())).await;
        let done = |s: &str| Outcome::Completed(s.into());
        assert_eq!(
            outcomes(&progress),
            [
                (1, done("Some(100) Some(60)")),
                (2, done("Some(460) Some(420)")),
                (3, done("None None")),
            ]
        );
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
