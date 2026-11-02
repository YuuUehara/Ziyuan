//! Local human drafts and immutable review submissions, never approved writes.
//! Text, submissions and durable retry receipts share one atomic JSON publication.
use super::{
    Result, TargetMark, TargetMarkKind, TranslationError, Translations, ensure_plain_dirs,
    find_block, invalid, markup, sha256_hex,
};
use crate::text_project::{BlockParagraph, PromptBlock};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io};
use ziyuan_storage::{JsonFile, LockScope, Workspace};

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

pub use ziyuan_core::DraftBasis;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftParagraph {
    pub id: String,
    pub text: String,
    /// Target-text ranges only. Empty marks preserve legacy hashes/receipts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<TargetMark>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanDraft {
    pub project: String,
    pub block: String,
    pub revision: String,
    pub basis: DraftBasis,
    /// A source-ordered subset; empty text and missing paragraphs remain drafts.
    pub paragraphs: Vec<DraftParagraph>,
}

/// Full-block replacement. Local callers are trusted; future remote entry points
/// must authenticate write access before calling this API (there is no Web endpoint).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftSave {
    pub request_id: String,
    pub expected_revision: Option<String>,
    pub basis: DraftBasis,
    pub paragraphs: Vec<DraftParagraph>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftReceipt {
    pub revision: String,
    pub content_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DraftView {
    /// Current material binding; may differ from the stored draft's basis.
    pub basis: DraftBasis,
    pub draft: Option<HumanDraft>,
}

/// IDs are scoped to project/block/submit, separate from save IDs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftSubmit {
    pub request_id: String,
    pub expected_revision: String,
}

/// Frozen review material, not an approval or an eligible finalized translation.
/// Future approval must explicitly bind the then-current approval/event identity;
/// the disk hash in `basis` alone is insufficient for that authorization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanSubmission {
    pub request_id: String,
    pub draft: HumanDraft,
    pub candidate_hash: String,
}

impl HumanSubmission {
    /// Prepare frozen pending material only; no lock, admission, append or write.
    /// Authentication, current basis and predecessor resolution remain the future
    /// publication caller's responsibility, not evidence supplied by this object.
    pub fn prepare_binding(
        &self,
        request_id: String,
        actor: ziyuan_core::HumanActor,
        previous: ziyuan_core::TranslationSource,
    ) -> Result<ziyuan_core::HumanSubmissionBinding> {
        check_request_id(&request_id)?;
        check_request_id(&self.request_id)?;
        let bytes = self.candidate()?.serialized()?;
        if sha256_hex(&bytes) != self.candidate_hash {
            return Err(invalid("人工送审候选摘要不符"));
        }
        let request = ziyuan_core::HumanBindingRequest {
            project: self.draft.project.clone(),
            block: self.draft.block.clone(),
            request_id,
            actor,
            submission_id: self.request_id.clone(),
            revision: self.draft.revision.clone(),
            basis: self.draft.basis.clone(),
            candidate: String::from_utf8(bytes).expect("candidate JSON is UTF-8"),
            candidate_hash: self.candidate_hash.clone(),
            previous,
        };
        Ok(ziyuan_core::HumanSubmissionBinding {
            request_hash: ziyuan_governance::human_binding_request_hash(&request),
            request,
        })
    }

    /// Legacy plain candidates keep their exact bytes; marked candidates use v2.
    /// Neither representation confers approval or formal export eligibility.
    pub fn candidate(&self) -> Result<ReviewCandidate> {
        let candidate = ReviewCandidate {
            schema_version: if self.draft.paragraphs.iter().any(|p| !p.marks.is_empty()) {
                2
            } else {
                1
            },
            block_id: self.draft.block.clone(),
            paragraphs: self.draft.paragraphs.clone(),
        };
        candidate.validate()?;
        Ok(candidate)
    }
}

/// Target-only review JSON. v1 is byte-compatible with plain PromptBlock;
/// v2 preserves marks and must not enter the legacy model/approval pipeline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewCandidate {
    pub schema_version: u32,
    pub block_id: String,
    pub paragraphs: Vec<DraftParagraph>,
}

