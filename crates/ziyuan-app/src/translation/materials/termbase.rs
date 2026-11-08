//! 术语表交换：CSV 与 TBX 都保存类别、范围、可用起点、读音、证据与称谓规则。
//!
//! 两个方向都只合并进本地资料草稿（未批准、不外发、不调用模型）。CSV 以表头取列，
//! 旧八列表格仍可读；无法解析的行整批拒绝，不静默丢字段或改写成「通用」类别。
use super::super::{
    AddressRule, GlossaryEvidence, GlossaryPosition, GlossaryScope, GlossarySpeaker,
    GlossaryStatus, Person, PersonVoice, Result, StructuredGlossary, Term, TermCategory,
    Translations, invalid,
};
use super::{MaterialReceipt, MaterialSave};
use std::collections::BTreeMap;

pub(crate) const CSV_HEADER: &[&str] = &[
    "kind",
    "id",
    "source",
    "name",
    "allowed",
    "aliases",
    "status",
    "note",
    "category",
    "scope",
    "available_from",
    "reading",
    "evidence",
    "target",
    "speaker",
    "term",
    "owner",
    "terms",
    "voice",
    "examples",
];

fn quote(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn encode_position(position: &GlossaryPosition) -> String {
    match &position.paragraph {
        Some(paragraph) => format!("{}:{paragraph}", position.chapter),
        None => position.chapter.clone(),
    }
}

fn decode_position(value: &str, row: usize, field: &str) -> Result<GlossaryPosition> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid(format!("第 {row} 行 {field} 不得为空")));
    }
    let (chapter, paragraph) = match value.split_once(':') {
        Some((chapter, paragraph)) => {
            if paragraph.is_empty() {
                return Err(invalid(format!("第 {row} 行 {field} 段落为空")));
            }
            (chapter, Some(paragraph.to_owned()))
        }
        None => (value, None),
    };
    if chapter.is_empty() {
        return Err(invalid(format!("第 {row} 行 {field} 缺少章 ID")));
    }
    Ok(GlossaryPosition {
        chapter: chapter.to_owned(),
        paragraph,
    })
}

fn encode_scope(scope: &GlossaryScope) -> String {
    match scope {
        GlossaryScope::Project => "project".into(),
        GlossaryScope::Range { from, to } => {
            format!("range:{}..{}", encode_position(from), encode_position(to))
        }
    }
}

fn decode_scope(value: &str, row: usize) -> Result<GlossaryScope> {
    let value = value.trim();
    match value {
        "" | "project" => Ok(GlossaryScope::Project),
        _ => {
            let range = value.strip_prefix("range:").ok_or_else(|| {
                invalid(format!("第 {row} 行范围只接受 project 或 range:<起>..<止>"))
            })?;
            let (from, to) = range
                .split_once("..")
                .ok_or_else(|| invalid(format!("第 {row} 行范围缺少 .. 分隔的起止位置")))?;
            Ok(GlossaryScope::Range {
                from: decode_position(from, row, "范围起点")?,
                to: decode_position(to, row, "范围终点")?,
            })
        }
    }
}

fn decode_available_from(value: &str, row: usize) -> Result<Option<GlossaryPosition>> {
    let value = value.trim();
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(decode_position(value, row, "可用起点")?))
    }
}

fn decode_evidence(value: &str, row: usize) -> Result<Vec<GlossaryEvidence>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(value).map_err(|e| invalid(format!("第 {row} 行证据不是有效 JSON：{e}")))
}

fn decode_examples(value: &str, row: usize) -> Result<Option<Vec<String>>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.starts_with('[') {
        let examples: Vec<String> = serde_json::from_str(value)
            .map_err(|e| invalid(format!("第 {row} 行声线范例不是有效 JSON：{e}")))?;
        Ok((!examples.is_empty()).then_some(examples))
    } else {
        let examples = split_list(value);
        Ok((!examples.is_empty()).then_some(examples))
    }
}

fn decode_status(value: &str) -> GlossaryStatus {
    if value.trim() == "confirmed" {
        GlossaryStatus::Confirmed
    } else {
        GlossaryStatus::Tentative
    }
}

