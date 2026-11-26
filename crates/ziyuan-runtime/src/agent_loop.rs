//! 独立于治理层的最小 Tool Calling Loop；模型 Adapter 见 `ziyuan-llm`。
use crate::{BoxError, ExecutionHooks, guard};
use serde_json::Value;
use std::cell::Cell;
use std::fmt;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Instant;
use ziyuan_core::{FileChange, Threat, TokenUsage};

/// 对话估算超过此数（tokens）时节略较早的工具输出。
pub const CONTEXT_TOKENS: u64 = 48_000;
mod compaction;

pub use compaction::{abridge, compact_turns, estimate_tokens};
use compaction::{compact_reads, compact_turns_inner};

/// 二阶段历史压缩摘要前的固定系统说明，保持与后续 User 摘要的严格角色交替。
pub(crate) const COMPACT_ACK: &str =
    "〔系统注：以下为较早工作轮次的结构化压缩摘要，原文已归档以节省上下文窗口。〕";
/// 度支余额扣去预计输入后不足此数，就不再发请求（只够几个字的回答没有意义）。
const MIN_OUTPUT: u64 = 64;
/// 每次调用为输出预留的上限。Adapter 只在输出上限低于 8192 时才向 API 传 `max_tokens`，
/// 模型写得更长时超出部分照实结算（度支是软上限）。
const RESERVED_OUTPUT: u64 = 8192;

#[derive(Debug)]
pub enum LoopError {
    Model(BoxError),
    EmptyToolCalls,
    UnauthorizedTool(String),
    StepLimit(usize),
    Cancelled,
    /// 调用前估算本轮输入已超出度支余额：不发请求。
    Budget {
        estimate: u64,
        allowance: u64,
    },
}
impl fmt::Display for LoopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model(e) => write!(f, "模型调用失败: {e}"),
            Self::EmptyToolCalls => f.write_str("模型返回空工具调用"),
            Self::UnauthorizedTool(name) => write!(f, "未授权的工具: {name}"),
            Self::StepLimit(n) => write!(f, "超出最大模型调用步数: {n}"),
            Self::Cancelled => f.write_str("任务已被撤销或超时"),
            Self::Budget {
                estimate,
                allowance,
            } => write!(
                f,
                "度支将尽：本轮输入预计约 {estimate} tokens，余额 {allowance}，户部停支，未发请求"
            ),
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
    /// 一轮工具调用之前模型输出的文本与思维链；其后的 `ToolCall` 属于同一条 assistant 消息。
    /// 思考模式下（如 DeepSeek）思维链须原样回传，否则 API 拒绝请求。
    CallPrelude {
        text: String,
        reasoning: String,
    },
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
    Calls {
        calls: Vec<ToolRequest>,
        /// 调用前模型输出的文本（可为空）。
        text: String,
        /// 思维链（`reasoning_content`，可为空）。
        reasoning: String,
    },
}

pub trait ModelProvider: Send + Sync {
    /// 实现可以通过 `hooks.text_delta` 流式报告生成的文本，并应在 `hooks.is_cancelled()`
    /// 为真时尽快中止请求。
    fn next_turn(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
        hooks: &dyn ExecutionHooks,
    ) -> Result<ModelTurn, BoxError>;

    /// 模型名，供界面显示。
    fn model_name(&self) -> Option<&str> {
        None
    }
}
/// 单轮内连续只读工具调用的最大并发线程数，避免大批量调用撑爆线程或文件句柄。
const MAX_PARALLEL_READS: usize = 8;

pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn execute(&self, args_json: &str) -> Result<String, BoxError>;

    /// 支持协作式撤销的执行入口；长耗时工具（如 `run_tests`）可覆写此方法，在 `cancel` 置位后尽快终止子进程。
    fn execute_with_cancel(
        &self,
        args_json: &str,
        _cancel: Option<&AtomicBool>,
    ) -> Result<String, BoxError> {
        self.execute(args_json)
    }

    /// 只读无副作用工具可返回 `true`，同一模型回合内连续多条只读调用将并发执行，
    /// 并按原始调用顺序回传结果与触发钩子；写操作、测试与外部 MCP 工具默认返回 `false`。
    fn is_read_only(&self) -> bool {
        false
    }
}

/// 按 JSON Schema 的常用子集（`type`、`properties`、`required`、`additionalProperties: false`、
/// `items`、`enum`）校验工具参数。不认识的关键字不检查。
pub fn validate_args(schema: &Value, value: &Value) -> Result<(), String> {
    validate_at(schema, value, "参数")
}

