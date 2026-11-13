//! Immutable source revisions; the manifest is the sole publication point.
//! TXT, Markdown, and text extracted from DOCX/EPUB share one paragraph-identity gate.
use super::store::{random_id, sha256, write_new};
use super::*;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceUpdate {
    pub chapter: Option<String>,
    pub name: String,
    /// Canonical UTF-8 body. For DOCX/EPUB this is filled only after extraction.
    pub text: String,
    /// Original DOCX/EPUB bytes, standard Base64. Omitted for TXT and Markdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_base64: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct SourceChange {
    pub paragraph: String,
    pub before: Option<String>,
    pub after: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct SourceUpdatePreview {
    pub project: String,
    pub chapter: String,
    pub fingerprint: String,
    pub changes: Vec<SourceChange>,
    pub changed_blocks: Vec<String>,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}
impl TextProjects {
    pub(crate) fn prepare_update(
        &self,
        id: &str,
        input: &SourceUpdate,
    ) -> Result<(Project, Project, SourceUpdatePreview)> {
        let kind = source_kind(&input.name)?;
        if input.package_base64.is_some() {
            return Err(invalid("原文须先抽出正文再核对段落"));
        }
        if input.name.len() > 255
            || input
                .name
                .chars()
                .any(|c| c.is_control() || "/\\:".contains(c))
        {
            return Err(invalid("原文文件名须为不含路径的普通文件名"));
        }
        if input.text.len() > 8 * 1024 * 1024 {
            return Err(invalid("原文正文超过 8 MiB"));
        }
        let old = self.show(id)?;
        let index = match &input.chapter {
            Some(chapter) => old
                .chapters
                .iter()
                .position(|c| c.id == *chapter)
                .ok_or_else(|| invalid("章节不存在"))?,
            None => old.chapters.len(),
        };
        let previous = old.chapters.get(index);
        let chapter = format!("c{:04}", index + 1);
        let (paragraphs, parsing, filename) = match kind {
            SourceKind::Text => text_paragraphs(&old, previous, &chapter, input)?,
            SourceKind::Markdown => markdown_paragraphs(&old, previous, &chapter, input)?,
            SourceKind::Docx | SourceKind::Epub => {
                if old.chapters.iter().any(|c| c.parsing.is_some()) {
                    return Err(invalid(
                        "此项目按 TXT 或 Markdown 解析保存，不能用 DOCX／EPUB 追加或修订；请改用同格式文本，或另建项目",
                    ));
                }
                (
                    split_paragraphs(&chapter, input.text.as_bytes())?,
                    None,
                    format!("{chapter}.txt"),
                )
            }
        };
        let blocks = chunk_paragraphs(&chapter, &paragraphs, old.block_bytes)?;
        let mut blockers = Vec::new();
        if let Some(previous) = previous {
            let identities = |c: &[PromptBlock]| {
                c.iter()
                    .map(|b| {
                        (
                            b.block_id.clone(),
                            b.paragraphs
                                .iter()
                                .map(|p| p.id.clone())
                                .collect::<Vec<_>>(),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            if previous.paragraphs.len() != paragraphs.len()
                || identities(&previous.blocks) != identities(&blocks)
            {
                blockers.push(
                    "段数或分块归属发生变化，不能安全继承旧译；请拆分此次修订或另建项目".into(),
                );
            }
        }
        let changes = paragraphs
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let before = previous
                    .and_then(|c| c.paragraphs.get(i))
                    .map(|p| p.text.clone());
                (before.as_ref() != Some(&p.text)).then(|| SourceChange {
                    paragraph: p.id.clone(),
                    before,
                    after: Some(p.text.clone()),
                })
            })
            .chain(
                previous
                    .into_iter()
                    .flat_map(|c| c.paragraphs.iter().skip(paragraphs.len()))
                    .map(|p| SourceChange {
                        paragraph: p.id.clone(),
                        before: Some(p.text.clone()),
                        after: None,
                    }),
            )
            .collect();
        let changed_blocks = blocks
            .iter()
            .filter(|b| !previous.is_some_and(|c| c.blocks.contains(b)))
            .map(|b| b.block_id.clone())
            .collect();
        let mut candidate = old.clone();
        let updated = Chapter {
            id: chapter.clone(),
            source_name: input.name.clone(),
            original_path: format!(".import-00000000000000000000000000000000/originals/{filename}"),
            sha256: sha256(input.text.as_bytes()),
            byte_len: input.text.len(),
            paragraphs,
            blocks,
            parsing,
        };
        if previous.is_some() {
            candidate.chapters[index] = updated;
        } else {
            candidate.chapters.push(updated);
        }
        let warnings = update_warnings(kind, previous.is_some(), old.chapters.len());
        let fingerprint = sha256(&serde_json::to_vec(&(&old, input))?);
        Ok((
            old,
            candidate,
            SourceUpdatePreview {
                project: id.into(),
                chapter,
                fingerprint,
                changes,
                changed_blocks,
                blockers,
                warnings,
            },
        ))
    }
    pub(crate) fn publish_update(
        &self,
        old: &Project,
        mut candidate: Project,
        input: &SourceUpdate,
        chapter: &str,
    ) -> Result<Project> {
        let protocol = self.project_protocol(&old.id)?;
        let directory = self.project_dir(&old.id)?;
        self.manifest(&old.id, protocol)?
            .update::<_, TextProjectError>(|stored| {
                if stored.ok_or_else(|| invalid("项目不存在"))?.value != *old {
                    return Err(invalid("项目已变化，请重新预览"));
                }
                let bundle = format!(".import-{}", random_id()?);
                let stage = directory.join(&bundle);
                fs::create_dir(&stage)?;
                fs::create_dir(stage.join("originals"))?;
                fs::create_dir(stage.join("blocks"))?;
                let target = candidate
                    .chapters
                    .iter_mut()
                    .find(|c| c.id == chapter)
                    .ok_or_else(|| invalid("章节不存在"))?;
                let filename = target
                    .original_path
                    .rsplit('/')
                    .next()
                    .ok_or_else(|| invalid("原文路径不合法"))?
                    .to_owned();
                target.original_path = format!("{bundle}/originals/{filename}");
                write_new(
                    &directory.join(&target.original_path),
                    input.text.as_bytes(),
                )?;
                for block in &target.blocks {
                    write_new(
                        &stage
                            .join("blocks")
                            .join(format!("{}.json", block.block_id)),
                        &block.serialized()?,
                    )?;
                }
                let history = directory.join("source-history");
                fs::create_dir_all(&history)?;
                let metadata = fs::symlink_metadata(&history)?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(invalid("原文历史须为普通目录"));
                }
                let bytes = serde_json::to_vec_pretty(old)?;
                let path = history.join(format!("{}.json", sha256(&bytes)));
                match fs::symlink_metadata(&path) {
                    Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
                        if fs::read(&path)? != bytes {
                            return Err(invalid("原文历史与摘要不符"));
                        }
                    }
                    Ok(_) => return Err(invalid("原文历史须为普通文件")),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        write_new(&path, &bytes)?
                    }
                    Err(error) => return Err(error.into()),
                }
                if serde_json::to_vec(&candidate)?.len() as u64 + 1024 > self.manifest_bytes {
                    return Err(invalid("更新后项目清单过大"));
                }
                Ok((candidate.clone(), ()))
            })?;
        self.show(&old.id)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    Text,
    Markdown,
    Docx,
    Epub,
}

fn source_kind(name: &str) -> Result<SourceKind> {
    match name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("txt") => Ok(SourceKind::Text),
        Some("md" | "markdown") => Ok(SourceKind::Markdown),
        Some("docx") => Ok(SourceKind::Docx),
        Some("epub") => Ok(SourceKind::Epub),
        _ => Err(invalid("原文更新仅接受 TXT、Markdown、DOCX 或 EPUB")),
    }
}

