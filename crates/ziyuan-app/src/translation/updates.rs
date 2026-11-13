use super::*;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
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
        let (body, package) = canonical_source(input)?;
        let (old, candidate, mut preview) = self.texts().prepare_update(id, &body)?;
        self.note_package(id, input, &package, &mut preview)?;
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
        let package_sha = package.as_ref().map(|item| sha256_hex(&item.bytes));
        preview.fingerprint = sha256_hex(&serde_json::to_vec(&(
            &preview.fingerprint,
            bindings,
            history
                .iter()
                .map(ziyuan_storage::encode_event_line)
                .collect::<Vec<_>>(),
            package_sha,
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
        // 先把新原件写到临时文件。清单发布成功后再改名盖上；清单失败则删掉临时文件，旧原件不动。
        let mut staged = None;
        EventLog::new(self.root())
            .append_if_unchanged_then(&history, &[], || {
                staged = self.stage_saved_package(
                    id,
                    input.chapter.is_some() && old.chapters.len() == 1,
                    &package,
                )?;
                published =
                    Some(
                        self.texts()
                            .publish_update(&old, candidate, &body, &preview.chapter)?,
                    );
                if let Some(swap) = staged.take() {
                    swap.commit()?;
                }
                Ok::<_, TranslationError>(())
            })
            .map_err(|e| invalid(e.to_string()))??;
        published.ok_or_else(|| invalid("原文发布未完成"))
    }
    fn note_package(
        &self,
        project: &str,
        input: &SourceUpdate,
        package: &Option<ExtractedPackage>,
        preview: &mut SourceUpdatePreview,
    ) -> Result<()> {
        let Some(package) = package else {
            return Ok(());
        };
        let saved = self.saved_package(project)?;
        let single = input.chapter.is_some() && self.project(project)?.chapters.len() == 1;
        match saved {
            Some((extension, _)) if extension != package.extension && single => {
                preview.blockers.push(format!(
                    "已有归档原件是 {}，不能改成 {}",
                    extension.to_uppercase(),
                    package.extension.to_uppercase()
                ));
            }
            Some((extension, _)) if extension == package.extension && single => {
                preview.warnings.push(format!(
                    "会换掉已有的 {} 归档原件，旧文件按内容摘要留在原文历史里。已有译稿不会自动改。",
                    extension.to_uppercase()
                ));
            }
            Some(_) => {
                preview.warnings.push("已有原件保留，这次只改正文。".into());
            }
            None => {
                preview.warnings.push(
                    "项目里没有可替换的同格式归档原件，这次只改正文，不会新建 DOCX 或 EPUB 原件。"
                        .into(),
                );
            }
        }
        Ok(())
    }
    fn saved_package(&self, project: &str) -> Result<Option<(String, Vec<u8>)>> {
        let dir = self
            .root()
            .join(".ziyuan/text-projects")
            .join(project)
            .join("source-docs");
        let docx = super::read_plain_opt(&dir.join("source.docx"))?;
        let epub = super::read_plain_opt(&dir.join("source.epub"))?;
        match (docx, epub) {
            (Some(_), Some(_)) => Err(invalid("归档原件不能同时有 DOCX 和 EPUB")),
            (Some(bytes), None) => Ok(Some(("docx".into(), bytes))),
            (None, Some(bytes)) => Ok(Some(("epub".into(), bytes))),
            (None, None) => Ok(None),
        }
    }
    fn stage_saved_package(
        &self,
        project: &str,
        revision: bool,
        package: &Option<ExtractedPackage>,
    ) -> Result<Option<StagedPackage>> {
        let Some(package) = package else {
            return Ok(None);
        };
        if !revision {
            return Ok(None);
        }
        let Some((extension, previous)) = self.saved_package(project)? else {
            return Ok(None);
        };
        if extension != package.extension {
            return Err(invalid(format!(
                "已有归档原件是 {}，不能改成 {}",
                extension.to_uppercase(),
                package.extension.to_uppercase()
            )));
        }
        let history = self.store_dir(project)?.join("source-history");
        fs::create_dir_all(&history)?;
        let archived = history.join(format!("{}.{extension}", sha256_hex(&previous)));
        if !archived.is_file() {
            super::write_plain(&archived, &previous)?;
        }
        let package_dir = self.store_dir(project)?.join("source-docs");
        let destination = package_dir.join(format!("source.{extension}"));
        let temporary =
            write_new_temp(&package_dir, &format!("source.{extension}"), &package.bytes)?;
        Ok(Some(StagedPackage {
            temporary,
            destination,
            committed: false,
        }))
    }
}

/// 新原件先落在临时文件上。清单发布成功后才改名覆盖；未提交时丢弃临时文件。
struct StagedPackage {
    temporary: PathBuf,
    destination: PathBuf,
    committed: bool,
}

impl StagedPackage {
    fn commit(mut self) -> Result<()> {
        fs::rename(&self.temporary, &self.destination)
            .map_err(|error| invalid(error.to_string()))?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for StagedPackage {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.temporary);
        }
    }
}

