//! Local glossary hints share the paragraph-level applicability contract.
use super::GlossaryHint;
use crate::text_project::{Project, PromptBlock};
use crate::translation::{
    Glossary, Result,
    visibility::{ParagraphAssociations, VisibilitySelection, evaluate_context_visibility},
};
use std::collections::{HashMap, HashSet};

struct Coverage<'a> {
    terms: HashSet<(&'a str, &'a str)>,
    persons: HashSet<(&'a str, &'a str)>,
    rules: HashSet<(&'a str, usize, &'a str)>,
}

impl<'a> Coverage<'a> {
    fn new(selection: &'a VisibilitySelection) -> Self {
        Self {
            terms: selection
                .terms
                .iter()
                .flat_map(|t| {
                    t.paragraphs
                        .iter()
                        .map(move |p| (t.term_id.as_str(), p.as_str()))
                })
                .collect(),
            persons: selection
                .persons
                .iter()
                .flat_map(|t| {
                    t.paragraphs
                        .iter()
                        .map(move |p| (t.person_id.as_str(), p.as_str()))
                })
                .collect(),
            rules: selection
                .address_rules
                .iter()
                .flat_map(|r| {
                    r.paragraphs
                        .iter()
                        .map(move |p| (r.person_id.as_str(), r.rule_index, p.as_str()))
                })
                .collect(),
        }
    }
}

fn message(kind: &str, visible: bool) -> String {
    let suffix = if visible {
        "仅字面提示，仍须人工核对"
    } else {
        "未通过块首可见性门槛，仅供本地回顾，不表示可自动选入模型请求"
    };
    format!("第二版{kind}：原文命中而译文未见允许译法；已按本段适用条件筛选；{suffix}")
}

pub(super) fn glossary_hints(
    project: &Project,
    glossary: &Glossary,
    source: &PromptBlock,
    target: &PromptBlock,
    associations: &[ParagraphAssociations],
) -> Result<Vec<GlossaryHint>> {
    let mut hints = Vec::new();
    let Glossary::Structured(data) = glossary else {
        let Glossary::Legacy(entries) = glossary else {
            unreachable!()
        };
        for (source, target) in source.paragraphs.iter().zip(&target.paragraphs) {
            for entry in entries {
                if source.text.contains(&entry.source)
                    && !entry.allowed.iter().any(|a| target.text.contains(a))
                {
                    hints.push(GlossaryHint {
                        paragraph: source.id.clone(),
                        source: entry.source.clone(),
                        allowed: entry.allowed.clone(),
                        scope: entry.scope.clone(),
                        note: entry.note.clone(),
                        message: if entry.scope.is_some() {
                            "原文命中词条而译文未见允许译法；作用范围为自由文本，先人工核对是否适用"
                        } else {
                            "原文命中词条而译文未见允许译法；仅字面提示，不是语义错译判决"
                        }
                        .into(),
                    });
                }
            }
        }
        return Ok(hints);
    };
    // Applicability permits local retrospective checks. Automatic visibility is a
    // separate, stricter gate; never substitute local hints for request selection.
    let report = evaluate_context_visibility(project, glossary, &source.block_id, associations)?;
    let applicable = Coverage::new(&report.applicable);
    let automatic = Coverage::new(&report.automatic);
    let terms: HashMap<_, _> = data.terms.iter().map(|t| (t.term_id.as_str(), t)).collect();
    for (source, target) in source.paragraphs.iter().zip(&target.paragraphs) {
        let paragraph = source.id.as_str();
        for term in &data.terms {
            let key = (term.term_id.as_str(), paragraph);
            if applicable.terms.contains(&key)
                && source.text.contains(&term.source)
                && !term.allowed.iter().any(|a| target.text.contains(a))
            {
                hints.push(GlossaryHint {
                    paragraph: source.id.clone(),
                    source: term.source.clone(),
                    allowed: term.allowed.clone(),
                    scope: None,
                    note: term.note.clone(),
                    message: message("术语", automatic.terms.contains(&key)),
                });
            }
        }
        for person in &data.persons {
            let key = (person.person_id.as_str(), paragraph);
            if !applicable.persons.contains(&key) {
                continue;
            }
            let names: Vec<_> = std::iter::once(&person.name)
                .chain(&person.aliases)
                .collect();
            let mut allowed: Vec<String> = names.iter().map(|s| (*s).clone()).collect();
            for id in &person.terms {
                if applicable.terms.contains(&(id.as_str(), paragraph))
                    && let Some(term) = terms.get(id.as_str())
                    && names.iter().any(|name| {
                        name.as_str() == term.source && source.text.contains(name.as_str())
                    })
                {
                    for spelling in &term.allowed {
                        if !allowed.contains(spelling) {
                            allowed.push(spelling.clone());
                        }
                    }
                }
            }
            if names.iter().any(|name| source.text.contains(name.as_str()))
                && !allowed.iter().any(|name| target.text.contains(name))
            {
                hints.push(GlossaryHint {
                    paragraph: source.id.clone(),
                    source: person.name.clone(),
                    allowed,
                    scope: None,
                    note: person.reading.clone().filter(|s| !s.is_empty()),
                    message: message("人物", automatic.persons.contains(&key)),
                });
            }
            for (index, rule) in person.address_rules.iter().enumerate() {
                let key = (person.person_id.as_str(), index, paragraph);
                if !applicable.rules.contains(&key) {
                    continue;
                }
                let Some(term) = terms.get(rule.term.as_str()) else {
                    continue;
                };
                if source.text.contains(&term.source)
                    && !term.allowed.iter().any(|a| target.text.contains(a))
                {
                    hints.push(GlossaryHint {
                        paragraph: source.id.clone(),
                        source: term.source.clone(),
                        allowed: term.allowed.clone(),
                        scope: None,
                        note: rule.note.clone(),
                        message: message("称谓", automatic.rules.contains(&key)),
                    });
                }
            }
        }
    }
    Ok(hints)
}
