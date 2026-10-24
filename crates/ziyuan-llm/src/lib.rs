//! 模型 Adapter：OpenAI 兼容的 Chat Completions API（OpenAI、DeepSeek、通义千问、Kimi、Ollama 等）。
//!
//! 同步、流式（SSE）；文本片段经 `ExecutionHooks::text_delta` 实时报告。只实现 Tool Calling 所需的最小字段。
use serde_json::{Value, json};
use std::env;
use std::io::{BufRead, BufReader};
use std::time::Duration;
use ziyuan_core::{Price, TokenUsage};
use ziyuan_runtime::agent_loop::{Message, ModelProvider, ModelTurn, ToolRequest, ToolSpec};
use ziyuan_runtime::{BoxError, ExecutionHooks, ModelCatalog};

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

    /// 同一服务地址与 Key 下的模型目录，供按官职切换模型。
    pub fn catalog(&self) -> Catalog {
        Catalog {
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
        }
    }
}

pub struct Catalog {
    base_url: String,
    api_key: Option<String>,
}

impl ModelCatalog for Catalog {
    fn provider(&self, model: &str) -> Box<dyn ModelProvider> {
        Box::new(OpenAiCompatible::new(
            self.base_url.clone(),
            self.api_key.clone(),
            model,
        ))
    }

    fn price(&self, model: &str) -> Option<Price> {
        price_for(model)
    }
}

/// 估算花费所用的单价。内置价目取 DeepSeek 官方 2026-10 的高峰时段、未命中缓存价，是估算上限：
/// 空闲时段与缓存命中都更便宜。不在价目中的模型用 `ZIYUAN_LLM_PRICE`（「输入,输出[,币种]」，
/// 每百万 token），未设置则返回 `None`。
pub fn price_for(model: &str) -> Option<Price> {
    let (input, output) = match model {
        "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4-flash-vision-exp" => (2.0, 8.0),
        "deepseek-v4-pro" => (9.0, 27.0),
        _ => {
            return env::var("ZIYUAN_LLM_PRICE")
                .ok()
                .and_then(|s| parse_price(&s));
        }
    };
    Some(Price {
        input_per_million: input,
        output_per_million: output,
        currency: "元".into(),
    })
}

fn parse_price(spec: &str) -> Option<Price> {
    let mut parts = spec.split(',').map(str::trim);
    let input = parts.next()?.parse().ok()?;
    let output = parts.next()?.parse().ok()?;
    let currency = parts.next().filter(|c| !c.is_empty()).unwrap_or("元");
    Some(Price {
        input_per_million: input,
        output_per_million: output,
        currency: currency.into(),
    })
}

