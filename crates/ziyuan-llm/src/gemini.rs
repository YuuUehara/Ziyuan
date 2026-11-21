//! 原生 Google Gemini API（`models/{model}:streamGenerateContent?alt=sse`）适配（实验性：当前仅经本地模拟服务验证）。
//!
//! 凭据经 `x-goog-api-key` 请求头传递（不拼入 URL 查询参数，避免日志或报错泄露）；
//! 支持 SSE 流式输出、`functionCall`／`functionResponse` 工具调用、思维链标记（`thought: true`）
//! 及 `usageMetadata.cachedContentTokenCount` 缓存命中统计。

use crate::{Failure, MAX_TOKENS_CAP, REQUEST_TIMEOUT, Retry};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::time::Duration;
use ziyuan_core::TokenUsage;
use ziyuan_runtime::agent_loop::{Message, ModelProvider, ModelTurn, ToolRequest, ToolSpec};
use ziyuan_runtime::{BoxError, ExecutionHooks, guard};

pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

pub struct GeminiNative {
    base_url: String,
    api_key: Option<String>,
    model: String,
    display_name: Option<String>,
    agent: ureq::Agent,
    retry: Retry,
}

impl GeminiNative {
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

    pub fn model(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.model)
    }

    fn endpoint(&self) -> String {
        let model = self.model.strip_prefix("models/").unwrap_or(&self.model);
        format!(
            "{}/models/{model}:streamGenerateContent?alt=sse",
            self.base_url
        )
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
            .post(self.endpoint())
            .header("Content-Type", "application/json")
            .header("X-Client-Request-Id", trace);
        if let Some(key) = &self.api_key {
            request = request.header("x-goog-api-key", key);
        }
        let response = request
            .send(body)
            .map_err(|e| fail(format!("Gemini API 请求失败: {e}").into(), true))?;
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
            reauth: None,
        };
        if !status.is_success() {
            let text = body.read_to_string().unwrap_or_default();
            let snippet: String = text.chars().take(500).collect();
            let retryable = status.as_u16() == 429 || status.is_server_error();
            return Err(failed(
                format!("Gemini API 返回 {status}: {snippet}").into(),
                retryable,
            ));
        }
        if !is_stream {
            let parsed = body
                .read_to_string()
                .map_err(BoxError::from)
                .and_then(|text| serde_json::from_str::<Value>(&text).map_err(BoxError::from));
            let response = parsed.map_err(|e| failed(e, false))?;
            return decode_gemini_chunks(std::slice::from_ref(&response), hooks)
                .map_err(|e| failed(e, false));
        }
        read_gemini_stream(BufReader::new(body.into_reader()), hooks).map_err(|e| failed(e, false))
    }
}

impl ModelProvider for GeminiNative {
    fn model_name(&self) -> Option<&str> {
        Some(self.model())
    }

    fn next_turn(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
        hooks: &dyn ExecutionHooks,
    ) -> Result<ModelTurn, BoxError> {
        let max_output = hooks
            .max_output()
            .filter(|&room| room < MAX_TOKENS_CAP)
            .map(|room| room.max(1));
        let body = build_request_body(messages, tools, max_output)?.to_string();
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

fn push_part(contents: &mut Vec<Value>, role: &str, part: Value) {
    if let Some(last) = contents.last_mut()
        && last["role"] == role
        && let Some(arr) = last["parts"].as_array_mut()
    {
        arr.push(part);
        return;
    }
    contents.push(json!({
        "role": role,
        "parts": [part],
    }));
}

fn strip_unsupported_schema_keys(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("$schema");
            map.remove("additionalProperties");
            for v in map.values_mut() {
                strip_unsupported_schema_keys(v);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                strip_unsupported_schema_keys(v);
            }
        }
        _ => {}
    }
}

