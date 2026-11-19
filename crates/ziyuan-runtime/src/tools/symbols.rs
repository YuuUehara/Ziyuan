//! 轻量代码符号提取：供 `file_outline` 与 `find_symbols` 使用。
//!
//! 纯词法扫描、零外部依赖，只读取文本内容，不执行外部解析器或命令。
//! 跳过行注释、块注释（含 Rust 嵌套块注释）、原始／多行字符串与 Markdown 代码块，
//! 避免注释或测试样例里的伪定义混入结果。

use std::path::Path;

/// 识别出的代码符号。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Symbol {
    /// 1-based 行号。
    pub line: usize,
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
        Language::Rust => extract_rust(text),
        Language::JsTs => extract_jsts(text),
        Language::Python => extract_python(text),
        Language::Go => extract_go(text),
        Language::CCpp => extract_ccpp(text),
        Language::Markdown => extract_markdown(text),
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
                    self.rust_raw_hashes = Some(hashes);
                    i = start + hashes + 1;
                    continue;
                }
            }
            // 字符串字面量
            if bytes[i] == b'"' {
                in_quote = Some(b'"');
                i += 1;
                continue;
            }
            if bytes[i] == b'\'' {
                // Rust 生命周期 `'a` 与字符字面量 `'x'` 区分：仅当闭合单引号在 2..=6 字节内且不是生命周期时才当作字符字面量。
                if nested_comments {
                    if let Some(end) = rust_char_literal_end(bytes, i) {
                        i = end;
                        continue;
                    }
                } else {
                    in_quote = Some(b'\'');
                    i += 1;
                    continue;
                }
            }
            if backticks && bytes[i] == b'`' {
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

// ---------- Rust ----------

fn extract_rust(text: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    let mut lex = BraceLexState::default();
    let mut depth: usize = 0;
    // (容器展示名, 进入容器时的花括号深度)
    let mut containers: Vec<(String, usize)> = Vec::new();
    // 跨行 impl/trait/mod 头部尚未遇到 `{` 时暂存容器名。
    let mut pending_container: Option<String> = None;

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let code = lex.strip_line(raw_line, true, false);
        let trimmed = code.trim();
        let (opens, closes) = count_braces(&code);

        // 先处理行首的 `}` 使容器出栈，防止 `}` 后同行又写了新项。
        let leading_closes = trimmed.bytes().take_while(|&b| b == b'}').count();
        let effective_depth = depth.saturating_sub(leading_closes);
        while containers
            .last()
            .is_some_and(|(_, enter)| *enter > effective_depth)
        {
            containers.pop();
        }

        // 若前一行是未见 `{` 的 `impl`/`trait`/`mod`，在本行见到 `{` 时入栈；见到 `;` 则放弃。
        if let Some(name) = pending_container.as_ref() {
            if opens > 0 {
                containers.push((name.clone(), effective_depth + 1));
                pending_container = None;
            } else if trimmed.contains(';') {
                pending_container = None;
            }
        }

        // 只提取顶层或模块／impl／trait 内的声明（不抓取函数体内部的局部辅助项，减少噪音）。
        let in_container = containers.len() == effective_depth;
        if in_container && !trimmed.is_empty() && !trimmed.starts_with('#') {
            let parent = containers.last().map(|(name, _)| name.clone());
            let sym_depth = containers.len();
            if let Some((kind, name, container_label)) = parse_rust_item(trimmed) {
                symbols.push(Symbol {
                    line: line_no,
                    kind,
                    name,
                    parent,
                    signature: clip_sig(trimmed),
                    depth: sym_depth,
                });
                if let Some(label) = container_label {
                    if opens > closes {
                        containers.push((label, effective_depth + 1));
                    } else if opens == 0 && !trimmed.contains(';') {
                        pending_container = Some(label);
                    }
                }
            }
        }

        depth = depth.saturating_add(opens).saturating_sub(closes);
        while containers.last().is_some_and(|(_, enter)| *enter > depth) {
            containers.pop();
        }
    }
    symbols
}

