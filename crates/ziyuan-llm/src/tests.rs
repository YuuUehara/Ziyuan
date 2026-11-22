use super::*;
use crate::openai::*;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use ziyuan_core::TokenUsage;
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
            mcp: Default::default(),
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
        // 有实录时另给只读的 search_records，不受玉玺工具所限。
        records: std::sync::Arc::new(vec![ziyuan_core::Record {
            task: TaskId(0),
            department: Department::Works,
            edict: "旧诏".into(),
            worker: AgentId(4),
            handler: "陈工".into(),
            summary: "旧奏".into(),
            verdict: "待评定".into(),
        }]),
        workspace_override: None,
        workspace_lease: None,
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
    assert_eq!(
        names,
        [
            "search_code",
            "find_symbols",
            "find_references",
            "symbol_impact",
            "search_records"
        ]
    );
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
            output: 40,
            cache_read: 0,
            cache_write: 0,
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
            delta(json!({"tool_calls": [{"index": 0, "function": {"arguments": "th\":\"x\"}"}}]})),
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
            output: 9,
            cache_read: 0,
            cache_write: 0,
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
        cache_read: 0,
        cache_write: 0,
    };
    assert_eq!(price.cost(usage), 6.0);
    assert_eq!(price.describe(usage), "约 6.0000 元");
    assert_eq!(parse_price("0.5,1.5,USD").unwrap().currency, "USD");
    assert!(parse_price("免费").is_none() && parse_price("1").is_none());
    for invalid in ["NaN,1", "1,inf", "-1,2", "2,-0.1"] {
        assert!(parse_price(invalid).is_none(), "{invalid}");
    }
    let (price, source) = price_quote("deepseek-v4-pro").unwrap();
    assert_eq!(price.input_per_million, 9.0);
    assert_eq!(price.output_per_million, 27.0);
    assert!(source.contains("2026-10"));
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

