//! Python、Go、C/C++ 与 Markdown 符号大纲提取。

use super::{
    BraceLexState, BraceSpanTracker, SpanExpect, Symbol, clip_sig, count_braces,
    has_unclosed_paren, skip_generics, strip_word, take_ident,
};

// ---------- Python ----------

pub(super) fn extract_python(text: &str) -> Vec<Symbol> {
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut triple_quote: Option<&'static str> = None;
    let mut classes: Vec<(String, usize)> = Vec::new();
    let mut fn_indents: Vec<usize> = Vec::new();
    let mut open_spans: Vec<(usize, usize, usize)> = Vec::new();

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let was_in_triple = triple_quote.is_some();
        let code = strip_python_line(raw_line, &mut triple_quote);
        let trimmed = code.trim();
        let raw_indent = raw_line
            .bytes()
            .take_while(|b| *b == b' ' || *b == b'\t')
            .map(|b| if b == b'\t' { 4 } else { 1 })
            .sum::<usize>();

        if trimmed.is_empty() {
            if !raw_line.trim().is_empty() {
                for &mut (_, sym_indent, ref mut last_line) in &mut open_spans {
                    if was_in_triple || raw_indent > sym_indent {
                        *last_line = line_no;
                    }
                }
            }
            continue;
        }
        let indent = raw_indent;

        while let Some(&(sym_idx, sym_indent, last_line)) = open_spans.last() {
            if indent <= sym_indent {
                symbols[sym_idx].end_line = last_line;
                open_spans.pop();
            } else {
                break;
            }
        }
        for &mut (_, _, ref mut last_line) in &mut open_spans {
            *last_line = line_no;
        }

        while classes.last().is_some_and(|(_, ci)| indent <= *ci) {
            classes.pop();
        }
        while fn_indents.last().is_some_and(|fi| indent <= *fi) {
            fn_indents.pop();
        }

        let parent = classes.last().map(|(n, _)| n.clone());
        let sym_depth = classes.len();
        if let Some(rest) = strip_word(trimmed, "class")
            && let Some((name, _)) = take_ident(rest)
        {
            if fn_indents.is_empty() {
                let sym_idx = symbols.len();
                symbols.push(Symbol {
                    line: line_no,
                    end_line: line_no,
                    kind: "class",
                    name: name.to_owned(),
                    parent,
                    signature: clip_sig(trimmed.trim_end_matches(':')),
                    depth: sym_depth,
                });
                open_spans.push((sym_idx, indent, line_no));
                classes.push((format!("class {name}"), indent));
            }
            continue;
        }
        let fn_rest = strip_word(trimmed, "async")
            .and_then(|r| strip_word(r, "def"))
            .or_else(|| strip_word(trimmed, "def"));
        if let Some(rest) = fn_rest
            && let Some((name, _)) = take_ident(rest)
        {
            let expected_member = classes.last().is_some_and(|(_, ci)| indent > *ci);
            if fn_indents.is_empty() && (indent == 0 || expected_member) {
                let sym_idx = symbols.len();
                symbols.push(Symbol {
                    line: line_no,
                    end_line: line_no,
                    kind: "fn",
                    name: name.to_owned(),
                    parent,
                    signature: clip_sig(trimmed.trim_end_matches(':')),
                    depth: sym_depth,
                });
                open_spans.push((sym_idx, indent, line_no));
                fn_indents.push(indent);
            }
        }
    }
    for (sym_idx, _, last_line) in open_spans {
        symbols[sym_idx].end_line = last_line;
    }
    symbols
}

fn strip_python_line(line: &str, triple: &mut Option<&'static str>) -> String {
    strip_python_line_with_pad(line, triple, false)
}

pub(super) fn strip_python_line_with_pad(
    line: &str,
    triple: &mut Option<&'static str>,
    pad: bool,
) -> String {
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
            if pad {
                out.push(' ');
            }
            *triple = Some("\"\"\"");
            i += 3;
            continue;
        }
        if line[i..].starts_with("'''") {
            if pad {
                out.push(' ');
            }
            *triple = Some("'''");
            i += 3;
            continue;
        }
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            if pad {
                out.push(' ');
            }
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

