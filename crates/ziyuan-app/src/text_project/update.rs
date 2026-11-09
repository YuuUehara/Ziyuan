//! Immutable TXT source revisions; the manifest is the sole publication point.
use super::store::{random_id, sha256, write_new};
use super::*;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceUpdate {
    pub chapter: Option<String>,
    pub name: String,
    pub text: String,
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
        if input.text.len() > 8 * 1024 * 1024
            || !input.name.to_ascii_lowercase().ends_with(".txt")
            || input.name.len() > 255
            || input
                .name
                .chars()
                .any(|c| c.is_control() || "/\\:".contains(c))
        {
            return Err(invalid("原文更新仅接受8 MiB以内、普通文件名的UTF-8 TXT"));
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
        if previous.is_some_and(|c| {
            c.parsing
                .as_ref()
                .is_some_and(|p| p.format != SourceFormat::Text || p.part != 0)
        }) {
            return Err(invalid("本批只支持单文件单章TXT修订"));
        }
        let chapter = format!("c{:04}", index + 1);
        let prepared = old.chapters.first().is_some_and(|c| c.parsing.is_some());
        let options = previous
            .and_then(|c| c.parsing.as_ref())
            .map(|p| p.options.clone())
            .unwrap_or_default();
        let (paragraphs, parsing, filename) = if prepared {
            let documents = parse_document(
                input.text.as_bytes(),
                SourceFormat::Text,
                &options,
                super::import::fallback_title(&input.name),
            )?;
            if documents.len() != 1 {
                return Err(invalid("更新须为单章TXT"));
            }
            let document = &documents[0];
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
                super::import::paragraphs(&chapter, document),
                Some(ChapterParsing {
                    version: 1,
                    format: SourceFormat::Text,
                    options,
                    source_file: file,
                    part: 0,
                    title: document.title.clone(),
                    byte_start: document.byte_start,
                    byte_end: document.byte_end,
                    warnings: document.warnings.clone(),
                }),
                super::import::original_name(file, &SourceFormat::Text),
            )
        } else {
            (
                split_paragraphs(&chapter, input.text.as_bytes())?,
                None,
                format!("{chapter}.txt"),
            )
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
                warnings: vec![
                    "旧原件和旧译保留；受影响块的送审、勘校与定稿须重新核对；不自动批准".into(),
                ],
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
