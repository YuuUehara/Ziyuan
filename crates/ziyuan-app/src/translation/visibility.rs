//! Local, identity-only analysis. None of these results are approved model context.
use super::{
    Glossary, GlossaryEvidence, GlossaryPosition, GlossaryScope, GlossarySpeaker, GlossaryStatus,
    Result, invalid,
};
use crate::text_project::{Project, PromptBlock};
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParagraphAssociations {
    pub paragraph: String,
    pub persons: Vec<String>,
    pub targets: Vec<String>,
    pub speaker: Option<GlossarySpeaker>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VisibilityReport {
    pub project: String,
    pub block: String,
    pub applicable: VisibilitySelection,
    pub automatic: VisibilitySelection,
    pub diagnostics: Vec<VisibilityDiagnostic>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct VisibilitySelection {
    pub terms: Vec<VisibilityTerm>,
    pub persons: Vec<VisibilityPerson>,
    pub address_rules: Vec<VisibilityRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VisibilityTerm {
    pub term_id: String,
    pub status: GlossaryStatus,
    pub paragraphs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VisibilityPerson {
    pub person_id: String,
    pub status: GlossaryStatus,
    pub paragraphs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VisibilityRule {
    pub person_id: String,
    pub rule_index: usize,
    pub person_status: GlossaryStatus,
    pub term_status: GlossaryStatus,
    pub paragraphs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityKind {
    Term,
    Person,
    AddressRule,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct VisibilityIdentity {
    pub kind: VisibilityKind,
    /// For an address rule, the owning person's ID.
    pub id: String,
    pub rule_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityReason {
    MissingFrom,
    AfterBlockStart,
    FutureEvidence,
    InvisibleDependency,
    OutsideScope,
    UnassociatedPerson,
    UnassociatedTarget,
    UnannotatedSpeaker,
    SpeakerMismatch,
    UnmatchedConditional,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VisibilityDiagnostic {
    pub identity: VisibilityIdentity,
    pub reason: VisibilityReason,
    pub dependency: Option<VisibilityIdentity>,
    pub paragraphs: Vec<String>,
}

#[derive(Default)]
struct Diagnostics {
    entries: Vec<VisibilityDiagnostic>,
    indices: HashMap<
        (
            VisibilityIdentity,
            VisibilityReason,
            Option<VisibilityIdentity>,
        ),
        usize,
    >,
    paragraphs: Vec<HashSet<String>>,
}

impl Diagnostics {
    fn push(
        &mut self,
        identity: VisibilityIdentity,
        reason: VisibilityReason,
        dependency: Option<VisibilityIdentity>,
        paragraphs: Vec<String>,
    ) {
        let key = (identity.clone(), reason.clone(), dependency.clone());
        let index = if let Some(index) = self.indices.get(&key) {
            *index
        } else {
            let index = self.entries.len();
            self.indices.insert(key, index);
            self.entries.push(VisibilityDiagnostic {
                identity,
                reason,
                dependency,
                paragraphs: vec![],
            });
            self.paragraphs.push(HashSet::new());
            index
        };
        for paragraph in paragraphs {
            if self.paragraphs[index].insert(paragraph.clone()) {
                self.entries[index].paragraphs.push(paragraph);
            }
        }
    }
}

/// Pure local analysis; does not read provenance files, approve, or export material.
/// Targets not in `persons` are accepted as annotations but cannot match a rule.
pub fn evaluate_context_visibility(
    project: &Project,
    glossary: &Glossary,
    block: &str,
    associations: &[ParagraphAssociations],
) -> Result<VisibilityReport> {
    let Glossary::Structured(data) = glossary else {
        return Err(invalid("旧词表须人工确认并显式升级后才能作结构化判定"));
    };
    // Reuse the complete format/shape/reference preflight, including for callers
    // constructing Rust values directly. Never trust a prior JSON parse.
    Glossary::parse(&serde_json::to_vec(data)?)?;
    let order = ProjectOrder::new(project)?;
    glossary.validate_project(project)?;
    let target = project
        .chapters
        .iter()
        .flat_map(|c| &c.blocks)
        .find(|b| b.block_id == block)
        .ok_or_else(|| invalid(format!("项目 {} 没有块 {block}", project.id)))?;
    let paragraphs: Vec<_> = target.paragraphs.iter().map(|p| p.id.clone()).collect();
    let start = order.paragraphs[paragraphs[0].as_str()];
    let terms: HashMap<_, _> = data
        .terms
        .iter()
        .enumerate()
        .map(|(i, t)| (t.term_id.as_str(), i))
        .collect();
    let persons: HashMap<_, _> = data
        .persons
        .iter()
        .enumerate()
        .map(|(i, p)| (p.person_id.as_str(), i))
        .collect();
    let annotations = validate_associations(target, associations, &persons)?;
    let mut diagnostics = Diagnostics::default();
    let mut report = VisibilityReport {
        project: project.id.clone(),
        block: block.into(),
        applicable: VisibilitySelection::default(),
        automatic: VisibilitySelection::default(),
        diagnostics: vec![],
    };
    let mut term_visible = Vec::with_capacity(data.terms.len());
    for term in &data.terms {
        term_visible.push(visible(
            &order,
            start,
            &term.available_from,
            term.evidence.iter(),
            &identity(VisibilityKind::Term, &term.term_id, None),
            &paragraphs,
            &mut diagnostics,
        ));
    }
    let mut person_visible = Vec::with_capacity(data.persons.len());
    let mut dependencies = vec![Vec::new(); data.persons.len()];
    let mut reverse = vec![Vec::new(); data.persons.len()];
    for (i, person) in data.persons.iter().enumerate() {
        let id = identity(VisibilityKind::Person, &person.person_id, None);
        let mut gate = visible(
            &order,
            start,
            &person.available_from,
            person
                .evidence
                .iter()
                .chain(person.address_rules.iter().flat_map(|r| &r.evidence)),
            &id,
            &paragraphs,
            &mut diagnostics,
        );
        let mut seen_terms = HashSet::new();
        for term in person
            .terms
            .iter()
            .chain(person.address_rules.iter().map(|r| &r.term))
        {
            if seen_terms.insert(term.as_str()) && !term_visible[terms[term.as_str()]] {
                gate = false;
                diagnose(
                    &mut diagnostics,
                    &id,
                    VisibilityReason::InvisibleDependency,
                    Some(identity(VisibilityKind::Term, term, None)),
                    paragraphs.clone(),
                );
            }
        }
        let mut seen_persons = HashSet::new();
        for rule in &person.address_rules {
            let speaker = match &rule.speaker {
                GlossarySpeaker::Person { person_id } => Some(person_id),
                _ => None,
            };
            for dependency in std::iter::once(&rule.target).chain(speaker) {
                let d = persons[dependency.as_str()];
                if seen_persons.insert(d) {
                    dependencies[i].push(d);
                    reverse[d].push(i);
                }
            }
        }
        person_visible.push(gate);
    }
    // Greatest viable subset: start with all locally viable cards, then propagate
    // failures once along reverse edges. Visible cycles survive, independently of
    // glossary order; no recursive DFS or repeated full-graph scans.
    let mut queue: VecDeque<_> = person_visible
        .iter()
        .enumerate()
        .filter_map(|(i, ok)| (!ok).then_some(i))
        .collect();
    while let Some(hidden) = queue.pop_front() {
        for &dependent in &reverse[hidden] {
            if person_visible[dependent] {
                person_visible[dependent] = false;
                queue.push_back(dependent);
            }
        }
    }
    for (i, person) in data.persons.iter().enumerate() {
        let id = identity(VisibilityKind::Person, &person.person_id, None);
        for &d in &dependencies[i] {
            if !person_visible[d] {
                diagnose(
                    &mut diagnostics,
                    &id,
                    VisibilityReason::InvisibleDependency,
                    Some(identity(
                        VisibilityKind::Person,
                        &data.persons[d].person_id,
                        None,
                    )),
                    paragraphs.clone(),
                );
            }
        }
        let coverage: Vec<_> = paragraphs
            .iter()
            .filter(|p| {
                annotations
                    .get(p.as_str())
                    .is_some_and(|a| a.persons.contains(person.person_id.as_str()))
            })
            .cloned()
            .collect();
        let absent: Vec<_> = paragraphs
            .iter()
            .filter(|p| {
                !annotations
                    .get(p.as_str())
                    .is_some_and(|a| a.persons.contains(person.person_id.as_str()))
            })
            .cloned()
            .collect();
        if !absent.is_empty() {
            diagnose(
                &mut diagnostics,
                &id,
                VisibilityReason::UnassociatedPerson,
                None,
                absent,
            );
        }
        if !coverage.is_empty() {
            let item = VisibilityPerson {
                person_id: person.person_id.clone(),
                status: person.status.clone(),
                paragraphs: coverage,
            };
            report.applicable.persons.push(item.clone());
            if person_visible[i] {
                report.automatic.persons.push(item);
            }
        }
    }

    let mut conditional = vec![false; data.terms.len()];
    let mut term_applicable = vec![vec![false; paragraphs.len()]; data.terms.len()];
    let mut term_automatic = term_applicable.clone();
    for (i, person) in data.persons.iter().enumerate() {
        for (ri, rule) in person.address_rules.iter().enumerate() {
            let ti = terms[rule.term.as_str()];
            conditional[ti] = true;
            let id = identity(VisibilityKind::AddressRule, &person.person_id, Some(ri));
            let mut coverage = vec![];
            for (pi, paragraph) in paragraphs.iter().enumerate() {
                let at = order.paragraphs[paragraph.as_str()];
                let a = annotations.get(paragraph.as_str());
                let reason = if !order.contains(&rule.scope, at)
                    || !order.contains(&data.terms[ti].scope, at)
                {
                    Some(VisibilityReason::OutsideScope)
                } else if !a.is_some_and(|a| a.persons.contains(person.person_id.as_str())) {
                    Some(VisibilityReason::UnassociatedPerson)
                } else if !a.is_some_and(|a| {
                    a.targets.contains(rule.target.as_str())
                        && a.persons.contains(rule.target.as_str())
                }) {
                    Some(VisibilityReason::UnassociatedTarget)
                } else {
                    match a.and_then(|a| a.speaker) {
                        None => Some(VisibilityReason::UnannotatedSpeaker),
                        Some(speaker) if speaker != &rule.speaker => {
                            Some(VisibilityReason::SpeakerMismatch)
                        }
                        Some(_) => None,
                    }
                };
                if let Some(reason) = reason {
                    diagnose(&mut diagnostics, &id, reason, None, vec![paragraph.clone()]);
                } else {
                    coverage.push(paragraph.clone());
                    term_applicable[ti][pi] = true;
                    if person_visible[i] {
                        term_automatic[ti][pi] = true;
                    }
                }
            }
            if !coverage.is_empty() {
                let item = VisibilityRule {
                    person_id: person.person_id.clone(),
                    rule_index: ri,
                    person_status: person.status.clone(),
                    term_status: data.terms[ti].status.clone(),
                    paragraphs: coverage,
                };
                report.applicable.address_rules.push(item.clone());
                // Owner closure already includes this term, target, and specific speaker.
                if person_visible[i] {
                    report.automatic.address_rules.push(item);
                } else {
                    diagnose(
                        &mut diagnostics,
                        &id,
                        VisibilityReason::InvisibleDependency,
                        Some(identity(VisibilityKind::Person, &person.person_id, None)),
                        item.paragraphs,
                    );
                }
            }
        }
    }
    for (ti, term) in data.terms.iter().enumerate() {
        let id = identity(VisibilityKind::Term, &term.term_id, None);
        if !conditional[ti] {
            for (pi, paragraph) in paragraphs.iter().enumerate() {
                if order.contains(&term.scope, order.paragraphs[paragraph.as_str()]) {
                    term_applicable[ti][pi] = true;
                    term_automatic[ti][pi] = term_visible[ti];
                } else {
                    diagnose(
                        &mut diagnostics,
                        &id,
                        VisibilityReason::OutsideScope,
                        None,
                        vec![paragraph.clone()],
                    );
                }
            }
        }
        let coverage = selected_paragraphs(&paragraphs, &term_applicable[ti]);
        if conditional[ti] && coverage.is_empty() {
            diagnose(
                &mut diagnostics,
                &id,
                VisibilityReason::UnmatchedConditional,
                None,
                paragraphs.clone(),
            );
        }
        if !coverage.is_empty() {
            report.applicable.terms.push(VisibilityTerm {
                term_id: term.term_id.clone(),
                status: term.status.clone(),
                paragraphs: coverage,
            });
        }
        let coverage = selected_paragraphs(&paragraphs, &term_automatic[ti]);
        if !coverage.is_empty() {
            report.automatic.terms.push(VisibilityTerm {
                term_id: term.term_id.clone(),
                status: term.status.clone(),
                paragraphs: coverage,
            });
        }
    }
    report.diagnostics = diagnostics.entries;
    Ok(report)
}

fn selected_paragraphs(paragraphs: &[String], selected: &[bool]) -> Vec<String> {
    paragraphs
        .iter()
        .zip(selected)
        .filter_map(|(p, selected)| selected.then_some(p.clone()))
        .collect()
}

fn identity(kind: VisibilityKind, id: &str, rule_index: Option<usize>) -> VisibilityIdentity {
    VisibilityIdentity {
        kind,
        id: id.into(),
        rule_index,
    }
}

fn diagnose(
    diagnostics: &mut Diagnostics,
    identity: &VisibilityIdentity,
    reason: VisibilityReason,
    dependency: Option<VisibilityIdentity>,
    paragraphs: Vec<String>,
) {
    diagnostics.push(identity.clone(), reason, dependency, paragraphs);
}

fn visible<'a>(
    order: &ProjectOrder<'_>,
    start: (usize, usize),
    from: &Option<GlossaryPosition>,
    evidence: impl Iterator<Item = &'a GlossaryEvidence>,
    id: &VisibilityIdentity,
    paragraphs: &[String],
    diagnostics: &mut Diagnostics,
) -> bool {
    let mut ok = true;
    let reason = match from {
        None => Some(VisibilityReason::MissingFrom),
        Some(from) if order.position(from, false) > start => {
            Some(VisibilityReason::AfterBlockStart)
        }
        Some(_) => None,
    };
    if let Some(reason) = reason {
        ok = false;
        diagnose(diagnostics, id, reason, None, paragraphs.to_vec());
    }
    let mut future: Vec<_> = evidence
        .filter(|e| order.paragraphs[e.paragraph.as_str()] > start)
        .map(|e| e.paragraph.clone())
        .collect();
    if !future.is_empty() {
        ok = false;
        future.sort_by_key(|p| order.paragraphs[p.as_str()]);
        future.dedup();
        diagnose(
            diagnostics,
            id,
            VisibilityReason::FutureEvidence,
            None,
            future,
        );
    }
    ok
}

struct Annotation<'a> {
    persons: HashSet<&'a str>,
    targets: HashSet<&'a str>,
    speaker: Option<&'a GlossarySpeaker>,
}

fn validate_associations<'a>(
    target: &PromptBlock,
    associations: &'a [ParagraphAssociations],
    persons: &HashMap<&str, usize>,
) -> Result<HashMap<&'a str, Annotation<'a>>> {
    let paragraphs: HashSet<_> = target.paragraphs.iter().map(|p| p.id.as_str()).collect();
    let mut result = HashMap::new();
    for (i, a) in associations.iter().enumerate() {
        let at = format!("associations.{i}");
        if !paragraphs.contains(a.paragraph.as_str()) {
            return Err(invalid(format!(
                "{at}.paragraph 不属于目标块：{}",
                a.paragraph
            )));
        }
        if result.contains_key(a.paragraph.as_str()) {
            return Err(invalid(format!("{at}.paragraph 重复：{}", a.paragraph)));
        }
        let mut sets = [HashSet::new(), HashSet::new()];
        for ((values, field), set) in [(&a.persons, "persons"), (&a.targets, "targets")]
            .into_iter()
            .zip(&mut sets)
        {
            for (j, id) in values.iter().enumerate() {
                if !persons.contains_key(id.as_str()) {
                    return Err(invalid(format!("{at}.{field}.{j} 人物不存在：{id}")));
                }
                if !set.insert(id.as_str()) {
                    return Err(invalid(format!("{at}.{field}.{j} 人物重复：{id}")));
                }
            }
        }
        if let Some(GlossarySpeaker::Person { person_id }) = &a.speaker
            && !persons.contains_key(person_id.as_str())
        {
            return Err(invalid(format!(
                "{at}.speaker.person_id 人物不存在：{person_id}"
            )));
        }
        let [persons, targets] = sets;
        result.insert(
            a.paragraph.as_str(),
            Annotation {
                persons,
                targets,
                speaker: a.speaker.as_ref(),
            },
        );
    }
    Ok(result)
}

/// Actual array order only. No files, hashes, lexical ID ordering, or story chronology.
struct ProjectOrder<'a> {
    chapters: HashMap<&'a str, (usize, usize)>,
    paragraphs: HashMap<&'a str, (usize, usize)>,
}

impl<'a> ProjectOrder<'a> {
    fn new(project: &'a Project) -> Result<Self> {
        let mut chapters = HashMap::new();
        let mut paragraphs = HashMap::new();
        let mut blocks = HashSet::new();
        let mut references = HashSet::new();
        for (ci, c) in project.chapters.iter().enumerate() {
            if c.id.is_empty()
                || chapters
                    .insert(c.id.as_str(), (ci, c.paragraphs.len()))
                    .is_some()
            {
                return Err(invalid(format!("chapters.{ci}.id 为空或重复：{}", c.id)));
            }
            let mut local = HashMap::new();
            for (pi, p) in c.paragraphs.iter().enumerate() {
                if p.id.is_empty() || paragraphs.insert(p.id.as_str(), (ci, pi + 1)).is_some() {
                    return Err(invalid(format!(
                        "chapters.{ci}.paragraphs.{pi}.id 为空或重复：{}",
                        p.id
                    )));
                }
                local.insert(p.id.as_str(), (pi, p));
            }
            for (bi, b) in c.blocks.iter().enumerate() {
                let at = format!("chapters.{ci}.blocks.{bi}");
                if b.block_id.is_empty() || !blocks.insert(b.block_id.as_str()) {
                    return Err(invalid(format!("{at}.block_id 为空或重复：{}", b.block_id)));
                }
                if b.schema_version != 1 || b.paragraphs.is_empty() {
                    return Err(invalid(format!("{at} 块版本须为 1 且原段不得为空")));
                }
                let mut last = None;
                for (pi, p) in b.paragraphs.iter().enumerate() {
                    let &(rank, source) = local.get(p.id.as_str()).ok_or_else(|| {
                        invalid(format!("{at}.paragraphs.{pi} 不属于本章原段：{}", p.id))
                    })?;
                    if last.is_some_and(|last| rank <= last) || !references.insert(p.id.as_str()) {
                        return Err(invalid(format!(
                            "{at}.paragraphs.{pi} 原段重复或顺序不对：{}",
                            p.id
                        )));
                    }
                    if p.text != source.text || p.inlines != source.inlines {
                        return Err(invalid(format!(
                            "{at}.paragraphs.{pi} 与实际原段内容不符：{}",
                            p.id
                        )));
                    }
                    last = Some(rank);
                }
            }
        }
        Ok(Self {
            chapters,
            paragraphs,
        })
    }

    fn position(&self, p: &GlossaryPosition, end: bool) -> (usize, usize) {
        // All positions and ranges have already passed glossary.validate_project.
        if let Some(paragraph) = &p.paragraph {
            self.paragraphs[paragraph.as_str()]
        } else {
            let (ci, count) = self.chapters[p.chapter.as_str()];
            (ci, if end { count + 1 } else { 0 })
        }
    }

    fn contains(&self, scope: &GlossaryScope, p: (usize, usize)) -> bool {
        match scope {
            GlossaryScope::Project => true,
            GlossaryScope::Range { from, to } => {
                self.position(from, false) <= p && p <= self.position(to, true)
            }
        }
    }
}

#[cfg(test)]
mod tests;
