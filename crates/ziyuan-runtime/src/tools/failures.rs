//! 从常见测试框架的输出里抽出失败测试名和断言摘录。
//!
//! 目前识别 `cargo test`、`pytest`、`node --test`、`go test` 与 `jest`/`vitest` 测试失败，
//! 以及 `rustc`/`cargo`、`tsc`、`mypy`/`pyright`、`go build`/`go vet`、`ruff`/`flake8`、`eslint` 的结构化编译／静态检查诊断。
//! 认不出时返回 `None`，调用方继续只给出输出末尾，绝不凭空猜测失败原因。

/// 单次附在测试结果里的失败条数上限。
const MAX_FAILURES: usize = 8;
/// 每条断言摘录的字数上限。
const MAX_EXCERPT_CHARS: usize = 400;
/// 每条摘录最多保留的行数。
const MAX_EXCERPT_LINES: usize = 8;

/// 测试命令未通过时的失败名单与断言摘录。不是已知测试框架输出时为 `None`。
pub(super) fn failure_note(log: &str) -> Option<String> {
    cargo_failure_note(log)
        .or_else(|| pytest_failure_note(log))
        .or_else(|| node_test_failure_note(log))
        .or_else(|| go_test_failure_note(log))
        .or_else(|| jest_vitest_failure_note(log))
        .or_else(|| compiler_diagnostic_note(log))
}

fn format_failures(items: &[(String, Option<String>)]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut out = String::from("〔失败测试〕\n");
    let shown = items.len().min(MAX_FAILURES);
    for (name, excerpt) in items.iter().take(shown) {
        out.push_str(&format!("- {name}\n"));
        if let Some(excerpt) = excerpt {
            for line in excerpt.lines() {
                out.push_str(&format!("  {line}\n"));
            }
        }
    }
    if items.len() > shown {
        out.push_str(&format!("（其余 {} 个失败未列出）\n", items.len() - shown));
    }
    Some(out)
}

fn push_excerpt_line(out: &mut String, line: &str, count: &mut usize) -> bool {
    if *count >= MAX_EXCERPT_LINES || out.chars().count() >= MAX_EXCERPT_CHARS {
        return false;
    }
    let trimmed = line.trim_end();
    if trimmed.is_empty() {
        return true;
    }
    if !out.is_empty() {
        out.push('\n');
    }
    let remaining = MAX_EXCERPT_CHARS.saturating_sub(out.chars().count());
    let clipped = match trimmed.char_indices().nth(remaining) {
        Some((at, _)) => &trimmed[..at],
        None => trimmed,
    };
    out.push_str(clipped);
    *count += 1;
    true
}

// ---------- cargo test ----------

/// cargo test 未通过时的失败名单与断言摘录。不是这种输出时为 `None`。
fn cargo_failure_note(log: &str) -> Option<String> {
    if !log.contains("test result: FAILED") {
        return None;
    }
    let names = cargo_failure_names(log);
    if names.is_empty() {
        return None;
    }
    let items: Vec<_> = names
        .into_iter()
        .map(|name| {
            let excerpt = cargo_excerpt_for(log, &name);
            (name, excerpt)
        })
        .collect();
    format_failures(&items)
}

/// 取最后一块「failures:」下面缩进的测试名。前面那块通常是各测试的 stdout，不是名单。
fn cargo_failure_names(log: &str) -> Vec<String> {
    let mut last = Vec::new();
    let mut current = Vec::new();
    let mut in_block = false;
    for line in log.lines() {
        if line.trim() == "failures:" {
            if !current.is_empty() {
                last = std::mem::take(&mut current);
            }
            in_block = true;
            continue;
        }
        if !in_block {
            continue;
        }
        if let Some(rest) = line.strip_prefix("    ") {
            let name = rest.trim();
            if !name.is_empty() {
                current.push(name.to_owned());
            }
            continue;
        }
        if line.trim().is_empty() {
            if !current.is_empty() {
                last = std::mem::take(&mut current);
                in_block = false;
            }
            continue;
        }
        in_block = false;
        current.clear();
    }
    if !current.is_empty() {
        last = current;
    }
    last
}

/// 兼容 Rust 旧版 `thread 'name' panicked at` 与 1.89+ `thread 'name' (12345) panicked at`。
fn find_cargo_panic_start(log: &str, name: &str) -> Option<usize> {
    let prefix = format!("thread '{name}'");
    let mut search_from = 0;
    while let Some(rel) = log[search_from..].find(&prefix) {
        let start = search_from + rel;
        let after = &log[start + prefix.len()..];
        let after_tid = if let Some(rest) = after.strip_prefix(" (") {
            rest.split_once(')').map(|(_, tail)| tail).unwrap_or(after)
        } else {
            after
        };
        if after_tid.starts_with(" panicked at") {
            return Some(start);
        }
        search_from = start + prefix.len();
    }
    None
}