fn validate_at(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    if let Some(kind) = schema["type"].as_str() {
        let ok = match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => true,
        };
        if !ok {
            return Err(format!("{path} 应为 {kind}"));
        }
    }
    if let Some(allowed) = schema["enum"].as_array()
        && !allowed.contains(value)
    {
        return Err(format!("{path} 不在可选值之内"));
    }
    if let Some(object) = value.as_object() {
        let properties = schema["properties"].as_object();
        for key in schema["required"].as_array().into_iter().flatten() {
            if let Some(key) = key.as_str()
                && !object.contains_key(key)
            {
                return Err(format!("{path} 缺少必填字段 `{key}`"));
            }
        }
        for (key, item) in object {
            match properties.and_then(|p| p.get(key)) {
                Some(sub) => validate_at(sub, item, &format!("{path}.{key}"))?,
                None if schema["additionalProperties"] == Value::Bool(false) => {
                    return Err(format!("{path} 不认识字段 `{key}`"));
                }
                None => {}
            }
        }
    }
    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array()) {
        for (i, item) in array.iter().enumerate() {
            validate_at(items, item, &format!("{path}[{i}]"))?;
        }
    }
    Ok(())
}

/// 一次模型调用的度支预留；离开作用域（含 panic）时结算，预留不会滞留而令他件空等。
struct Reservation<'a> {
    hooks: &'a dyn ExecutionHooks,
    answered: bool,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.hooks.settle_tokens(self.answered);
    }
}

/// 包住执行钩子：记下每次调用服务端报告的用量，据此预判下一次的输入，
/// 并把本次调用的输出上限经 [`ExecutionHooks::max_output`] 交给模型 Adapter。
struct Metered<'a> {
    inner: &'a dyn ExecutionHooks,
    /// 上一次调用时的消息条数与服务端报告的用量。
    last: Cell<(usize, TokenUsage)>,
    cap: Cell<Option<u64>>,
}

impl ExecutionHooks for Metered<'_> {
    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
    fn cancel_flag(&self) -> Option<Arc<AtomicBool>> {
        self.inner.cancel_flag()
    }
    fn tool_started(&self, step: usize, tool: &str, summary: &str) {
        self.inner.tool_started(step, tool, summary);
    }
    fn tool_finished(&self, step: usize, tool: &str, ok: bool, duration_ms: u64) {
        self.inner.tool_finished(step, tool, ok, duration_ms);
    }
    fn tool_invoked(&self, tool: &str, args_json: &str, ok: bool) {
        self.inner.tool_invoked(tool, args_json, ok);
    }
    fn text_delta(&self, text: &str) {
        self.inner.text_delta(text);
    }
    fn model_usage(&self, usage: TokenUsage) {
        let (len, _) = self.last.get();
        self.last.set((len, usage));
        self.inner.model_usage(usage);
    }
    fn model_trace(&self, trace: &str) {
        self.inner.model_trace(trace);
    }
    fn token_allowance(&self) -> Option<u64> {
        self.inner.token_allowance()
    }
    fn max_output(&self) -> Option<u64> {
        self.cap.get()
    }
    fn reserve_tokens(&self, min: u64, max: u64) -> Option<u64> {
        self.inner.reserve_tokens(min, max)
    }
    fn settle_tokens(&self, answered: bool) {
        self.inner.settle_tokens(answered);
    }
    fn changes_proposed(&self, changes: Vec<FileChange>) {
        self.inner.changes_proposed(changes);
    }
    fn suspicion(&self, source: &str, threat: Threat, excerpt: &str) {
        self.inner.suspicion(source, threat, excerpt);
    }
}

