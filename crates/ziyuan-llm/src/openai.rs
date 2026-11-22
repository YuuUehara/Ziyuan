//! OpenAI 兼容的 Chat Completions（及 ChatGPT 订阅 Responses API 转发）实现。
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::env;
use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::time::Duration;
use ziyuan_core::TokenUsage;
use ziyuan_runtime::agent_loop::{Message, ModelProvider, ModelTurn, ToolRequest, ToolSpec};
use ziyuan_runtime::{BoxError, ExecutionHooks, guard};

use crate::{
    Auth, Catalog, DEFAULT_BASE_URL, Failure, MAX_TOKENS_CAP, PromptCache, ProviderKind,
    REQUEST_TIMEOUT, Retry, chatgpt, responses,
};

pub struct OpenAiCompatible {
    base_url: String,
    pub(crate) auth: Auth,
    model: String,
    display_name: Option<String>,
    agent: ureq::Agent,
    retry: Retry,
    prompt_cache: PromptCache,
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
            auth: Auth::Key(api_key),
            model: model.into(),
            display_name: None,
            agent,
            retry: Retry::default(),
            prompt_cache: PromptCache::Auto,
        }
    }

    /// 用 ChatGPT 订阅额度调用 `base_url`（通常为 [`chatgpt::API_BASE`]）的 Responses API。
    pub fn chatgpt_plan(
        account: Arc<chatgpt::Account>,
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let mut this = Self::new(base_url, None, model);
        this.auth = Auth::Plan(account);
        this
    }

    /// 用的是 ChatGPT 订阅额度。
    pub fn uses_chatgpt_plan(&self) -> bool {
        matches!(self.auth, Auth::Plan(_))
    }

    pub fn with_retry(mut self, retry: Retry) -> Self {
        self.retry = retry;
        self
    }

    pub fn with_prompt_cache(mut self, prompt_cache: PromptCache) -> Self {
        self.prompt_cache = prompt_cache;
        self
    }

    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    /// 读取 `ZIYUAN_LLM_MODEL`（必需）、`ZIYUAN_LLM_BASE_URL`、`ZIYUAN_LLM_API_KEY`；
    /// `ZIYUAN_LLM_PROVIDER=chatgpt` 改用 `ziyuan login` 登录的 ChatGPT 订阅（不需要 Key）。
    /// 未设置模型时返回 `None`；选了订阅却未登录、未授权时报错。
    pub fn from_env() -> Result<Option<Self>, BoxError> {
        let Some(model) = env::var("ZIYUAN_LLM_MODEL").ok().filter(|m| !m.is_empty()) else {
            return Ok(None);
        };
        let base_url = env::var("ZIYUAN_LLM_BASE_URL")
            .ok()
            .filter(|u| !u.is_empty());
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
        let this = match env::var("ZIYUAN_LLM_PROVIDER").unwrap_or_default().as_str() {
            "" | "api" | "openai" => Self::new(
                base_url.unwrap_or_else(|| DEFAULT_BASE_URL.into()),
                api_key,
                model,
            ),
            "chatgpt" => {
                let account = chatgpt::Account::from_env()?;
                if !account.load()?.is_some_and(|c| c.plan_enabled()) {
                    return Err("ZIYUAN_LLM_PROVIDER=chatgpt：尚未登录 ChatGPT 订阅，请先执行 `ziyuan login`".into());
                }
                Self::chatgpt_plan(
                    Arc::new(account),
                    base_url.unwrap_or_else(|| chatgpt::API_BASE.into()),
                    model,
                )
            }
            other => {
                return Err(
                    format!("ZIYUAN_LLM_PROVIDER 只能是 api 或 chatgpt，而不是 {other}").into(),
                );
            }
        };
        let prompt_cache = PromptCache::from_env()?;
        Ok(Some(this.with_retry(retry).with_prompt_cache(prompt_cache)))
    }

    pub fn model(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.model)
    }

    /// 同一服务地址与 Key 下的模型目录，供按官职切换模型。
    pub fn catalog(&self) -> Catalog {
        let default_kind = match self.auth {
            Auth::Key(_) => ProviderKind::OpenAi,
            Auth::Plan(_) => ProviderKind::ChatGpt,
        };
        Catalog {
            default_kind,
            base_url: self.base_url.clone(),
            auth: self.auth.clone(),
            retry: self.retry,
            prompt_cache: self.prompt_cache,
            profiles: BTreeMap::new(),
        }
    }
}