#[test]
fn prompt_cache_injects_anthropic_breakpoints_only_when_applicable() {
    let messages = vec![
        Message::System("你是工部官员".into()),
        Message::User("读取 src/lib.rs".into()),
        Message::ToolCall {
            id: "c1".into(),
            name: "read_file".into(),
            args_json: r#"{"path":"src/lib.rs"}"#.into(),
        },
        Message::ToolOutput {
            call_id: "c1".into(),
            output: "fn main() {}".into(),
        },
        Message::User("再检查一下测试".into()),
    ];
    let tools = vec![
        ToolSpec {
            name: "read_file".into(),
            description: "读文件".into(),
            input_schema_json: r#"{"type":"object"}"#.into(),
        },
        ToolSpec {
            name: "write_file".into(),
            description: "写文件".into(),
            input_schema_json: r#"{"type":"object"}"#.into(),
        },
    ];

    // 1. Auto + Claude 模型：自动加 system / 末个 tool / 历史边界三处 cache_control。
    let (url, seen) = mock_server(vec![(200, final_reply("ok"))]);
    OpenAiCompatible::new(url, Some("k".into()), "anthropic/claude-sonnet-4.5")
        .with_retry(Retry {
            attempts: 0,
            ..Retry::default()
        })
        .next_turn(&messages, &tools, &())
        .unwrap();
    let body = &seen.lock().unwrap()[0].0;
    assert_eq!(
        body["messages"][0]["content"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert!(body["tools"][0].get("cache_control").is_none());
    assert_eq!(
        body["tools"][1]["cache_control"],
        json!({"type": "ephemeral"})
    );
    // 历史断点打在倒数第二条（tool output）上，最后一条新用户输入保持纯文本。
    assert_eq!(
        body["messages"][3]["content"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(body["messages"][4]["content"], "再检查一下测试");

    // 2. Auto + DeepSeek / OpenAI：保持原始纯文本格式，不注入 cache_control。
    let (url, seen) = mock_server(vec![(200, final_reply("ok"))]);
    OpenAiCompatible::new(url, Some("k".into()), "deepseek-v4-pro")
        .with_retry(Retry {
            attempts: 0,
            ..Retry::default()
        })
        .next_turn(&messages, &tools, &())
        .unwrap();
    let body = &seen.lock().unwrap()[0].0;
    assert_eq!(body["messages"][0]["content"], "你是工部官员");
    assert!(body["tools"][1].get("cache_control").is_none());

    // 3. Off + Claude：显式关闭时不注入。
    let (url, seen) = mock_server(vec![(200, final_reply("ok"))]);
    OpenAiCompatible::new(url, Some("k".into()), "claude-opus-4")
        .with_prompt_cache(PromptCache::Off)
        .with_retry(Retry {
            attempts: 0,
            ..Retry::default()
        })
        .next_turn(&messages, &tools, &())
        .unwrap();
    let body = &seen.lock().unwrap()[0].0;
    assert_eq!(body["messages"][0]["content"], "你是工部官员");
    assert!(body["tools"][1].get("cache_control").is_none());

    // 4. Anthropic 强制模式：自定义模型名也注入。
    let (url, seen) = mock_server(vec![(200, final_reply("ok"))]);
    OpenAiCompatible::new(url, Some("k".into()), "my-proxy-model")
        .with_prompt_cache(PromptCache::Anthropic)
        .with_retry(Retry {
            attempts: 0,
            ..Retry::default()
        })
        .next_turn(&messages, &tools, &())
        .unwrap();
    let body = &seen.lock().unwrap()[0].0;
    assert_eq!(
        body["messages"][0]["content"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(PromptCache::parse("auto").unwrap(), PromptCache::Auto);
    assert_eq!(PromptCache::parse("on").unwrap(), PromptCache::Anthropic);
    assert_eq!(PromptCache::parse("off").unwrap(), PromptCache::Off);
    assert!(PromptCache::parse("bogus").is_err());
}

#[test]
fn report_usage_parses_openai_deepseek_and_anthropic_cache_fields() {
    // OpenAI / OpenRouter: prompt_tokens_details.cached_tokens + cache_write_tokens
    let hooks = Recorder::default();
    report_usage(
        &json!({
            "usage": {
                "prompt_tokens": 1200,
                "completion_tokens": 80,
                "prompt_tokens_details": {
                    "cached_tokens": 900,
                    "cache_write_tokens": 200
                }
            }
        }),
        &hooks,
    );
    // DeepSeek: prompt_cache_hit_tokens
    report_usage(
        &json!({
            "usage": {
                "prompt_tokens": 500,
                "completion_tokens": 40,
                "prompt_cache_hit_tokens": 350,
                "prompt_cache_miss_tokens": 150
            }
        }),
        &hooks,
    );
    // Anthropic 风格：input_tokens 不含 cache_read/cache_creation，折算入总 input
    report_usage(
        &json!({
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_read_input_tokens": 800,
                "cache_creation_input_tokens": 300
            }
        }),
        &hooks,
    );
    assert_eq!(
        *hooks.usage.borrow(),
        [
            TokenUsage::with_cache(1200, 80, 900, 200),
            TokenUsage::with_cache(500, 40, 350, 0),
            TokenUsage::with_cache(1200, 50, 800, 300),
        ]
    );
}

type RawRequests = Arc<Mutex<Vec<(Value, String)>>>;

fn mock_http(replies: Vec<(u16, &'static str, &'static str, String)>) -> (String, RawRequests) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    thread::spawn(move || {
        for (status, content_type, extra, reply) in replies {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut length = 0usize;
            let mut raw_headers = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let lower = line.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
                raw_headers.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            log.lock().unwrap().push((
                serde_json::from_slice(&body).unwrap(),
                raw_headers.to_ascii_lowercase(),
            ));
            let mut stream = reader.into_inner();
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

fn expect_final(turn: ModelTurn) -> String {
    match turn {
        ModelTurn::Final(text) => text,
        other => panic!("expected ModelTurn::Final, got {other:?}"),
    }
}

#[test]
fn anthropic_native_streams_tools_thinking_and_cache_usage() {
    let sse = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":120,\"cache_creation_input_tokens\":300,\"cache_read_input_tokens\":800,\"output_tokens\":1}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"先看源码\"}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"我来查一下\"}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"src/lib.rs\\\"}\"}}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":45}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n"
    );
    let (url, seen) = mock_http(vec![
        (
            529,
            "application/json",
            "",
            "{\"error\":\"overloaded\"}".into(),
        ),
        (200, "text/event-stream", "", sse.into()),
    ]);
    let messages = vec![
        Message::System("你是工部尚书".into()),
        Message::User("检查 lib.rs".into()),
        Message::CallPrelude {
            text: "先列出文件".into(),
            reasoning: "内部思维链不回传".into(),
        },
        Message::ToolCall {
            id: "toolu_0".into(),
            name: "list_files".into(),
            args_json: "{}".into(),
        },
        Message::ToolOutput {
            call_id: "toolu_0".into(),
            output: "src/lib.rs".into(),
        },
        Message::User("请继续读取".into()),
    ];
    let tools = vec![ToolSpec {
        name: "read_file".into(),
        description: "读取文件".into(),
        input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#.into(),
    }];
    let hooks = Recorder::default();
    let provider = AnthropicMessages::new(url, Some("sk-ant-test".into()), "claude-sonnet-4-5")
        .with_retry(Retry {
            attempts: 1,
            base: Duration::from_millis(1),
            max: Duration::from_millis(5),
        });
    let turn = provider.next_turn(&messages, &tools, &hooks).unwrap();
    match turn {
        ModelTurn::Calls {
            calls,
            text,
            reasoning,
        } => {
            assert_eq!(text, "我来查一下");
            assert_eq!(reasoning, "先看源码");
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].id, "toolu_1");
            assert_eq!(calls[0].name, "read_file");
            assert_eq!(calls[0].args_json, r#"{"path":"src/lib.rs"}"#);
        }
        other => panic!("expected tool calls, got {other:?}"),
    }
    assert_eq!(
        *hooks.usage.borrow(),
        [TokenUsage::with_cache(1220, 45, 800, 300)]
    );
    assert_eq!(hooks.deltas.borrow().concat(), "我来查一下\n");

    let recorded = seen.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    let (body, headers) = &recorded[1];
    assert!(headers.contains("x-api-key: sk-ant-test"));
    assert!(headers.contains("anthropic-version: 2023-06-01"));
    assert_eq!(body["model"], "claude-sonnet-4-5");
    assert_eq!(
        body["system"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(
        body["tools"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    // 连续 user 消息（ToolOutput + User）合并为同一条 user 消息，且倒数第二条消息有缓存断点。
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["content"][1]["type"], "tool_use");
    assert_eq!(
        msgs[1]["content"][1]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(msgs[2]["role"], "user");
    assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
    assert_eq!(msgs[2]["content"][1]["type"], "text");
}

#[test]
fn gemini_native_streams_tools_thought_and_cached_usage() {
    let sse = concat!(
        "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"先分析结构\",\"thought\":true},{\"text\":\"开始查阅\"}]}}]}\n\n",
        "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"name\":\"search_code\",\"args\":{\"query\":\"main\"}}}]}}],\"usageMetadata\":{\"promptTokenCount\":900,\"candidatesTokenCount\":35,\"cachedContentTokenCount\":600}}\n\n"
    );
    let (url, seen) = mock_http(vec![(200, "text/event-stream", "", sse.into())]);
    let messages = vec![
        Message::System("你是都察院御史".into()),
        Message::User("查 main".into()),
        Message::ToolCall {
            id: "gemini-call-0".into(),
            name: "list_files".into(),
            args_json: "{}".into(),
        },
        Message::ToolOutput {
            call_id: "gemini-call-0".into(),
            output: "src/main.rs".into(),
        },
    ];
    let tools = vec![ToolSpec {
        name: "search_code".into(),
        description: "搜索代码".into(),
        input_schema_json: r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{"query":{"type":"string"}}}"#.into(),
    }];
    let hooks = Recorder::default();
    let provider = GeminiNative::new(url, Some("gem-secret-key".into()), "gemini-2.5-pro")
        .with_retry(Retry {
            attempts: 0,
            ..Retry::default()
        });
    let turn = provider.next_turn(&messages, &tools, &hooks).unwrap();
    match turn {
        ModelTurn::Calls {
            calls,
            text,
            reasoning,
        } => {
            assert_eq!(text, "开始查阅");
            assert_eq!(reasoning, "先分析结构");
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].id, "gemini-call-0");
            assert_eq!(calls[0].name, "search_code");
            assert_eq!(calls[0].args_json, r#"{"query":"main"}"#);
        }
        other => panic!("expected tool calls, got {other:?}"),
    }
    assert_eq!(
        *hooks.usage.borrow(),
        [TokenUsage::with_cache(900, 35, 600, 0)]
    );

    let recorded = seen.lock().unwrap();
    let (body, headers) = &recorded[0];
    assert!(headers.contains("x-goog-api-key: gem-secret-key"));
    assert!(!headers.contains("key=gem-secret-key"));
    assert_eq!(
        body["systemInstruction"]["parts"][0]["text"],
        "你是都察院御史"
    );
    assert!(
        body["tools"][0]["functionDeclarations"][0]["parameters"]
            .get("$schema")
            .is_none()
    );
    assert!(
        body["tools"][0]["functionDeclarations"][0]["parameters"]
            .get("additionalProperties")
            .is_none()
    );
    // ToolOutput 对应回传的 functionResponse.name 还原为 list_files。
    assert_eq!(
        body["contents"][2]["parts"][0]["functionResponse"]["name"],
        "list_files"
    );
}

#[test]
fn models_config_forbids_plaintext_secrets_and_validates_fields() {
    let bad_secret = r#"{
        "profiles": {
            "claude": {
                "provider": "anthropic",
                "model": "claude-sonnet-4-5",
                "api_key": "sk-ant-never-put-here"
            }
        }
    }"#;
    let err = ModelsConfig::parse(bad_secret).unwrap_err().to_string();
    assert!(err.contains("禁止写入明文密钥"), "{err}");

    let bad_default = r#"{
        "default": "missing",
        "profiles": {
            "local": { "model": "qwen3:32b", "base_url": "http://localhost:11434/v1" }
        }
    }"#;
    assert!(ModelsConfig::parse(bad_default).is_err());

    let valid = r#"{
        "default": "deepseek",
        "profiles": {
            "deepseek": {
                "provider": "openai",
                "base_url": "https://api.deepseek.com/v1",
                "model": "deepseek-v4-pro",
                "api_key_env": "DEEPSEEK_API_KEY"
            },
            "claude-sonnet": {
                "provider": "anthropic",
                "model": "claude-sonnet-4-5",
                "api_key_env": "ANTHROPIC_API_KEY",
                "prompt_cache": "anthropic",
                "price": { "input": 3.0, "output": 15.0, "currency": "USD" }
            },
            "gemini-pro": {
                "provider": "gemini",
                "model": "gemini-2.5-pro",
                "api_key_env": "GEMINI_API_KEY"
            }
        }
    }"#;
    let cfg = ModelsConfig::parse(valid).unwrap();
    assert_eq!(cfg.default_profile.as_deref(), Some("deepseek"));
    assert_eq!(cfg.profiles.len(), 3);
    assert_eq!(
        cfg.profiles["claude-sonnet"].provider,
        ProviderKind::Anthropic
    );
    assert_eq!(cfg.profiles["gemini-pro"].provider, ProviderKind::Gemini);
}

#[test]
fn heterogeneous_catalog_routes_openai_anthropic_and_gemini_and_fails_missing_key_per_profile() {
    let (openai_url, _) = mock_server(vec![(200, final_reply("来自 OpenAI 兼容"))]);
    let (anthropic_url, _) = mock_http(vec![(
        200,
        "application/json",
        "",
        json!({
            "content": [{"type": "text", "text": "来自 Anthropic 原生"}],
            "usage": {"input_tokens": 10, "output_tokens": 5}
        })
        .to_string(),
    )]);
    let (gemini_url, _) = mock_http(vec![(
        200,
        "application/json",
        "",
        json!({
            "candidates": [{"content": {"role": "model", "parts": [{"text": "来自 Gemini 原生"}]}}],
            "usageMetadata": {"promptTokenCount": 12, "candidatesTokenCount": 6}
        })
        .to_string(),
    )]);

    let cfg = ModelsConfig::parse(
        &json!({
            "default": "oai",
            "profiles": {
                "oai": {
                    "provider": "openai",
                    "base_url": openai_url,
                    "model": "deepseek-v4-pro"
                },
                "ant": {
                    "provider": "anthropic",
                    "base_url": anthropic_url,
                    "model": "claude-sonnet-4-5",
                    "price": {"input": 3.0, "output": 15.0, "currency": "USD"}
                },
                "gem": {
                    "provider": "gemini",
                    "base_url": gemini_url,
                    "model": "gemini-2.5-pro"
                },
                "needs-key": {
                    "provider": "anthropic",
                    "base_url": "http://127.0.0.1:9/v1",
                    "model": "claude-opus-4",
                    "api_key_env": "ZIYUAN_TEST_DEFINITELY_UNSET_KEY_99999"
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let catalog = OpenAiCompatible::new("http://127.0.0.1:9/v1", None, "fallback")
        .with_retry(Retry {
            attempts: 0,
            ..Retry::default()
        })
        .catalog()
        .with_profiles(cfg.profiles);

    let msgs = [Message::User("请回奏".into())];
    assert_eq!(
        expect_final(catalog.provider("oai").next_turn(&msgs, &[], &()).unwrap()),
        "来自 OpenAI 兼容"
    );
    assert_eq!(
        expect_final(catalog.provider("ant").next_turn(&msgs, &[], &()).unwrap()),
        "来自 Anthropic 原生"
    );
    assert_eq!(
        expect_final(catalog.provider("gem").next_turn(&msgs, &[], &()).unwrap()),
        "来自 Gemini 原生"
    );
    // 缺少环境变量仅在调用该配置时报错，不影响其余配置。
    let err = catalog
        .provider("needs-key")
        .next_turn(&msgs, &[], &())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("ZIYUAN_TEST_DEFINITELY_UNSET_KEY_99999"),
        "{err}"
    );

    assert_eq!(catalog.price("oai").unwrap().currency, "元");
    assert_eq!(catalog.price("ant").unwrap().currency, "USD");
    assert_eq!(catalog.price("gem").unwrap().currency, "USD");
    assert_eq!(
        price_for("anthropic/claude-sonnet-4-5")
            .unwrap()
            .input_per_million,
        3.0
    );
    assert_eq!(price_for("gpt-5").unwrap().currency, "USD");
}
