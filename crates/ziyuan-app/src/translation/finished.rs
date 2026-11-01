//! Read-only exports: immutable bytes, not paths to reread while streaming a download.
use super::{BlockState, Result, Translations, invalid, read_plain_opt, sha256_hex};
use crate::text_project::{Chapter, Inline, Project, PromptBlock, SourceFormat};
use crate::{AppError, Session};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use ziyuan_core::TaskId;
use ziyuan_storage::EventStore;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    #[default]
    Txt,
    Markdown,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportLayout {
    #[default]
    Chapters,
    Book,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExportOptions {
    /// Empty selects all; otherwise checked for duplicates/unknown IDs, then project order.
    pub chapters: Vec<String>,
    pub format: ExportFormat,
    pub layout: ExportLayout,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExportFile {
    pub name: String,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExportBundle {
    pub files: Vec<ExportFile>,
    pub manifest: Value,
    pub warnings: Vec<String>,
}

fn selection<'a>(project: &'a Project, ids: &[String]) -> Result<Vec<&'a Chapter>> {
    let mut wanted = BTreeSet::new();
    for id in ids {
        if !wanted.insert(id.as_str()) {
            return Err(invalid(format!("重复章节：{id}")));
        }
        if !project.chapters.iter().any(|c| c.id == *id) {
            return Err(invalid(format!("未知章节：{id}")));
        }
    }
    let selected: Vec<_> = project
        .chapters
        .iter()
        .filter(|c| wanted.is_empty() || wanted.contains(c.id.as_str()))
        .collect();
    if selected.is_empty() {
        return Err(invalid("项目尚无章节，不能导出"));
    }
    Ok(selected)
}

fn title(c: &Chapter) -> &str {
    c.parsing.as_ref().map_or(&c.source_name, |p| &p.title)
}

fn bundle(files: Vec<ExportFile>, mut manifest: Value, warnings: Vec<String>) -> ExportBundle {
    manifest["files"] = json!(
        files
            .iter()
            .map(|f| json!({
                "name": f.name, "byte_len": f.bytes.len(), "sha256": sha256_hex(&f.bytes),
            }))
            .collect::<Vec<_>>()
    );
    manifest["warnings"] = json!(warnings);
    let snapshot = sha256_hex(&serde_json::to_vec(&manifest).expect("JSON value serializes"));
    manifest["snapshot_sha256"] = json!(snapshot);
    ExportBundle {
        files,
        manifest,
        warnings,
    }
}

impl Translations {
    /// Retrieve each unchanged archived file once, even when it contains several chapters.
    /// Never substitutes extracted chapter text for the original bytes.
    pub fn export_originals(&self, project: &str, chapters: &[String]) -> Result<ExportBundle> {
        let first = self.original_snapshot(project, chapters)?;
        if first != self.original_snapshot(project, chapters)? {
            return Err(invalid("取回原件期间内容有变，请重试"));
        }
        Ok(first)
    }

    fn original_snapshot(&self, id: &str, ids: &[String]) -> Result<ExportBundle> {
        let p = self.project(id)?;
        let chapters = selection(&p, ids)?;
        let mut files = Vec::new();
        let mut names: BTreeMap<String, String> = BTreeMap::new();
        let mut records = Vec::new();
        for c in chapters {
            let name = if let Some(name) = names.get(&c.original_path) {
                name.clone()
            } else {
                let bytes = read_plain_opt(&self.store_dir(id)?.join(&c.original_path))?
                    .ok_or_else(|| invalid("归档原件不存在"))?;
                if sha256_hex(&bytes) != c.sha256 {
                    return Err(invalid("归档原件在读取期间有变"));
                }
                let ext = match c.parsing.as_ref().map(|p| &p.format) {
                    Some(SourceFormat::Markdown) => "md",
                    _ => "txt",
                };
                let name = format!("source-{:04}.{ext}", files.len() + 1);
                names.insert(c.original_path.clone(), name.clone());
                files.push(ExportFile {
                    name: name.clone(),
                    bytes,
                });
                name
            };
            records.push(json!({"chapter":c.id,"title":title(c),"file":name,
                "sha256":c.sha256,"parsing":c.parsing}));
        }
        Ok(bundle(
            files,
            json!({"schema_version":1,"export_rules_version":1,
            "kind":"original","project":id,"metadata":p.metadata,
            "chapters":records,"project_sha256":sha256_hex(&serde_json::to_vec(&p)?)}),
            vec!["原件按归档文件去重；选择其中一章时，该文件内其他章节也随完整原件取回。".into()],
        ))
    }
}

impl<S: EventStore> Session<S> {
    /// Replays and rechecks both sides of collection; no event, source, or draft is written.
    /// An exported snapshot remains valid evidence of that point in time, not a live draft.
    pub fn export_finished(
        &mut self,
        root: &Path,
        project: &str,
        options: &ExportOptions,
    ) -> std::result::Result<ExportBundle, AppError> {
        self.reload()?;
        let first = self.finished_snapshot(root, project, options)?;
        self.reload()?;
        if first != self.finished_snapshot(root, project, options)? {
            return Err(invalid("导出期间原文、定稿或审批状态有变，请重新预览").into());
        }
        Ok(first)
    }

    fn finished_snapshot(
        &self,
        root: &Path,
        id: &str,
        options: &ExportOptions,
    ) -> std::result::Result<ExportBundle, AppError> {
        let t = Translations::new(root);
        let p = t.project(id)?;
        let chapters = selection(&p, &options.chapters)?;
        let statuses = self.translation_status(root, id)?;
        let context = t.current_context(id)?;
        let mut failures = Vec::new();
        let mut records = Vec::new();
        let mut rendered = Vec::new();
        let mut warnings = Vec::new();
        for c in chapters {
            let mut texts = BTreeMap::new();
            let mut blocks = Vec::new();
            if c.blocks.is_empty() {
                failures.push(format!("{}：没有提示块", c.id));
            }
            for b in &c.blocks {
                let state = &statuses
                    .iter()
                    .find(|s| s.block == b.block_id)
                    .ok_or_else(|| invalid("缺少块状态"))?
                    .state;
                let BlockState::Finalized {
                    task,
                    translation_hash,
                } = state
                else {
                    let detail = match state {
                        BlockState::Stale { reasons, .. } => reasons
                            .iter()
                            .map(|r| r.describe())
                            .collect::<Vec<_>>()
                            .join("；"),
                        BlockState::Failed { reason, .. } | BlockState::Uncertain { reason } => {
                            reason.clone()
                        }
                        _ => String::new(),
                    };
                    failures.push(format!(
                        "{} / {}：{}{}",
                        c.id,
                        b.block_id,
                        state.label(),
                        if detail.is_empty() {
                            String::new()
                        } else {
                            format!("（{detail}）")
                        }
                    ));
                    continue;
                };
                let writer = self
                    .court()
                    .task(TaskId(*task))
                    .ok_or_else(|| invalid("定稿来源诏令不存在"))?;
                let link = writer
                    .translation
                    .as_ref()
                    .ok_or_else(|| invalid("定稿来源缺少译书关联"))?;
                let zhupi_hash = t.block_zhupi_hash(&p, &b.block_id)?;
                if link.zhupi_hash != zhupi_hash {
                    failures.push(format!(
                        "{} / {}：朱批自下诏后有变，须重修定稿",
                        c.id, b.block_id
                    ));
                    continue;
                }
                let bytes = t
                    .read_translation(id, &b.block_id)?
                    .ok_or_else(|| invalid("定稿译文不存在"))?;
                if sha256_hex(&bytes) != *translation_hash {
                    return Err(invalid("定稿译文在读取期间有变").into());
                }
                let report = t
                    .texts()
                    .check(id, &b.block_id, &bytes)
                    .map_err(super::TranslationError::from)?;
                if !report.valid {
                    failures.push(format!("{} / {}：译文结构无效", c.id, b.block_id));
                    continue;
                }
                let candidate: PromptBlock =
                    serde_json::from_slice(&bytes).map_err(super::TranslationError::from)?;
                for para in candidate.paragraphs {
                    texts.insert(
                        para.id,
                        translated_text(&para.text, &para.inlines, options.format)?,
                    );
                }
                blocks.push(json!({"block":b.block_id,"task":task,"source_sha256":link.source_hash,
                    "translation_sha256":translation_hash,"context_version":link.context_version,
                    "zhupi_sha256":zhupi_hash,"signoff":self.court().translation_signoff(id,&b.block_id)}));
            }
            if c.paragraphs.iter().any(|p| !texts.contains_key(&p.id)) {
                continue;
            }
            let raw = read_plain_opt(&t.store_dir(id)?.join(&c.original_path))?
                .ok_or_else(|| invalid("原件不存在"))?;
            if sha256_hex(&raw) != c.sha256 {
                return Err(invalid("原件在导出期间有变").into());
            }
            let body = render_chapter(c, &raw, &texts, options.format, &mut warnings)?;
            rendered.push((c.id.clone(), body));
            records.push(
                json!({"chapter":c.id,"title":title(c),"source_sha256":c.sha256,
                "parsing":c.parsing,"blocks":blocks}),
            );
        }
        if !failures.is_empty() {
            return Err(invalid(format!(
                "不能导出成稿（整批未导出）：\n{}",
                failures.join("\n")
            ))
            .into());
        }
        let ext = if options.format == ExportFormat::Txt {
            "txt"
        } else {
            "md"
        };
        let files = match options.layout {
            ExportLayout::Book => vec![ExportFile {
                name: format!("book.{ext}"),
                bytes: rendered
                    .into_iter()
                    .map(|(_, body)| body)
                    .collect::<Vec<_>>()
                    .join("\n")
                    .into_bytes(),
            }],
            ExportLayout::Chapters => rendered
                .into_iter()
                .map(|(id, body)| ExportFile {
                    name: format!("{id}.{ext}"),
                    bytes: body.into_bytes(),
                })
                .collect(),
        };
        warnings.sort();
        warnings.dedup();
        Ok(bundle(
            files,
            json!({"schema_version":1,"export_rules_version":1,"kind":"finished",
            "project":id,"metadata":p.metadata,"options":options,"chapters":records,
            "project_sha256":sha256_hex(&serde_json::to_vec(&p).map_err(super::TranslationError::from)?),
            "history_sha256":sha256_hex(self.history()?.iter().map(ziyuan_storage::encode_event_line).collect::<String>().as_bytes()),
            "context":context}),
            warnings,
        ))
    }
}

/// Escaping each ASCII punctuation also prevents autolinks, entity interpretation and lists.
fn markdown_text(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(ch);
    }
    // Four spaces/tabs at the start of a line must not turn approved prose into code.
    out.split('\n')
        .map(|line| {
            let n = line.len() - line.trim_start_matches([' ', '\t']).len();
            format!(
                "{}{}",
                "&#32;".repeat(
                    line[..n]
                        .chars()
                        .map(|c| if c == '\t' { 4 } else { 1 })
                        .sum()
                ),
                &line[n..]
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn html_text(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '"' => "&quot;".into(),
            '\'' => "&#39;".into(),
            // CommonMark parses Markdown between inline HTML tags too.
            c if c.is_ascii_punctuation() => format!("&#{};", c as u32),
            c => c.to_string(),
        })
        .collect()
}

fn translated_text(text: &str, inlines: &[Inline], format: ExportFormat) -> Result<String> {
    let plain = |s: &str| {
        if format == ExportFormat::Txt {
            s.to_owned()
        } else {
            markdown_text(s)
        }
    };
    if inlines.is_empty() {
        return Ok(plain(text));
    }
    let base: String = inlines
        .iter()
        .map(|i| match i {
            Inline::Text { text } | Inline::Emphasis { text } => text.as_str(),
            Inline::Ruby { base, .. } => base.as_str(),
        })
        .collect();
    if base != text {
        return Err(invalid(
            "批准译文的结构化注音与正文不一致，不能自动套用源文注音",
        ));
    }
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Text { text } | Inline::Emphasis { text } => out.push_str(&plain(text)),
            Inline::Ruby { base, reading, .. } => {
                if base.is_empty()
                    || reading.is_empty()
                    || base.contains(['\n', '\r', '｜', '《', '》'])
                    || reading.contains(['\n', '\r', '｜', '《', '》'])
                {
                    return Err(invalid("批准译文含无法安全导出的注音"));
                }
                if format == ExportFormat::Txt {
                    out.push_str(&format!("｜{base}《{reading}》"));
                } else {
                    out.push_str(&format!(
                        "<ruby>{}<rt>{}</rt></ruby>",
                        html_text(base),
                        html_text(reading)
                    ));
                }
            }
        }
    }
    Ok(out)
}

