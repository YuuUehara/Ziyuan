//! DOCX／EPUB 回填的共用工具：保留原件、按比例分配译文到原有文字节点、重写 ZIP。
//!
//! 回填只改文字节点，不重建段落、不删表格与图片；粗体、斜体等由原有文字节点承载，
//! 因此按节点长度比例分配译文后，原有标记范围仍在。分配是保守的近似，不是逐字对齐，
//! 也不承诺版式、字体或分页与原件完全一致。
use super::{Result, Translations, invalid};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

/// 取普通文件；不存在为 `None`，符号链接或非普通文件报错。
fn read_plain(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => Err(invalid(format!(
            "{} 不是普通文件（拒绝符号链接）",
            path.display()
        ))),
        Ok(_) => Ok(Some(std::fs::read(path)?)),
    }
}

/// 原子写入一个新文件；目标已存在、父目录不存在或目标是符号链接时拒绝。
pub(crate) fn write_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("输出路径没有上级目录"))?;
    if !parent.is_dir() {
        return Err(invalid(format!("输出目录不存在：{}", parent.display())));
    }
    let name = path
        .file_name()
        .ok_or_else(|| invalid("输出路径没有文件名"))?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&tmp)
        .map_err(|e| invalid(format!("无法创建输出临时文件：{e}")))?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err(invalid("写入输出文件失败"));
    }
    // 不覆盖已有文件：rename 在 Windows 会覆盖，先确认目标不存在。
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(invalid(format!(
                "输出文件已存在，拒绝覆盖：{}",
                path.display()
            )));
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        invalid(format!("发布输出文件失败：{e}"))
    })
}

/// 按原文字节点长度分配译文；最后一段收下全部余数，保证一字不丢。
pub(crate) fn distribute(text: &str, lengths: &[usize]) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let total: usize = lengths.iter().sum();
    let mut out = Vec::with_capacity(lengths.len());
    if lengths.is_empty() {
        return out;
    }
    if total == 0 {
        let mut out = vec![String::new(); lengths.len()];
        out[0] = text.to_owned();
        return out;
    }
    let mut used = 0usize;
    for (i, len) in lengths.iter().enumerate() {
        let take = if i + 1 == lengths.len() {
            chars.len().saturating_sub(used)
        } else {
            chars.len() * len / total
        };
        let end = (used + take).min(chars.len());
        out.push(chars[used..end].iter().collect());
        used = end;
    }
    out
}

/// 改写 ZIP 中的若干条目，其余字节原样复制。`mimetype` 保持不压缩，排在最前。
pub(crate) fn rewrite_zip(template: &[u8], replaced: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut source = zip::ZipArchive::new(Cursor::new(template))
        .map_err(|e| invalid(format!("原件不是有效 ZIP：{e}")))?;
    let mut buffer = Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(&mut buffer);
    for index in 0..source.len() {
        let mut entry = source
            .by_index_raw(index)
            .map_err(|e| invalid(format!("读取原件条目失败：{e}")))?;
        let name = entry
            .name()
            .map_err(|e| invalid(format!("原件条目名不是有效 UTF-8：{e}")))?
            .into_owned();
        let is_dir = entry.is_dir();
        let stored = name == "mimetype";
        let options = zip::write::SimpleFileOptions::default().compression_method(if stored {
            zip::CompressionMethod::Stored
        } else {
            zip::CompressionMethod::Deflated
        });
        if is_dir {
            writer
                .add_directory(name, options)
                .map_err(|e| invalid(format!("写入原件目录失败：{e}")))?;
            continue;
        }
        writer
            .start_file(name.clone(), options)
            .map_err(|e| invalid(format!("写入原件条目失败：{e}")))?;
        match replaced.iter().find(|(path, _)| *path == name) {
            Some((_, bytes)) => writer
                .write_all(bytes)
                .map_err(|e| invalid(format!("写入回填条目失败：{e}")))?,
            None => {
                std::io::copy(&mut entry, &mut writer)
                    .map_err(|e| invalid(format!("复制原件条目失败：{e}")))?;
            }
        }
    }
    writer
        .finish()
        .map_err(|e| invalid(format!("收尾 ZIP 失败：{e}")))?;
    Ok(buffer.into_inner())
}

