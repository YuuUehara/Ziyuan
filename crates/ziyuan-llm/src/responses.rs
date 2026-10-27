//! ChatGPT 订阅额度所用的 Responses API：请求体、SSE 事件流、错误码。
//!
//! 订阅额度只接受 `store:false`、`stream:true`；系统提示走 `instructions`；
//! 函数工具须归入一个 namespace（见 <https://developers.openai.com/siwc>）。
use serde_json::{Value, json};
use std::io::BufRead;
use ziyuan_core::TokenUsage;
use ziyuan_runtime::agent_loop::{Message, ModelTurn, ToolRequest, ToolSpec};
use ziyuan_runtime::{BoxError, ExecutionHooks};

use crate::chatgpt::USAGE_URL;

/// 紫垣工具所在的 namespace。
const NAMESPACE: &str = "ziyuan";

pub(crate) fn body(
    model: &str,
    messages: &[Message],
    tools: &[ToolSpec],
) -> Result<Value, BoxError> {
    let mut instructions: Vec<&str> = Vec::new();
    let mut input: Vec<Value> = Vec::new();
    for message in messages {
        match message {
            Message::System(text) => instructions.push(text),
            Message::User(text) => input.push(json!({"role": "user", "content": text})),
            Message::Assistant(text) => input.push(json!({"role": "assistant", "content": text})),
            // 思维链不回传：store:false 下未加密的推理项无法续用。
            Message::CallPrelude { text, .. } => {
                if !text.is_empty() {
                    input.push(json!({"role": "assistant", "content": text}));
                }
            }
            Message::ToolCall {
                id,
                name,
                args_json,
            } => input.push(json!({
                "type": "function_call",
                "call_id": id,
                "name": name,
                "namespace": NAMESPACE,
                "arguments": args_json,
            })),
            Message::ToolOutput { call_id, output } => input.push(json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": output,
            })),
        }
    }
    let mut body = json!({
        "model": model,
        "input": input,
        "store": false,
        "stream": true,
    });
    if !instructions.is_empty() {
        body["instructions"] = json!(instructions.join("\n\n"));
    }
    if !tools.is_empty() {
        let functions = tools
            .iter()
            .map(|tool| {
                let parameters: Value = serde_json::from_str(&tool.input_schema_json)?;
                Ok(json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": parameters,
                }))
            })
            .collect::<Result<Vec<_>, BoxError>>()?;
        body["tools"] = json!([{
            "type": "namespace",
            "name": NAMESPACE,
            "description": "紫垣朝廷的工具（文件、命令、诏令等）。",
            "tools": functions,
        }]);
    }
    Ok(body)
}

/// 订阅额度的错误：(可否重试, 中文说明)。正文可能是 `{"error":{"code":…}}`，也可能是 `{"detail":…}`。
pub(crate) fn plan_error(status: u16, text: &str) -> (bool, Option<String>) {
    let value: Value = serde_json::from_str(text).unwrap_or_default();
    let code = value
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let param = value.pointer("/error/param").and_then(Value::as_str);
    let note = describe(code, param).or_else(|| match status {
        401 => Some(
            "ChatGPT 订阅未通过验证：请检查所登录的账号与授权，必要时重新 `ziyuan login`".into(),
        ),
        403 => Some(format!(
            "ChatGPT 订阅被拒（地区或权限限制）：{}",
            value["detail"].as_str().unwrap_or(text)
        )),
        503 => Some("ChatGPT 订阅直连暂不可用，稍后再试".into()),
        _ => None,
    });
    let retryable = match code {
        "subscription_sharing_usage_unavailable" | "subscription_sharing_user_unavailable" => true,
        "" => status == 503 || (status >= 500 && status != 501),
        _ => false,
    };
    (retryable, note)
}

