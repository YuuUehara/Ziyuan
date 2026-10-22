//! 廷枢：运行时调度与模型/工具接口。
//!
//! 执行后端：[`DemoBackend`]（不执行任务）或 [`AgentBackend`]（模型 + 只读工具）。
//! [`Scheduler`] 在 Tokio 上并发执行任务，负责并发上限、超时与撤销。
use ziyuan_core::{Agent, Edict};

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
}

/// 运行时在执行期间提供给后端：协作式取消与工具调用审计。`()` 为空实现。
pub trait ExecutionHooks {
    /// 超时或被撤销后为真；后端应尽快停止并返回。
    fn is_cancelled(&self) -> bool {
        false
    }
    fn tool_invoked(&self, _tool: &str, _args_json: &str, _ok: bool) {}
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
pub struct AgentBackend {
    model: Box<dyn agent_loop::ModelProvider>,
    tools: Vec<Box<dyn agent_loop::Tool>>,
    max_steps: usize,
}

impl AgentBackend {
    pub fn new(
        model: Box<dyn agent_loop::ModelProvider>,
        tools: Vec<Box<dyn agent_loop::Tool>>,
        max_steps: usize,
    ) -> Self {
        Self {
            model,
            tools,
            max_steps,
        }
    }
}

impl WorkerBackend for AgentBackend {
    fn execute(
        &self,
        actor: &Agent,
        edict: &Edict,
        hooks: &dyn ExecutionHooks,
    ) -> Result<String, BoxError> {
        let system = format!(
            "你是紫垣朝廷{}的官员{}，奉诏办理下列事务。\n\
             你只能使用所提供的只读工具查阅当前项目，不能修改文件或执行命令。\n\
             请如实回奏：说明查阅了哪些内容、得出什么结论；没有做到的事不得声称已完成。",
            edict.department.label(),
            actor.name,
        );
        let messages = vec![
            agent_loop::Message::System(system),
            agent_loop::Message::User(edict.text.clone()),
        ];
        Ok(agent_loop::run(
            self.model.as_ref(),
            &self.tools,
            messages,
            self.max_steps,
            hooks,
        )?)
    }
}

pub mod agent_loop;
pub mod tools;