fn parse_rust_item(trimmed: &str) -> Option<(&'static str, String, Option<String>)> {
    let mut s = strip_leading_vis_and_attrs(trimmed);
    if s.is_empty() {
        return None;
    }
    // 跳过 `default` / `unsafe` / `async` / `const` / `extern "..."` 修饰符。
    let mut saw_const = false;
    loop {
        if let Some(rest) = strip_word(s, "default") {
            s = rest;
        } else if let Some(rest) = strip_word(s, "unsafe") {
            s = rest;
        } else if let Some(rest) = strip_word(s, "async") {
            s = rest;
        } else if let Some(rest) = strip_word(s, "const") {
            // `const fn` 是函数，`const FOO:` 是常量。
            if strip_word(rest, "fn").is_some()
                || strip_word(rest, "unsafe").is_some()
                || strip_word(rest, "extern").is_some()
            {
                s = rest;
                continue;
            }
            saw_const = true;
            s = rest;
            break;
        } else if let Some(rest) = strip_word(s, "extern") {
            // 跳过可选 ABI（`extern "C"` 被剥字符串后只剩空白）。
            s = rest;
        } else {
            break;
        }
    }

    if saw_const {
        let (name, _) = take_ident(s)?;
        return Some(("const", name.to_owned(), None));
    }

    if let Some(rest) = strip_word(s, "fn") {
        let (name, _) = take_ident(rest)?;
        return Some(("fn", name.to_owned(), None));
    }
    if let Some(rest) = strip_word(s, "struct") {
        let (name, _) = take_ident(rest)?;
        return Some(("struct", name.to_owned(), None));
    }
    if let Some(rest) = strip_word(s, "enum") {
        let (name, _) = take_ident(rest)?;
        return Some(("enum", name.to_owned(), None));
    }
    if let Some(rest) = strip_word(s, "union") {
        let (name, _) = take_ident(rest)?;
        return Some(("union", name.to_owned(), None));
    }
    if let Some(rest) = strip_word(s, "trait") {
        let (name, _) = take_ident(rest)?;
        return Some(("trait", name.to_owned(), Some(format!("trait {name}"))));
    }
    if let Some(rest) = strip_word(s, "impl") {
        let header = parse_rust_impl_name(rest)?;
        return Some(("impl", header.clone(), Some(format!("impl {header}"))));
    }
    if let Some(rest) = strip_word(s, "mod") {
        let (name, after) = take_ident(rest)?;
        let is_inline = !after.trim_start().starts_with(';');
        return Some((
            "mod",
            name.to_owned(),
            is_inline.then(|| format!("mod {name}")),
        ));
    }
    if let Some(rest) = strip_word(s, "type") {
        let (name, _) = take_ident(rest)?;
        return Some(("type", name.to_owned(), None));
    }
    if let Some(rest) = strip_word(s, "static") {
        let rest = strip_word(rest, "mut").unwrap_or(rest);
        let (name, _) = take_ident(rest)?;
        return Some(("static", name.to_owned(), None));
    }
    if let Some(rest) = s.strip_prefix("macro_rules!") {
        let (name, _) = take_ident(rest.trim_start())?;
        return Some(("macro", name.to_owned(), None));
    }
    None
}

fn parse_rust_impl_name(rest: &str) -> Option<String> {
    let body = skip_generics(rest);
    let head = body
        .split('{')
        .next()
        .unwrap_or(body)
        .split(" where")
        .next()
        .unwrap_or(body)
        .trim();
    if head.is_empty() {
        return None;
    }
    let clean = head.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(clean)
}

// ---------- JavaScript / TypeScript ----------

fn extract_jsts(text: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    let mut lex = BraceLexState::default();
    let mut depth: usize = 0;
    let mut containers: Vec<(String, usize)> = Vec::new();

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let code = lex.strip_line(raw_line, false, true);
        let trimmed = code.trim();
        let (opens, closes) = count_braces(&code);

        let leading_closes = trimmed.bytes().take_while(|&b| b == b'}').count();
        let effective_depth = depth.saturating_sub(leading_closes);
        while containers
            .last()
            .is_some_and(|(_, enter)| *enter > effective_depth)
        {
            containers.pop();
        }

        let in_container = containers.len() == effective_depth;
        if in_container && !trimmed.is_empty() {
            let parent = containers.last().map(|(n, _)| n.clone());
            let sym_depth = containers.len();
            if let Some((kind, name, container_label)) =
                parse_jsts_item(trimmed, !containers.is_empty())
            {
                symbols.push(Symbol {
                    line: line_no,
                    kind,
                    name,
                    parent,
                    signature: clip_sig(trimmed),
                    depth: sym_depth,
                });
                if let Some(label) = container_label
                    && opens > closes
                {
                    containers.push((label, effective_depth + 1));
                }
            }
        }

        depth = depth.saturating_add(opens).saturating_sub(closes);
        while containers.last().is_some_and(|(_, enter)| *enter > depth) {
            containers.pop();
        }
    }
    symbols
}

