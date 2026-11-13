//! EPUB 导入与回填：按 spine 顺序取正文，保留粗体、斜体、表格与图片，只替换文字。
//!
//! 回填按原文字节点长度比例分配译文，图片与表格结构原样保留；spine 文档内的段落数
//! 与译稿段数不一致时整批拒绝，绝不猜对应关系。
use super::packages::{distribute, rewrite_zip, write_new_file};
use super::{Result, Translations, invalid};
use quick_xml::events::{BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer, XmlVersion};
use std::io::{Cursor, Read};

const BLOCKS: [&[u8]; 10] = [
    b"p",
    b"div",
    b"h1",
    b"h2",
    b"h3",
    b"h4",
    b"li",
    b"blockquote",
    b"td",
    b"th",
];

fn attr(event: &BytesStart, name: &[u8]) -> Option<String> {
    event
        .attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| {
            a.normalized_value(XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

fn is_block(event: &BytesStart) -> bool {
    BLOCKS.contains(&event.local_name().as_ref())
}

fn is_hidden(event: &BytesStart) -> bool {
    matches!(event.local_name().as_ref(), b"script" | b"style")
}

fn is_hidden_end(event: &quick_xml::events::BytesEnd) -> bool {
    matches!(event.local_name().as_ref(), b"script" | b"style")
}

fn read_entry(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<String> {
    let mut file = zip
        .by_name(name)
        .map_err(|e| invalid(format!("EPUB 缺少 {name}：{e}")))?;
    let mut out = String::new();
    file.read_to_string(&mut out)
        .map_err(|e| invalid(format!("读取 {name} 失败：{e}")))?;
    Ok(out)
}

fn opf_path(zip: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Result<String> {
    let container = read_entry(zip, "META-INF/container.xml")?;
    let mut reader = Reader::from_str(&container);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.local_name().as_ref() == b"rootfile" => {
                if let Some(path) = attr(&e, b"full-path") {
                    return Ok(path);
                }
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
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.local_name().as_ref() == b"item" => {
                if let (Some(id), Some(href)) = (attr(&e, b"id"), attr(&e, b"href")) {
                    manifest.insert(id, href);
                }
            }
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"spine" => in_spine = true,
            Ok(Event::End(e)) if e.local_name().as_ref() == b"spine" => in_spine = false,
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
                if in_spine && e.local_name().as_ref() == b"itemref" =>
            {
                if let Some(idref) = attr(&e, b"idref") {
                    order.push(idref);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(invalid(format!("OPF 解析失败：{e}"))),
            _ => {}
        }
    }
    let base = opf
        .rsplit_once('/')
        .map(|(dir, _)| dir.to_owned())
        .unwrap_or_default();
    order
        .into_iter()
        .map(|id| {
            let href = manifest
                .get(&id)
                .ok_or_else(|| invalid(format!("OPF 缺少清单项 {id}")))?;
            Ok(if base.is_empty() {
                href.clone()
            } else {
                format!("{base}/{href}")
            })
        })
        .collect()
}

struct Segment {
    text: String,
    /// 有意义文字节点的字符数；空白节点保留不替换。
    lengths: Vec<usize>,
}

#[derive(Default)]
struct Collected {
    segments: Vec<Segment>,
    /// 按文档顺序的每个文字节点：`Some((段落, 节点))` 参与替换，`None` 原样保留。
    plans: Vec<Option<(usize, usize)>>,
}

struct Builder {
    current: String,
    nodes: Vec<(usize, bool)>,
    collected: Collected,
}

impl Builder {
    fn new() -> Self {
        Self {
            current: String::new(),
            nodes: Vec::new(),
            collected: Collected::default(),
        }
    }

    fn push_text(&mut self, text: &str) {
        self.current.push_str(text);
        self.nodes
            .push((text.chars().count(), !text.trim().is_empty()));
    }

    fn flush(&mut self) {
        let text = self.current.trim();
        if !text.is_empty() {
            let mut lengths = Vec::new();
            let mut meaningful = 0usize;
            let segment = self.collected.segments.len();
            for (len, is_text) in &self.nodes {
                if *is_text {
                    self.collected.plans.push(Some((segment, meaningful)));
                    lengths.push(*len);
                    meaningful += 1;
                } else {
                    self.collected.plans.push(None);
                }
            }
            self.collected.segments.push(Segment {
                text: text.to_owned(),
                lengths,
            });
        } else {
            for _ in &self.nodes {
                self.collected.plans.push(None);
            }
        }
        self.current.clear();
        self.nodes.clear();
    }
}

/// 按与正文相同的规则取段落：块级元素是分隔，空白节点不替换。
fn collect(xml: &str, path: &str) -> Result<Collected> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut builder = Builder::new();
    let mut in_body = false;
    let mut hidden = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"body" => in_body = true,
            Ok(Event::End(e)) if e.local_name().as_ref() == b"body" => {
                if in_body {
                    builder.flush();
                }
                in_body = false;
            }
            Ok(Event::Start(e)) if in_body && is_hidden(&e) => hidden += 1,
            Ok(Event::End(e)) if in_body && is_hidden_end(&e) => hidden = hidden.saturating_sub(1),
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if in_body && is_block(&e) => {
                if in_body && hidden == 0 {
                    builder.flush();
                }
            }
            Ok(Event::Text(e)) if in_body && hidden == 0 => {
                let text = e
                    .xml_content(XmlVersion::Implicit1_0)
                    .map_err(|e| invalid(format!("{path} 文本解码失败：{e}")))?;
                builder.push_text(&text);
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(invalid(format!("{path} 解析失败：{e}"))),
            _ => {}
        }
    }
    if in_body {
        builder.flush();
    }
    Ok(builder.collected)
}

fn html_text(path: &str, xml: &str) -> Result<String> {
    let collected = collect(xml, path)?;
    if collected.segments.is_empty() {
        return Ok(String::new());
    }
    Ok(collected
        .segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n"))
}

/// 回填一份 XHTML：只替换参与分配的文字节点，其余事件原样写出。
fn rewrite(xml: &str, path: &str, allocations: &[Vec<String>]) -> Result<String> {
    let collected = collect(xml, path)?;
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut in_body = false;
    let mut hidden = 0usize;
    let mut node = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"body" => {
                in_body = true;
                writer
                    .write_event(Event::Start(e))
                    .map_err(|e| invalid(format!("{path} 写出失败：{e}")))?;
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"body" => {
                in_body = false;
                writer
                    .write_event(Event::End(e))
                    .map_err(|e| invalid(format!("{path} 写出失败：{e}")))?;
            }
            Ok(Event::Start(e)) if in_body && is_hidden(&e) => {
                hidden += 1;
                writer
                    .write_event(Event::Start(e))
                    .map_err(|e| invalid(format!("{path} 写出失败：{e}")))?;
            }
            Ok(Event::End(e)) if in_body && is_hidden_end(&e) => {
                hidden = hidden.saturating_sub(1);
                writer
                    .write_event(Event::End(e))
                    .map_err(|e| invalid(format!("{path} 写出失败：{e}")))?;
            }
            Ok(Event::Text(e)) if in_body && hidden == 0 => {
                let plan = collected.plans.get(node).copied().flatten();
                node += 1;
                match plan.and_then(|(segment, index)| {
                    allocations
                        .get(segment)
                        .and_then(|values| values.get(index))
                        .cloned()
                }) {
                    Some(text) => writer
                        .write_event(Event::Text(BytesText::new(&text)))
                        .map_err(|e| invalid(format!("{path} 写出失败：{e}")))?,
                    None => writer
                        .write_event(Event::Text(e))
                        .map_err(|e| invalid(format!("{path} 写出失败：{e}")))?,
                }
            }
            Ok(Event::Eof) => break,
            Ok(event) => writer
                .write_event(event)
                .map_err(|e| invalid(format!("{path} 写出失败：{e}")))?,
            Err(e) => return Err(invalid(format!("{path} 解析失败：{e}"))),
        }
    }
    String::from_utf8(writer.into_inner()).map_err(|_| invalid("EPUB 写出结果不是 UTF-8"))
}

/// 与导入相同：按 spine 取出正文，段落之间空一行。不含图片与样式。
pub(crate) fn epub_plain_text(bytes: &[u8]) -> Result<(String, usize)> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| invalid(format!("EPUB 不是有效 ZIP：{e}")))?;
    let opf = opf_path(&mut zip)?;
    let files = spine(&mut zip, &opf)?;
    let mut parts = Vec::new();
    for file in files {
        let xml = read_entry(&mut zip, &file)?;
        let text = html_text(&file, &xml)?;
        if !text.is_empty() {
            parts.push(text);
        }
    }
    if parts.is_empty() {
        return Err(invalid("EPUB 没有可导入的正文"));
    }
    Ok((parts.join("\n\n") + "\n", parts.len()))
}

impl Translations {
    pub fn import_epub(&self, project: &str, path: &std::path::Path) -> Result<usize> {
        let bytes = std::fs::read(path).map_err(|e| invalid(format!("读取 EPUB 失败：{e}")))?;
        let (body, parts) = epub_plain_text(&bytes)?;
        let temp = std::env::temp_dir().join(format!(
            "ziyuan-epub-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&temp, body.as_bytes()).map_err(|e| invalid(e.to_string()))?;
        let result = self
            .texts()
            .import(project, std::slice::from_ref(&temp))
            .map_err(|e| invalid(e.to_string()));
        let _ = std::fs::remove_file(&temp);
        result?;
        self.store_source_package(project, "epub", &bytes)?;
        Ok(parts)
    }

    /// 把现行译稿写回 EPUB：保留 spine、图片、表格与行内粗斜体。
    pub fn export_epub(&self, project: &str, output: &std::path::Path) -> Result<usize> {
        let texts = self.translated_paragraphs(project)?;
        let (_, template) = self.source_package(project, "epub")?;
        let mut archive = zip::ZipArchive::new(Cursor::new(template.as_slice()))
            .map_err(|e| invalid(format!("EPUB 不是有效 ZIP：{e}")))?;
        let opf = opf_path(&mut archive)?;
        let files = spine(&mut archive, &opf)?;
        let mut docs = Vec::new();
        for file in &files {
            docs.push((file.clone(), read_entry(&mut archive, file)?));
        }
        drop(archive);
        let mut replaced = Vec::new();
        let mut offset = 0usize;
        for (file, xml) in &docs {
            let collected = collect(xml, file)?;
            if collected.segments.is_empty() {
                continue;
            }
            if offset + collected.segments.len() > texts.len() {
                return Err(invalid(format!(
                    "EPUB 正文段落数超过译稿段数（{file}），拒绝回填以免错位"
                )));
            }
            let mut allocations = Vec::with_capacity(collected.segments.len());
            for segment in &collected.segments {
                allocations.push(distribute(&texts[offset], &segment.lengths));
                offset += 1;
            }
            let rewritten = rewrite(xml, file, &allocations)?;
            replaced.push((file.clone(), rewritten.into_bytes()));
        }
        if offset != texts.len() {
            return Err(invalid(format!(
                "EPUB 正文段落数 {} 与译稿段数 {} 不一致，拒绝回填以免错位",
                offset,
                texts.len()
            )));
        }
        let package = rewrite_zip(&template, &replaced)?;
        write_new_file(output, &package)?;
        Ok(texts.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_keeps_inline_markup_and_replaces_text_nodes() {
        let xml = r#"<html><body><p>甲<b>乙</b><img src="x.png"/></p><table><tr><td>丙</td></tr></table></body></html>"#;
        let collected = collect(xml, "c1.xhtml").unwrap();
        let texts: Vec<_> = collected.segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["甲乙", "丙"]);
        let allocations = vec![distribute("一二三四", &[1, 1]), distribute("五", &[1])];
        let rewritten = rewrite(xml, "c1.xhtml", &allocations).unwrap();
        assert!(rewritten.contains("<b>"));
        assert!(rewritten.contains("<img"));
        assert!(rewritten.contains("<table>"));
        assert!(rewritten.contains("一二"));
        assert!(rewritten.contains("三四"));
        assert!(rewritten.contains("五"));
    }
}
