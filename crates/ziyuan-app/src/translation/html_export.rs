//! Self-contained, inert finished prose with every supported target mark.
use super::{DraftParagraph, Result, TargetMarkKind};
use std::collections::BTreeMap;
fn escape(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '"' => "&quot;".into(),
            '\'' => "&#39;".into(),
            c => c.to_string(),
        })
        .collect()
}
pub(super) fn marked_html(paragraph: &DraftParagraph) -> Result<String> {
    super::markup::validate(std::slice::from_ref(paragraph))?;
    let mut ranges: BTreeMap<(usize, usize), Vec<&TargetMarkKind>> = BTreeMap::new();
    for mark in &paragraph.marks {
        ranges
            .entry((mark.start, mark.end))
            .or_default()
            .push(&mark.kind);
    }
    let mut out = String::new();
    let mut cursor = 0;
    for ((start, end), marks) in ranges {
        out.push_str(&escape(&paragraph.text[cursor..start]));
        let mut inner = escape(&paragraph.text[start..end]);
        for mark in &marks {
            match mark {
                TargetMarkKind::Strong {} => inner = format!("<strong>{inner}</strong>"),
                TargetMarkKind::Emphasis {} => inner = format!("<em>{inner}</em>"),
                TargetMarkKind::EmphasisDots {} => {
                    inner = format!("<span class=\"emphasis-dots\">{inner}</span>")
                }
                TargetMarkKind::Ruby { .. } => {}
            }
        }
        for mark in marks {
            if let TargetMarkKind::Ruby { reading } = mark {
                inner = format!("<ruby>{inner}<rt>{}</rt></ruby>", escape(reading));
            }
        }
        out.push_str(&inner);
        cursor = end;
    }
    out.push_str(&escape(&paragraph.text[cursor..]));
    Ok(out)
}
pub(super) fn document(body: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\"><title>定稿</title><style>body{{max-width:48rem;margin:auto;padding:2rem;font:1.1rem/1.9 serif;overflow-wrap:anywhere}}p{{white-space:pre-wrap}}.emphasis-dots{{text-emphasis:filled dot;-webkit-text-emphasis:filled dot}}rt{{font-size:.55em}}section{{break-after:page}}</style></head><body>{body}</body></html>\n"
    )
}
