//! 模型 Adapter：OpenAI 兼容的 Chat Completions API（OpenAI、DeepSeek、通义千问、Kimi、Ollama 等）。
//!
//! 同步、流式（SSE）；文本片段经 `ExecutionHooks::text_delta` 实时报告。只实现 Tool Calling 所需的最小字段。
use serde_json::{Value, json};
use std::env;
use std::io::{BufRead, BufReader};
use std::time::Duration;
use ziyuan_runtime::agent_loop::{Message, ModelProvider, ModelTurn, ToolRequest, ToolSpec};
use ziyuan_runtime::{BoxError, ExecutionHooks};

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
    fn next_turn(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
        hooks: &dyn ExecutionHooks,
    ) -> Result<ModelTurn, BoxError> {
        let mut body = json!({
            "model": self.model,
            "messages": encode_messages(messages),
            "stream": true,
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
        let response = request.send(body.to_string())?;
        let status = response.status();
        let is_stream = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let mut body = response.into_body();
        if !status.is_success() {
            let text = body.read_to_string()?;
            let snippet: String = text.chars().take(500).collect();
            return Err(format!("模型 API 返回 {status}: {snippet}").into());
        }
        if !is_stream {
            // 不支持流式的服务会忽略 `stream`，直接返回完整 JSON。
            let turn = decode_turn(&serde_json::from_str(&body.read_to_string()?)?)?;
            if let ModelTurn::Final(text) = &turn {
                hooks.text_delta(text);
            }
            return Ok(turn);
        }
        read_stream(BufReader::new(body.into_reader()), hooks)
    }
}

/// 流式响应中逐片拼接的工具调用。
#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

/// 解析 SSE（`data: {chunk}` … `data: [DONE]`）。文本片段实时交给 `hooks`；
/// 工具调用按 `index` 拼接。每读一行检查一次撤销。
fn read_stream(reader: impl BufRead, hooks: &dyn ExecutionHooks) -> Result<ModelTurn, BoxError> {
    let mut content = String::new();
    let mut calls: Vec<PartialCall> = Vec::new();
    let mut done = false;
    for line in reader.lines() {
        if hooks.is_cancelled() {
            return Err("已撤销，中止流式响应".into());
        }
        let line = line?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data == "[DONE]" {
            done = true;
            break;
        }
        let chunk: Value = serde_json::from_str(data)?;
        if let Some(error) = chunk.get("error") {
            return Err(format!("模型流式响应返回错误: {error}").into());
        }
        let delta = &chunk["choices"][0]["delta"];
        if let Some(text) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            hooks.text_delta(text);
            content.push_str(text);
        }
        for part in delta["tool_calls"].as_array().into_iter().flatten() {
            let index = part["index"]
                .as_u64()
                .map(|i| i as usize)
                .filter(|i| *i <= calls.len())
                .ok_or_else(|| format!("tool_call 的 index 无效: {part}"))?;
            if index == calls.len() {
                calls.push(PartialCall::default());
            }
            let call = &mut calls[index];
            if let Some(id) = part["id"].as_str() {
                call.id = id.to_owned();
            }
            if let Some(name) = part.pointer("/function/name").and_then(Value::as_str) {
                call.name.push_str(name);
            }
            if let Some(args) = part.pointer("/function/arguments").and_then(Value::as_str) {
                call.arguments.push_str(args);
            }
        }
    }
    if !done {
        return Err("流式响应在 [DONE] 之前中断".into());
    }
    if calls.is_empty() {
        return Ok(ModelTurn::Final(content));
    }
    calls
        .into_iter()
        .map(|call| {
            if call.id.is_empty() || call.name.is_empty() {
                return Err(BoxError::from("流式 tool_call 缺少 id 或 name"));
            }
            Ok(ToolRequest {
                id: call.id,
                name: call.name,
                args_json: call.arguments,
            })
        })
        .collect::<Result<_, _>>()
        .map(ModelTurn::Calls)
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
                // 以 `data:` 开头的回复按 SSE 流发送，其余按普通 JSON。
                let content_type = if reply.starts_with("data:") {
                    "text/event-stream"
                } else {
                    "application/json"
                };
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
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

    /// 记录流式文本；可在收到 `cancel_after` 个片段后报告已撤销。
    #[derive(Default)]
    struct Recorder {
        deltas: std::cell::RefCell<Vec<String>>,
        cancel_after: Option<usize>,
    }
    impl ExecutionHooks for Recorder {
        fn is_cancelled(&self) -> bool {
            self.cancel_after
                .is_some_and(|n| self.deltas.borrow().len() >= n)
        }
        fn text_delta(&self, text: &str) {
            self.deltas.borrow_mut().push(text.into());
        }
    }

    fn sse(chunks: &[Value], done: bool) -> String {
        let mut out: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
        if done {
            out.push_str("data: [DONE]\n\n");
        }
        out
    }

    fn delta(delta: Value) -> Value {
        json!({"choices": [{"index": 0, "delta": delta}]})
    }

    #[test]
    fn streamed_tool_calls_are_assembled_across_chunks() {
        let reply = sse(
            &[
                delta(json!({"role": "assistant", "content": "先查"})),
                delta(
                    json!({"tool_calls": [{"index": 0, "id": "a", "type": "function",
                    "function": {"name": "read_file", "arguments": "{\"pa"}}]}),
                ),
                delta(
                    json!({"tool_calls": [{"index": 0, "function": {"arguments": "th\":\"x\"}"}}]}),
                ),
                delta(
                    json!({"tool_calls": [{"index": 1, "id": "b", "type": "function",
                    "function": {"name": "search_code", "arguments": "{}"}}]}),
                ),
            ],
            true,
        );
        let (url, seen) = mock_server(vec![(200, reply)]);
        let hooks = Recorder::default();
        let turn = OpenAiCompatible::new(url, None, "m")
            .next_turn(&[Message::User("q".into())], &[], &hooks)
            .unwrap();
        let ModelTurn::Calls(calls) = turn else {
            panic!("应为工具调用: {turn:?}");
        };
        let calls: Vec<_> = calls
            .iter()
            .map(|c| (c.id.as_str(), c.name.as_str(), c.args_json.as_str()))
            .collect();
        assert_eq!(
            calls,
            [
                ("a", "read_file", r#"{"path":"x"}"#),
                ("b", "search_code", "{}")
            ]
        );
        assert_eq!(*hooks.deltas.borrow(), ["先查"]);
        assert_eq!(seen.lock().unwrap()[0].0["stream"], true);
    }

    #[test]
    fn streamed_text_is_reported_as_it_arrives() {
        let chunks: Vec<_> = ["紫", "垣", "回奏"]
            .iter()
            .map(|t| delta(json!({"content": t})))
            .collect();
        let (url, _) = mock_server(vec![(200, sse(&chunks, true))]);
        let hooks = Recorder::default();
        let turn = OpenAiCompatible::new(url, None, "m")
            .next_turn(&[Message::User("q".into())], &[], &hooks)
            .unwrap();
        assert!(
            matches!(turn, ModelTurn::Final(ref t) if t == "紫垣回奏"),
            "{turn:?}"
        );
        assert_eq!(*hooks.deltas.borrow(), ["紫", "垣", "回奏"]);
    }

    #[test]
    fn stream_stops_when_cancelled_or_cut_off() {
        let chunks: Vec<_> = ["一", "二", "三"]
            .iter()
            .map(|t| delta(json!({"content": t})))
            .collect();
        let (url, _) = mock_server(vec![(200, sse(&chunks, true)), (200, sse(&chunks, false))]);
        let model = OpenAiCompatible::new(url, None, "m");
        let messages = [Message::User("q".into())];

        let hooks = Recorder {
            cancel_after: Some(1),
            ..Recorder::default()
        };
        let err = model.next_turn(&messages, &[], &hooks).unwrap_err();
        assert!(err.to_string().contains("撤销"), "{err}");
        assert_eq!(*hooks.deltas.borrow(), ["一"], "撤销后不应再读取");

        let err = model.next_turn(&messages, &[], &()).unwrap_err();
        assert!(err.to_string().contains("[DONE]"), "{err}");
    }

    #[test]
    fn http_error_is_reported_with_status() {
        let (url, _) = mock_server(vec![(401, r#"{"error":"bad key"}"#.into())]);
        let model = OpenAiCompatible::new(url, None, "m");
        let err = model
            .next_turn(&[Message::User("hi".into())], &[], &())
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