fn describe(code: &str, param: Option<&str>) -> Option<String> {
    Some(match code {
        "subscription_sharing_usage_limit_exceeded" => {
            format!("ChatGPT 订阅额度已用尽（可能是紫垣这一应用的额度），用量见 {USAGE_URL}")
        }
        "subscription_sharing_usage_unavailable" => {
            "暂时无法查询 ChatGPT 订阅额度，稍后再试".into()
        }
        "subscription_sharing_user_not_eligible" => {
            "此账号或工作区不能把 ChatGPT 订阅额度用于紫垣".into()
        }
        "subscription_sharing_unsupported_capability" => format!(
            "ChatGPT 订阅不支持请求中的 {}",
            param.unwrap_or("某项参数（模型、工具或输入）")
        ),
        "subscription_sharing_route_not_supported" => "ChatGPT 订阅不支持此接口".into(),
        "subscription_sharing_invalid_user" => {
            "ChatGPT 订阅账号验证失败，请重新 `ziyuan login`".into()
        }
        "subscription_sharing_user_unavailable" => "ChatGPT 账号信息暂不可用，稍后再试".into(),
        "chatpass_v2_scope_not_authorized" | "chatpass_v2_invalid_authorization_context" => {
            "授权范围不含此操作，请重新 `ziyuan login` 并允许使用订阅额度".into()
        }
        _ => return None,
    })
}

/// 解析 Responses 的 SSE 事件流；只有收到 `response.completed` 才算成功。每读一行检查一次撤销。
pub(crate) fn read_events(
    reader: impl BufRead,
    hooks: &dyn ExecutionHooks,
) -> Result<ModelTurn, BoxError> {
    let mut content = String::new();
    let mut calls: Vec<ToolRequest> = Vec::new();
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
        let event: Value = serde_json::from_str(data)?;
        match event["type"].as_str().unwrap_or_default() {
            "response.output_text.delta" => {
                if let Some(text) = event["delta"].as_str().filter(|t| !t.is_empty()) {
                    hooks.text_delta(text);
                    content.push_str(text);
                }
            }
            "response.output_item.done" if event["item"]["type"] == "function_call" => {
                let item = &event["item"];
                let field = |key: &str| {
                    item[key]
                        .as_str()
                        .filter(|v| !v.is_empty())
                        .map(str::to_owned)
                        .ok_or_else(|| BoxError::from(format!("function_call 缺少 {key}: {item}")))
                };
                let name = field("name")?;
                // namespace 内的工具可能以 `ziyuan.name` 返回。
                let name = name
                    .strip_prefix(&format!("{NAMESPACE}."))
                    .map_or(name.clone(), str::to_owned);
                calls.push(ToolRequest {
                    id: field("call_id")?,
                    name,
                    args_json: item["arguments"].as_str().unwrap_or("{}").to_owned(),
                });
            }
            "response.completed" => {
                let usage = &event["response"]["usage"];
                if let (Some(input), Some(output)) = (
                    usage["input_tokens"].as_u64(),
                    usage["output_tokens"].as_u64(),
                ) {
                    hooks.model_usage(TokenUsage { input, output });
                }
                if calls.is_empty() {
                    return Ok(ModelTurn::Final(content));
                }
                if !content.is_empty() && !content.ends_with('\n') {
                    hooks.text_delta("\n");
                }
                return Ok(ModelTurn::Calls {
                    calls,
                    text: content,
                    reasoning: String::new(),
                });
            }
            "response.failed" | "error" => {
                let error = event
                    .pointer("/response/error")
                    .or_else(|| event.get("error"))
                    .unwrap_or(&event);
                let code = error["code"].as_str().unwrap_or_default();
                let message = error["message"].as_str().unwrap_or_default();
                return Err(match describe(code, error["param"].as_str()) {
                    Some(note) => format!("{note}（{code}）"),
                    None => format!("模型响应失败：{code} {message}"),
                }
                .into());
            }
            "response.incomplete" => {
                let reason = event
                    .pointer("/response/incomplete_details/reason")
                    .and_then(Value::as_str)
                    .unwrap_or("未知原因");
                return Err(format!("模型响应未完成：{reason}").into());
            }
            _ => {}
        }
    }
    Err("流式响应在 response.completed 之前中断".into())
}
