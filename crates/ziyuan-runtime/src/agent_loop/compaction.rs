//! 上下文估算、旧工具输出节略、二阶段轮次压缩与重复 `read_file` 请求去重投影。

use serde_json::Value;

use crate::guard;

use super::{COMPACT_ACK, Message, ToolSpec};

/// 节略时原样保留的最近几份工具输出。
const KEEP_RECENT_OUTPUTS: usize = 4;
/// 节略后每份工具输出保留的开头字数。
const ABRIDGED_HEAD: usize = 200;
/// 二阶段轮次压缩时原样保留的最近完整轮次数。
const KEEP_RECENT_TURNS: usize = 4;
/// 二阶段轮次压缩至少一次折叠的较早轮次数（避免单轮抖动）。
const MIN_FOLD_TURNS: usize = 2;
/// 结构化历史压缩摘要的字符上限。
const MAX_DIGEST_CHARS: usize = 3_000;

/// 粗估 token 数：ASCII 约 4 字节一个，其余（汉字等）每字一个；宁多勿少，供度支与节略预判。
pub fn estimate_tokens(messages: &[Message], tools: &[ToolSpec]) -> u64 {
    let count = |text: &str| {
        let ascii = text.bytes().filter(u8::is_ascii).count() as u64;
        let other = text.chars().filter(|c| !c.is_ascii()).count() as u64;
        ascii.div_ceil(4) + other
    };
    let messages: u64 = messages
        .iter()
        .map(|m| match m {
            Message::System(t) | Message::User(t) | Message::Assistant(t) => count(t),
            Message::CallPrelude { text, reasoning } => count(text) + count(reasoning),
            Message::ToolCall {
                id,
                name,
                args_json,
            } => count(id) + count(name) + count(args_json),
            Message::ToolOutput { call_id, output } => count(call_id) + count(output),
        } + 4)
        .sum();
    let tools: u64 = tools
        .iter()
        .map(|t| count(&t.name) + count(&t.description) + count(&t.input_schema_json))
        .sum();
    messages + tools
}

/// 节略：对话估算超过 `limit` 时，从最早的起把工具输出压成开头一段（仍包在外来文书边界内），
/// 最近 [`KEEP_RECENT_OUTPUTS`] 份不动。不调用模型。返回节略的份数。
pub fn abridge(messages: &mut [Message], tools: &[ToolSpec], limit: u64, nonce: &str) -> usize {
    let outputs: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m, Message::ToolOutput { .. }))
        .map(|(i, _)| i)
        .collect();
    let old = outputs.len().saturating_sub(KEEP_RECENT_OUTPUTS);
    let mut abridged = 0;
    for &index in &outputs[..old] {
        if estimate_tokens(messages, tools) <= limit {
            break;
        }
        let Message::ToolOutput { output, .. } = &mut messages[index] else {
            unreachable!("只收集了工具输出");
        };
        let inner = guard::unfence(output, nonce);
        let total = inner.chars().count();
        if total <= ABRIDGED_HEAD * 2 || inner.contains("〔节略") {
            continue;
        }
        let head: String = inner.chars().take(ABRIDGED_HEAD).collect();
        let note =
            format!("{head}…\n〔节略：原文 {total} 字，此处只留开头；如需全文请再调用工具〕");
        *output = guard::fence(&note, nonce);
        abridged += 1;
    }
    abridged
}

/// 二阶段可控轮次压缩：经 [`abridge`] 节略旧工具输出后，若估算 token 仍超出 `limit`
/// 且已积累足够多的完整历史轮次（至少保留最近 [`KEEP_RECENT_TURNS`] 轮完整对话），
/// 则把较早的完整轮次折叠为一条带外来文书边界的结构化工作摘要（保留已修改文件、
/// 测试诊断与查阅足迹），不调用模型，且严格保持 `CallPrelude`/`ToolCall`/`ToolOutput`
/// 配对与 `user`/`assistant` 交替协议。返回本次折叠的轮次数。
pub fn compact_turns(
    messages: &mut Vec<Message>,
    tools: &[ToolSpec],
    limit: u64,
    nonce: &str,
) -> usize {
    compact_turns_inner(messages, None, tools, limit, nonce)
}