fn cargo_excerpt_for(log: &str, name: &str) -> Option<String> {
    let start = find_cargo_panic_start(log, name)?;
    let mut out = String::new();
    let mut count = 0;
    for (index, line) in log[start..].lines().enumerate() {
        if index > 0
            && (line.trim().is_empty()
                || line.starts_with("note: run with")
                || line.trim() == "failures:")
        {
            break;
        }
        if !push_excerpt_line(&mut out, line, &mut count) {
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

// ---------- pytest ----------

fn pytest_failure_note(log: &str) -> Option<String> {
    // 须见到 pytest 的 summary 或 FAILURES 标头，避免把随意一行 `FAILED foo` 误认作 pytest。
    let has_marker = log.contains("short test summary info")
        || log.contains("= FAILURES =")
        || log.contains("=== FAILURES ===");
    if !has_marker {
        return None;
    }

    let mut items: Vec<(String, Option<String>)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    // 优先读 `short test summary info` 中的 `FAILED <nodeid> - <reason>`。
    if let Some(pos) = log.rfind("short test summary info") {
        for line in log[pos..].lines().skip(1) {
            let trimmed = line.trim();
            if trimmed.starts_with('=') {
                break;
            }
            if let Some(rest) = trimmed
                .strip_prefix("FAILED ")
                .or_else(|| trimmed.strip_prefix("ERROR "))
            {
                let (node, inline_reason) = match rest.split_once(" - ") {
                    Some((n, r)) => (n.trim(), Some(r.trim())),
                    None => (rest.trim(), None),
                };
                if node.is_empty() || !seen.insert(node.to_owned()) {
                    continue;
                }
                let excerpt = pytest_excerpt_for(log, node)
                    .or_else(|| inline_reason.filter(|r| !r.is_empty()).map(str::to_owned));
                items.push((node.to_owned(), excerpt));
            }
        }
    }

    // 若未开 short summary（例如 `-q --tb=short` 且只有 FAILURES 区块），回退扫描 `___ name ___`。
    if items.is_empty()
        && let Some(pos) = log.find("= FAILURES =")
    {
        for line in log[pos..].lines().skip(1) {
            let trimmed = line.trim();
            if trimmed.starts_with('=') && trimmed.contains("failed") {
                break;
            }
            if let Some(name) = parse_pytest_section_header(trimmed)
                && seen.insert(name.to_owned())
            {
                let excerpt = pytest_excerpt_for(log, name);
                items.push((name.to_owned(), excerpt));
            }
        }
    }

    format_failures(&items)
}

fn parse_pytest_section_header(trimmed: &str) -> Option<&str> {
    if !trimmed.starts_with("___") || !trimmed.ends_with("___") {
        return None;
    }
    let inner = trimmed.trim_matches('_').trim();
    (!inner.is_empty()).then_some(inner)
}

fn pytest_excerpt_for(log: &str, node: &str) -> Option<String> {
    let short_name = node.rsplit("::").next().unwrap_or(node).replace("::", ".");
    let lines: Vec<&str> = log.lines().collect();
    let header_idx = lines.iter().position(|line| {
        parse_pytest_section_header(line.trim())
            .is_some_and(|h| h == short_name || h == node || h.ends_with(&format!(".{short_name}")))
    })?;
    let mut out = String::new();
    let mut count = 0;
    for line in &lines[header_idx + 1..] {
        let trimmed = line.trim();
        if trimmed.starts_with("___") || trimmed.starts_with("===") {
            break;
        }
        let keep = trimmed.starts_with("E   ")
            || trimmed.starts_with("E\t")
            || trimmed.starts_with('>')
            || (trimmed.contains(".py:") && trimmed.contains(": "));
        if keep && !push_excerpt_line(&mut out, trimmed, &mut count) {
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

// ---------- node --test ----------

fn node_test_failure_note(log: &str) -> Option<String> {
    let is_node_spec = log.contains("✖ failing tests:")
        || log.contains("ℹ fail ")
        || (log.contains("# fail ") && log.contains("TAP version"));
    if !is_node_spec {
        return None;
    }

    let mut items: Vec<(String, Option<String>)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    // Node spec reporter：`✖ failing tests:` 块列出所有失败测试与错误详情。
    if let Some(pos) = log.rfind("✖ failing tests:") {
        let lines: Vec<&str> = log[pos..].lines().skip(1).collect();
        let mut i = 0;
        while i < lines.len() {
            let trimmed = lines[i].trim();
            if let Some(rest) = trimmed.strip_prefix("✖ ") {
                let name = strip_trailing_duration(rest).trim().to_owned();
                i += 1;
                let mut excerpt = String::new();
                let mut count = 0;
                while i < lines.len() {
                    let cur = lines[i].trim();
                    if cur.starts_with("✖ ") || cur.starts_with("test at ") || cur.starts_with("ℹ ")
                    {
                        break;
                    }
                    if !cur.is_empty() && !cur.starts_with("at ") && !cur.starts_with("node:") {
                        let _ = push_excerpt_line(&mut excerpt, cur, &mut count);
                    }
                    i += 1;
                }
                if !name.is_empty() && seen.insert(name.clone()) {
                    items.push((name, (!excerpt.is_empty()).then_some(excerpt)));
                }
                continue;
            }
            i += 1;
        }
    }

    // TAP reporter：`not ok <n> - <name>` 后跟 YAML 块 `--- ... ...`。
    if items.is_empty() {
        let lines: Vec<&str> = log.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let trimmed = lines[i].trim_start();
            if let Some(rest) = trimmed.strip_prefix("not ok ") {
                let name = rest
                    .split_once(" - ")
                    .map(|(_, n)| n)
                    .unwrap_or(rest)
                    .trim()
                    .to_owned();
                i += 1;
                let mut excerpt = String::new();
                let mut count = 0;
                if i < lines.len() && lines[i].trim() == "---" {
                    i += 1;
                    while i < lines.len() && lines[i].trim() != "..." {
                        let cur = lines[i].trim();
                        if cur.starts_with("error:")
                            || cur.starts_with("expected:")
                            || cur.starts_with("actual:")
                            || cur.starts_with("operator:")
                            || cur.starts_with("code:")
                        {
                            let _ = push_excerpt_line(&mut excerpt, cur, &mut count);
                        }
                        i += 1;
                    }
                }
                if !name.is_empty() && seen.insert(name.clone()) {
                    items.push((name, (!excerpt.is_empty()).then_some(excerpt)));
                }
                continue;
            }
            i += 1;
        }
    }

    format_failures(&items)
}

fn strip_trailing_duration(s: &str) -> &str {
    if let Some(open) = s.rfind(" (")
        && s.ends_with(')')
        && (s[open..].contains("ms") || s[open..].contains('s'))
    {
        return &s[..open];
    }
    s
}

// ---------- go test ----------

fn go_test_failure_note(log: &str) -> Option<String> {
    if !log.contains("--- FAIL: ") {
        return None;
    }
    let lines: Vec<&str> = log.lines().collect();
    let mut items: Vec<(String, Option<String>)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if let Some(rest) = trimmed.strip_prefix("--- FAIL: ") {
            let name = rest.split_whitespace().next().unwrap_or("").to_owned();
            i += 1;
            let mut excerpt = String::new();
            let mut count = 0;
            while i < lines.len() {
                let raw = lines[i];
                let t = raw.trim_start();
                if t.starts_with("=== RUN")
                    || t.starts_with("--- ")
                    || t.starts_with("FAIL")
                    || t.starts_with("PASS")
                {
                    break;
                }
                if !t.is_empty() {
                    let _ = push_excerpt_line(&mut excerpt, t, &mut count);
                }
                i += 1;
            }
            if !name.is_empty() && seen.insert(name.clone()) {
                items.push((name, (!excerpt.is_empty()).then_some(excerpt)));
            }
            continue;
        }
        i += 1;
    }
    format_failures(&items)
}

// ---------- jest / vitest ----------

fn jest_vitest_failure_note(log: &str) -> Option<String> {
    let has_jest_vitest =
        log.contains("FAIL ") && (log.contains("● ") || log.contains("AssertionError"));
    if !has_jest_vitest {
        return None;
    }
    let lines: Vec<&str> = log.lines().collect();
    let mut items: Vec<(String, Option<String>)> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        let name = trimmed
            .strip_prefix("● ")
            .or_else(|| trimmed.strip_prefix("FAIL  "))
            .map(str::trim);
        if let Some(name) = name
            && !name.is_empty()
            && !name.ends_with(".ts")
            && !name.ends_with(".js")
            && !name.ends_with(".tsx")
            && !name.ends_with(".jsx")
        {
            let name = name.to_owned();
            i += 1;
            let mut excerpt = String::new();
            let mut count = 0;
            while i < lines.len() {
                let cur = lines[i].trim();
                if cur.starts_with("● ")
                    || cur.starts_with("FAIL ")
                    || cur.starts_with("Test Suites:")
                    || cur.starts_with("Test Files")
                {
                    break;
                }
                if !cur.is_empty()
                    && !cur.starts_with("at ")
                    && (cur.contains("Expected")
                        || cur.contains("Received")
                        || cur.contains("AssertionError")
                        || cur.starts_with("expect(")
                        || cur.starts_with("Error:")
                        || cur.starts_with('-')
                        || cur.starts_with('+'))
                {
                    let _ = push_excerpt_line(&mut excerpt, cur, &mut count);
                }
                i += 1;
            }
            if seen.insert(name.clone()) {
                items.push((name, (!excerpt.is_empty()).then_some(excerpt)));
            }
            continue;
        }
        i += 1;
    }
    format_failures(&items)
}

// ---------- 编译与静态检查诊断（rustc / tsc / mypy / go / ruff / eslint） ----------

fn format_diagnostics(items: &[(String, Option<String>)]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut out = String::from("〔编译诊断〕\n");
    let shown = items.len().min(MAX_FAILURES);
    for (header, detail) in items.iter().take(shown) {
        out.push_str(&format!("- {header}\n"));
        if let Some(detail) = detail {
            for line in detail.lines() {
                out.push_str(&format!("  {line}\n"));
            }
        }
    }
    if items.len() > shown {
        out.push_str(&format!("（其余 {} 条诊断未列出）\n", items.len() - shown));
    }
    Some(out)
}

fn compiler_diagnostic_note(log: &str) -> Option<String> {
    rustc_diagnostic_note(log)
        .or_else(|| tsc_diagnostic_note(log))
        .or_else(|| mypy_diagnostic_note(log))
        .or_else(|| go_compiler_diagnostic_note(log))
        .or_else(|| ruff_flake8_diagnostic_note(log))
        .or_else(|| eslint_diagnostic_note(log))
}

/// 解析 `rustc` / `cargo check` / `cargo clippy` 诊断块：
/// 必须带 `--> <文件>:<行>:<列>` 定位，不把末尾的 `error: could not compile` 汇总当成诊断。
fn rustc_diagnostic_note(log: &str) -> Option<String> {
    let lines: Vec<&str> = log.lines().collect();
    let mut items: Vec<(String, Option<String>)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end();
        let is_err = (line.starts_with("error[") || line.starts_with("error:"))
            && !line.starts_with("error: could not compile")
            && !line.starts_with("error: aborting due to")
            && !line.starts_with("error: test failed");
        if !is_err {
            i += 1;
            continue;
        }
        let mut loc: Option<&str> = None;
        let mut excerpt = String::new();
        let mut excerpt_lines = 0usize;
        let mut j = i + 1;
        while j < lines.len() {
            let cur = lines[j].trim_end();
            if cur.is_empty() {
                // 跳过紧跟在诊断头后的单空行；已有定位后再遇空行则块结束。
                if loc.is_some() {
                    break;
                }
                j += 1;
                continue;
            }
            if cur.starts_with("error[")
                || cur.starts_with("error:")
                || cur.starts_with("warning[")
                || cur.starts_with("warning:")
            {
                break;
            }
            let trimmed = cur.trim_start();
            if let Some(after) = trimmed.strip_prefix("--> ") {
                if loc.is_none() {
                    loc = Some(after.trim());
                } else {
                    push_excerpt_line(&mut excerpt, trimmed, &mut excerpt_lines);
                }
            } else if trimmed.starts_with("help:")
                || trimmed.starts_with("note:")
                || trimmed.contains("^^^")
                || trimmed.contains("expected ")
                || trimmed.contains("found ")
            {
                push_excerpt_line(&mut excerpt, trimmed, &mut excerpt_lines);
            }
            j += 1;
        }
        if let Some(loc) = loc {
            let header = format!("{loc}: {line}");
            let detail = (!excerpt.is_empty()).then_some(excerpt);
            items.push((header, detail));
        }
        i = j.max(i + 1);
    }
    format_diagnostics(&items)
}

/// 解析 TypeScript `tsc` 诊断：
/// 支持 `src/app.ts(12,5): error TS2322: ...` 与 `src/app.ts:12:5 - error TS2322: ...`。
fn tsc_diagnostic_note(log: &str) -> Option<String> {
    let mut items: Vec<(String, Option<String>)> = Vec::new();
    for line in log.lines() {
        let trimmed = line.trim();
        let parsed = if let Some((left, right)) = trimmed.split_once("): error TS") {
            let (path, coords) = left.rsplit_once('(').unwrap_or(("", ""));
            if !path.is_empty()
                && coords
                    .split(',')
                    .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            {
                Some(format!("{path}({coords}): error TS{right}"))
            } else {
                None
            }
        } else if let Some((left, right)) = trimmed.split_once(" - error TS") {
            let parts: Vec<&str> = left.rsplitn(3, ':').collect();
            if parts.len() == 3
                && !parts[2].is_empty()
                && parts[0].bytes().all(|b| b.is_ascii_digit())
                && parts[1].bytes().all(|b| b.is_ascii_digit())
            {
                Some(format!("{left}: error TS{right}"))
            } else {
                None
            }
        } else {
            None
        };
        if let Some(header) = parsed {
            items.push((header, None));
        }
    }
    format_diagnostics(&items)
}

/// 解析 Python `mypy` / `pyright` 诊断：
/// 支持 `path.py:12: error: ...`、`path.py:12:5: error: ...` 与 `path.py:12:5 - error: ...`。
fn mypy_diagnostic_note(log: &str) -> Option<String> {
    let mut items: Vec<(String, Option<String>)> = Vec::new();
    for line in log.lines() {
        let trimmed = line.trim();
        let split = trimmed
            .split_once(": error: ")
            .or_else(|| trimmed.split_once(" - error: "));
        let Some((left, msg)) = split else {
            continue;
        };
        // left 形如 `src/app.py:12` 或 `src/app.py:12:5`。
        let parts: Vec<&str> = left.split(':').collect();
        let valid_loc = match parts.as_slice() {
            [path, ln] => {
                !path.is_empty() && !ln.is_empty() && ln.bytes().all(|b| b.is_ascii_digit())
            }
            [path, ln, col] => {
                !path.is_empty()
                    && !ln.is_empty()
                    && !col.is_empty()
                    && ln.bytes().all(|b| b.is_ascii_digit())
                    && col.bytes().all(|b| b.is_ascii_digit())
            }
            _ => false,
        };
        if valid_loc && !msg.trim().is_empty() {
            items.push((format!("{left}: error: {}", msg.trim()), None));
        }
    }
    format_diagnostics(&items)
}

/// 解析 `go build` / `go vet` 诊断：
/// 支持 `./pkg/foo.go:12:5: undefined: bar`、`main.go:8: syntax error: ...`；跳过 `# <pkg>` 头与缩进堆栈行。
fn go_compiler_diagnostic_note(log: &str) -> Option<String> {
    let mut items: Vec<(String, Option<String>)> = Vec::new();
    for raw in log.lines() {
        let line = raw.trim_end();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with(' ')
            || line.starts_with('\t')
        {
            continue;
        }
        let Some((left, msg)) = line.split_once(": ") else {
            continue;
        };
        let msg = msg.trim();
        if msg.is_empty() {
            continue;
        }
        // 从右向左剥离 `:<数字>`（列号可选、行号必有），剩余路径必须以 `.go` 结尾且不含空白。
        let Some((before_last, last)) = left.rsplit_once(':') else {
            continue;
        };
        if last.is_empty() || !last.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let path = match before_last.rsplit_once(':') {
            Some((p, ln)) if !ln.is_empty() && ln.bytes().all(|b| b.is_ascii_digit()) => p,
            _ => before_last,
        };
        if path.ends_with(".go") && path.len() > 3 && !path.bytes().any(|b| b.is_ascii_whitespace())
        {
            items.push((format!("{left}: {msg}"), None));
        }
    }
    format_diagnostics(&items)
}

fn is_python_lint_code(token: &str) -> bool {
    let bytes = token.as_bytes();
    if !(4..=8).contains(&bytes.len()) {
        return false;
    }
    let letters = bytes.iter().take_while(|b| b.is_ascii_uppercase()).count();
    let digits = bytes.len() - letters;
    (1..=4).contains(&letters)
        && (3..=4).contains(&digits)
        && bytes[letters..].iter().all(|b| b.is_ascii_digit())
}

fn is_python_file_loc(left: &str) -> bool {
    let Some((before_col, col)) = left.rsplit_once(':') else {
        return false;
    };
    let Some((path, ln)) = before_col.rsplit_once(':') else {
        return false;
    };
    !path.is_empty()
        && !path.bytes().any(|b| b.is_ascii_whitespace())
        && (path.ends_with(".py") || path.ends_with(".pyi"))
        && !ln.is_empty()
        && ln.bytes().all(|b| b.is_ascii_digit())
        && !col.is_empty()
        && col.bytes().all(|b| b.is_ascii_digit())
}

/// 解析 Python `ruff check` 与 `flake8` 诊断：
/// 支持 concise／flake8 单行格式（`src/app.py:12:5: F401 ...`）以及 ruff 默认块格式（`F401 ...` + `--> src/app.py:12:5`）。
fn ruff_flake8_diagnostic_note(log: &str) -> Option<String> {
    let lines: Vec<&str> = log.lines().collect();
    let mut items: Vec<(String, Option<String>)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        if let Some((left, rest)) = trimmed.split_once(": ")
            && is_python_file_loc(left)
        {
            let rest = rest.trim();
            let first_tok = rest.split_whitespace().next().unwrap_or("");
            if is_python_lint_code(first_tok) {
                items.push((format!("{left}: {rest}"), None));
                i += 1;
                continue;
            }
        }
        // ruff 默认多行格式：`<CODE> ...` 后跟 ` --> path.py:line:col`。
        let first_tok = trimmed.split_whitespace().next().unwrap_or("");
        if is_python_lint_code(first_tok)
            && !trimmed.starts_with("-->")
            && let Some(next_line) = lines.get(i + 1)
            && let Some(loc) = next_line.trim().strip_prefix("--> ")
            && is_python_file_loc(loc.trim())
        {
            items.push((format!("{}: {trimmed}", loc.trim()), None));
            i += 2;
            continue;
        }
        i += 1;
    }
    format_diagnostics(&items)
}

fn is_eslint_file_header(line: &str) -> bool {
    if line.is_empty() || line.starts_with(' ') || line.starts_with('\t') || line.contains("  ") {
        return false;
    }
    const EXTS: &[&str] = &[
        ".js", ".mjs", ".cjs", ".jsx", ".ts", ".mts", ".cts", ".tsx", ".vue", ".svelte",
    ];
    EXTS.iter()
        .any(|ext| line.ends_with(ext) && line.len() > ext.len())
}

/// 解析 ESLint 默认 `stylish` 诊断：文件路径头行后跟随缩进的 `<行>:<列>  error|warning  <说明>  [规则]`。
/// 有 `error` 时优先只收 `error`，无 `error` 时再收 `warning`（适配 `--max-warnings 0`）。
fn eslint_diagnostic_note(log: &str) -> Option<String> {
    let mut errors: Vec<(String, Option<String>)> = Vec::new();
    let mut warnings: Vec<(String, Option<String>)> = Vec::new();
    let mut current_file: Option<&str> = None;

    for raw in log.lines() {
        let trimmed_end = raw.trim_end();
        if trimmed_end.is_empty() {
            current_file = None;
            continue;
        }
        if is_eslint_file_header(trimmed_end) {
            current_file = Some(trimmed_end);
            continue;
        }
        let Some(file) = current_file else {
            continue;
        };
        if !raw.starts_with(' ') && !raw.starts_with('\t') {
            current_file = None;
            continue;
        }
        let trimmed = trimmed_end.trim_start();
        let mut parts = trimmed.split_whitespace();
        let Some(pos) = parts.next() else {
            continue;
        };
        let Some((ln, col)) = pos.split_once(':') else {
            continue;
        };
        if ln.is_empty()
            || col.is_empty()
            || !ln.bytes().all(|b| b.is_ascii_digit())
            || !col.bytes().all(|b| b.is_ascii_digit())
        {
            continue;
        }
        let Some(severity) = parts.next() else {
            continue;
        };
        if severity != "error" && severity != "warning" {
            continue;
        }
        let detail: Vec<&str> = parts.collect();
        if detail.is_empty() {
            continue;
        }
        let header = format!("{file}:{ln}:{col}: {severity}: {}", detail.join(" "));
        if severity == "error" {
            errors.push((header, None));
        } else {
            warnings.push((header, None));
        }
    }
    if !errors.is_empty() {
        format_diagnostics(&errors)
    } else {
        format_diagnostics(&warnings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_cargo() -> String {
        "\
running 2 tests
test tests::ok ... ok
test tests::foo ... FAILED

failures:

---- tests::foo stdout ----
thread 'tests::foo' (12345) panicked at src/lib.rs:10:5:
assertion `left == right` failed
  left: 1
 right: 2
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:
    tests::foo
    tests::bar

test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
"
        .into()
    }

    #[test]
    fn cargo_note_lists_names_and_assertion_including_rust_189_thread_id() {
        let note = failure_note(&sample_cargo()).unwrap();
        assert!(note.starts_with("〔失败测试〕\n- tests::foo\n"), "{note}");
        assert!(note.contains("assertion `left == right` failed"), "{note}");
        assert!(note.contains("\n- tests::bar\n"), "{note}");
        assert!(!note.contains("RUST_BACKTRACE"), "{note}");
    }

    #[test]
    fn pytest_note_extracts_node_and_assertion_lines() {
        let log = "\
============================= FAILURES =============================
___________________________ test_divide ____________________________
    def test_divide():
>       assert 1 / 1 == 2
E       assert 1.0 == 2

tests/test_math.py:4: AssertionError
===================== short test summary info ======================
FAILED tests/test_math.py::test_divide - assert 1.0 == 2
1 failed, 2 passed in 0.12s
";
        let note = failure_note(log).unwrap();
        assert!(
            note.contains("- tests/test_math.py::test_divide\n"),
            "{note}"
        );
        assert!(note.contains("assert 1.0 == 2"), "{note}");
        assert!(
            note.contains("tests/test_math.py:4: AssertionError"),
            "{note}"
        );
    }

    #[test]
    fn node_test_note_extracts_spec_and_tap_failures() {
        let spec = "\
✔ ok test (1.0ms)
✖ failing case (2.5ms)
ℹ tests 2
ℹ pass 1
ℹ fail 1

✖ failing tests:

test at a.test.mjs:5:1
✖ failing case (2.5ms)
  AssertionError [ERR_ASSERTION]: Expected values to be strictly equal:
  1 !== 2
      at TestContext.<anonymous> (file:///a.test.mjs:6:10)
";
        let note = failure_note(spec).unwrap();
        assert!(note.contains("- failing case\n"), "{note}");
        assert!(note.contains("AssertionError"), "{note}");
        assert!(!note.contains("at TestContext"), "{note}");

        let tap = "\
TAP version 13
ok 1 - passes
not ok 2 - tap failure
  ---
  error: 'expected 1 to equal 2'
  expected: 2
  actual: 1
  operator: strictEqual
  ...
# fail 1
";
        let tap_note = failure_note(tap).unwrap();
        assert!(tap_note.contains("- tap failure\n"), "{tap_note}");
        assert!(tap_note.contains("expected: 2"), "{tap_note}");
    }

    #[test]
    fn go_test_and_jest_extract_failures_cleanly() {
        let go_log = "\
=== RUN   TestAdd
--- FAIL: TestAdd (0.00s)
    math_test.go:12: got 3, want 4
FAIL
FAIL\texample.com/demo\t0.002s
";
        let go_note = failure_note(go_log).unwrap();
        assert!(go_note.contains("- TestAdd\n"), "{go_note}");
        assert!(
            go_note.contains("math_test.go:12: got 3, want 4"),
            "{go_note}"
        );

        let jest_log = "\
FAIL src/app.test.ts
  ● renders title

    expect(received).toBe(expected)

    Expected: \"hello\"
    Received: \"world\"

      at Object.<anonymous> (src/app.test.ts:8:19)

Test Suites: 1 failed, 1 total
";
        let jest_note = failure_note(jest_log).unwrap();
        assert!(jest_note.contains("- renders title\n"), "{jest_note}");
        assert!(jest_note.contains("Expected: \"hello\""), "{jest_note}");
        assert!(!jest_note.contains("at Object."), "{jest_note}");
    }

    #[test]
    fn other_commands_and_compile_errors_are_left_alone() {
        assert!(failure_note("").is_none());
        assert!(failure_note("未通过（退出码 3）\n").is_none());
        assert!(failure_note("FAILED tests/test_app.py::test_x\n").is_none());
        assert!(failure_note("error: could not compile `demo`\nerror: test failed\n").is_none());
    }

    #[test]
    fn extra_failures_are_counted_not_all_printed() {
        let mut log = String::from("test result: FAILED\nfailures:\n");
        for index in 0..10 {
            log.push_str(&format!("    tests::case{index}\n"));
        }
        let note = failure_note(&log).unwrap();
        assert!(note.contains("- tests::case0\n"), "{note}");
        assert!(note.contains("- tests::case7\n"), "{note}");
        assert!(!note.contains("tests::case8"), "{note}");
        assert!(note.contains("其余 2 个失败未列出"), "{note}");
    }

    #[test]
    fn compiler_diagnostics_extract_rustc_tsc_and_mypy() {
        let rustc_log = "\
error[E0308]: mismatched types
  --> crates/demo/src/lib.rs:42:12
   |
42 |     return \"oops\";
   |            ^^^^^^ expected `usize`, found `&str`
   |
   = help: try parsing the string

error: could not compile `demo` (lib) due to 1 previous error
";
        let note = failure_note(rustc_log).unwrap();
        assert!(note.contains("〔编译诊断〕"), "{note}");
        assert!(
            note.contains("crates/demo/src/lib.rs:42:12: error[E0308]: mismatched types"),
            "{note}"
        );
        assert!(note.contains("expected `usize`, found `&str`"), "{note}");
        assert!(!note.contains("could not compile"), "{note}");

        let tsc_log = "\
src/view.ts(18,7): error TS2322: Type 'string' is not assignable to type 'number'.
src/api.ts:9:3 - error TS2339: Property 'foo' does not exist on type 'Bar'.
";
        let tsc_note = failure_note(tsc_log).unwrap();
        assert!(
            tsc_note.contains("src/view.ts(18,7): error TS2322"),
            "{tsc_note}"
        );
        assert!(
            tsc_note.contains("src/api.ts:9:3: error TS2339"),
            "{tsc_note}"
        );

        let mypy_log = "\
pkg/calc.py:14: error: Incompatible return value type (got \"str\", expected \"int\")  [return-value]
pkg/calc.py:21:8 - error: Cannot access member \"bar\" for type \"None\"
Found 2 errors in 1 file
";
        let mypy_note = failure_note(mypy_log).unwrap();
        assert!(
            mypy_note.contains("pkg/calc.py:14: error: Incompatible return value type"),
            "{mypy_note}"
        );
        assert!(
            mypy_note.contains("pkg/calc.py:21:8: error: Cannot access member"),
            "{mypy_note}"
        );
    }

    #[test]
    fn go_vet_ruff_and_eslint_diagnostics_are_extracted() {
        let go_log = "\
# example.com/app/internal/srv
./internal/srv/http.go:42:10: fmt.Printf format %d has arg name of wrong type string
internal/srv/main.go:12: undefined: missingHelper
	/usr/local/go/src/runtime/panic.go:914 +0x123
";
        let go_note = failure_note(go_log).unwrap();
        assert!(go_note.contains("〔编译诊断〕"), "{go_note}");
        assert!(
            go_note.contains("./internal/srv/http.go:42:10: fmt.Printf format %d"),
            "{go_note}"
        );
        assert!(
            go_note.contains("internal/srv/main.go:12: undefined: missingHelper"),
            "{go_note}"
        );
        assert!(
            !go_note.contains("panic.go"),
            "缩进堆栈不应被当成编译诊断：{go_note}"
        );

        let ruff_concise = "\
src/app.py:1:8: F401 [*] `os` imported but unused
src/app.py:24:89: E501 Line too long (102 > 88)
Found 2 errors.
";
        let ruff_note = failure_note(ruff_concise).unwrap();
        assert!(
            ruff_note.contains("src/app.py:1:8: F401 [*] `os` imported but unused"),
            "{ruff_note}"
        );
        assert!(ruff_note.contains("src/app.py:24:89: E501"), "{ruff_note}");

        let ruff_default = "\
RUF001 String contains ambiguous unicode character
 --> pkg/text.py:9:12
  |
9 | s = 'a'
  |
";
        let ruff_default_note = failure_note(ruff_default).unwrap();
        assert!(
            ruff_default_note.contains("pkg/text.py:9:12: RUF001"),
            "{ruff_default_note}"
        );

        let eslint_log = "\
/repo/src/editor.ts
  12:5   error    'unused' is assigned a value but never used  no-unused-vars
  18:3   warning  Unexpected console statement                 no-console

/repo/src/index.tsx
   4:10  error    'Foo' is not defined                         no-undef

✖ 3 problems (2 errors, 1 warning)
";
        let eslint_note = failure_note(eslint_log).unwrap();
        assert!(
            eslint_note.contains("/repo/src/editor.ts:12:5: error: 'unused' is assigned"),
            "{eslint_note}"
        );
        assert!(
            eslint_note.contains("/repo/src/index.tsx:4:10: error: 'Foo' is not defined"),
            "{eslint_note}"
        );
        assert!(
            !eslint_note.contains("no-console"),
            "存在 error 时不混入 warning 噪音：{eslint_note}"
        );
    }
}
