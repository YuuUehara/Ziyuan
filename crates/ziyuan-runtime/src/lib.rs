//! 廷枢：运行时调度与模型/工具接口。v0.1 默认使用显式模拟后端。
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

pub mod agent_loop {
    //! 独立于治理层的最小 Tool Calling Loop，供后续真实模型 Adapter 实现。
    use super::BoxError;
    use std::fmt;

    #[derive(Debug)]
    pub enum LoopError {
        Model(BoxError),
        EmptyToolCalls,
        UnauthorizedTool(String),
        StepLimit(usize),
    }
    impl fmt::Display for LoopError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Model(e) => write!(f, "模型调用失败: {e}"),
                Self::EmptyToolCalls => f.write_str("模型返回空工具调用"),
                Self::UnauthorizedTool(name) => write!(f, "未授权的工具: {name}"),
                Self::StepLimit(n) => write!(f, "超出最大模型调用步数: {n}"),
            }
        }
    }
    impl std::error::Error for LoopError {}

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct ToolSpec {
        pub name: String,
        pub description: String,
        /// JSON Schema 字符串；由具体模型 Adapter 转换到 API 格式。
        pub input_schema_json: String,
    }
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Message {
        User(String),
        Assistant(String),
        ToolCall {
            id: String,
            name: String,
            args_json: String,
        },
        ToolOutput {
            call_id: String,
            output: String,
        },
    }
    #[derive(Debug)]
    pub struct ToolRequest {
        pub id: String,
        pub name: String,
        pub args_json: String,
    }
    #[derive(Debug)]
    pub enum ModelTurn {
        Final(String),
        Calls(Vec<ToolRequest>),
    }

    pub trait ModelProvider {
        fn next_turn(
            &mut self,
            messages: &[Message],
            tools: &[ToolSpec],
        ) -> Result<ModelTurn, BoxError>;
    }
    pub trait Tool {
        fn spec(&self) -> ToolSpec;
        fn execute(&self, args_json: &str) -> Result<String, BoxError>;
    }

    pub fn run(
        model: &mut dyn ModelProvider,
        tools: &[Box<dyn Tool>],
        user_request: &str,
        max_steps: usize,
    ) -> Result<String, LoopError> {
        let mut messages = vec![Message::User(user_request.into())];
        let specs: Vec<ToolSpec> = tools.iter().map(|tool| tool.spec()).collect();
        for _ in 0..max_steps {
            match model
                .next_turn(&messages, &specs)
                .map_err(LoopError::Model)?
            {
                ModelTurn::Final(answer) => return Ok(answer),
                ModelTurn::Calls(calls) => {
                    if calls.is_empty() {
                        return Err(LoopError::EmptyToolCalls);
                    }
                    for call in calls {
                        // 只有注册的工具才能执行；工具输入校验由 Tool Adapter 实现。
                        let tool = tools
                            .iter()
                            .find(|t| t.spec().name == call.name)
                            .ok_or_else(|| LoopError::UnauthorizedTool(call.name.clone()))?;
                        messages.push(Message::ToolCall {
                            id: call.id.clone(),
                            name: call.name,
                            args_json: call.args_json.clone(),
                        });
                        let result = tool
                            .execute(&call.args_json)
                            .unwrap_or_else(|err| format!("tool error: {err}"));
                        messages.push(Message::ToolOutput {
                            call_id: call.id,
                            output: result,
                        });
                    }
                }
            }
        }
        Err(LoopError::StepLimit(max_steps))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        struct FakeModel;
        impl ModelProvider for FakeModel {
            fn next_turn(
                &mut self,
                messages: &[Message],
                _tools: &[ToolSpec],
            ) -> Result<ModelTurn, BoxError> {
                if messages
                    .iter()
                    .any(|m| matches!(m, Message::ToolOutput { .. }))
                {
                    Ok(ModelTurn::Final("已完成".into()))
                } else {
                    Ok(ModelTurn::Calls(vec![ToolRequest {
                        id: "call1".into(),
                        name: "echo".into(),
                        args_json: "{}".into(),
                    }]))
                }
            }
        }
        struct Echo;
        impl Tool for Echo {
            fn spec(&self) -> ToolSpec {
                ToolSpec {
                    name: "echo".into(),
                    description: "echo".into(),
                    input_schema_json: "{\"type\":\"object\"}".into(),
                }
            }
            fn execute(&self, _args_json: &str) -> Result<String, BoxError> {
                Ok("ok".into())
            }
        }
        #[test]
        fn model_can_call_a_tool() {
            let tools: Vec<Box<dyn Tool>> = vec![Box::new(Echo)];
            assert_eq!(run(&mut FakeModel, &tools, "test", 3).unwrap(), "已完成");
        }
        #[test]
        fn model_step_limit_enforced() {
            assert!(run(&mut FakeModel, &[], "test", 0).is_err());
        }
    }
}
