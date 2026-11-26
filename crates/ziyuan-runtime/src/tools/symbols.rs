//! 轻量代码符号提取：供 `file_outline` 与 `find_symbols` 使用。
//!
//! 纯词法扫描、零外部依赖，只读取文本内容，不执行外部解析器或命令。
//! 跳过行注释、块注释（含 Rust 嵌套块注释）、原始／多行字符串与 Markdown 代码块，
//! 避免注释或测试样例里的伪定义混入结果。

use std::path::Path;

mod other_langs;
mod rust_jsts;
#[cfg(test)]
mod tests;

/// 识别出的代码符号。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Symbol {
    /// 1-based 起始行号。
    pub line: usize,
    /// 1-based 结束行号（含，恒有 `end_line >= line`）。
    pub end_line: usize,
    /// 规范类别名（如 `fn`、`struct`、`enum`、`trait`、`impl`、`class`、`interface`、`type`、`const`、`mod`、`macro`、`heading`）。
    pub kind: &'static str,
    /// 符号短名（如 `parse_hunk_reviews`、`Config`）。
    pub name: String,
    /// 外层容器名（如 `impl Config`、`trait Tool`、`class Editor`），顶层为空。
    pub parent: Option<String>,
    /// 签名摘录（去掉多余空白并截断），便于模型不必先读整文件就能判断是否要看。
    pub signature: String,
    /// 嵌套层级（0 为顶层，1 为 `impl`/`trait`/`class` 成员等）。
    pub depth: usize,
}

impl Symbol {
    /// 带外层容器的展示名（如 `impl Config > load`）。
    pub(super) fn qualified_name(&self) -> String {
        match &self.parent {
            Some(parent) => format!("{parent} > {}", self.name),
            None => self.name.clone(),
        }
    }

    /// 行号范围展示：单行符号返回 `"42"`，跨行符号返回 `"42-98"`。
    pub(super) fn line_span(&self) -> String {
        if self.end_line > self.line {
            format!("{}-{}", self.line, self.end_line)
        } else {
            self.line.to_string()
        }
    }
}

/// 支持符号提取的语言。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Language {
    Rust,
    JsTs,
    Python,
    Go,
    CCpp,
    Markdown,
}

impl Language {
    pub(super) fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "rs" => Some(Self::Rust),
            "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" => Some(Self::JsTs),
            "py" | "pyi" => Some(Self::Python),
            "go" => Some(Self::Go),
            "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Some(Self::CCpp),
            "md" | "markdown" => Some(Self::Markdown),
            _ => None,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::JsTs => "JavaScript/TypeScript",
            Self::Python => "Python",
            Self::Go => "Go",
            Self::CCpp => "C/C++",
            Self::Markdown => "Markdown",
        }
    }
}

/// 规范化用户传入的 `kind` 过滤参数，支持常见别名（如 `function`/`method` → `fn`）。
pub(super) fn normalize_kind(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "fn" | "func" | "function" | "method" | "def" => "fn".into(),
        "struct" => "struct".into(),
        "enum" => "enum".into(),
        "trait" => "trait".into(),
        "impl" => "impl".into(),
        "class" => "class".into(),
        "interface" => "interface".into(),
        "type" | "typedef" => "type".into(),
        "const" | "static" | "var" => "const".into(),
        "mod" | "module" | "namespace" => "mod".into(),
        "macro" => "macro".into(),
        "heading" | "section" | "h" => "heading".into(),
        other => other.to_owned(),
    }
}

pub(super) fn kind_matches(symbol_kind: &str, filter: &str) -> bool {
    let norm = normalize_kind(filter);
    if norm == symbol_kind {
        return true;
    }
    matches!(
        (norm.as_str(), symbol_kind),
        ("const", "static") | ("const", "var") | ("mod", "namespace") | ("struct", "union")
    )
}

/// 按路径后缀识别语言并提取文件内全部符号。不支持的后缀返回 `None`。
pub(super) fn extract_for_path(path: &Path, text: &str) -> Option<(Language, Vec<Symbol>)> {
    let lang = Language::from_path(path)?;
    Some((lang, extract(lang, text)))
}