fn decode_category(value: &str, row: usize) -> Result<TermCategory> {
    Ok(match value.trim() {
        "" | "common" => TermCategory::Common,
        "person_name" => TermCategory::PersonName,
        "place" => TermCategory::Place,
        "organization" => TermCategory::Organization,
        "title" => TermCategory::Title,
        "culture" => TermCategory::Culture,
        other => {
            return Err(invalid(format!(
                "第 {row} 行类别未知：{other}（可用 person_name／place／organization／title／culture／common）"
            )));
        }
    })
}

fn decode_speaker(value: &str, row: usize) -> Result<GlossarySpeaker> {
    let value = value.trim();
    Ok(match value {
        "" | "unknown" => GlossarySpeaker::Unknown,
        "narration" => GlossarySpeaker::Narration,
        _ => {
            let id = value.strip_prefix("person:").ok_or_else(|| {
                invalid(format!(
                    "第 {row} 行说话人只接受 person:<人物ID>、narration 或 unknown"
                ))
            })?;
            if id.is_empty() {
                return Err(invalid(format!("第 {row} 行说话人缺少人物 ID")));
            }
            GlossarySpeaker::Person {
                person_id: id.to_owned(),
            }
        }
    })
}

fn encode_speaker(speaker: &GlossarySpeaker) -> String {
    match speaker {
        GlossarySpeaker::Person { person_id } => format!("person:{person_id}"),
        GlossarySpeaker::Narration => "narration".into(),
        GlossarySpeaker::Unknown => "unknown".into(),
    }
}

/// 完整 CSV 读取器：支持带引号的逗号、引号与换行。记录按出现顺序返回。
fn parse_csv(text: &str) -> Result<Vec<Vec<String>>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => record.push(std::mem::take(&mut field)),
            '\r' if !quoted => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            '\n' if !quoted => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            _ => field.push(c),
        }
    }
    if quoted {
        return Err(invalid("术语表 CSV 有未闭合的引号"));
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    Ok(records)
}

struct Columns {
    index: BTreeMap<String, usize>,
}

impl Columns {
    fn of(header: &[String]) -> Result<Self> {
        let mut index = BTreeMap::new();
        for (i, name) in header.iter().enumerate() {
            let name = name.trim().trim_start_matches('\u{feff}').to_owned();
            if name.is_empty() {
                continue;
            }
            if index.insert(name.clone(), i).is_some() {
                return Err(invalid(format!("术语表表头重复列：{name}")));
            }
        }
        if !index.contains_key("kind") || !index.contains_key("id") {
            return Err(invalid("术语表缺少 kind 或 id 列"));
        }
        Ok(Self { index })
    }

    fn get<'a>(&self, row: &'a [String], column: &str) -> &'a str {
        self.index
            .get(column)
            .and_then(|&i| row.get(i))
            .map(String::as_str)
            .unwrap_or("")
    }
}

