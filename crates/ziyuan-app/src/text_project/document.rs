//! Conservative TXT/CommonMark import. Offsets always address the untouched UTF-8 input.
//!
//! This is not a full Aozora or HTML dialect parser. Implicit ruby accepts a contiguous
//! run of Han graphemes (including supplementary Han); 々/〻 extend an existing run,
//! never start one. Combining marks stay with their grapheme, and kana ends the run.
//! HTML is data, not a rendering instruction: only the documented single-pair ruby
//! subset is interpreted, and all other HTML is retained literally with a warning.
//! The HTML subset uses exact lowercase, attribute-free tags on one line, one rt,
//! an optional rb around the base, and optional rp before/after rt. Entities inside
//! HTML ruby are deliberately not decoded. Markdown escapes/entities are decoded by
//! CommonMark but are not used to construct ruby with guessed byte coordinates.
use std::ops::Range;

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

use super::{Result, invalid};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFormat {
    Text,
    Markdown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ParseOptions {
    pub ruby: bool,
    pub heading_level: Option<u8>,
    #[serde(skip_serializing_if = "TextParagraphs::is_default")]
    pub text_paragraphs: TextParagraphs,
    #[serde(skip_serializing_if = "is_false")]
    pub split_long_paragraphs: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextParagraphs {
    #[default]
    BlankLines,
    Lines,
}
impl TextParagraphs {
    fn is_default(&self) -> bool {
        *self == Self::BlankLines
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Inline {
    Text {
        text: String,
    },
    Ruby {
        base: String,
        reading: String,
        byte_start: usize,
        byte_end: usize,
    },
    Emphasis {
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentParagraph {
    pub byte_start: usize,
    pub byte_end: usize,
    pub text: String,
    pub inlines: Vec<Inline>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportWarning {
    pub byte_start: usize,
    pub byte_end: usize,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedChapter {
    pub title: String,
    pub byte_start: usize,
    pub byte_end: usize,
    pub paragraphs: Vec<DocumentParagraph>,
    pub warnings: Vec<ImportWarning>,
}

/// Derive translation material without modifying, normalizing, or re-encoding the source.
pub fn parse_document(
    bytes: &[u8],
    format: SourceFormat,
    options: &ParseOptions,
    fallback_title: &str,
) -> Result<Vec<ParsedChapter>> {
    if !matches!(options.heading_level, None | Some(1 | 2)) {
        return Err(invalid("分章标题级别仅支持 1 或 2；或不设置以整文件导入"));
    }
    let source = std::str::from_utf8(bytes)
        .map_err(|_| invalid(format!("{fallback_title}: 仅接受 UTF-8 TXT/Markdown")))?;
    let bom = usize::from(source.starts_with('\u{feff}')) * 3;
    let mut boundaries = Vec::new();
    if format == SourceFormat::Markdown
        && let Some(level) = options.heading_level
    {
        let mut nested = 0usize;
        let mut heading: Option<(usize, String)> = None;
        for (event, range) in Parser::new(&source[bom..]).into_offset_iter() {
            match event {
                Event::Start(Tag::BlockQuote(_) | Tag::List(_) | Tag::Item) => nested += 1,
                Event::End(TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::Item) => nested -= 1,
                Event::Start(Tag::Heading { level: h, .. })
                    if nested == 0 && heading_number(h) == level =>
                {
                    heading = Some((range.start + bom, String::new()));
                }
                Event::Text(t) | Event::Code(t) if heading.is_some() => {
                    heading.as_mut().unwrap().1.push_str(&t);
                }
                Event::SoftBreak | Event::HardBreak if heading.is_some() => {
                    heading.as_mut().unwrap().1.push(' ');
                }
                Event::End(TagEnd::Heading(_)) => {
                    if let Some((start, title)) = heading.take() {
                        boundaries.push((start, range.end + bom, title));
                    }
                }
                _ => {}
            }
        }
    }
    let mut sections = Vec::new();
    if boundaries.is_empty() {
        sections.push((0, bom, source.len(), fallback_title.to_owned()));
    } else {
        let first = boundaries[0].0;
        if !source[bom..first].trim().is_empty() {
            sections.push((0, bom, first, fallback_title.to_owned()));
        }
        for (index, (start, body, title)) in boundaries.iter().enumerate() {
            let end = boundaries.get(index + 1).map_or(source.len(), |b| b.0);
            sections.push((
                if index == 0 && sections.is_empty() {
                    0
                } else {
                    *start
                },
                *body,
                end,
                title.clone(),
            ));
        }
    }
    let mut chapters = Vec::new();
    for (start, body, end, title) in sections {
        let mut chapter = ParsedChapter {
            title,
            byte_start: start,
            byte_end: end,
            paragraphs: Vec::new(),
            warnings: Vec::new(),
        };
        match format {
            SourceFormat::Text => text_paragraphs(source, body..end, options, &mut chapter),
            SourceFormat::Markdown => markdown_paragraphs(source, body..end, options, &mut chapter),
        }
        if format == SourceFormat::Text && options.split_long_paragraphs {
            split_long_text(source, options, &mut chapter)?;
        }
        if !chapter.paragraphs.iter().any(|p| !p.text.trim().is_empty()) {
            return Err(invalid(format!(
                "{}: 原文为空或没有可翻译正文（字节 {start}..{end}）；请补充正文、调整分章级别或改用普通文本导入",
                chapter.title
            )));
        }
        chapters.push(chapter);
    }
    Ok(chapters)
}

fn heading_number(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn warning(out: &mut Vec<ImportWarning>, range: Range<usize>, message: &str) {
    out.push(ImportWarning {
        byte_start: range.start,
        byte_end: range.end,
        message: message.into(),
    });
}

fn visible(inline: &Inline) -> &str {
    match inline {
        Inline::Text { text } | Inline::Emphasis { text } => text,
        Inline::Ruby { base, .. } => base,
    }
}

fn append(out: &mut Vec<Inline>, text: &str, emphasis: bool) {
    if text.is_empty() {
        return;
    }
    let text = text.replace("\r\n", "\n");
    match (out.last_mut(), emphasis) {
        (Some(Inline::Text { text: last }), false)
        | (Some(Inline::Emphasis { text: last }), true) => last.push_str(&text),
        (_, true) => out.push(Inline::Emphasis { text }),
        (_, false) => out.push(Inline::Text { text }),
    }
}

fn finish(out: &mut ParsedChapter, range: Range<usize>, inlines: Vec<Inline>) {
    let text: String = inlines.iter().map(visible).collect();
    if !text.trim().is_empty() {
        out.paragraphs.push(DocumentParagraph {
            byte_start: range.start,
            byte_end: range.end,
            text,
            inlines,
        });
    }
}

fn text_paragraphs(
    source: &str,
    range: Range<usize>,
    options: &ParseOptions,
    out: &mut ParsedChapter,
) {
    let mut start = None;
    let mut end = range.start;
    let mut offset = range.start;
    for line in source[range.clone()].split_inclusive('\n') {
        let body = match line.strip_suffix('\n') {
            Some(body) => body.strip_suffix('\r').unwrap_or(body),
            None => line,
        };
        if body.trim().is_empty() {
            if let Some(begin) = start.take() {
                let inlines = parse_inlines(
                    &source[begin..end],
                    begin,
                    options.ruby,
                    false,
                    &mut out.warnings,
                );
                finish(out, begin..end, inlines);
            }
        } else {
            start.get_or_insert(offset);
            end = offset + body.len();
            if options.text_paragraphs == TextParagraphs::Lines {
                let begin = start.take().expect("nonempty line has a start");
                let inlines = parse_inlines(
                    &source[begin..end],
                    begin,
                    options.ruby,
                    false,
                    &mut out.warnings,
                );
                finish(out, begin..end, inlines);
            }
        }
        offset += line.len();
    }
    if let Some(begin) = start {
        let inlines = parse_inlines(
            &source[begin..end],
            begin,
            options.ruby,
            false,
            &mut out.warnings,
        );
        finish(out, begin..end, inlines);
    }
}

/// Only an explicitly opted-in TXT import gains working paragraphs. Source ranges remain
/// contiguous inside each original paragraph, and annotated spans are indivisible.
fn split_long_text(
    source: &str,
    options: &ParseOptions,
    chapter: &mut ParsedChapter,
) -> Result<()> {
    const BUDGET: usize = super::DEFAULT_BLOCK_BYTES - 1024;
    fn encoded_size(p: &DocumentParagraph) -> Result<usize> {
        let annotated = p.inlines.iter().any(|i| !matches!(i, Inline::Text { .. }));
        Ok(super::PromptBlock {
            schema_version: 1,
            block_id: "c00000000-b00000000".into(),
            paragraphs: vec![super::BlockParagraph {
                id: "c00000000-p00000000".into(),
                text: p.text.clone(),
                inlines: if annotated {
                    p.inlines.clone()
                } else {
                    Vec::new()
                },
            }],
        }
        .serialized()?
        .len())
    }
    fn paragraph(source: &str, start: usize, end: usize, ruby: bool) -> DocumentParagraph {
        let inlines = parse_inlines(&source[start..end], start, ruby, false, &mut Vec::new());
        DocumentParagraph {
            byte_start: start,
            byte_end: end,
            text: inlines.iter().map(visible).collect(),
            inlines,
        }
    }
    fn ends_sentence(prefix: &str) -> bool {
        let trimmed = prefix.trim_end_matches(['」', '』', '”', '’', '）', ')']);
        trimmed.ends_with(['。', '！', '？', '.', '!', '?', '\n'])
            && (trimmed.len() == prefix.len() || !trimmed.ends_with('\n'))
    }
    let mut out = Vec::new();
    for original in std::mem::take(&mut chapter.paragraphs) {
        let annotated = original
            .inlines
            .iter()
            .any(|i| !matches!(i, Inline::Text { .. }));
        if !annotated && original.text.len() * 6 + 256 <= BUDGET {
            out.push(original);
            continue;
        }
        if encoded_size(&original)? <= BUDGET {
            out.push(original);
            continue;
        }
        let start = original.byte_start;
        let end = original.byte_end;
        let mut protected = Vec::new();
        // Collect exact protected spans using the inline parser itself so malformed markup
        // never blocks splitting and unparsed HTML is never reinterpreted across a cut.
        if options.ruby {
            parse_inlines_inner(
                &source[start..end],
                start,
                true,
                false,
                &mut Vec::new(),
                Some(&mut protected),
            );
        }
        protected.sort_by_key(|r| r.start);
        let mut protected_index = 0;
        let boundaries: Vec<usize> = source[start..end]
            .grapheme_indices(true)
            .map(|(i, g)| start + i + g.len())
            .filter(|n| {
                while protected.get(protected_index).is_some_and(|r| r.end <= *n) {
                    protected_index += 1;
                }
                !protected
                    .get(protected_index)
                    .is_some_and(|r| r.start < *n && *n < r.end)
            })
            .collect();
        let mut begin = start;
        let before = out.len();
        while begin < end {
            let from = boundaries.partition_point(|n| *n <= begin);
            let until = boundaries
                .partition_point(|n| *n <= begin.saturating_add(super::DEFAULT_BLOCK_BYTES));
            let available = &boundaries[from..until];
            let mut low = 0;
            let mut high = available.len();
            // Encoding length is monotonic at safe inline boundaries.
            while low < high {
                let middle = (low + high) / 2;
                if encoded_size(&paragraph(source, begin, available[middle], options.ruby))?
                    <= BUDGET
                {
                    low = middle + 1;
                } else {
                    high = middle;
                }
            }
            if low == 0 {
                return Err(invalid(format!(
                    "{}：一处注音、强调标记或连续字形过长，无法完整放入译稿工作段；请缩短该处标记，或关闭注音解析后重新预览（原文字节 {begin}..{end}）",
                    chapter.title
                )));
            }
            let maximum = available[low - 1];
            // Prefer a sentence end in the latter half; otherwise retain a whole grapheme.
            let cut = if maximum == end {
                end
            } else {
                available[..low]
                    .iter()
                    .rev()
                    .copied()
                    .find(|n| *n >= begin + (maximum - begin) / 2 && ends_sentence(&source[..*n]))
                    .unwrap_or(maximum)
            };
            out.push(paragraph(source, begin, cut, options.ruby));
            begin = cut;
        }
        if out[before..]
            .iter()
            .map(|p| p.text.as_str())
            .collect::<String>()
            != original.text
        {
            return Err(invalid(format!(
                "{}：长段中的复杂标记无法完整拆分；请调整该处标记后重新预览（原文字节 {start}..{end}）",
                chapter.title
            )));
        }
        warning(
            &mut chapter.warnings,
            start..end,
            &format!(
                "长段已按句末或完整字形分为 {} 个译稿工作段；原文副本保持不变。",
                out.len() - before
            ),
        );
    }
    chapter.paragraphs = out;
    Ok(())
}

fn markdown_paragraphs(
    source: &str,
    section: Range<usize>,
    options: &ParseOptions,
    out: &mut ParsedChapter,
) {
    let mut current: Option<(Range<usize>, Vec<Inline>)> = None;
    let mut emphasis = 0usize;
    let mut code = false;
    let mut consumed = 0usize;
    for (event, local) in Parser::new(&source[section.clone()]).into_offset_iter() {
        let range = local.start + section.start..local.end + section.start;
        match event {
            Event::Start(Tag::CodeBlock(_)) => {
                code = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                code = false;
            }
            _ if code => {}
            Event::Start(Tag::Paragraph | Tag::Heading { .. }) => {
                if let Some((r, items)) = current.take() {
                    finish(out, r, items);
                }
                current = Some((range, Vec::new()));
            }
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_)) => {
                if let Some((mut r, items)) = current.take() {
                    r.end = range.end;
                    finish(out, r, items);
                }
            }
            Event::End(TagEnd::Item) => {
                if let Some((r, items)) = current.take() {
                    finish(out, r, items);
                }
            }
            Event::Start(Tag::Emphasis | Tag::Strong) => emphasis += 1,
            Event::End(TagEnd::Emphasis | TagEnd::Strong) => emphasis -= 1,
            Event::Start(Tag::Image { .. }) => warning(
                &mut out.warnings,
                range,
                "图片不交给模型；仅保留替代文字，不读取图片地址",
            ),
            Event::Text(text) if range.start >= consumed => {
                let (r, items) = current.get_or_insert_with(|| (range.clone(), Vec::new()));
                r.end = r.end.max(range.end);
                // Escapes/entities can change byte lengths. Do not invent a source mapping.
                if source[range.clone()] == *text {
                    items.extend(parse_inlines(
                        &text,
                        range.start,
                        options.ruby,
                        emphasis > 0,
                        &mut out.warnings,
                    ));
                } else {
                    append(items, &text, emphasis > 0);
                }
            }
            Event::Html(_)
                if range.start >= consumed && !source[range.clone()].starts_with("<ruby") =>
            {
                warning(
                    &mut out.warnings,
                    range.clone(),
                    "未解释的 HTML 块；原样保留，仅作为不可信文本显示",
                );
                let (r, items) = current.get_or_insert_with(|| (range.clone(), Vec::new()));
                r.end = r.end.max(range.end);
                append(items, &source[range], emphasis > 0);
            }
            Event::Html(_) | Event::InlineHtml(_) if range.start >= consumed => {
                let mut end = range.end;
                end = end.max(html_extent(&source[range.start..section.end]) + range.start);
                consumed = end;
                let (r, items) = current.get_or_insert_with(|| (range.start..end, Vec::new()));
                r.end = r.end.max(end);
                items.extend(parse_inlines(
                    &source[range.start..end],
                    range.start,
                    options.ruby,
                    emphasis > 0,
                    &mut out.warnings,
                ));
            }
            Event::SoftBreak | Event::HardBreak if range.start >= consumed => {
                if let Some((r, items)) = current.as_mut() {
                    r.end = r.end.max(range.end);
                    append(items, "\n", emphasis > 0);
                }
            }
            Event::Rule => warning(
                &mut out.warnings,
                range,
                "分隔线保留在原件中，不作为翻译正文",
            ),
            Event::Start(Tag::HtmlBlock) => {
                if let Some((r, items)) = current.take() {
                    finish(out, r, items);
                }
                current = Some((range, Vec::new()));
            }
            Event::End(TagEnd::HtmlBlock) => {
                if let Some((r, items)) = current.take() {
                    finish(out, r, items);
                }
            }
            Event::Code(_) => {} // Inline code, like fenced/indented code, stays only in raw.
            Event::FootnoteReference(_)
            | Event::TaskListMarker(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_) => {
                warning(
                    &mut out.warnings,
                    range.clone(),
                    "暂不支持此 Markdown 节点；按字面保留",
                );
                let (_, items) = current.get_or_insert_with(|| (range.clone(), Vec::new()));
                append(items, &source[range], false);
            }
            _ => {}
        }
    }
    if let Some((r, items)) = current {
        finish(out, r, items);
    }
}

fn han(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x2ffff | 0x30000..=0x323af)
}

fn implicit_base(text: &str) -> usize {
    let mut start = text.len();
    let mut has_han = false;
    for (index, grapheme) in text.grapheme_indices(true).rev() {
        let c = grapheme.chars().next().unwrap();
        if han(c) {
            has_han = true;
            start = index;
        } else if matches!(c, '々' | '〻') {
            start = index;
        } else {
            break;
        }
    }
    if !has_han {
        return text.len();
    }
    // Iteration signs may extend a run, but cannot be its first grapheme.
    while start < text.len() {
        let c = text[start..].chars().next().unwrap();
        if !matches!(c, '々' | '〻') {
            break;
        }
        start += c.len_utf8();
    }
    start
}

fn boundary(text: &str, index: usize) -> bool {
    // Ask at the requested offset; scanning every preceding grapheme for every ruby
    // turned a whole-novel TXT import into quadratic work.
    unicode_segmentation::GraphemeCursor::new(index, text.len(), true)
        .is_boundary(text, 0)
        .unwrap_or(false)
}

fn pair(text: &str, open: usize) -> Option<(usize, &str)> {
    if !text[open..].starts_with('《') || text[open..].starts_with("《《") {
        return None;
    }
    let begin = open + '《'.len_utf8();
    let end = begin + text[begin..].find('》')?;
    let reading = &text[begin..end];
    let after = end + '》'.len_utf8();
    if reading.trim().is_empty()
        || reading.contains(['\r', '\n', '《', '<', '>'])
        || !boundary(text, begin)
        || !boundary(text, after)
    {
        return None;
    }
    Some((after, reading))
}

/// Consume an entire candidate so malformed/nested markup cannot be partly reinterpreted.
fn html_extent(text: &str) -> usize {
    let text = &text[..text.find('\n').unwrap_or(text.len())];
    if text.starts_with("<ruby") {
        return text
            .find("</ruby>")
            .map_or(text.len(), |end| end + "</ruby>".len());
    }
    if text.starts_with("<!--") {
        return text.find("-->").map_or(text.len(), |end| end + 3);
    }
    // A tiny tokenizer, not tag stripping: quoted '>' is part of an attribute.
    let mut quote = None;
    let mut tag_end = text.len();
    for (index, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if q == c => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            (None, '>') => {
                tag_end = index + 1;
                break;
            }
            _ => {}
        }
    }
    let name_end = text[1..]
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .map_or(text.len(), |i| i + 1);
    let name = &text[1..name_end];
    if !name.is_empty() && !text[..tag_end].ends_with("/>") {
        let close = format!("</{name}>");
        if let Some(end) = text[tag_end..].find(&close) {
            return tag_end + end + close.len();
        }
    }
    tag_end
}

fn simple_html(text: &str) -> Option<(&str, &str)> {
    let body = text.strip_prefix("<ruby>")?.strip_suffix("</ruby>")?;
    let (before, after) = body.split_once("<rt>")?;
    let (reading, suffix) = after.split_once("</rt>")?;
    let (base, prefix_rp) = if let Some(rest) = before.strip_prefix("<rb>") {
        rest.split_once("</rb>")?
    } else if let Some((base, _)) = before.split_once("<rp>") {
        (base, &before[base.len()..])
    } else {
        (before, "")
    };
    fn rp(text: &str) -> bool {
        text.is_empty()
            || text
                .strip_prefix("<rp>")
                .and_then(|t| t.strip_suffix("</rp>"))
                .is_some_and(|t| !t.contains(['<', '>', '\n', '\r']))
    }
    if base.is_empty()
        || reading.is_empty()
        || base.contains(['<', '>', '\n', '\r', '&'])
        || reading.contains(['<', '>', '\n', '\r', '&'])
        || !rp(prefix_rp)
        || !rp(suffix)
    {
        return None;
    }
    Some((base, reading))
}

fn parse_inlines(
    text: &str,
    offset: usize,
    ruby: bool,
    emphasis: bool,
    warnings: &mut Vec<ImportWarning>,
) -> Vec<Inline> {
    parse_inlines_inner(text, offset, ruby, emphasis, warnings, None)
}

fn parse_inlines_inner(
    text: &str,
    offset: usize,
    ruby: bool,
    emphasis: bool,
    warnings: &mut Vec<ImportWarning>,
    mut protected: Option<&mut Vec<Range<usize>>>,
) -> Vec<Inline> {
    let mut out = Vec::new();
    let mut cursor = 0;
    let mut plain = 0;
    while cursor < text.len() {
        let tail = &text[cursor..];
        let mut parsed = None;
        if tail.starts_with('<')
            && tail
                .chars()
                .nth(1)
                .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '/' | '!' | '?'))
        {
            let end = cursor + html_extent(tail);
            append(&mut out, &text[plain..cursor], emphasis);
            let literal = &text[cursor..end];
            if let Some((base, reading)) = (ruby && boundary(text, end))
                .then(|| simple_html(literal))
                .flatten()
            {
                if let Some(ranges) = protected.as_deref_mut() {
                    ranges.push(offset + cursor..offset + end);
                }
                out.push(Inline::Ruby {
                    base: base.into(),
                    reading: reading.into(),
                    byte_start: offset + cursor,
                    byte_end: offset + end,
                });
            } else {
                if ruby
                    && (literal.contains('《') || literal.contains("<ruby"))
                    && let Some(ranges) = protected.as_deref_mut()
                {
                    ranges.push(offset + cursor..offset + end);
                }
                warning(
                    warnings,
                    offset + cursor..offset + end,
                    "未解释的 HTML 或复杂/破损 ruby；原样保留，仅作为不可信文本显示",
                );
                append(&mut out, literal, emphasis);
            }
            cursor = end;
            plain = cursor;
            continue;
        }
        if ruby && tail.starts_with("《《") {
            if let Some(close) = tail[6..].find("》》") {
                let end = cursor + 6 + close + 6;
                if boundary(text, cursor + 6)
                    && boundary(text, end)
                    && !tail[6..6 + close].contains(['\n', '\r', '《', '》'])
                {
                    parsed = Some((
                        cursor,
                        end,
                        Inline::Emphasis {
                            text: tail[6..6 + close].into(),
                        },
                    ));
                }
            }
            if parsed.is_none() {
                warning(
                    warnings,
                    offset + cursor..offset + text.len(),
                    "破损的日文强调标记；保留字面文字",
                );
                // Do not reinterpret its second opening bracket as a ruby opener.
                cursor += tail.graphemes(true).take(2).map(str::len).sum::<usize>();
                continue;
            }
        } else if ruby && (tail.starts_with('｜') || tail.starts_with('|')) {
            let base_start = cursor + tail.chars().next().unwrap().len_utf8();
            if let Some(relative) = text[base_start..].find('《') {
                let open = base_start + relative;
                let base = &text[base_start..open];
                if !base.is_empty()
                    && boundary(text, base_start)
                    && boundary(text, open)
                    && !base.contains(['\n', '\r', '|', '｜', '》', '<', '>'])
                    && let Some((end, reading)) = pair(text, open)
                {
                    parsed = Some((
                        cursor,
                        end,
                        Inline::Ruby {
                            base: base.into(),
                            reading: reading.into(),
                            byte_start: offset + cursor,
                            byte_end: offset + end,
                        },
                    ));
                }
            }
        } else if ruby && tail.starts_with('《') {
            let start = plain + implicit_base(&text[plain..cursor]);
            if start < cursor
                && let Some((end, reading)) = pair(text, cursor)
            {
                parsed = Some((
                    start,
                    end,
                    Inline::Ruby {
                        base: text[start..cursor].into(),
                        reading: reading.into(),
                        byte_start: offset + start,
                        byte_end: offset + end,
                    },
                ));
            }
            if parsed.is_none() {
                warning(
                    warnings,
                    offset + cursor..offset + text.len(),
                    "无法确定 ruby 基底或注音未闭合；保留字面文字",
                );
            }
        } else if ruby && tail.starts_with("［＃") {
            warning(
                warnings,
                offset + cursor..offset + text.len(),
                "暂不支持青空文库批注；保留字面文字",
            );
        }
        if let Some((start, end, inline)) = parsed {
            if let Some(ranges) = protected.as_deref_mut() {
                ranges.push(offset + start..offset + end);
            }
            append(&mut out, &text[plain..start], emphasis);
            out.push(inline);
            cursor = end;
            plain = end;
        } else {
            // Advancing by grapheme, not scalar, prevents splitting combining sequences.
            cursor += tail.graphemes(true).next().unwrap().len();
        }
    }
    append(&mut out, &text[plain..], emphasis);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(
        text: &str,
        format: SourceFormat,
        ruby: bool,
        heading_level: Option<u8>,
    ) -> Vec<ParsedChapter> {
        parse_document(
            text.as_bytes(),
            format,
            &ParseOptions {
                ruby,
                heading_level,
                ..Default::default()
            },
            "原題",
        )
        .unwrap()
    }

    fn assert_ranges(source: &str, chapters: &[ParsedChapter]) {
        for chapter in chapters {
            assert!(chapter.byte_start <= chapter.byte_end);
            assert!(source.get(chapter.byte_start..chapter.byte_end).is_some());
            for p in &chapter.paragraphs {
                assert!(p.byte_start >= chapter.byte_start && p.byte_end <= chapter.byte_end);
                assert!(source.get(p.byte_start..p.byte_end).is_some());
                assert_eq!(p.text, p.inlines.iter().map(visible).collect::<String>());
                for item in &p.inlines {
                    if let Inline::Ruby {
                        byte_start,
                        byte_end,
                        ..
                    } = item
                    {
                        assert!(*byte_start >= p.byte_start && *byte_end <= p.byte_end);
                        assert!(source.get(*byte_start..*byte_end).is_some());
                    }
                }
            }
            for w in &chapter.warnings {
                assert!(w.byte_start >= chapter.byte_start && w.byte_end <= chapter.byte_end);
                assert!(!w.message.is_empty());
                assert!(source.get(w.byte_start..w.byte_end).is_some());
            }
        }
    }

    #[test]
    fn document_default_text_preserves_old_punctuation_bom_and_crlf_ranges() {
        let source = "\u{feff}漢字（かんじ）(old)｜字《じ》《《強調》》\r\n次行\r\n \r\n終。\r\n";
        let chapters = parse(source, SourceFormat::Text, false, Some(1));
        assert_eq!(chapters.len(), 1); // TXT never guesses chapter headings.
        assert_eq!(chapters[0].byte_start, 0);
        assert_eq!(chapters[0].byte_end, source.len());
        let p = &chapters[0].paragraphs[0];
        assert_eq!(p.byte_start, 3);
        assert_eq!(
            &source[p.byte_start..p.byte_end],
            "漢字（かんじ）(old)｜字《じ》《《強調》》\r\n次行"
        );
        assert_eq!(p.text, "漢字（かんじ）(old)｜字《じ》《《強調》》\n次行");
        assert_eq!(chapters[0].paragraphs[1].text, "終。");
        assert_ranges(source, &chapters);
    }

    #[test]
    fn document_mixed_ruby_and_emphasis_have_independent_exact_source_ranges() {
        let source = "前｜食べる《たべる》と|かな《カナ》漢字《かんじ》《《強調》》後";
        let chapters = parse(source, SourceFormat::Text, true, None);
        let p = &chapters[0].paragraphs[0];
        assert_eq!(p.text, "前食べるとかな漢字強調後");
        let ruby: Vec<_> = p
            .inlines
            .iter()
            .filter_map(|i| match i {
                Inline::Ruby {
                    base,
                    reading,
                    byte_start,
                    byte_end,
                } => Some((
                    base.as_str(),
                    reading.as_str(),
                    &source[*byte_start..*byte_end],
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            ruby,
            [
                ("食べる", "たべる", "｜食べる《たべる》"),
                ("かな", "カナ", "|かな《カナ》"),
                ("漢字", "かんじ", "漢字《かんじ》")
            ]
        );
        assert!(p.inlines.contains(&Inline::Emphasis {
            text: "強調".into()
        }));
        assert!(chapters[0].warnings.is_empty());
        assert_ranges(source, &chapters);
    }

    #[test]
    fn document_graphemes_kana_iteration_and_no_nfkc() {
        let source = "かな漢々《かんかん》、々字《じ》、かな《かな》、｜か\u{3099}ｶﾅ《ガカナ》、漢\u{301}《かん》、漢《かん》\u{301}";
        let chapters = parse(source, SourceFormat::Text, true, None);
        assert_eq!(
            chapters[0].paragraphs[0].text,
            "かな漢々、々字、かな《かな》、か\u{3099}ｶﾅ、漢\u{301}、漢《かん》\u{301}"
        );
        assert_ranges(source, &chapters);
        assert!(!chapters[0].warnings.is_empty());
    }

    #[test]
    fn document_simple_html_optional_rb_rp_and_multiple_adjacent_groups() {
        for format in [SourceFormat::Text, SourceFormat::Markdown] {
            let source = "前<ruby><rb>漢字</rb><rp>（</rp><rt>かんじ</rt><rp>）</rp></ruby><ruby>名<rt>な</rt></ruby>後";
            let chapters = parse(source, format.clone(), true, None);
            assert_eq!(chapters[0].paragraphs[0].text, "前漢字名後");
            assert!(chapters[0].warnings.is_empty());
            assert_ranges(source, &chapters);
            let source = "<ruby>漢<rp>(</rp><rt>かん</rt><rp>)</rp></ruby>";
            let chapters = parse(source, format, true, None);
            assert_eq!(chapters[0].paragraphs[0].text, "漢");
        }
    }

    #[test]
    fn document_unsupported_html_is_literal_and_warned_not_stripped() {
        let fixtures = [
            "<ruby class='x'>漢<rt>かん</rt></ruby>",
            "<ruby>漢<rt>かん</rt>字<rt>じ</rt></ruby>",
            "<ruby>漢<rt><ruby>字<rt>じ</rt></ruby></rt></ruby>",
            "<ruby>漢<rt>かん</rt>",
            "<script src='https://invalid.test/x'>alert(1)</script>",
            "<script>漢《かん》</script>",
            "<span title='a>b'>字《じ》</span>",
            "<!-- >漢《かん》 -->",
            "<img src='https://invalid.test/x' onerror='evil()'>",
        ];
        for format in [SourceFormat::Text, SourceFormat::Markdown] {
            for source in fixtures {
                let chapters = parse(source, format.clone(), true, None);
                assert_eq!(chapters[0].paragraphs[0].text, source);
                assert!(!chapters[0].warnings.is_empty(), "{source}");
                assert!(
                    !chapters[0].paragraphs[0]
                        .inlines
                        .iter()
                        .any(|i| matches!(i, Inline::Ruby { .. }))
                );
                assert_ranges(source, &chapters);
            }
        }
    }

    #[test]
    fn document_broken_ruby_and_unsupported_annotations_are_literal() {
        for source in ["字《じ", "｜字《", "《《強調》", "［＃傍点］字"] {
            let chapters = parse(source, SourceFormat::Text, true, None);
            assert_eq!(chapters[0].paragraphs[0].text, source);
            assert!(!chapters[0].warnings.is_empty());
            assert_ranges(source, &chapters);
        }
    }

    #[test]
    fn document_markdown_preamble_and_only_top_level_headings_split() {
        let source = "\u{feff}前書き\r\n\r\n# 一\r\n本文\r\n\r\n```md\r\n# 偽\r\n```\r\n\r\n> # 引用\r\n> 引文\r\n\r\n- # 項目\r\n\r\n# 二\r\n後文\r\n";
        let chapters = parse(source, SourceFormat::Markdown, false, Some(1));
        assert_eq!(
            chapters
                .iter()
                .map(|c| c.title.as_str())
                .collect::<Vec<_>>(),
            ["原題", "一", "二"]
        );
        assert_eq!(chapters[0].paragraphs[0].text, "前書き");
        assert_eq!(chapters[0].byte_end, source.find("# 一").unwrap());
        assert_eq!(chapters[1].byte_start, chapters[0].byte_end);
        assert_eq!(
            chapters[1]
                .paragraphs
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>(),
            ["本文", "引用", "引文", "項目"]
        );
        assert_ranges(source, &chapters);
    }

    #[test]
    fn document_markdown_rendering_omits_code_and_targets_but_warns_for_images() {
        let source = "# 題\n\n*強調*と**太字** [label](https://example.test/漢《かん》) `字《じ》`\n次行\n\n- 項目\n- ![代替](https://invalid.test/x)\n\n> 引用\n\n---\n\n```\nnot translated\n```\n";
        let chapters = parse(source, SourceFormat::Markdown, true, None);
        assert_eq!(
            chapters[0]
                .paragraphs
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>(),
            ["題", "強調と太字 label \n次行", "項目", "代替", "引用"]
        );
        let p = &chapters[0].paragraphs[1];
        assert!(p.inlines.contains(&Inline::Emphasis {
            text: "強調".into()
        }));
        assert!(p.inlines.contains(&Inline::Emphasis {
            text: "太字".into()
        }));
        assert_eq!(chapters[0].warnings.len(), 2);
        assert_ranges(source, &chapters);
    }

    #[test]
    fn document_level_two_and_setext_titles_keep_raw_spelling() {
        let source = "第一\n----\n\n字《じ》\n\n## 第二\n\n文";
        let chapters = parse(source, SourceFormat::Markdown, true, Some(2));
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0].title, "第一");
        assert_eq!(chapters[1].title, "第二");
        assert_eq!(chapters[0].paragraphs[0].text, "字");
        assert_eq!(
            &source[chapters[0].byte_start..chapters[0].byte_end],
            "第一\n----\n\n字《じ》\n\n"
        );
        assert_ranges(source, &chapters);
    }

    #[test]
    fn document_rejects_invalid_utf8_options_and_empty_chapters() {
        assert!(
            parse_document(&[0xff], SourceFormat::Text, &ParseOptions::default(), "題").is_err()
        );
        assert!(
            parse_document(
                b"text",
                SourceFormat::Text,
                &ParseOptions {
                    ruby: false,
                    heading_level: Some(3),
                    ..Default::default()
                },
                "題"
            )
            .is_err()
        );
        for (source, format, level) in [
            ("\u{feff}\r\n  ", SourceFormat::Text, None),
            ("# 空\n\n# 次\n文", SourceFormat::Markdown, Some(1)),
            ("# 空\n\n```\ncode\n```", SourceFormat::Markdown, Some(1)),
            (
                "```\ncode\n```\n\n# 次\n文",
                SourceFormat::Markdown,
                Some(1),
            ),
        ] {
            let error = parse_document(
                source.as_bytes(),
                format,
                &ParseOptions {
                    ruby: true,
                    heading_level: level,
                    ..Default::default()
                },
                "題",
            )
            .unwrap_err();
            assert!(error.to_string().contains("正文"));
        }
    }

    #[test]
    fn document_public_serialization_contract() {
        assert_eq!(
            serde_json::to_string(&SourceFormat::Markdown).unwrap(),
            "\"markdown\""
        );
        assert_eq!(
            serde_json::from_str::<ParseOptions>("{}").unwrap(),
            ParseOptions::default()
        );
        assert!(serde_json::from_str::<ParseOptions>("{\"unknown\":true}").is_err());
        let parsed = parse("字《じ》", SourceFormat::Text, true, None);
        let json = serde_json::to_string(&parsed).unwrap();
        assert!(json.contains("\"kind\":\"ruby\""));
        assert_eq!(
            serde_json::from_str::<Vec<ParsedChapter>>(&json).unwrap(),
            parsed
        );
    }

    #[test]
    fn document_malformed_html_never_consumes_following_paragraphs() {
        let source = "前<ruby>漢<rt>かん\n\n次段</ruby>終\n\n<ruby>字<rt>じ</rt></ruby>\n<ruby>名<rt>な</rt></ruby>";
        let chapters = parse(source, SourceFormat::Markdown, true, None);
        assert_eq!(
            chapters[0]
                .paragraphs
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>(),
            ["前<ruby>漢<rt>かん", "次段</ruby>終", "字\n名"]
        );
        assert_ranges(source, &chapters);
    }

    #[test]
    fn split_long_text_ignores_multiline_broken_emphasis_and_keeps_closing_quotes() {
        let broken = format!("《《未閉じた強調\n{}\n》》", "春の風。".repeat(8000));
        let chapters = parse_document(
            broken.as_bytes(),
            SourceFormat::Text,
            &ParseOptions {
                ruby: true,
                text_paragraphs: TextParagraphs::BlankLines,
                split_long_paragraphs: true,
                ..Default::default()
            },
            "長文",
        )
        .unwrap();
        assert!(chapters[0].paragraphs.len() > 1);
        assert_eq!(
            chapters[0]
                .paragraphs
                .iter()
                .map(|p| p.text.as_str())
                .collect::<String>(),
            broken
        );

        let quoted = format!(
            "{}「dialogue。」{}「next。」",
            "春。".repeat(7900),
            "夏。".repeat(4000)
        );
        let quote_chapters = parse_document(
            quoted.as_bytes(),
            SourceFormat::Text,
            &ParseOptions {
                ruby: true,
                text_paragraphs: TextParagraphs::Lines,
                split_long_paragraphs: true,
                ..Default::default()
            },
            "会話",
        )
        .unwrap();
        assert!(quote_chapters[0].paragraphs.len() > 1);
        assert!(!quote_chapters[0].paragraphs[1].text.starts_with("」"));

        let html_ruby = format!(
            "{}<script>漢《かん》</script>{}",
            "あ".repeat(15_990),
            "い".repeat(4000)
        );
        let html_chapters = parse_document(
            html_ruby.as_bytes(),
            SourceFormat::Text,
            &ParseOptions {
                ruby: true,
                text_paragraphs: TextParagraphs::Lines,
                split_long_paragraphs: true,
                ..Default::default()
            },
            "脚本",
        )
        .unwrap();
        assert_eq!(
            html_chapters[0]
                .paragraphs
                .iter()
                .map(|p| p.text.as_str())
                .collect::<String>(),
            html_ruby
        );
    }

    #[test]
    fn document_adversarial_fragments_keep_ranges_in_bounds_and_complete_inlines() {
        let fragments = [
            "字",
            "漢《かん》",
            "｜かな《かな》",
            "《《強調》》",
            "《《",
            "》》",
            "<ruby>",
            "</ruby>",
            "<rt>かん</rt>",
            "\u{301}",
            "\u{600}",
            "`code`",
            "\r\n",
            "\n\n",
            "- ",
            "**",
            "![alt](x)",
        ];
        for first in fragments {
            for second in fragments {
                let source = format!("前{first}{second}後");
                for format in [SourceFormat::Text, SourceFormat::Markdown] {
                    let chapters = parse(&source, format, true, None);
                    assert_ranges(&source, &chapters);
                }
            }
        }
    }
}