pub(super) fn extract(lang: Language, text: &str) -> Vec<Symbol> {
    match lang {
        Language::Rust => rust_jsts::extract_rust(text),
        Language::JsTs => rust_jsts::extract_jsts(text),
        Language::Python => other_langs::extract_python(text),
        Language::Go => other_langs::extract_go(text),
        Language::CCpp => other_langs::extract_ccpp(text),
        Language::Markdown => other_langs::extract_markdown(text),
    }
}

pub(super) fn is_ref_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// 校验 `find_references` 的符号名：1–120 字节的单个标识符。
pub(super) fn is_valid_symbol_name(symbol: &str) -> bool {
    !symbol.is_empty() && symbol.len() <= 120 && symbol.chars().all(is_ref_ident_char)
}

/// 在已剔除注释与字符串的代码行中做整词标识符匹配（不匹配更长标识符的子串）。
pub(super) fn has_whole_ident(line: &str, symbol: &str) -> bool {
    if symbol.is_empty() {
        return false;
    }
    let mut start = 0;
    while let Some(rel) = line[start..].find(symbol) {
        let abs = start + rel;
        let end = abs + symbol.len();
        let before_ok = line[..abs]
            .chars()
            .next_back()
            .is_none_or(|c| !is_ref_ident_char(c));
        let after_ok = line[end..]
            .chars()
            .next()
            .is_none_or(|c| !is_ref_ident_char(c));
        if before_ok && after_ok {
            return true;
        }
        start = abs + line[abs..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// 按路径后缀剔除注释与字符串字面量，返回与原文件逐行对齐的代码骨架，供 `find_references` 使用。
/// 被剔除的注释／字符串以空格占位，防止两侧标识符黏连成伪匹配。
pub(super) fn strip_code_lines(path: &Path, text: &str) -> Vec<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    match Language::from_path(path) {
        Some(Language::Rust) => {
            let mut lex = BraceLexState::default();
            text.lines()
                .map(|line| lex.strip_line_with_pad(line, true, false, true))
                .collect()
        }
        Some(Language::JsTs | Language::Go) => {
            let mut lex = BraceLexState::default();
            text.lines()
                .map(|line| lex.strip_line_with_pad(line, false, true, true))
                .collect()
        }
        Some(Language::CCpp) => {
            let mut lex = BraceLexState::default();
            text.lines()
                .map(|line| lex.strip_line_with_pad(line, false, false, true))
                .collect()
        }
        Some(Language::Python) => {
            let mut triple = None;
            text.lines()
                .map(|line| other_langs::strip_python_line_with_pad(line, &mut triple, true))
                .collect()
        }
        Some(Language::Markdown) => text.lines().map(str::to_owned).collect(),
        None => match ext.as_deref() {
            Some("sh" | "bash" | "zsh" | "rb" | "toml" | "yaml" | "yml" | "ps1") => {
                let mut triple = None;
                text.lines()
                    .map(|line| other_langs::strip_python_line_with_pad(line, &mut triple, true))
                    .collect()
            }
            _ => {
                let mut lex = BraceLexState::default();
                text.lines()
                    .map(|line| lex.strip_line_with_pad(line, false, false, true))
                    .collect()
            }
        },
    }
}

// ---------- 通用辅助 ----------

fn clip_sig(line: &str) -> String {
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_end_matches('{').trim();
    match trimmed.char_indices().nth(120) {
        Some((at, _)) => format!("{}…", &trimmed[..at]),
        None => trimmed.to_owned(),
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$'
}

fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn take_ident(s: &str) -> Option<(&str, &str)> {
    let bytes = s.as_bytes();
    if bytes.is_empty() || !is_ident_start(bytes[0]) {
        return None;
    }
    let mut i = 1;
    while i < bytes.len() && is_ident_cont(bytes[i]) {
        i += 1;
    }
    Some((&s[..i], &s[i..]))
}

/// 从 C 系语言的一行中剔除注释与字符串内容，保留代码骨架与花括号，并维护跨行状态。
#[derive(Default)]
struct BraceLexState {
    /// 块注释嵌套深度（Rust 支持嵌套 `/* /* */ */`，其他 C 系语言 0/1）。
    block_comment: usize,
    /// Rust 原始字符串 `r#"..."#` 所需的 `#` 个数（`Some(n)` 表示在原始字符串内）。
    rust_raw_hashes: Option<usize>,
    /// JS 模板字符串或 Go 原始反引号字符串是否尚未闭合。
    in_backtick: bool,
}

impl BraceLexState {
    fn strip_line(&mut self, line: &str, nested_comments: bool, backticks: bool) -> String {
        self.strip_line_with_pad(line, nested_comments, backticks, false)
    }

    fn strip_line_with_pad(
        &mut self,
        line: &str,
        nested_comments: bool,
        backticks: bool,
        pad: bool,
    ) -> String {
        let bytes = line.as_bytes();
        let mut out = String::with_capacity(line.len());
        let mut i = 0;
        let mut in_quote: Option<u8> = None;

        while i < bytes.len() {
            if let Some(hashes) = self.rust_raw_hashes {
                if bytes[i] == b'"' {
                    let mut count = 0;
                    while i + 1 + count < bytes.len() && bytes[i + 1 + count] == b'#' {
                        count += 1;
                    }
                    if count >= hashes {
                        self.rust_raw_hashes = None;
                        i += 1 + hashes;
                        continue;
                    }
                }
                i += 1;
                continue;
            }

            if self.in_backtick {
                if bytes[i] == b'\\' && !nested_comments && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'`' {
                    self.in_backtick = false;
                }
                i += 1;
                continue;
            }

            if self.block_comment > 0 {
                if nested_comments
                    && i + 1 < bytes.len()
                    && bytes[i] == b'/'
                    && bytes[i + 1] == b'*'
                {
                    self.block_comment += 1;
                    i += 2;
                    continue;
                }
                if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    self.block_comment -= 1;
                    i += 2;
                    continue;
                }
                i += 1;
                continue;
            }

            if let Some(q) = in_quote {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if bytes[i] == q {
                    in_quote = None;
                }
                i += 1;
                continue;
            }

            // 行注释
            if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
                break;
            }
            // 块注释开始
            if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                if pad {
                    out.push(' ');
                }
                self.block_comment = 1;
                i += 2;
                continue;
            }
            // Rust 原始字符串 r#"..."# / br#"..."#
            if nested_comments
                && (bytes[i] == b'r' || (bytes[i] == b'b' && bytes.get(i + 1) == Some(&b'r')))
                && (i == 0 || !is_ident_cont(bytes[i - 1]))
            {
                let start = if bytes[i] == b'b' { i + 2 } else { i + 1 };
                let mut hashes = 0;
                while start + hashes < bytes.len() && bytes[start + hashes] == b'#' {
                    hashes += 1;
                }
                if bytes.get(start + hashes) == Some(&b'"') {
                    if pad {
                        out.push(' ');
                    }
                    self.rust_raw_hashes = Some(hashes);
                    i = start + hashes + 1;
                    continue;
                }
            }
            // 字符串字面量
            if bytes[i] == b'"' {
                if pad {
                    out.push(' ');
                }
                in_quote = Some(b'"');
                i += 1;
                continue;
            }
            if bytes[i] == b'\'' {
                // Rust 生命周期 `'a` 与字符字面量 `'x'` 区分：仅当闭合单引号在 2..=6 字节内且不是生命周期时才当作字符字面量。
                if nested_comments {
                    if let Some(end) = rust_char_literal_end(bytes, i) {
                        if pad {
                            out.push(' ');
                        }
                        i = end;
                        continue;
                    }
                } else {
                    if pad {
                        out.push(' ');
                    }
                    in_quote = Some(b'\'');
                    i += 1;
                    continue;
                }
            }
            if backticks && bytes[i] == b'`' {
                if pad {
                    out.push(' ');
                }
                self.in_backtick = true;
                i += 1;
                continue;
            }

            let ch = line[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
        out
    }
}

fn rust_char_literal_end(bytes: &[u8], start: usize) -> Option<usize> {
    let next = *bytes.get(start + 1)?;
    if next == b'\\' {
        let end = bytes.len().min(start + 12);
        if let Some(rel) = bytes[start + 3..end].iter().position(|&b| b == b'\'') {
            return Some(start + 3 + rel + 1);
        }
        None
    } else if bytes.get(start + 2) == Some(&b'\'') {
        Some(start + 3)
    } else {
        // 多字节 UTF-8 单字符字面量
        let rest = std::str::from_utf8(&bytes[start + 1..]).ok()?;
        let ch = rest.chars().next()?;
        let end = start + 1 + ch.len_utf8();
        (bytes.get(end) == Some(&b'\'')).then_some(end + 1)
    }
}

fn count_braces(code: &str) -> (usize, usize) {
    let mut open = 0;
    let mut close = 0;
    for b in code.bytes() {
        if b == b'{' {
            open += 1;
        } else if b == b'}' {
            close += 1;
        }
    }
    (open, close)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SpanExpect {
    SingleLine,
    BlockOrSemi,
    SemiOnly,
}

#[derive(Default)]
struct BraceSpanTracker {
    open: Vec<(usize, usize)>,
    pending: Option<(usize, usize, bool)>,
}

impl BraceSpanTracker {
    fn close_leading(&mut self, symbols: &mut [Symbol], effective_depth: usize, line_no: usize) {
        while let Some(&(idx, target)) = self.open.last() {
            if target >= effective_depth {
                symbols[idx].end_line = line_no;
                self.open.pop();
            } else {
                break;
            }
        }
        if let Some((_, target, _)) = self.pending
            && target > effective_depth
        {
            self.pending = None;
        }
    }

    fn register(&mut self, sym_idx: usize, effective_depth: usize, expect: SpanExpect) {
        self.pending = match expect {
            SpanExpect::SingleLine => None,
            SpanExpect::BlockOrSemi => Some((sym_idx, effective_depth, false)),
            SpanExpect::SemiOnly => Some((sym_idx, effective_depth, true)),
        };
    }

    fn advance(
        &mut self,
        symbols: &mut [Symbol],
        rest: &str,
        mut cur_depth: usize,
        line_no: usize,
    ) -> usize {
        for b in rest.bytes() {
            match b {
                b'{' => {
                    if let Some((idx, target, false)) = self.pending
                        && cur_depth == target
                    {
                        self.open.push((idx, target));
                        self.pending = None;
                    }
                    cur_depth += 1;
                }
                b'}' => {
                    cur_depth = cur_depth.saturating_sub(1);
                    while let Some(&(idx, target)) = self.open.last() {
                        if target >= cur_depth {
                            symbols[idx].end_line = line_no;
                            self.open.pop();
                        } else {
                            break;
                        }
                    }
                    if let Some((_, target, _)) = self.pending
                        && target > cur_depth
                    {
                        self.pending = None;
                    }
                }
                b';' => {
                    if let Some((idx, target, _)) = self.pending
                        && cur_depth == target
                    {
                        symbols[idx].end_line = line_no;
                        self.pending = None;
                    }
                }
                _ => {}
            }
        }
        cur_depth
    }
}

fn has_unclosed_paren(code: &str) -> bool {
    let mut depth = 0isize;
    for b in code.bytes() {
        if b == b'(' {
            depth += 1;
        } else if b == b')' {
            depth -= 1;
        }
    }
    depth > 0
}

fn strip_leading_vis_and_attrs(mut s: &str) -> &str {
    loop {
        s = s.trim_start();
        if let Some(rest) = s.strip_prefix("#[") {
            if let Some(end) = rest.find(']') {
                s = &rest[end + 1..];
                continue;
            }
            return "";
        }
        if let Some(rest) = s.strip_prefix("pub") {
            if let Some(after_paren) = rest.strip_prefix('(') {
                if let Some(end) = after_paren.find(')') {
                    s = &after_paren[end + 1..];
                    continue;
                }
                return "";
            }
            if rest.starts_with(char::is_whitespace) {
                s = rest;
                continue;
            }
        }
        break;
    }
    s.trim_start()
}

fn strip_word<'a>(s: &'a str, word: &str) -> Option<&'a str> {
    let rest = s.strip_prefix(word)?;
    if rest.is_empty() || !is_ident_cont(rest.as_bytes()[0]) {
        Some(rest.trim_start())
    } else {
        None
    }
}

/// 跳过尖括号泛型参数（如 `<T: Clone, 'a>`），返回剩余切片。
fn skip_generics(s: &str) -> &str {
    let s = s.trim_start();
    if !s.starts_with('<') {
        return s;
    }
    let mut depth = 0usize;
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'<' => depth += 1,
            b'>' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return s[i + 1..].trim_start();
                }
            }
            _ => {}
        }
    }
    ""
}
