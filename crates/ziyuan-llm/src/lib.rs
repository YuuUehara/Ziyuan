//! 模型 Adapter：OpenAI 兼容的 Chat Completions API（OpenAI、DeepSeek、通义千问、Kimi、Ollama 等）。
//!
//! 同步、非流式；只实现 Tool Calling 所需的最小字段。
use serde_json::{Value, json};
use std::env;
use std::time::Duration;
use ziyuan_runtime::BoxError;
use ziyuan_runtime::agent_loop::{Message, ModelProvider, ModelTurn, ToolRequest, ToolSpec};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

pub struct OpenAiCompatible {
    base_url: String,
    api_key: Option<String>,
    model: String,
    agent: ureq::Agent,
}

impl OpenAiCompatible {
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key,
            model: model.into(),
            agent,
        }
    }

    /// 读取 `ZIYUAN_LLM_MODEL`（必需）、`ZIYUAN_LLM_BASE_URL`、`ZIYUAN_LLM_API_KEY`。
    /// 未设置模型时返回 `None`。
    pub fn from_env() -> Option<Self> {
        let model = env::var("ZIYUAN_LLM_MODEL")
            .ok()
            .filter(|m| !m.is_empty())?;
        let base_url = env::var("ZIYUAN_LLM_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.into());
        let api_key = env::var("ZIYUAN_LLM_API_KEY")
            .ok()
            .filter(|k| !k.is_empty());
        Some(Self::new(base_url, api_key, model))
    }

    pub fn model(&self) -> &str {
        &self.model
    }
}

impl ModelProvider for OpenAiCompatible {
    fn next_turn(&self, messages: &[Message], tools: &[ToolSpec]) -> Result<ModelTurn, BoxError> {
        let mut body = json!({
            "model": self.model,
            "messages": encode_messages(messages),
        });
        if !tools.is_empty() {
            body["tools"] = encode_tools(tools)?;
        }
        let mut request = self
            .agent
            .post(format!("{}/chat/completions", self.base_url))
            .header("Content-Type", "application/json");
        if let Some(key) = &self.api_key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }
        let mut response = request.send(body.to_string())?;
        let status = response.status();
        let text = response.body_mut().read_to_string()?;
        if !status.is_success() {
            let snippet: String = text.chars().take(500).collect();
            return Err(format!("模型 API 返回 {status}: {snippet}").into());
        }
        decode_turn(&serde_json::from_str(&text)?)
    }
}

fn encode_tools(tools: &[ToolSpec]) -> Result<Value, BoxError> {
    tools
        .iter()
        .map(|tool| {
            let parameters: Value = serde_json::from_str(&tool.input_schema_json)?;
            Ok(json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": parameters,
                },
            }))
        })
        .collect()
}

/// 连续的 `ToolCall` 合并为一条带 `tool_calls` 的 assistant 消息。
fn encode_messages(messages: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for message in messages {
        match message {
            Message::System(text) => out.push(json!({"role": "system", "content": text})),
            Message::User(text) => out.push(json!({"role": "user", "content": text})),
            Message::Assistant(text) => out.push(json!({"role": "assistant", "content": text})),
            Message::ToolCall {
                id,
                name,
                args_json,
            } => {
                let call = json!({
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": args_json},
                });
                match out.last_mut() {
                    Some(last) if last.get("tool_calls").is_some() => {
                        last["tool_calls"].as_array_mut().expect("数组").push(call);
                    }
                    _ => out
                        .push(json!({"role": "assistant", "content": null, "tool_calls": [call]})),
                }
            }
            Message::ToolOutput { call_id, output } => {
                out.push(json!({"role": "tool", "tool_call_id": call_id, "content": output}))
            }
        }
    }
    out
}

