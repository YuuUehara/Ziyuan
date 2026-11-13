use super::*;
use std::fs;
use ziyuan_governance::{Court, TaskStatus};
use ziyuan_storage::{EventLog, EventStore, LockScope, Workspace};

impl Translations {
    /// Read a caller-supplied file into a source update. DOCX and EPUB keep their
    /// bytes so the same import parsers can extract paragraphs; nothing is written.
    pub fn source_update_from_bytes(
        chapter: Option<String>,
        name: String,
        bytes: &[u8],
    ) -> Result<SourceUpdate> {
        if name.len() > 255 || name.chars().any(|c| c.is_control() || "/\\:".contains(c)) {
            return Err(invalid("原文文件名须为不含路径的普通文件名"));
        }
        let ext = name
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "txt" | "md" | "markdown" => {
                if bytes.len() > 8 * 1024 * 1024 {
                    return Err(invalid("原文正文超过 8 MiB"));
                }
                let text = String::from_utf8(bytes.to_vec())
                    .map_err(|_| invalid("仅接受 UTF-8 TXT 或 Markdown"))?;
                Ok(SourceUpdate {
                    chapter,
                    name,
                    text,
                    package_base64: None,
                })
            }
            "docx" | "epub" => {
                if bytes.len() > 16 * 1024 * 1024 {
                    return Err(invalid("原件超过 16 MiB"));
                }
                Ok(SourceUpdate {
                    chapter,
                    name,
                    text: String::new(),
                    package_base64: Some(b64_encode(bytes)),
                })
            }
            _ => Err(invalid("原文更新仅接受 TXT、Markdown、DOCX 或 EPUB")),
        }
    }
    pub fn preview_source_update(
        &self,
        id: &str,
        input: &SourceUpdate,
    ) -> Result<SourceUpdatePreview> {
        Ok(self.prepare_source_update(id, input)?.2)
    }
    fn prepare_source_update(
        &self,
        id: &str,
        input: &SourceUpdate,
    ) -> Result<(
        Project,
        Project,
        SourceUpdatePreview,
        Vec<ziyuan_core::CourtEvent>,
    )> {
        let (body, _package) = canonical_source(input)?;
        let (old, candidate, mut preview) = self.texts().prepare_update(id, &body)?;
        let mut bindings = Vec::new();
        let context = self.store_dir(id)?.join("context");
        if context.exists() {
            for entry in fs::read_dir(context)? {
                let path = entry?.path().join("glossary.json");
                if path.is_file() {
                    let bytes = fs::read(path)?;
                    let glossary = Glossary::parse(&bytes)?;
                    if let Err(error) = glossary.validate_project(&candidate) {
                        preview
                            .blockers
                            .push(format!("旧上下文证据无法保留：{error}"));
                    }
                    bindings.push(sha256_hex(&bytes));
                }
            }
        }
        bindings.sort();
        let history = EventLog::new(self.root())
            .load()
            .map_err(|e| invalid(e.to_string()))?;
        if !history.is_empty() {
            let court = Court::replay(&history).map_err(|e| invalid(e.to_string()))?;
            for task in court.tasks() {
                if task
                    .translation
                    .as_ref()
                    .is_some_and(|l| l.project == id && preview.changed_blocks.contains(&l.block))
                    && matches!(
                        task.status,
                        TaskStatus::Pending | TaskStatus::Assigned { .. }
                    )
                {
                    preview
                        .blockers
                        .push(format!("诏令 {} 尚未完成，请先收尾或撤销", task.edict.id.0));
                }
            }
        }
        preview.fingerprint = sha256_hex(&serde_json::to_vec(&(
            &preview.fingerprint,
            bindings,
            history
                .iter()
                .map(ziyuan_storage::encode_event_line)
                .collect::<Vec<_>>(),
        ))?);
        Ok((old, candidate, preview, history))
    }
    pub fn apply_source_update(
        &self,
        id: &str,
        input: &SourceUpdate,
        expected: &str,
    ) -> Result<Project> {
        let workspace = Workspace::open(self.root()).map_err(|e| invalid(e.to_string()))?;
        let _guard = workspace
            .lock(LockScope::Workspace)
            .map_err(|e| invalid(e.to_string()))?;
        workspace
            .ensure_writable()
            .map_err(|e| invalid(e.to_string()))?;
        let (body, package) = canonical_source(input)?;
        let (old, candidate, preview, history) = self.prepare_source_update(id, input)?;
        if preview.fingerprint != expected {
            return Err(invalid("预览后原文或引用材料有变，请重新预览"));
        }
        if !preview.blockers.is_empty() {
            return Err(invalid(preview.blockers.join("；")));
        }
        let mut published = None;
        EventLog::new(self.root())
            .append_if_unchanged_then(&history, &[], || {
                published =
                    Some(
                        self.texts()
                            .publish_update(&old, candidate, &body, &preview.chapter)?,
                    );
                Ok::<_, TranslationError>(())
            })
            .map_err(|e| invalid(e.to_string()))??;
        let project = published.ok_or_else(|| invalid("原文发布未完成"))?;
        if let Some(ExtractedPackage { extension, bytes }) = package
            && input.chapter.is_some()
            && project.chapters.len() == 1
        {
            self.archive_previous_package(id, &extension)?;
            self.store_source_package(id, &extension, &bytes)?;
        }
        Ok(project)
    }
    fn archive_previous_package(&self, project: &str, extension: &str) -> Result<()> {
        let Ok((_, bytes)) = self.source_package(project, extension) else {
            return Ok(());
        };
        let history = self.store_dir(project)?.join("source-history");
        fs::create_dir_all(&history)?;
        let path = history.join(format!("{}.{}", sha256_hex(&bytes), extension));
        if path.is_file() {
            return Ok(());
        }
        super::write_plain(&path, &bytes)?;
        Ok(())
    }
}

