//! 原生 Anthropic Messages API（`/v1/messages`）适配（实验性：当前仅经本地模拟服务验证）。
//!
//! 支持 SSE 流式输出、Tool Calling（`tool_use` / `tool_result`）、
//! Prompt Caching（`cache_control: {"type": "ephemeral"}` 至多 3 处断点）
//! 与 `cache_read_input_tokens` / `cache_creation_input_tokens` 缓存用量精确统计。

use crate::{Failure, MAX_TOKENS_CAP, PromptCache, REQUEST_TIMEOUT, Retry};
use serde_json::{Map, Value, json};
use std::io::{BufRead, BufReader};
use std::time::Duration;
use ziyuan_core::TokenUsage;
use ziyuan_runtime::agent_loop::{Message, ModelProvider, ModelTurn, ToolRequest, ToolSpec};
use ziyuan_runtime::{BoxError, ExecutionHooks, guard};

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u64 = MAX_TOKENS_CAP;

pub struct AnthropicMessages {
    base_url: String,
    api_key: Option<String>,
    model: String,
    /// 可选对外展示名（例如 `.ziyuan/models.json` 中的配置别名）。
    display_name: Option<String>,
    agent: ureq::Agent,
    retry: Retry,
    prompt_cache: PromptCache,
}

impl AnthropicMessages {
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
            display_name: None,
            agent,
            retry: Retry::default(),
            prompt_cache: PromptCache::Auto,
        }
    }

    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    pub fn with_retry(mut self, retry: Retry) -> Self {
        self.retry = retry;
        self
    }

    pub fn with_prompt_cache(mut self, prompt_cache: PromptCache) -> Self {
        self.prompt_cache = prompt_cache;
        self
    }

    pub fn model(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.model)
    }

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
            reauth: None,
        };
        let mut request = self
            .agent
            .post(format!("{}/messages", self.base_url))
            .header("Content-Type", "application/json")
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("X-Client-Request-Id", trace);
        if let Some(key) = &self.api_key {
            request = request.header("x-api-key", key);
        }
        let response = request
            .send(body)
            .map_err(|e| fail(format!("Anthropic API 请求失败: {e}").into(), true))?;
        let status = response.status();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let request_id = header("request-id").or_else(|| header("x-request-id"));
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
            reauth: None,
        };
        if !status.is_success() {
            let text = body.read_to_string().unwrap_or_default();
            let snippet: String = text.chars().take(500).collect();
            let code = status.as_u16();
            let retryable = code == 429 || code == 529 || status.is_server_error();
            return Err(failed(
                format!("Anthropic API 返回 {status}: {snippet}").into(),
                retryable,
            ));
        }
        if !is_stream {
            let parsed = body
                .read_to_string()
                .map_err(BoxError::from)
                .and_then(|text| serde_json::from_str::<Value>(&text).map_err(BoxError::from));
            let response = parsed.map_err(|e| failed(e, false))?;
            report_anthropic_usage(&response["usage"], hooks);
            let turn = decode_anthropic_response(&response).map_err(|e| failed(e, false))?;
            if let ModelTurn::Final(text) = &turn {
                hooks.text_delta(text);
            }
            return Ok(turn);
        }
        read_anthropic_stream(BufReader::new(body.into_reader()), hooks)
            .map_err(|e| failed(e, false))
    }
}

impl ModelProvider for AnthropicMessages {
    fn model_name(&self) -> Option<&str> {
        Some(self.model())
    }

