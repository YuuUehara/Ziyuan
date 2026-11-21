//! Operator-owned lexical evidence and semantic checklists; never model input or signoff gates.
mod hints;

use super::{Result, Translations, find_block, invalid, sha256_hex};
use crate::text_project::PromptBlock;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use ziyuan_governance::{ChangeState, Court, TaskStatus};
use ziyuan_storage::JsonFile;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStatus {
    Tentative,
    Confirmed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceInput {
    pub paragraph: String,
    pub quote: String,
    pub source: String,
    pub locator: String,
    pub explanation: String,
    pub context: String,
    pub status: EvidenceStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LexicalEvidence {
    pub id: u64,
    pub block: String,
    pub details: EvidenceInput,
    pub created: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticCategory {
    SubjectSpeaker,
    AgencyNegation,
    Quantity,
    Timeline,
    NamesRelations,
    HumorAdditions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Accepted,
    Rejected,
    Pending,
    NotApplicable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    Unchecked,
    Resolved,
    Unresolved,
    NotApplicable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdviceLink {
    pub task: u64,
    pub quote: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticCheck {
    pub category: SemanticCategory,
    pub disposition: Disposition,
    pub resolution: Resolution,
    pub note: String,
    pub advice: Vec<AdviceLink>,
    pub evidence: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QaIssue {
    pub paragraph: String,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QaReport {
    pub project: String,
    pub block: String,
    pub paragraphs: usize,
    pub issues: Vec<QaIssue>,
}

/// 整书质检总览中的单块摘要（只读、确定性，不代表语义正确）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProjectQaBlockSummary {
    pub block: String,
    pub chapter_id: String,
    pub chapter_title: String,
    pub state: Option<String>,
    pub state_label: Option<String>,
    pub paragraphs: usize,
    pub has_translation: bool,
    pub issue_count: usize,
}

/// 整书质检总览中的单条确定性问题（含原文／译文短摘录与精确定位）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProjectQaIssue {
    pub block: String,
    pub chapter_id: String,
    pub chapter_title: String,
    pub paragraph: String,
    pub code: &'static str,
    pub message: String,
    pub source_excerpt: String,
    pub target_excerpt: String,
}

/// 整书质检总览的块级汇总统计。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ProjectQaSummary {
    pub total_blocks: usize,
    pub translated_blocks: usize,
    pub untranslated_blocks: usize,
    pub finalized_blocks: usize,
    pub checked_blocks: usize,
    pub clean_blocks: usize,
    pub blocks_with_issues: usize,
    pub total_issues: usize,
}

/// 整书级批量确定性 QA 报告（纯只读，零落盘，不调用模型）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProjectQaReport {
    pub project: String,
    pub founded: bool,
    pub summary: ProjectQaSummary,
    pub by_code: BTreeMap<String, usize>,
    pub blocks: Vec<ProjectQaBlockSummary>,
    pub issues: Vec<ProjectQaIssue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationInput {
    pub paragraph: String,
    pub fingerprint: String,
    pub checks: Vec<SemanticCheck>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewAdvice {
    pub task: u64,
    pub kind: ziyuan_core::TranslationKind,
    pub writer_task: Option<u64>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationSnapshot {
    pub source_hash: String,
    pub translation_hash: Option<String>,
    pub context_hash: String,
    pub zhupi_hash: String,
    pub writer_task: Option<u64>,
    pub reviews: Vec<ReviewAdvice>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationRecord {
    pub id: u64,
    pub block: String,
    pub paragraph: String,
    pub checks: Vec<SemanticCheck>,
    pub snapshot: VerificationSnapshot,
    pub source: String,
    pub translation: String,
    pub zhupi: Option<String>,
    pub previous: Option<u64>,
    pub created: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TranslationChange {
    pub before: String,
    pub after: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VerificationRecordView {
    pub record: VerificationRecord,
    pub current: bool,
    pub reason: Option<String>,
    pub change: Option<TranslationChange>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ParagraphVerification {
    pub paragraph: String,
    pub latest: Option<u64>,
    pub current: bool,
    pub opinions_handled: bool,
    pub operator_reviewed: bool,
    pub unresolved: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GlossaryHint {
    pub paragraph: String,
    pub source: String,
    pub allowed: Vec<String>,
    pub scope: Option<String>,
    pub note: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct VerificationView {
    pub project: String,
    pub block: String,
    pub fingerprint: String,
    pub source: crate::text_project::PromptBlock,
    pub translation: Option<crate::text_project::PromptBlock>,
    pub structure: Option<crate::text_project::ValidationReport>,
    pub writer_task: Option<u64>,
    pub reviews: Vec<ReviewAdvice>,
    pub zhupi: super::BlockZhupi,
    pub evidence: Vec<LexicalEvidence>,
    pub records: Vec<VerificationRecordView>,
    pub hints: Vec<GlossaryHint>,
    pub paragraphs: Vec<ParagraphVerification>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerificationBook {
    sequence: u64,
    evidence: Vec<LexicalEvidence>,
    records: Vec<VerificationRecord>,
}

impl VerificationBook {
    fn next(&mut self) -> Result<u64> {
        if self.evidence.len() + self.records.len() >= 10000 {
            return Err(invalid("复核记录已达 10000 条上限；不会自动清理历史"));
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| invalid("复核序号已满"))?;
        Ok(self.sequence)
    }
}

fn field(label: &str, text: &str, limit: usize, required: bool) -> Result<()> {
    if (required && text.trim().is_empty())
        || text.chars().count() > limit
        || text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(invalid(format!(
            "{label}须{}且不超过 {limit} 字，不含控制字符",
            if required { "非空" } else { "为文本" }
        )));
    }
    Ok(())
}

fn hash<T: Serialize>(value: &T) -> Result<String> {
    Ok(sha256_hex(&serde_json::to_vec(value)?))
}

fn advice(court: Option<&Court>, project: &str, block: &str) -> Vec<ReviewAdvice> {
    let Some(court) = court else {
        return Vec::new();
    };
    court
        .translation_tasks(project, block)
        .filter_map(|task| {
            let link = task.translation.as_ref()?;
            if link.kind.writes() {
                return None;
            }
            let TaskStatus::Completed { summary, .. } = &task.status else {
                return None;
            };
            let writer_task = court
                .translation_tasks(project, block)
                .filter(|w| {
                    w.edict.id < task.edict.id
                        && w.translation.as_ref().is_some_and(|l| l.kind.writes())
                })
                .filter(|w| {
                    w.changes
                        .as_ref()
                        .is_some_and(|c| c.state == ChangeState::Applied)
                })
                .last()
                .map(|w| w.edict.id.0);
            Some(ReviewAdvice {
                task: task.edict.id.0,
                kind: link.kind,
                writer_task,
                text: summary.to_string(),
            })
        })
        .collect()
}

struct Materials {
    snapshot: VerificationSnapshot,
    source: PromptBlock,
    translation: Option<PromptBlock>,
    structure: Option<crate::text_project::ValidationReport>,
    context: Option<super::ContextSnapshot>,
    zhupi: super::BlockZhupi,
}

impl Translations {
    fn verification_file(&self, project: &str) -> Result<JsonFile<VerificationBook>> {
        Ok(JsonFile::new(
            self.store_dir(project)?.join("verification.json"),
            1,
            16 * 1024 * 1024,
        ))
    }

    fn verification_materials(
        &self,
        court: Option<&Court>,
        project: &str,
        block: &str,
    ) -> Result<Materials> {
        let p = self.project(project)?;
        let (_, _, source) = find_block(&p, block)?;
        let bytes = self.read_translation(project, block)?;
        let structure = bytes
            .as_ref()
            .map(|b| self.texts().check_in(&p, block, b))
            .transpose()?;
        // Invalid candidates remain visible as structural failures, not "reviewed" text.
        let translation = if structure.as_ref().is_some_and(|s| s.valid) {
            Some(serde_json::from_slice::<PromptBlock>(
                bytes.as_deref().expect("checked bytes"),
            )?)
        } else {
            None
        };
        let translation_hash = bytes.as_ref().map(|b| sha256_hex(b));
        let writer_task = court.and_then(|c| {
            c.translation_tasks(project, block)
                .filter(|w| w.translation.as_ref().is_some_and(|l| l.kind.writes()))
                .filter(|w| {
                    super::status::applied_output(c, w)
                        .is_some_and(|text| Some(sha256_hex(text.as_bytes())) == translation_hash)
                })
                .last()
                .map(|w| w.edict.id.0)
        });
        let context = self.current_context(project)?;
        let zhupi = self.zhupi(project, block)?;
        let snapshot = VerificationSnapshot {
            source_hash: super::block_source_hash(&p, block)?,
            translation_hash,
            context_hash: hash(&context)?,
            zhupi_hash: hash(&zhupi)?,
            writer_task,
            reviews: advice(court, project, block),
        };
        Ok(Materials {
            snapshot,
            source: source.clone(),
            translation,
            structure,
            context,
            zhupi,
        })
    }

    /// Append evidence without fetching URLs, changing approved context, or exporting it to a model.
    pub fn add_evidence(
        &self,
        project: &str,
        block: &str,
        input: &EvidenceInput,
    ) -> Result<LexicalEvidence> {
        let p = self.project(project)?;
        let (_, _, source) = find_block(&p, block)?;
        let paragraph = source
            .paragraphs
            .iter()
            .find(|p| p.id == input.paragraph)
            .ok_or_else(|| invalid("词证段号不属于当前块"))?;
        field("原文短引", &input.quote, 1000, true)?;
        if !paragraph.text.contains(&input.quote) {
            return Err(invalid("词证短引须确实出现在关联原段"));
        }
        field("来源 URL／书目信息", &input.source, 2000, true)?;
        field("定位", &input.locator, 1000, true)?;
        field("核对说明", &input.explanation, 4000, true)?;
        field("适用语境", &input.context, 2000, true)?;
        let (_, evidence) = self.verification_file(project)?.update(|old| {
            let mut book = old.map(|s| s.value).unwrap_or_default();
            let evidence = LexicalEvidence {
                id: book.next()?,
                block: block.into(),
                details: input.clone(),
                created: crate::time::now(),
            };
            book.evidence.push(evidence.clone());
            Ok::<_, super::TranslationError>((book, evidence))
        })?;
        Ok(evidence)
    }

    /// Read-only material-bound operator records, independent from structural checks and signoff.
    pub fn verification(
        &self,
        court: Option<&Court>,
        project: &str,
        block: &str,
    ) -> Result<VerificationView> {
        self.verification_with_associations(court, project, block, &[])
    }

    /// Same read-only view, with explicit paragraph associations for v2 address rules.
    pub fn verification_with_associations(
        &self,
        court: Option<&Court>,
        project: &str,
        block: &str,
        associations: &[crate::translation::visibility::ParagraphAssociations],
    ) -> Result<VerificationView> {
        let material = self.verification_materials(court, project, block)?;
        let book = self
            .verification_file(project)?
            .load()?
            .map(|s| s.value)
            .unwrap_or_default();
        let fingerprint = hash(&(&material.snapshot, book.sequence))?;
        let evidence = book
            .evidence
            .iter()
            .filter(|e| e.block == block)
            .cloned()
            .collect();
        let records: Vec<_> = book
            .records
            .iter()
            .filter(|r| r.block == block)
            .map(|r| {
                let current = r.snapshot == material.snapshot;
                let change = r
                    .previous
                    .and_then(|id| book.records.iter().find(|old| old.id == id))
                    .map(|old| TranslationChange {
                        before: old.translation.clone(),
                        after: r.translation.clone(),
                    });
                VerificationRecordView {
                    record: r.clone(),
                    current,
                    reason: (!current).then(|| {
                        "原文、译稿、上下文、朱批或审校／来源任务已变；需重新复核，旧意见保留"
                            .into()
                    }),
                    change,
                }
            })
            .collect();
        let paragraphs = material
            .source
            .paragraphs
            .iter()
            .map(|p| {
                let latest = records.iter().rev().find(|r| r.record.paragraph == p.id);
                let current = latest.is_some_and(|r| r.current);
                let checks = latest
                    .map(|r| r.record.checks.as_slice())
                    .unwrap_or_default();
                let unresolved = checks
                    .iter()
                    .filter(|c| {
                        c.disposition == Disposition::Pending
                            || matches!(
                                c.resolution,
                                Resolution::Unchecked | Resolution::Unresolved
                            )
                    })
                    .count();
                ParagraphVerification {
                    paragraph: p.id.clone(),
                    latest: latest.map(|r| r.record.id),
                    current,
                    opinions_handled: current
                        && checks.iter().any(|c| !c.advice.is_empty())
                        && checks.iter().all(|c| c.disposition != Disposition::Pending),
                    operator_reviewed: current && checks.len() == 6 && unresolved == 0,
                    unresolved: if checks.is_empty() {
                        6
                    } else {
                        unresolved + 6 - checks.len()
                    },
                }
            })
            .collect();
        let hints =
            if let (Some(context), Some(target)) = (&material.context, &material.translation) {
                hints::glossary_hints(
                    &self.project(project)?,
                    &context.glossary,
                    &material.source,
                    target,
                    associations,
                )?
            } else {
                Vec::new()
            };
        if self.verification_materials(court, project, block)?.snapshot != material.snapshot {
            return Err(invalid("资料在读取期间变化，请重试"));
        }
        Ok(VerificationView {
            project: project.into(),
            block: block.into(),
            fingerprint,
            source: material.source,
            translation: material.translation,
            structure: material.structure,
            writer_task: material.snapshot.writer_task,
            reviews: material.snapshot.reviews,
            zhupi: material.zhupi,
            evidence,
            records,
            hints,
            paragraphs,
        })
    }

    /// Deterministic, local QA. No model call, no writes, no approval.
    pub fn qa(&self, project: &str, block: &str) -> Result<QaReport> {
        let material = self.verification_materials(None, project, block)?;
        let mut issues = Vec::new();
        let Some(translation) = &material.translation else {
            issues.push(QaIssue {
                paragraph: String::new(),
                code: "no_translation",
                message: "没有结构有效的译稿；请先完成初译或人工译稿。".into(),
            });
            return Ok(QaReport {
                project: project.into(),
                block: block.into(),
                paragraphs: material.source.paragraphs.len(),
                issues,
            });
        };
        let source_language = self
            .project(project)
            .map(|p| p.metadata.source_language)
            .unwrap_or_default();
        let allowed_terms = collect_allowed_glossary_terms(material.context.as_ref());
        let source = &material.source.paragraphs;
        let target = &translation.paragraphs;
        let mut seen_ids = std::collections::HashSet::new();
        for (index, source_para) in source.iter().enumerate() {
            let Some(target_para) = target.get(index) else {
                issues.push(QaIssue {
                    paragraph: source_para.id.clone(),
                    code: "missing_paragraph",
                    message: "译稿缺少这一段。".into(),
                });
                continue;
            };
            if target_para.id != source_para.id {
                issues.push(QaIssue {
                    paragraph: source_para.id.clone(),
                    code: "paragraph_id_mismatch",
                    message: format!("译稿段号是 {}，与原文不一致。", target_para.id),
                });
            }
            if !seen_ids.insert(target_para.id.clone()) {
                issues.push(QaIssue {
                    paragraph: target_para.id.clone(),
                    code: "duplicate_paragraph",
                    message: "译稿里同一段号出现多次。".into(),
                });
            }
            if target_para.text.trim().is_empty() {
                issues.push(QaIssue {
                    paragraph: source_para.id.clone(),
                    code: "empty_translation",
                    message: "译文为空。".into(),
                });
            } else if target_para.text.trim() == source_para.text.trim() {
                issues.push(QaIssue {
                    paragraph: source_para.id.clone(),
                    code: "untranslated",
                    message: "译文与原文相同，可能未翻译。".into(),
                });
            }
            let source_digits: String = source_para
                .text
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect();
            let target_digits: String = target_para
                .text
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect();
            if source_digits != target_digits {
                issues.push(QaIssue {
                    paragraph: source_para.id.clone(),
                    code: "number_mismatch",
                    message: format_number_mismatch(&source_digits, &target_digits),
                });
            }
            if !target_para.text.trim().is_empty()
                && target_para.text.trim() != source_para.text.trim()
            {
                if let Some(message) = detect_source_script_leak(
                    &source_language,
                    &source_para.text,
                    &target_para.text,
                    &allowed_terms,
                ) {
                    issues.push(QaIssue {
                        paragraph: source_para.id.clone(),
                        code: "source_script_leak",
                        message,
                    });
                }
                if let Some(message) = detect_punctuation_mismatch(&target_para.text) {
                    issues.push(QaIssue {
                        paragraph: source_para.id.clone(),
                        code: "punctuation_mismatch",
                        message,
                    });
                }
            }
        }
        if target.len() > source.len() {
            issues.push(QaIssue {
                paragraph: String::new(),
                code: "extra_paragraph",
                message: "译稿比原文多出段落。".into(),
            });
        }
        let mut texts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for para in target {
            if !para.text.trim().is_empty() {
                *texts.entry(para.text.as_str()).or_default() += 1;
            }
        }
        for (text, count) in texts {
            if count > 1 {
                issues.push(QaIssue {
                    paragraph: String::new(),
                    code: "duplicate_text",
                    message: format!(
                        "同一段译文重复 {count} 次：{}",
                        text.chars().take(30).collect::<String>()
                    ),
                });
            }
        }
        if let Some(context) = &material.context {
            match &context.glossary {
                crate::translation::Glossary::Legacy(entries) => {
                    for entry in entries {
                        for (source_para, target_para) in source.iter().zip(target) {
                            if source_para.text.contains(&entry.source)
                                && !entry.allowed.iter().any(|a| target_para.text.contains(a))
                            {
                                issues.push(QaIssue {
                                    paragraph: source_para.id.clone(),
                                    code: "term_inconsistent",
                                    message: format!("术语「{}」没有使用允许译法。", entry.source),
                                });
                            }
                        }
                    }
                }
                crate::translation::Glossary::Structured(_) => {
                    // QA has no explicit person/speaker associations. The shared
                    // evaluator therefore yields standalone term hints only.
                    for hint in hints::glossary_hints(
                        &self.project(project)?,
                        &context.glossary,
                        &material.source,
                        translation,
                        &[],
                    )? {
                        issues.push(QaIssue {
                            paragraph: hint.paragraph,
                            code: "term_inconsistent",
                            message: format!(
                                "术语「{}」未见允许译法。{}",
                                hint.source, hint.message
                            ),
                        });
                    }
                }
            }
        }
        Ok(QaReport {
            project: project.into(),
            block: block.into(),
            paragraphs: source.len(),
            issues,
        })
    }

    /// 整书级批量确定性 QA 汇总（纯只读）：逐块执行 [`Self::qa`]，汇总问题分布并附带段级短摘录。
    ///
    /// 未译块仅计入 `summary.untranslated_blocks` 与 `blocks[].has_translation = false`，
    /// 不把 `no_translation` 混入已译块的问题明细，避免半译书籍被大量未译提示淹没。
    pub fn project_qa_report(
        &self,
        project: &str,
        statuses: Option<&[super::BlockStatus]>,
    ) -> Result<ProjectQaReport> {
        let p = self.project(project)?;
        let status_map: BTreeMap<&str, &super::BlockStatus> = statuses
            .into_iter()
            .flatten()
            .map(|s| (s.block.as_str(), s))
            .collect();
        let mut summary = ProjectQaSummary::default();
        let mut by_code: BTreeMap<String, usize> = BTreeMap::new();
        let mut blocks = Vec::new();
        let mut issues = Vec::new();

        for chapter in &p.chapters {
            let chapter_title = chapter
                .parsing
                .as_ref()
                .map(|parsed| parsed.title.as_str())
                .filter(|t| !t.trim().is_empty())
                .unwrap_or(&chapter.source_name)
                .to_owned();
            for prompt in &chapter.blocks {
                summary.total_blocks += 1;
                let status = status_map.get(prompt.block_id.as_str()).copied();
                let (state, state_label) = match status {
                    Some(st) => {
                        if matches!(
                            st.state,
                            super::BlockState::Finalized { .. }
                                | super::BlockState::HumanFinalized { .. }
                        ) {
                            summary.finalized_blocks += 1;
                        }
                        (
                            Some(block_state_code(&st.state).to_owned()),
                            Some(st.state.label().to_owned()),
                        )
                    }
                    None => (None, None),
                };
                let report = self.qa(project, &prompt.block_id)?;
                let has_translation = !report.issues.iter().any(|i| i.code == "no_translation");
                if !has_translation {
                    summary.untranslated_blocks += 1;
                    blocks.push(ProjectQaBlockSummary {
                        block: prompt.block_id.clone(),
                        chapter_id: chapter.id.clone(),
                        chapter_title: chapter_title.clone(),
                        state,
                        state_label,
                        paragraphs: report.paragraphs,
                        has_translation: false,
                        issue_count: 0,
                    });
                    continue;
                }
                summary.translated_blocks += 1;
                summary.checked_blocks += 1;
                if report.issues.is_empty() {
                    summary.clean_blocks += 1;
                } else {
                    summary.blocks_with_issues += 1;
                    summary.total_issues += report.issues.len();
                    let target_paras: BTreeMap<String, String> = self
                        .read_translation(project, &prompt.block_id)?
                        .and_then(|bytes| {
                            super::read_approved_target(&bytes, &prompt.block_id).ok()
                        })
                        .map(|approved| match approved {
                            super::ApprovedTarget::Plain(b) => b
                                .paragraphs
                                .into_iter()
                                .map(|para| (para.id, para.text))
                                .collect(),
                            super::ApprovedTarget::Marked(b) => b
                                .paragraphs
                                .into_iter()
                                .map(|para| (para.id, para.text))
                                .collect(),
                        })
                        .unwrap_or_default();
                    let source_paras: BTreeMap<&str, &str> = prompt
                        .paragraphs
                        .iter()
                        .map(|para| (para.id.as_str(), para.text.as_str()))
                        .collect();
                    for issue in &report.issues {
                        *by_code.entry(issue.code.to_owned()).or_default() += 1;
                        let source_excerpt = if issue.paragraph.is_empty() {
                            String::new()
                        } else {
                            source_paras
                                .get(issue.paragraph.as_str())
                                .map(|t| compact_excerpt(t, 80))
                                .unwrap_or_default()
                        };
                        let target_excerpt = if issue.paragraph.is_empty() {
                            String::new()
                        } else {
                            target_paras
                                .get(&issue.paragraph)
                                .map(|t| compact_excerpt(t, 80))
                                .unwrap_or_default()
                        };
                        issues.push(ProjectQaIssue {
                            block: prompt.block_id.clone(),
                            chapter_id: chapter.id.clone(),
                            chapter_title: chapter_title.clone(),
                            paragraph: issue.paragraph.clone(),
                            code: issue.code,
                            message: issue.message.clone(),
                            source_excerpt,
                            target_excerpt,
                        });
                    }
                }
                blocks.push(ProjectQaBlockSummary {
                    block: prompt.block_id.clone(),
                    chapter_id: chapter.id.clone(),
                    chapter_title: chapter_title.clone(),
                    state,
                    state_label,
                    paragraphs: report.paragraphs,
                    has_translation: true,
                    issue_count: report.issues.len(),
                });
            }
        }

        Ok(ProjectQaReport {
            project: project.into(),
            founded: statuses.is_some(),
            summary,
            by_code,
            blocks,
            issues,
        })
    }

    /// Save an explicit operator decision against the exact material preview; no model completion can call this implicitly.
    pub fn record_verification(
        &self,
        court: Option<&Court>,
        project: &str,
        block: &str,
        input: &VerificationInput,
    ) -> Result<VerificationRecord> {
        let material = self.verification_materials(court, project, block)?;
        let source = material
            .source
            .paragraphs
            .iter()
            .find(|p| p.id == input.paragraph)
            .ok_or_else(|| invalid("复核段号不属于当前块"))?;
        let target = material
            .translation
            .as_ref()
            .ok_or_else(|| invalid("复核须有结构通过的现行译稿；结构检查不代表语义正确"))?
            .paragraphs
            .iter()
            .find(|p| p.id == input.paragraph)
            .expect("structural paragraph match");
        if input.checks.is_empty() || input.checks.len() > 6 {
            return Err(invalid("请明确记录 1 至 6 项复核，不自动勾选"));
        }
        let mut categories = BTreeSet::new();
        for check in &input.checks {
            if !categories.insert(check.category) {
                return Err(invalid("同一复核类别不能重复"));
            }
            field(
                "复核理由",
                &check.note,
                4000,
                check.disposition != Disposition::Pending,
            )?;
            match (check.disposition, check.resolution) {
                (Disposition::Accepted, Resolution::NotApplicable)
                | (Disposition::Pending, Resolution::Resolved)
                | (
                    Disposition::Rejected | Disposition::NotApplicable,
                    Resolution::Resolved | Resolution::Unresolved,
                ) => {
                    return Err(invalid(
                        "意见处理与修订复核不相符；接受意见不等于已核实解决",
                    ));
                }
                _ => {}
            }
            if check.advice.len() > 8 || check.evidence.len() > 16 {
                return Err(invalid("每项最多关联 8 条审校短引与 16 条词证"));
            }
            let mut advice_ids = BTreeSet::new();
            for link in &check.advice {
                field("审校短引", &link.quote, 2000, true)?;
                if !advice_ids.insert((link.task, &link.quote))
                    || !material
                        .snapshot
                        .reviews
                        .iter()
                        .any(|r| r.task == link.task && r.text.contains(&link.quote))
                {
                    return Err(invalid("审校关联须为本块已完成回奏中的真实短引且不重复"));
                }
            }
        }
        let (_, record) = self.verification_file(project)?.update(|old| {
            let mut book = old.map(|s| s.value).unwrap_or_default();
            if hash(&(&material.snapshot, book.sequence))? != input.fingerprint {
                return Err(invalid(
                    "复核确认已过期；请重新读取材料与记录，不覆盖他人意见",
                ));
            }
            for check in &input.checks {
                let mut ids = BTreeSet::new();
                for id in &check.evidence {
                    if !ids.insert(id)
                        || !book.evidence.iter().any(|e| {
                            e.id == *id
                                && e.block == block
                                && e.details.paragraph == input.paragraph
                        })
                    {
                        return Err(invalid("词证须属于本段且不能重复"));
                    }
                }
            }
            if self.verification_materials(court, project, block)?.snapshot != material.snapshot {
                return Err(invalid("资料在保存期间变化；请重新复核"));
            }
            let previous = book
                .records
                .iter()
                .rev()
                .find(|r| r.block == block && r.paragraph == input.paragraph)
                .map(|r| r.id);
            let record = VerificationRecord {
                id: book.next()?,
                block: block.into(),
                paragraph: input.paragraph.clone(),
                checks: input.checks.clone(),
                snapshot: material.snapshot.clone(),
                source: source.text.clone(),
                translation: target.text.clone(),
                zhupi: material
                    .zhupi
                    .items
                    .get(&input.paragraph)
                    .map(|n| n.text.clone()),
                previous,
                created: crate::time::now(),
            };
            book.records.push(record.clone());
            Ok::<_, super::TranslationError>((book, record))
        })?;
        Ok(record)
    }
}

fn block_state_code(state: &super::BlockState) -> &'static str {
    match state {
        super::BlockState::NotStarted => "not_started",
        super::BlockState::Running { .. } => "running",
        super::BlockState::AwaitingApproval { .. } => "awaiting_approval",
        super::BlockState::Applied { .. } => "applied",
        super::BlockState::AwaitingYulan { .. } => "awaiting_yulan",
        super::BlockState::Finalized { .. } => "finalized",
        super::BlockState::Failed { .. } => "failed",
        super::BlockState::Cancelled { .. } => "cancelled",
        super::BlockState::Stale { .. } => "stale",
        super::BlockState::HumanApproved { .. } => "human_approved",
        super::BlockState::HumanFinalized { .. } => "human_finalized",
        super::BlockState::HumanStale { .. } => "human_stale",
        super::BlockState::Uncertain { .. } => "uncertain",
    }
}

fn compact_excerpt(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut prev_ws = false;
    let mut count = 0usize;
    let mut truncated = false;
    for ch in text.trim().chars() {
        let mapped = if ch.is_whitespace() { ' ' } else { ch };
        if mapped == ' ' && prev_ws {
            continue;
        }
        prev_ws = mapped == ' ';
        if count >= max_chars {
            truncated = true;
            break;
        }
        out.push(mapped);
        count += 1;
    }
    if truncated {
        out.push('…');
    }
    out
}

fn format_number_mismatch(source_digits: &str, target_digits: &str) -> String {
    fn show(d: &str) -> &str {
        if d.is_empty() { "无" } else { d }
    }
    format!(
        "数字不一致：原文含 {}，译文含 {}。",
        show(source_digits),
        show(target_digits)
    )
}

fn collect_allowed_glossary_terms(context: Option<&super::ContextSnapshot>) -> Vec<String> {
    let mut allowed = Vec::new();
    if let Some(context) = context {
        match &context.glossary {
            crate::translation::Glossary::Legacy(entries) => {
                for entry in entries {
                    allowed.extend(
                        entry
                            .allowed
                            .iter()
                            .map(|s| s.trim())
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned),
                    );
                }
            }
            crate::translation::Glossary::Structured(glossary) => {
                for term in &glossary.terms {
                    allowed.extend(
                        term.allowed
                            .iter()
                            .map(|s| s.trim())
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned),
                    );
                }
                for person in &glossary.persons {
                    let name = person.name.trim();
                    if !name.is_empty() {
                        allowed.push(name.to_owned());
                    }
                    allowed.extend(
                        person
                            .aliases
                            .iter()
                            .map(|s| s.trim())
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned),
                    );
                }
            }
        }
    }
    allowed.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    allowed.dedup();
    allowed
}

fn mask_allowed_terms(text: &str, allowed: &[String]) -> String {
    let mut masked = text.to_owned();
    for term in allowed {
        if masked.contains(term.as_str()) {
            masked = masked.replace(term.as_str(), " ");
        }
    }
    masked
}

fn is_japanese_kana(ch: char) -> bool {
    matches!(ch, '\u{3040}'..='\u{309F}')
        || (matches!(ch, '\u{30A1}'..='\u{30FF}') && !matches!(ch, '・' | 'ー'))
}

fn is_hangul(ch: char) -> bool {
    matches!(
        ch,
        '\u{AC00}'..='\u{D7A3}' | '\u{1100}'..='\u{11FF}' | '\u{3131}'..='\u{318E}'
    )
}

fn first_script_run(
    text: &str,
    predicate: impl Fn(char) -> bool,
    extend: impl Fn(char) -> bool,
) -> Option<String> {
    let mut run = String::new();
    for ch in text.chars() {
        if run.is_empty() {
            if predicate(ch) {
                run.push(ch);
            }
        } else if predicate(ch) || extend(ch) {
            if run.chars().count() < 16 {
                run.push(ch);
            }
        } else {
            break;
        }
    }
    (!run.is_empty()).then_some(run)
}

fn extract_western_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphabetic() || ((ch == '-' || ch == '\'') && !current.is_empty()) {
            current.push(ch);
        } else if !current.is_empty() {
            let trimmed = current.trim_matches(['-', '\'']);
            if !trimmed.is_empty() {
                words.push(trimmed.to_owned());
            }
            current.clear();
        }
    }
    let trimmed = current.trim_matches(['-', '\'']);
    if !trimmed.is_empty() {
        words.push(trimmed.to_owned());
    }
    words
}

fn detect_source_script_leak(
    source_language: &str,
    source: &str,
    target: &str,
    allowed: &[String],
) -> Option<String> {
    let masked = mask_allowed_terms(target, allowed);
    match source_language {
        "ja" => first_script_run(&masked, is_japanese_kana, |ch| ch == 'ー')
            .map(|sample| format!("译文残留日语假名「{sample}」，请确认是否漏译。")),
        "ko" => first_script_run(&masked, is_hangul, |_| false)
            .map(|sample| format!("译文残留韩文字符「{sample}」，请确认是否漏译。")),
        "fr" | "de" => {
            const FR_DIACRITICS: &str = "éèêëàâçîïôùûüÿœæÉÈÊËÀÂÇÎÏÔÙÛÜŸŒÆ";
            const DE_DIACRITICS: &str = "äöüßÄÖÜ";
            let diacritics = if source_language == "fr" {
                FR_DIACRITICS
            } else {
                DE_DIACRITICS
            };
            let source_words: std::collections::HashSet<String> = extract_western_words(source)
                .into_iter()
                .map(|w| w.to_lowercase())
                .collect();
            for word in extract_western_words(&masked) {
                if word.chars().count() >= 2
                    && word.chars().any(|c| diacritics.contains(c))
                    && source_words.contains(&word.to_lowercase())
                {
                    return Some(format!("译文残留源语词「{word}」，请确认是否漏译。"));
                }
            }
            None
        }
        _ => None,
    }
}

fn strip_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("http://").or_else(|| rest.find("https://")) {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let end = tail
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '）' | '」' | '』' | '》' | '】' | ')' | ']')
            })
            .unwrap_or(tail.len());
        out.push(' ');
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn is_cjk_ideograph(ch: char) -> bool {
    matches!(ch, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

fn detect_punctuation_mismatch(target: &str) -> Option<String> {
    let cleaned = strip_urls(target);
    const PAIRS: &[(char, char, &str)] = &[
        ('「', '」', "「」"),
        ('『', '』', "『』"),
        ('“', '”', "“”"),
        ('《', '》', "《》"),
        ('（', '）', "（）"),
        ('【', '】', "【】"),
    ];
    let mut bad_pairs = Vec::new();
    for &(open, close, label) in PAIRS {
        let mut depth: i32 = 0;
        let mut broken = false;
        for ch in cleaned.chars() {
            if ch == open {
                depth += 1;
            } else if ch == close {
                depth -= 1;
                if depth < 0 {
                    broken = true;
                    depth = 0;
                }
            }
        }
        if broken || depth != 0 {
            bad_pairs.push(label);
        }
    }
    let mut halfwidth = Vec::new();
    let mut prev: Option<char> = None;
    for ch in cleaned.chars() {
        if matches!(ch, ',' | '?' | '!' | ';' | ':')
            && prev.is_some_and(is_cjk_ideograph)
            && !halfwidth.contains(&ch)
        {
            halfwidth.push(ch);
        }
        prev = Some(ch);
    }
    let mut parts = Vec::new();
    if !bad_pairs.is_empty() {
        parts.push(format!("{}未配对", bad_pairs.join("、")));
    }
    if !halfwidth.is_empty() {
        let joined: Vec<String> = halfwidth.into_iter().map(|c| c.to_string()).collect();
        parts.push(format!("含贴近汉字的半角标点（{}）", joined.join("、")));
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("标点异常：{}。", parts.join("；")))
    }
}

#[cfg(test)]
mod tests {
    use super::{detect_punctuation_mismatch, detect_source_script_leak, format_number_mismatch};

    #[test]
    fn formats_number_mismatch_without_debug_quotes() {
        assert_eq!(
            format_number_mismatch("1904", ""),
            "数字不一致：原文含 1904，译文含 无。"
        );
        assert_eq!(
            format_number_mismatch("1904", "1905"),
            "数字不一致：原文含 1904，译文含 1905。"
        );
    }

    #[test]
    fn detects_source_script_leaks_per_language_and_respects_glossary() {
        assert!(
            detect_source_script_leak("ja", "雨の夜でした。", "那是一个雨の夜。", &[])
                .unwrap()
                .contains("の")
        );
        assert!(
            detect_source_script_leak("ja", "アルカディアへ", "前往アルカディア。", &[])
                .unwrap()
                .contains("アルカディア")
        );
        assert!(
            detect_source_script_leak(
                "ja",
                "アルカディアへ",
                "前往アルカディア。",
                &["アルカディア".into()]
            )
            .is_none()
        );
        // Standalone middle dot used in Chinese transliterated names must not trigger.
        assert!(detect_source_script_leak("ja", "テスト", "亚瑟・潘德拉贡", &[]).is_none());

        assert!(
            detect_source_script_leak("ko", "안녕하세요", "你好 안녕하세요", &[])
                .unwrap()
                .contains("안녕하세요")
        );
        assert!(detect_source_script_leak("ko", "안녕하세요", "你好。", &[]).is_none());

        assert!(
            detect_source_script_leak("fr", "Un été à Paris", "巴黎的一个 été。", &[])
                .unwrap()
                .contains("été")
        );
        assert!(detect_source_script_leak("fr", "Un été à Paris", "巴黎的夏天。", &[]).is_none());
        assert!(
            detect_source_script_leak("fr", "Le café de Flore", "在 café 小坐。", &["café".into()])
                .is_none()
        );

        assert!(
            detect_source_script_leak("de", "Über die Straße", "穿过那条 Straße。", &[])
                .unwrap()
                .contains("Straße")
        );
        assert!(detect_source_script_leak("de", "Über die Straße", "穿过街道。", &[]).is_none());
    }

    #[test]
    fn detects_punctuation_mismatches_without_false_positives() {
        assert!(detect_punctuation_mismatch("「今天天气真好。」（记于长安）").is_none());
        assert!(
            detect_punctuation_mismatch("详见 https://example.com/a?x=1:2 与 12:30。").is_none()
        );
        let unclosed = detect_punctuation_mismatch("「今天天气真好。").unwrap();
        assert!(unclosed.contains("「」未配对"), "{unclosed}");
        let half = detect_punctuation_mismatch("真的吗?太好了!").unwrap();
        assert!(half.contains("?") && half.contains("!"), "{half}");
        let both = detect_punctuation_mismatch("《长安记，你好,世界").unwrap();
        assert!(both.contains("《》未配对") && both.contains(","), "{both}");
    }

    #[test]
    fn project_qa_report_summarizes_translated_untranslated_and_issues() {
        let dir = std::env::temp_dir().join(format!(
            "ziyuan-project-qa-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let tp = crate::text_project::TextProjects::new(&dir);
        let meta = tp
            .create(crate::text_project::NewProject {
                title: "整书质检示例".into(),
                source_language: "ja".into(),
                target_language: "zh-CN".into(),
                provenance: crate::text_project::Provenance {
                    source: "local".into(),
                    rights: "owned".into(),
                    author: String::new(),
                    version: String::new(),
                    date: String::new(),
                    scope: String::new(),
                },
            })
            .unwrap();
        let ch1 = dir.join("ch1.txt");
        let ch2 = dir.join("ch2.txt");
        let ch3 = dir.join("ch3.txt");
        std::fs::write(
            &ch1,
            "雨の夜だった。

静かな朝。",
        )
        .unwrap();
        std::fs::write(&ch2, "風の音。").unwrap();
        std::fs::write(&ch3, "月の光。").unwrap();
        let project = tp.import(&meta.id, &[ch1, ch2, ch3]).unwrap();
        let b1 = &project.chapters[0].blocks[0];
        let b2 = &project.chapters[1].blocks[0];
        let b3 = &project.chapters[2].blocks[0];

        // b1: translated with source script leak + punctuation mismatch
        let b1_dir = dir.join(super::super::block_dir_rel(&meta.id, &b1.block_id));
        std::fs::create_dir_all(&b1_dir).unwrap();
        let zh1 = serde_json::json!({
            "schema_version": b1.schema_version,
            "block_id": b1.block_id,
            "paragraphs": [
                { "id": b1.paragraphs[0].id, "text": "那是一个雨の夜?" },
                { "id": b1.paragraphs[1].id, "text": "宁静的清晨。" }
            ]
        });
        std::fs::write(
            dir.join(super::super::output_rel(&meta.id, &b1.block_id)),
            serde_json::to_vec_pretty(&zh1).unwrap(),
        )
        .unwrap();

        // b2: clean translation
        let b2_dir = dir.join(super::super::block_dir_rel(&meta.id, &b2.block_id));
        std::fs::create_dir_all(&b2_dir).unwrap();
        let zh2 = serde_json::json!({
            "schema_version": b2.schema_version,
            "block_id": b2.block_id,
            "paragraphs": [
                { "id": b2.paragraphs[0].id, "text": "风声。" }
            ]
        });
        std::fs::write(
            dir.join(super::super::output_rel(&meta.id, &b2.block_id)),
            serde_json::to_vec_pretty(&zh2).unwrap(),
        )
        .unwrap();

        let _ = b3;
        let t = super::Translations::new(&dir);
        let report = t.project_qa_report(&meta.id, None).unwrap();
        assert!(!report.founded);
        assert_eq!(report.summary.total_blocks, 3);
        assert_eq!(report.summary.translated_blocks, 2);
        assert_eq!(report.summary.untranslated_blocks, 1);
        assert_eq!(report.summary.checked_blocks, 2);
        assert_eq!(report.summary.clean_blocks, 1);
        assert_eq!(report.summary.blocks_with_issues, 1);
        assert_eq!(report.summary.total_issues, 2);
        assert_eq!(report.by_code.get("source_script_leak"), Some(&1));
        assert_eq!(report.by_code.get("punctuation_mismatch"), Some(&1));
        assert!(!report.by_code.contains_key("no_translation"));
        assert_eq!(report.issues.len(), 2);
        assert_eq!(report.issues[0].block, b1.block_id);
        assert_eq!(report.issues[0].paragraph, b1.paragraphs[0].id);
        assert_eq!(report.issues[0].source_excerpt, "雨の夜だった。");
        assert_eq!(report.issues[0].target_excerpt, "那是一个雨の夜?");
        assert!(!report.blocks[2].has_translation);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