struct ExtractedPackage {
    extension: String,
    bytes: Vec<u8>,
}

fn canonical_source(input: &SourceUpdate) -> Result<(SourceUpdate, Option<ExtractedPackage>)> {
    let ext = input
        .name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "txt" | "md" | "markdown" => {
            if input.package_base64.is_some() {
                return Err(invalid("TXT／Markdown 不接受原件字节"));
            }
            Ok((
                SourceUpdate {
                    package_base64: None,
                    ..input.clone()
                },
                None,
            ))
        }
        "docx" | "epub" => {
            if !input.text.is_empty() {
                return Err(invalid("DOCX／EPUB 只接受原件字节，不接受已解码文本"));
            }
            let encoded = input
                .package_base64
                .as_deref()
                .ok_or_else(|| invalid("请提供 DOCX 或 EPUB 原件"))?;
            let bytes = b64_decode(encoded)?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err(invalid("原件超过 16 MiB"));
            }
            let text = if ext == "docx" {
                super::docx::docx_text(&bytes)?
            } else {
                super::epub::epub_plain_text(&bytes)?.0
            };
            if text.len() > 8 * 1024 * 1024 {
                return Err(invalid("抽出的正文超过 8 MiB"));
            }
            Ok((
                SourceUpdate {
                    chapter: input.chapter.clone(),
                    name: input.name.clone(),
                    text,
                    package_base64: None,
                },
                Some(ExtractedPackage {
                    extension: ext,
                    bytes,
                }),
            ))
        }
        _ => Err(invalid("原文更新仅接受 TXT、Markdown、DOCX 或 EPUB")),
    }
}