fn write_new_temp(dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let path = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| invalid(error.to_string()))?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written {
        let _ = fs::remove_file(&path);
        return Err(error.into());
    }
    Ok(path)
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
    use ziyuan_storage::EventStore;

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
            !plain_root
                .join(".ziyuan/text-projects")
                .join(&plain_id)
                .join("source-docs/source.docx")
                .is_file(),
            "纯文本项目没有归档原件，修订不得新建"
        );
        assert!(
            preview
                .warnings
                .iter()
                .any(|item| item.contains("不会新建"))
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

    fn docx_with(paragraphs: &[&str], extra: Option<(&str, &str)>) -> Vec<u8> {
        let body: String = paragraphs
            .iter()
            .map(|text| format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>"))
            .collect();
        let xml = format!("<w:document><w:body>{body}</w:body></w:document>");
        let mut files = vec![("word/document.xml".to_owned(), xml)];
        if let Some((name, text)) = extra {
            files.push((name.to_owned(), text.to_owned()));
        }
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(name, text)| (name.as_str(), text.as_str()))
            .collect();
        zip_bytes(&refs)
    }

    #[test]
    fn swapping_paragraphs_in_one_block_is_rejected() {
        let (root, id) = workspace();
        let source = root.join("chapter.txt");
        std::fs::write(&source, "朝だ。\n\n雨だ。\n").unwrap();
        TextProjects::new(&root).import(&id, &[source]).unwrap();
        let service = Translations::new(&root);
        let block = service.project(&id).unwrap().chapters[0].blocks[0]
            .block_id
            .clone();
        let zh = service.block_dir(&id, &block).join("zh.json");
        std::fs::create_dir_all(zh.parent().unwrap()).unwrap();
        std::fs::write(&zh, b"kept-bytes").unwrap();
        let swapped = Translations::source_update_from_bytes(
            Some("c0001".into()),
            "chapter.txt".into(),
            "雨だ。\n\n朝だ。\n".as_bytes(),
        )
        .unwrap();
        let preview = service.preview_source_update(&id, &swapped).unwrap();
        assert!(
            preview
                .blockers
                .iter()
                .any(|item| item.contains("段落顺序")),
            "{:?}",
            preview.blockers
        );
        assert!(
            service
                .apply_source_update(&id, &swapped, &preview.fingerprint)
                .is_err()
        );
        assert_eq!(std::fs::read(&zh).unwrap(), b"kept-bytes");
        let project = service.project(&id).unwrap();
        assert!(project.chapters[0].paragraphs[0].text.contains("朝"));
        assert!(project.chapters[0].paragraphs[1].text.contains("雨"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn docx_and_epub_cannot_replace_each_other_and_bytes_change_the_fingerprint() {
        let (root, id) = workspace();
        let service = Translations::new(&root);
        let original = docx_with(&["朝だ。", "雨だ。"], None);
        let path = root.join("book.docx");
        std::fs::write(&path, &original).unwrap();
        service.import_docx(&id, &path).unwrap();
        let saved = root
            .join(".ziyuan/text-projects")
            .join(&id)
            .join("source-docs/source.docx");
        assert_eq!(std::fs::read(&saved).unwrap(), original);

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
                "<html><body><p>海だ。</p><p>雪だ。</p></body></html>",
            ),
        ]);
        let epub_update =
            Translations::source_update_from_bytes(Some("c0001".into()), "book.epub".into(), &epub)
                .unwrap();
        let preview = service.preview_source_update(&id, &epub_update).unwrap();
        assert!(
            preview
                .blockers
                .iter()
                .any(|item| item.contains("不能改成")),
            "{:?}",
            preview.blockers
        );
        assert!(
            service
                .apply_source_update(&id, &epub_update, &preview.fingerprint)
                .is_err()
        );
        assert_eq!(std::fs::read(&saved).unwrap(), original);
        assert!(!saved.with_file_name("source.epub").is_file());

        let same_text = docx_with(
            &["朝だ。", "雨だ。"],
            Some(("extra.txt", "different-bytes")),
        );
        let same = Translations::source_update_from_bytes(
            Some("c0001".into()),
            "book.docx".into(),
            &same_text,
        )
        .unwrap();
        let again = Translations::source_update_from_bytes(
            Some("c0001".into()),
            "book.docx".into(),
            &original,
        )
        .unwrap();
        let left = service.preview_source_update(&id, &same).unwrap();
        let right = service.preview_source_update(&id, &again).unwrap();
        assert_ne!(left.fingerprint, right.fingerprint);
        assert!(left.blockers.is_empty(), "{:?}", left.blockers);

        let revised = docx_with(&["海だ。", "雪だ。"], None);
        let update = Translations::source_update_from_bytes(
            Some("c0001".into()),
            "book.docx".into(),
            &revised,
        )
        .unwrap();
        let preview = service.preview_source_update(&id, &update).unwrap();
        assert!(preview.blockers.is_empty(), "{:?}", preview.blockers);
        service
            .apply_source_update(&id, &update, &preview.fingerprint)
            .unwrap();
        assert_eq!(std::fs::read(&saved).unwrap(), revised);
        let archived = root
            .join(".ziyuan/text-projects")
            .join(&id)
            .join("source-history")
            .join(format!("{}.docx", sha256_hex(&original)));
        assert_eq!(std::fs::read(archived).unwrap(), original);

        let history = root
            .join(".ziyuan/text-projects")
            .join(&id)
            .join("source-history");
        let current = std::fs::read(&saved).unwrap();
        std::fs::create_dir(history.join(format!("{}.docx", sha256_hex(&current)))).unwrap();
        let blocked = docx_with(&["夜だ。", "雪だ。"], None);
        let blocked_update = Translations::source_update_from_bytes(
            Some("c0001".into()),
            "book.docx".into(),
            &blocked,
        )
        .unwrap();
        let preview = service.preview_source_update(&id, &blocked_update).unwrap();
        assert!(
            service
                .apply_source_update(&id, &blocked_update, &preview.fingerprint)
                .is_err()
        );
        assert_eq!(std::fs::read(&saved).unwrap(), current);
        assert!(
            service.project(&id).unwrap().chapters[0].paragraphs[0]
                .text
                .contains("海")
        );
        let temps: Vec<_> = std::fs::read_dir(saved.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp"))
            .collect();
        assert!(temps.is_empty(), "{temps:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn existing_package_on_a_multi_chapter_project_is_kept() {
        let (root, id) = workspace();
        let service = Translations::new(&root);
        let original = docx_with(&["朝だ。", "雨だ。"], None);
        let path = root.join("book.docx");
        std::fs::write(&path, &original).unwrap();
        service.import_docx(&id, &path).unwrap();
        let saved = root
            .join(".ziyuan/text-projects")
            .join(&id)
            .join("source-docs/source.docx");
        let append = Translations::source_update_from_bytes(
            None,
            "next.docx".into(),
            &docx_with(&["次の章。"], None),
        )
        .unwrap();
        let preview = service.preview_source_update(&id, &append).unwrap();
        assert!(
            preview
                .warnings
                .iter()
                .any(|item| item == "已有原件保留，这次只改正文。"),
            "{:?}",
            preview.warnings
        );
        assert!(
            preview
                .warnings
                .iter()
                .all(|item| !item.contains("没有可替换"))
        );
        service
            .apply_source_update(&id, &append, &preview.fingerprint)
            .unwrap();
        assert_eq!(std::fs::read(&saved).unwrap(), original);
        let revise = Translations::source_update_from_bytes(
            Some("c0001".into()),
            "book.docx".into(),
            &docx_with(&["海だ。", "雪だ。"], None),
        )
        .unwrap();
        let preview = service.preview_source_update(&id, &revise).unwrap();
        assert!(
            preview
                .warnings
                .iter()
                .any(|item| item == "已有原件保留，这次只改正文。"),
            "{:?}",
            preview.warnings
        );
        service
            .apply_source_update(&id, &revise, &preview.fingerprint)
            .unwrap();
        assert_eq!(std::fs::read(&saved).unwrap(), original);
        assert!(
            service.project(&id).unwrap().chapters[0].paragraphs[0]
                .text
                .contains("海")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn manifest_failure_after_staging_keeps_the_old_package() {
        let (root, id) = workspace();
        let service = Translations::new(&root);
        let original = docx_with(&["朝だ。", "雨だ。"], None);
        let path = root.join("book.docx");
        std::fs::write(&path, &original).unwrap();
        service.import_docx(&id, &path).unwrap();
        let saved = root
            .join(".ziyuan/text-projects")
            .join(&id)
            .join("source-docs/source.docx");
        let project_file = root
            .join(".ziyuan/text-projects")
            .join(&id)
            .join("project.json");
        let before = std::fs::read(&project_file).unwrap();
        let revise = Translations::source_update_from_bytes(
            Some("c0001".into()),
            "book.docx".into(),
            &docx_with(&["这是一段明显更长的修订，用来撑大清单。", "雪だ。"], None),
        )
        .unwrap();
        let preview = service.preview_source_update(&id, &revise).unwrap();
        assert!(preview.blockers.is_empty(), "{:?}", preview.blockers);
        ziyuan_storage::fail_next_replace_for_test();
        assert!(
            service
                .apply_source_update(&id, &revise, &preview.fingerprint)
                .is_err()
        );
        assert_eq!(std::fs::read(&saved).unwrap(), original);
        assert_eq!(std::fs::read(&project_file).unwrap(), before);
        let temps: Vec<_> = std::fs::read_dir(saved.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp"))
            .collect();
        assert!(temps.is_empty(), "{temps:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn pending_and_assigned_model_tasks_block_changed_blocks() {
        for assigned in [false, true] {
            let (root, id) = workspace();
            crate::Session::found(ziyuan_storage::EventLog::new(&root)).unwrap();
            let source = root.join("chapter.txt");
            std::fs::write(&source, "朝だ。\n\n雨だ。\n").unwrap();
            TextProjects::new(&root).import(&id, &[source]).unwrap();
            let mut session = crate::Session::open(ziyuan_storage::EventLog::new(&root)).unwrap();
            let edict = session
                .issue_translation(&root, &id, "c0001-b0001")
                .unwrap();
            if assigned {
                let worker = session.court().route(&edict).unwrap().worker;
                ziyuan_storage::EventLog::new(&root)
                    .append(
                        session.event_count(),
                        &[ziyuan_core::CourtEvent::TaskAssigned {
                            task: edict.id,
                            worker,
                            model: None,
                        }],
                    )
                    .unwrap();
            }
            let service = Translations::new(&root);
            let update = Translations::source_update_from_bytes(
                Some("c0001".into()),
                "chapter.txt".into(),
                "海だ。\n\n雨だ。\n".as_bytes(),
            )
            .unwrap();
            let preview = service.preview_source_update(&id, &update).unwrap();
            assert!(
                preview
                    .blockers
                    .iter()
                    .any(|item| item.contains("尚未完成")),
                "assigned={assigned} {:?}",
                preview.blockers
            );
            assert!(
                service
                    .apply_source_update(&id, &update, &preview.fingerprint)
                    .is_err()
            );
            assert!(
                service.project(&id).unwrap().chapters[0].paragraphs[0]
                    .text
                    .contains("朝")
            );
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn zip_rejects_too_many_entries_and_oversized_decompression() {
        let mut files = Vec::new();
        for index in 0..4097 {
            files.push((format!("f{index}"), String::new()));
        }
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(name, text)| (name.as_str(), text.as_str()))
            .collect();
        let crowded = zip_bytes(&refs);
        let error = super::packages::read_zip_text(&crowded, "f0").unwrap_err();
        assert!(error.to_string().contains("文件过多"), "{error}");
        let epub_error = super::epub::epub_plain_text(&crowded).unwrap_err();
        assert!(epub_error.to_string().contains("文件过多"), "{epub_error}");

        let mut cursor = Cursor::new(Vec::new());
        let mut archive = zip::ZipWriter::new(&mut cursor);
        archive
            .start_file(
                "word/document.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        let chunk = vec![b'a'; 1024 * 1024];
        for _ in 0..33 {
            archive.write_all(&chunk).unwrap();
        }
        archive.finish().unwrap();
        let error =
            super::packages::read_zip_text(&cursor.into_inner(), "word/document.xml").unwrap_err();
        assert!(error.to_string().contains("超过 32 MiB"), "{error}");
    }
}
