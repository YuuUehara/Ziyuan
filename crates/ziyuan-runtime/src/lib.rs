//! 廷枢：运行时调度与模型/工具接口。
//!
//! 执行后端：[`DemoBackend`]（不执行任务）或 [`AgentBackend`]（模型 + 只读工具）。
//! [`Scheduler`] 在 Tokio 上并发执行任务，负责并发上限、超时与撤销。
use std::path::PathBuf;
use ziyuan_core::{Agent, Edict, Price, TokenUsage};

mod scheduler;
pub use scheduler::{CancelToken, Job, Limits, Outcome, Progress, Scheduler};

/// 后端/模型/工具 Adapter 的错误类型：由具体实现决定内部结构。
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// 执行后端。由 [`Scheduler`] 在阻塞线程池中调用，因此可以同步阻塞（如 HTTP 请求）。
pub trait WorkerBackend: Send + Sync {
    fn execute(
        &self,
        actor: &Agent,
        edict: &Edict,
        hooks: &dyn ExecutionHooks,
    ) -> Result<String, BoxError>;

    /// 承办官员所用的模型名，供界面显示；不调用模型的后端返回 `None`。
    fn model_name(&self) -> Option<&str> {
        None
    }

    /// 模型单价，用于估算花费；未知时为 `None`。
    fn price(&self) -> Option<Price> {
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
    fn execute(
        &self,
        actor: &Agent,
        edict: &Edict,
        _hooks: &dyn ExecutionHooks,
    ) -> Result<String, BoxError> {
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
    root: PathBuf,
    max_steps: usize,
}

impl AgentBackend {
    /// `root` 为项目根目录，玉玺的查阅范围相对于它。
    pub fn new(model: Box<dyn agent_loop::ModelProvider>, root: PathBuf, max_steps: usize) -> Self {
        Self {
            model,
            root,
            max_steps,
        }
    }
}

impl WorkerBackend for AgentBackend {
    fn model_name(&self) -> Option<&str> {
        self.model.model_name()
    }

    fn price(&self) -> Option<Price> {
        self.model.price()
    }

    fn execute(
        &self,
        actor: &Agent,
        edict: &Edict,
        hooks: &dyn ExecutionHooks,
    ) -> Result<String, BoxError> {
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
            self.model.as_ref(),
            &tools,
            messages,
            self.max_steps,
            hooks,
        )?)
    }
}

pub mod agent_loop;
pub mod tools;