pub(super) fn compact_turns_inner(
    messages: &mut Vec<Message>,
    successful_reads: Option<&mut Vec<(usize, String)>>,
    tools: &[ToolSpec],
    limit: u64,
    nonce: &str,
) -> usize {
    if estimate_tokens(messages, tools) <= limit {
        return 0;
    }
    // 保留开头全部 System 与首条任务 User 消息；若开头无 User 消息则不介入。
    let prefix_end = messages
        .iter()
        .take_while(|m| matches!(m, Message::System(_) | Message::User(_)))
        .count();
    if prefix_end == 0
        || !messages[..prefix_end]
            .iter()
            .any(|m| matches!(m, Message::User(_)))
    {
        return 0;
    }
    let (turns_start, previous_digest) =
        match (messages.get(prefix_end), messages.get(prefix_end + 1)) {
            (Some(Message::Assistant(ack)), Some(Message::User(digest))) if ack == COMPACT_ACK => (
                prefix_end + 2,
                Some(guard::unfence(digest, nonce).to_owned()),
            ),
            _ => (prefix_end, None),
        };
    let turns = collect_foldable_turns(messages, turns_start);
    if turns.len() < KEEP_RECENT_TURNS + MIN_FOLD_TURNS {
        return 0;
    }
    let fold_count = turns.len() - KEEP_RECENT_TURNS;
    let fold_start = prefix_end;
    let fold_end = turns[fold_count - 1].1;
    let digest_body = build_turn_digest(
        &messages[turns_start..fold_end],
        fold_count,
        previous_digest.as_deref(),
        nonce,
    );
    let replacement = [
        Message::Assistant(COMPACT_ACK.to_owned()),
        Message::User(guard::fence(&digest_body, nonce)),
    ];
    let before_tokens = estimate_tokens(&messages[fold_start..fold_end], &[]);
    let after_tokens = estimate_tokens(&replacement, &[]);
    if after_tokens >= before_tokens {
        return 0;
    }
    let removed_len = fold_end - fold_start;
    messages.splice(fold_start..fold_end, replacement);
    if let Some(reads) = successful_reads {
        reads.retain_mut(|(idx, _)| {
            if *idx < fold_start {
                true
            } else if *idx < fold_end {
                false
            } else {
                *idx = *idx - removed_len + 2;
                true
            }
        });
    }
    fold_count
}

/// 从 `start` 起扫描连续的完整原子轮次，绝不拆散 `CallPrelude`/`ToolCall`/`ToolOutput`。
fn collect_foldable_turns(messages: &[Message], start: usize) -> Vec<(usize, usize)> {
    let mut turns = Vec::new();
    let mut i = start;
    while i < messages.len() {
        match &messages[i] {
            Message::CallPrelude { .. } | Message::ToolCall { .. } => {
                let turn_start = i;
                if matches!(messages[i], Message::CallPrelude { .. }) {
                    i += 1;
                }
                let mut saw_call = false;
                while i < messages.len() && matches!(messages[i], Message::ToolCall { .. }) {
                    saw_call = true;
                    i += 1;
                }
                if !saw_call {
                    break;
                }
                while i < messages.len() && matches!(messages[i], Message::ToolOutput { .. }) {
                    i += 1;
                }
                turns.push((turn_start, i));
            }
            Message::Assistant(_) => {
                let turn_start = i;
                i += 1;
                if i < messages.len() && matches!(messages[i], Message::User(_)) {
                    i += 1;
                }
                turns.push((turn_start, i));
            }
            _ => break,
        }
    }
    turns
}

fn clip_chars(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.push('…');
    }
    out
}

