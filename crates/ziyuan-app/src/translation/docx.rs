//! DOCX 导入与回填：保留原有段落、粗体、斜体、表格与图片，只替换文字节点。
//!
//! 导入只取 `word/document.xml` 的文字，含表格单元格与图片旁的文字；回填按原段落顺序
//! 把译文按文字节点长度比例写回，因此原有 run 的粗体／斜体仍在。段落数不一致时整批
//! 拒绝，绝不猜对应关系。
use super::packages::{distribute, read_zip_text, rewrite_zip, write_new_file};
use super::{Result, Translations, invalid};
use quick_xml::events::{BytesText, Event};
use quick_xml::{Reader, Writer, XmlVersion};

struct Segment {
    text: String,
    /// 每个 `<w:t>` 的字符数，按文档顺序。
    lengths: Vec<usize>,
}

/// 逐段落取文字与文字节点长度；空段落保留占位 `None`，供回填对齐。
fn segments(xml: &str) -> Result<Vec<Option<Segment>>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut out = Vec::new();
    let mut in_paragraph = false;
    let mut in_text = false;
    let mut current = String::new();
    let mut lengths = Vec::new();
    let mut node_chars = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"p" => {
                if in_paragraph {
                    return Err(invalid("DOCX 含嵌套段落，暂不支持回填"));
                }
                in_paragraph = true;
                current.clear();
                lengths.clear();
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"p" => {
                if !in_paragraph {
                    continue;
                }
                in_paragraph = false;
                in_text = false;
                if current.trim().is_empty() {
                    out.push(None);
                } else {
                    out.push(Some(Segment {
                        text: current.trim().to_owned(),
                        lengths: std::mem::take(&mut lengths),
                    }));
                }
            }
            Ok(Event::Empty(e)) if e.local_name().as_ref() == b"t" && in_paragraph => {
                lengths.push(0);
            }
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"t" && in_paragraph => {
                in_text = true;
                node_chars = 0;
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"t" && in_paragraph => {
                in_text = false;
                lengths.push(node_chars);
            }
            Ok(Event::Text(e)) if in_text => {
                let text = e
                    .xml_content(XmlVersion::Implicit1_0)
                    .map_err(|e| invalid(format!("DOCX 文本解码失败：{e}")))?;
                current.push_str(&text);
                node_chars += text.chars().count();
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(invalid(format!("DOCX XML 解析失败：{e}"))),
            _ => {}
        }
    }
    Ok(out)
}

/// 回填写入文字：跳过原文字事件，在每个 `</w:t>` 前写入本次分配。
fn rewrite(xml: &str, allocations: &[Option<Vec<String>>]) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut paragraph = 0usize;
    let mut in_paragraph = false;
    let mut in_text = false;
    let mut node = 0usize;
    let mut active: Option<Vec<String>> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"p" => {
                in_paragraph = true;
                active = allocations.get(paragraph).cloned().flatten();
                node = 0;
                writer
                    .write_event(Event::Start(e))
                    .map_err(|e| invalid(format!("DOCX 写出失败：{e}")))?;
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"p" => {
                writer
                    .write_event(Event::End(e))
                    .map_err(|e| invalid(format!("DOCX 写出失败：{e}")))?;
                in_paragraph = false;
                active = None;
                paragraph += 1;
            }
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"t" && in_paragraph => {
                in_text = true;
                writer
                    .write_event(Event::Start(e))
                    .map_err(|e| invalid(format!("DOCX 写出失败：{e}")))?;
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"t" && in_paragraph => {
                in_text = false;
                if let Some(values) = &active {
                    if let Some(value) = values.get(node) {
                        writer
                            .write_event(Event::Text(BytesText::new(value)))
                            .map_err(|e| invalid(format!("DOCX 写出失败：{e}")))?;
                    }
                    node += 1;
                }
                writer
                    .write_event(Event::End(e))
                    .map_err(|e| invalid(format!("DOCX 写出失败：{e}")))?;
            }
            Ok(Event::Empty(e)) if e.local_name().as_ref() == b"t" && in_paragraph => {
                if active.is_some() {
                    node += 1;
                }
                writer
                    .write_event(Event::Empty(e))
                    .map_err(|e| invalid(format!("DOCX 写出失败：{e}")))?;
            }
            Ok(Event::Text(_)) if in_text && active.is_some() => {
                // 原文由分配替换，不重复写出。
            }
            Ok(Event::Eof) => break,
            Ok(event) => writer
                .write_event(event)
                .map_err(|e| invalid(format!("DOCX 写出失败：{e}")))?,
            Err(e) => return Err(invalid(format!("DOCX XML 解析失败：{e}"))),
        }
    }
    String::from_utf8(writer.into_inner()).map_err(|_| invalid("DOCX 写出结果不是 UTF-8"))
}