pub(super) fn extract_go(text: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    let mut lex = BraceLexState::default();
    let mut spans = BraceSpanTracker::default();
    let mut depth: usize = 0;

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let code = lex.strip_line(raw_line, false, true);
        let trimmed = code.trim();
        let (opens, _) = count_braces(&code);

        let leading_closes = trimmed.bytes().take_while(|&b| b == b'}').count();
        let effective_depth = depth.saturating_sub(leading_closes);
        spans.close_leading(&mut symbols, effective_depth, line_no);

        if effective_depth == 0 && !trimmed.is_empty() {
            let mut added = None;
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
                        let sym_idx = symbols.len();
                        symbols.push(Symbol {
                            line: line_no,
                            end_line: line_no,
                            kind: "fn",
                            name: name.to_owned(),
                            parent,
                            signature: clip_sig(trimmed),
                            depth: 0,
                        });
                        added = Some((sym_idx, opens > 0 || has_unclosed_paren(trimmed)));
                    }
                } else if let Some((name, _)) = take_ident(rest) {
                    let sym_idx = symbols.len();
                    symbols.push(Symbol {
                        line: line_no,
                        end_line: line_no,
                        kind: "fn",
                        name: name.to_owned(),
                        parent: None,
                        signature: clip_sig(trimmed),
                        depth: 0,
                    });
                    added = Some((sym_idx, opens > 0 || has_unclosed_paren(trimmed)));
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
                let sym_idx = symbols.len();
                symbols.push(Symbol {
                    line: line_no,
                    end_line: line_no,
                    kind,
                    name: name.to_owned(),
                    parent: None,
                    signature: clip_sig(trimmed),
                    depth: 0,
                });
                added = Some((sym_idx, opens > 0));
            } else if let Some(rest) = strip_word(trimmed, "const")
                && let Some((name, _)) = take_ident(rest)
            {
                let sym_idx = symbols.len();
                symbols.push(Symbol {
                    line: line_no,
                    end_line: line_no,
                    kind: "const",
                    name: name.to_owned(),
                    parent: None,
                    signature: clip_sig(trimmed),
                    depth: 0,
                });
                added = Some((sym_idx, opens > 0));
            }
            if let Some((sym_idx, track_block)) = added {
                spans.register(
                    sym_idx,
                    effective_depth,
                    if track_block {
                        SpanExpect::BlockOrSemi
                    } else {
                        SpanExpect::SingleLine
                    },
                );
            }
        }

        depth = spans.advance(
            &mut symbols,
            &trimmed[leading_closes..],
            effective_depth,
            line_no,
        );
    }
    symbols
}

// ---------- C / C++ ----------

pub(super) fn extract_ccpp(text: &str) -> Vec<Symbol> {
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut lex = BraceLexState::default();
    let mut spans = BraceSpanTracker::default();
    let mut depth: usize = 0;
    let mut containers: Vec<(String, usize)> = Vec::new();
    let mut macro_cont: Option<usize> = None;

    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        if let Some(sym_idx) = macro_cont {
            symbols[sym_idx].end_line = line_no;
            if !raw_line.trim_end().ends_with('\\') {
                macro_cont = None;
            }
            continue;
        }
        let code = lex.strip_line(raw_line, false, false);
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

        if let Some(rest) = trimmed.strip_prefix("#define")
            && let Some((name, _)) = take_ident(rest.trim_start())
        {
            let sym_idx = symbols.len();
            symbols.push(Symbol {
                line: line_no,
                end_line: line_no,
                kind: "macro",
                name: name.to_owned(),
                parent: None,
                signature: clip_sig(trimmed),
                depth: 0,
            });
            if raw_line.trim_end().ends_with('\\') {
                macro_cont = Some(sym_idx);
            }
            continue;
        }

        let in_container = containers.len() == effective_depth;
        if in_container && !trimmed.is_empty() && !trimmed.starts_with('#') {
            let parent = containers.last().map(|(n, _)| n.clone());
            let sym_depth = containers.len();
            if let Some((kind, name, container_label)) = parse_ccpp_item(trimmed) {
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
                spans.register(sym_idx, effective_depth, SpanExpect::BlockOrSemi);
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

pub(super) fn extract_markdown(text: &str) -> Vec<Symbol> {
    let lines: Vec<&str> = text.lines().collect();
    let mut symbols = Vec::new();
    let mut fence: Option<(u8, usize)> = None;

    for (idx, raw_line) in lines.iter().enumerate() {
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
                    end_line: line_no,
                    kind: "heading",
                    name: title.to_owned(),
                    parent: None,
                    signature: clip_sig(trimmed),
                    depth: hashes - 1,
                });
            }
        }
    }
    let total = lines.len();
    for i in 0..symbols.len() {
        let depth_i = symbols[i].depth;
        let mut end = symbols[i + 1..]
            .iter()
            .find(|next| next.depth <= depth_i)
            .map_or(total, |next| next.line.saturating_sub(1));
        while end > symbols[i].line && lines[end - 1].trim().is_empty() {
            end -= 1;
        }
        symbols[i].end_line = end.max(symbols[i].line);
    }
    symbols
}
