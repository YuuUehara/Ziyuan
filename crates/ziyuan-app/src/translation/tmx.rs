//! Explicit plain-text TMX 1.4 exchange. Inline markup is rejected, never flattened.
use super::{Result, Translations, invalid, sha256_hex};
use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
const BYTES: usize = 8 * 1024 * 1024;
const RECORDS: usize = 10000;
type Pair = (String, String);
type Parsed = (Vec<Pair>, Vec<String>);

#[derive(Clone, Debug, Serialize)]
pub struct TmxPreview {
    pub fingerprint: String,
    pub entries: usize,
    pub duplicates: usize,
    pub source_language: String,
    pub target_language: String,
    pub warnings: Vec<String>,
}
fn xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r') || (c >= '\u{20}' && c != '\u{fffe}' && c != '\u{ffff}')
}
fn escape(text: &str) -> Result<String> {
    if !text.chars().all(xml_char) {
        return Err(invalid("TMX文字含XML不能表示的控制字符"));
    }
    Ok(text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\r', "&#13;"))
}
fn attributes(event: &BytesStart<'_>) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for attr in event.attributes() {
        let attr = attr.map_err(|e| invalid(format!("TMX属性无效：{e}")))?;
        let name =
            std::str::from_utf8(attr.key.as_ref()).map_err(|_| invalid("TMX属性不是UTF-8"))?;
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|e| invalid(e.to_string()))?;
        if out.insert(name.into(), value.into_owned()).is_some() {
            return Err(invalid("TMX属性重复"));
        }
    }
    Ok(out)
}
struct Parser<'a> {
    source: &'a str,
    target: &'a str,
    stack: Vec<String>,
    root: bool,
    header: bool,
    body: bool,
    row: BTreeMap<String, String>,
    language: Option<String>,
    segment: Option<String>,
    segment_seen: bool,
    pairs: Vec<(String, String)>,
    metadata: bool,
    other_languages: bool,
}
impl<'a> Parser<'a> {
    fn open(&mut self, event: &BytesStart<'_>) -> Result<()> {
        let name = std::str::from_utf8(event.name().as_ref())
            .map_err(|_| invalid("TMX标签不是UTF-8"))?
            .to_owned();
        let attrs = attributes(event)?;
        let parent = self.stack.last().map(String::as_str);
        match (name.as_str(), parent) {
            ("tmx", None) if !self.root => {
                if attrs.get("version").map(String::as_str) != Some("1.4") {
                    return Err(invalid("仅支持TMX 1.4纯文本格式"));
                }
                self.root = true;
            }
            ("header", Some("tmx")) if !self.header && !self.body => {
                let language = attrs
                    .get("srclang")
                    .ok_or_else(|| invalid("TMX缺少来源语种"))?;
                if language != "*all*" && !language.eq_ignore_ascii_case(self.source) {
                    return Err(invalid("TMX来源语种与项目不符"));
                }
                self.header = true;
            }
            ("body", Some("tmx")) if self.header && !self.body => self.body = true,
            ("tu", Some("body")) => {
                self.row.clear();
            }
            ("tuv", Some("tu")) => {
                self.language = Some(
                    attrs
                        .get("xml:lang")
                        .ok_or_else(|| invalid("TMX译段缺xml:lang"))?
                        .to_ascii_lowercase(),
                );
                self.segment_seen = false;
            }
            ("seg", Some("tuv")) if !self.segment_seen => {
                self.segment_seen = true;
                self.segment = Some(String::new());
            }
            ("prop" | "note", Some("header" | "tu" | "tuv")) => self.metadata = true,
            _ => {
                return Err(invalid(format!(
                    "不支持的TMX结构或行内标记：{name}；本次未导入"
                )));
            }
        }
        self.stack.push(name);
        Ok(())
    }
    fn close(&mut self, name: &[u8]) -> Result<()> {
        let name = std::str::from_utf8(name).map_err(|_| invalid("TMX标签不是UTF-8"))?;
        if self.stack.pop().as_deref() != Some(name) {
            return Err(invalid("TMX标签不匹配"));
        }
        match name {
            "seg" => {
                let segment = self
                    .segment
                    .take()
                    .ok_or_else(|| invalid("TMX译段缺正文"))?;
                if segment.trim().is_empty() {
                    return Err(invalid("TMX原译文不能为空"));
                }
                let language = self
                    .language
                    .clone()
                    .ok_or_else(|| invalid("TMX译段缺语种"))?;
                if self.row.insert(language, segment).is_some() {
                    return Err(invalid("同一TMX单元含重复语种，不能猜测译法"));
                }
            }
            "tuv" => {
                if !self.segment_seen {
                    return Err(invalid("TMX译段缺seg正文"));
                }
                self.language = None;
            }
            "tu" => {
                let source = self
                    .row
                    .get(&self.source.to_ascii_lowercase())
                    .ok_or_else(|| invalid("TMX单元缺项目原文语种"))?;
                let target = self
                    .row
                    .get(&self.target.to_ascii_lowercase())
                    .ok_or_else(|| invalid("TMX单元缺项目译文语种"))?;
                if self.pairs.len() >= RECORDS {
                    return Err(invalid("TMX超过10000条上限"));
                }
                self.pairs.push((source.clone(), target.clone()));
                if self.row.len() > 2 {
                    self.other_languages = true;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn text(&mut self, text: &str) -> Result<()> {
        if !text.chars().all(xml_char) {
            return Err(invalid("TMX文字含无效XML字符"));
        }
        match self.stack.last().map(String::as_str) {
            Some("seg") => {
                let segment = self
                    .segment
                    .as_mut()
                    .ok_or_else(|| invalid("TMX结构无效"))?;
                if segment.len() + text.len() > 256 * 1024 {
                    return Err(invalid("TMX单侧译文超过256 KiB"));
                }
                segment.push_str(text);
            }
            Some("prop" | "note") => {}
            _ if !text.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r')) => {
                return Err(invalid("TMX正文必须放在seg内"));
            }
            _ => {}
        }
        Ok(())
    }
}
fn parse(bytes: &[u8], source: &str, target: &str) -> Result<Parsed> {
    if bytes.len() > BYTES {
        return Err(invalid("TMX超过8 MiB"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("TMX仅接受UTF-8"))?;
    if source.eq_ignore_ascii_case(target) {
        return Err(invalid("TMX交换要求项目原译文语种不同"));
    }
    let mut p = Parser {
        source,
        target,
        stack: Vec::new(),
        root: false,
        header: false,
        body: false,
        row: BTreeMap::new(),
        language: None,
        segment: None,
        segment_seen: false,
        pairs: Vec::new(),
        metadata: false,
        other_languages: false,
    };
    let mut reader = Reader::from_str(text);
    loop {
        match reader
            .read_event()
            .map_err(|e| invalid(format!("TMX XML无效：{e}")))?
        {
            Event::Start(e) => p.open(&e)?,
            Event::Empty(e) => {
                p.open(&e)?;
                p.close(e.name().as_ref())?;
            }
            Event::End(e) => p.close(e.name().as_ref())?,
            Event::Text(e) => p.text(
                &e.xml_content(quick_xml::XmlVersion::Implicit1_0)
                    .map_err(|e| invalid(e.to_string()))?,
            )?,
            Event::CData(e) => {
                if !matches!(
                    p.stack.last().map(String::as_str),
                    Some("seg" | "prop" | "note")
                ) {
                    return Err(invalid("TMX CDATA须在正文或备注内"));
                }
                p.text(
                    &e.xml_content(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(|e| invalid(e.to_string()))?,
                )?;
            }
            Event::GeneralRef(e) => {
                if !matches!(
                    p.stack.last().map(String::as_str),
                    Some("seg" | "prop" | "note")
                ) {
                    return Err(invalid("TMX实体须在正文或备注内"));
                }
                let name =
                    std::str::from_utf8(e.as_ref()).map_err(|_| invalid("TMX实体不是UTF-8"))?;
                let ch = match name {
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    "apos" => '\'',
                    _ => {
                        let number = if let Some(hex) = name.strip_prefix("#x") {
                            u32::from_str_radix(hex, 16).ok()
                        } else {
                            name.strip_prefix('#').and_then(|s| s.parse().ok())
                        };
                        number
                            .and_then(char::from_u32)
                            .ok_or_else(|| invalid("TMX含自定义或外部实体，拒绝导入"))?
                    }
                };
                p.text(&ch.to_string())?;
            }
            Event::DocType(_) => return Err(invalid("TMX禁止DTD和外部实体")),
            Event::Decl(e) => {
                if e.version().map_err(|e| invalid(e.to_string()))?.as_ref() != b"1.0" {
                    return Err(invalid("TMX仅接受XML 1.0"));
                }
                if let Some(encoding) = e.encoding() {
                    let encoding = encoding.map_err(|e| invalid(e.to_string()))?;
                    if !encoding.eq_ignore_ascii_case(b"UTF-8") {
                        return Err(invalid("TMX仅接受UTF-8声明"));
                    }
                }
            }
            Event::Comment(_) => {}
            Event::PI(_) => return Err(invalid("TMX不支持处理指令")),
            Event::Eof => break,
        }
    }
    if !p.root || !p.header || !p.body || !p.stack.is_empty() || p.pairs.is_empty() {
        return Err(invalid("TMX不完整或无可导入单元"));
    }
    let mut warnings = Vec::new();
    if p.metadata {
        warnings.push("仅导入原译文；TMX备注、属性及来源标签不写入本地记忆。".into());
    }
    if p.other_languages {
        warnings.push("仅导入当前项目的原译文语种对，其他语种未导入。".into());
    }
    Ok((p.pairs, warnings))
}
impl Translations {
    pub fn export_tmx(&self, project: &str) -> Result<Vec<u8>> {
        let p = self.project(project)?;
        let entries = self.tm_list(project)?;
        if entries.is_empty() {
            return Err(invalid("尚无翻译记忆可导出"));
        }
        if p.metadata
            .source_language
            .eq_ignore_ascii_case(&p.metadata.target_language)
        {
            return Err(invalid("TMX交换要求项目原译文语种不同"));
        }
        if entries.len() > RECORDS {
            return Err(invalid("TMX超过10000条上限"));
        }
        let source = escape(&p.metadata.source_language)?;
        let target = escape(&p.metadata.target_language)?;
        let mut xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<tmx version=\"1.4\"><header creationtool=\"ZiYuan\" creationtoolversion=\"{}\" segtype=\"paragraph\" o-tmf=\"plaintext\" adminlang=\"en-US\" srclang=\"{source}\" datatype=\"PlainText\"/><body>\n",
            env!("CARGO_PKG_VERSION")
        );
        for entry in entries {
            xml.push_str(&format!("<tu><tuv xml:lang=\"{source}\"><seg>{}</seg></tuv><tuv xml:lang=\"{target}\"><seg>{}</seg></tuv></tu>\n",escape(&entry.source)?,escape(&entry.target)?));
            if xml.len() > BYTES {
                return Err(invalid("TMX超过8 MiB"));
            }
        }
        xml.push_str("</body></tmx>\n");
        if xml.len() > BYTES {
            return Err(invalid("TMX超过8 MiB"));
        }
        Ok(xml.into_bytes())
    }
    pub fn preview_tmx(&self, project: &str, bytes: &[u8]) -> Result<TmxPreview> {
        let p = self.project(project)?;
        let (pairs, warnings) = parse(
            bytes,
            &p.metadata.source_language,
            &p.metadata.target_language,
        )?;
        let entries = self.tm_list(project)?;
        let mut seen: BTreeSet<_> = entries
            .iter()
            .map(|e| (e.source.clone(), e.target.clone()))
            .collect();
        let mut duplicates = 0;
        for pair in &pairs {
            if !seen.insert(pair.clone()) {
                duplicates += 1;
            }
        }
        Ok(TmxPreview {
            fingerprint: sha256_hex(&serde_json::to_vec(&(&p, sha256_hex(bytes), &entries))?),
            entries: pairs.len(),
            duplicates,
            source_language: p.metadata.source_language,
            target_language: p.metadata.target_language,
            warnings,
        })
    }
    pub fn import_tmx(&self, project: &str, bytes: &[u8], expected: &str) -> Result<usize> {
        let p = self.project(project)?;
        let (pairs, _) = parse(
            bytes,
            &p.metadata.source_language,
            &p.metadata.target_language,
        )?;
        let entries = self.tm_list(project)?;
        let fingerprint = sha256_hex(&serde_json::to_vec(&(&p, sha256_hex(bytes), &entries))?);
        if fingerprint != expected {
            return Err(invalid("TMX或本地记忆已变化，请重新预览"));
        }
        self.tm_merge_checked(project, &pairs, &entries)
    }
}