fn docx_text(bytes: &[u8]) -> Result<String> {
    let xml = read_zip_text(bytes, "word/document.xml")?;
    let paragraphs: Vec<String> = segments(&xml)?
        .into_iter()
        .flatten()
        .map(|segment| segment.text)
        .collect();
    if paragraphs.is_empty() {
        return Err(invalid("DOCX 没有可导入的文本段落"));
    }
    Ok(paragraphs.join("\n\n") + "\n")
}

impl Translations {
    /// 导入 DOCX 的可见文字；粗体、斜体、表格与图片留在原件，供回填保留。
    pub fn import_docx(&self, project: &str, path: &std::path::Path) -> Result<usize> {
        let bytes = std::fs::read(path).map_err(|e| invalid(format!("读取 DOCX 失败：{e}")))?;
        let text = docx_text(&bytes)?;
        let temp = std::env::temp_dir().join(format!(
            "ziyuan-docx-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&temp, text.as_bytes())
            .map_err(|e| invalid(format!("写临时文件失败：{e}")))?;
        let result = self
            .texts()
            .import(project, std::slice::from_ref(&temp))
            .map_err(|e| invalid(e.to_string()));
        let _ = std::fs::remove_file(&temp);
        result?;
        self.store_source_package(project, "docx", &bytes)?;
        Ok(text.lines().filter(|line| !line.is_empty()).count())
    }

    /// 把现行译稿写回原件：保留粗体、斜体、表格与图片，只替换文字。
    pub fn export_docx(&self, project: &str, output: &std::path::Path) -> Result<usize> {
        let texts = self.translated_paragraphs(project)?;
        let (_, template) = self.source_package(project, "docx")?;
        let xml = read_zip_text(&template, "word/document.xml")?;
        let shapes = segments(&xml)?;
        let counted = shapes.iter().filter(|segment| segment.is_some()).count();
        if counted != texts.len() {
            return Err(invalid(format!(
                "DOCX 文字段落数 {counted} 与译稿段数 {} 不一致，拒绝回填以免错位",
                texts.len()
            )));
        }
        let mut allocations = Vec::with_capacity(shapes.len());
        let mut text_index = 0usize;
        for shape in &shapes {
            match shape {
                Some(segment) => {
                    allocations.push(Some(distribute(&texts[text_index], &segment.lengths)));
                    text_index += 1;
                }
                None => allocations.push(None),
            }
        }
        let rewritten = rewrite(&xml, &allocations)?;
        let package = rewrite_zip(
            &template,
            &[("word/document.xml".to_owned(), rewritten.into_bytes())],
        )?;
        write_new_file(output, &package)?;
        Ok(texts.len())
    }
}

/// 仅测试使用：从字节构造 ZIP。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_keep_markup_and_table_cells_in_order() {
        let xml = r#"<w:document><w:body><w:p><w:r><w:rPr><w:b/></w:rPr><w:t>甲</w:t></w:r><w:r><w:rPr><w:i/></w:rPr><w:t>乙</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>丙</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:drawing/></w:r></w:p></w:body></w:document>"#;
        let shapes = segments(xml).unwrap();
        let texts: Vec<_> = shapes.iter().flatten().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["甲乙", "丙"]);
        assert_eq!(shapes[0].as_ref().unwrap().lengths, [1, 1]);
        let allocations = vec![
            Some(distribute("一二三四", &[1, 1])),
            Some(distribute("五", &[1])),
            None,
        ];
        let rewritten = rewrite(xml, &allocations).unwrap();
        assert!(rewritten.contains("<w:b/>"));
        assert!(rewritten.contains("<w:i/>"));
        assert!(rewritten.contains("<w:drawing/>"));
        assert!(rewritten.contains("一二"));
        assert!(rewritten.contains("三四"));
        assert!(rewritten.contains("五"));
    }
}