fn first_meaningful_line(text: &str, max_chars: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    clip_chars(line, max_chars)
}

pub(super) fn build_turn_digest(
    slice: &[Message],
    fold_count: usize,
    previous_digest: Option<&str>,
    nonce: &str,
) -> String {
    use std::collections::{BTreeMap, HashMap};

    let mut outputs: HashMap<&str, String> = HashMap::new();
    for msg in slice {
        if let Message::ToolOutput { call_id, output } = msg {
            let clean = guard::strip_fences(guard::unfence(output, nonce), nonce);
            outputs.insert(call_id.as_str(), clean);
        }
    }

    let mut total_calls = 0usize;
    let mut edits: BTreeMap<String, String> = BTreeMap::new();
    let mut tests: Vec<String> = Vec::new();
    let mut explorations: Vec<String> = Vec::new();

    for msg in slice {
        let Message::ToolCall {
            id,
            name,
            args_json,
        } = msg
        else {
            continue;
        };
        total_calls += 1;
        let args: Value = serde_json::from_str(args_json).unwrap_or(Value::Null);
        let output = outputs.get(id.as_str()).map(String::as_str).unwrap_or("");
        let is_err = output.starts_with("tool error:");
        let summary_line = first_meaningful_line(output, 120);

        match name.as_str() {
            "write_file" | "replace_text" | "delete_file" => {
                let path = args["path"].as_str().unwrap_or("<unknown>").to_owned();
                let status = if is_err {
                    format!("{name} 失败（{}）", clip_chars(&summary_line, 80))
                } else {
                    format!("{name} 已暂存")
                };
                edits.insert(path, status);
            }
            "rename_file" => {
                let from = args["from"].as_str().unwrap_or("?");
                let to = args["to"].as_str().unwrap_or("?");
                let key = format!("{from} -> {to}");
                let status = if is_err {
                    format!("rename_file 失败（{}）", clip_chars(&summary_line, 80))
                } else {
                    "rename_file 已暂存".to_owned()
                };
                edits.insert(key, status);
            }
            "revert_staged" => {
                if is_err {
                    let target = args["path"].as_str().unwrap_or("all").to_owned();
                    edits.insert(
                        target,
                        format!("revert_staged 失败（{}）", clip_chars(&summary_line, 80)),
                    );
                } else if args["all"] == Value::Bool(true) {
                    edits.clear();
                    edits.insert(
                        "（全部暂存）".to_owned(),
                        "revert_staged 已清空全部暂存，恢复磁盘原状".to_owned(),
                    );
                } else if let Some(path) = args["path"].as_str() {
                    edits.insert(
                        path.to_owned(),
                        "revert_staged 已撤销暂存（恢复磁盘原状）".to_owned(),
                    );
                }
            }
            "staged_diff" => {
                let target = args["path"].as_str().unwrap_or("all");
                let entry = format!("staged_diff:{target}");
                if !explorations.contains(&entry) {
                    explorations.push(entry);
                }
            }
            "run_tests" => {
                let preset = args["preset"]
                    .as_str()
                    .map(|p| format!("[{p}] "))
                    .unwrap_or_default();
                let lines: Vec<&str> = output
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .take(2)
                    .collect();
                let excerpt = clip_chars(&lines.join(" | "), 160);
                tests.push(format!("{preset}{excerpt}"));
            }
            "read_file" => {
                if let Some(path) = args["path"].as_str() {
                    let entry = match (
                        args["offset"].as_i64(),
                        args["end_line"].as_i64(),
                        args["lines"].as_i64(),
                    ) {
                        (Some(o), Some(e), _) => format!("read_file:{path} (L{o}-{e})"),
                        (None, Some(e), _) => format!("read_file:{path} (L1-{e})"),
                        (Some(o), None, Some(l)) => format!("read_file:{path} (L{o}+{l})"),
                        (Some(o), None, None) => format!("read_file:{path} (L{o}-)"),
                        _ => format!("read_file:{path}"),
                    };
                    if !explorations.contains(&entry) {
                        explorations.push(entry);
                    }
                }
            }
            "search_code" | "find_symbols" | "find_references" | "symbol_impact"
            | "search_records" => {
                let q = args["query"]
                    .as_str()
                    .or_else(|| args["name"].as_str())
                    .or_else(|| args["symbol"].as_str())
                    .unwrap_or("");
                let entry = if q.is_empty() {
                    name.clone()
                } else {
                    format!("{name}:`{}`", clip_chars(q, 40))
                };
                if !explorations.contains(&entry) {
                    explorations.push(entry);
                }
            }
            _ => {
                let target = args["path"].as_str().unwrap_or("");
                let entry = if target.is_empty() {
                    name.clone()
                } else {
                    format!("{name}:{target}")
                };
                if !explorations.contains(&entry) {
                    explorations.push(entry);
                }
            }
        }
    }

    let mut sections: Vec<String> = Vec::new();
    if let Some(prev) = previous_digest.map(str::trim).filter(|s| !s.is_empty()) {
        sections.push(format!(
            "【更早归档摘要】\n{}",
            clip_chars(&guard::strip_fences(prev, nonce), 1_200)
        ));
    }
    sections.push(format!(
        "【阶段历史压缩】已折叠 {fold_count} 轮较早对话（共 {total_calls} 次工具调用；如需细节可重新调用只读工具核查）。"
    ));
    if !edits.is_empty() {
        let mut lines = vec!["已暂存／尝试的改动：".to_owned()];
        for (path, status) in &edits {
            lines.push(format!("- {path}: {status}"));
        }
        sections.push(lines.join("\n"));
    }
    if !tests.is_empty() {
        let mut lines = vec!["近期测试／检查结果：".to_owned()];
        let skip = tests.len().saturating_sub(4);
        for item in &tests[skip..] {
            lines.push(format!("- {item}"));
        }
        sections.push(lines.join("\n"));
    }
    if !explorations.is_empty() {
        let shown: Vec<&str> = explorations.iter().take(20).map(String::as_str).collect();
        let tail = if explorations.len() > 20 {
            format!(" 等共 {} 项", explorations.len())
        } else {
            String::new()
        };
        sections.push(format!("已查阅／检索：{}{tail}", shown.join("、")));
    }

    let combined = sections.join("\n");
    clip_chars(&guard::strip_fences(&combined, nonce), MAX_DIGEST_CHARS)
}