fn update_warnings(kind: SourceKind, revision: bool, chapters: usize) -> Vec<String> {
    let mut warnings =
        vec!["旧原件和旧译保留；受影响块的送审、勘校与定稿须重新核对；不自动批准".into()];
    if matches!(kind, SourceKind::Docx | SourceKind::Epub) {
        if revision && chapters == 1 {
            warnings
                .push("单章修订会替换同格式归档原件，供日后回填；不会自动改写已有译稿。".into());
        } else {
            warnings.push(
                "归档原件不随本次更新替换；DOCX／EPUB 回填仍使用导入时的原件。本次只更新项目正文。"
                    .into(),
            );
        }
    }
    if kind == SourceKind::Markdown && !revision {
        warnings.push("追加的 Markdown 整文件作为一章，不按标题再拆章。".into());
    }
    warnings
}

fn text_paragraphs(
    old: &Project,
    previous: Option<&Chapter>,
    chapter: &str,
    input: &SourceUpdate,
) -> Result<(Vec<Paragraph>, Option<ChapterParsing>, String)> {
    let prepared = old.chapters.first().is_some_and(|c| c.parsing.is_some());
    if previous.is_some_and(|c| {
        c.parsing
            .as_ref()
            .is_some_and(|p| p.format != SourceFormat::Text || p.part != 0)
    }) {
        return Err(invalid(
            "该章不是单文件纯文本，不能用 TXT 修订；请使用与该章相同的格式",
        ));
    }
    let options = previous
        .and_then(|c| c.parsing.as_ref())
        .map(|p| p.options.clone())
        .unwrap_or_default();
    if prepared {
        let documents = parse_document(
            input.text.as_bytes(),
            SourceFormat::Text,
            &options,
            super::import::fallback_title(&input.name),
        )?;
        if documents.len() != 1 {
            return Err(invalid("更新须为单章 TXT"));
        }
        Ok(parsed_chapter(
            old,
            previous,
            chapter,
            &documents[0],
            SourceFormat::Text,
            options,
        ))
    } else {
        Ok((
            split_paragraphs(chapter, input.text.as_bytes())?,
            None,
            format!("{chapter}.txt"),
        ))
    }
}

