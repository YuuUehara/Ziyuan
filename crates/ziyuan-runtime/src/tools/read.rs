//! 只读文件与文本检索工具：`read_file`、`list_files`、`search_code`。

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use ziyuan_core::Capability;

use crate::BoxError;
use crate::agent_loop::{Tool, ToolSpec};

use super::{
    CollectedTextFiles, MAX_CONTEXT_LINES, MAX_LISTED, MAX_MATCHES, MAX_READ_BYTES, Sandbox,
    StagedEntry, clip_str, glob, line_spans, object_args, optional_bool, optional_i64,
    optional_string, required_string,
};

/// 一页搜索结果。默认搜索在凑满一页后再遇到一条就停，以免改掉原来的截断语义。
pub(super) struct MatchPage {
    pub(super) offset: usize,
    pub(super) limit: usize,
    pub(super) stop_when_full: bool,
    pub(super) total: usize,
    pub(super) more: bool,
    pub(super) hits: Vec<String>,
}

impl MatchPage {
    pub(super) fn consider(&mut self, hit: String) -> bool {
        if self.stop_when_full {
            if self.hits.len() == self.limit {
                self.more = true;
                return true;
            }
            self.hits.push(hit);
            self.total += 1;
            return false;
        }
        if self.total >= self.offset && self.hits.len() < self.limit {
            self.hits.push(hit);
        }
        self.total += 1;
        false
    }
}

enum SearchMatcher {
    Literal { query: String, ignore_case: bool },
    Regex(regex::Regex),
}

impl SearchMatcher {
    fn is_match(&self, line: &str) -> bool {
        match self {
            Self::Literal { query, ignore_case } => {
                if *ignore_case {
                    line.to_lowercase().contains(query)
                } else {
                    line.contains(query.as_str())
                }
            }
            Self::Regex(re) => re.is_match(line),
        }
    }
}

pub(super) struct ReadFile(pub(super) Arc<Sandbox>);

impl Tool for ReadFile {
    fn is_read_only(&self) -> bool {
        true
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Capability::ReadFile.code().into(),
            description: format!(
                "读取项目内的文本文件（相对项目根目录的路径）。只能读取授权范围：{}。不给 offset、lines 与 end_line 时从开头读，超过 {MAX_READ_BYTES} 字节截断。给出 offset（从 1 起的行号）、lines（行数）或 end_line（结束行号，含，与 lines 互斥）时按行窗口读取，并说明总行数和后面是否还有。",
                self.0.scope_label()
            ),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"相对项目根目录的文件路径"},"offset":{"type":"integer","description":"从第几行开始读，从 1 起"},"lines":{"type":"integer","description":"最多读几行（与 end_line 互斥）"},"end_line":{"type":"integer","description":"读到第几行（含，从 1 起，与 lines 互斥；可直接使用 file_outline / find_symbols 返回的行号范围）"}},"required":["path"],"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(args_json, &["path", "offset", "lines", "end_line"])?;
        let relative = required_string(&args, "path")?;
        let offset = optional_i64(&args, "offset")?;
        let lines = optional_i64(&args, "lines")?;
        let end_line = optional_i64(&args, "end_line")?;
        if lines.is_some() && end_line.is_some() {
            return Err("lines 与 end_line 不能同时指定".into());
        }
        let effective_lines = match end_line {
            None => lines,
            Some(e) if e < 1 => return Err("end_line 从 1 起".into()),
            Some(e) => {
                let start = offset.unwrap_or(1);
                if start < 1 {
                    return Err("offset 从 1 起".into());
                }
                if e < start {
                    return Err(format!("end_line ({e}) 不能小于 offset ({start})").into());
                }
                let span = e
                    .checked_sub(start)
                    .and_then(|d| d.checked_add(1))
                    .ok_or("行号范围过大")?;
                Some(span)
            }
        };
        let bytes = match self.0.staged_entry(&relative) {
            Some(StagedEntry::Deleted) => return Err(format!("{relative} 已被暂存删除").into()),
            Some(StagedEntry::Content(text)) => text.into_bytes(),
            None => fs::read(self.0.resolve(&relative)?)?,
        };
        if offset.is_none() && effective_lines.is_none() {
            let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_READ_BYTES)]);
            return if bytes.len() > MAX_READ_BYTES {
                Ok(format!("{text}\n[已截断：文件共 {} 字节]", bytes.len()))
            } else {
                Ok(text.into_owned())
            };
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| "不是文本文件，不能按行读取")?;
        read_window(text, offset, effective_lines)
    }
}