impl Metered<'_> {
    /// 历史消息被二阶段压缩后重置上次用量基线，令下一次调用按压缩后的上下文重新估算。
    fn reset_baseline(&self) {
        self.last.set((0, TokenUsage::default()));
    }

    /// 预计本次输入：有上次的实际用量时，为上次的输入与输出加上新增消息的估算；
    /// 否则为整段对话的估算。估算部分加两成余量。
    fn predict(&self, messages: &[Message], specs: &[ToolSpec]) -> u64 {
        let margin = |n: u64| n + n.div_ceil(4);
        match self.last.get() {
            (len, used) if used.total() > 0 && len <= messages.len() => {
                used.total() + margin(estimate_tokens(&messages[len..], &[]))
            }
            _ => margin(estimate_tokens(messages, specs)),
        }
    }

    fn call(
        &self,
        model: &dyn ModelProvider,
        messages: &[Message],
        specs: &[ToolSpec],
    ) -> Result<ModelTurn, LoopError> {
        if self.is_cancelled() {
            return Err(LoopError::Cancelled);
        }
        // 预判度支：余额扣去预计输入后至少还要够写一段回答；再向度支预留预计输入加输出上限，
        // 并发的他件不会同时用同一笔余额。
        let mut reservation = None;
        let cap = match self.token_allowance() {
            Some(allowance) => {
                let estimate = self.predict(messages, specs);
                let budget = LoopError::Budget {
                    estimate,
                    allowance,
                };
                if estimate + MIN_OUTPUT > allowance {
                    return Err(budget);
                }
                let want = estimate + (allowance - estimate).min(RESERVED_OUTPUT);
                let Some(got) = self.inner.reserve_tokens(estimate + MIN_OUTPUT, want) else {
                    return Err(if self.is_cancelled() {
                        LoopError::Cancelled
                    } else {
                        budget
                    });
                };
                reservation = Some(Reservation {
                    hooks: self.inner,
                    answered: false,
                });
                Some(got - estimate)
            }
            None => None,
        };
        self.cap.set(cap);
        self.last.set((messages.len(), TokenUsage::default()));
        let turn = model.next_turn(messages, specs, self);
        if let Some(reservation) = reservation.as_mut() {
            reservation.answered = turn.is_ok();
        }
        drop(reservation);
        turn.map_err(|e| {
            // 撤销导致的中止不算模型故障。
            if self.is_cancelled() {
                LoopError::Cancelled
            } else {
                LoopError::Model(e)
            }
        })
    }
}

/// 步数用尽时的收束：不再提供工具，请模型就已查阅的内容回奏。
pub const STEP_LIMIT_NOTE: &str = "〔步数用尽，据已查阅的内容回奏〕";

/// 授了工具却一次未查就回奏时，发回重办的话（小模型常见）。只发回一次。
pub const UNCHECKED_SEND_BACK: &str = "发回重办：你尚未调用任何工具查阅，就已回奏。请先调用工具（如 read_file、search_code）查证，再按体例回奏；\
     确实无须查阅的，在「依据」中说明理由。";

/// 从 `messages`（通常是 System + User）开始，循环调用模型与工具，直到模型给出最终答复。
///
/// 工具输出是外来文书：以 `nonce` 为暗号包上边界（系统提示应含 [`guard::warning`]），
/// 其中检出的劫持指令经 `hooks.suspicion` 上报（锦衣卫），输出照常交给模型。
/// 参数不合工具 schema 的调用不执行，作为工具错误退回模型。对话过长时节略较早的工具输出；
/// 每次调用前按 `hooks.token_allowance` 与上次的实际用量预判度支，余额将尽时限定输出长度。`max_steps` 次调用后仍要调用工具的，
/// 再请模型不带工具回奏一次（回奏以 [`STEP_LIMIT_NOTE`] 开头），仍不回奏则报步数超限。
pub fn run(
    model: &dyn ModelProvider,
    tools: &[Box<dyn Tool>],
    messages: Vec<Message>,
    max_steps: usize,
    nonce: &str,
    hooks: &dyn ExecutionHooks,
) -> Result<String, LoopError> {
    run_with_context(
        model,
        tools,
        messages,
        max_steps,
        nonce,
        hooks,
        CONTEXT_TOKENS,
    )
}

