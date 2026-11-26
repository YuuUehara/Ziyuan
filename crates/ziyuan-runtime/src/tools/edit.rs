//! 暂存写工具：`write_file`、`replace_text`、`delete_file`、`rename_file`（均写入内存暂存区，不直接写盘；自审与回退见 `staged`）。

use serde_json::Value;
use std::fs;
use std::sync::Arc;
use ziyuan_core::{Capability, FileChange};

use crate::BoxError;
use crate::agent_loop::{Tool, ToolSpec};

use super::{
    MAX_REPLACE_EDITS, MAX_WRITE_BYTES, Sandbox, clip_str, line_spans, object_args, optional_i64,
    required_string, string_arg,
};

pub(super) struct WriteFile(pub(super) Arc<Sandbox>);

impl Tool for WriteFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Capability::WriteFile.code().into(),
            description: format!(
                "以 content 整体改写（或新建）授权范围（{}）内的文本文件。改动只暂存，诏令完成后呈报皇帝，准奏后才写入项目；暂存后 read_file 读到的是新内容。最多 {MAX_WRITE_BYTES} 字节。",
                self.0.scope_label()
            ),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"相对项目根目录的文件路径"},"content":{"type":"string","description":"文件的完整新内容"}},"required":["path","content"],"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let content = string_arg(args_json, "content")?;
        if content.len() > MAX_WRITE_BYTES {
            return Err(format!("内容超过 {MAX_WRITE_BYTES} 字节").into());
        }
        let (path, key) = self.0.resolve_write(&string_arg(args_json, "path")?)?;
        if path.is_dir() {
            return Err(format!("{key} 是目录").into());
        }
        let mut staged = self.0.staged.lock().expect("暂存区锁未中毒");
        let before = match staged.get(&key) {
            Some(change) => change.before.clone(),
            None if path.exists() => Some(
                String::from_utf8(fs::read(&path)?).map_err(|_| format!("{key} 不是文本文件"))?,
            ),
            None => None,
        };
        let bytes = content.len();
        staged.insert(
            key.clone(),
            FileChange {
                path: key.clone(),
                before,
                after: content,
                deleted: false,
            },
        );
        Ok(format!(
            "已暂存 {key}（{bytes} 字节）；诏令完成后呈报皇帝，准奏后才写入项目"
        ))
    }
}

pub(super) struct ReplaceText(pub(super) Arc<Sandbox>);

