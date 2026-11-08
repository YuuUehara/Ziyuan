//! Read-only comparison between the local draft and the approved production glossary.
use super::super::{Glossary, Person, Result, Term, Translations, invalid};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MaterialChange {
    pub id: String,
    pub kind: &'static str,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MaterialDiff {
    pub project: String,
    pub draft_revision: String,
    pub approved_version: Option<u32>,
    pub approved_versions: Vec<u32>,
    pub legacy_production: bool,
    pub terms: Vec<MaterialChange>,
    pub persons: Vec<MaterialChange>,
}

fn fields<T: Serialize>(old: &T, new: &T) -> Vec<String> {
    let (Ok(Value::Object(old)), Ok(Value::Object(new))) =
        (serde_json::to_value(old), serde_json::to_value(new))
    else {
        return Vec::new();
    };
    let mut keys: Vec<&String> = old.keys().chain(new.keys()).collect();
    keys.sort_unstable();
    keys.dedup();
    keys.into_iter()
        .filter(|key| old.get(*key) != new.get(*key))
        .cloned()
        .collect()
}

fn diff<T: Serialize>(old: &BTreeMap<String, T>, new: &BTreeMap<String, T>) -> Vec<MaterialChange> {
    let mut out = Vec::new();
    for (id, current) in old {
        match new.get(id) {
            None => out.push(MaterialChange {
                id: id.clone(),
                kind: "removed",
                fields: Vec::new(),
            }),
            Some(approved) => {
                let fields = fields(current, approved);
                if !fields.is_empty() {
                    out.push(MaterialChange {
                        id: id.clone(),
                        kind: "changed",
                        fields,
                    });
                }
            }
        }
    }
    for id in new.keys() {
        if !old.contains_key(id) {
            out.push(MaterialChange {
                id: id.clone(),
                kind: "added",
                fields: Vec::new(),
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

impl Translations {
    /// Compare the current local draft with the currently approved context.
    /// Read-only: never saves, approves or exports anything.
    pub fn material_diff(&self, project: &str) -> Result<MaterialDiff> {
        let draft = self
            .material_draft(project)
            .map_err(|e| invalid(e.to_string()))?
            .ok_or_else(|| invalid("尚无本地资料草稿"))?;
        let snapshot = self.current_context(project)?;
        let approved_versions = self.context_versions(project)?;
        let approved_version = snapshot.as_ref().map(|s| s.version);
        let mut legacy_production = false;
        let mut old_terms: BTreeMap<String, Term> = BTreeMap::new();
        let mut old_persons: BTreeMap<String, Person> = BTreeMap::new();
        if let Some(snapshot) = &snapshot {
            match &snapshot.glossary {
                Glossary::Legacy(_) => legacy_production = true,
                Glossary::Structured(data) => {
                    old_terms = data
                        .terms
                        .iter()
                        .map(|t| (t.term_id.clone(), t.clone()))
                        .collect();
                    old_persons = data
                        .persons
                        .iter()
                        .map(|p| (p.person_id.clone(), p.clone()))
                        .collect();
                }
            }
        }
        let new_terms: BTreeMap<String, Term> = draft
            .material
            .terms
            .iter()
            .map(|t| (t.term_id.clone(), t.clone()))
            .collect();
        let new_persons: BTreeMap<String, Person> = draft
            .material
            .persons
            .iter()
            .map(|p| (p.person_id.clone(), p.clone()))
            .collect();
        Ok(MaterialDiff {
            project: project.to_owned(),
            draft_revision: draft.revision,
            approved_version,
            approved_versions,
            legacy_production,
            terms: diff(&old_terms, &new_terms),
            persons: diff(&old_persons, &new_persons),
        })
    }
}