fn parse_jsts_item(
    trimmed: &str,
    inside_class: bool,
) -> Option<(&'static str, String, Option<String>)> {
    let mut s = trimmed;
    for kw in ["export", "default", "declare", "abstract", "async"] {
        if let Some(rest) = strip_word(s, kw) {
            s = rest;
        }
    }

    if let Some(rest) = strip_word(s, "function") {
        let rest = rest.strip_prefix('*').map(str::trim_start).unwrap_or(rest);
        let (name, _) = take_ident(rest)?;
        return Some(("fn", name.to_owned(), None));
    }
    if let Some(rest) = strip_word(s, "class") {
        let (name, _) = take_ident(rest)?;
        return Some(("class", name.to_owned(), Some(format!("class {name}"))));
    }
    if let Some(rest) = strip_word(s, "interface") {
        let (name, _) = take_ident(rest)?;
        return Some((
            "interface",
            name.to_owned(),
            Some(format!("interface {name}")),
        ));
    }
    if let Some(rest) = strip_word(s, "type") {
        let (name, after) = take_ident(rest)?;
        if after.trim_start().starts_with('=') || after.trim_start().starts_with('<') {
            return Some(("type", name.to_owned(), None));
        }
    }
    if let Some(rest) = strip_word(s, "enum") {
        let (name, _) = take_ident(rest)?;
        return Some(("enum", name.to_owned(), None));
    }
    // `const foo = (...) =>` / `const foo = async (...) =>` / `const foo = function`
    if !inside_class
        && let Some(rest) = strip_word(s, "const")
            .or_else(|| strip_word(s, "let"))
            .or_else(|| strip_word(s, "var"))
        && let Some((name, after)) = take_ident(rest)
    {
        let rhs = after
            .split_once('=')
            .map(|(_, r)| r.trim_start())
            .unwrap_or("");
        let is_fn =
            rhs.contains("=>") || rhs.starts_with("function") || rhs.starts_with("async function");
        if is_fn {
            return Some(("fn", name.to_owned(), None));
        }
        // 顶层大写的导出常量也收录（如 `export const MAX_SIZE = ...`）
        if trimmed.starts_with("export ") {
            return Some(("const", name.to_owned(), None));
        }
    }

    // 类方法（如 `async save(x) {`、`static create() {`、`get value() {`）
    if inside_class {
        let mut m = s;
        for kw in [
            "public",
            "private",
            "protected",
            "readonly",
            "static",
            "async",
            "get",
            "set",
        ] {
            if let Some(rest) = strip_word(m, kw) {
                m = rest;
            }
        }
        if let Some((name, after)) = take_ident(m)
            && !matches!(
                name,
                "if" | "for" | "while" | "switch" | "catch" | "return" | "throw" | "new"
            )
            && (after.trim_start().starts_with('(') || after.trim_start().starts_with('<'))
        {
            return Some(("fn", name.to_owned(), None));
        }
    }
    None
}

// ---------- Python ----------

