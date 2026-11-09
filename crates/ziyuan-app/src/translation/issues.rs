//! Append-only human decisions. Stale resolutions never count as current.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use ziyuan_storage::{JsonFile, LockScope, Workspace};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueState {
    Pending,
    Confirmed,
    Intentional,
    Resolved,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueInput {
    pub paragraph: String,
    pub related: Vec<String>,
    pub rule: String,
    pub body: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueReview {
    pub version: u64,
    pub fingerprint: String,
    pub state: IssueState,
    pub reason: String,
    #[serde(default)]
    pub recheck: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueRecord {
    pub issue_id: u64,
    pub version: u64,
    pub input: IssueInput,
    pub state: IssueState,
    pub reason: String,
    pub fingerprint: String,
    pub created: u64,
    pub anchors: Vec<ReadingParagraph>,
}
#[derive(Clone, Debug, Serialize)]
pub struct IssueView {
    pub record: IssueRecord,
    pub current: bool,
    pub fingerprint: String,
    pub anchors: Vec<ReadingParagraph>,
    pub history: Vec<IssueRecord>,
}
#[derive(Clone, Debug, Serialize)]
pub struct IssueList {
    pub fingerprint: String,
    pub issues: Vec<IssueView>,
    pub anchors: Vec<ReadingParagraph>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Book {
    sequence: u64,
    records: Vec<IssueRecord>,
}
impl Book {
    fn next(&mut self) -> Result<u64> {
        if self.records.len() >= 10000 {
            return Err(invalid("待查历史已达10000条上限"));
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| invalid("序号溢出"))?;
        Ok(self.sequence)
    }
}
fn binding(
    input: &IssueInput,
    material: &reading::Materials,
) -> Result<(String, Vec<ReadingParagraph>)> {
    if input.rule.trim().is_empty()
        || input.rule.len() > 1000
        || input.body.trim().is_empty()
        || input.body.len() > 16000
        || input.related.len() > 16
    {
        return Err(invalid("规则/问题说明须非空且在长度限制内，关联段最多16项"));
    }
    let ids: Vec<_> = std::iter::once(&input.paragraph)
        .chain(&input.related)
        .collect();
    let mut seen = BTreeSet::new();
    let mut anchors = Vec::new();
    let mut blocks = BTreeMap::new();
    for id in ids {
        if !seen.insert(id) {
            return Err(invalid("关联段重复"));
        }
        let anchor = material
            .anchor_index
            .get(id)
            .and_then(|index| material.reading.paragraphs.get(*index))
            .ok_or_else(|| invalid("关联段不属于当前项目"))?;
        blocks.insert(&anchor.block, material.block_hashes.get(&anchor.block));
        anchors.push(anchor.clone());
    }
    let fingerprint = sha256_hex(&serde_json::to_vec(&(
        &material.reading.project,
        &anchors,
        &blocks,
        &material.context_hash,
        input,
    ))?);
    Ok((fingerprint, anchors))
}
impl Translations {
    fn issues_file(&self, project: &str) -> Result<JsonFile<Book>> {
        Ok(JsonFile::new(
            self.store_dir(project)?.join("cross-chapter-issues.json"),
            1,
            32 * 1024 * 1024,
        ))
    }
    pub fn issues(&self, project: &str) -> Result<IssueList> {
        let material = self.reading_materials(project)?;
        let book = self
            .issues_file(project)?
            .load()?
            .map(|s| s.value)
            .unwrap_or_default();
        let mut latest = BTreeMap::new();
        for record in &book.records {
            latest.insert(record.issue_id, record);
        }
        let mut issues = Vec::new();
        for record in latest.values() {
            let (fingerprint, anchors) = binding(&record.input, &material)?;
            issues.push(IssueView {
                record: (*record).clone(),
                current: record.fingerprint == fingerprint,
                fingerprint,
                anchors,
                history: book
                    .records
                    .iter()
                    .filter(|r| r.issue_id == record.issue_id)
                    .cloned()
                    .collect(),
            });
        }
        if self.reading_materials(project)?.reading.fingerprint != material.reading.fingerprint {
            return Err(invalid("材料读取期间有变，请重试"));
        }
        Ok(IssueList {
            fingerprint: material.reading.fingerprint,
            issues,
            anchors: material.reading.paragraphs,
        })
    }
    pub fn add_issue(
        &self,
        project: &str,
        input: &IssueInput,
        expected: &str,
    ) -> Result<IssueRecord> {
        let workspace = Workspace::open(self.root()).map_err(|e| invalid(e.to_string()))?;
        let _guard = workspace
            .lock(LockScope::Workspace)
            .map_err(|e| invalid(e.to_string()))?;
        workspace
            .ensure_writable()
            .map_err(|e| invalid(e.to_string()))?;
        let material = self.reading_materials(project)?;
        if material.reading.fingerprint != expected {
            return Err(invalid("材料已变化，请重新读取"));
        }
        let (fingerprint, anchors) = binding(input, &material)?;
        let (_, record) = self
            .issues_file(project)?
            .update::<_, TranslationError>(|stored| {
                let mut book = stored.map(|s| s.value).unwrap_or_default();
                if let Some(record) = book
                    .records
                    .iter()
                    .find(|r| r.input == *input && r.fingerprint == fingerprint)
                {
                    let record = record.clone();
                    return Ok((book, record));
                }
                let id = book.next()?;
                let record = IssueRecord {
                    issue_id: id,
                    version: id,
                    input: input.clone(),
                    state: IssueState::Pending,
                    reason: String::new(),
                    fingerprint,
                    created: crate::time::now(),
                    anchors,
                };
                book.records.push(record.clone());
                Ok((book, record))
            })?;
        Ok(record)
    }
    pub fn review_issue(&self, project: &str, id: u64, input: &IssueReview) -> Result<IssueRecord> {
        if input.reason.trim().is_empty() || input.reason.len() > 16000 {
            return Err(invalid("须填写复核理由（最多16000字节）"));
        }
        let workspace = Workspace::open(self.root()).map_err(|e| invalid(e.to_string()))?;
        let _guard = workspace
            .lock(LockScope::Workspace)
            .map_err(|e| invalid(e.to_string()))?;
        workspace
            .ensure_writable()
            .map_err(|e| invalid(e.to_string()))?;
        let material = self.reading_materials(project)?;
        let (_, record) = self
            .issues_file(project)?
            .update::<_, TranslationError>(|stored| {
                let mut book = stored.map(|s| s.value).unwrap_or_default();
                let old = book
                    .records
                    .iter()
                    .rev()
                    .find(|r| r.issue_id == id)
                    .ok_or_else(|| invalid("待查问题不存在"))?
                    .clone();
                let (fingerprint, anchors) = binding(&old.input, &material)?;
                if old.version != input.version || fingerprint != input.fingerprint {
                    return Err(invalid("问题或材料已变化，请重新读取"));
                }
                if old.fingerprint != fingerprint
                    && (!input.recheck || input.state != IssueState::Pending)
                {
                    return Err(invalid("旧处置已过期，须先显式重新核对"));
                }
                let record = IssueRecord {
                    issue_id: id,
                    version: book.next()?,
                    input: old.input,
                    state: input.state.clone(),
                    reason: input.reason.clone(),
                    fingerprint,
                    created: crate::time::now(),
                    anchors,
                };
                book.records.push(record.clone());
                Ok((book, record))
            })?;
        Ok(record)
    }
}
