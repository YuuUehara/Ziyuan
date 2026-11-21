//! 代码结构与符号导航工具：`file_outline`、`find_symbols`、`find_references`、`symbol_impact`。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::BoxError;
use crate::agent_loop::{Tool, ToolSpec};

use super::{
    CollectedTextFiles, DEFAULT_IMPACT_CALL_SITES, DEFAULT_OUTLINE_SYMBOLS, MAX_IMPACT_CALL_SITES,
    MAX_IMPACT_CONTEXT_LINES, MAX_IMPACT_DEFS, MAX_OUTLINE_BYTES, MAX_OUTLINE_SYMBOLS,
    MAX_REF_MATCHES, MAX_SYMBOL_FILE_BYTES, MAX_SYMBOL_MATCHES, Sandbox, StagedEntry, glob, index,
    object_args, optional_bool, optional_i64, optional_string, read::MatchPage, required_string,
    symbols, under_skipped,
};

fn is_reference_candidate(rel: &str, explicit_target: bool) -> bool {
    if explicit_target {
        return true;
    }
    if let Some(lang) = symbols::Language::from_path(Path::new(rel)) {
        return lang != symbols::Language::Markdown;
    }
    let Some(ext) = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
    else {
        return false;
    };
    matches!(
        ext.as_str(),
        "java"
            | "kt"
            | "kts"
            | "cs"
            | "swift"
            | "rb"
            | "php"
            | "scala"
            | "zig"
            | "lua"
            | "sh"
            | "bash"
            | "zsh"
            | "ps1"
            | "sql"
            | "vue"
            | "svelte"
            | "css"
            | "scss"
            | "html"
            | "xml"
            | "toml"
            | "yaml"
            | "yml"
    )
}

pub(super) struct FileOutline(pub(super) Arc<Sandbox>);