struct ReplaceEdit {
    old: String,
    new: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

impl Tool for ReplaceText {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "replace_text".into(),
            description: format!(
                "把授权范围（{}）内某个文本文件里唯一的一段 old 换成 new，或通过 edits 数组在单次调用内按顺序原子应用多处补丁（最多 {MAX_REPLACE_EDITS} 处；任一处找不到或出现不止一处即整批拒绝，不留半截改动）。支持可选的 start_line/end_line（1 起始含首尾行号）将匹配限定在指定行范围以消除歧义；匹配失败时会返回候选行号或歧义行号便于定位。不做模糊替换，也不新建或删除文件。改动只暂存，与 write_file 一样等准奏后才落盘。结果最多 {MAX_WRITE_BYTES} 字节。",
                self.0.scope_label()
            ),
            input_schema_json: format!(
                r#"{{"type":"object","properties":{{"path":{{"type":"string","description":"相对项目根目录的文件路径"}},"old":{{"type":"string","description":"单处替换时的原文，须在文件（或指定行范围）中恰好出现一次（与 edits 二选一）"}},"new":{{"type":"string","description":"单处替换后的文本，可以是空字符串（与 edits 二选一）"}},"start_line":{{"type":"integer","description":"可选，单处替换时限定的起始行号（从 1 起，含该行；须与 old/new 同用）"}},"end_line":{{"type":"integer","description":"可选，单处替换时限定的结束行号（从 1 起，含该行；须与 old/new 同用）"}},"edits":{{"type":"array","description":"批量补丁数组（最多 {MAX_REPLACE_EDITS} 项，与 old/new 二选一），按顺序基于当前文本原子应用","items":{{"type":"object","properties":{{"old":{{"type":"string","description":"该处要替换的原文，须在文件或行范围内唯一匹配"}},"new":{{"type":"string","description":"替换后的文本，可以是空字符串"}},"start_line":{{"type":"integer","description":"可选，该处补丁限定的起始行号（从 1 起，含该行）"}},"end_line":{{"type":"integer","description":"可选，该处补丁限定的结束行号（从 1 起，含该行）"}}}},"required":["old","new"],"additionalProperties":false}}}}}},"required":["path"],"additionalProperties":false}}"#
            ),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(
            args_json,
            &["path", "old", "new", "start_line", "end_line", "edits"],
        )?;
        let relative = required_string(&args, "path")?;
        let has_edits = args.contains_key("edits");
        let has_single = args.contains_key("old")
            || args.contains_key("new")
            || args.contains_key("start_line")
            || args.contains_key("end_line");
        let (batch_mode, edits): (bool, Vec<ReplaceEdit>) = if has_edits {
            if has_single {
                return Err("edits 与 old/new/start_line/end_line 不能同时传入".into());
            }
            let arr = args
                .get("edits")
                .and_then(Value::as_array)
                .ok_or("edits 须为对象数组")?;
            if arr.is_empty() {
                return Err("edits 不能为空".into());
            }
            if arr.len() > MAX_REPLACE_EDITS {
                return Err(format!("edits 最多支持 {MAX_REPLACE_EDITS} 处补丁").into());
            }
            let mut parsed = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let obj = item
                    .as_object()
                    .ok_or_else(|| format!("第 {} 处补丁须为 JSON 对象", i + 1))?;
                for key in obj.keys() {
                    if !matches!(key.as_str(), "old" | "new" | "start_line" | "end_line") {
                        return Err(format!("第 {} 处补丁含未知字段：{key}", i + 1).into());
                    }
                }
                let old = obj
                    .get("old")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("第 {} 处补丁缺少字符串参数 old", i + 1))?;
                let new = obj
                    .get("new")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("第 {} 处补丁缺少字符串参数 new", i + 1))?;
                let start_line = parse_optional_line(obj, "start_line")
                    .map_err(|e| format!("第 {} 处补丁：{e}", i + 1))?;
                let end_line = parse_optional_line(obj, "end_line")
                    .map_err(|e| format!("第 {} 处补丁：{e}", i + 1))?;
                parsed.push(ReplaceEdit {
                    old: old.to_owned(),
                    new: new.to_owned(),
                    start_line,
                    end_line,
                });
            }
            (true, parsed)
        } else {
            let old = required_string(&args, "old")?;
            let new = required_string(&args, "new")?;
            let start_line = parse_optional_line(&args, "start_line")?;
            let end_line = parse_optional_line(&args, "end_line")?;
            (
                false,
                vec![ReplaceEdit {
                    old,
                    new,
                    start_line,
                    end_line,
                }],
            )
        };
        // 路径先过玉玺和禁区。空 old、新旧相同都不能让越界路径报成功。
        let (path, key) = self.0.resolve_write(&relative)?;
        if path.is_dir() {
            return Err(format!("{key} 是目录").into());
        }
        for (i, edit) in edits.iter().enumerate() {
            if edit.old.is_empty() {
                return Err(if batch_mode {
                    format!("第 {} 处补丁的 old 不能为空", i + 1).into()
                } else {
                    "old 不能为空".into()
                });
            }
            if let (Some(start), Some(end)) = (edit.start_line, edit.end_line)
                && start > end
            {
                let msg = format!("start_line ({start}) 不能大于 end_line ({end})");
                return Err(if batch_mode {
                    format!("第 {} 处补丁：{msg}", i + 1).into()
                } else {
                    msg.into()
                });
            }
        }
        if !batch_mode
            && edits[0].start_line.is_none()
            && edits[0].end_line.is_none()
            && edits[0].old == edits[0].new
        {
            return Ok("新旧文本相同，未写入暂存".into());
        }
        let mut staged = self.0.staged.lock().expect("暂存区锁未中毒");
        let (before, current) = if let Some(change) = staged.get(&key) {
            if change.deleted {
                return Err(format!("{key} 已被暂存删除，不能替换").into());
            }
            (change.before.clone(), change.after.clone())
        } else if path.exists() {
            let text =
                String::from_utf8(fs::read(&path)?).map_err(|_| format!("{key} 不是文本文件"))?;
            (Some(text.clone()), text)
        } else {
            return Err(format!("{key} 不存在，不能替换").into());
        };
        let mut working = current.clone();
        let mut old_bytes = 0usize;
        let mut new_bytes = 0usize;
        for (i, edit) in edits.iter().enumerate() {
            let spans = line_spans(&working);
            let win = resolve_line_window(&spans, working.len(), edit.start_line, edit.end_line)
                .map_err(|e| {
                    if batch_mode {
                        BoxError::from(format!("第 {} 处补丁：{e}", i + 1))
                    } else {
                        e
                    }
                })?;
            let window = &working[win.start..win.end];
            let mut effective_old = edit.old.clone();
            let mut effective_new = edit.new.clone();
            let mut matches: Vec<usize> = window
                .match_indices(&effective_old)
                .map(|(pos, _)| pos)
                .collect();
            if matches.is_empty()
                && window.contains("\r\n")
                && edit.old.contains('\n')
                && !edit.old.contains("\r\n")
            {
                let crlf_old = edit.old.replace('\n', "\r\n");
                let crlf_matches: Vec<usize> = window
                    .match_indices(&crlf_old)
                    .map(|(pos, _)| pos)
                    .collect();
                if !crlf_matches.is_empty() {
                    matches = crlf_matches;
                    effective_old = crlf_old;
                    effective_new = edit.new.replace("\r\n", "\n").replace('\n', "\r\n");
                }
            }
            if matches.is_empty() {
                let detail = format_missing_match(
                    &working, &spans, win.start, win.end, &edit.old, win.range,
                );
                return Err(if batch_mode {
                    format!("第 {} 处补丁：{detail}", i + 1).into()
                } else {
                    detail.into()
                });
            }
            if matches.len() > 1 {
                let detail = format_ambiguous_matches(&spans, win.start, &matches, win.range);
                return Err(if batch_mode {
                    format!("第 {} 处补丁：{detail}", i + 1).into()
                } else {
                    detail.into()
                });
            }
            let abs_start = win.start + matches[0];
            let abs_end = abs_start + effective_old.len();
            working.replace_range(abs_start..abs_end, &effective_new);
            old_bytes += effective_old.len();
            new_bytes += effective_new.len();
        }
        if working.len() > MAX_WRITE_BYTES {
            return Err(format!("替换后超过 {MAX_WRITE_BYTES} 字节").into());
        }
        if working == current {
            return Ok("新旧文本相同，未写入暂存".into());
        }
        staged.insert(
            key.clone(),
            FileChange {
                path: key.clone(),
                before,
                after: working,
                deleted: false,
            },
        );
        Ok(format!(
            "已暂存 {key}：替换 {} 处（{old_bytes} 字节 → {new_bytes} 字节）；诏令完成后呈报皇帝，准奏后才写入项目",
            edits.len(),
        ))
    }
}