/// 从 ZIP 中读取一个 UTF-8 文本条目。
pub(crate) fn read_zip_text(bytes: &[u8], name: &str) -> Result<String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| invalid(format!("原件不是有效 ZIP：{e}")))?;
    let mut file = archive
        .by_name(name)
        .map_err(|e| invalid(format!("原件缺少 {name}：{e}")))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|e| invalid(format!("读取 {name} 失败：{e}")))?;
    Ok(text)
}

impl Translations {
    /// 全书顺序的现行译稿段落文本；缺块、段号不符或空译整批拒绝，避免回填错位。
    pub(crate) fn translated_paragraphs(&self, project: &str) -> Result<Vec<String>> {
        let project_data = self.project(project)?;
        let mut out = Vec::new();
        for (_, block) in super::all_blocks(&project_data) {
            let bytes = self
                .read_translation(project, &block.block_id)?
                .ok_or_else(|| {
                    invalid(format!(
                        "块 {} 没有译稿，不能回填；请先完成译稿并准奏或定稿",
                        block.block_id
                    ))
                })?;
            let target = super::read_approved_target(&bytes, &block.block_id)?;
            let paragraphs = match target {
                super::ApprovedTarget::Plain(prompt) => prompt
                    .paragraphs
                    .into_iter()
                    .map(|p| (p.id, p.text))
                    .collect::<Vec<_>>(),
                super::ApprovedTarget::Marked(_) => {
                    return Err(invalid(
                        "原件回填不能保留译文文学标记；请改用 HTML 成稿，不能静默删除标记",
                    ));
                }
            };
            if paragraphs.len() != block.paragraphs.len() {
                return Err(invalid(format!(
                    "块 {} 的译稿段数 {} 与原文段数 {} 不一致，拒绝回填",
                    block.block_id,
                    paragraphs.len(),
                    block.paragraphs.len()
                )));
            }
            for ((id, text), source) in paragraphs.into_iter().zip(&block.paragraphs) {
                if id != source.id {
                    return Err(invalid(format!(
                        "块 {} 的译稿段号 {id} 与原文 {} 不符，拒绝回填",
                        block.block_id, source.id
                    )));
                }
                if text.trim().is_empty() {
                    return Err(invalid(format!(
                        "块 {} 段 {id} 的译文为空，拒绝回填",
                        block.block_id
                    )));
                }
                out.push(text);
            }
        }
        Ok(out)
    }

    fn source_docs_dir(&self, project: &str) -> Result<PathBuf> {
        // 先确认项目存在，再用项目 ID 作目录成分。
        self.project(project)?;
        super::ensure_plain_dirs(
            &self.root,
            &[".ziyuan", "text-projects", project, "source-docs"],
        )
    }

    /// 导入成功后把原件留在项目内，供日后回填使用；原件字节不变。
    pub(crate) fn store_source_package(
        &self,
        project: &str,
        extension: &str,
        bytes: &[u8],
    ) -> Result<String> {
        if !extension
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            return Err(invalid("原件扩展名不合法"));
        }
        let name = format!("source.{extension}");
        let dir = self.source_docs_dir(project)?;
        super::write_plain(&dir.join(&name), bytes)?;
        Ok(name)
    }

    /// 读取项目内保留的原件；没有或以旧方式导入时报出可执行的下一步。
    pub(crate) fn source_package(
        &self,
        project: &str,
        extension: &str,
    ) -> Result<(String, Vec<u8>)> {
        let dir = self
            .root
            .join(".ziyuan")
            .join("text-projects")
            .join(project)
            .join("source-docs");
        let path = dir.join(format!("source.{extension}"));
        let Some(bytes) = read_plain(&path)? else {
            return Err(invalid(format!(
                "项目没有保留原始 .{extension}；请用 text import-{extension} 重新导入后再回填"
            )));
        };
        Ok((path.to_string_lossy().into_owned(), bytes))
    }
}