impl Translations {
    /// 导出本地资料草稿：术语、人物与称谓规则各占一行，其余字段原样保存。
    pub fn export_termbase_csv(&self, project: &str) -> Result<String> {
        let draft = self
            .material_draft(project)
            .map_err(|e| invalid(e.to_string()))?
            .ok_or_else(|| invalid("尚无本地资料草稿"))?;
        let mut rows = Vec::new();
        for term in &draft.material.terms {
            let mut row = vec![String::new(); CSV_HEADER.len()];
            let mut put = |column: &str, value: String| {
                if let Some(i) = CSV_HEADER.iter().position(|c| *c == column) {
                    row[i] = value;
                }
            };
            put("kind", "term".into());
            put("id", quote(&term.term_id));
            put("source", quote(&term.source));
            put("allowed", quote(&term.allowed.join(";")));
            put("status", status_text(&term.status).into());
            put("note", quote(term.note.as_deref().unwrap_or("")));
            put("category", category_text(&term.category));
            put("scope", quote(&encode_scope(&term.scope)));
            put(
                "available_from",
                quote(
                    &term
                        .available_from
                        .as_ref()
                        .map(encode_position)
                        .unwrap_or_default(),
                ),
            );
            put(
                "evidence",
                quote(&serde_json::to_string(&term.evidence).unwrap_or_else(|_| "[]".into())),
            );
            rows.push(row);
        }
        for person in &draft.material.persons {
            let mut row = vec![String::new(); CSV_HEADER.len()];
            let mut put = |column: &str, value: String| {
                if let Some(i) = CSV_HEADER.iter().position(|c| *c == column) {
                    row[i] = value;
                }
            };
            put("kind", "person".into());
            put("id", quote(&person.person_id));
            put("name", quote(&person.name));
            put("aliases", quote(&person.aliases.join(";")));
            put("status", status_text(&person.status).into());
            put("reading", quote(person.reading.as_deref().unwrap_or("")));
            put(
                "available_from",
                quote(
                    &person
                        .available_from
                        .as_ref()
                        .map(encode_position)
                        .unwrap_or_default(),
                ),
            );
            put("terms", quote(&person.terms.join(";")));
            put("voice", quote(&person.voice.description));
            put(
                "examples",
                quote(
                    &person
                        .voice
                        .examples
                        .as_ref()
                        .map(|e| serde_json::to_string(e).unwrap_or_else(|_| "[]".into()))
                        .unwrap_or_default(),
                ),
            );
            put(
                "evidence",
                quote(&serde_json::to_string(&person.evidence).unwrap_or_else(|_| "[]".into())),
            );
            rows.push(row);
        }
        for person in &draft.material.persons {
            for rule in &person.address_rules {
                let mut row = vec![String::new(); CSV_HEADER.len()];
                let mut put = |column: &str, value: String| {
                    if let Some(i) = CSV_HEADER.iter().position(|c| *c == column) {
                        row[i] = value;
                    }
                };
                put("kind", "rule".into());
                put("owner", quote(&person.person_id));
                put("target", quote(&rule.target));
                put("speaker", quote(&encode_speaker(&rule.speaker)));
                put("term", quote(&rule.term));
                put("scope", quote(&encode_scope(&rule.scope)));
                put("note", quote(rule.note.as_deref().unwrap_or("")));
                put(
                    "evidence",
                    quote(&serde_json::to_string(&rule.evidence).unwrap_or_else(|_| "[]".into())),
                );
                rows.push(row);
            }
        }
        let mut lines = vec![CSV_HEADER.join(",")];
        lines.extend(rows.into_iter().map(|row| row.join(",")));
        Ok(lines.join("\n") + "\n")
    }