pub(crate) fn build_request_body(
    messages: &[Message],
    tools: &[ToolSpec],
    max_output: Option<u64>,
) -> Result<Value, BoxError> {
    let mut system_parts: Vec<Value> = Vec::new();
    let mut contents: Vec<Value> = Vec::new();
    let mut call_names: HashMap<String, String> = HashMap::new();

    for message in messages {
        match message {
            Message::System(text) => {
                if !text.is_empty() {
                    system_parts.push(json!({ "text": text }));
                }
            }
            Message::User(text) => {
                push_part(&mut contents, "user", json!({ "text": text }));
            }
            Message::Assistant(text) => {
                push_part(&mut contents, "model", json!({ "text": text }));
            }
            Message::CallPrelude { text, .. } => {
                if !text.is_empty() {
                    push_part(&mut contents, "model", json!({ "text": text }));
                }
            }
            Message::ToolCall {
                id,
                name,
                args_json,
            } => {
                call_names.insert(id.clone(), name.clone());
                let args = match serde_json::from_str::<Value>(args_json.trim()) {
                    Ok(Value::Object(map)) => Value::Object(map),
                    _ => Value::Object(Map::new()),
                };
                push_part(
                    &mut contents,
                    "model",
                    json!({
                        "functionCall": {
                            "name": name,
                            "args": args,
                        }
                    }),
                );
            }
            Message::ToolOutput { call_id, output } => {
                let name = call_names
                    .get(call_id)
                    .cloned()
                    .unwrap_or_else(|| "tool".into());
                push_part(
                    &mut contents,
                    "user",
                    json!({
                        "functionResponse": {
                            "name": name,
                            "response": { "output": output },
                        }
                    }),
                );
            }
        }
    }

    let mut body = json!({
        "contents": contents,
    });
    if !system_parts.is_empty() {
        body["systemInstruction"] = json!({ "parts": system_parts });
    }
    if !tools.is_empty() {
        let decls: Vec<Value> = tools
            .iter()
            .map(|tool| {
                let mut parameters: Value = serde_json::from_str(&tool.input_schema_json)?;
                strip_unsupported_schema_keys(&mut parameters);
                Ok(json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": parameters,
                }))
            })
            .collect::<Result<_, BoxError>>()?;
        body["tools"] = json!([{ "functionDeclarations": decls }]);
    }
    if let Some(max_tokens) = max_output {
        body["generationConfig"] = json!({ "maxOutputTokens": max_tokens });
    }
    Ok(body)
}

fn read_gemini_stream(
    reader: impl BufRead,
    hooks: &dyn ExecutionHooks,
) -> Result<ModelTurn, BoxError> {
    let mut chunks = Vec::new();
    for line in reader.lines() {
        if hooks.is_cancelled() {
            return Err("已撤销，中止流式响应".into());
        }
        let line = line?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let chunk: Value = serde_json::from_str(data)?;
        if let Some(error) = chunk.get("error") {
            return Err(format!("Gemini 流式响应返回错误: {error}").into());
        }
        chunks.push(chunk);
    }
    if chunks.is_empty() {
        return Err("Gemini 流式响应未返回任何有效数据块".into());
    }
    decode_gemini_chunks(&chunks, hooks)
}

fn decode_gemini_chunks(
    chunks: &[Value],
    hooks: &dyn ExecutionHooks,
) -> Result<ModelTurn, BoxError> {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut calls: Vec<ToolRequest> = Vec::new();
    let mut last_usage: Option<TokenUsage> = None;

    for chunk in chunks {
        if let Some(error) = chunk.get("error") {
            return Err(format!("Gemini API 返回错误: {error}").into());
        }
        if let Some(meta) = chunk.get("usageMetadata") {
            let input = meta["promptTokenCount"].as_u64();
            let output = meta["candidatesTokenCount"].as_u64().or_else(|| {
                let total = meta["totalTokenCount"].as_u64()?;
                let prompt = input?;
                Some(total.saturating_sub(prompt))
            });
            let cache_read = meta["cachedContentTokenCount"].as_u64().unwrap_or(0);
            if let (Some(input), Some(output)) = (input, output) {
                last_usage = Some(TokenUsage::with_cache(input, output, cache_read, 0));
            }
        }
        let parts = chunk
            .pointer("/candidates/0/content/parts")
            .and_then(Value::as_array);
        for part in parts.into_iter().flatten() {
            let is_thought = part["thought"].as_bool().unwrap_or(false);
            if let Some(text) = part["text"].as_str().filter(|s| !s.is_empty()) {
                if is_thought {
                    reasoning.push_str(text);
                } else {
                    hooks.text_delta(text);
                    content.push_str(text);
                }
            }
            if let Some(fc) = part.get("functionCall").and_then(Value::as_object) {
                let name = fc
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("Gemini functionCall 缺少 name")?
                    .to_owned();
                let id = fc
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("gemini-call-{}", calls.len()));
                let args_json = fc.get("args").map_or_else(|| "{}".into(), Value::to_string);
                calls.push(ToolRequest {
                    id,
                    name,
                    args_json,
                });
            }
        }
    }

    if let Some(usage) = last_usage {
        hooks.model_usage(usage);
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