fn render_chapter(
    c: &Chapter,
    raw: &[u8],
    texts: &BTreeMap<String, String>,
    format: ExportFormat,
    warnings: &mut Vec<String>,
) -> Result<String> {
    let heading = title(c).replace(['\r', '\n'], " ");
    let mut out = if format == ExportFormat::Txt {
        format!("{heading}\n\n")
    } else {
        format!("# {}\n\n", markdown_text(&heading))
    };
    let is_md = c
        .parsing
        .as_ref()
        .is_some_and(|p| p.format == SourceFormat::Markdown);
    if is_md && format == ExportFormat::Markdown {
        let raw = std::str::from_utf8(raw).map_err(|_| invalid("原件不是 UTF-8"))?;
        let info = c.parsing.as_ref().expect("Markdown parsing");
        // The splitting heading becomes the generated chapter title, not untranslated prose.
        let mut start = info.byte_start;
        if start == 0 && raw.starts_with('\u{feff}') {
            start = '\u{feff}'.len_utf8();
        }
        if info.options.heading_level.is_some() {
            let mut events =
                pulldown_cmark::Parser::new(&raw[start..info.byte_end]).into_offset_iter();
            if matches!(events.next(), Some((pulldown_cmark::Event::Start(pulldown_cmark::Tag::Heading {level,..}),_)) if Some(level as u8) == info.options.heading_level)
            {
                for (event, range) in events {
                    if matches!(
                        event,
                        pulldown_cmark::Event::End(pulldown_cmark::TagEnd::Heading(_))
                    ) {
                        start += range.end;
                        break;
                    }
                }
            }
        }
        for para in &c.paragraphs {
            if para.byte_start < start || para.byte_end > info.byte_end {
                return Err(invalid("章节字节范围重叠"));
            }
            if pulldown_cmark::Parser::new(&raw[para.byte_start..para.byte_end])
                .any(|e| matches!(e, pulldown_cmark::Event::Code(_)))
            {
                return Err(invalid(format!(
                    "{} / {}：原段含行内代码，现有逐段译稿没有位置锚点，不能无损回填；请取回原件或选择 TXT（明确丢弃代码）",
                    c.id, para.id
                )));
            }
            out.push_str(&raw[start..para.byte_start].replace("\r\n", "\n"));
            // Keep heading hierarchy; paragraph ranges include Markdown delimiters.
            if let Some((event, _)) =
                pulldown_cmark::Parser::new(&raw[para.byte_start..para.byte_end])
                    .into_offset_iter()
                    .next()
                && let pulldown_cmark::Event::Start(pulldown_cmark::Tag::Heading { level, .. }) =
                    event
            {
                out.push_str(&"#".repeat(level as usize));
                out.push(' ');
            }
            out.push_str(&texts[&para.id]);
            if raw[para.byte_start..para.byte_end].ends_with('\n') {
                out.push('\n');
            }
            start = para.byte_end;
        }
        out.push_str(&raw[start..info.byte_end].replace("\r\n", "\n"));
        warnings.push("Markdown 以原件字节范围回填定稿段落并保留非翻译节点；原文行内样式、链接及图片目标不套到译文，章名仍用导入时的标题。".into());
    } else {
        for para in &c.paragraphs {
            out.push_str(&texts[&para.id]);
            out.push_str("\n\n");
        }
        if is_md {
            warnings.push("Markdown → TXT 只导出定稿正文与章名；代码、分隔线、链接目标和排版不转换。原记法请取回归档原件。".into());
        }
    }
    while out.ends_with("\n\n\n") {
        out.pop();
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}