fn extract_python(text: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    let mut triple_quote: Option<&'static str> = None;
    // (容器名, 缩进空格数)
    let mut classes: Vec<(String, usize)> = Vec::new();

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        // 处理三引号多行字符串／docstring
        let code = strip_python_line(raw_line, &mut triple_quote);
        let trimmed = code.trim();
        if trimmed.is_empty() {
            continue;
        }
        let indent = raw_line
            .bytes()
            .take_while(|b| *b == b' ' || *b == b'\t')
            .map(|b| if b == b'\t' { 4 } else { 1 })
            .sum::<usize>();

        while classes.last().is_some_and(|(_, ci)| indent <= *ci) {
            classes.pop();
        }

        let parent = classes.last().map(|(n, _)| n.clone());
        let sym_depth = classes.len();
        if let Some(rest) = strip_word(trimmed, "class")
            && let Some((name, _)) = take_ident(rest)
        {
            symbols.push(Symbol {
                line: line_no,
                kind: "class",
                name: name.to_owned(),
                parent,
                signature: clip_sig(trimmed.trim_end_matches(':')),
                depth: sym_depth,
            });
            classes.push((format!("class {name}"), indent));
            continue;
        }
        let fn_rest = strip_word(trimmed, "async")
            .and_then(|r| strip_word(r, "def"))
            .or_else(|| strip_word(trimmed, "def"));
        if let Some(rest) = fn_rest
            && let Some((name, _)) = take_ident(rest)
        {
            // 仅收顶层函数与类直接方法，跳过嵌套局部 def。
            let expected_member = classes.last().is_some_and(|(_, ci)| indent > *ci);
            if indent == 0 || expected_member {
                symbols.push(Symbol {
                    line: line_no,
                    kind: "fn",
                    name: name.to_owned(),
                    parent,
                    signature: clip_sig(trimmed.trim_end_matches(':')),
                    depth: sym_depth,
                });
            }
        }
    }
    symbols
}

