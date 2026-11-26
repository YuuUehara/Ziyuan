//! Rust 与 JavaScript / TypeScript 符号大纲提取。

use super::{
    BraceLexState, BraceSpanTracker, SpanExpect, Symbol, clip_sig, count_braces,
    has_unclosed_paren, skip_generics, strip_leading_vis_and_attrs, strip_word, take_ident,
};

// ---------- Rust ----------

pub(super) fn extract_rust(text: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    let mut lex = BraceLexState::default();
    let mut spans = BraceSpanTracker::default();
    let mut depth: usize = 0;
    let mut containers: Vec<(String, usize)> = Vec::new();
    let mut pending_container: Option<String> = None;

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let code = lex.strip_line(raw_line, true, false);
        let trimmed = code.trim();
        let (opens, closes) = count_braces(&code);

        let leading_closes = trimmed.bytes().take_while(|&b| b == b'}').count();
        let effective_depth = depth.saturating_sub(leading_closes);
        spans.close_leading(&mut symbols, effective_depth, line_no);
        while containers
            .last()
            .is_some_and(|(_, enter)| *enter > effective_depth)
        {
            containers.pop();
        }

        if let Some(name) = pending_container.as_ref() {
            if opens > 0 {
                containers.push((name.clone(), effective_depth + 1));
                pending_container = None;
            } else if trimmed.contains(';') {
                pending_container = None;
            }
        }

        let in_container = containers.len() == effective_depth;
        if in_container && !trimmed.is_empty() && !trimmed.starts_with('#') {
            let parent = containers.last().map(|(name, _)| name.clone());
            let sym_depth = containers.len();
            if let Some((kind, name, container_label)) = parse_rust_item(trimmed) {
                let sym_idx = symbols.len();
                symbols.push(Symbol {
                    line: line_no,
                    end_line: line_no,
                    kind,
                    name,
                    parent,
                    signature: clip_sig(trimmed),
                    depth: sym_depth,
                });
                let expect = match kind {
                    "const" | "static" | "type" => SpanExpect::SemiOnly,
                    _ => SpanExpect::BlockOrSemi,
                };
                spans.register(sym_idx, effective_depth, expect);
                if let Some(label) = container_label {
                    if opens > closes {
                        containers.push((label, effective_depth + 1));
                    } else if opens == 0 && !trimmed.contains(';') {
                        pending_container = Some(label);
                    }
                }
            }
        }

        depth = spans.advance(
            &mut symbols,
            &trimmed[leading_closes..],
            effective_depth,
            line_no,
        );
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
    let mut saw_const = false;
    loop {
        if let Some(rest) = strip_word(s, "default") {
            s = rest;
        } else if let Some(rest) = strip_word(s, "unsafe") {
            s = rest;
        } else if let Some(rest) = strip_word(s, "async") {
            s = rest;
        } else if let Some(rest) = strip_word(s, "const") {
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

pub(super) fn extract_jsts(text: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    let mut lex = BraceLexState::default();
    let mut spans = BraceSpanTracker::default();
    let mut depth: usize = 0;
    let mut containers: Vec<(String, usize)> = Vec::new();

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let code = lex.strip_line(raw_line, false, true);
        let trimmed = code.trim();
        let (opens, closes) = count_braces(&code);

        let leading_closes = trimmed.bytes().take_while(|&b| b == b'}').count();
        let effective_depth = depth.saturating_sub(leading_closes);
        spans.close_leading(&mut symbols, effective_depth, line_no);
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
                let sym_idx = symbols.len();
                symbols.push(Symbol {
                    line: line_no,
                    end_line: line_no,
                    kind,
                    name,
                    parent,
                    signature: clip_sig(trimmed),
                    depth: sym_depth,
                });
                let expect = if opens > 0 {
                    SpanExpect::BlockOrSemi
                } else if matches!(kind, "const" | "type") {
                    if trimmed.ends_with('=')
                        || trimmed.ends_with('|')
                        || trimmed.ends_with('&')
                        || trimmed.ends_with('(')
                        || trimmed.ends_with(',')
                    {
                        SpanExpect::BlockOrSemi
                    } else {
                        SpanExpect::SingleLine
                    }
                } else if kind == "fn"
                    && trimmed.contains("=>")
                    && !trimmed.ends_with("=>")
                    && !has_unclosed_paren(trimmed)
                {
                    SpanExpect::SingleLine
                } else {
                    SpanExpect::BlockOrSemi
                };
                spans.register(sym_idx, effective_depth, expect);
                if let Some(label) = container_label
                    && opens > closes
                {
                    containers.push((label, effective_depth + 1));
                }
            }
        }

        depth = spans.advance(
            &mut symbols,
            &trimmed[leading_closes..],
            effective_depth,
            line_no,
        );
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
        if trimmed.starts_with("export ") {
            return Some(("const", name.to_owned(), None));
        }
    }

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
