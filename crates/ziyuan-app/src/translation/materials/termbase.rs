//! CSV termbase exchange for the local draft; no production approval or network.
use super::super::{
    GlossaryScope, GlossaryStatus, Person, PersonVoice, Result, StructuredGlossary, Term,
    TermCategory, Translations, invalid,
};
use super::{MaterialReceipt, MaterialSave};
use std::collections::BTreeMap;

const HEADER: &str = "kind,id,source,name,allowed,aliases,status,note";

fn quote(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn parse_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut field)),
            _ => field.push(c),
        }
    }
    out.push(field);
    out
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

impl Translations {
    /// Export the current local draft as CSV (terms and persons).
    pub fn export_termbase_csv(&self, project: &str) -> Result<String> {
        let draft = self
            .material_draft(project)
            .map_err(|e| invalid(e.to_string()))?
            .ok_or_else(|| invalid("尚无本地资料草稿"))?;
        let mut lines = vec![HEADER.to_owned()];
        for term in &draft.material.terms {
            lines.push(
                [
                    "term".into(),
                    quote(&term.term_id),
                    quote(&term.source),
                    String::new(),
                    quote(&term.allowed.join(";")),
                    String::new(),
                    match term.status {
                        GlossaryStatus::Confirmed => "confirmed".into(),
                        GlossaryStatus::Tentative => "tentative".into(),
                    },
                    quote(term.note.as_deref().unwrap_or("")),
                ]
                .join(","),
            );
        }
        for person in &draft.material.persons {
            lines.push(
                [
                    "person".into(),
                    quote(&person.person_id),
                    String::new(),
                    quote(&person.name),
                    String::new(),
                    quote(&person.aliases.join(";")),
                    match person.status {
                        GlossaryStatus::Confirmed => "confirmed".into(),
                        GlossaryStatus::Tentative => "tentative".into(),
                    },
                    String::new(),
                ]
                .join(","),
            );
        }
        Ok(lines.join("\n") + "\n")
    }

    /// Merge rows from a CSV into the current draft. Existing IDs are replaced.
    pub fn import_termbase_csv(&self, project: &str, bytes: &[u8]) -> Result<MaterialReceipt> {
        let text = std::str::from_utf8(bytes).map_err(|_| invalid("术语表不是 UTF-8"))?;
        let mut material = match self
            .material_draft(project)
            .map_err(|e| invalid(e.to_string()))?
        {
            Some(draft) => draft.material,
            None => StructuredGlossary {
                format: 2,
                terms: Vec::new(),
                persons: Vec::new(),
            },
        };
        let mut terms: BTreeMap<String, Term> = material
            .terms
            .into_iter()
            .map(|t| (t.term_id.clone(), t))
            .collect();
        let mut persons: BTreeMap<String, Person> = material
            .persons
            .into_iter()
            .map(|p| (p.person_id.clone(), p))
            .collect();
        let mut seen_header = false;
        let mut imported = 0usize;
        for (number, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let columns = parse_line(line);
            if !seen_header && columns.first().is_some_and(|c| c == "kind") {
                seen_header = true;
                continue;
            }
            if columns.len() < 8 {
                return Err(invalid(format!("第 {} 行少于 7 列", number + 1)));
            }
            let kind = columns[0].trim();
            let id = columns[1].trim();
            if id.is_empty() {
                return Err(invalid(format!("第 {} 行缺少 ID", number + 1)));
            }
            let status = if columns[6].trim() == "confirmed" {
                GlossaryStatus::Confirmed
            } else {
                GlossaryStatus::Tentative
            };
            match kind {
                "term" => {
                    let source = columns[2].trim();
                    let allowed = split_list(&columns[4]);
                    if source.is_empty() || allowed.is_empty() {
                        return Err(invalid(format!("第 {} 行术语缺原文或允许译法", number + 1)));
                    }
                    terms.insert(
                        id.to_owned(),
                        Term {
                            term_id: id.to_owned(),
                            source: source.to_owned(),
                            category: TermCategory::Common,
                            allowed,
                            note: (!columns[7].trim().is_empty())
                                .then(|| columns[7].trim().to_owned()),
                            scope: GlossaryScope::Project,
                            available_from: None,
                            evidence: Vec::new(),
                            status,
                        },
                    );
                }
                "person" => {
                    let name = columns[3].trim();
                    if name.is_empty() {
                        return Err(invalid(format!("第 {} 行人物缺少姓名", number + 1)));
                    }
                    persons.insert(
                        id.to_owned(),
                        Person {
                            person_id: id.to_owned(),
                            available_from: None,
                            name: name.to_owned(),
                            reading: None,
                            aliases: split_list(&columns[5]),
                            terms: Vec::new(),
                            address_rules: Vec::new(),
                            voice: PersonVoice {
                                description: "导入".into(),
                                examples: None,
                            },
                            evidence: Vec::new(),
                            status,
                        },
                    );
                }
                other => return Err(invalid(format!("第 {} 行类型未知：{other}", number + 1))),
            }
            imported += 1;
        }
        if imported == 0 {
            return Err(invalid("术语表没有可导入的行"));
        }
        material.format = 2;
        material.terms = terms.into_values().collect();
        material.persons = persons.into_values().collect();
        let revision = self
            .material_draft(project)
            .map_err(|e| invalid(e.to_string()))?
            .map(|d| d.revision);
        self.save_material_draft(
            project,
            &MaterialSave {
                request_id: format!("termbase-import-{}", imported),
                expected_revision: revision,
                material,
            },
        )
        .map_err(|e| invalid(e.to_string()))
    }
}
