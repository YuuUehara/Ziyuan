//! 改动预览：在 [`FileChange::hunks`] 的段落之上，给每行配上原、新行号，按语言着色
//! （syntect），并把成对的删、增行做词级比对（similar），标出真正改了的字词。
//! Web 与 TUI 共用这一份结果；段号与 [`crate::render_diffs`]、逐段准奏所用的一致。

use serde::Serialize;
use similar::{Algorithm, DiffTag, capture_diff_slices};
use std::sync::OnceLock;
use syntect::parsing::{ParseState, Scope, ScopeStack, ScopeStackOp, SyntaxReference, SyntaxSet};
use ziyuan_core::FileChange;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewFile {
    pub path: String,
    pub created: bool,
    pub hunks: Vec<PreviewHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewHunk {
    /// 段号：跨文件连续、从 1 起。
    pub number: u32,
    pub lines: Vec<PreviewLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewLine {
    /// `+` 新增、`-` 删去、空格为上下文。
    pub kind: char,
    /// 原文行号（新增行没有）。
    pub old: Option<usize>,
    /// 新文行号（删去行没有）。
    pub new: Option<usize>,
    pub spans: Vec<PreviewSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreviewSpan {
    pub text: String,
    /// 语法类别：`keyword`、`string`、`comment`、`number`、`constant`、`function`、`type`、`macro`；
    /// 不着色为 `None`。
    pub token: Option<&'static str>,
    /// 成对的删、增行中真正改动的字词。
    pub changed: bool,
}

/// 各处改动的预览，一处一份，与 [`crate::render_diffs`] 同序。
pub fn preview_changes(files: &[FileChange]) -> Vec<PreviewFile> {
    let mut number = 0;
    files
        .iter()
        .map(|change| {
            let syntax = syntax_for(&change.path);
            let old_tokens = change.before.as_deref().map(|t| highlight(t, syntax));
            let new_tokens = highlight(&change.after, syntax);
            let mut shift: isize = 0; // 新行号 − 原行号，随段累计。
            let hunks = change
                .hunks()
                .into_iter()
                .map(|hunk| {
                    number += 1;
                    let mut old = hunk.start;
                    let mut new = if change.before.is_none() {
                        1
                    } else {
                        (hunk.start as isize + shift) as usize
                    };
                    let mut lines: Vec<PreviewLine> = hunk
                        .lines
                        .iter()
                        .map(|(kind, text)| {
                            let (o, n) = match kind {
                                '+' => (None, Some(new)),
                                '-' => (Some(old), None),
                                _ => (Some(old), Some(new)),
                            };
                            if o.is_some() {
                                old += 1;
                            }
                            if n.is_some() {
                                new += 1;
                            }
                            let tokens = match (kind, &old_tokens) {
                                ('-', Some(t)) => o.and_then(|o| t.get(o - 1)),
                                ('-', None) => None,
                                _ => n.and_then(|n| new_tokens.get(n - 1)),
                            };
                            PreviewLine {
                                kind: *kind,
                                old: o,
                                new: n,
                                spans: spans(text, tokens.map(Vec::as_slice).unwrap_or(&[]), &[]),
                            }
                        })
                        .collect();
                    emphasize_pairs(&mut lines, &hunk.lines);
                    shift += hunk.lines.iter().filter(|l| l.0 == '+').count() as isize
                        - hunk.lines.iter().filter(|l| l.0 == '-').count() as isize;
                    PreviewHunk { number, lines }
                })
                .collect();
            PreviewFile {
                path: change.path.clone(),
                created: change.before.is_none(),
                hunks,
            }
        })
        .collect()
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn syntax_for(path: &str) -> Option<&'static SyntaxReference> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let extension = name.rsplit_once('.').map_or(name, |(_, e)| e);
    syntaxes().find_syntax_by_extension(extension)
}

/// 每行的语法类别区间（字节偏移）。认不出语言时为空。
fn highlight(
    text: &str,
    syntax: Option<&SyntaxReference>,
) -> Vec<Vec<(usize, usize, &'static str)>> {
    let Some(syntax) = syntax else {
        return Vec::new();
    };
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut out = Vec::new();
    for line in text.split_inclusive('\n') {
        let owned;
        let line = if line.ends_with('\n') {
            line
        } else {
            owned = format!("{line}\n");
            &owned
        };
        let Ok(ops) = state.parse_line(line, syntaxes()) else {
            return Vec::new();
        };
        let mut ranges = Vec::new();
        let mut at = 0;
        for (pos, op) in ops.into_iter().chain([(line.len(), ScopeStackOp::Noop)]) {
            if pos > at
                && let Some(token) = classify(stack.as_slice())
            {
                ranges.push((at, pos, token));
            }
            at = pos;
            if stack.apply(&op).is_err() {
                return Vec::new();
            }
        }
        out.push(ranges);
    }
    out
}

/// 由内向外找第一个认得的作用域。
fn classify(stack: &[Scope]) -> Option<&'static str> {
    const TABLE: &[(&str, &str)] = &[
        ("comment", "comment"),
        ("string", "string"),
        ("constant.numeric", "number"),
        ("constant.character", "string"),
        ("constant", "constant"),
        ("entity.name.function", "function"),
        ("support.function", "function"),
        ("variable.function", "function"),
        ("support.macro", "macro"),
        ("entity.name.macro", "macro"),
        ("entity.name.type", "type"),
        ("entity.name.class", "type"),
        ("support.type", "type"),
        ("support.class", "type"),
        ("storage.type", "keyword"),
        ("storage.modifier", "keyword"),
        ("keyword.operator", ""),
        ("keyword", "keyword"),
        ("markup.heading", "keyword"),
        ("markup.bold", "keyword"),
        ("markup.raw", "string"),
    ];
    stack
        .iter()
        .rev()
        .find_map(|scope| {
            let name = scope.build_string();
            TABLE
                .iter()
                .find(|(prefix, _)| name.starts_with(prefix))
                .map(|(_, token)| *token)
        })
        .filter(|t| !t.is_empty())
}

/// 把一行切成片段：语法区间与改动区间的边界都切开。
fn spans(
    text: &str,
    tokens: &[(usize, usize, &'static str)],
    changed: &[(usize, usize)],
) -> Vec<PreviewSpan> {
    let mut cuts: Vec<usize> = vec![0, text.len()];
    for &(a, b, _) in tokens {
        cuts.extend([a.min(text.len()), b.min(text.len())]);
    }
    for &(a, b) in changed {
        cuts.extend([a, b]);
    }
    cuts.retain(|&c| text.is_char_boundary(c));
    cuts.sort_unstable();
    cuts.dedup();
    let mut out: Vec<PreviewSpan> = Vec::new();
    for pair in cuts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let token = tokens.iter().find(|t| t.0 <= a && b <= t.1).map(|t| t.2);
        let is_changed = changed.iter().any(|c| c.0 <= a && b <= c.1);
        match out.last_mut() {
            Some(last) if last.token == token && last.changed == is_changed => {
                last.text.push_str(&text[a..b]);
            }
            _ => out.push(PreviewSpan {
                text: text[a..b].to_owned(),
                token,
                changed: is_changed,
            }),
        }
    }
    out
}

/// 一段中相邻的「删 n 行、增 m 行」逐行配对，做词级比对；相似度太低的不标（整行重写）。
fn emphasize_pairs(lines: &mut [PreviewLine], raw: &[(char, String)]) {
    let mut i = 0;
    while i < raw.len() {
        let dels = raw[i..].iter().take_while(|l| l.0 == '-').count();
        let adds = raw[i + dels..].iter().take_while(|l| l.0 == '+').count();
        if dels == 0 {
            i += 1;
            continue;
        }
        for k in 0..dels.min(adds) {
            let (old, new) = (&raw[i + k].1, &raw[i + dels + k].1);
            if let Some((old_changed, new_changed)) = word_diff(old, new) {
                for (index, text, changed) in
                    [(i + k, old, old_changed), (i + dels + k, new, new_changed)]
                {
                    let tokens = tokens_of(&lines[index].spans);
                    lines[index].spans = spans(text, &tokens, &changed);
                }
            }
        }
        i += dels + adds;
    }
}

/// 从已切好的片段还原语法区间。
fn tokens_of(spans: &[PreviewSpan]) -> Vec<(usize, usize, &'static str)> {
    let mut at = 0;
    let mut out = Vec::new();
    for span in spans {
        let end = at + span.text.len();
        if let Some(token) = span.token {
            out.push((at, end, token));
        }
        at = end;
    }
    out
}

/// 两行的改动区间（各自的字节偏移）。词元：ASCII 字母数字串、空白串、其余每字一个。
/// 相同的词元不足四成时返回 `None`：整行都算改了，不必逐词标出。
fn word_diff(old: &str, new: &str) -> Option<(Vec<Range>, Vec<Range>)> {
    let (a, b) = (words(old), words(new));
    let texts = |w: &[(usize, usize)], s: &str| -> Vec<String> {
        w.iter().map(|&(x, y)| s[x..y].to_owned()).collect()
    };
    let (ta, tb) = (texts(&a, old), texts(&b, new));
    let ops = capture_diff_slices(Algorithm::Myers, &ta, &tb);
    let same: usize = ops
        .iter()
        .filter(|op| op.tag() == DiffTag::Equal)
        .map(|op| op.old_range().len())
        .sum();
    if same * 10 < ta.len().max(tb.len()) * 4 {
        return None;
    }
    let range = |w: &[(usize, usize)], r: std::ops::Range<usize>| {
        (!r.is_empty()).then(|| (w[r.start].0, w[r.end - 1].1))
    };
    let (mut oc, mut nc) = (Vec::new(), Vec::new());
    for op in &ops {
        if op.tag() != DiffTag::Equal {
            oc.extend(range(&a, op.old_range()));
            nc.extend(range(&b, op.new_range()));
        }
    }
    Some((oc, nc))
}

/// 字节区间 `(起, 止)`。
type Range = (usize, usize);

fn words(s: &str) -> Vec<Range> {
    let class = |c: char| {
        if c.is_ascii_alphanumeric() || c == '_' {
            1
        } else if c.is_whitespace() {
            2
        } else {
            0
        }
    };
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut last = None;
    for (i, c) in s.char_indices() {
        let k = class(c);
        match (out.last_mut(), last) {
            (Some(w), Some(prev)) if prev == k && k != 0 => w.1 = i + c.len_utf8(),
            _ => out.push((i, i + c.len_utf8())),
        }
        last = Some(k);
    }
    out
}