fn decode_turn(response: &Value) -> Result<ModelTurn, BoxError> {
    let message = &response["choices"][0]["message"];
    if let Some(calls) = message["tool_calls"].as_array().filter(|c| !c.is_empty()) {
        return calls
            .iter()
            .map(|call| {
                let field = |pointer: &str| {
                    call.pointer(pointer)
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| BoxError::from(format!("tool_call 缺少 {pointer}")))
                };
                Ok(ToolRequest {
                    id: field("/id")?,
                    name: field("/function/name")?,
                    args_json: field("/function/arguments")?,
                })
            })
            .collect::<Result<_, BoxError>>()
            .map(ModelTurn::Calls);
    }
    match message["content"].as_str() {
        Some(content) => Ok(ModelTurn::Final(content.to_owned())),
        None => Err(format!("无法识别的模型响应: {response}").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use ziyuan_core::{Agent, AgentId, Capability, Department, Edict, Seal, TaskId};
    use ziyuan_runtime::{AgentBackend, WorkerBackend};

    /// 每个请求的 JSON 请求体与 Authorization 头。
    type Requests = Arc<Mutex<Vec<(Value, String)>>>;

    /// 依次返回 `replies`，并记录收到的请求体与 Authorization 头。
    fn mock_server(replies: Vec<(u16, String)>) -> (String, Requests) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        thread::spawn(move || {
            for (status, reply) in replies {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let (mut length, mut auth) = (0, String::new());
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let lower = line.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    if lower.starts_with("authorization:") {
                        auth = line["authorization:".len()..].trim().to_owned();
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                log.lock()
                    .unwrap()
                    .push((serde_json::from_slice(&body).unwrap(), auth));
                let mut stream = reader.into_inner();
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .unwrap();
            }
        });
        (url, seen)
    }

    fn tool_call_reply(id: &str, name: &str, args: &str) -> String {
        json!({"choices": [{"message": {"role": "assistant", "content": null, "tool_calls": [
            {"id": id, "type": "function", "function": {"name": name, "arguments": args}}
        ]}}]})
        .to_string()
    }

    fn final_reply(text: &str) -> String {
        json!({"choices": [{"message": {"role": "assistant", "content": text}}]}).to_string()
    }

    #[test]
    fn agent_backend_reads_project_through_api() {
        let root = std::env::temp_dir().join(format!("ziyuan-llm-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("lib.rs"), "unsafe fn danger() {}\n").unwrap();
        let (url, seen) = mock_server(vec![
            (
                200,
                tool_call_reply("c1", "search_code", r#"{"query":"unsafe"}"#),
            ),
            (200, final_reply("lib.rs 第 1 行有 unsafe")),
        ]);
        let backend = AgentBackend::new(
            Box::new(OpenAiCompatible::new(url, Some("sk-test".into()), "m")),
            root.clone(),
            4,
        );
        let actor = Agent {
            id: AgentId(4),
            name: "陈工".into(),
            speciality: Department::Works,
        };
        let edict = Edict {
            id: TaskId(1),
            department: Department::Works,
            text: "查 unsafe".into(),
            // 只盖 search_code：模型不应看到 read_file。
            seal: Seal {
                capabilities: [Capability::SearchCode].into(),
                scope: vec![".".into()],
            },
        };
        assert_eq!(
            backend.execute(&actor, &edict, &()).unwrap(),
            "lib.rs 第 1 行有 unsafe"
        );

        let seen = seen.lock().unwrap();
        let (first, auth) = &seen[0];
        assert_eq!(auth, "Bearer sk-test");
        assert_eq!(first["model"], "m");
        assert_eq!(first["messages"][0]["role"], "system");
        assert_eq!(first["messages"][1]["content"], "查 unsafe");
        let names: Vec<_> = first["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["search_code"]);
        let system = first["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("工具：search_code"), "{system}");
        // 第二次请求带回工具调用与真实工具输出。
        let messages = &seen[1].0["messages"];
        assert_eq!(messages[2]["tool_calls"][0]["id"], "c1");
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "c1");
        assert_eq!(messages[3]["content"], "lib.rs:1: unsafe fn danger() {}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn http_error_is_reported_with_status() {
        let (url, _) = mock_server(vec![(401, r#"{"error":"bad key"}"#.into())]);
        let model = OpenAiCompatible::new(url, None, "m");
        let err = model
            .next_turn(&[Message::User("hi".into())], &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("401") && err.contains("bad key"), "{err}");
    }

    #[test]
    fn parallel_tool_calls_share_one_assistant_message() {
        let messages = [
            Message::User("q".into()),
            Message::ToolCall {
                id: "a".into(),
                name: "read_file".into(),
                args_json: "{}".into(),
            },
            Message::ToolCall {
                id: "b".into(),
                name: "search_code".into(),
                args_json: "{}".into(),
            },
            Message::ToolOutput {
                call_id: "a".into(),
                output: "1".into(),
            },
            Message::ToolOutput {
                call_id: "b".into(),
                output: "2".into(),
            },
        ];
        let encoded = encode_messages(&messages);
        assert_eq!(encoded.len(), 4);
        assert_eq!(encoded[1]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(encoded[3]["tool_call_id"], "b");
    }
}