/// Representation profiles, not a list of implemented formal exporters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewFormat {
    ReviewJson,
    LegacyPromptJson,
    PlainText,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UnsupportedTargetMark {
    pub paragraph_id: String,
    /// Index in the candidate's original, unsorted marks array.
    pub mark_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReviewFormatCheck {
    pub format: ReviewFormat,
    /// Mark semantics only, not project identity or approval eligibility.
    pub lossless: bool,
    pub unsupported_marks: Vec<UnsupportedTargetMark>,
}

impl ReviewCandidate {
    /// Parse the one authoritative candidate, even without the local draft book.
    /// This checks frozen representation only, never authorization or freshness.
    pub fn from_binding(binding: &ziyuan_core::HumanSubmissionBinding) -> Result<Self> {
        let r = &binding.request;
        let candidate: Self = serde_json::from_str(&r.candidate)?;
        if candidate.block_id != r.block
            || candidate.serialized()? != r.candidate.as_bytes()
            || sha256_hex(r.candidate.as_bytes()) != r.candidate_hash
            || ziyuan_governance::human_binding_request_hash(r) != binding.request_hash
        {
            return Err(invalid("人工送审绑定与冻结候选不符"));
        }
        Ok(candidate)
    }

    fn validate(&self) -> Result<()> {
        if !matches!(self.schema_version, 1 | 2)
            || (self.schema_version == 1 && self.paragraphs.iter().any(|p| !p.marks.is_empty()))
        {
            return Err(invalid("不支持的人工候选版本或 v1 含文学标记"));
        }
        markup::validate(&self.paragraphs)?;
        if self.paragraphs.iter().flat_map(|p| &p.marks).any(
            |m| matches!(&m.kind, TargetMarkKind::Ruby { reading } if reading.trim().is_empty()),
        ) {
            return Err(invalid("送审候选的注音读音不得为空或全为空白"));
        }
        Ok(())
    }

    /// Exact hash input, including target marks and the final newline.
    pub fn serialized(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Read-only diagnostic. No conversion, file output or implicit lossy choice.
    pub fn check_format(&self, format: ReviewFormat) -> Result<ReviewFormatCheck> {
        self.validate()?;
        let mut unsupported_marks = Vec::new();
        if format != ReviewFormat::ReviewJson {
            for paragraph in &self.paragraphs {
                for mark_index in 0..paragraph.marks.len() {
                    unsupported_marks.push(UnsupportedTargetMark {
                        paragraph_id: paragraph.id.clone(),
                        mark_index,
                    });
                }
            }
        }
        Ok(ReviewFormatCheck {
            format,
            lossless: unsupported_marks.is_empty(),
            unsupported_marks,
        })
    }

    /// Only for reusing paragraph membership/completeness validation. Never
    /// stored, hashed or published as the marked candidate.
    fn text_structure(&self) -> PromptBlock {
        PromptBlock {
            schema_version: 1,
            block_id: self.block_id.clone(),
            paragraphs: self
                .paragraphs
                .iter()
                .map(|p| BlockParagraph {
                    id: p.id.clone(),
                    text: p.text.clone(),
                    inlines: Vec::new(),
                })
                .collect(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedRequest {
    hash: String,
    receipt: DraftReceipt,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftBook {
    draft: HumanDraft,
    requests: BTreeMap<String, SavedRequest>,
    #[serde(default)]
    submissions: BTreeMap<String, HumanSubmission>,
}

impl DraftBook {
    fn recorded(&self, draft: &HumanDraft) -> Result<bool> {
        let hash = sha256_hex(&serde_json::to_vec(draft)?);
        Ok(self
            .requests
            .values()
            .any(|r| r.receipt.revision == draft.revision && r.receipt.content_hash == hash))
    }

    fn check(&self, project: &str, block: &str) -> Result<()> {
        if self.draft.project != project
            || self.draft.block != block
            || !self.recorded(&self.draft)?
        {
            return Err(ziyuan_storage::JsonFileError::Corrupt(
                "人工草稿位置或现行回执摘要不符".into(),
            )
            .into());
        }
        for (id, submission) in &self.submissions {
            if id != &submission.request_id
                || submission.draft.project != project
                || submission.draft.block != block
                || !self.recorded(&submission.draft)?
                || submission.candidate_hash != sha256_hex(&submission.candidate()?.serialized()?)
            {
                return Err(ziyuan_storage::JsonFileError::Corrupt(
                    "人工提交位置、草稿回执或候选摘要不符".into(),
                )
                .into());
            }
        }
        Ok(())
    }
}

fn check_request_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(invalid("草稿请求身份须为 1–128 位 ASCII 字母数字、- 或 _"));
    }
    Ok(())
}

impl Translations {
    fn draft_file(&self, project: &str, block: &str) -> Result<JsonFile<DraftBook>> {
        // Membership is checked before using either identifier as a path component.
        find_block(&self.project(project)?, block)?;
        let dir = ensure_plain_dirs(
            &self.root,
            &[".ziyuan", "text-projects", project, "human-drafts"],
        )?;
        let file = JsonFile::new(dir.join(format!("{block}.json")), 4, MAX_FILE_BYTES);
        for path in [file.path(), file.lock_path()] {
            match fs::symlink_metadata(path) {
                Ok(m) if m.is_file() && !m.file_type().is_symlink() => {}
                Ok(_) => return Err(invalid("人工草稿及其锁须为普通文件")),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(file)
    }

    fn draft_basis(&self, project: &str, block: &str) -> Result<DraftBasis> {
        let p = self.project(project)?;
        let (_, _, source) = find_block(&p, block)?;
        Ok(DraftBasis {
            project_hash: sha256_hex(&serde_json::to_vec(&p)?),
            source_hash: sha256_hex(&source.serialized()?),
            context_version: self.context_version(project)?,
            zhupi_hash: self.block_zhupi_hash(&p, block)?,
            translation_hash: self
                .read_translation(project, block)?
                .map(|bytes| sha256_hex(&bytes)),
        })
    }

    /// Reopen current text even if its stored basis is stale. Acquires workspace
    /// exclusion and may create control directories, but changes no events/generation.
    pub fn human_draft(&self, project: &str, block: &str) -> Result<DraftView> {
        let workspace = Workspace::open(&self.root).map_err(|e| invalid(e.to_string()))?;
        let _guard = workspace
            .lock(LockScope::Workspace)
            .map_err(|e| invalid(e.to_string()))?;
        workspace
            .ensure_writable()
            .map_err(|e| invalid(e.to_string()))?;
        let basis = self.draft_basis(project, block)?;
        let draft = self
            .draft_file(project, block)?
            .load()?
            .map(|s| {
                s.value.check(project, block)?;
                Ok::<_, TranslationError>(s.value.draft)
            })
            .transpose()?;
        Ok(DraftView { basis, draft })
    }

    /// Save text and a receipt together. Retrying an older successful request returns
    /// its original receipt, never rolls back a newer draft and never writes.
    pub fn save_human_draft(
        &self,
        project: &str,
        block: &str,
        request: &DraftSave,
    ) -> Result<DraftReceipt> {
        check_request_id(&request.request_id)?;
        markup::validate(&request.paragraphs)?;
        let workspace = Workspace::open(&self.root).map_err(|e| invalid(e.to_string()))?;
        let _guard = workspace
            .lock(LockScope::Workspace)
            .map_err(|e| invalid(e.to_string()))?;
        workspace
            .ensure_writable()
            .map_err(|e| invalid(e.to_string()))?;
        let file = self.draft_file(project, block)?;
        let current = file.load()?;
        if let Some(stored) = &current {
            stored.value.check(project, block)?;
        }
        let hash = sha256_hex(&serde_json::to_vec(&(project, block, request))?);
        if let Some(saved) = current
            .as_ref()
            .and_then(|s| s.value.requests.get(&request.request_id))
        {
            return if saved.hash == hash {
                Ok(saved.receipt.clone())
            } else {
                Err(TranslationError::DraftRequestConflict)
            };
        }
        let revision = current.as_ref().map(|s| s.value.draft.revision.clone());
        if request.expected_revision != revision {
            return Err(TranslationError::DraftConflict { current: revision });
        }
        if request.basis != self.draft_basis(project, block)? {
            return Err(TranslationError::DraftBasisChanged);
        }
        let p = self.project(project)?;
        let (_, _, source) = find_block(&p, block)?;
        let mut previous = None;
        for paragraph in &request.paragraphs {
            let rank = source
                .paragraphs
                .iter()
                .position(|p| p.id == paragraph.id)
                .ok_or_else(|| invalid(format!("块 {block} 没有段 {}", paragraph.id)))?;
            if previous.is_some_and(|prev| rank <= prev) {
                return Err(invalid("草稿段 ID 不得重复或乱序"));
            }
            previous = Some(rank);
        }
        let mut random = [0u8; 16];
        SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| invalid("无法生成草稿修订身份"))?;
        let revision = random.iter().map(|b| format!("{b:02x}")).collect();
        let draft = HumanDraft {
            project: project.into(),
            block: block.into(),
            revision,
            basis: request.basis.clone(),
            paragraphs: request.paragraphs.clone(),
        };
        let receipt = DraftReceipt {
            revision: draft.revision.clone(),
            content_hash: sha256_hex(&serde_json::to_vec(&draft)?),
        };
        let (mut requests, submissions) = current
            .map(|s| (s.value.requests, s.value.submissions))
            .unwrap_or_default();
        requests.insert(
            request.request_id.clone(),
            SavedRequest {
                hash,
                receipt: receipt.clone(),
            },
        );
        // Workspace exclusion covers load/check/update. JsonFile keeps the previous
        // publication on error; text and deduplication do not need multi-file recovery.
        file.update::<_, TranslationError>(|_| {
            if request.basis != self.draft_basis(project, block)? {
                return Err(TranslationError::DraftBasisChanged);
            }
            Ok((
                DraftBook {
                    draft,
                    requests,
                    submissions,
                },
                (),
            ))
        })?;
        Ok(receipt)
    }

    /// Freeze a complete, structurally valid draft for later review. Does not
    /// emit court events, change generation/zh.json, or confer approval eligibility.
    pub fn submit_human_draft(
        &self,
        project: &str,
        block: &str,
        request: &DraftSubmit,
    ) -> Result<HumanSubmission> {
        check_request_id(&request.request_id)?;
        let workspace = Workspace::open(&self.root).map_err(|e| invalid(e.to_string()))?;
        let _guard = workspace
            .lock(LockScope::Workspace)
            .map_err(|e| invalid(e.to_string()))?;
        workspace
            .ensure_writable()
            .map_err(|e| invalid(e.to_string()))?;
        let file = self.draft_file(project, block)?;
        let mut book = file
            .load()?
            .ok_or_else(|| invalid("尚无可提交的人工草稿"))?
            .value;
        book.check(project, block)?;
        if let Some(saved) = book.submissions.get(&request.request_id) {
            return if saved.draft.revision == request.expected_revision {
                Ok(saved.clone())
            } else {
                Err(TranslationError::DraftRequestConflict)
            };
        }
        if book.draft.revision != request.expected_revision {
            return Err(TranslationError::DraftConflict {
                current: Some(book.draft.revision),
            });
        }
        if book.draft.basis != self.draft_basis(project, block)? {
            return Err(TranslationError::DraftBasisChanged);
        }
        let mut submission = HumanSubmission {
            request_id: request.request_id.clone(),
            draft: book.draft.clone(),
            candidate_hash: String::new(),
        };
        let candidate = submission.candidate()?;
        let bytes = candidate.serialized()?;
        let report =
            self.texts()
                .check(project, block, &candidate.text_structure().serialized()?)?;
        if !report.valid {
            return Err(invalid(format!(
                "人工提交结构检查未通过：{:?}",
                report.issues
            )));
        }
        submission.candidate_hash = sha256_hex(&bytes);
        book.submissions
            .insert(request.request_id.clone(), submission.clone());
        file.update::<_, TranslationError>(|_| {
            if book.draft.basis != self.draft_basis(project, block)? {
                return Err(TranslationError::DraftBasisChanged);
            }
            Ok((book, ()))
        })?;
        Ok(submission)
    }

    /// Includes stale historical snapshots; reads never upgrade or reset storage.
    pub fn human_submissions(&self, project: &str, block: &str) -> Result<Vec<HumanSubmission>> {
        let workspace = Workspace::open(&self.root).map_err(|e| invalid(e.to_string()))?;
        let _guard = workspace
            .lock(LockScope::Workspace)
            .map_err(|e| invalid(e.to_string()))?;
        workspace
            .ensure_writable()
            .map_err(|e| invalid(e.to_string()))?;
        let Some(stored) = self.draft_file(project, block)?.load()? else {
            return Ok(Vec::new());
        };
        stored.value.check(project, block)?;
        Ok(stored.value.submissions.into_values().collect())
    }
}
