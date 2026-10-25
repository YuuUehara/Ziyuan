//! 模型 Adapter：OpenAI 兼容的 Chat Completions API（OpenAI、DeepSeek、通义千问、Kimi、Ollama 等）。
//!
//! 同步、流式（SSE）；文本片段经 `ExecutionHooks::text_delta` 实时报告。只实现 Tool Calling 所需的最小字段。
use serde_json::{Value, json};
use std::env;
use std::io::{BufRead, BufReader};
use std::time::Duration;
use ziyuan_core::{Price, TokenUsage};
use ziyuan_runtime::agent_loop::{Message, ModelProvider, ModelTurn, ToolRequest, ToolSpec};
use ziyuan_runtime::{BoxError, ExecutionHooks, ModelCatalog, guard};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// 度支余额低于此数时才向 API 传 `max_tokens`（再高的值有些服务会拒绝）。
const MAX_TOKENS_CAP: u64 = 8192;

/// 请求失败（网络错误、429、5xx）时的重试：第 n 次重试前等待 `base × 2ⁿ`（不超过 `max`），
/// 服务端给了 `Retry-After` 则照办（同样不超过 `max`）。流式响应开始之后不重试。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retry {
    pub attempts: u32,
    pub base: Duration,
    pub max: Duration,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            attempts: 3,
            base: Duration::from_secs(1),
            max: Duration::from_secs(30),
        }
    }
}

impl Retry {
    fn delay(&self, retry: u32, after: Option<Duration>) -> Duration {
        after
            .unwrap_or_else(|| self.base.saturating_mul(1 << retry.min(16)))
            .min(self.max)
    }
}

pub struct OpenAiCompatible {
    base_url: String,
    api_key: Option<String>,
    model: String,
    agent: ureq::Agent,
    retry: Retry,
}

/// 一次请求的失败：可否重试、服务端要求等多久、服务端的请求 ID。
struct Failure {
    error: BoxError,
    retryable: bool,
    after: Option<Duration>,
    request_id: Option<String>,
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
            retry: Retry::default(),
        }
    }

    pub fn with_retry(mut self, retry: Retry) -> Self {
        self.retry = retry;
        self
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
        let mut retry = Retry::default();
        if let Some(n) = env::var("ZIYUAN_LLM_RETRIES")
            .ok()
            .and_then(|n| n.parse().ok())
        {
            retry.attempts = n;
        }
        Some(Self::new(base_url, api_key, model).with_retry(retry))
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// 同一服务地址与 Key 下的模型目录，供按官职切换模型。
    pub fn catalog(&self) -> Catalog {
        Catalog {
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            retry: self.retry,
        }
    }
}

pub struct Catalog {
    base_url: String,
    api_key: Option<String>,
    retry: Retry,
}

impl ModelCatalog for Catalog {
    fn provider(&self, model: &str) -> Box<dyn ModelProvider> {
        Box::new(
            OpenAiCompatible::new(self.base_url.clone(), self.api_key.clone(), model)
                .with_retry(self.retry),
        )
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

    /// 每次调用带一个 trace ID（请求头 `X-Client-Request-Id`，重试沿用），出错时连同服务端的
    /// 请求 ID 写进错误信息，便于对照服务商的日志。
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
        // 度支将尽：输出至多用完余额。
        if let Some(room) = hooks.max_output().filter(|&room| room < MAX_TOKENS_CAP) {
            body[output_cap_field(&self.base_url)] = json!(room.max(1));
        }
        let body = body.to_string();
        let trace = format!("zy-{}", &guard::nonce()[..12]);
        hooks.model_trace(&trace);
        let mut retry = 0;
        loop {
            let failure = match self.attempt(&body, &trace, hooks) {
                Ok(turn) => return Ok(turn),
                Err(failure) => failure,
            };
            if !failure.retryable || retry >= self.retry.attempts || hooks.is_cancelled() {
                let server = failure
                    .request_id
                    .map_or(String::new(), |id| format!("，服务端 {id}"));
                let tries = if retry > 0 {
                    format!("，已重试 {retry} 次")
                } else {
                    String::new()
                };
                return Err(format!("{}（trace {trace}{server}{tries}）", failure.error).into());
            }
            // 退避期间也响应撤销。
            let wait = self.retry.delay(retry, failure.after);
            let until = std::time::Instant::now() + wait;
            while std::time::Instant::now() < until {
                if hooks.is_cancelled() {
                    return Err("已撤销，不再重试".into());
                }
                std::thread::sleep(
                    (until - std::time::Instant::now()).min(Duration::from_millis(50)),
                );
            }
            retry += 1;
        }
    }
}

