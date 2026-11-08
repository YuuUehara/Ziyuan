//! DOCX import with a conservative fail-closed policy: only plain paragraphs are accepted.
use super::{Result, Translations, invalid};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::io::{Cursor, Read};

fn docx_text(bytes: &[u8]) -> Result<String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| invalid(format!("DOCX 不是有效 ZIP：{e}")))?;
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .map_err(|_| invalid("DOCX 缺少 word/document.xml"))?
        .read_to_string(&mut xml)
        .map_err(|e| invalid(format!("读取 DOCX 失败：{e}")))?;
    for marker in ["<w:tbl", "<w:drawing", "<w:footnoteReference", "<w:endnoteReference"] {
        if xml.contains(marker) {
            return Err(invalid("DOCX 含表格、图片或脚注，暂不支持；请先转为纯文本"));
        }
    }
    if xml.contains("<w:b/>") || xml.contains("<w:i/>") || xml.contains("<w:b ") || xml.contains("<w:i ") {
        return Err(invalid("DOCX 含加粗或斜体，暂不支持标记回填；请先转为纯文本"));
    }
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(false);
    let mut paragraphs = Vec::new();
    let mut current = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"p" => current.clear(),
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"t" => in_text = true,
            Ok(Event::End(e)) if e.local_name().as_ref() == b"t" => in_text = false,
            Ok(Event::End(e)) if e.local_name().as_ref() == b"p" => {
                let text = current.trim().to_owned();
                if !text.is_empty() {
                    paragraphs.push(text);
                }
                current.clear();
            }
            Ok(Event::Text(e)) if in_text => {
                current.push_str(&e.unescape().map_err(|e| invalid(format!("DOCX 文本解码失败：{e}")))?);
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(invalid(format!("DOCX XML 解析失败：{e}"))),
            _ => {}
        }
    }
    if paragraphs.is_empty() {
        return Err(invalid("DOCX 没有可导入的文本段落"));
    }
    Ok(paragraphs.join("\n\n") + "\n")
}

impl Translations {
    /// Import a plain DOCX into an existing, not-yet-imported text project.
    pub fn import_docx(&self, project: &str, path: &std::path::Path) -> Result<usize> {
        let bytes = std::fs::read(path).map_err(|e| invalid(format!("读取 DOCX 失败：{e}")))?;
        let text = docx_text(&bytes)?;
        let temp = std::env::temp_dir().join(format!(
            "ziyuan-docx-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        ));
        std::fs::write(&temp, text.as_bytes()).map_err(|e| invalid(format!("写临时文件失败：{e}")))?;
        let result = self
            .texts()
            .import(project, std::slice::from_ref(&temp))
            .map_err(|e| invalid(e.to_string()));
        let _ = std::fs::remove_file(&temp);
        result.map(|_| text.lines().count())
    }
}