    /// 合并 CSV 到当前草稿。旧八列表头仍可读，缺的字段取安全默认值。
    pub fn import_termbase_csv(&self, project: &str, bytes: &[u8]) -> Result<MaterialReceipt> {
        let text = std::str::from_utf8(bytes).map_err(|_| invalid("术语表不是 UTF-8"))?;
        let records = parse_csv(text)?;
        let mut rows = records
            .into_iter()
            .filter(|row| row.iter().any(|cell| !cell.trim().is_empty()));
        let header = rows.next().ok_or_else(|| invalid("术语表没有表头"))?;
        if header
            .first()
            .is_none_or(|c| c.trim().trim_start_matches('\u{feff}') != "kind")
        {
            return Err(invalid("术语表缺少 kind 表头"));
        }
        let columns = Columns::of(&header)?;
        let mut terms: BTreeMap<String, Term> = BTreeMap::new();
        let mut persons: BTreeMap<String, Person> = BTreeMap::new();
        let mut rules: Vec<(String, AddressRule)> = Vec::new();
        let mut imported = 0usize;
        for (offset, row) in rows.enumerate() {
            let number = offset + 2;
            let kind = columns.get(&row, "kind").trim();
            if kind.is_empty() {
                return Err(invalid(format!("第 {number} 行缺少 kind")));
            }
            let id = columns.get(&row, "id").trim();
            if kind != "rule" && id.is_empty() {
                return Err(invalid(format!("第 {number} 行缺少 ID")));
            }
            match kind {
                "term" => {
                    let source = columns.get(&row, "source").trim();
                    let allowed = split_list(columns.get(&row, "allowed"));
                    if source.is_empty() || allowed.is_empty() {
                        return Err(invalid(format!("第 {number} 行术语缺原文或允许译法")));
                    }
                    terms.insert(
                        id.to_owned(),
                        Term {
                            term_id: id.to_owned(),
                            source: source.to_owned(),
                            category: decode_category(columns.get(&row, "category"), number)?,
                            allowed,
                            note: optional_text(columns.get(&row, "note")),
                            scope: decode_scope(columns.get(&row, "scope"), number)?,
                            available_from: decode_available_from(
                                columns.get(&row, "available_from"),
                                number,
                            )?,
                            evidence: decode_evidence(columns.get(&row, "evidence"), number)?,
                            status: decode_status(columns.get(&row, "status")),
                        },
                    );
                }
                "person" => {
                    let name = columns.get(&row, "name").trim();
                    if name.is_empty() {
                        return Err(invalid(format!("第 {number} 行人物缺少姓名")));
                    }
                    persons.insert(
                        id.to_owned(),
                        Person {
                            person_id: id.to_owned(),
                            available_from: decode_available_from(
                                columns.get(&row, "available_from"),
                                number,
                            )?,
                            name: name.to_owned(),
                            reading: optional_text(columns.get(&row, "reading")),
                            aliases: split_list(columns.get(&row, "aliases")),
                            terms: split_list(columns.get(&row, "terms")),
                            address_rules: Vec::new(),
                            voice: PersonVoice {
                                description: {
                                    let voice = columns.get(&row, "voice").trim();
                                    if voice.is_empty() {
                                        "导入".into()
                                    } else {
                                        voice.to_owned()
                                    }
                                },
                                examples: decode_examples(columns.get(&row, "examples"), number)?,
                            },
                            evidence: decode_evidence(columns.get(&row, "evidence"), number)?,
                            status: decode_status(columns.get(&row, "status")),
                        },
                    );
                }
                "rule" => {
                    let owner = columns.get(&row, "owner").trim();
                    if owner.is_empty() {
                        return Err(invalid(format!(
                            "第 {number} 行称谓规则缺少 owner（人物 ID）"
                        )));
                    }
                    for (field, value) in [
                        ("target", columns.get(&row, "target")),
                        ("term", columns.get(&row, "term")),
                    ] {
                        if value.trim().is_empty() {
                            return Err(invalid(format!("第 {number} 行称谓规则缺少 {field}")));
                        }
                    }
                    rules.push((
                        owner.to_owned(),
                        AddressRule {
                            target: columns.get(&row, "target").trim().to_owned(),
                            speaker: decode_speaker(columns.get(&row, "speaker"), number)?,
                            scope: decode_scope(columns.get(&row, "scope"), number)?,
                            term: columns.get(&row, "term").trim().to_owned(),
                            note: optional_text(columns.get(&row, "note")),
                            evidence: decode_evidence(columns.get(&row, "evidence"), number)?,
                        },
                    ));
                }
                other => return Err(invalid(format!("第 {number} 行类型未知：{other}"))),
            }
            imported += 1;
        }
        if imported == 0 && terms.is_empty() && persons.is_empty() {
            return Err(invalid("术语表没有可导入的行"));
        }
        for (owner, rule) in rules {
            let person = persons
                .get_mut(&owner)
                .ok_or_else(|| invalid(format!("称谓规则所属人物 {owner} 不在同一份术语表里")))?;
            person.address_rules.push(rule);
        }
        let existing = self
            .material_draft(project)
            .map_err(|e| invalid(e.to_string()))?;
        let mut material =
            existing
                .as_ref()
                .map(|d| d.material.clone())
                .unwrap_or(StructuredGlossary {
                    format: 2,
                    terms: Vec::new(),
                    persons: Vec::new(),
                });
        material.format = 2;
        for term in terms.into_values() {
            material.terms.retain(|t| t.term_id != term.term_id);
            material.terms.push(term);
        }
        for person in persons.into_values() {
            material.persons.retain(|p| p.person_id != person.person_id);
            material.persons.push(person);
        }
        self.save_material_draft(
            project,
            &MaterialSave {
                request_id: format!("termbase-import-{}", &super::super::sha256_hex(bytes)[..16]),
                expected_revision: existing.as_ref().map(|d| d.revision.clone()),
                material,
            },
        )
        .map_err(|e| invalid(e.to_string()))
    }
}

fn status_text(status: &GlossaryStatus) -> &'static str {
    match *status {
        GlossaryStatus::Confirmed => "confirmed",
        GlossaryStatus::Tentative => "tentative",
    }
}