pub(super) struct ListFiles(pub(super) Arc<Sandbox>);

impl Tool for ListFiles {
    fn is_read_only(&self) -> bool {
        true
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_files".into(),
            description: format!(
                "列出授权范围（{}）内某目录下的全部文件（递归，含本次 write_file 暂存的新文件），返回 `路径（字节数）`，最多 {MAX_LISTED} 个；不给 path 则列整个授权范围。可用 glob 过滤路径（如 `*.rs`、`crates/**/tests/*.rs`、`*.{{rs,toml}}`）。跳过 target、node_modules 与 Python 缓存目录。",
                self.0.scope_label()
            ),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"相对项目根目录的目录路径；省略则列整个授权范围"},"glob":{"type":"string","description":"可选：路径通配过滤，如 *.rs、crates/**/*.rs、*.{rs,toml}"}},"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(args_json, &["path", "glob"])?;
        let path_arg = optional_string(&args, "path")?;
        let glob_matcher = match optional_string(&args, "glob")? {
            Some(pat) if !pat.trim().is_empty() => Some(glob::GlobMatcher::compile(&pat)?),
            Some(_) => return Err("glob 不能为空".into()),
            None => None,
        };
        let dirs = match path_arg.as_deref() {
            Some(p) if !p.trim().is_empty() && p.trim() != "." => vec![self.0.resolve(p)?],
            _ => self.0.scope.clone(),
        };
        let mut files = BTreeMap::new();
        for dir in &dirs {
            self.0.list(dir, &mut files)?;
        }
        let prefixes: Vec<String> = dirs.iter().map(|d| self.0.display(d)).collect();
        let staged = self.0.staged.lock().expect("暂存区锁未中毒");
        let mut lines: Vec<String> = Vec::new();
        for (path, change) in staged.iter() {
            let under = prefixes
                .iter()
                .any(|p| p == "." || path == p || path.starts_with(&format!("{p}/")));
            if !under {
                continue;
            }
            if change.deleted {
                files.remove(path);
            } else if let Some(size) = files.get_mut(path) {
                *size = change.after.len() as u64;
            } else if glob_matcher.as_ref().is_none_or(|g| g.is_match(path)) {
                lines.push(format!("{path}（暂存新建，{} 字节）", change.after.len()));
            }
        }
        drop(staged);
        if let Some(matcher) = &glob_matcher {
            files.retain(|path, _| matcher.is_match(path));
        }
        let total = files.len() + lines.len();
        lines.extend(
            files
                .iter()
                .map(|(path, size)| format!("{path}（{size} 字节）")),
        );
        lines.sort();
        if lines.is_empty() {
            return Ok("无文件".into());
        }
        lines.truncate(MAX_LISTED);
        let mut out = lines.join("\n");
        if total > MAX_LISTED {
            out.push_str(&format!(
                "\n[已截断：共 {total} 个，仅显示前 {MAX_LISTED} 个；可指定子目录或 glob]"
            ));
        }
        Ok(out)
    }
}

pub(super) struct SearchCode(pub(super) Arc<Sandbox>);