impl OpenAiCompatible {
    fn attempt(
        &self,
        body: &str,
        trace: &str,
        hooks: &dyn ExecutionHooks,
    ) -> Result<ModelTurn, Failure> {
        let fail = |error: BoxError, retryable: bool| Failure {
            error,
            retryable,
            after: None,
            request_id: None,
        };
        let mut request = self
            .agent
            .post(format!("{}/chat/completions", self.base_url))
            .header("Content-Type", "application/json")
            .header("X-Client-Request-Id", trace);
        if let Some(key) = &self.api_key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }
        // 连接失败、超时等网络错误可以重试。
        let response = request
            .send(body)
            .map_err(|e| fail(format!("模型 API 请求失败: {e}").into(), true))?;
        let status = response.status();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let request_id = header("x-request-id").or_else(|| header("request-id"));
        let after = header("retry-after")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let is_stream = header("content-type").is_some_and(|v| v.starts_with("text/event-stream"));
        let mut body = response.into_body();
        let failed = |error: BoxError, retryable: bool| Failure {
            error,
            retryable,
            after,
            request_id: request_id.clone(),
        };
        if !status.is_success() {
            let text = body.read_to_string().unwrap_or_default();
            let snippet: String = text.chars().take(500).collect();
            let retryable = status.as_u16() == 429 || status.is_server_error();
            return Err(failed(
                format!("模型 API 返回 {status}: {snippet}").into(),
                retryable,
            ));
        }
        // 以下已开始接收答复（可能已流式显示了文字），出错不再重试。
        if !is_stream {
            // 不支持流式的服务会忽略 `stream`，直接返回完整 JSON。
            let parsed = body
                .read_to_string()
                .map_err(BoxError::from)
                .and_then(|text| serde_json::from_str::<Value>(&text).map_err(BoxError::from));
            let response = parsed.map_err(|e| failed(e, false))?;
            report_usage(&response, hooks);
            let turn = decode_turn(&response).map_err(|e| failed(e, false))?;
            if let ModelTurn::Final(text) = &turn {
                hooks.text_delta(text);
            }
            return Ok(turn);
        }
        read_stream(BufReader::new(body.into_reader()), hooks).map_err(|e| failed(e, false))
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
        if let Some(text) = reasoning_of(delta) {
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
    let (think, rest) = split_think(&content);
    if let Some(think) = think {
        reasoning.push_str(think);
        content = rest.to_owned();
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
                reasoning: reasoning_of(message).unwrap_or_default().to_owned(),
            });
    }
    match message["content"].as_str() {
        Some(content) => Ok(ModelTurn::Final(split_think(content).1.to_owned())),
        None => Err(format!("无法识别的模型响应: {response}").into()),
    }
}

/// 输出上限的参数名：OpenAI 已弃用 `max_tokens`（推理模型直接拒绝），改用
/// `max_completion_tokens`；DeepSeek、Ollama 等仍只认 `max_tokens`。按服务地址的主机名区分。
fn output_cap_field(base_url: &str) -> &'static str {
    let host = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest)
        .split(['/', ':'])
        .next()
        .unwrap_or_default();
    if host == "api.openai.com" {
        "max_completion_tokens"
    } else {
        "max_tokens"
    }
}

/// 思维链：DeepSeek 为 `reasoning_content`，Ollama 等为 `reasoning`。
fn reasoning_of(message: &Value) -> Option<&str> {
    message["reasoning_content"]
        .as_str()
        .or_else(|| message["reasoning"].as_str())
}

