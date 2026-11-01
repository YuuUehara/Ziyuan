//! Operator-owned lexical evidence and semantic checklists; never model input or signoff gates.
use super::{Result, Translations, find_block, invalid, sha256_hex};
use crate::text_project::PromptBlock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
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
            .map(|b| self.texts().check(project, block, b))
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
        let mut hints = Vec::new();
        if let (Some(context), Some(target)) = (&material.context, &material.translation) {
            for (source, target) in material.source.paragraphs.iter().zip(&target.paragraphs) {
                for entry in &context.glossary {
                    if source.text.contains(&entry.source)
                        && !entry.allowed.iter().any(|a| target.text.contains(a))
                    {
                        hints.push(GlossaryHint { paragraph: source.id.clone(), source: entry.source.clone(), allowed: entry.allowed.clone(), scope: entry.scope.clone(), note: entry.note.clone(),
                            message: if entry.scope.is_some() { "原文命中词条而译文未见允许译法；作用范围为自由文本，先人工核对是否适用" } else { "原文命中词条而译文未见允许译法；仅字面提示，不是语义错译判决" }.into() });
                    }
                }
            }
        }
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