impl Tool for SearchCode {
    fn is_read_only(&self) -> bool {
        true
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Capability::SearchCode.code().into(),
            description: format!(
                "在授权范围（{}）内的文本文件中搜索（含暂存新建／改动，跳过暂存删除），返回 `路径:行号: 内容`。默认区分大小写按字面量查，一页最多 {MAX_MATCHES} 条。可用 path 限定目录或文件、glob 过滤路径（如 `*.rs`、`crates/**/*.rs`）、regex 启用正则搜索、ignore_case 忽略大小写、context_lines（0–{MAX_CONTEXT_LINES}）附带上下邻行、offset 与 limit 翻页。先按授权范围和禁区过滤，不查 .git、.jj、.ziyuan、.env*、target、node_modules 与 Python 缓存。",
                self.0.scope_label()
            ),
            input_schema_json: format!(
                r#"{{"type":"object","properties":{{"query":{{"type":"string","description":"要查找的字面量文本，或 regex=true 时的正则表达式"}},"path":{{"type":"string","description":"只查这个相对目录或文件；省略则查整个授权范围"}},"glob":{{"type":"string","description":"可选：路径通配过滤，如 *.rs、crates/**/*.rs、*.{{rs,toml}}"}},"regex":{{"type":"boolean","description":"为 true 时按 Rust 正则表达式匹配"}},"ignore_case":{{"type":"boolean","description":"为 true 时忽略大小写"}},"context_lines":{{"type":"integer","description":"可选：每条匹配前后附带的上下文行数，0 到 {MAX_CONTEXT_LINES}"}},"offset":{{"type":"integer","description":"跳过前若干条匹配，从 0 起"}},"limit":{{"type":"integer","description":"本页条数，1 到 {MAX_MATCHES}"}}}},"required":["query"],"additionalProperties":false}}"#
            ),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(
            args_json,
            &[
                "query",
                "path",
                "glob",
                "regex",
                "ignore_case",
                "context_lines",
                "offset",
                "limit",
            ],
        )?;
        let query = required_string(&args, "query")?;
        if query.is_empty() {
            return Err("query 不能为空".into());
        }
        let use_regex = optional_bool(&args, "regex")?.unwrap_or(false);
        let ignore_case = optional_bool(&args, "ignore_case")?.unwrap_or(false);
        let context_lines = match optional_i64(&args, "context_lines")? {
            None => 0usize,
            Some(n) if (0..=MAX_CONTEXT_LINES as i64).contains(&n) => n as usize,
            Some(_) => {
                return Err(format!("context_lines 须在 0 到 {MAX_CONTEXT_LINES} 之间").into());
            }
        };
        let offset = optional_i64(&args, "offset")?.unwrap_or(0);
        if offset < 0 {
            return Err("offset 不能为负".into());
        }
        let limit = match optional_i64(&args, "limit")? {
            None => MAX_MATCHES as i64,
            Some(n) if (1..=MAX_MATCHES as i64).contains(&n) => n,
            Some(_) => return Err(format!("limit 须在 1 到 {MAX_MATCHES} 之间").into()),
        };
        let path = optional_string(&args, "path")?;
        let glob_matcher = match optional_string(&args, "glob")? {
            Some(pat) if !pat.trim().is_empty() => Some(glob::GlobMatcher::compile(&pat)?),
            Some(_) => return Err("glob 不能为空".into()),
            None => None,
        };
        let matcher = if use_regex {
            let compiled = regex::RegexBuilder::new(&query)
                .case_insensitive(ignore_case)
                .size_limit(1 << 20)
                .build()
                .map_err(|e| format!("正则表达式无效：{e}"))?;
            SearchMatcher::Regex(compiled)
        } else {
            SearchMatcher::Literal {
                query: if ignore_case {
                    query.to_lowercase()
                } else {
                    query
                },
                ignore_case,
            }
        };
        let enhanced = path.is_some()
            || glob_matcher.is_some()
            || use_regex
            || ignore_case
            || context_lines > 0
            || args.contains_key("offset")
            || args.contains_key("limit");
        let mut page = MatchPage {
            offset: usize::try_from(offset).unwrap_or(usize::MAX),
            limit: usize::try_from(limit).unwrap_or(MAX_MATCHES),
            stop_when_full: !enhanced,
            total: 0,
            more: false,
            hits: Vec::new(),
        };
        let CollectedTextFiles {
            files,
            staged: staged_snapshot,
            ..
        } = self.0.collect_text_files(path.as_deref())?;
        'outer: for (relative, disk_path) in files {
            if let Some(g) = &glob_matcher
                && !g.is_match(&relative)
            {
                continue;
            }
            let text = if let Some(entry) = staged_snapshot.get(&relative) {
                let Some(staged) = entry else {
                    continue;
                };
                staged.clone()
            } else if let Some(disk) = disk_path {
                match fs::read_to_string(&disk) {
                    Ok(text) => text,
                    Err(_) => continue,
                }
            } else {
                continue;
            };
            let lines: Vec<&str> = text.lines().collect();
            for (number, line) in lines.iter().enumerate() {
                if !matcher.is_match(line) {
                    continue;
                }
                let hit = if context_lines == 0 {
                    format!("{relative}:{}: {}", number + 1, line.trim())
                } else {
                    let from = number.saturating_sub(context_lines);
                    let to = (number + context_lines).min(lines.len().saturating_sub(1));
                    let mut block = Vec::with_capacity(to - from + 1);
                    for (idx, ctx_line) in lines.iter().enumerate().take(to + 1).skip(from) {
                        let sep = if idx == number { ':' } else { '-' };
                        block.push(format!(
                            "{relative}:{}{sep} {}",
                            idx + 1,
                            ctx_line.trim_end()
                        ));
                    }
                    block.join("\n")
                };
                if page.consider(hit) {
                    break 'outer;
                }
            }
        }
        Ok(format_matches(&page, enhanced, context_lines > 0))
    }
}