fn parse_optional_line(
    args: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<usize>, BoxError> {
    match optional_i64(args, key)? {
        None => Ok(None),
        Some(n) if n < 1 => Err(format!("`{key}` 从 1 起").into()),
        Some(n) => usize::try_from(n)
            .map(Some)
            .map_err(|_| format!("`{key}` 过大").into()),
    }
}

struct LineWindow {
    start: usize,
    end: usize,
    range: Option<(usize, usize)>,
}

fn resolve_line_window(
    spans: &[(usize, usize)],
    text_len: usize,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<LineWindow, BoxError> {
    if start_line.is_none() && end_line.is_none() {
        return Ok(LineWindow {
            start: 0,
            end: text_len,
            range: None,
        });
    }
    if spans.is_empty() {
        return Err("文件为空，无法按行范围替换".into());
    }
    let total = spans.len();
    let start = start_line.unwrap_or(1);
    let end = end_line.unwrap_or(total);
    if start > end {
        return Err(format!("start_line ({start}) 不能大于 end_line ({end})").into());
    }
    if start > total || end > total {
        return Err(format!("行范围 {start}..={end} 超出文件总行数 {total}").into());
    }
    Ok(LineWindow {
        start: spans[start - 1].0,
        end: spans[end - 1].1,
        range: Some((start, end)),
    })
}

fn byte_offset_to_line(spans: &[(usize, usize)], offset: usize) -> usize {
    if spans.is_empty() {
        return 1;
    }
    (spans.partition_point(|&(_, end)| end <= offset) + 1).min(spans.len())
}

fn format_line_list(lines: &[usize]) -> String {
    let preview: Vec<String> = lines.iter().take(5).map(usize::to_string).collect();
    if lines.len() > 5 {
        format!("第 {} 行等共 {} 处", preview.join("、"), lines.len())
    } else {
        format!("第 {} 行", preview.join("、"))
    }
}

fn format_ambiguous_matches(
    spans: &[(usize, usize)],
    win_start: usize,
    matches: &[usize],
    range: Option<(usize, usize)>,
) -> String {
    let lines: Vec<usize> = matches
        .iter()
        .map(|&rel| byte_offset_to_line(spans, win_start + rel))
        .collect();
    let count = matches.len();
    let lines_desc = format_line_list(&lines);
    match range {
        Some((start, end)) => format!(
            "在指定行范围 {start}..={end} 内匹配到 {count} 处（{lines_desc}），须唯一才能替换；请缩小 start_line/end_line 或补充前后上下文"
        ),
        None => format!(
            "匹配到 {count} 处（{lines_desc}），须唯一才能替换；可补充前后上下文，或通过 start_line/end_line 指定行范围唯一锚定"
        ),
    }
}

fn format_missing_match(
    working: &str,
    spans: &[(usize, usize)],
    win_start: usize,
    win_end: usize,
    old: &str,
    range: Option<(usize, usize)>,
) -> String {
    if let Some((start, end)) = range {
        let mut anywhere: Vec<usize> = working
            .match_indices(old)
            .map(|(pos, _)| byte_offset_to_line(spans, pos))
            .collect();
        if anywhere.is_empty()
            && working.contains("\r\n")
            && old.contains('\n')
            && !old.contains("\r\n")
        {
            let crlf_old = old.replace('\n', "\r\n");
            anywhere = working
                .match_indices(&crlf_old)
                .map(|(pos, _)| byte_offset_to_line(spans, pos))
                .collect();
        }
        if !anywhere.is_empty() {
            return format!(
                "在指定行范围 {start}..={end} 内找不到要替换的文本（但文件其他位置{}存在完全匹配，请核对行号）",
                format_line_list(&anywhere)
            );
        }
    }
    let base = match range {
        Some((start, end)) => format!("在指定行范围 {start}..={end} 内找不到要替换的文本"),
        None => "找不到要替换的文本".to_owned(),
    };
    let candidates = find_candidate_lines(working, spans, win_start, win_end, old);
    if candidates.is_empty() {
        return base;
    }
    let formatted: Vec<String> = candidates
        .into_iter()
        .map(|(line_no, snippet)| format!("  L{line_no}: {snippet}"))
        .collect();
    format!("{base}。最接近的候选行：\n{}", formatted.join("\n"))
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extract_tokens(text: &str) -> Vec<&str> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|tok| tok.len() >= 2)
        .collect()
}

fn find_candidate_lines(
    working: &str,
    spans: &[(usize, usize)],
    win_start: usize,
    win_end: usize,
    old: &str,
) -> Vec<(usize, String)> {
    let anchor = old
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_else(|| old.trim());
    if anchor.is_empty() {
        return Vec::new();
    }
    let anchor_ws = collapse_whitespace(anchor);
    let anchor_tokens = extract_tokens(anchor);
    let mut scored: Vec<(u32, usize, String)> = Vec::new();
    for (idx, &(s, e)) in spans.iter().enumerate() {
        if e <= win_start || s >= win_end {
            continue;
        }
        let raw = working[s..e].trim_end_matches(['\r', '\n']);
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let score = if trimmed == anchor {
            1000
        } else if !anchor_ws.is_empty() && collapse_whitespace(trimmed) == anchor_ws {
            850
        } else if (anchor.len() >= 4 && trimmed.contains(anchor))
            || (trimmed.len() >= 4 && anchor.contains(trimmed))
        {
            650
        } else if anchor_tokens.len() >= 2 {
            let line_tokens = extract_tokens(trimmed);
            let matched = anchor_tokens
                .iter()
                .filter(|tok| line_tokens.contains(tok))
                .count();
            if matched >= 2 && matched * 2 >= anchor_tokens.len() {
                200 + ((matched as u32 * 300) / anchor_tokens.len() as u32)
            } else {
                0
            }
        } else {
            0
        };
        if score > 0 {
            let preview = clip_str(raw, 100);
            let suffix = if preview.len() < raw.len() { "…" } else { "" };
            scored.push((score, idx + 1, format!("{preview}{suffix}")));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(3)
        .map(|(_, line_no, preview)| (line_no, preview))
        .collect()
}

pub(super) struct DeleteFile(pub(super) Arc<Sandbox>);

impl Tool for DeleteFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "delete_file".into(),
            description: format!(
                "暂存删除授权范围（{}）内的单个文本文件（不能删目录、二进制文件或超过 {MAX_WRITE_BYTES} 字节的文件）。改动只暂存，诏令完成后呈报皇帝，准奏后才从项目删除。",
                self.0.scope_label()
            ),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"相对项目根目录的文件路径"}},"required":["path"],"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(args_json, &["path"])?;
        let relative = required_string(&args, "path")?;
        let (path, key) = self.0.resolve_write(&relative)?;
        if path.is_dir() {
            return Err(format!("{key} 是目录，不能删除").into());
        }
        let mut staged = self.0.staged.lock().expect("暂存区锁未中毒");
        if let Some(existing) = staged.get(&key) {
            if existing.deleted {
                return Ok(format!("{key} 已在暂存中标记删除"));
            }
            if existing.before.is_none() {
                staged.remove(&key);
                return Ok(format!("已取消暂存新建的 {key}"));
            }
            let before = existing.before.clone();
            staged.insert(
                key.clone(),
                FileChange {
                    path: key.clone(),
                    before,
                    after: String::new(),
                    deleted: true,
                },
            );
            return Ok(format!(
                "已暂存删除 {key}；诏令完成后呈报皇帝，准奏后才从项目删除"
            ));
        }
        if !path.exists() {
            return Err(format!("{key} 不存在，不能删除").into());
        }
        let bytes = fs::read(&path)?;
        if bytes.len() > MAX_WRITE_BYTES {
            return Err(format!("{key} 超过 {MAX_WRITE_BYTES} 字节，暂不支持暂存删除").into());
        }
        let text = String::from_utf8(bytes).map_err(|_| format!("{key} 不是文本文件，不能删除"))?;
        staged.insert(
            key.clone(),
            FileChange {
                path: key.clone(),
                before: Some(text),
                after: String::new(),
                deleted: true,
            },
        );
        Ok(format!(
            "已暂存删除 {key}；诏令完成后呈报皇帝，准奏后才从项目删除"
        ))
    }
}