impl Tool for FileOutline {
    fn is_read_only(&self) -> bool {
        true
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "file_outline".into(),
            description: format!(
                "列出授权范围（{}）内单个代码或 Markdown 文件的符号大纲（函数、结构体、枚举、trait、impl、类、接口、常量、标题及起止行号，如 `42-98`）。支持 Rust、JS/TS、Python、Go、C/C++、Markdown；暂存改动优先于磁盘。可用 kind 按类别过滤，offset 与 limit 翻页（默认 {DEFAULT_OUTLINE_SYMBOLS} 条，最多 {MAX_OUTLINE_SYMBOLS} 条）。",
                self.0.scope_label()
            ),
            input_schema_json: format!(
                r#"{{"type":"object","properties":{{"path":{{"type":"string","description":"相对项目根目录的文件路径"}},"kind":{{"type":"string","description":"可选：只看某类符号，如 fn、struct、enum、trait、impl、class、interface、type、const、mod、macro、heading"}},"offset":{{"type":"integer","description":"跳过前若干个符号，从 0 起"}},"limit":{{"type":"integer","description":"本页条数，1 到 {MAX_OUTLINE_SYMBOLS}"}}}},"required":["path"],"additionalProperties":false}}"#
            ),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(args_json, &["path", "kind", "offset", "limit"])?;
        let relative = required_string(&args, "path")?;
        let kind = optional_string(&args, "kind")?
            .map(|k| k.trim().to_owned())
            .filter(|k| !k.is_empty());
        let offset = optional_i64(&args, "offset")?.unwrap_or(0);
        if offset < 0 {
            return Err("offset 不能为负".into());
        }
        let limit = match optional_i64(&args, "limit")? {
            None => DEFAULT_OUTLINE_SYMBOLS as i64,
            Some(n) if (1..=MAX_OUTLINE_SYMBOLS as i64).contains(&n) => n,
            Some(_) => return Err(format!("limit 须在 1 到 {MAX_OUTLINE_SYMBOLS} 之间").into()),
        };
        let (display_path, lang, all) = match self.0.staged_entry(&relative) {
            Some(StagedEntry::Deleted) => return Err(format!("{relative} 已被暂存删除").into()),
            Some(StagedEntry::Content(staged)) => {
                let (_, key) = self.0.resolve_write(&relative)?;
                let Some((lang, all)) = symbols::extract_for_path(Path::new(&key), &staged) else {
                    return Ok(format!(
                        "{key}：暂不支持该文件类型的符号大纲（支持 Rust、JS/TS、Python、Go、C/C++、Markdown），可用 read_file 查看"
                    ));
                };
                (key, lang, all)
            }
            None => {
                let resolved = self.0.resolve(&relative)?;
                if resolved.is_dir() {
                    return Err(
                        format!("{relative} 是目录，请指定单个文件或使用 find_symbols").into(),
                    );
                }
                let meta = fs::metadata(&resolved)?;
                let len = meta.len();
                if len > MAX_OUTLINE_BYTES as u64 {
                    return Err(format!("文件过大（{len} 字节），请改用 read_file 按行读取").into());
                }
                let display_path = self.0.display(&resolved);
                let stamp = index::FileStamp::from_metadata(&meta);
                let lang = symbols::Language::from_path(Path::new(&display_path));
                if let (Some(lang), Some(stamp)) = (lang, stamp) {
                    let cached = {
                        let mut idx = self.0.symbol_index.lock().expect("符号索引锁未中毒");
                        idx.get_symbols(&display_path, stamp)
                    };
                    if let Some(syms) = cached {
                        (display_path, lang, syms)
                    } else {
                        let bytes = fs::read(&resolved)?;
                        let text = String::from_utf8(bytes)
                            .map_err(|_| "不是 UTF-8 文本文件，无法提取符号大纲")?;
                        let (_, all) = symbols::extract_for_path(Path::new(&display_path), &text)
                            .expect("已确认受支持语种");
                        let mut idx = self.0.symbol_index.lock().expect("符号索引锁未中毒");
                        idx.put_symbols(display_path.clone(), stamp, &all);
                        idx.flush_if_dirty();
                        (display_path, lang, all)
                    }
                } else {
                    let bytes = fs::read(&resolved)?;
                    let _ = String::from_utf8(bytes)
                        .map_err(|_| "不是 UTF-8 文本文件，无法提取符号大纲")?;
                    return Ok(format!(
                        "{display_path}：暂不支持该文件类型的符号大纲（支持 Rust、JS/TS、Python、Go、C/C++、Markdown），可用 read_file 查看"
                    ));
                }
            }
        };
        let filtered: Vec<_> = match kind.as_deref() {
            Some(filter) => all
                .into_iter()
                .filter(|sym| symbols::kind_matches(sym.kind, filter))
                .collect(),
            None => all,
        };
        if filtered.is_empty() {
            return Ok(format!(
                "{display_path}（{}）：未识别到匹配的符号",
                lang.label()
            ));
        }
        let total = filtered.len();
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(total);
        let lim = usize::try_from(limit).unwrap_or(DEFAULT_OUTLINE_SYMBOLS);
        let slice = &filtered[start..(start + lim).min(total)];
        if slice.is_empty() {
            return Ok(format!(
                "{display_path}（{}）：共 {total} 个符号，offset={start} 已超出范围",
                lang.label()
            ));
        }
        let mut out = format!(
            "{display_path}（{}，共 {total} 个符号，显示第 {}–{} 个）
",
            lang.label(),
            start + 1,
            start + slice.len()
        );
        for sym in slice {
            let indent = "  ".repeat(sym.depth.min(4));
            out.push_str(&format!(
                "{}: {indent}[{}] {} — {}
",
                sym.line_span(),
                sym.kind,
                sym.name,
                sym.signature
            ));
        }
        if start + slice.len() < total {
            out.push_str(&format!(
                "[还有 {} 个符号，可用 offset={} 翻页]",
                total - (start + slice.len()),
                start + slice.len()
            ));
        } else {
            out.pop();
        }
        Ok(out)
    }
}

pub(super) struct FindSymbols(pub(super) Arc<Sandbox>);

