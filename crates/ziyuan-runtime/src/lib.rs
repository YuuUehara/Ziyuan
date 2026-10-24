//! 廷枢：运行时调度与模型/工具接口。
//!
//! 执行后端：[`DemoBackend`]（不执行任务）或 [`AgentBackend`]（模型 + 只读工具）。
//! [`Scheduler`] 在 Tokio 上并发执行任务，负责并发上限、超时与撤销。
use std::path::PathBuf;
use ziyuan_core::{Price, TokenUsage};

mod scheduler;
pub use scheduler::{CancelToken, Job, Limits, Outcome, Progress, Scheduler};

/// 后端/模型/工具 Adapter 的错误类型：由具体实现决定内部结构。
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// 执行后端。由 [`Scheduler`] 在阻塞线程池中调用，因此可以同步阻塞（如 HTTP 请求）。
pub trait WorkerBackend: Send + Sync {
    /// 执行 `job`：以 `job.edict.seal`（已受官职上限约束）为权限，`job.model` 为模型（`None` 用默认）。
    fn execute(&self, job: &Job, hooks: &dyn ExecutionHooks) -> Result<String, BoxError>;

    /// 承办官员所用的模型名，供界面显示；不调用模型的后端返回 `None`。
    fn model_name(&self) -> Option<&str> {
        None
    }

    /// 不带工具的单轮问答（如内阁票拟），返回模型的回复与用量。`model` 为 `None` 用默认模型。
    /// 不调用模型的后端返回错误，调用方应自行兜底。
    fn complete(
        &self,
        _model: Option<&str>,
        _system: &str,
        _user: &str,
    ) -> Result<(String, TokenUsage), BoxError> {
        Err("演示后端不调用模型".into())
    }

    /// `model` 的单价，用于估算花费；未知时为 `None`。
    fn price(&self, _model: &str) -> Option<Price> {
        None
    }
}

/// 运行时在执行期间提供给后端：协作式取消、工具调用审计与流式输出。`()` 为空实现。
pub trait ExecutionHooks {
    /// 超时或被撤销后为真；后端应尽快停止并返回。
    fn is_cancelled(&self) -> bool {
        false
    }
    fn tool_invoked(&self, _tool: &str, _args_json: &str, _ok: bool) {}
    /// 模型生成的一段文本（流式）。只用于实时显示，不写入事件簿。
    fn text_delta(&self, _text: &str) {}
    /// 一次模型调用的 token 用量（服务端报告时）。
    fn model_usage(&self, _usage: TokenUsage) {}
}
impl ExecutionHooks for () {}

/// 仅供验证调度链路；不访问项目、不使用 LLM，也不会真正检查源码。
pub struct DemoBackend;
impl WorkerBackend for DemoBackend {
    fn execute(&self, job: &Job, _hooks: &dyn ExecutionHooks) -> Result<String, BoxError> {
        let (actor, edict) = (&job.actor, &job.edict);
        Ok(format!(
            "[DEMO ONLY] {}收到诏令：{}。未连接 LLM 或运行任何工具。",
            actor.name, edict.text
        ))
    }
}

/// 由模型驱动的执行后端：官员通过只读工具调查项目，并以模型的最终答复回奏。
///
/// 每件诏令的工具都按其玉玺（`Edict::seal`）重新构造，模型无法使用玉玺之外的工具或路径。
pub struct AgentBackend {
    model: Box<dyn agent_loop::ModelProvider>,
    /// 按官职切换模型与查询单价；`None` 时只能用默认模型。
    catalog: Option<Box<dyn ModelCatalog>>,
    root: PathBuf,
    max_steps: usize,
}

/// 同一服务商的模型目录：按名称构造模型并给出单价。
pub trait ModelCatalog: Send + Sync {
    fn provider(&self, model: &str) -> Box<dyn agent_loop::ModelProvider>;
    fn price(&self, model: &str) -> Option<Price>;
}

impl AgentBackend {
    /// `root` 为项目根目录，玉玺的查阅范围相对于它。
    pub fn new(model: Box<dyn agent_loop::ModelProvider>, root: PathBuf, max_steps: usize) -> Self {
        Self {
            model,
            catalog: None,
            root,
            max_steps,
        }
    }

    pub fn with_catalog(mut self, catalog: Box<dyn ModelCatalog>) -> Self {
        self.catalog = Some(catalog);
        self
    }
}

impl AgentBackend {
    /// 需要切换到默认模型以外的 `name` 时构造该模型；用默认模型时为 `None`。
    fn switch_to(
        &self,
        name: Option<&str>,
    ) -> Result<Option<Box<dyn agent_loop::ModelProvider>>, BoxError> {
        match name {
            Some(name) if Some(name) != self.model.model_name() => match &self.catalog {
                Some(catalog) => Ok(Some(catalog.provider(name))),
                None => Err(format!("执行后端不支持切换到模型 {name}").into()),
            },
            _ => Ok(None),
        }
    }
}

/// 只累计用量的执行钩子。
#[derive(Default)]
struct UsageTotal(std::cell::Cell<TokenUsage>);
impl ExecutionHooks for UsageTotal {
    fn model_usage(&self, usage: TokenUsage) {
        let mut total = self.0.get();
        total += usage;
        self.0.set(total);
    }
}

impl WorkerBackend for AgentBackend {
    fn complete(
        &self,
        model: Option<&str>,
        system: &str,
        user: &str,
    ) -> Result<(String, TokenUsage), BoxError> {
        let switched = self.switch_to(model)?;
        let model = switched.as_deref().unwrap_or(self.model.as_ref());
        let messages = [
            agent_loop::Message::System(system.into()),
            agent_loop::Message::User(user.into()),
        ];
        let hooks = UsageTotal::default();
        match model.next_turn(&messages, &[], &hooks)? {
            agent_loop::ModelTurn::Final(text) => Ok((text, hooks.0.get())),
            agent_loop::ModelTurn::Calls { .. } => Err("未提供工具，模型却请求调用工具".into()),
        }
    }

    fn model_name(&self) -> Option<&str> {
        self.model.model_name()
    }

    fn price(&self, model: &str) -> Option<Price> {
        self.catalog.as_ref()?.price(model)
    }

    fn execute(&self, job: &Job, hooks: &dyn ExecutionHooks) -> Result<String, BoxError> {
        let (actor, edict) = (&job.actor, &job.edict);
        let switched = self.switch_to(job.model.as_deref())?;
        let model = switched.as_deref().unwrap_or(self.model.as_ref());
        let tools = tools::sealed_tools(&self.root, &edict.seal)?;
        let system = format!(
            "你是紫垣朝廷{}的官员{}，奉诏办理下列事务。\n\
             玉玺授权（{}）。你只能使用所提供的只读工具查阅授权范围内的内容，不能修改文件或执行命令。\n\
             请如实回奏：说明查阅了哪些内容、得出什么结论；没有做到的事不得声称已完成。",
            edict.department.label(),
            actor.name,
            edict.seal,
        );
        let messages = vec![
            agent_loop::Message::System(system),
            agent_loop::Message::User(edict.text.clone()),
        ];
        Ok(agent_loop::run(
            model,
            &tools,
            messages,
            self.max_steps,
            hooks,
        )?)
    }
}

pub mod agent_loop;
pub mod tools;