/// 同 [`run`]，但允许指定旧工具输出的节略触发阈值。
/// 这不是模型的硬性窗口上限；最近四份工具输出始终保留。
pub fn run_with_context(
    model: &dyn ModelProvider,
    tools: &[Box<dyn Tool>],
    mut messages: Vec<Message>,
    max_steps: usize,
    nonce: &str,
    hooks: &dyn ExecutionHooks,
    context_tokens: u64,
) -> Result<String, LoopError> {
    let specs: Vec<ToolSpec> = tools.iter().map(|tool| tool.spec()).collect();
    let hooks = &Metered {
        inner: hooks,
        last: Cell::new((0, TokenUsage::default())),
        cap: Cell::new(None),
    };
    // 有工具可用、对话中尚无工具输出：首次回奏若一次未查，发回重办一次。
    let mut unchecked = !specs.is_empty()
        && !messages
            .iter()
            .any(|m| matches!(m, Message::ToolOutput { .. }));
    let mut successful_reads = Vec::new();
    for step_idx in 0..max_steps {
        let step = step_idx + 1;
        abridge(&mut messages, &specs, context_tokens, nonce);
        if compact_turns_inner(
            &mut messages,
            Some(&mut successful_reads),
            &specs,
            context_tokens,
            nonce,
        ) > 0
        {
            hooks.reset_baseline();
        }
        let request = compact_reads(&messages, &successful_reads, nonce);
        match hooks.call(model, &request, &specs)? {
            ModelTurn::Final(answer) if unchecked => {
                unchecked = false;
                messages.push(Message::Assistant(answer));
                messages.push(Message::User(UNCHECKED_SEND_BACK.into()));
            }
            ModelTurn::Final(answer) => return Ok(guard::strip_fences(&answer, nonce)),
            ModelTurn::Calls {
                calls,
                text,
                reasoning,
            } => {
                if calls.is_empty() {
                    return Err(LoopError::EmptyToolCalls);
                }
                unchecked = false;
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
                messages.push(Message::CallPrelude { text, reasoning });
                for (_, call) in &resolved {
                    messages.push(Message::ToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        args_json: call.args_json.clone(),
                    });
                }
                let cancel_flag = hooks.cancel_flag();
                let mut idx = 0;
                while idx < resolved.len() {
                    if hooks.is_cancelled() {
                        return Err(LoopError::Cancelled);
                    }
                    if resolved[idx].0.is_read_only() {
                        let mut end = idx + 1;
                        while end < resolved.len() && resolved[end].0.is_read_only() {
                            end += 1;
                        }
                        if end - idx >= 2 {
                            let mut chunk_start = idx;
                            while chunk_start < end {
                                if hooks.is_cancelled() {
                                    return Err(LoopError::Cancelled);
                                }
                                let chunk_end = (chunk_start + MAX_PARALLEL_READS).min(end);
                                let chunk = &resolved[chunk_start..chunk_end];
                                for (_, call) in chunk {
                                    let summary = summarize_tool_args(&call.name, &call.args_json);
                                    hooks.tool_started(step, &call.name, &summary);
                                }
                                let results: Vec<(Result<String, BoxError>, std::time::Duration)> =
                                    std::thread::scope(|scope| {
                                        let handles: Vec<_> = chunk
                                            .iter()
                                            .map(|(tool, call)| {
                                                let tool = *tool;
                                                let cancel = cancel_flag.as_deref();
                                                scope.spawn(move || {
                                                    let started = Instant::now();
                                                    let res =
                                                        check_args(&tool.spec(), &call.args_json)
                                                            .and_then(|()| {
                                                                tool.execute_with_cancel(
                                                                    &call.args_json,
                                                                    cancel,
                                                                )
                                                            });
                                                    (res, started.elapsed())
                                                })
                                            })
                                            .collect();
                                        handles
                                            .into_iter()
                                            .map(|h| {
                                                h.join().unwrap_or_else(|_| {
                                                    (
                                                        Err("工具执行异常终止".into()),
                                                        std::time::Duration::ZERO,
                                                    )
                                                })
                                            })
                                            .collect()
                                    });
                                for ((_, call), (result, elapsed)) in chunk.iter().zip(results) {
                                    let duration_ms =
                                        u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
                                    hooks.tool_finished(
                                        step,
                                        &call.name,
                                        result.is_ok(),
                                        duration_ms,
                                    );
                                    record_tool_result(
                                        call,
                                        result,
                                        hooks,
                                        nonce,
                                        &mut messages,
                                        &mut successful_reads,
                                    );
                                }
                                chunk_start = chunk_end;
                            }
                            idx = end;
                            continue;
                        }
                    }
                    let (tool, call) = &resolved[idx];
                    let summary = summarize_tool_args(&call.name, &call.args_json);
                    hooks.tool_started(step, &call.name, &summary);
                    let started = Instant::now();
                    let result = check_args(&tool.spec(), &call.args_json).and_then(|()| {
                        tool.execute_with_cancel(&call.args_json, cancel_flag.as_deref())
                    });
                    let duration_ms =
                        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    hooks.tool_finished(step, &call.name, result.is_ok(), duration_ms);
                    record_tool_result(
                        call,
                        result,
                        hooks,
                        nonce,
                        &mut messages,
                        &mut successful_reads,
                    );
                    idx += 1;
                }
            }
        }
    }
    // 收束：工具一律收回，请官员据已查阅的内容回奏。
    messages.push(Message::User(format!(
        "已用尽 {max_steps} 步，不能再调用工具。请就已查阅的内容按体例回奏，没查完的列入「未办之事」。"
    )));
    abridge(&mut messages, &[], context_tokens, nonce);
    if compact_turns_inner(
        &mut messages,
        Some(&mut successful_reads),
        &[],
        context_tokens,
        nonce,
    ) > 0
    {
        hooks.reset_baseline();
    }
    let request = compact_reads(&messages, &successful_reads, nonce);
    match hooks.call(model, &request, &[]) {
        Ok(ModelTurn::Final(answer)) => Ok(format!(
            "{STEP_LIMIT_NOTE}\n{}",
            guard::strip_fences(&answer, nonce)
        )),
        Err(e @ (LoopError::Cancelled | LoopError::Budget { .. })) => Err(e),
        _ => Err(LoopError::StepLimit(max_steps)),
    }
}