    fn next_turn(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
        hooks: &dyn ExecutionHooks,
    ) -> Result<ModelTurn, BoxError> {
        let cache_enabled = self.prompt_cache != PromptCache::Off;
        let max_tokens = hooks
            .max_output()
            .filter(|&room| room < DEFAULT_MAX_TOKENS)
            .map_or(DEFAULT_MAX_TOKENS, |room| room.max(1));
        let body = build_request_body(&self.model, messages, tools, max_tokens, cache_enabled)?
            .to_string();
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

fn push_role_block(out: &mut Vec<Value>, role: &str, block: Value) {
    if let Some(last) = out.last_mut()
        && last["role"] == role
        && let Some(arr) = last["content"].as_array_mut()
    {
        arr.push(block);
        return;
    }
    out.push(json!({
        "role": role,
        "content": [block],
    }));
}

fn parse_tool_input(args_json: &str) -> Value {
    let trimmed = args_json.trim();
    if trimmed.is_empty() {
        return Value::Object(Map::new());
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Object(map)) => Value::Object(map),
        _ => Value::Object(Map::new()),
    }
}

pub(crate) fn build_request_body(
    model: &str,
    messages: &[Message],
    tools: &[ToolSpec],
    max_tokens: u64,
    cache_enabled: bool,
) -> Result<Value, BoxError> {
    let mut system_blocks: Vec<Value> = Vec::new();
    let mut encoded_messages: Vec<Value> = Vec::new();

    for message in messages {
        match message {
            Message::System(text) => {
                if !text.is_empty() {
                    system_blocks.push(json!({ "type": "text", "text": text }));
                }
            }
            Message::User(text) => {
                push_role_block(
                    &mut encoded_messages,
                    "user",
                    json!({ "type": "text", "text": text }),
                );
            }
            Message::Assistant(text) => {
                push_role_block(
                    &mut encoded_messages,
                    "assistant",
                    json!({ "type": "text", "text": text }),
                );
            }
            Message::CallPrelude { text, .. } => {
                if !text.is_empty() {
                    push_role_block(
                        &mut encoded_messages,
                        "assistant",
                        json!({ "type": "text", "text": text }),
                    );
                }
            }
            Message::ToolCall {
                id,
                name,
                args_json,
            } => {
                push_role_block(
                    &mut encoded_messages,
                    "assistant",
                    json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": parse_tool_input(args_json),
                    }),
                );
            }
            Message::ToolOutput { call_id, output } => {
                push_role_block(
                    &mut encoded_messages,
                    "user",
                    json!({
                        "type": "tool_result",
                        "tool_use_id": call_id,
                        "content": output,
                    }),
                );
            }
        }
    }

    let mut encoded_tools: Vec<Value> = tools
        .iter()
        .map(|tool| {
            let input_schema: Value = serde_json::from_str(&tool.input_schema_json)?;
            Ok(json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": input_schema,
            }))
        })
        .collect::<Result<_, BoxError>>()?;

    if cache_enabled {
        // 断点 1：system 末尾块。
        if let Some(last_sys) = system_blocks.last_mut().and_then(Value::as_object_mut) {
            last_sys.insert("cache_control".into(), json!({ "type": "ephemeral" }));
        }
        // 断点 2：工具定义数组末项。
        if let Some(last_tool) = encoded_tools.last_mut().and_then(Value::as_object_mut) {
            last_tool.insert("cache_control".into(), json!({ "type": "ephemeral" }));
        }
        // 断点 3：多轮历史倒数第二条消息末尾块（总计至多 3 处，低于 4 处上限）。
        if encoded_messages.len() >= 2 {
            let prev_idx = encoded_messages.len() - 2;
            if let Some(last_block) = encoded_messages[prev_idx]["content"]
                .as_array_mut()
                .and_then(|arr| arr.last_mut())
                .and_then(Value::as_object_mut)
            {
                last_block.insert("cache_control".into(), json!({ "type": "ephemeral" }));
            }
        }
    }

    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": encoded_messages,
        "stream": true,
    });
    if !system_blocks.is_empty() {
        body["system"] = Value::Array(system_blocks);
    }
    if !encoded_tools.is_empty() {
        body["tools"] = Value::Array(encoded_tools);
    }
    Ok(body)
}

#[derive(Default)]
struct UsageAcc {
    uncached_input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    seen: bool,
}

impl UsageAcc {
    fn absorb(&mut self, usage: &Value) {
        let Some(obj) = usage.as_object() else {
            return;
        };
        if let Some(v) = obj.get("input_tokens").and_then(Value::as_u64) {
            self.uncached_input = v;
            self.seen = true;
        }
        if let Some(v) = obj.get("output_tokens").and_then(Value::as_u64) {
            self.output = v;
            self.seen = true;
        }
        if let Some(v) = obj.get("cache_read_input_tokens").and_then(Value::as_u64) {
            self.cache_read = v;
            self.seen = true;
        }
        if let Some(v) = obj
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
        {
            self.cache_write = v;
            self.seen = true;
        }
    }

    fn report(&self, hooks: &dyn ExecutionHooks) {
        if !self.seen {
            return;
        }
        let total_input = self
            .uncached_input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write);
        hooks.model_usage(TokenUsage::with_cache(
            total_input,
            self.output,
            self.cache_read,
            self.cache_write,
        ));
    }
}

fn report_anthropic_usage(usage: &Value, hooks: &dyn ExecutionHooks) {
    let mut acc = UsageAcc::default();
    acc.absorb(usage);
    acc.report(hooks);
}

enum StreamBlock {
    Text,
    Thinking,
    ToolUse {
        id: String,
        name: String,
        args_json: String,
        initial_input: Option<Value>,
    },
    Other,
}