/// 有些服务把思维链以 `<think>…</think>` 写在正文开头：分出（思维链, 正文）。
fn split_think(content: &str) -> (Option<&str>, &str) {
    let trimmed = content.trim_start();
    trimmed
        .strip_prefix("<think>")
        .and_then(|rest| rest.split_once("</think>"))
        .map_or((None, content), |(think, rest)| {
            (Some(think.trim()), rest.trim_start())
        })
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

    /// 每个请求的 JSON 请求体、Authorization 头与 X-Client-Request-Id 头。
    type Requests = Arc<Mutex<Vec<(Value, String, String)>>>;

    fn mock_server(replies: Vec<(u16, String)>) -> (String, Requests) {
        mock_server_with(
            replies
                .into_iter()
                .map(|(s, r)| (s, r, String::new()))
                .collect(),
        )
    }

    /// 依次返回 `replies`（状态、正文、额外响应头），并记录收到的请求。
    fn mock_server_with(replies: Vec<(u16, String, String)>) -> (String, Requests) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        thread::spawn(move || {
            for (status, reply, extra) in replies {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let (mut length, mut auth, mut trace) = (0, String::new(), String::new());
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let lower = line.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    if lower.starts_with("x-client-request-id:") {
                        trace = line["x-client-request-id:".len()..].trim().to_owned();
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
                    .push((serde_json::from_slice(&body).unwrap(), auth, trace));
                let mut stream = reader.into_inner();
                // 以 `data:` 开头的回复按 SSE 流发送，其余按普通 JSON。
                let content_type = if reply.starts_with("data:") {
                    "text/event-stream"
                } else {
                    "application/json"
                };
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
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
        // 回奏不合体例会发回重写，那次请求打到已关闭的模拟服务上：不重试，照收原奏。
        let no_retry = Retry {
            attempts: 0,
            ..Retry::default()
        };
        let default = OpenAiCompatible::new(url, Some("sk-test".into()), "m").with_retry(no_retry);
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
            urgent: false,
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
        let (first, auth, _) = &seen[0];
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
        // 工具输出包在外来文书边界中，暗号与系统提示中的一致。
        let nonce = system
            .split("〔外来文书 ")
            .nth(1)
            .and_then(|rest| rest.split(' ').next())
            .unwrap();
        assert_eq!(
            messages[3]["content"],
            format!(
                "〔外来文书 {nonce} 起〕\nlib.rs:1: unsafe fn danger() {{}}\n〔外来文书 {nonce} 止〕"
            )
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// 记录流式文本；可在收到 `cancel_after` 个片段后报告已撤销。
    #[derive(Default)]
    struct Recorder {
        deltas: std::cell::RefCell<Vec<String>>,
        usage: std::cell::RefCell<Vec<TokenUsage>>,
        traces: std::cell::RefCell<Vec<String>>,
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
        fn model_trace(&self, trace: &str) {
            self.traces.borrow_mut().push(trace.into());
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

    /// 按 OpenAI Chat Completions 文档的流式格式回放（未经真实账号实测）：首块 `content: null`、
    /// 工具调用首块 `arguments` 为空串、`finish_reason: "tool_calls"`、末块 `choices` 为空只带 usage。
    #[test]
    fn openai_shaped_stream_is_understood() {
        let chunk = |choices: Value| {
            json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1,
                   "model": "gpt-5-mini", "system_fingerprint": null, "choices": choices})
        };
        let reply = sse(
            &[
                chunk(
                    json!([{"index": 0, "delta": {"role": "assistant", "content": null,
                    "tool_calls": [{"index": 0, "id": "call_1", "type": "function",
                    "function": {"name": "read_file", "arguments": ""}}], "refusal": null},
                    "logprobs": null, "finish_reason": null}]),
                ),
                chunk(json!([{"index": 0, "delta": {"tool_calls": [{"index": 0,
                    "function": {"arguments": "{\"path\":"}}]}, "finish_reason": null}])),
                chunk(json!([{"index": 0, "delta": {"tool_calls": [{"index": 0,
                    "function": {"arguments": "\"src/lib.rs\"}"}}]}, "finish_reason": null}])),
                chunk(json!([{"index": 0, "delta": {}, "finish_reason": "tool_calls"}])),
                {
                    let mut last = chunk(json!([]));
                    last["usage"] = json!({"prompt_tokens": 120, "completion_tokens": 40,
                        "total_tokens": 160, "prompt_tokens_details": {"cached_tokens": 0},
                        "completion_tokens_details": {"reasoning_tokens": 32}});
                    last
                },
            ],
            true,
        );
        let (url, seen) = mock_server(vec![(200, reply)]);
        let hooks = Recorder::default();
        let turn = OpenAiCompatible::new(url, Some("k".into()), "gpt-5-mini")
            .next_turn(&[Message::User("q".into())], &[], &hooks)
            .unwrap();
        let ModelTurn::Calls { calls, text, .. } = turn else {
            panic!("应为工具调用: {turn:?}");
        };
        assert_eq!((calls.len(), calls[0].id.as_str()), (1, "call_1"));
        assert_eq!(calls[0].args_json, r#"{"path":"src/lib.rs"}"#);
        assert!(text.is_empty());
        // 用量之前先报 trace ID，与请求头一致（事件簿据此把 trace 记在用量上）。
        assert_eq!(*hooks.traces.borrow(), [seen.lock().unwrap()[0].2.clone()]);
        // 推理 token 已含在 completion_tokens 内，不重复计。
        assert_eq!(
            *hooks.usage.borrow(),
            [TokenUsage {
                input: 120,
                output: 40
            }]
        );
    }

    #[test]
    fn ollama_reasoning_and_inline_think_stay_out_of_the_answer() {
        // Ollama 的思维链在 `reasoning`；另一些服务把 `<think>` 写在正文开头。
        let ollama = sse(
            &[
                delta(json!({"role": "assistant", "content": "", "reasoning": "想"})),
                delta(json!({"content": "", "reasoning": "一想"})),
                delta(json!({"content": "工具后"})),
                delta(
                    json!({"tool_calls": [{"index": 0, "id": "a", "type": "function",
                    "function": {"name": "read_file", "arguments": "{}"}}]}),
                ),
            ],
            true,
        );
        let inline = sse(
            &[delta(
                json!({"content": "<think>\n先想\n</think>\n\n7\n理由"}),
            )],
            true,
        );
        let (url, _) = mock_server(vec![(200, ollama), (200, inline)]);
        let model = OpenAiCompatible::new(url, None, "m");
        let ask = [Message::User("q".into())];
        let turn = model.next_turn(&ask, &[], &Recorder::default()).unwrap();
        assert!(
            matches!(&turn, ModelTurn::Calls { reasoning, text, .. } if reasoning == "想一想" && text == "工具后"),
            "{turn:?}"
        );
        let turn = model.next_turn(&ask, &[], &Recorder::default()).unwrap();
        assert!(
            matches!(&turn, ModelTurn::Final(t) if t == "7\n理由"),
            "{turn:?}"
        );
        assert_eq!(
            split_think("答：<think>不是开头</think>"),
            (None, "答：<think>不是开头</think>")
        );
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

    fn quick(url: String, attempts: u32) -> OpenAiCompatible {
        OpenAiCompatible::new(url, None, "m").with_retry(Retry {
            attempts,
            base: Duration::from_millis(10),
            max: Duration::from_millis(200),
        })
    }

    #[test]
    fn transient_failures_are_retried_with_the_same_trace_id() {
        let (url, seen) = mock_server_with(vec![
            (503, "busy".into(), String::new()),
            (429, "slow down".into(), "Retry-After: 0\r\n".into()),
            (200, final_reply("好"), String::new()),
        ]);
        let turn = quick(url, 3).next_turn(&[Message::User("hi".into())], &[], &());
        assert!(matches!(turn, Ok(ModelTurn::Final(t)) if t == "好"));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert!(
            seen[0].2.starts_with("zy-") && seen[0].2.len() == 15,
            "{}",
            seen[0].2
        );
        assert!(seen.iter().all(|r| r.2 == seen[0].2), "重试沿用同一 trace");
    }

    #[test]
    fn retries_give_up_with_trace_and_server_request_id() {
        let busy = || {
            (
                502,
                "bad gateway".to_owned(),
                "X-Request-Id: req-42\r\n".to_owned(),
            )
        };
        let (url, seen) = mock_server_with(vec![busy(), busy()]);
        let err = quick(url, 1)
            .next_turn(&[Message::User("hi".into())], &[], &())
            .unwrap_err()
            .to_string();
        let trace = seen.lock().unwrap()[1].2.clone();
        assert!(
            err.contains("502") && err.contains(&format!("trace {trace}")),
            "{err}"
        );
        assert!(
            err.contains("服务端 req-42") && err.contains("已重试 1 次"),
            "{err}"
        );
        // 4xx（429 除外）是请求本身的问题，不重试。
        let (url, seen) = mock_server(vec![(400, "bad request".into())]);
        assert!(
            quick(url, 3)
                .next_turn(&[Message::User("hi".into())], &[], &())
                .is_err()
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    struct Cap(u64);
    impl ExecutionHooks for Cap {
        fn max_output(&self) -> Option<u64> {
            Some(self.0)
        }
    }

    #[test]
    fn low_allowance_caps_max_tokens() {
        let messages = [Message::User("hi".into())];
        let (url, seen) = mock_server(vec![(200, final_reply("a")), (200, final_reply("b"))]);
        let model = quick(url, 0);
        model.next_turn(&messages, &[], &Cap(300)).unwrap();
        model.next_turn(&messages, &[], &Cap(1_000_000)).unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0["max_tokens"], 300);
        assert!(seen[1].0.get("max_tokens").is_none(), "余额充裕时不传");
        assert_eq!(
            output_cap_field("https://api.openai.com/v1"),
            "max_completion_tokens"
        );
        for other in [
            "https://api.deepseek.com/v1",
            "http://127.0.0.1:11434/v1",
            "https://x.dev/api.openai.com/v1",
        ] {
            assert_eq!(output_cap_field(other), "max_tokens", "{other}");
        }
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