impl Tool for FindSymbols {
    fn is_read_only(&self) -> bool {
        true
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "find_symbols".into(),
            description: format!(
                "在授权范围（{}）内的代码与 Markdown 文件中按符号名查找定义（函数、结构体、枚举、trait、impl、类、接口、常量、标题），返回 `路径:起止行号: [类别] 限定名 — 签名`（单行只写行号，跨行写 `起-止`）。只匹配符号定义而非注释或普通引用；暂存改动优先于磁盘。可用 path 限目录或文件、kind 限类别、exact 精确匹配、ignore_case 忽略大小写、offset 与 limit 翻页（一页最多 {MAX_SYMBOL_MATCHES} 条）。先按授权范围和禁区过滤，跳过 target 与 node_modules。",
                self.0.scope_label()
            ),
            input_schema_json: format!(
                r#"{{"type":"object","properties":{{"query":{{"type":"string","description":"要查找的符号名或子串"}},"path":{{"type":"string","description":"可选：只查这个相对目录或文件；省略则查整个授权范围"}},"kind":{{"type":"string","description":"可选：只看某类符号，如 fn、struct、enum、trait、impl、class、interface、type、const、mod、macro、heading"}},"exact":{{"type":"boolean","description":"为 true 时要求符号短名完全一致"}},"ignore_case":{{"type":"boolean","description":"为 true 时忽略大小写"}},"offset":{{"type":"integer","description":"跳过前若干条匹配，从 0 起"}},"limit":{{"type":"integer","description":"本页条数，1 到 {MAX_SYMBOL_MATCHES}"}}}},"required":["query"],"additionalProperties":false}}"#
            ),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(
            args_json,
            &[
                "query",
                "path",
                "kind",
                "exact",
                "ignore_case",
                "offset",
                "limit",
            ],
        )?;
        let query = required_string(&args, "query")?;
        if query.trim().is_empty() {
            return Err("query 不能为空".into());
        }
        let kind = optional_string(&args, "kind")?
            .map(|k| k.trim().to_owned())
            .filter(|k| !k.is_empty());
        let exact = optional_bool(&args, "exact")?.unwrap_or(false);
        let ignore_case = optional_bool(&args, "ignore_case")?.unwrap_or(false);
        let offset = optional_i64(&args, "offset")?.unwrap_or(0);
        if offset < 0 {
            return Err("offset 不能为负".into());
        }
        let limit = match optional_i64(&args, "limit")? {
            None => MAX_SYMBOL_MATCHES as i64,
            Some(n) if (1..=MAX_SYMBOL_MATCHES as i64).contains(&n) => n,
            Some(_) => return Err(format!("limit 须在 1 到 {MAX_SYMBOL_MATCHES} 之间").into()),
        };
        let path = optional_string(&args, "path")?;
        let roots = if let Some(relative) = path.as_deref() {
            if relative.trim().is_empty() || relative.trim() == "." {
                self.0.scope.clone()
            } else {
                let start = self.0.resolve(relative)?;
                if under_skipped(&self.0.root, &start) {
                    return Err(format!("不搜索构建产物: {relative}").into());
                }
                vec![start]
            }
        } else {
            self.0.scope.clone()
        };

        // 收集范围内全部候选文件（磁盘 + 暂存新建），按相对路径有序遍历。
        let mut files: BTreeMap<String, Option<PathBuf>> = BTreeMap::new();
        for root in &roots {
            self.0.collect_symbol_files(root, &mut files)?;
        }
        let prefixes: Vec<String> = roots.iter().map(|d| self.0.display(d)).collect();
        let staged_snapshot: BTreeMap<String, Option<String>> = {
            let staged = self.0.staged.lock().expect("暂存区锁未中毒");
            staged
                .iter()
                .map(|(k, v)| (k.clone(), (!v.deleted).then(|| v.after.clone())))
                .collect()
        };
        for (rel, content) in &staged_snapshot {
            let under = prefixes
                .iter()
                .any(|p| p == "." || rel == p || rel.starts_with(&format!("{p}/")));
            if !under {
                continue;
            }
            if content.is_some() {
                files.entry(rel.clone()).or_insert(None);
            } else {
                files.remove(rel);
            }
        }

        let mut page = MatchPage {
            offset: usize::try_from(offset).unwrap_or(usize::MAX),
            limit: usize::try_from(limit).unwrap_or(MAX_SYMBOL_MATCHES),
            stop_when_full: false,
            total: 0,
            more: false,
            hits: Vec::new(),
        };
        let q_lower = query.to_lowercase();

        for (rel, disk_path) in files {
            if symbols::Language::from_path(Path::new(&rel)).is_none() {
                continue;
            }
            let syms = if let Some(entry) = staged_snapshot.get(&rel) {
                let Some(staged) = entry else {
                    continue;
                };
                let Some((_, syms)) = symbols::extract_for_path(Path::new(&rel), staged) else {
                    continue;
                };
                syms
            } else if let Some(disk) = disk_path {
                let Ok(meta) = fs::metadata(&disk) else {
                    continue;
                };
                if meta.len() > MAX_SYMBOL_FILE_BYTES {
                    continue;
                }
                let stamp = index::FileStamp::from_metadata(&meta);
                let cached = stamp.and_then(|s| {
                    let mut idx = self.0.symbol_index.lock().expect("符号索引锁未中毒");
                    idx.get_symbols(&rel, s)
                });
                if let Some(syms) = cached {
                    syms
                } else {
                    let Ok(text) = fs::read_to_string(&disk) else {
                        continue;
                    };
                    let Some((_, syms)) = symbols::extract_for_path(Path::new(&rel), &text) else {
                        continue;
                    };
                    if let Some(s) = stamp {
                        let mut idx = self.0.symbol_index.lock().expect("符号索引锁未中毒");
                        idx.put_symbols(rel.clone(), s, &syms);
                    }
                    syms
                }
            } else {
                continue;
            };
            for sym in syms {
                if let Some(filter) = kind.as_deref()
                    && !symbols::kind_matches(sym.kind, filter)
                {
                    continue;
                }
                let qualified = sym.qualified_name();
                let matched = if exact {
                    if ignore_case {
                        sym.name.to_lowercase() == q_lower
                    } else {
                        sym.name == query
                    }
                } else if ignore_case {
                    sym.name.to_lowercase().contains(&q_lower)
                        || qualified.to_lowercase().contains(&q_lower)
                } else {
                    sym.name.contains(&query) || qualified.contains(&query)
                };
                if matched {
                    page.consider(format!(
                        "{rel}:{}: [{}] {} — {}",
                        sym.line_span(),
                        sym.kind,
                        qualified,
                        sym.signature
                    ));
                }
            }
        }

        {
            let mut idx = self.0.symbol_index.lock().expect("符号索引锁未中毒");
            idx.flush_if_dirty();
        }
        if page.total == 0 {
            return Ok("未找到匹配的符号".into());
        }
        if page.hits.is_empty() {
            return Ok(format!(
                "共 {} 个符号，offset={} 已超出范围",
                page.total, page.offset
            ));
        }
        let from = page.offset + 1;
        let to = page.offset + page.hits.len();
        let mut out = page.hits.join("\n");
        if page.total > page.hits.len() || page.offset > 0 {
            out.push_str(&format!(
                "\n[共 {} 个符号，当前第 {from}–{to} 个]",
                page.total
            ));
        }
        Ok(out)
    }
}

