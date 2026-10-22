//! 独立于治理层的最小 Tool Calling Loop；模型 Adapter 见 `ziyuan-llm`。
use crate::BoxError;
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
    System(String),
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
    fn next_turn(&self, messages: &[Message], tools: &[ToolSpec]) -> Result<ModelTurn, BoxError>;
}
pub trait Tool {
    fn spec(&self) -> ToolSpec;
    fn execute(&self, args_json: &str) -> Result<String, BoxError>;
}

/// 从 `messages`（通常是 System + User）开始，循环调用模型与工具，直到模型给出最终答复。
pub fn run(
    model: &dyn ModelProvider,
    tools: &[Box<dyn Tool>],
    mut messages: Vec<Message>,
    max_steps: usize,
) -> Result<String, LoopError> {
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
                // 只有注册的工具才能执行；任何一个未授权即整轮拒绝。
                let resolved = calls
                    .into_iter()
                    .map(|call| {
                        let tool = tools
                            .iter()
                            .find(|t| t.spec().name == call.name)
                            .ok_or_else(|| LoopError::UnauthorizedTool(call.name.clone()))?;
                        Ok((tool, call))
                    })
                    .collect::<Result<Vec<_>, LoopError>>()?;
                // 先记录本轮全部调用，再记录各自输出：与 OpenAI 等 API 的消息顺序一致。
                for (_, call) in &resolved {
                    messages.push(Message::ToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        args_json: call.args_json.clone(),
                    });
                }
                for (tool, call) in resolved {
                    // 工具错误回传给模型，由模型决定如何继续。
                    let output = tool
                        .execute(&call.args_json)
                        .unwrap_or_else(|err| format!("tool error: {err}"));
                    messages.push(Message::ToolOutput {
                        call_id: call.id,
                        output,
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
            &self,
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
        assert_eq!(
            run(&FakeModel, &tools, vec![Message::User("test".into())], 3).unwrap(),
            "已完成"
        );
    }
    #[test]
    fn model_step_limit_enforced() {
        assert!(run(&FakeModel, &[], vec![Message::User("test".into())], 0).is_err());
    }
}