impl ModelProvider for OpenAiCompatible {
    fn model_name(&self) -> Option<&str> {
        Some(self.model())
    }

    /// 每次调用带一个 trace ID（请求头 `X-Client-Request-Id`，重试沿用），出错时连同服务端的
    /// 请求 ID 写进错误信息，便于对照服务商的日志。
    fn next_turn(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
        hooks: &dyn ExecutionHooks,
    ) -> Result<ModelTurn, BoxError> {
        let body = match self.auth {
            Auth::Key(_) => {
                let mut encoded_messages = encode_messages(messages);
                let mut encoded_tools = if tools.is_empty() {
                    None
                } else {
                    Some(encode_tools(tools)?)
                };
                if self
                    .prompt_cache
                    .uses_anthropic_cache_control(&self.base_url, &self.model)
                {
                    apply_anthropic_cache_control(&mut encoded_messages, encoded_tools.as_mut());
                }
                let mut body = json!({
                    "model": self.model,
                    "messages": encoded_messages,
                    "stream": true,
                    // 让流的最后一块携带 usage；不支持的服务会忽略。
                    "stream_options": {"include_usage": true},
                });
                if let Some(encoded_tools) = encoded_tools {
                    body["tools"] = encoded_tools;
                }
                // 度支将尽：输出至多用完余额。
                if let Some(room) = hooks.max_output().filter(|&room| room < MAX_TOKENS_CAP) {
                    body[output_cap_field(&self.base_url)] = json!(room.max(1));
                }
                body
            }
            // 订阅额度不收输出上限：度支只能在调用前核算（agent_loop 的余量检查）与事后记账。
            Auth::Plan(_) => responses::body(&self.model, messages, tools)?,
        }
        .to_string();
        let trace = format!("zy-{}", &guard::nonce()[..12]);
        hooks.model_trace(&trace);
        let mut retry = 0;
        let mut reauthed = false;
        loop {
            let failure = match self.attempt(&body, &trace, hooks) {
                Ok(turn) => return Ok(turn),
                Err(failure) => failure,
            };
            // 订阅访问令牌被拒：续期后再试一次，不计入重试。
            if let (Auth::Plan(account), Some(stale), false) =
                (&self.auth, &failure.reauth, reauthed)
            {
                reauthed = true;
                account
                    .refresh(Some(stale.as_str()))
                    .map_err(|e| format!("{e}（trace {trace}）"))?;
                continue;
            }
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
            reauth: None,
        };
        let (path, token) = match &self.auth {
            Auth::Key(key) => ("chat/completions", key.clone()),
            Auth::Plan(account) => (
                "responses",
                Some(account.access_token().map_err(|e| fail(e, false))?),
            ),
        };
        let mut request = self
            .agent
            .post(format!("{}/{path}", self.base_url))
            .header("Content-Type", "application/json")
            .header("X-Client-Request-Id", trace);
        if let Some(token) = &token {
            request = request.header("Authorization", format!("Bearer {token}"));
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
            reauth: None,
        };
        if !status.is_success() {
            let text = body.read_to_string().unwrap_or_default();
            let snippet: String = text.chars().take(500).collect();
            let mut retryable = status.as_u16() == 429 || status.is_server_error();
            let mut error = format!("模型 API 返回 {status}: {snippet}");
            if let Auth::Plan(_) = self.auth {
                let (plan_retryable, note) = responses::plan_error(status.as_u16(), &text);
                retryable = plan_retryable;
                if let Some(note) = note {
                    error = format!("{note}（{status}）");
                }
                if status.as_u16() == 401 {
                    return Err(Failure {
                        reauth: token,
                        ..failed(error.into(), false)
                    });
                }
            }
            return Err(failed(error.into(), retryable));
        }
        // 以下已开始接收答复（可能已流式显示了文字），出错不再重试。
        if let Auth::Plan(_) = self.auth {
            return responses::read_events(BufReader::new(body.into_reader()), hooks)
                .map_err(|e| failed(e, false));
        }
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
pub(crate) fn read_stream(
    reader: impl BufRead,
    hooks: &dyn ExecutionHooks,
) -> Result<ModelTurn, BoxError> {
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

pub(crate) fn encode_tools(tools: &[ToolSpec]) -> Result<Value, BoxError> {
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
pub(crate) fn encode_messages(messages: &[Message]) -> Vec<Value> {
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

/// 为 OpenAI 兼容的 Anthropic / OpenRouter 请求注入 `cache_control: {"type": "ephemeral"}` 断点：
/// 1) system prompt；2) 工具定义数组末项；3) 多轮对话中倒数第二条之前最近的可缓存文本消息。
///
/// 至多 3 个断点（低于 Anthropic 4 断点上限）。
pub(crate) fn apply_anthropic_cache_control(messages: &mut [Value], tools: Option<&mut Value>) {
    let mark_content_block = |msg: &mut Value| -> bool {
        let Some(text) = msg.get("content").and_then(Value::as_str) else {
            return false;
        };
        if text.is_empty() {
            return false;
        }
        let text = text.to_owned();
        msg["content"] = json!([{
            "type": "text",
            "text": text,
            "cache_control": { "type": "ephemeral" }
        }]);
        true
    };

    // 断点 1：system prompt（最多标首个非空 system，避免多条 system 耗尽断点额度）。
    for msg in messages.iter_mut() {
        if msg["role"] == "system" && mark_content_block(msg) {
            break;
        }
    }

    // 断点 2：工具定义末尾（标最后一件工具即覆盖全部工具 schema）。
    if let Some(tools) = tools
        && let Some(last_tool) = tools.as_array_mut().and_then(|arr| arr.last_mut())
        && let Some(obj) = last_tool.as_object_mut()
    {
        obj.insert("cache_control".into(), json!({ "type": "ephemeral" }));
    }

    // 断点 3：多轮历史边界（跳过末尾最新一条，给前序最近一条非 system 文本消息加缓存点）。
    let non_system: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter_map(|(i, m)| (m["role"] != "system").then_some(i))
        .collect();
    if non_system.len() >= 2 {
        for &idx in non_system[..non_system.len() - 1].iter().rev() {
            if mark_content_block(&mut messages[idx]) {
                break;
            }
        }
    }
}

/// 解析 OpenAI / DeepSeek / Anthropic 兼容端点的 `usage` 字段（流式时位于最后一块）。
pub(crate) fn report_usage(value: &Value, hooks: &dyn ExecutionHooks) {
    let usage = &value["usage"];
    let cache_read = usage["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .or_else(|| usage["prompt_cache_hit_tokens"].as_u64())
        .or_else(|| usage["cache_read_input_tokens"].as_u64())
        .unwrap_or(0);
    let cache_write = usage["cache_creation_input_tokens"]
        .as_u64()
        .or_else(|| usage["prompt_tokens_details"]["cache_write_tokens"].as_u64())
        .unwrap_or(0);

    let (input, output) = match (
        usage["prompt_tokens"].as_u64(),
        usage["completion_tokens"].as_u64(),
    ) {
        (Some(input), Some(output)) => (input, output),
        _ => {
            // 兼容 Anthropic 风格字段名：其 `input_tokens` 不含缓存读写部分，统一折算进总输入。
            let (Some(uncached_input), Some(output)) = (
                usage["input_tokens"].as_u64(),
                usage["output_tokens"].as_u64(),
            ) else {
                return;
            };
            (
                uncached_input
                    .saturating_add(cache_read)
                    .saturating_add(cache_write),
                output,
            )
        }
    };
    hooks.model_usage(TokenUsage::with_cache(
        input,
        output,
        cache_read,
        cache_write,
    ));
}

pub(crate) fn decode_turn(response: &Value) -> Result<ModelTurn, BoxError> {
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
pub(crate) fn output_cap_field(base_url: &str) -> &'static str {
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
pub(crate) fn reasoning_of(message: &Value) -> Option<&str> {
    message["reasoning_content"]
        .as_str()
        .or_else(|| message["reasoning"].as_str())
}

/// 有些服务把思维链以 `<think>…</think>` 写在正文开头：分出（思维链, 正文）。
pub(crate) fn split_think(content: &str) -> (Option<&str>, &str) {
    let trimmed = content.trim_start();
    trimmed
        .strip_prefix("<think>")
        .and_then(|rest| rest.split_once("</think>"))
        .map_or((None, content), |(think, rest)| {
            (Some(think.trim()), rest.trim_start())
        })
}
