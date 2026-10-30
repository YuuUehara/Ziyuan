//! Structural checks only. Passing says nothing about translation fidelity or quality.
use super::{PromptBlock, Result, TextProjects, invalid};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    InvalidJson,
    NewerVersion,
    UnsupportedVersion,
    WrongBlock,
    Missing,
    Duplicate,
    OutOfOrder,
    Unknown,
    EmptyText,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ValidationIssue {
    pub kind: IssueKind,
    pub chapter_id: String,
    pub block_id: String,
    pub paragraph_id: Option<String>,
    /// One-based position in the candidate, when applicable.
    pub position: Option<usize>,
    pub message: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ValidationReport {
    pub project_id: String,
    pub chapter_id: String,
    pub block_id: String,
    pub expected_ids: Vec<String>,
    /// Structural validity only; never a translation-quality verdict.
    pub valid: bool,
    pub issues: Vec<ValidationIssue>,
}
impl ValidationReport {
    fn issue(
        &mut self,
        kind: IssueKind,
        id: Option<&str>,
        position: Option<usize>,
        message: impl Into<String>,
    ) {
        self.valid = false;
        self.issues.push(ValidationIssue {
            kind,
            chapter_id: self.chapter_id.clone(),
            block_id: self.block_id.clone(),
            paragraph_id: id.map(str::to_owned),
            position,
            message: message.into(),
        });
    }
}
#[derive(Deserialize)]
struct Header {
    schema_version: u64,
}

impl TextProjects {
    /// Read-only: validates the stored originals first, then checks candidate JSON bytes.
    /// Does not store/copy candidate content, call models, apply files or modify the manifest.
    /// Project/source failures are `Err`; candidate failures are a serializable report.
    pub fn check(&self, id: &str, block_id: &str, bytes: &[u8]) -> Result<ValidationReport> {
        let project = self.show(id)?;
        let (chapter, expected) = project
            .chapters
            .iter()
            .find_map(|chapter| {
                chapter
                    .blocks
                    .iter()
                    .find(|block| block.block_id == block_id)
                    .map(|block| (chapter, block))
            })
            .ok_or_else(|| invalid(format!("未知提示块: {block_id}")))?;
        let mut report = ValidationReport {
            project_id: id.to_owned(),
            chapter_id: chapter.id.clone(),
            block_id: block_id.to_owned(),
            expected_ids: expected.paragraphs.iter().map(|p| p.id.clone()).collect(),
            valid: true,
            issues: Vec::new(),
        };
        let header: Header = match serde_json::from_slice(bytes) {
            Ok(header) => header,
            Err(e) => {
                report.issue(IssueKind::InvalidJson, None, None, e.to_string());
                return Ok(report);
            }
        };
        if header.schema_version != 1 {
            let kind = if header.schema_version > 1 {
                IssueKind::NewerVersion
            } else {
                IssueKind::UnsupportedVersion
            };
            report.issue(
                kind,
                None,
                None,
                format!(
                    "候选格式 v{}，仅支持 v1；更新版本请升级",
                    header.schema_version
                ),
            );
            return Ok(report);
        }
        let candidate: PromptBlock = match serde_json::from_slice(bytes) {
            Ok(candidate) => candidate,
            Err(e) => {
                report.issue(IssueKind::InvalidJson, None, None, e.to_string());
                return Ok(report);
            }
        };
        if candidate.block_id != block_id {
            report.issue(
                IssueKind::WrongBlock,
                None,
                None,
                format!("预期 {block_id}，收到 {}", candidate.block_id),
            );
        }
        validate_ids(expected, &candidate, &mut report);
        Ok(report)
    }
}

fn validate_ids(expected: &PromptBlock, candidate: &PromptBlock, report: &mut ValidationReport) {
    let ranks: BTreeMap<_, _> = expected
        .paragraphs
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id.as_str(), i))
        .collect();
    let present: BTreeSet<_> = candidate.paragraphs.iter().map(|p| p.id.as_str()).collect();
    // Missing IDs follow source order; all other diagnostics follow candidate order.
    for paragraph in &expected.paragraphs {
        if !present.contains(paragraph.id.as_str()) {
            report.issue(
                IssueKind::Missing,
                Some(&paragraph.id),
                None,
                "缺少原文段落 ID",
            );
        }
    }
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    let mut highest = None;
    for (i, paragraph) in candidate.paragraphs.iter().enumerate() {
        let id = paragraph.id.as_str();
        let repeated = !seen.insert(id);
        if repeated && duplicates.insert(id) {
            report.issue(IssueKind::Duplicate, Some(id), Some(i + 1), "重复段落 ID");
        }
        match ranks.get(id).copied() {
            None => report.issue(
                IssueKind::Unknown,
                Some(id),
                Some(i + 1),
                "不属于此提示块的段落 ID",
            ),
            Some(rank) if !repeated => {
                if highest.is_some_and(|highest| rank < highest) {
                    report.issue(
                        IssueKind::OutOfOrder,
                        Some(id),
                        Some(i + 1),
                        "段落顺序与原文不符",
                    );
                }
                highest = Some(highest.map_or(rank, |highest: usize| highest.max(rank)));
            }
            Some(_) => {}
        }
        if paragraph.text.trim().is_empty() {
            report.issue(
                IssueKind::EmptyText,
                Some(id),
                Some(i + 1),
                "候选段落文本不能为空或仅有空白",
            );
        }
    }
}