pub(super) struct FindReferences(pub(super) Arc<Sandbox>);

impl Tool for FindReferences {
    fn is_read_only(&self) -> bool {
        true
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "find_references".into(),
            description: format!(
                "在授权范围（{}）内的源码文件中按整词查找符号的调用点与引用（跳过注释、字符串与更长标识符的子串；暂存改动优先于磁盘），区分 `[定义]` 与 `[引用]`。可用 path 限目录或文件、glob 过滤路径（如 `*.rs`）、include_definitions 控制是否包含定义行（默认 true）、offset 与 limit 翻页（一页最多 {MAX_REF_MATCHES} 条）。",
                self.0.scope_label()
            ),
            input_schema_json: format!(
                r#"{{"type":"object","properties":{{"symbol":{{"type":"string","description":"要查找的完整符号标识符（如 parse_test_presets、Seal）"}},"path":{{"type":"string","description":"可选：只查这个相对目录或文件；省略则查整个授权范围"}},"glob":{{"type":"string","description":"可选：路径通配过滤，如 *.rs、crates/**/*.rs"}},"include_definitions":{{"type":"boolean","description":"是否包含符号定义行；默认 true，设为 false 时只列调用／引用点"}},"offset":{{"type":"integer","description":"跳过前若干条结果，从 0 起"}},"limit":{{"type":"integer","description":"本页条数，1 到 {MAX_REF_MATCHES}"}}}},"required":["symbol"],"additionalProperties":false}}"#
            ),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(
            args_json,
            &[
                "symbol",
                "path",
                "glob",
                "include_definitions",
                "offset",
                "limit",
            ],
        )?;
        let symbol = required_string(&args, "symbol")?.trim().to_owned();
        if !symbols::is_valid_symbol_name(&symbol) {
            return Err("symbol 须为 1–120 字节的单个标识符（不含空格或标点）".into());
        }
        let include_definitions = optional_bool(&args, "include_definitions")?.unwrap_or(true);
        let offset = optional_i64(&args, "offset")?.unwrap_or(0);
        if offset < 0 {
            return Err("offset 不能为负".into());
        }
        let limit = match optional_i64(&args, "limit")? {
            None => MAX_REF_MATCHES as i64,
            Some(n) if (1..=MAX_REF_MATCHES as i64).contains(&n) => n,
            Some(_) => return Err(format!("limit 须在 1 到 {MAX_REF_MATCHES} 之间").into()),
        };
        let path = optional_string(&args, "path")?;
        let glob_matcher = match optional_string(&args, "glob")? {
            Some(pat) if !pat.trim().is_empty() => Some(glob::GlobMatcher::compile(&pat)?),
            Some(_) => return Err("glob 不能为空".into()),
            None => None,
        };
        let CollectedTextFiles {
            files,
            staged: staged_snapshot,
            explicit_file,
        } = self.0.collect_text_files(path.as_deref())?;
        let explicit_target = explicit_file || glob_matcher.is_some();

        let mut page = MatchPage {
            offset: usize::try_from(offset).unwrap_or(usize::MAX),
            limit: usize::try_from(limit).unwrap_or(MAX_REF_MATCHES),
            stop_when_full: false,
            total: 0,
            more: false,
            hits: Vec::new(),
        };
        let mut def_count = 0usize;

        for (rel, disk_path) in files {
            if let Some(g) = &glob_matcher
                && !g.is_match(&rel)
            {
                continue;
            }
            if !is_reference_candidate(&rel, explicit_target) {
                continue;
            }
            let rel_path = Path::new(&rel);
            let (text, stripped, def_lines) = if let Some(entry) = staged_snapshot.get(&rel) {
                let Some(staged) = entry else {
                    continue;
                };
                if !staged.contains(&symbol) {
                    continue;
                }
                let def_lines: BTreeSet<usize> = symbols::extract_for_path(rel_path, staged)
                    .map(|(_, syms)| {
                        syms.into_iter()
                            .filter(|s| s.name == symbol)
                            .map(|s| s.line)
                            .collect()
                    })
                    .unwrap_or_default();
                let stripped = symbols::strip_code_lines(rel_path, staged);
                (staged.clone(), stripped, def_lines)
            } else if let Some(disk) = disk_path {
                let Ok(meta) = fs::metadata(&disk) else {
                    continue;
                };
                if meta.len() > MAX_SYMBOL_FILE_BYTES {
                    continue;
                }
                let stamp = index::FileStamp::from_metadata(&meta);
                let bloom_check = stamp.and_then(|s| {
                    let mut idx = self.0.symbol_index.lock().expect("符号索引锁未中毒");
                    idx.might_contain_ident(&rel, s, &symbol)
                });
                if bloom_check == Some(false) {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&disk) else {
                    continue;
                };
                let stripped = symbols::strip_code_lines(rel_path, &text);
                let cached_syms = stamp.and_then(|s| {
                    let mut idx = self.0.symbol_index.lock().expect("符号索引锁未中毒");
                    idx.get_symbols(&rel, s)
                });
                let syms = match cached_syms {
                    Some(s) => s,
                    None => symbols::extract_for_path(rel_path, &text)
                        .map(|(_, s)| s)
                        .unwrap_or_default(),
                };
                if bloom_check.is_none()
                    && let Some(s) = stamp
                {
                    let bloom = index::build_ident_bloom(&stripped);
                    let mut idx = self.0.symbol_index.lock().expect("符号索引锁未中毒");
                    idx.put_symbols_and_bloom(rel.clone(), s, &syms, bloom);
                }
                if !text.contains(&symbol) {
                    continue;
                }
                let def_lines: BTreeSet<usize> = syms
                    .into_iter()
                    .filter(|s| s.name == symbol)
                    .map(|s| s.line)
                    .collect();
                (text, stripped, def_lines)
            } else {
                continue;
            };
            for (idx, (raw_line, code_line)) in text.lines().zip(stripped.iter()).enumerate() {
                if !symbols::has_whole_ident(code_line, &symbol) {
                    continue;
                }
                let line_no = idx + 1;
                let is_def = def_lines.contains(&line_no);
                if is_def && !include_definitions {
                    continue;
                }
                if is_def {
                    def_count += 1;
                }
                let tag = if is_def { "定义" } else { "引用" };
                page.consider(format!("{rel}:{line_no} [{tag}]: {}", raw_line.trim()));
            }
        }

        {
            let mut idx = self.0.symbol_index.lock().expect("符号索引锁未中毒");
            idx.flush_if_dirty();
        }
        if page.total == 0 {
            return Ok(format!("未找到符号 `{symbol}` 的引用"));
        }
        if page.hits.is_empty() {
            return Ok(format!(
                "共 {} 处匹配，offset={} 已超出范围",
                page.total, page.offset
            ));
        }
        let from = page.offset + 1;
        let to = page.offset + page.hits.len();
        let mut out = page.hits.join("\n");
        let summary = if def_count > 0 {
            format!(
                "共 {} 处匹配（含 {def_count} 处定义），当前第 {from}–{to} 处",
                page.total
            )
        } else {
            format!("共 {} 处引用，当前第 {from}–{to} 处", page.total)
        };
        out.push_str(&format!("\n[{summary}]"));
        Ok(out)
    }
}