/// A request-only projection: history keeps the full outputs so abridging a later
/// request can never leave a persistent pointer to an abridged material copy.
/// Only successful reads executed in this run are eligible; arguments and fenced
/// bytes must both match exactly. Never fold abridged heads of distinct files.
pub(super) fn compact_reads(
    messages: &[Message],
    reads: &[(usize, String)],
    nonce: &str,
) -> Vec<Message> {
    let mut request = messages.to_vec();
    for (position, (index, args)) in reads.iter().enumerate() {
        let Message::ToolOutput { output, .. } = &messages[*index] else {
            continue;
        };
        if guard::unfence(output, nonce).contains("〔节略") {
            continue;
        }
        let later =
            reads[position + 1..]
                .iter()
                .rev()
                .find_map(|(next, next_args)| match &messages[*next] {
                    Message::ToolOutput {
                        call_id,
                        output: next_output,
                    } if args == next_args && output == next_output => Some(call_id),
                    _ => None,
                });
        if let Some(call_id) = later {
            let pointer = guard::fence(
                &format!(
                    "〔重复读取：相同 read_file 参数与完整返回字节；全文保留在后续工具调用 {call_id} 的输出中，请以该份全文为准。〕"
                ),
                nonce,
            );
            if pointer.len() < output.len() {
                let Message::ToolOutput { output, .. } = &mut request[*index] else {
                    unreachable!();
                };
                *output = pointer;
            }
        }
    }
    request
}
