//! Typed v2 glossary: parsing does not enable approval or legacy consumption.
use super::context::{MAX_ALLOWED, MAX_ENTRIES, MAX_NOTE, MAX_TERM, check_text, check_text_limits};
use super::strict_json::parse_strict_json;
use super::{GlossaryEntry, Result, invalid};
use crate::text_project::Project;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Glossary {
    Legacy(Vec<GlossaryEntry>),
    Structured(StructuredGlossary),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredGlossary {
    pub format: u32,
    pub terms: Vec<Term>,
    pub persons: Vec<Person>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Term {
    pub term_id: String,
    pub source: String,
    pub category: TermCategory,
    pub allowed: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub scope: GlossaryScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_from: Option<GlossaryPosition>,
    pub evidence: Vec<GlossaryEvidence>,
    pub status: GlossaryStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TermCategory {
    PersonName,
    Place,
    Organization,
    Title,
    Culture,
    Common,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GlossaryStatus {
    Tentative,
    Confirmed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GlossaryStance {
    ProjectFact,
    CharacterBelief,
    Narration,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Person {
    pub person_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_from: Option<GlossaryPosition>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reading: Option<String>,
    pub aliases: Vec<String>,
    pub terms: Vec<String>,
    pub address_rules: Vec<AddressRule>,
    pub voice: PersonVoice,
    pub evidence: Vec<GlossaryEvidence>,
    pub status: GlossaryStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonVoice {
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub examples: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddressRule {
    pub target: String,
    pub speaker: GlossarySpeaker,
    pub scope: GlossaryScope,
    pub term: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub evidence: Vec<GlossaryEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GlossarySpeaker {
    Person { person_id: String },
    Narration,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GlossaryScope {
    Project,
    Range {
        from: GlossaryPosition,
        to: GlossaryPosition,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlossaryPosition {
    pub chapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paragraph: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlossaryEvidence {
    pub paragraph: String,
    pub quote: String,
    pub explanation: String,
    pub stance: GlossaryStance,
    pub status: GlossaryStatus,
}

impl Glossary {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let value = parse_strict_json(bytes, "glossary.json", None)?;
        if value.is_array() {
            return super::context::parse_legacy_glossary(bytes).map(Self::Legacy);
        }
        if !value.is_object() {
            return Err(invalid("glossary.json 格式不对：须为数组或对象"));
        }
        check_format(&value)?;
        shape(&value, Node::Root, "glossary.json", ShapeMode::Strict)?;
        let structured: StructuredGlossary = serde_json::from_value(value)
            .map_err(|e| invalid(format!("glossary.json 格式不对：{e}")))?;
        structured.validate_references()?;
        Ok(Self::Structured(structured))
    }

    /// Validate against actual project order and source paragraphs, never ID sorting.
    pub fn validate_project(&self, project: &Project) -> Result<()> {
        let Self::Structured(data) = self else {
            return Ok(());
        };
        let index = ProjectIndex::new(project);
        for (i, term) in data.terms.iter().enumerate() {
            let at = format!("terms.{i}");
            index.scope(&term.scope, &format!("{at}.scope"))?;
            if let Some(position) = &term.available_from {
                index.position(position, false, &format!("{at}.available_from"))?;
            }
            index.evidence(&term.evidence, &format!("{at}.evidence"))?;
        }
        for (i, person) in data.persons.iter().enumerate() {
            let at = format!("persons.{i}");
            if let Some(position) = &person.available_from {
                index.position(position, false, &format!("{at}.available_from"))?;
            }
            index.evidence(&person.evidence, &format!("{at}.evidence"))?;
            for (j, rule) in person.address_rules.iter().enumerate() {
                let at = format!("{at}.address_rules.{j}");
                index.scope(&rule.scope, &format!("{at}.scope"))?;
                index.evidence(&rule.evidence, &format!("{at}.evidence"))?;
            }
        }
        Ok(())
    }

    pub fn into_legacy(self) -> Result<Vec<GlossaryEntry>> {
        match self {
            Self::Legacy(entries) => Ok(entries),
            Self::Structured(_) => Err(invalid("人物／术语资料尚未开放")),
        }
    }
}

impl StructuredGlossary {
    /// Parse an unfinished v2 material, without references or project validation.
    #[allow(dead_code)] // Used by the local-materials boundary in the next task.
    pub(crate) fn parse_editable(bytes: &[u8]) -> Result<Self> {
        let value = parse_strict_json(bytes, "glossary.json", Some(32))?;
        check_format(&value)?;
        shape(&value, Node::Root, "glossary.json", ShapeMode::Editable)?;
        serde_json::from_value(value).map_err(|e| invalid(format!("glossary.json 格式不对：{e}")))
    }

    /// Apply the same save boundary to directly constructed Rust values.
    #[allow(dead_code)] // Used by the local-materials boundary in the next task.
    pub(crate) fn validate_editable_shape(&self) -> Result<()> {
        let value = serde_json::to_value(self)?;
        check_format(&value)?;
        shape(&value, Node::Root, "glossary.json", ShapeMode::Editable)
    }

    fn validate_references(&self) -> Result<()> {
        fn ids<'a>(items: impl Iterator<Item = &'a str>, at: &str) -> Result<HashSet<&'a str>> {
            let mut ids = HashSet::new();
            for (i, id) in items.enumerate() {
                if !ids.insert(id) {
                    return Err(invalid(format!("{at}.{i} ID 重复：{id}")));
                }
            }
            Ok(ids)
        }
        fn reference(ids: &HashSet<&str>, id: &str, at: &str) -> Result<()> {
            if ids.contains(id) {
                Ok(())
            } else {
                Err(invalid(format!("{at} 引用不存在：{id}")))
            }
        }
        let terms = ids(self.terms.iter().map(|t| t.term_id.as_str()), "terms")?;
        let persons = ids(self.persons.iter().map(|p| p.person_id.as_str()), "persons")?;
        for (i, person) in self.persons.iter().enumerate() {
            for (j, id) in person.terms.iter().enumerate() {
                reference(&terms, id, &format!("persons.{i}.terms.{j}"))?;
            }
            for (j, rule) in person.address_rules.iter().enumerate() {
                let at = format!("persons.{i}.address_rules.{j}");
                reference(&persons, &rule.target, &format!("{at}.target"))?;
                reference(&terms, &rule.term, &format!("{at}.term"))?;
                if let GlossarySpeaker::Person { person_id } = &rule.speaker {
                    reference(&persons, person_id, &format!("{at}.speaker.person_id"))?;
                }
            }
        }
        Ok(())
    }
}

fn check_format(value: &Value) -> Result<()> {
    match value.get("format") {
        Some(Value::Number(n)) if n.as_u64() == Some(2) => {}
        Some(Value::Number(n)) if n.as_f64().is_some_and(|v| v > 2.0) => {
            return Err(invalid("glossary.json.format 须升级程序才能读取"));
        }
        _ => return Err(invalid("glossary.json.format 格式不对：只接受整数 2")),
    }
    Ok(())
}

/// Editable accepts unfinished semantics, never weaker resource/type bounds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ShapeMode {
    Strict,
    Editable,
}

/// Preflight provides precise Chinese field/index errors before typed deserialization.
#[derive(Clone, Copy)]
enum Node {
    Root,
    Term,
    Person,
    Rule,
    Voice,
    Scope,
    Position,
    Evidence,
    Speaker,
}

fn object<'a>(
    v: &'a Value,
    at: &str,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a serde_json::Map<String, Value>> {
    let obj = v
        .as_object()
        .ok_or_else(|| invalid(format!("{at} 格式不对：须为对象")))?;
    for field in obj.keys() {
        if !required.contains(&field.as_str()) && !optional.contains(&field.as_str()) {
            return Err(invalid(format!("{at}.{field} 为未知字段")));
        }
    }
    for field in required {
        if !obj.contains_key(*field) {
            return Err(invalid(format!("{at}.{field} 缺少必填字段")));
        }
    }
    Ok(obj)
}

fn text(v: &Value, at: &str, max: usize, mode: ShapeMode) -> Result<()> {
    let s = v
        .as_str()
        .ok_or_else(|| invalid(format!("{at} 须为文本")))?;
    match mode {
        ShapeMode::Strict => check_text(at, s, max),
        ShapeMode::Editable => check_text_limits(at, s, max),
    }
}

fn id(v: &Value, at: &str, mode: ShapeMode) -> Result<()> {
    let s = v
        .as_str()
        .ok_or_else(|| invalid(format!("{at} ID 须为文本")))?;
    if mode == ShapeMode::Editable {
        if s.len() > 64 {
            return Err(invalid(format!("{at} ID 至多 64 UTF-8 字节")));
        }
        return check_text_limits(at, s, 64);
    }
    if s.is_empty()
        || s.len() > 64
        || !s
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(invalid(format!(
            "{at} ID 须为 1–64 位 ASCII 字母数字、- 或 _"
        )));
    }
    Ok(())
}

fn choice(v: &Value, at: &str, choices: &[&str]) -> Result<()> {
    if v.as_str().is_some_and(|s| choices.contains(&s)) {
        Ok(())
    } else {
        Err(invalid(format!(
            "{at} 格式不对：须为 {}",
            choices.join("／")
        )))
    }
}

fn array(
    v: &Value,
    at: &str,
    max: usize,
    mode: ShapeMode,
    mut item: impl FnMut(&Value, &str, ShapeMode) -> Result<()>,
) -> Result<()> {
    let items = v
        .as_array()
        .ok_or_else(|| invalid(format!("{at} 须为数组")))?;
    if items.len() > max {
        return Err(invalid(format!("{at} 至多 {max} 条")));
    }
    for (i, value) in items.iter().enumerate() {
        item(value, &format!("{at}.{i}"), mode)?;
    }
    Ok(())
}

fn shape(v: &Value, node: Node, at: &str, mode: ShapeMode) -> Result<()> {
    use Node::*;
    let (required, optional): (&[&str], &[&str]) = match node {
        Root => (&["format", "terms", "persons"], &[]),
        Term => (
            &[
                "term_id", "source", "category", "allowed", "scope", "evidence", "status",
            ],
            &["note", "available_from"],
        ),
        Person => (
            &[
                "person_id",
                "name",
                "aliases",
                "terms",
                "address_rules",
                "voice",
                "evidence",
                "status",
            ],
            &["reading", "available_from"],
        ),
        Rule => (
            &["target", "speaker", "scope", "term", "evidence"],
            &["note"],
        ),
        Voice => (&["description"], &["examples"]),
        Position => (&["chapter"], &["paragraph"]),
        Evidence => (
            &["paragraph", "quote", "explanation", "stance", "status"],
            &[],
        ),
        Scope | Speaker => {
            let obj = v
                .as_object()
                .ok_or_else(|| invalid(format!("{at} 须为对象")))?;
            let kind = obj
                .get("kind")
                .ok_or_else(|| invalid(format!("{at}.kind 缺少必填字段")))?;
            if matches!(node, Scope) {
                choice(kind, &format!("{at}.kind"), &["project", "range"])?;
                if kind == "range" {
                    (&["kind", "from", "to"], &[])
                } else {
                    (&["kind"], &[])
                }
            } else {
                choice(
                    kind,
                    &format!("{at}.kind"),
                    &["person", "narration", "unknown"],
                )?;
                if kind == "person" {
                    (&["kind", "person_id"], &[])
                } else {
                    (&["kind"], &[])
                }
            }
        }
    };
    let obj = object(v, at, required, optional)?;
    for (key, value) in obj {
        let path = format!("{at}.{key}");
        // Optional fields may be absent, but if present must have the declared shape.
        match key.as_str() {
            "format" | "kind" => {}
            "term_id" | "person_id" | "target" | "term" | "chapter" | "paragraph" => {
                id(value, &path, mode)?
            }
            "source" | "name" | "reading" | "quote" => text(value, &path, MAX_TERM, mode)?,
            "note" | "description" | "explanation" => text(value, &path, MAX_NOTE, mode)?,
            "category" => choice(
                value,
                &path,
                &[
                    "person_name",
                    "place",
                    "organization",
                    "title",
                    "culture",
                    "common",
                ],
            )?,
            "status" => choice(value, &path, &["tentative", "confirmed"])?,
            "stance" => choice(
                value,
                &path,
                &["project_fact", "character_belief", "narration"],
            )?,
            "allowed" | "aliases" | "examples" => {
                array(value, &path, MAX_ALLOWED, mode, |v, p, m| {
                    text(v, p, MAX_TERM, m)
                })?;
                if mode == ShapeMode::Strict
                    && key == "allowed"
                    && value.as_array().is_some_and(Vec::is_empty)
                {
                    return Err(invalid(format!("{path} 须为 1–{MAX_ALLOWED} 种")));
                }
            }
            "terms" if matches!(node, Person) => array(value, &path, MAX_ENTRIES, mode, id)?,
            "terms" => array(value, &path, MAX_ENTRIES, mode, |v, p, m| {
                shape(v, Term, p, m)
            })?,
            "persons" => array(value, &path, MAX_ENTRIES, mode, |v, p, m| {
                shape(v, Person, p, m)
            })?,
            "address_rules" => array(value, &path, MAX_ENTRIES, mode, |v, p, m| {
                shape(v, Rule, p, m)
            })?,
            "evidence" => array(value, &path, MAX_ENTRIES, mode, |v, p, m| {
                shape(v, Evidence, p, m)
            })?,
            "scope" => shape(value, Scope, &path, mode)?,
            "available_from" | "from" | "to" => shape(value, Position, &path, mode)?,
            "speaker" => shape(value, Speaker, &path, mode)?,
            "voice" => shape(value, Voice, &path, mode)?,
            _ => unreachable!("object fields were checked"),
        }
    }
    Ok(())
}

struct ProjectIndex<'a> {
    chapters: HashMap<&'a str, (usize, usize)>,
    paragraphs: HashMap<&'a str, (usize, usize, &'a str)>,
}

impl<'a> ProjectIndex<'a> {
    fn new(project: &'a Project) -> Self {
        let mut chapters = HashMap::new();
        let mut paragraphs = HashMap::new();
        for (ci, chapter) in project.chapters.iter().enumerate() {
            chapters.insert(chapter.id.as_str(), (ci, chapter.paragraphs.len()));
            for (pi, paragraph) in chapter.paragraphs.iter().enumerate() {
                paragraphs.insert(paragraph.id.as_str(), (ci, pi + 1, paragraph.text.as_str()));
            }
        }
        Self {
            chapters,
            paragraphs,
        }
    }

    fn position(&self, position: &GlossaryPosition, end: bool, at: &str) -> Result<(usize, usize)> {
        let &(ci, count) = self
            .chapters
            .get(position.chapter.as_str())
            .ok_or_else(|| invalid(format!("{at}.chapter 不属于本项目：{}", position.chapter)))?;
        if let Some(id) = &position.paragraph {
            let &(actual_chapter, pi, _) = self
                .paragraphs
                .get(id.as_str())
                .ok_or_else(|| invalid(format!("{at}.paragraph 不属于本项目：{id}")))?;
            if actual_chapter != ci {
                return Err(invalid(format!("{at}.paragraph 不属于所指定的章：{id}")));
            }
            Ok((ci, pi))
        } else {
            // Whole-chapter lower/upper bounds enclose all its paragraphs.
            Ok((ci, if end { count + 1 } else { 0 }))
        }
    }

    fn scope(&self, scope: &GlossaryScope, at: &str) -> Result<()> {
        if let GlossaryScope::Range { from, to } = scope {
            let from = self.position(from, false, &format!("{at}.from"))?;
            let to = self.position(to, true, &format!("{at}.to"))?;
            if from > to {
                return Err(invalid(format!("{at} 区间倒置：from 不得晚于 to")));
            }
        }
        Ok(())
    }

    fn evidence(&self, evidence: &[GlossaryEvidence], at: &str) -> Result<()> {
        for (i, evidence) in evidence.iter().enumerate() {
            let &(_, _, source) = self
                .paragraphs
                .get(evidence.paragraph.as_str())
                .ok_or_else(|| {
                    invalid(format!(
                        "{at}.{i}.paragraph 不属于本项目：{}",
                        evidence.paragraph
                    ))
                })?;
            if evidence.quote.is_empty() || !source.contains(&evidence.quote) {
                return Err(invalid(format!("{at}.{i}.quote 须为所引段原文的非空子串")));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