fn read_window(text: &str, offset: Option<i64>, lines: Option<i64>) -> Result<String, BoxError> {
    let start = match offset.unwrap_or(1) {
        n if n < 1 => return Err("offset 从 1 起".into()),
        n => usize::try_from(n).map_err(|_| "offset 过大")?,
    };
    let want = match lines {
        None => None,
        Some(n) if n < 1 => return Err("lines 至少为 1".into()),
        Some(n) => Some(usize::try_from(n).map_err(|_| "lines 过大")?),
    };
    let spans = line_spans(text);
    let total = spans.len();
    if total == 0 {
        return Ok("〔共 0 行〕".into());
    }
    if start > total {
        return Err(format!("行号超出范围：文件共 {total} 行").into());
    }
    let mut body = String::new();
    let mut end = start - 1;
    let mut byte_cut = false;
    let last = want.map(|n| (start + n - 1).min(total)).unwrap_or(total);
    for (number, &(from, to)) in spans
        .iter()
        .enumerate()
        .skip(start - 1)
        .take(last - (start - 1))
    {
        let piece = &text[from..to];
        if body.len() + piece.len() > MAX_READ_BYTES {
            let room = MAX_READ_BYTES.saturating_sub(body.len());
            if room == 0 {
                break;
            }
            body.push_str(clip_str(piece, room));
            end = number + 1;
            byte_cut = true;
            break;
        }
        body.push_str(piece);
        end = number + 1;
    }
    let more = byte_cut || end < total;
    let tail = if byte_cut {
        format!("已按 {MAX_READ_BYTES} 字节截断，后面还有")
    } else if more {
        "后面还有".into()
    } else {
        "已到末尾".into()
    };
    Ok(format!(
        "〔第 {start}–{end} 行，共 {total} 行；{tail}〕\n{body}"
    ))
}

fn format_matches(page: &MatchPage, enhanced: bool, with_context: bool) -> String {
    if page.hits.is_empty() {
        return if enhanced && page.offset > 0 && page.total > 0 {
            format!("无匹配（共 {} 条，偏移已越过末尾）", page.total)
        } else {
            "无匹配".into()
        };
    }
    let mut out = String::new();
    if enhanced {
        let from = page.offset + 1;
        let to = page.offset + page.hits.len();
        let tail = if page.offset + page.hits.len() < page.total {
            "；后面还有"
        } else {
            ""
        };
        out.push_str(&format!(
            "〔第 {from}–{to} 条，共 {} 条{tail}〕\n",
            page.total
        ));
    }
    let sep = if with_context { "\n---\n" } else { "\n" };
    out.push_str(&page.hits.join(sep));
    if !enhanced && page.more {
        out.push_str(&format!("\n[已截断：仅显示前 {MAX_MATCHES} 条]"));
    }
    out
}
