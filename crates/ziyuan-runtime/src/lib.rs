//! 廷枢：运行时调度与模型/工具接口。
//!
//! 执行后端：[`DemoBackend`]（不执行任务）或 [`AgentBackend`]（模型 + 只读工具）。
use std::fmt;
use ziyuan_core::{Agent, AgentId, Edict};
use ziyuan_governance::{Court, GovernanceError, Route};

/// 后端/模型/工具 Adapter 的错误类型：由具体实现决定内部结构。
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub trait WorkerBackend {
    fn execute(&self, actor: &Agent, edict: &Edict) -> Result<String, BoxError>;
}

#[derive(Debug)]
pub enum DispatchError {
    /// 治理层拒绝派工（职位空缺等）。
    Route(GovernanceError),
    Backend(BoxError),
}
impl fmt::Display for DispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Route(e) => e.fmt(f),
            Self::Backend(e) => write!(f, "执行后端失败: {e}"),
        }
    }
}
impl std::error::Error for DispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Route(e) => Some(e),
            Self::Backend(e) => Some(e.as_ref()),
        }
    }
}

/// 仅供验证调度链路；不访问项目、不使用 LLM，也不会真正检查源码。
pub struct DemoBackend;
impl WorkerBackend for DemoBackend {
    fn execute(&self, actor: &Agent, edict: &Edict) -> Result<String, BoxError> {
        Ok(format!(
            "[DEMO ONLY] {}收到诏令：{}。未连接 LLM 或运行任何工具。",
            actor.name, edict.text
        ))
    }
}

#[derive(Clone, Debug)]
pub struct ExecutionReport {
    pub path: Vec<String>,
    pub worker: AgentId,
    pub summary: String,
}

pub fn dispatch(
    court: &Court,
    edict: &Edict,
    backend: &dyn WorkerBackend,
) -> Result<ExecutionReport, DispatchError> {
    let Route { path, worker } = court.route(edict).map_err(DispatchError::Route)?;
    let agent = court
        .agent(worker)
        .ok_or(DispatchError::Route(GovernanceError::UnknownAgent(worker)))?;
    let summary = backend
        .execute(agent, edict)
        .map_err(DispatchError::Backend)?;
    Ok(ExecutionReport {
        path,
        worker,
        summary,
    })
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
    fn execute(&self, actor: &Agent, edict: &Edict) -> Result<String, BoxError> {
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
        )?)
    }
}

pub mod agent_loop;
pub mod tools;