fn read_anthropic_stream(
    reader: impl BufRead,
    hooks: &dyn ExecutionHooks,
) -> Result<ModelTurn, BoxError> {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut blocks: Vec<Option<StreamBlock>> = Vec::new();
    let mut usage = UsageAcc::default();
    let mut done = false;

    for line in reader.lines() {
        if hooks.is_cancelled() {
            return Err("已撤销，中止流式响应".into());
        }
        let line = line?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data.is_empty() {
            continue;
        }
        let event: Value = serde_json::from_str(data)?;
        let kind = event["type"].as_str().unwrap_or_default();
        match kind {
            "message_start" => {
                usage.absorb(&event["message"]["usage"]);
            }
            "content_block_start" => {
                let idx = event["index"]
                    .as_u64()
                    .ok_or("Anthropic content_block_start 缺少 index")?
                    as usize;
                if blocks.len() <= idx {
                    blocks.resize_with(idx + 1, || None);
                }
                let cb = &event["content_block"];
                let block = match cb["type"].as_str().unwrap_or_default() {
                    "text" => {
                        if let Some(t) = cb["text"].as_str().filter(|s| !s.is_empty()) {
                            hooks.text_delta(t);
                            content.push_str(t);
                        }
                        StreamBlock::Text
                    }
                    "thinking" => {
                        if let Some(t) = cb["thinking"].as_str().filter(|s| !s.is_empty()) {
                            reasoning.push_str(t);
                        }
                        StreamBlock::Thinking
                    }
                    "tool_use" => {
                        let id = cb["id"]
                            .as_str()
                            .ok_or("Anthropic tool_use 缺少 id")?
                            .to_owned();
                        let name = cb["name"]
                            .as_str()
                            .ok_or("Anthropic tool_use 缺少 name")?
                            .to_owned();
                        let initial_input = cb
                            .get("input")
                            .filter(|v| v.as_object().is_some_and(|m| !m.is_empty()))
                            .cloned();
                        StreamBlock::ToolUse {
                            id,
                            name,
                            args_json: String::new(),
                            initial_input,
                        }
                    }
                    _ => StreamBlock::Other,
                };
                blocks[idx] = Some(block);
            }
            "content_block_delta" => {
                let idx = event["index"]
                    .as_u64()
                    .ok_or("Anthropic content_block_delta 缺少 index")?
                    as usize;
                let delta = &event["delta"];
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        if let Some(t) = delta["text"].as_str().filter(|s| !s.is_empty()) {
                            hooks.text_delta(t);
                            content.push_str(t);
                        }
                    }
                    "thinking_delta" => {
                        if let Some(t) = delta["thinking"].as_str().filter(|s| !s.is_empty()) {
                            reasoning.push_str(t);
                        }
                    }
                    "input_json_delta" => {
                        if let Some(partial) = delta["partial_json"].as_str()
                            && let Some(Some(StreamBlock::ToolUse { args_json, .. })) =
                                blocks.get_mut(idx)
                        {
                            args_json.push_str(partial);
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                usage.absorb(&event["usage"]);
            }
            "message_stop" => {
                done = true;
                break;
            }
            "error" => {
                return Err(format!("Anthropic 流式响应返回错误: {}", event["error"]).into());
            }
            _ => {}
        }
    }

    if !done {
        return Err("Anthropic 流式响应在 message_stop 之前中断".into());
    }
    usage.report(hooks);

    let mut calls = Vec::new();
    for block in blocks.into_iter().flatten() {
        if let StreamBlock::ToolUse {
            id,
            name,
            args_json,
            initial_input,
        } = block
        {
            let final_args = if !args_json.trim().is_empty() {
                args_json
            } else if let Some(val) = initial_input {
                val.to_string()
            } else {
                "{}".into()
            };
            calls.push(ToolRequest {
                id,
                name,
                args_json: final_args,
            });
        }
    }
    if calls.is_empty() {
        return Ok(ModelTurn::Final(content));
    }
    if !content.is_empty() && !content.ends_with('\n') {
        hooks.text_delta("\n");
    }
    Ok(ModelTurn::Calls {
        calls,
        text: content,
        reasoning,
    })
}

fn decode_anthropic_response(response: &Value) -> Result<ModelTurn, BoxError> {
    let blocks = response["content"]
        .as_array()
        .ok_or_else(|| format!("无法识别的 Anthropic 响应: {response}"))?;
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut calls = Vec::new();
    for block in blocks {
        match block["type"].as_str().unwrap_or_default() {
            "text" => {
                if let Some(t) = block["text"].as_str() {
                    content.push_str(t);
                }
            }
            "thinking" => {
                if let Some(t) = block["thinking"].as_str() {
                    reasoning.push_str(t);
                }
            }
            "tool_use" => {
                let id = block["id"]
                    .as_str()
                    .ok_or("Anthropic tool_use 缺少 id")?
                    .to_owned();
                let name = block["name"]
                    .as_str()
                    .ok_or("Anthropic tool_use 缺少 name")?
                    .to_owned();
                let args_json = block
                    .get("input")
                    .map_or_else(|| "{}".into(), Value::to_string);
                calls.push(ToolRequest {
                    id,
                    name,
                    args_json,
                });
            }
            _ => {}
        }
    }
    if calls.is_empty() {
        Ok(ModelTurn::Final(content))
    } else {
        Ok(ModelTurn::Calls {
            calls,
            text: content,
            reasoning,
        })
    }
}
