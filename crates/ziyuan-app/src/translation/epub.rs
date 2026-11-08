//! EPUB import: spine order, plain text only; unsupported markup fails closed.
use super::{Result, Translations, invalid};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use std::io::{Cursor, Read};

fn attr(event: &BytesStart, name: &[u8]) -> Option<String> {
    event
        .attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

fn read_entry(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<String> {
    let mut file = zip.by_name(name).map_err(|e| invalid(format!("EPUB 缺少 {name}：{e}")))?;
    let mut out = String::new();
    file.read_to_string(&mut out).map_err(|e| invalid(format!("读取 {name} 失败：{e}")))?;
    Ok(out)
}

fn opf_path(zip: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Result<String> {
    let container = read_entry(zip, "META-INF/container.xml")?;
    let mut reader = Reader::from_str(&container);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"rootfile" => {
                if let Some(path) = attr(&e, b"full-path") { return Ok(path); }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(invalid(format!("container.xml 解析失败：{e}"))),
            _ => {}
        }
    }
    Err(invalid("EPUB 没有 rootfile"))
}

fn spine(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, opf: &str) -> Result<Vec<String>> {
    let xml = read_entry(zip, opf)?;
    let mut reader = Reader::from_str(&xml);
    let mut manifest = std::collections::HashMap::new();
    let mut order = Vec::new();
    let mut in_spine = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"manifest" => {}
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"item" => {
                if let (Some(id), Some(href)) = (attr(&e, b"id"), attr(&e, b"href")) {
                    manifest.insert(id, href);
                }
            }
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"spine" => in_spine = true,
            Ok(Event::End(e)) if e.local_name().as_ref() == b"spine" => in_spine = false,
            Ok(Event::Start(e)) if in_spine && e.local_name().as_ref() == b"itemref" => {
                if let Some(idref) = attr(&e, b"idref") { order.push(idref); }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(invalid(format!("OPF 解析失败：{e}"))),
            _ => {}
        }
    }
    let base = opf.rsplit_once('/').map(|(dir, _)| dir.to_owned()).unwrap_or_default();
    order
        .into_iter()
        .map(|id| {
            let href = manifest.get(&id).ok_or_else(|| invalid(format!("OPF 缺少清单项 {id}")))?;
            Ok(if base.is_empty() { href.clone() } else { format!("{base}/{href}") })
        })
        .collect()
}

fn html_text(path: &str, xml: &str) -> Result<String> {
    for tag in ["<img", "<svg", "<table", "<script", "<b>", "<i>", "<strong>", "<em>"] {
        if xml.contains(tag) {
            return Err(invalid(format!("{path} 含图片/表格/粗斜体标记，暂不支持")));
        }
    }
    let mut reader = Reader::from_str(xml);
    let mut in_body = false;
    let mut text = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"body" => in_body = true,
            Ok(Event::End(e)) if e.local_name().as_ref() == b"body" => in_body = false,
            Ok(Event::Start(e))
                if in_body && matches!(e.local_name().as_ref(), b"p" | b"div" | b"h1" | b"h2" | b"h3" | b"h4" | b"li" | b"blockquote") =>
            {
                if !text.is_empty() && !text.ends_with("\n\n") { text.push_str("\n\n"); }
            }
            Ok(Event::Text(e)) if in_body => {
                text.push_str(&e.unescape().map_err(|e| invalid(format!("{path} 文本解码失败：{e}")))?);
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(invalid(format!("{path} 解析失败：{e}"))),
            _ => {}
        }
    }
    Ok(text.trim().to_owned())
}

impl Translations {
    pub fn import_epub(&self, project: &str, path: &std::path::Path) -> Result<usize> {
        let bytes = std::fs::read(path).map_err(|e| invalid(format!("读取 EPUB 失败：{e}")))?;
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes.as_slice())).map_err(|e| invalid(format!("EPUB 不是有效 ZIP：{e}")))?;
        let opf = opf_path(&mut zip)?;
        let files = spine(&mut zip, &opf)?;
        let mut parts = Vec::new();
        for file in files {
            let xml = read_entry(&mut zip, &file)?;
            let text = html_text(&file, &xml)?;
            if !text.is_empty() { parts.push(text); }
        }
        if parts.is_empty() { return Err(invalid("EPUB 没有可导入的正文")); }
        let body = parts.join("\n\n") + "\n";
        let temp = std::env::temp_dir().join(format!("ziyuan-epub-{}.txt", std::process::id()));
        std::fs::write(&temp, body.as_bytes()).map_err(|e| invalid(e.to_string()))?;
        let result = self.texts().import(project, std::slice::from_ref(&temp)).map_err(|e| invalid(e.to_string()));
        let _ = std::fs::remove_file(&temp);
        result.map(|_| parts.len())
    }
}
