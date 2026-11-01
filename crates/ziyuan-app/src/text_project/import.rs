//! Explicit, read-only previews and compare-before-publish chapter imports.
use super::store::{
    Source, estimated_manifest_bytes, random_id, sha256, validate_project, write_new,
};
use super::{
    Chapter, ChapterParsing, Inline, Paragraph, ParseOptions, ParsedChapter, Project, Result,
    SourceFormat, TextProjectError, TextProjects, chunk_paragraphs, invalid, parse_document,
};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

const SOURCE_BYTES: usize = 8 * 1024 * 1024;

/// Web callers construct Text explicitly; they must never deserialize client filesystem paths.
pub enum ImportSource {
    File(PathBuf),
    Text { name: String, text: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImportOptions {
    pub parsing: ParseOptions,
    /// Empty means use source titles; otherwise provide every chapter title in import order.
    pub chapter_titles: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ImportPreview {
    pub project: String,
    pub fingerprint: String,
    pub options: ImportOptions,
    /// Original paths are relative to the not-yet-created import bundle.
    pub chapters: Vec<Chapter>,
}

enum Contents {
    File(Box<Source>),
    Text(Vec<u8>),
}
struct Snapshot {
    name: String,
    format: SourceFormat,
    contents: Contents,
}
impl Snapshot {
    fn bytes(&self) -> &[u8] {
        match &self.contents {
            Contents::File(s) => &s.bytes,
            Contents::Text(bytes) => bytes,
        }
    }
    fn verify(&mut self) -> Result<()> {
        match &mut self.contents {
            Contents::File(s) => s.verify(),
            Contents::Text(_) => Ok(()),
        }
    }
}

impl TextProjects {
    /// Does not stage files, modify the project or invoke a model.
    pub fn preview_import(
        &self,
        id: &str,
        sources: &[ImportSource],
        options: &ImportOptions,
    ) -> Result<ImportPreview> {
        let project = self.show(id)?;
        require_empty(&project)?;
        let mut snapshots = snapshots(sources)?;
        let preview = preview(&project, &snapshots, options)?;
        self.check_import_size(&project, &preview.chapters)?;
        for snapshot in &mut snapshots {
            snapshot.verify()?;
        }
        Ok(preview)
    }

    /// Re-read sources and require the exact confirmed content, order, options and metadata.
    /// The manifest lock and atomic replacement are the sole publication point.
    pub fn import_prepared(
        &self,
        id: &str,
        sources: &[ImportSource],
        options: &ImportOptions,
        expected: &str,
    ) -> Result<Project> {
        let mut snapshots = snapshots(sources)?;
        let dir = self.project_dir(id)?;
        let (stored, ()) = self
            .manifest(id)?
            .update::<_, TextProjectError>(|current| {
                let mut project = current.ok_or_else(|| invalid("项目不存在"))?.value;
                validate_project(&project, id)?;
                require_empty(&project)?;
                let preview = preview(&project, &snapshots, options)?;
                if preview.fingerprint != expected {
                    return Err(invalid(
                        "预览指纹不符：原文、顺序、标题或解析选项已变化，请重新预览并确认",
                    ));
                }
                let bundle = format!(".import-{}", random_id()?);
                let mut chapters = preview.chapters;
                for chapter in &mut chapters {
                    chapter.original_path = format!("{bundle}/{}", chapter.original_path);
                }
                self.check_import_size(&project, &chapters)?;
                let stage = dir.join(&bundle);
                fs::create_dir(&stage)?;
                fs::create_dir(stage.join("originals"))?;
                fs::create_dir(stage.join("blocks"))?;
                // Split chapters share a single untouched original, rather than duplicating a book
                // once for every heading. Source ranges always refer to that complete original.
                for (index, snapshot) in snapshots.iter().enumerate() {
                    let name = original_name(index + 1, &snapshot.format);
                    write_new(&stage.join("originals").join(name), snapshot.bytes())?;
                }
                for chapter in &chapters {
                    for block in &chapter.blocks {
                        write_new(
                            &stage
                                .join("blocks")
                                .join(format!("{}.json", block.block_id)),
                            &block.serialized()?,
                        )?;
                    }
                }
                for snapshot in &mut snapshots {
                    snapshot.verify()?;
                }
                project.chapters = chapters;
                Ok((project, ()))
            })?;
        Ok(stored.value)
    }

    fn check_import_size(&self, empty: &Project, chapters: &[Chapter]) -> Result<()> {
        let mut candidate = empty.clone();
        candidate.chapters = chapters.to_vec();
        if estimated_manifest_bytes(&candidate)? > self.manifest_bytes {
            return Err(invalid(
                "这些原文与注音的项目记录过大，请拆成多个项目；本次未发布导入",
            ));
        }
        Ok(())
    }
}

fn require_empty(project: &Project) -> Result<()> {
    if !project.chapters.is_empty() {
        return Err(invalid("已有原文，拒绝覆盖或追加；请新建项目导入"));
    }
    Ok(())
}

fn snapshots(sources: &[ImportSource]) -> Result<Vec<Snapshot>> {
    if sources.is_empty() {
        return Err(invalid("至少指定一个 TXT 或 Markdown 文件"));
    }
    let mut total = 0usize;
    let mut out = Vec::new();
    for input in sources {
        let name = match input {
            ImportSource::File(path) => path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| invalid("原文文件名必须是 UTF-8"))?
                .to_owned(),
            ImportSource::Text { name, .. } => name.clone(),
        };
        if name.is_empty()
            || name.len() > 255
            || name
                .chars()
                .any(|c| c.is_control() || c == '/' || c == '\\')
        {
            return Err(invalid(
                "原文文件名必须是无路径、无控制字符的普通文件名（最多 255 字节）",
            ));
        }
        let format = format_for_name(&name)?;
        let remaining = SOURCE_BYTES.saturating_sub(total);
        let contents = match input {
            ImportSource::File(path) => {
                Contents::File(Box::new(Source::read_limited(path, remaining)?))
            }
            ImportSource::Text { text, .. } => {
                if text.len() > remaining {
                    return Err(invalid("导入原文合计超过 8 MiB，请拆成多个项目"));
                }
                Contents::Text(text.as_bytes().to_vec())
            }
        };
        let snapshot = Snapshot {
            name,
            format,
            contents,
        };
        total += snapshot.bytes().len();
        out.push(snapshot);
    }
    Ok(out)
}

pub(super) fn format_for_name(name: &str) -> Result<SourceFormat> {
    match name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("txt") => Ok(SourceFormat::Text),
        Some("md" | "markdown") => Ok(SourceFormat::Markdown),
        _ => Err(invalid("仅支持 UTF-8 .txt、.md 或 .markdown 原文")),
    }
}

pub(super) fn original_name(file: usize, format: &SourceFormat) -> String {
    let extension = match format {
        SourceFormat::Text => "txt",
        SourceFormat::Markdown => "md",
    };
    format!("f{file:04}.{extension}")
}

pub(super) fn fallback_title(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

fn preview(
    project: &Project,
    snapshots: &[Snapshot],
    options: &ImportOptions,
) -> Result<ImportPreview> {
    let mut chapters = Vec::new();
    for (index, snapshot) in snapshots.iter().enumerate() {
        let parsed = parse_document(
            snapshot.bytes(),
            snapshot.format.clone(),
            &options.parsing,
            fallback_title(&snapshot.name),
        )?;
        let digest = sha256(snapshot.bytes());
        for (part, document) in parsed.into_iter().enumerate() {
            let id = format!("c{:04}", chapters.len() + 1);
            let paragraphs = paragraphs(&id, &document);
            let blocks = chunk_paragraphs(&id, &paragraphs, project.block_bytes)?;
            chapters.push(Chapter {
                id,
                source_name: snapshot.name.clone(),
                original_path: format!("originals/{}", original_name(index + 1, &snapshot.format)),
                sha256: digest.clone(),
                byte_len: snapshot.bytes().len(),
                paragraphs,
                blocks,
                parsing: Some(ChapterParsing {
                    version: 1,
                    format: snapshot.format.clone(),
                    options: options.parsing.clone(),
                    source_file: index + 1,
                    part,
                    title: document.title,
                    byte_start: document.byte_start,
                    byte_end: document.byte_end,
                    warnings: document.warnings,
                }),
            });
        }
    }
    if !options.chapter_titles.is_empty() {
        if options.chapter_titles.len() != chapters.len()
            || options.chapter_titles.iter().any(|t| t.trim().is_empty())
        {
            return Err(invalid("章节标题须按顺序完整提供，每章一个非空标题"));
        }
        for (chapter, title) in chapters.iter_mut().zip(&options.chapter_titles) {
            chapter
                .parsing
                .as_mut()
                .expect("newly parsed chapter")
                .title = title.clone();
        }
    }
    // Bind metadata too: changing the project language or provenance after preview is not
    // confirmation of a different import. The fingerprint is not an authorization token.
    let fingerprint = sha256(&serde_json::to_vec(&(project, options, &chapters))?);
    Ok(ImportPreview {
        project: project.id.clone(),
        fingerprint,
        options: options.clone(),
        chapters,
    })
}

pub(super) fn paragraphs(id: &str, document: &ParsedChapter) -> Vec<Paragraph> {
    document
        .paragraphs
        .iter()
        .enumerate()
        .map(|(index, paragraph)| {
            // Plain text needs no duplicate inline copy. Empty annotations preserve legacy block
            // serialization; actual ruby/emphasis and its encoding count toward the block limit.
            let annotated = paragraph
                .inlines
                .iter()
                .any(|inline| !matches!(inline, Inline::Text { .. }));
            Paragraph {
                id: format!("{id}-p{:04}", index + 1),
                byte_start: paragraph.byte_start,
                byte_end: paragraph.byte_end,
                text: paragraph.text.clone(),
                inlines: if annotated {
                    paragraph.inlines.clone()
                } else {
                    Vec::new()
                },
            }
        })
        .collect()
}