pub(super) struct SymbolImpact(pub(super) Arc<Sandbox>);

impl Tool for SymbolImpact {
    fn is_read_only(&self) -> bool {
        true
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "symbol_impact".into(),
            description: format!(
                "一次汇总授权范围（{}）内某个符号的定义位置、受影响文件及按文件分组的调用／引用点（附上下文行），用于改动前快速评估波及面；跳过注释、字符串与更长标识符子串，暂存改动优先于磁盘。可用 path、glob、kind 过滤，max_call_sites 控制展示调用点数（默认 {DEFAULT_IMPACT_CALL_SITES}，上限 {MAX_IMPACT_CALL_SITES}），context_lines 控制上下文行数（默认 1，0–{MAX_IMPACT_CONTEXT_LINES}）。",
                self.0.scope_label()
            ),
            input_schema_json: format!(
                r#"{{"type":"object","properties":{{"symbol":{{"type":"string","description":"要评估影响面的完整符号标识符（如 parse_test_presets、Seal）"}},"path":{{"type":"string","description":"可选：只查这个相对目录或文件；省略则查整个授权范围"}},"glob":{{"type":"string","description":"可选：路径通配过滤，如 *.rs、crates/**/*.rs"}},"kind":{{"type":"string","description":"可选：按定义类型过滤（如 fn、struct、enum、trait、class、interface、method）"}},"max_call_sites":{{"type":"integer","description":"最多展示的调用／引用点数，默认 {DEFAULT_IMPACT_CALL_SITES}，范围 1–{MAX_IMPACT_CALL_SITES}"}},"context_lines":{{"type":"integer","description":"每个调用点前后附带的上下文行数，默认 1，范围 0–{MAX_IMPACT_CONTEXT_LINES}"}}}},"required":["symbol"],"additionalProperties":false}}"#
            ),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(
            args_json,
            &[
                "symbol",
                "path",
                "glob",
                "kind",
                "max_call_sites",
                "context_lines",
            ],
        )?;
        let symbol = required_string(&args, "symbol")?.trim().to_owned();
        if !symbols::is_valid_symbol_name(&symbol) {
            return Err("symbol 须为 1–120 字节的单个标识符（不含空格或标点）".into());
        }
        let kind = optional_string(&args, "kind")?
            .map(|k| k.trim().to_owned())
            .filter(|k| !k.is_empty());
        let max_call_sites = match optional_i64(&args, "max_call_sites")? {
            None => DEFAULT_IMPACT_CALL_SITES,
            Some(n) if (1..=MAX_IMPACT_CALL_SITES as i64).contains(&n) => n as usize,
            Some(_) => {
                return Err(
                    format!("max_call_sites 须在 1 到 {MAX_IMPACT_CALL_SITES} 之间").into(),
                );
            }
        };
        let context_lines = match optional_i64(&args, "context_lines")? {
            None => 1usize,
            Some(n) if (0..=MAX_IMPACT_CONTEXT_LINES as i64).contains(&n) => n as usize,
            Some(_) => {
                return Err(
                    format!("context_lines 须在 0 到 {MAX_IMPACT_CONTEXT_LINES} 之间").into(),
                );
            }
        };
        let path = optional_string(&args, "path")?;
        let glob_matcher = match optional_string(&args, "glob")? {
            Some(pat) if !pat.trim().is_empty() => Some(glob::GlobMatcher::compile(&pat)?),
            Some(_) => return Err("glob 不能为空".into()),
            None => None,
        };
        let CollectedTextFiles {
            files,
            staged: staged_snapshot,
            explicit_file,
        } = self.0.collect_text_files(path.as_deref())?;
        let explicit_target = explicit_file || glob_matcher.is_some();

        let mut def_entries: Vec<String> = Vec::new();
        let mut def_files: BTreeSet<String> = BTreeSet::new();
        let mut total_defs = 0usize;

        let mut call_site_sections: Vec<String> = Vec::new();
        let mut total_call_sites = 0usize;
        let mut call_site_files = 0usize;
        let mut shown_call_sites = 0usize;

        for (rel, disk_path) in files {
            if let Some(g) = &glob_matcher
                && !g.is_match(&rel)
            {
                continue;
            }
            if !is_reference_candidate(&rel, explicit_target) {
                continue;
            }
            let rel_path = Path::new(&rel);
            let (text, stripped, syms) = if let Some(entry) = staged_snapshot.get(&rel) {
                let Some(staged) = entry else {
                    continue;
                };
                if !staged.contains(&symbol) {
                    continue;
                }
                let syms = symbols::extract_for_path(rel_path, staged)
                    .map(|(_, s)| s)
                    .unwrap_or_default();
                let stripped = symbols::strip_code_lines(rel_path, staged);
                (staged.clone(), stripped, syms)
            } else if let Some(disk) = disk_path {
                let Ok(meta) = fs::metadata(&disk) else {
                    continue;
                };
                if meta.len() > MAX_SYMBOL_FILE_BYTES {
                    continue;
                }
                let stamp = index::FileStamp::from_metadata(&meta);
                let bloom_check = stamp.and_then(|s| {
                    let mut idx = self.0.symbol_index.lock().expect("符号索引锁未中毒");
                    idx.might_contain_ident(&rel, s, &symbol)
                });
                if bloom_check == Some(false) {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&disk) else {
                    continue;
                };
                let stripped = symbols::strip_code_lines(rel_path, &text);
                let cached_syms = stamp.and_then(|s| {
                    let mut idx = self.0.symbol_index.lock().expect("符号索引锁未中毒");
                    idx.get_symbols(&rel, s)
                });
                let syms = match cached_syms {
                    Some(s) => s,
                    None => symbols::extract_for_path(rel_path, &text)
                        .map(|(_, s)| s)
                        .unwrap_or_default(),
                };
                if bloom_check.is_none()
                    && let Some(s) = stamp
                {
                    let bloom = index::build_ident_bloom(&stripped);
                    let mut idx = self.0.symbol_index.lock().expect("符号索引锁未中毒");
                    idx.put_symbols_and_bloom(rel.clone(), s, &syms, bloom);
                }
                if !text.contains(&symbol) {
                    continue;
                }
                (text, stripped, syms)
            } else {
                continue;
            };

            let mut all_def_lines: BTreeSet<usize> = BTreeSet::new();
            for sym in syms.into_iter().filter(|s| s.name == symbol) {
                all_def_lines.insert(sym.line);
                if let Some(filter) = kind.as_deref()
                    && !symbols::kind_matches(sym.kind, filter)
                {
                    continue;
                }
                total_defs += 1;
                def_files.insert(rel.clone());
                if def_entries.len() < MAX_IMPACT_DEFS {
                    def_entries.push(format!(
                        "- {rel}:{} [{}] {} — {}",
                        sym.line_span(),
                        sym.kind,
                        sym.qualified_name(),
                        sym.signature
                    ));
                }
            }

            let raw_lines: Vec<&str> = text.lines().collect();
            let mut site_indices: Vec<usize> = Vec::new();
            for (idx, code_line) in stripped.iter().enumerate() {
                if idx >= raw_lines.len() {
                    break;
                }
                if !symbols::has_whole_ident(code_line, &symbol) {
                    continue;
                }
                if all_def_lines.contains(&(idx + 1)) {
                    continue;
                }
                site_indices.push(idx);
            }

            if site_indices.is_empty() {
                continue;
            }
            total_call_sites += site_indices.len();
            call_site_files += 1;

            if shown_call_sites < max_call_sites {
                let remaining = max_call_sites - shown_call_sites;
                let take_n = site_indices.len().min(remaining);
                let chosen = &site_indices[..take_n];
                let match_set: BTreeSet<usize> = chosen.iter().copied().collect();

                let mut section = if take_n < site_indices.len() {
                    format!(
                        "- {rel}（共 {} 处，本处列出前 {take_n} 处）",
                        site_indices.len()
                    )
                } else {
                    format!("- {rel}（共 {} 处）", site_indices.len())
                };
                let mut last_emitted: Option<usize> = None;
                for &match_idx in chosen {
                    let start = match_idx.saturating_sub(context_lines);
                    let end = (match_idx + context_lines).min(raw_lines.len().saturating_sub(1));
                    if let Some(prev) = last_emitted
                        && start > prev + 1
                    {
                        section.push_str("\n  ...");
                    }
                    let begin = match last_emitted {
                        Some(prev) => start.max(prev + 1),
                        None => start,
                    };
                    for (line_idx, raw_line) in
                        raw_lines.iter().enumerate().take(end + 1).skip(begin)
                    {
                        let sep = if match_set.contains(&line_idx) {
                            ':'
                        } else {
                            '-'
                        };
                        section.push_str(&format!(
                            "\n  {}{sep} {}",
                            line_idx + 1,
                            raw_line.trim_end()
                        ));
                    }
                    last_emitted = Some(end);
                }
                call_site_sections.push(section);
                shown_call_sites += take_n;
            }
        }

        {
            let mut idx = self.0.symbol_index.lock().expect("符号索引锁未中毒");
            idx.flush_if_dirty();
        }

        if total_defs == 0 && total_call_sites == 0 {
            return Ok(format!("未找到符号 `{symbol}` 的定义或调用点"));
        }

        let mut out = format!("〔符号影响面：`{symbol}`〕\n\n〔定义〕\n");
        if def_entries.is_empty() {
            out.push_str("（无匹配定义）\n");
        } else {
            out.push_str(&def_entries.join("\n"));
            out.push('\n');
            if total_defs > def_entries.len() {
                out.push_str(&format!(
                    "（其余 {} 处定义未列出）\n",
                    total_defs - def_entries.len()
                ));
            }
        }

        out.push_str("\n〔影响面／调用点〕\n");
        if call_site_sections.is_empty() {
            out.push_str("（无调用／引用点）\n");
        } else {
            out.push_str(&call_site_sections.join("\n"));
            out.push('\n');
            if total_call_sites > shown_call_sites {
                out.push_str(&format!(
                    "（其余 {} 处调用／引用未列出；可调大 max_call_sites 或用 path/glob 缩小范围）\n",
                    total_call_sites - shown_call_sites
                ));
            }
        }

        out.push_str(&format!(
            "\n〔汇总〕定义 {total_defs} 处（分布于 {} 个文件）；调用／引用 {total_call_sites} 处（分布于 {call_site_files} 个文件，已展示 {shown_call_sites} 处）",
            def_files.len()
        ));
        Ok(out)
    }
}