fn b64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        let value = ((bytes[index] as u32) << 16)
            | ((bytes[index + 1] as u32) << 8)
            | bytes[index + 2] as u32;
        for shift in [18, 12, 6, 0] {
            out.push(TABLE[((value >> shift) & 63) as usize] as char);
        }
        index += 3;
    }
    match bytes.len() - index {
        1 => {
            let value = (bytes[index] as u32) << 16;
            out.push(TABLE[((value >> 18) & 63) as usize] as char);
            out.push(TABLE[((value >> 12) & 63) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let value = ((bytes[index] as u32) << 16) | ((bytes[index + 1] as u32) << 8);
            out.push(TABLE[((value >> 18) & 63) as usize] as char);
            out.push(TABLE[((value >> 12) & 63) as usize] as char);
            out.push(TABLE[((value >> 6) & 63) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

fn b64_decode(text: &str) -> Result<Vec<u8>> {
    fn value(byte: u8) -> Result<u8> {
        Ok(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(invalid("原件编码无效")),
        })
    }
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(invalid("原件编码无效"));
    }
    let mut out = Vec::new();
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().rev().take_while(|b| **b == b'=').count();
        if pad > 2 {
            return Err(invalid("原件编码无效"));
        }
        let mut accum = 0u32;
        for (index, byte) in chunk.iter().enumerate() {
            let piece = if *byte == b'=' {
                if index < 4 - pad {
                    return Err(invalid("原件编码无效"));
                }
                0
            } else if index >= 4 - pad {
                return Err(invalid("原件编码无效"));
            } else {
                value(*byte)? as u32
            };
            accum = (accum << 6) | piece;
        }
        out.push((accum >> 16) as u8);
        if pad < 2 {
            out.push((accum >> 8) as u8);
        }
        if pad < 1 {
            out.push(accum as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_project::{NewProject, Provenance, TextProjects};
    use std::io::{Cursor, Write};

    fn workspace() -> (std::path::PathBuf, String) {
        let root = std::env::temp_dir().join(format!(
            "ziyuan-source-fmt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let project = TextProjects::new(&root)
            .create(NewProject {
                title: "格式".into(),
                source_language: "ja".into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap();
        (root, project.id)
    }

    fn zip_bytes(files: &[(&str, &str)]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        let mut archive = zip::ZipWriter::new(&mut cursor);
        let options = zip::write::SimpleFileOptions::default();
        for (name, body) in files {
            archive.start_file(*name, options).unwrap();
            archive.write_all(body.as_bytes()).unwrap();
        }
        archive.finish().unwrap();
        cursor.into_inner()
    }

    #[test]
    fn base64_round_trip_keeps_binary_bytes() {
        let bytes = [0u8, 1, 2, 250, 255];
        assert_eq!(b64_decode(&b64_encode(&bytes)).unwrap(), bytes);
    }

    #[test]
    fn markdown_docx_and_epub_updates_keep_paragraph_identity() {
        let (root, id) = workspace();
        let texts = TextProjects::new(&root);
        let input = [crate::text_project::ImportSource::Text {
            name: "chapter.md".into(),
            text: "朝、窓を開けた。\n\n雨はもう上がっていた。\n".into(),
        }];
        let options = crate::text_project::ImportOptions::default();
        let preview = texts.preview_import(&id, &input, &options).unwrap();
        texts
            .import_prepared(&id, &input, &options, &preview.fingerprint)
            .unwrap();
        let service = Translations::new(&root);
        let block = service.project(&id).unwrap().chapters[0].blocks[0]
            .block_id
            .clone();
        std::fs::create_dir_all(service.block_dir(&id, &block)).unwrap();
        std::fs::write(service.block_dir(&id, &block).join("zh.json"), b"kept").unwrap();

        let revised = Translations::source_update_from_bytes(
            Some("c0001".into()),
            "chapter.md".into(),
            "朝、海を見た。\n\n雨はもう上がっていた。\n".as_bytes(),
        )
        .unwrap();
        let preview = service.preview_source_update(&id, &revised).unwrap();
        assert!(preview.blockers.is_empty(), "{:?}", preview.blockers);
        service
            .apply_source_update(&id, &revised, &preview.fingerprint)
            .unwrap();
        assert_eq!(
            std::fs::read(service.block_dir(&id, &block).join("zh.json")).unwrap(),
            b"kept"
        );
        assert!(
            service.project(&id).unwrap().chapters[0].paragraphs[0]
                .text
                .contains("海")
        );

        let split = Translations::source_update_from_bytes(
            Some("c0001".into()),
            "chapter.md".into(),
            "一段だけ。\n".as_bytes(),
        )
        .unwrap();
        let preview = service.preview_source_update(&id, &split).unwrap();
        assert!(
            preview
                .blockers
                .iter()
                .any(|item| item.contains("段数或分块归属"))
        );
        assert!(
            service
                .apply_source_update(&id, &split, &preview.fingerprint)
                .is_err()
        );

        let rejected = Translations::source_update_from_bytes(
            None,
            "extra.docx".into(),
            &zip_bytes(&[(
                "word/document.xml",
                r#"<w:document><w:body><w:p><w:r><w:t>追加の段。</w:t></w:r></w:p></w:body></w:document>"#,
            )]),
        )
        .unwrap();
        let error = service.preview_source_update(&id, &rejected).unwrap_err();
        assert!(error.to_string().contains("不能用 DOCX"), "{error}");

        let (plain_root, plain_id) = workspace();
        let source = plain_root.join("plain.txt");
        std::fs::write(&source, "朝だ。\n\n雨だ。\n").unwrap();
        TextProjects::new(&plain_root)
            .import(&plain_id, &[source])
            .unwrap();
        let plain = Translations::new(&plain_root);
        let docx = zip_bytes(&[(
            "word/document.xml",
            r#"<w:document><w:body><w:p><w:r><w:t>海だ。</w:t></w:r></w:p><w:p><w:r><w:t>雨だ。</w:t></w:r></w:p></w:body></w:document>"#,
        )]);
        let revise_docx =
            Translations::source_update_from_bytes(Some("c0001".into()), "book.docx".into(), &docx)
                .unwrap();
        let preview = plain
            .preview_source_update(&plain_id, &revise_docx)
            .unwrap();
        assert!(preview.blockers.is_empty(), "{:?}", preview.blockers);
        plain
            .apply_source_update(&plain_id, &revise_docx, &preview.fingerprint)
            .unwrap();
        assert!(
            plain.project(&plain_id).unwrap().chapters[0].paragraphs[0]
                .text
                .contains("海")
        );
        assert!(
            plain_root
                .join(".ziyuan/text-projects")
                .join(&plain_id)
                .join("source-docs/source.docx")
                .is_file()
        );

        let epub = zip_bytes(&[
            (
                "META-INF/container.xml",
                r#"<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#,
            ),
            (
                "content.opf",
                r#"<package><manifest><item id="c" href="chap.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c"/></spine></package>"#,
            ),
            (
                "chap.xhtml",
                r#"<html><body><p>EPUBの段。</p></body></html>"#,
            ),
        ]);
        let epub_update =
            Translations::source_update_from_bytes(None, "extra.epub".into(), &epub).unwrap();
        let preview = plain
            .preview_source_update(&plain_id, &epub_update)
            .unwrap();
        assert!(preview.blockers.is_empty(), "{:?}", preview.blockers);
        assert!(
            preview
                .warnings
                .iter()
                .any(|item| item.contains("归档原件"))
        );
        plain
            .apply_source_update(&plain_id, &epub_update, &preview.fingerprint)
            .unwrap();
        assert!(
            plain.project(&plain_id).unwrap().chapters[1].paragraphs[0]
                .text
                .contains("EPUB")
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(plain_root);
    }
}