fn strip_python_line(line: &str, triple: &mut Option<&'static str>) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    let mut single_quote: Option<u8> = None;

    while i < bytes.len() {
        if let Some(q) = *triple {
            if line[i..].starts_with(q) {
                *triple = None;
                i += 3;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(q) = single_quote {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if bytes[i] == q {
                single_quote = None;
            }
            i += 1;
            continue;
        }
        if bytes[i] == b'#' {
            break;
        }
        if line[i..].starts_with("\"\"\"") {
            *triple = Some("\"\"\"");
            i += 3;
            continue;
        }
        if line[i..].starts_with("'''") {
            *triple = Some("'''");
            i += 3;
            continue;
        }
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            single_quote = Some(bytes[i]);
            i += 1;
            continue;
        }
        let ch = line[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

// ---------- Go ----------

fn extract_go(text: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    let mut lex = BraceLexState::default();
    let mut depth: usize = 0;

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let code = lex.strip_line(raw_line, false, true);
        let trimmed = code.trim();
        let (opens, closes) = count_braces(&code);

        if depth == 0 && !trimmed.is_empty() {
            if let Some(rest) = strip_word(trimmed, "func") {
                if let Some(after_recv) = rest.strip_prefix('(') {
                    if let Some((recv, tail)) = after_recv.split_once(')')
                        && let Some((name, _)) = take_ident(tail.trim_start())
                    {
                        let recv_type = recv
                            .split_whitespace()
                            .last()
                            .unwrap_or("")
                            .trim_start_matches('*');
                        let parent = (!recv_type.is_empty()).then(|| recv_type.to_owned());
                        symbols.push(Symbol {
                            line: line_no,
                            kind: "fn",
                            name: name.to_owned(),
                            parent,
                            signature: clip_sig(trimmed),
                            depth: 0,
                        });
                    }
                } else if let Some((name, _)) = take_ident(rest) {
                    symbols.push(Symbol {
                        line: line_no,
                        kind: "fn",
                        name: name.to_owned(),
                        parent: None,
                        signature: clip_sig(trimmed),
                        depth: 0,
                    });
                }
            } else if let Some(rest) = strip_word(trimmed, "type")
                && let Some((name, after)) = take_ident(rest)
            {
                let after = skip_generics(after);
                let kind = if strip_word(after, "struct").is_some() {
                    "struct"
                } else if strip_word(after, "interface").is_some() {
                    "interface"
                } else {
                    "type"
                };
                symbols.push(Symbol {
                    line: line_no,
                    kind,
                    name: name.to_owned(),
                    parent: None,
                    signature: clip_sig(trimmed),
                    depth: 0,
                });
            } else if let Some(rest) = strip_word(trimmed, "const")
                && let Some((name, _)) = take_ident(rest)
            {
                symbols.push(Symbol {
                    line: line_no,
                    kind: "const",
                    name: name.to_owned(),
                    parent: None,
                    signature: clip_sig(trimmed),
                    depth: 0,
                });
            }
        }

        depth = depth.saturating_add(opens).saturating_sub(closes);
    }
    symbols
}

// ---------- C / C++ ----------

fn extract_ccpp(text: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    let mut lex = BraceLexState::default();
    let mut depth: usize = 0;
    let mut containers: Vec<(String, usize)> = Vec::new();

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let code = lex.strip_line(raw_line, false, false);
        let trimmed = code.trim();
        let (opens, closes) = count_braces(&code);

        let leading_closes = trimmed.bytes().take_while(|&b| b == b'}').count();
        let effective_depth = depth.saturating_sub(leading_closes);
        while containers
            .last()
            .is_some_and(|(_, enter)| *enter > effective_depth)
        {
            containers.pop();
        }

        if let Some(rest) = trimmed.strip_prefix("#define")
            && let Some((name, _)) = take_ident(rest.trim_start())
        {
            symbols.push(Symbol {
                line: line_no,
                kind: "macro",
                name: name.to_owned(),
                parent: None,
                signature: clip_sig(trimmed),
                depth: 0,
            });
            continue;
        }

        let in_container = containers.len() == effective_depth;
        if in_container && !trimmed.is_empty() && !trimmed.starts_with('#') {
            let parent = containers.last().map(|(n, _)| n.clone());
            let sym_depth = containers.len();
            if let Some((kind, name, container_label)) = parse_ccpp_item(trimmed) {
                symbols.push(Symbol {
                    line: line_no,
                    kind,
                    name,
                    parent,
                    signature: clip_sig(trimmed),
                    depth: sym_depth,
                });
                if let Some(label) = container_label
                    && opens > closes
                {
                    containers.push((label, effective_depth + 1));
                }
            }
        }

        depth = depth.saturating_add(opens).saturating_sub(closes);
        while containers.last().is_some_and(|(_, enter)| *enter > depth) {
            containers.pop();
        }
    }
    symbols
}

fn parse_ccpp_item(trimmed: &str) -> Option<(&'static str, String, Option<String>)> {
    if let Some(rest) = strip_word(trimmed, "namespace")
        && let Some((name, _)) = take_ident(rest)
    {
        return Some((
            "namespace",
            name.to_owned(),
            Some(format!("namespace {name}")),
        ));
    }
    if let Some(rest) = strip_word(trimmed, "class")
        && let Some((name, after)) = take_ident(rest)
        && !after.trim_start().starts_with(';')
    {
        return Some(("class", name.to_owned(), Some(format!("class {name}"))));
    }
    if let Some(rest) = strip_word(trimmed, "struct")
        && let Some((name, after)) = take_ident(rest)
        && !after.trim_start().starts_with(';')
    {
        return Some(("struct", name.to_owned(), Some(format!("struct {name}"))));
    }
    if let Some(rest) = strip_word(trimmed, "enum") {
        let rest = strip_word(rest, "class").unwrap_or(rest);
        if let Some((name, _)) = take_ident(rest) {
            return Some(("enum", name.to_owned(), None));
        }
    }
    // 顶层／类内函数定义或原型：含 `(` 且不以控制流关键字开头。
    if let Some(paren) = trimmed.find('(')
        && (trimmed.ends_with('{') || trimmed.ends_with(')') || trimmed.ends_with("const"))
    {
        let before = trimmed[..paren].trim_end();
        let name = before
            .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '~'))
            .next()?;
        if !name.is_empty()
            && !matches!(
                name,
                "if" | "for" | "while" | "switch" | "catch" | "return" | "sizeof"
            )
            && before.contains(char::is_whitespace)
        {
            return Some(("fn", name.to_owned(), None));
        }
    }
    None
}

// ---------- Markdown ----------