impl ModelProvider for OpenAiCompatible {
    fn model_name(&self) -> Option<&str> {
        Some(&self.model)
    }

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
            // 让流的最后一块携带 usage；不支持的服务会忽略。
            "stream_options": {"include_usage": true},
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
            let response: Value = serde_json::from_str(&body.read_to_string()?)?;
            report_usage(&response, hooks);
            let turn = decode_turn(&response)?;
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
    let mut reasoning = String::new();
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
        report_usage(&chunk, hooks);
        let delta = &chunk["choices"][0]["delta"];
        // 思考模式的思维链不显示，但须保留以便回传。
        if let Some(text) = delta["reasoning_content"].as_str() {
            reasoning.push_str(text);
        }
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
    // 调用工具前的话自成一行，不与之后的回奏粘连。
    if !content.is_empty() && !content.ends_with('\n') {
        hooks.text_delta("\n");
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
        .map(|calls| ModelTurn::Calls {
            calls,
            text: content,
            reasoning,
        })
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
            Message::CallPrelude { text, reasoning } => {
                let mut message = json!({
                    "role": "assistant",
                    "content": if text.is_empty() { Value::Null } else { json!(text) },
                    "tool_calls": [],
                });
                if !reasoning.is_empty() {
                    message["reasoning_content"] = json!(reasoning);
                }
                out.push(message);
            }
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

/// OpenAI 风格的 `usage` 字段（流式时位于最后一块，其 `choices` 为空）。
fn report_usage(value: &Value, hooks: &dyn ExecutionHooks) {
    let usage = &value["usage"];
    let (Some(input), Some(output)) = (
        usage["prompt_tokens"].as_u64(),
        usage["completion_tokens"].as_u64(),
    ) else {
        return;
    };
    hooks.model_usage(TokenUsage { input, output });
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
            .map(|calls| ModelTurn::Calls {
                calls,
                text: message["content"].as_str().unwrap_or_default().to_owned(),
                reasoning: message["reasoning_content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            });
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
    use ziyuan_runtime::{AgentBackend, Job, WorkerBackend};

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
        let default = OpenAiCompatible::new(url, Some("sk-test".into()), "m");
        let catalog = Box::new(default.catalog());
        let backend = AgentBackend::new(Box::new(default), root.clone(), 4).with_catalog(catalog);
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
        // 官职指定了另一个模型：请求应发给该模型，沿用同一服务地址与 Key。
        let job = Job {
            actor,
            edict,
            model: Some("deepseek-v4-pro".into()),
            budget: None,
            treasury: None,
        };
        assert_eq!(
            backend.execute(&job, &()).unwrap(),
            "lib.rs 第 1 行有 unsafe"
        );
        assert_eq!(
            backend.price("deepseek-v4-pro").unwrap().output_per_million,
            27.0
        );

        let seen = seen.lock().unwrap();
        let (first, auth) = &seen[0];
        assert_eq!(auth, "Bearer sk-test");
        assert_eq!(first["model"], "deepseek-v4-pro");
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
        usage: std::cell::RefCell<Vec<TokenUsage>>,
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
        fn model_usage(&self, usage: TokenUsage) {
            self.usage.borrow_mut().push(usage);
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
                delta(json!({"role": "assistant", "reasoning_content": "想一想"})),
                delta(json!({"content": "先查"})),
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
        let ModelTurn::Calls {
            calls,
            text,
            reasoning,
        } = turn
        else {
            panic!("应为工具调用: {turn:?}");
        };
        assert_eq!((text.as_str(), reasoning.as_str()), ("先查", "想一想"));
        // 思维链与调用前的话随下一次请求回传（DeepSeek 思考模式要求），且与工具调用同在一条消息。
        let mut history = vec![Message::CallPrelude { text, reasoning }];
        history.extend(calls.iter().map(|c| Message::ToolCall {
            id: c.id.clone(),
            name: c.name.clone(),
            args_json: c.args_json.clone(),
        }));
        let encoded = encode_messages(&history);
        assert_eq!(encoded.len(), 1);
        assert_eq!(encoded[0]["reasoning_content"], "想一想");
        assert_eq!(encoded[0]["content"], "先查");
        assert_eq!(encoded[0]["tool_calls"].as_array().unwrap().len(), 2);
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
        assert_eq!(
            *hooks.deltas.borrow(),
            ["先查", "\n"],
            "思维链不显示；调用前的话以换行结束"
        );
        assert_eq!(seen.lock().unwrap()[0].0["stream"], true);
    }

    #[test]
    fn streamed_text_is_reported_as_it_arrives() {
        let mut chunks: Vec<_> = ["紫", "垣", "回奏"]
            .iter()
            .map(|t| delta(json!({"content": t})))
            .collect();
        chunks.push(json!({"choices": [], "usage": {"prompt_tokens": 321, "completion_tokens": 9, "total_tokens": 330}}));
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
        assert_eq!(
            *hooks.usage.borrow(),
            [TokenUsage {
                input: 321,
                output: 9
            }]
        );
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
    fn prices_parse_and_estimate_cost() {
        let price = parse_price("2, 8").unwrap();
        let usage = TokenUsage {
            input: 1_000_000,
            output: 500_000,
        };
        assert_eq!(price.cost(usage), 6.0);
        assert_eq!(price.describe(usage), "约 6.0000 元");
        assert_eq!(parse_price("0.5,1.5,USD").unwrap().currency, "USD");
        assert!(parse_price("免费").is_none() && parse_price("1").is_none());
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