/// 从工具参数 JSON 中提取用于实时进度展示的简短定位摘要。
/// 绝不暴露文件内容或大段替换文本（`content`/`new_text`/`old_text`/`body` 等），长度截断至 60 字符内。
pub(crate) fn summarize_tool_args(tool: &str, args_json: &str) -> String {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(args_json) else {
        return String::new();
    };
    let get_str = |key: &str| {
        map.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let raw = if let (Some(from), Some(to)) = (get_str("from"), get_str("to")) {
        format!("{from} → {to}")
    } else if let Some(pattern) = get_str("pattern") {
        if let Some(path) = get_str("path") {
            format!("{pattern} @ {path}")
        } else {
            pattern.to_owned()
        }
    } else if let Some(path) = get_str("path") {
        if tool == "replace_text"
            && let Some(edits) = map.get("edits").and_then(Value::as_array)
        {
            format!("{path} ({} 处)", edits.len())
        } else {
            let start = map
                .get("start_line")
                .or_else(|| map.get("offset"))
                .and_then(Value::as_i64);
            let end = map.get("end_line").and_then(Value::as_i64);
            let lines = map.get("lines").and_then(Value::as_i64);
            match (start, end, lines) {
                (Some(s), Some(e), _) => format!("{path}:{s}-{e}"),
                (None, Some(e), _) => format!("{path}:1-{e}"),
                (Some(s), None, Some(n)) if s >= 1 && n >= 1 => {
                    format!("{path}:{s}-{}", s.saturating_add(n - 1))
                }
                (None, None, Some(n)) if n >= 1 => format!("{path}:1-{n}"),
                (Some(s), None, _) => format!("{path}:{s}+"),
                _ => path.to_owned(),
            }
        }
    } else if tool == "staged_diff"
        || (tool == "revert_staged" && map.get("all") == Some(&Value::Bool(true)))
    {
        "全部暂存".to_owned()
    } else if let Some(preset) = get_str("preset") {
        preset.to_owned()
    } else if tool == "run_tests"
        && let Some(command) = get_str("command")
    {
        command.to_owned()
    } else if let Some(symbol) = get_str("symbol") {
        symbol.to_owned()
    } else if let Some(query) = get_str("query") {
        query.to_owned()
    } else if let Some(rev) = get_str("rev").or_else(|| get_str("commit")) {
        rev.to_owned()
    } else if let Some(target) = get_str("dir")
        .or_else(|| get_str("file"))
        .or_else(|| get_str("name"))
    {
        target.to_owned()
    } else {
        return String::new();
    };
    let normalized: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX_CHARS: usize = 60;
    if normalized.chars().count() <= MAX_CHARS {
        normalized
    } else {
        let head: String = normalized.chars().take(MAX_CHARS - 1).collect();
        format!("{head}…")
    }
}

/// 按原始调用顺序记录单次工具结果：触发钩子、记录成功读取、锦衣卫扫描并套上外来文书围栏。
fn record_tool_result(
    call: &ToolRequest,
    result: Result<String, BoxError>,
    hooks: &dyn ExecutionHooks,
    nonce: &str,
    messages: &mut Vec<Message>,
    successful_reads: &mut Vec<(usize, String)>,
) {
    hooks.tool_invoked(&call.name, &call.args_json, result.is_ok());
    if call.name == "read_file" && result.is_ok() {
        successful_reads.push((messages.len(), call.args_json.clone()));
    }
    let output = result.unwrap_or_else(|err| format!("tool error: {err}"));
    for finding in guard::scan(&output, &[Threat::Hijack]) {
        let source = format!("tool:{}", call.name);
        hooks.suspicion(&source, finding.threat, &finding.excerpt);
    }
    messages.push(Message::ToolOutput {
        call_id: call.id.clone(),
        output: guard::fence(&output, nonce),
    });
}

/// 参数须是合法 JSON，并合乎工具声明的 schema。
fn check_args(spec: &ToolSpec, args_json: &str) -> Result<(), BoxError> {
    let args: Value = serde_json::from_str(if args_json.trim().is_empty() {
        "{}"
    } else {
        args_json
    })
    .map_err(|e| format!("参数不是合法 JSON：{e}"))?;
    let schema: Value = serde_json::from_str(&spec.input_schema_json)?;
    validate_args(&schema, &args).map_err(|why| format!("参数不合 schema：{why}").into())
}

#[cfg(test)]
mod tests;