pub(super) struct RenameFile(pub(super) Arc<Sandbox>);

impl Tool for RenameFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "rename_file".into(),
            description: format!(
                "暂存把授权范围（{}）内的单个文本文件从 from 改名或移动到 to（等价于原子暂存删除旧路径并新建目标路径；不允许覆盖已有文件，不支持目录与二进制文件，上限 {MAX_WRITE_BYTES} 字节）。改动只暂存，准奏后才落盘。",
                self.0.scope_label()
            ),
            input_schema_json: r#"{"type":"object","properties":{"from":{"type":"string","description":"原相对文件路径"},"to":{"type":"string","description":"目标相对文件路径"}},"required":["from","to"],"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(args_json, &["from", "to"])?;
        let from_rel = required_string(&args, "from")?;
        let to_rel = required_string(&args, "to")?;
        let (from_path, from_key) = self.0.resolve_write(&from_rel)?;
        let (to_path, to_key) = self.0.resolve_write(&to_rel)?;
        if from_key == to_key {
            return Ok("源路径与目标路径相同，未写入暂存".into());
        }
        if from_path.is_dir() {
            return Err(format!("{from_key} 是目录，不能改名").into());
        }
        if to_path.is_dir() {
            return Err(format!("{to_key} 是目录，不能作为目标路径").into());
        }
        let mut staged = self.0.staged.lock().expect("暂存区锁未中毒");
        let (from_before, content) = if let Some(change) = staged.get(&from_key) {
            if change.deleted {
                return Err(format!("{from_key} 已被暂存删除，不能改名").into());
            }
            (change.before.clone(), change.after.clone())
        } else if from_path.exists() {
            let bytes = fs::read(&from_path)?;
            if bytes.len() > MAX_WRITE_BYTES {
                return Err(
                    format!("{from_key} 超过 {MAX_WRITE_BYTES} 字节，暂不支持暂存改名").into(),
                );
            }
            let text = String::from_utf8(bytes)
                .map_err(|_| format!("{from_key} 不是文本文件，不能改名"))?;
            (Some(text.clone()), text)
        } else {
            return Err(format!("{from_key} 不存在，不能改名").into());
        };
        if content.len() > MAX_WRITE_BYTES {
            return Err(format!("改名内容超过 {MAX_WRITE_BYTES} 字节").into());
        }
        let to_exists = match staged.get(&to_key) {
            Some(c) => !c.deleted,
            None => to_path.exists(),
        };
        if to_exists {
            return Err(format!("{to_key} 已存在，拒绝覆盖改名").into());
        }
        let to_before = staged.get(&to_key).and_then(|c| c.before.clone());
        match from_before {
            Some(orig) => {
                staged.insert(
                    from_key.clone(),
                    FileChange {
                        path: from_key.clone(),
                        before: Some(orig),
                        after: String::new(),
                        deleted: true,
                    },
                );
            }
            None => {
                staged.remove(&from_key);
            }
        }
        if to_before.as_deref() == Some(content.as_str()) {
            staged.remove(&to_key);
        } else {
            staged.insert(
                to_key.clone(),
                FileChange {
                    path: to_key.clone(),
                    before: to_before,
                    after: content,
                    deleted: false,
                },
            );
        }
        Ok(format!(
            "已暂存改名 {from_key} → {to_key}；诏令完成后呈报皇帝，准奏后才落盘"
        ))
    }
}