fn optional_text(value: &str) -> Option<String> {
    // 备注等字段可能含有意保留的尾随制表符或换行：只在全空白时视为空。
    (!value.trim().is_empty()).then(|| value.to_owned())
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn tag_value(block: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(&close)? + start;
    Some(xml_unescape(&block[start..end]))
}

fn descrip(block: &str, kind: &str) -> Option<String> {
    let marker = format!("<descrip type=\"{kind}\">");
    let start = block.find(&marker)? + marker.len();
    let end = block[start..].find("</descrip>")? + start;
    Some(xml_unescape(&block[start..end]))
}

fn descrip_xml(kind: &str, value: &str) -> String {
    format!(
        "<descrip type=\"{}\">{}</descrip>",
        xml_escape(kind),
        xml_escape(value)
    )
}

fn evidence_json(evidence: &[GlossaryEvidence]) -> String {
    serde_json::to_string(evidence).unwrap_or_else(|_| "[]".into())
}

impl Translations {
    pub fn export_termbase_tbx(&self, project: &str) -> Result<String> {
        let draft = self
            .material_draft(project)
            .map_err(|e| invalid(e.to_string()))?
            .ok_or_else(|| invalid("尚无本地资料草稿"))?;
        let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<tbx><body>\n");
        for term in &draft.material.terms {
            out.push_str(&format!(
                "  <termEntry id=\"{}\">",
                xml_escape(&term.term_id)
            ));
            out.push_str(&descrip_xml("kind", "term"));
            out.push_str(&descrip_xml("category", &category_text(&term.category)));
            out.push_str(&descrip_xml("scope", &encode_scope(&term.scope)));
            out.push_str(&descrip_xml(
                "available_from",
                &term
                    .available_from
                    .as_ref()
                    .map(encode_position)
                    .unwrap_or_default(),
            ));
            out.push_str(&descrip_xml("evidence", &evidence_json(&term.evidence)));
            out.push_str(&descrip_xml("status", status_text(&term.status)));
            out.push_str(&descrip_xml("allowed", &term.allowed.join(";")));
            out.push_str(&descrip_xml("note", term.note.as_deref().unwrap_or("")));
            out.push_str(&format!(
                "<langSet><tig><term>{}</term></tig></langSet></termEntry>\n",
                xml_escape(&term.source)
            ));
        }
        for person in &draft.material.persons {
            out.push_str(&format!(
                "  <termEntry id=\"{}\">",
                xml_escape(&person.person_id)
            ));
            out.push_str(&descrip_xml("kind", "person"));
            out.push_str(&descrip_xml("status", status_text(&person.status)));
            out.push_str(&descrip_xml(
                "reading",
                person.reading.as_deref().unwrap_or(""),
            ));
            out.push_str(&descrip_xml(
                "available_from",
                &person
                    .available_from
                    .as_ref()
                    .map(encode_position)
                    .unwrap_or_default(),
            ));
            out.push_str(&descrip_xml("aliases", &person.aliases.join(";")));
            out.push_str(&descrip_xml("terms", &person.terms.join(";")));
            out.push_str(&descrip_xml("voice", &person.voice.description));
            out.push_str(&descrip_xml(
                "examples",
                &person
                    .voice
                    .examples
                    .as_ref()
                    .map(|e| serde_json::to_string(e).unwrap_or_else(|_| "[]".into()))
                    .unwrap_or_default(),
            ));
            out.push_str(&descrip_xml("evidence", &evidence_json(&person.evidence)));
            out.push_str(&descrip_xml(
                "address_rules",
                &serde_json::to_string(&person.address_rules).unwrap_or_else(|_| "[]".into()),
            ));
            out.push_str(&format!(
                "<langSet><tig><term>{}</term></tig></langSet></termEntry>\n",
                xml_escape(&person.name)
            ));
        }
        out.push_str("</body></tbx>\n");
        Ok(out)
    }

    /// 合并 TBX 到本地草稿；与 CSV 共用同一套字段校验与合并规则。
    pub fn import_termbase_tbx(&self, project: &str, bytes: &[u8]) -> Result<MaterialReceipt> {
        let text = std::str::from_utf8(bytes).map_err(|_| invalid("TBX 不是 UTF-8"))?;
        let mut terms = Vec::new();
        let mut persons = Vec::new();
        let mut imported = 0usize;
        for block in text.split("<termEntry").skip(1) {
            let id_start = block
                .find("id=\"")
                .ok_or_else(|| invalid("TBX 的 termEntry 缺少 id"))?
                + 4;
            let id_end = block[id_start..]
                .find('"')
                .ok_or_else(|| invalid("TBX 的 termEntry id 未闭合"))?;
            let id = block[id_start..id_start + id_end].to_owned();
            let kind = descrip(block, "kind").unwrap_or_else(|| "term".into());
            let term = tag_value(block, "term").unwrap_or_default();
            let status = decode_status(&descrip(block, "status").unwrap_or_default());
            let row = imported + 1;
            match kind.as_str() {
                "person" => {
                    if term.trim().is_empty() {
                        return Err(invalid(format!("TBX 第 {row} 个人物缺少姓名")));
                    }
                    let examples =
                        decode_examples(&descrip(block, "examples").unwrap_or_default(), row)?;
                    let rules: Vec<AddressRule> = {
                        let value = descrip(block, "address_rules").unwrap_or_default();
                        if value.trim().is_empty() {
                            Vec::new()
                        } else {
                            serde_json::from_str(&value).map_err(|e| {
                                invalid(format!("TBX 第 {row} 份称谓规则不是有效 JSON：{e}"))
                            })?
                        }
                    };
                    persons.push(Person {
                        person_id: id,
                        available_from: decode_available_from(
                            &descrip(block, "available_from").unwrap_or_default(),
                            row,
                        )?,
                        name: term,
                        reading: optional_text(&descrip(block, "reading").unwrap_or_default()),
                        aliases: split_list(&descrip(block, "aliases").unwrap_or_default()),
                        terms: split_list(&descrip(block, "terms").unwrap_or_default()),
                        address_rules: rules,
                        voice: PersonVoice {
                            description: {
                                let voice = descrip(block, "voice").unwrap_or_default();
                                if voice.trim().is_empty() {
                                    "导入".into()
                                } else {
                                    voice
                                }
                            },
                            examples,
                        },
                        evidence: decode_evidence(
                            &descrip(block, "evidence").unwrap_or_default(),
                            row,
                        )?,
                        status,
                    });
                }
                "term" => {
                    let allowed = split_list(&descrip(block, "allowed").unwrap_or_default());
                    if term.trim().is_empty() || allowed.is_empty() {
                        return Err(invalid(format!("TBX 第 {row} 条术语缺原文或允许译法")));
                    }
                    terms.push(Term {
                        term_id: id,
                        source: term,
                        category: decode_category(
                            &descrip(block, "category").unwrap_or_default(),
                            row,
                        )?,
                        allowed,
                        note: optional_text(&descrip(block, "note").unwrap_or_default()),
                        scope: decode_scope(&descrip(block, "scope").unwrap_or_default(), row)?,
                        available_from: decode_available_from(
                            &descrip(block, "available_from").unwrap_or_default(),
                            row,
                        )?,
                        evidence: decode_evidence(
                            &descrip(block, "evidence").unwrap_or_default(),
                            row,
                        )?,
                        status,
                    });
                }
                other => return Err(invalid(format!("TBX 第 {row} 条类型未知：{other}"))),
            }
            imported += 1;
        }
        if imported == 0 {
            return Err(invalid("TBX 没有可导入的 termEntry"));
        }
        let existing = self
            .material_draft(project)
            .map_err(|e| invalid(e.to_string()))?;
        let mut material =
            existing
                .as_ref()
                .map(|d| d.material.clone())
                .unwrap_or(StructuredGlossary {
                    format: 2,
                    terms: Vec::new(),
                    persons: Vec::new(),
                });
        material.format = 2;
        for term in terms {
            material.terms.retain(|t| t.term_id != term.term_id);
            material.terms.push(term);
        }
        for person in persons {
            material.persons.retain(|p| p.person_id != person.person_id);
            material.persons.push(person);
        }
        self.save_material_draft(
            project,
            &MaterialSave {
                request_id: format!("termbase-import-{}", &super::super::sha256_hex(bytes)[..16]),
                expected_revision: existing.as_ref().map(|d| d.revision.clone()),
                material,
            },
        )
        .map_err(|e| invalid(e.to_string()))
    }
}

fn category_text(category: &TermCategory) -> String {
    serde_json::to_value(category)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "common".into())
}