fn markdown_paragraphs(
    old: &Project,
    previous: Option<&Chapter>,
    chapter: &str,
    input: &SourceUpdate,
) -> Result<(Vec<Paragraph>, Option<ChapterParsing>, String)> {
    let options = if let Some(previous) = previous {
        match previous.parsing.as_ref() {
            Some(parsing) if parsing.format == SourceFormat::Markdown && parsing.part == 0 => {
                parsing.options.clone()
            }
            Some(parsing) if parsing.part != 0 => {
                return Err(invalid(
                    "该章来自多章文件的一部分，不能单独修订；请另建项目",
                ));
            }
            Some(_) => {
                return Err(invalid("修订须使用与该章相同的格式；此章不是 Markdown"));
            }
            None => {
                return Err(invalid("该章按纯文本分段，不能改用 Markdown 修订"));
            }
        }
    } else {
        ParseOptions::default()
    };
    let documents = parse_document(
        input.text.as_bytes(),
        SourceFormat::Markdown,
        &options,
        super::import::fallback_title(&input.name),
    )?;
    if documents.len() != 1 {
        return Err(invalid(
            "更新须为单章 Markdown；请去掉分章标题，或拆成单章文件后再追加或修订",
        ));
    }
    Ok(parsed_chapter(
        old,
        previous,
        chapter,
        &documents[0],
        SourceFormat::Markdown,
        options,
    ))
}

fn parsed_chapter(
    old: &Project,
    previous: Option<&Chapter>,
    chapter: &str,
    document: &ParsedChapter,
    format: SourceFormat,
    options: ParseOptions,
) -> (Vec<Paragraph>, Option<ChapterParsing>, String) {
    let file = previous
        .and_then(|c| c.parsing.as_ref())
        .map(|p| p.source_file)
        .unwrap_or_else(|| {
            old.chapters
                .last()
                .and_then(|c| c.parsing.as_ref())
                .map_or(1, |p| p.source_file + 1)
        });
    (
        super::import::paragraphs(chapter, document),
        Some(ChapterParsing {
            version: 1,
            format: format.clone(),
            options,
            source_file: file,
            part: 0,
            title: document.title.clone(),
            byte_start: document.byte_start,
            byte_end: document.byte_end,
            warnings: document.warnings.clone(),
        }),
        super::import::original_name(file, &format),
    )
}