fn extract_markdown(text: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    let mut fence: Option<(u8, usize)> = None;

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = raw_line.trim_start();
        if let Some((ch, len)) = fence {
            let count = trimmed.bytes().take_while(|&b| b == ch).count();
            if count >= len && trimmed[count..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        let ticks = trimmed.bytes().take_while(|&b| b == b'`').count();
        if ticks >= 3 {
            fence = Some((b'`', ticks));
            continue;
        }
        let tildes = trimmed.bytes().take_while(|&b| b == b'~').count();
        if tildes >= 3 {
            fence = Some((b'~', tildes));
            continue;
        }
        let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
        if (1..=6).contains(&hashes) && trimmed.as_bytes().get(hashes) == Some(&b' ') {
            let title = trimmed[hashes + 1..].trim().trim_end_matches('#').trim();
            if !title.is_empty() {
                symbols.push(Symbol {
                    line: line_no,
                    kind: "heading",
                    name: title.to_owned(),
                    parent: None,
                    signature: clip_sig(trimmed),
                    depth: hashes - 1,
                });
            }
        }
    }
    symbols
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_extracts_items_impl_methods_and_skips_comments_and_raw_strings() {
        let src = r####"
// fn commented_out() {}
/*
  struct InBlockComment;
  /* nested fn nested_comment() {} */
  enum StillComment {}
*/
pub const SAMPLE: &str = r#"
pub fn inside_raw_string() {}
"#;

pub mod parser {
    pub struct Config {
        pub limit: usize,
    }

    pub trait Runner {
        fn run(&self) -> bool;
    }

    impl Runner for Config {
        fn run(&self) -> bool {
            fn local_helper() {}
            true
        }
    }

    pub(crate) async unsafe fn serve<'a>(cfg: &'a Config) {}
}

macro_rules! quick {
    () => {};
}
"####;
        let syms = extract(Language::Rust, src);
        let summary: Vec<_> = syms.iter().map(|s| (s.kind, s.qualified_name())).collect();
        assert_eq!(
            summary,
            vec![
                ("const", "SAMPLE".into()),
                ("mod", "parser".into()),
                ("struct", "mod parser > Config".into()),
                ("trait", "mod parser > Runner".into()),
                ("fn", "trait Runner > run".into()),
                ("impl", "mod parser > Runner for Config".into()),
                ("fn", "impl Runner for Config > run".into()),
                ("fn", "mod parser > serve".into()),
                ("macro", "quick".into()),
            ]
        );
    }

    #[test]
    fn jsts_python_go_and_markdown_extract_clean_symbols() {
        let js = r#"
// function nope() {}
export class Editor {
    async save(doc) {
        return doc;
    }
}
export const renderView = (x) => x;
export function helper() {}
"#;
        let js_syms: Vec<_> = extract(Language::JsTs, js)
            .into_iter()
            .map(|s| (s.kind, s.qualified_name()))
            .collect();
        assert_eq!(
            js_syms,
            vec![
                ("class", "Editor".into()),
                ("fn", "class Editor > save".into()),
                ("fn", "renderView".into()),
                ("fn", "helper".into()),
            ]
        );

        let py = r#""""
def inside_docstring():
    pass
"""
class Service:
    async def fetch(self):
        def inner():
            pass
        return 1

def top_level():
    pass
"#;
        let py_syms: Vec<_> = extract(Language::Python, py)
            .into_iter()
            .map(|s| (s.kind, s.qualified_name()))
            .collect();
        assert_eq!(
            py_syms,
            vec![
                ("class", "Service".into()),
                ("fn", "class Service > fetch".into()),
                ("fn", "top_level".into()),
            ]
        );

        let go = r#"
package demo
type Server struct{}
func (s *Server) Start() error { return nil }
func New() *Server { return nil }
"#;
        let go_syms: Vec<_> = extract(Language::Go, go)
            .into_iter()
            .map(|s| (s.kind, s.qualified_name()))
            .collect();
        assert_eq!(
            go_syms,
            vec![
                ("struct", "Server".into()),
                ("fn", "Server > Start".into()),
                ("fn", "New".into()),
            ]
        );

        let md = "# Title\n```md\n# Not A Heading\n```\n## Section\n";
        let md_syms: Vec<_> = extract(Language::Markdown, md)
            .into_iter()
            .map(|s| (s.kind, s.name, s.depth))
            .collect();
        assert_eq!(
            md_syms,
            vec![
                ("heading", "Title".into(), 0),
                ("heading", "Section".into(), 1),
            ]
        );
    }
}
