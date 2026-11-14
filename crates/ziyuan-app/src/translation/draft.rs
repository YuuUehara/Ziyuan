//! Local human drafts and immutable review submissions, never approved writes.
//! Text, submissions and durable retry receipts share one atomic JSON publication.
use super::versions::VersionState;
use super::{
    Result, TargetMark, TargetMarkKind, TranslationError, Translations, check_plain_dirs,
    ensure_plain_dirs, find_block, invalid, markup, sha256_hex,
};
use crate::text_project::{BlockParagraph, IssueKind, Project, PromptBlock, ValidationIssue};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};
use ziyuan_storage::recovery::{
    ApplicationV2, EventBatch, FileState, HumanApplication, RecoveryRecordV2, TargetState,
};
use ziyuan_storage::{
    EventLog, EventStore, JsonFile, LockScope, RecordState, Workspace, WriteGuard,
};

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

/// Full-block replacement. Local callers are trusted; remote entry points
/// must authenticate write access before calling this API.
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
    /// 只读诊断：受限 HTML 的 Markdown 能表达 ruby、粗体与斜体。
    MarkdownHtml,
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

    pub(crate) fn validate(&self) -> Result<()> {
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
                for (mark_index, mark) in paragraph.marks.iter().enumerate() {
                    let expressible =
                        match format {
                            ReviewFormat::MarkdownHtml => {
                                matches!(
                                    mark.kind,
                                    TargetMarkKind::Ruby { .. }
                                        | TargetMarkKind::Strong {}
                                        | TargetMarkKind::Emphasis {}
                                ) && !paragraph.marks.iter().enumerate().any(
                                    |(other_index, other)| {
                                        other_index != mark_index
                                            && other.start < mark.end
                                            && mark.start < other.end
                                    },
                                )
                            }
                            _ => false,
                        };
                    if !expressible {
                        unsupported_marks.push(UnsupportedTargetMark {
                            paragraph_id: paragraph.id.clone(),
                            mark_index,
                        });
                    }
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
    pub(crate) fn text_structure(&self) -> PromptBlock {
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
    /// Local-only admission under the caller's matching workspace lock.
    /// Returns one pending event, or None for an identical durable receipt.
    /// Does not append, write translation bytes, begin a generation or approve.
    /// Use `publish_human_binding` to publish; never append this result directly.
    pub fn admit_human_binding(
        &self,
        guard: &WriteGuard,
        binding: &ziyuan_core::HumanSubmissionBinding,
    ) -> Result<Option<ziyuan_core::CourtEvent>> {
        self.check_human_binding(guard, binding, ziyuan_core::HumanActor::Local)
            .map(|(_, event)| event)
    }

    /// Publish only a pending binding, never an approval or translation file.
    /// Keep the matching workspace lock throughout; a log conflict restarts ALL
    /// admission checks rather than carrying a stale proposed event forward.
    /// Returns false for an identical durable receipt (including lost responses).
    pub fn publish_human_binding(
        &self,
        guard: &WriteGuard,
        binding: &ziyuan_core::HumanSubmissionBinding,
    ) -> Result<bool> {
        self.publish_human_binding_as(guard, binding, ziyuan_core::HumanActor::Local)
    }

    /// Authenticated Web entry only: the caller must enforce write-token access.
    pub fn publish_web_human_binding(
        &self,
        guard: &WriteGuard,
        binding: &ziyuan_core::HumanSubmissionBinding,
    ) -> Result<bool> {
        self.publish_human_binding_as(guard, binding, ziyuan_core::HumanActor::Web)
    }

    fn publish_human_binding_as(
        &self,
        guard: &WriteGuard,
        binding: &ziyuan_core::HumanSubmissionBinding,
        actor: ziyuan_core::HumanActor,
    ) -> Result<bool> {
        loop {
            let (history, event) = self.check_human_binding(guard, binding, actor)?;
            let Some(event) = event else {
                return Ok(false);
            };
            let mut events = Vec::new();
            let at = crate::time::now();
            if crate::time::last_clock(&history, None).is_none_or(|t| t / 60 != at / 60) {
                events.push(ziyuan_core::CourtEvent::Clock(at));
            }
            events.push(event);
            match EventLog::new(guard.workspace().root()).append_if_unchanged(&history, &events) {
                Ok(()) => return Ok(true),
                Err(
                    ziyuan_storage::StorageError::Conflict { .. }
                    | ziyuan_storage::StorageError::PrefixChanged,
                ) => continue,
                Err(e) => return Err(invalid(e.to_string())),
            }
        }
    }

    fn check_human_binding(
        &self,
        guard: &WriteGuard,
        binding: &ziyuan_core::HumanSubmissionBinding,
        actor: ziyuan_core::HumanActor,
    ) -> Result<(
        Vec<ziyuan_core::CourtEvent>,
        Option<ziyuan_core::CourtEvent>,
    )> {
        let workspace = guard.workspace();
        if fs::canonicalize(&self.root)? != fs::canonicalize(workspace.root())? {
            return Err(invalid("人工送审与所持写锁不属同一工作区"));
        }
        workspace
            .ensure_writable()
            .map_err(|e| invalid(e.to_string()))?;
        if workspace
            .write_protocol()
            .map_err(|e| invalid(e.to_string()))?
            != Some(2)
            || workspace
                .pending_record()
                .map_err(|e| invalid(e.to_string()))?
                .is_some()
        {
            return Err(invalid(
                "人工送审准入须先完成工作区换版，并收尾全部恢复记录",
            ));
        }
        let r = &binding.request;
        // The trusted entry point, never the payload, determines the channel.
        if r.actor != actor {
            return Err(invalid("人工送审来源与已认证入口不符"));
        }
        if self.texts().project_protocol(&r.project)? != 3 {
            return Err(invalid("人工送审准入须先为项目清单换版"));
        }
        let project = self.project(&r.project)?;
        find_block(&project, &r.block)?;
        let candidate = ReviewCandidate::from_binding(binding)?;
        let history = EventLog::new(workspace.root())
            .load()
            .map_err(|e| invalid(e.to_string()))?;
        let court =
            ziyuan_governance::Court::replay(&history).map_err(|e| invalid(e.to_string()))?;
        if let Some(saved) = court.human_submission_binding(&r.project, &r.block, &r.request_id) {
            return if saved == binding {
                Ok((history, None))
            } else {
                Err(TranslationError::DraftRequestConflict)
            };
        }
        let book = self
            .draft_file_in(&project, &r.block)?
            .load()?
            .ok_or_else(|| invalid("尚无可核对的人工送审快照"))?
            .value;
        book.check(&r.project, &r.block)?;
        let submission = book
            .submissions
            .get(&r.submission_id)
            .ok_or_else(|| invalid("人工送审快照不存在"))?;
        if submission.prepare_binding(r.request_id.clone(), r.actor, r.previous.clone())?
            != *binding
        {
            return Err(invalid("人工送审绑定与原提交不符"));
        }
        let report = self.texts().check_in(
            &project,
            &r.block,
            &candidate.text_structure().serialized()?,
        )?;
        if !report.valid {
            return Err(invalid(format_validation_issues(&report.issues)));
        }
        if self.draft_basis_in(&project, &r.block)? != r.basis {
            return Err(TranslationError::DraftBasisChanged);
        }
        let source = super::resolve_translation_source(&history, &r.project, &r.block)?;
        if source.source != r.previous || source.translation_hash != r.basis.translation_hash {
            return Err(invalid("人工送审的前一准奏来源或盘上译稿不符，请重新核对"));
        }
        let event = court
            .propose_human_submission_binding(binding.clone())
            .map_err(|e| invalid(e.to_string()))?;
        Ok((history, Some(event)))
    }

    fn draft_file(&self, project: &str, block: &str) -> Result<JsonFile<DraftBook>> {
        let p = self.project(project)?;
        self.draft_file_in(&p, block)
    }

    fn draft_file_in(&self, project: &Project, block: &str) -> Result<JsonFile<DraftBook>> {
        self.draft_file_with_dirs_in(project, block, true)
    }

    fn draft_file_with_dirs_in(
        &self,
        project: &Project,
        block: &str,
        create: bool,
    ) -> Result<JsonFile<DraftBook>> {
        // Membership is checked before using either identifier as a path component.
        find_block(project, block)?;
        let dirs = if create {
            ensure_plain_dirs
        } else {
            check_plain_dirs
        };
        let dir = dirs(
            &self.root,
            &[".ziyuan", "text-projects", &project.id, "human-drafts"],
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
        self.draft_basis_in(&p, block)
    }

    fn draft_basis_in(&self, p: &Project, block: &str) -> Result<DraftBasis> {
        let project_hash = sha256_hex(&serde_json::to_vec(p)?);
        self.draft_basis_with_hash_in(p, &project_hash, block)
    }

    fn draft_basis_with_hash_in(
        &self,
        p: &Project,
        project_hash: &str,
        block: &str,
    ) -> Result<DraftBasis> {
        let (_, _, source) = find_block(p, block)?;
        Ok(DraftBasis {
            project_hash: project_hash.to_owned(),
            source_hash: sha256_hex(&source.serialized()?),
            context_version: self.context_version(&p.id)?,
            zhupi_hash: self.block_zhupi_hash(p, block)?,
            translation_hash: self
                .read_translation(&p.id, block)?
                .map(|bytes| sha256_hex(&bytes)),
        })
    }

    /// Read-only view of the current human draft; never locks, writes or upgrades.
    pub fn peek_human_draft(&self, project: &str, block: &str) -> Result<DraftView> {
        let p = self.project(project)?;
        self.peek_human_draft_in(&p, block)
    }

    /// Read-only view of the current human draft against an already-validated [`Project`].
    pub fn peek_human_draft_in(&self, p: &Project, block: &str) -> Result<DraftView> {
        let basis = self.draft_basis_in(p, block)?;
        let draft = self.load_human_draft_in(p, block, false)?;
        Ok(DraftView { basis, draft })
    }

    pub(crate) fn peek_human_draft_with_hash_in(
        &self,
        p: &Project,
        project_hash: &str,
        block: &str,
    ) -> Result<DraftView> {
        let basis = self.draft_basis_with_hash_in(p, project_hash, block)?;
        let draft = self.load_human_draft_in(p, block, false)?;
        Ok(DraftView { basis, draft })
    }

    fn load_human_draft_in(
        &self,
        p: &Project,
        block: &str,
        create: bool,
    ) -> Result<Option<HumanDraft>> {
        self.draft_file_with_dirs_in(p, block, create)?
            .load()?
            .map(|s| {
                s.value.check(&p.id, block)?;
                Ok::<_, TranslationError>(s.value.draft)
            })
            .transpose()
    }

    /// Read-only progress check using the same typed file and path validation as the editor.
    /// A saved draft counts as started once any paragraph contains non-whitespace text.
    pub fn human_draft_written(&self, project: &str, block: &str) -> Result<bool> {
        let p = self.project(project)?;
        self.human_draft_written_in(&p, block)
    }

    /// Read-only progress check against an already-validated [`Project`].
    pub fn human_draft_written_in(&self, p: &Project, block: &str) -> Result<bool> {
        Ok(self
            .load_human_draft_in(p, block, false)?
            .is_some_and(|draft| draft.paragraphs.iter().any(|p| !p.text.trim().is_empty())))
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
        let p = self.project(project)?;
        let basis = self.draft_basis_in(&p, block)?;
        let draft = self.load_human_draft_in(&p, block, true)?;
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
        let p = self.project(project)?;
        let (_, _, source) = find_block(&p, block)?;
        let file = self.draft_file_in(&p, block)?;
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
        if request.basis != self.draft_basis_in(&p, block)? {
            return Err(TranslationError::DraftBasisChanged);
        }
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
            if request.basis != self.draft_basis_in(&p, block)? {
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
        // 自动快照是辅助材料：失败不撤销这次已成功的保存。
        let _ = self.snapshot_state_in(&p, block, VersionState::Draft);
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
        let p = self.project(project)?;
        let file = self.draft_file_in(&p, block)?;
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
        if book.draft.basis != self.draft_basis_in(&p, block)? {
            return Err(TranslationError::DraftBasisChanged);
        }
        let mut submission = HumanSubmission {
            request_id: request.request_id.clone(),
            draft: book.draft.clone(),
            candidate_hash: String::new(),
        };
        let candidate = submission.candidate()?;
        let bytes = candidate.serialized()?;
        let report = self
            .texts()
            .check_in(&p, block, &candidate.text_structure().serialized()?)?;
        if !report.valid {
            return Err(invalid(format_validation_issues(&report.issues)));
        }
        submission.candidate_hash = sha256_hex(&bytes);
        book.submissions
            .insert(request.request_id.clone(), submission.clone());
        file.update::<_, TranslationError>(|_| {
            if book.draft.basis != self.draft_basis_in(&p, block)? {
                return Err(TranslationError::DraftBasisChanged);
            }
            Ok((book, ()))
        })?;
        Ok(submission)
    }

    /// Freeze the current draft and prepare a binding under a workspace lock the caller holds.
    /// Writes the submission snapshot only; publishing the binding is a separate step.
    pub fn prepare_human_submission(
        &self,
        project: &str,
        block: &str,
        request_id: &str,
        binding_id: &str,
        expected_revision: &str,
    ) -> Result<ziyuan_core::HumanSubmissionBinding> {
        self.prepare_human_submission_as(
            project,
            block,
            request_id,
            binding_id,
            expected_revision,
            ziyuan_core::HumanActor::Local,
        )
    }

    /// Authenticated Web entry only: the caller must enforce write-token access.
    pub fn prepare_web_human_submission(
        &self,
        project: &str,
        block: &str,
        request_id: &str,
        binding_id: &str,
        expected_revision: &str,
    ) -> Result<ziyuan_core::HumanSubmissionBinding> {
        self.prepare_human_submission_as(
            project,
            block,
            request_id,
            binding_id,
            expected_revision,
            ziyuan_core::HumanActor::Web,
        )
    }

    fn prepare_human_submission_as(
        &self,
        project: &str,
        block: &str,
        request_id: &str,
        binding_id: &str,
        expected_revision: &str,
        actor: ziyuan_core::HumanActor,
    ) -> Result<ziyuan_core::HumanSubmissionBinding> {
        let submission = self.submit_human_draft(
            project,
            block,
            &DraftSubmit {
                request_id: request_id.into(),
                expected_revision: expected_revision.into(),
            },
        )?;
        let history = EventLog::new(&self.root)
            .load()
            .map_err(|e| invalid(e.to_string()))?;
        if history.is_empty() {
            return Err(invalid(
                "人工送审需要先开国（ziyuan init），以便事件簿记录送审与批准事实",
            ));
        }
        let previous = super::resolve_translation_source(&history, project, block)?.source;
        submission.prepare_binding(binding_id.into(), actor, previous)
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

impl Translations {
    /// Human finalization under the caller's matching lock; remote callers authenticate writes.
    /// Appends only the finalization fact; never writes zh.json or a model task.
    pub fn finalize_human_translation(
        &self,
        guard: &WriteGuard,
        project: &str,
        block: &str,
    ) -> Result<bool> {
        loop {
            let workspace = guard.workspace();
            if fs::canonicalize(&self.root)? != fs::canonicalize(workspace.root())? {
                return Err(invalid("人工定稿与所持写锁不属同一工作区"));
            }
            workspace
                .ensure_writable()
                .map_err(|e| invalid(e.to_string()))?;
            if workspace
                .write_protocol()
                .map_err(|e| invalid(e.to_string()))?
                != Some(2)
                || self.texts().project_protocol(project)? != 3
                || workspace
                    .pending_record_any()
                    .map_err(|e| invalid(e.to_string()))?
                    .is_some()
            {
                return Err(invalid("人工定稿须先完成工作区换版，并收尾全部恢复记录"));
            }
            let history = EventLog::new(workspace.root())
                .load()
                .map_err(|e| invalid(e.to_string()))?;
            let court =
                ziyuan_governance::Court::replay(&history).map_err(|e| invalid(e.to_string()))?;
            let resolved = super::resolve_translation_source(&history, project, block)?;
            let ziyuan_core::TranslationSource::Human {
                operation_id,
                event,
            } = resolved.source
            else {
                return Err(invalid("当前批准来源不是人工准奏，不能人工定稿"));
            };
            // Only this approval's own finalization is idempotent. Model signoffs and
            // earlier human rounds give way: the current source is this human approval,
            // the same rule status applies.
            if let Some(ziyuan_governance::Signoff::HumanFinalized {
                operation_id: signed,
                ..
            }) = court.translation_signoff(project, block)
                && *signed == operation_id
            {
                return Ok(false);
            }
            let index = usize::try_from(event.index).map_err(|_| invalid("批准事件位置不合法"))?;
            let Some(ziyuan_core::CourtEvent::HumanTranslationApproved { approval }) =
                history.get(index)
            else {
                return Err(invalid("批准事件不在事件簿的申报位置"));
            };
            if approval.operation_id != operation_id {
                return Err(invalid("批准事件与当前来源不符"));
            }
            if court.translation_tasks(project, block).any(|t| {
                t.status.is_open()
                    || t.changes
                        .as_ref()
                        .is_some_and(|c| c.state == ziyuan_governance::ChangeState::Proposed)
            }) {
                return Err(invalid("该块仍有活动或待决的模型任务，人工定稿须先收尾"));
            }
            let project_data = self.project(project)?;
            let source_hash = super::block_source_hash(&project_data, block)?;
            let context_version = self.context_version(project)?;
            let zhupi_hash = self.block_zhupi_hash(&project_data, block)?;
            let bytes = self
                .read_translation(project, block)?
                .ok_or_else(|| invalid("磁盘上没有 zh.json"))?;
            let target = super::read_approved_target(&bytes, block)?;
            let target_hash = sha256_hex(&bytes);
            if target.format() != approval.request.target.format
                || target_hash != approval.request.target.after
            {
                return Err(invalid("磁盘译稿与当前人工批准的目标不符"));
            }
            let basis = &court
                .human_submission_binding(project, block, &approval.request.submission_id)
                .ok_or_else(|| invalid("人工批准引用的送审绑定不存在"))?
                .request
                .basis;
            let reasons = super::status::human_basis_reasons(
                &super::BlockFacts {
                    block: block.into(),
                    chapter: String::new(),
                    source_hash: source_hash.clone(),
                    disk_hash: None,
                    zhupi_hash: zhupi_hash.clone(),
                },
                &super::ContextFacts {
                    current: context_version,
                    ..Default::default()
                },
                &basis.source_hash,
                basis.context_version,
                &basis.zhupi_hash,
            );
            if !reasons.is_empty() {
                return Err(invalid(format!(
                    "送审后材料有变（{}），人工定稿未入簿。\n下一步：重新保存草稿后运行 ziyuan text human submit {project} {block}，再准奏、定稿",
                    reasons
                        .iter()
                        .map(|r| r.describe())
                        .collect::<Vec<_>>()
                        .join("；")
                )));
            }
            let finalization = ziyuan_core::HumanFinalization {
                project: project.into(),
                block: block.into(),
                approval_request_id: approval.request.request_id.clone(),
                approval_operation_id: approval.operation_id.clone(),
                approval_event: event,
                submission_id: approval.request.submission_id.clone(),
                candidate_hash: court
                    .human_submission_binding(project, block, &approval.request.submission_id)
                    .ok_or_else(|| invalid("人工批准引用的送审绑定不存在"))?
                    .request
                    .candidate_hash
                    .clone(),
                source_hash,
                context_version,
                zhupi_hash,
                target_format: target.format().into(),
                target_hash,
            };
            let mut events = Vec::new();
            let at = crate::time::now();
            if crate::time::last_clock(&history, None).is_none_or(|t| t / 60 != at / 60) {
                events.push(ziyuan_core::CourtEvent::Clock(at));
            }
            events.push(
                court
                    .propose_human_translation_finalized(finalization)
                    .map_err(|e| invalid(e.to_string()))?,
            );
            match EventLog::new(workspace.root()).append_if_unchanged(&history, &events) {
                Ok(()) => {
                    // 自动快照是辅助材料：失败不撤销这次已完成的定稿事实。
                    let _ = self.auto_snapshot(project, block, VersionState::Finalized);
                    return Ok(true);
                }
                Err(
                    ziyuan_storage::StorageError::Conflict { .. }
                    | ziyuan_storage::StorageError::PrefixChanged,
                ) => continue,
                Err(e) => return Err(invalid(e.to_string())),
            }
        }
    }
}

/// Debug-only interruption points for real child-process tests. Release builds
/// compile them to nothing and never read the variable.
#[cfg(debug_assertions)]
fn interrupt(point: &str) {
    if std::env::var("ZIYUAN_INTERRUPT_RECOVERY").ok().as_deref() == Some(point) {
        std::process::abort();
    }
}

#[cfg(not(debug_assertions))]
fn interrupt(_point: &str) {}

/// Submission and binding request ID for one draft revision: each new revision is a
/// new review round, and retrying the same revision maps to the same receipts.
pub fn human_submit_request_id(revision: &str) -> String {
    format!("submit-r{revision}")
}

/// Approval request ID for one published binding.
pub fn human_approve_request_id(binding: &str) -> String {
    format!("approve-{binding}")
}

/// The recovery codec cannot represent the Windows verbatim prefix.
fn record_path(root: &Path) -> PathBuf {
    match root.to_string_lossy().strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => root.to_path_buf(),
    }
}

/// After publication nothing is retried or undone: the record is the evidence.
fn kept_scene(operation_id: &str, what: impl std::fmt::Display) -> TranslationError {
    invalid(format!(
        "{what}。\n恢复记录 {operation_id} 仍在盘上，现场已原样保留，未重试也未回滚。\n\
         请运行 ziyuan text recovery status 核对，再用 ziyuan text recovery finish {operation_id} --yes 收尾，\
         或在目标仍为旧内容时用 ziyuan text recovery abandon {operation_id} --yes 放弃。"
    ))
}

/// Replace `dir/zh.json` through a synced same-directory temporary file.
fn write_target_durable(dir: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = dir.join(format!(".zh.json.tmp-{}-{nanos}", std::process::id()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, dir.join("zh.json"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    // Persist the rename itself. std cannot open a directory handle for syncing on
    // Windows, where NTFS journals the rename metadata; there it is skipped.
    #[cfg(unix)]
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}

/// The block's most recent published binding, by log order.
fn latest_binding<'a>(
    history: &'a [ziyuan_core::CourtEvent],
    project: &str,
    block: &str,
) -> Option<(usize, &'a ziyuan_core::HumanSubmissionBinding)> {
    history
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, event)| match event {
            ziyuan_core::CourtEvent::HumanSubmissionBound { binding }
                if binding.request.project == project && binding.request.block == block =>
            {
                Some((index, binding.as_ref()))
            }
            _ => None,
        })
}

impl Translations {
    fn check_human_write(&self, workspace: &Workspace, project: &str, what: &str) -> Result<()> {
        if fs::canonicalize(&self.root)? != fs::canonicalize(workspace.root())? {
            return Err(invalid(format!("{what}与所持写锁不属同一工作区")));
        }
        workspace
            .ensure_writable()
            .map_err(|e| invalid(e.to_string()))?;
        if workspace
            .write_protocol()
            .map_err(|e| invalid(e.to_string()))?
            != Some(2)
            || self.texts().project_protocol(project)? != 3
            || workspace
                .pending_record_any()
                .map_err(|e| invalid(e.to_string()))?
                .is_some()
        {
            return Err(invalid(format!(
                "{what}须先完成工作区换版，并收尾全部恢复记录（ziyuan text recovery status）"
            )));
        }
        Ok(())
    }

    /// Approve the block's latest published binding and land its target in one managed
    /// transaction under the caller's workspace lock. Every check and the fixed record
    /// and batch come first; then: reserve odd generation → publish record → write
    /// zh.json → append batch → Completed → even generation → remove record. A failure
    /// before publication changes nothing; any failure after it keeps the scene for
    /// `ziyuan text recovery` and is never retried or rolled back.
    /// Returns false when this round's approval is already in the log.
    pub fn approve_human_submission(
        &self,
        guard: &mut WriteGuard,
        project: &str,
        block: &str,
    ) -> Result<bool> {
        self.approve_human_submission_as(guard, project, block, ziyuan_core::HumanActor::Local)
    }

    /// Authenticated Web entry only: the caller must enforce write-token access.
    pub fn approve_web_human_submission(
        &self,
        guard: &mut WriteGuard,
        project: &str,
        block: &str,
    ) -> Result<bool> {
        self.approve_human_submission_as(guard, project, block, ziyuan_core::HumanActor::Web)
    }

    fn approve_human_submission_as(
        &self,
        guard: &mut WriteGuard,
        project: &str,
        block: &str,
        actor: ziyuan_core::HumanActor,
    ) -> Result<bool> {
        let workspace = guard.workspace().clone();
        self.check_human_write(&workspace, project, "人工准奏")?;
        let log = EventLog::new(workspace.root());
        let history = log.load().map_err(|e| invalid(e.to_string()))?;
        let court =
            ziyuan_governance::Court::replay(&history).map_err(|e| invalid(e.to_string()))?;
        let (index, binding) = latest_binding(&history, project, block).ok_or_else(|| {
            invalid(format!(
                "该块尚未送审：请先运行 ziyuan text human submit {project} {block}"
            ))
        })?;
        let binding_id = binding.request.request_id.clone();
        let request_id = human_approve_request_id(&binding_id);
        if court.human_approval(project, block, &request_id).is_some() {
            return Ok(false);
        }
        if court.human_submission_binding(project, block, &binding_id) != Some(binding) {
            return Err(invalid(
                "送审绑定事件与回放结果不符，请运行 ziyuan text recovery status 核对",
            ));
        }
        let binding_event = ziyuan_storage::event_reference(index as u64, &history[index]);
        let current = super::resolve_translation_source(&history, project, block)?.source;
        if current != binding.request.previous {
            return Err(invalid(format!(
                "送审后当前来源已变（已有新的准奏），这份送审不能再准奏。\n下一步：重新保存草稿后运行 ziyuan text human submit {project} {block}"
            )));
        }
        let before = self
            .read_translation(project, block)?
            .map(|bytes| sha256_hex(&bytes));
        if before != binding.request.basis.translation_hash {
            return Err(invalid(format!(
                "磁盘上的 zh.json 自送审后已被改动，本次未写任何文件。\n下一步：核对 {} 后重新保存草稿并运行 ziyuan text human submit {project} {block}",
                super::output_rel(project, block)
            )));
        }
        if self.draft_basis(project, block)? != binding.request.basis {
            return Err(invalid(format!(
                "送审后原文、上下文或朱批已变，本次未写任何文件。\n下一步：重新保存草稿并运行 ziyuan text human submit {project} {block}"
            )));
        }
        let candidate = ReviewCandidate::from_binding(binding)?;
        let report =
            self.texts()
                .check(project, block, &candidate.text_structure().serialized()?)?;
        if !report.valid {
            return Err(invalid(format_validation_issues(&report.issues)));
        }
        // The target, not the candidate, decides format and bytes: an unmarked v2
        // candidate lands as the first format, which finalization can read back.
        let target = candidate.approved_target()?;
        let bytes = target.bytes()?;
        if super::read_approved_target(&bytes, block)?.format() != target.format() {
            return Err(invalid("批准目标的格式与写出字节不一致"));
        }
        let after = sha256_hex(&bytes);
        let path = super::output_rel(project, block);
        let request = ziyuan_core::HumanApprovalRequest {
            project: project.into(),
            block: block.into(),
            request_id: request_id.clone(),
            actor,
            submission_id: binding_id,
            binding: binding_event,
            previous: binding.request.previous.clone(),
            target: ziyuan_core::HumanApprovalTarget {
                format: target.format().into(),
                path: path.clone(),
                before: before.clone(),
                after: after.clone(),
            },
        };
        let request_hash = ziyuan_governance::human_approval_request_hash(&request);
        let operation_id =
            sha256_hex(format!("{request_hash}:{request_id}").as_bytes())[..32].to_owned();
        let approval = ziyuan_core::HumanApproval {
            request_hash: request_hash.clone(),
            request,
            operation_id: operation_id.clone(),
        };
        let mut events = Vec::new();
        let at = crate::time::now();
        if crate::time::last_clock(&history, None).is_none_or(|t| t / 60 != at / 60) {
            events.push(ziyuan_core::CourtEvent::Clock(at));
        }
        events.push(
            court
                .propose_human_translation_approved(approval)
                .map_err(|e| invalid(e.to_string()))?,
        );
        let expected_bytes = log
            .history_bytes()
            .map_err(|e| invalid(e.to_string()))?
            .unwrap_or(0);
        let raw_bytes = match fs::metadata(workspace.root().join(".ziyuan").join("court.log")) {
            Ok(meta) => meta.len(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e.into()),
        };
        if raw_bytes != expected_bytes {
            return Err(invalid(
                "事件簿末尾有未写完的残行，本次未写任何文件；请先核对 .ziyuan/court.log",
            ));
        }
        let history_sha256 = log
            .history_sha256(history.len())
            .map_err(|e| invalid(e.to_string()))?
            .ok_or_else(|| invalid("事件簿报不出前缀摘要"))?;
        let generation = workspace.generation().map_err(|e| invalid(e.to_string()))?;
        let targets = if before.as_deref() == Some(after.as_str()) {
            Vec::new()
        } else {
            vec![TargetState {
                path: path.clone(),
                before: before.clone().map_or(FileState::Absent, FileState::Present),
                after: FileState::Present(after.clone()),
            }]
        };
        let record = RecoveryRecordV2 {
            operation_id: operation_id.clone(),
            workspace: record_path(workspace.root()),
            generation_before: generation,
            generation_after: generation + 2,
            state: RecordState::Published,
            application: ApplicationV2::Human(HumanApplication {
                project: project.into(),
                block: block.into(),
                approval_request_id: request_id,
                approval_request_hash: request_hash,
                previous: binding.request.previous.clone(),
                target_format: target.format().into(),
                logical_path: path,
                before,
                after: after.clone(),
            }),
            events: EventBatch {
                expected_events: (history.len() + events.len()) as u64,
                expected_bytes,
                history_sha256,
                encoded_events: events
                    .iter()
                    .map(ziyuan_storage::encode_event_line)
                    .collect(),
            },
            targets,
        };
        // Size, shape and branch validation happen before anything is reserved.
        ziyuan_storage::recovery::encode_record_v2(&record)
            .map_err(|e| invalid(format!("人工准奏的恢复记录不合规：{e}")))?;

        guard.reserve().map_err(|e| invalid(e.to_string()))?;
        if let Err(error) = guard.publish_second_format(&record) {
            if !matches!(workspace.pending_record_any(), Ok(None)) {
                return Err(kept_scene(
                    &operation_id,
                    format!("发布恢复记录时出错（{error}），盘上可能留有记录"),
                ));
            }
            // Nothing was published, so the reservation can be closed like the model path.
            let _ = guard.finish();
            return Err(invalid(format!(
                "无法发布恢复记录（{error}）：译稿与事件簿均未改动，可直接重试"
            )));
        }
        interrupt("approve-after-publish");
        if record.targets.is_empty() {
            let now = self
                .read_translation(project, block)
                .map_err(|e| kept_scene(&operation_id, format!("无法核对同字节目标：{e}")))?;
            if now.map(|bytes| sha256_hex(&bytes)).as_deref() != Some(after.as_str()) {
                return Err(kept_scene(&operation_id, "同字节准奏的目标在核对后被改动"));
            }
        } else {
            let dir = ensure_plain_dirs(&self.root, &["translations", project, block])
                .map_err(|e| kept_scene(&operation_id, format!("无法准备译稿目录：{e}")))?;
            write_target_durable(&dir, &bytes)
                .map_err(|e| kept_scene(&operation_id, format!("写入 zh.json 失败：{e}")))?;
        }
        interrupt("approve-after-target");
        EventLog::new(workspace.root())
            .append_if_unchanged(&history, &events)
            .map_err(|e| kept_scene(&operation_id, format!("追加准奏事件失败（{e}）")))?;
        interrupt("approve-after-append");
        guard
            .complete_second_format(&operation_id)
            .map_err(|e| kept_scene(&operation_id, format!("标记恢复记录完成失败：{e}")))?;
        let finished = guard
            .finish()
            .map_err(|e| kept_scene(&operation_id, format!("推进代次失败：{e}")))?;
        if finished != record.generation_after {
            return Err(kept_scene(
                &operation_id,
                format!(
                    "代次推进到 {finished}，与记录的后代次 {} 不符",
                    record.generation_after
                ),
            ));
        }
        guard
            .remove_second_format()
            .map_err(|e| kept_scene(&operation_id, format!("清理恢复记录失败：{e}")))?;
        // 自动快照是辅助材料：失败不撤销这次已成功的准奏。
        let _ = self.auto_snapshot(project, block, VersionState::Approved);
        Ok(true)
    }

    /// Read-only confirmation for the legacy `apply` step: approval already lands the
    /// target. Checks that the current source is this block's human approval and that
    /// zh.json holds its bytes. Returns the approval request ID; never writes.
    pub fn apply_human_target(&self, project: &str, block: &str) -> Result<String> {
        find_block(&self.project(project)?, block)?;
        let workspace = Workspace::open(&self.root).map_err(|e| invalid(e.to_string()))?;
        if let Some(record) = workspace
            .pending_record_any()
            .map_err(|e| invalid(e.to_string()))?
        {
            let op = match record {
                ziyuan_storage::recovery::DecodedRecovery::V1(r) => r.operation_id,
                ziyuan_storage::recovery::DecodedRecovery::V2(r) => r.operation_id,
            };
            return Err(invalid(format!(
                "工作区仍有未收尾的恢复记录 {op}：请先运行 ziyuan text recovery status 核对并收尾"
            )));
        }
        let history = EventLog::new(&self.root)
            .load()
            .map_err(|e| invalid(e.to_string()))?;
        let resolved = super::resolve_translation_source(&history, project, block)?;
        let ziyuan_core::TranslationSource::Human {
            operation_id,
            event,
        } = resolved.source
        else {
            return Err(invalid(format!(
                "当前批准来源不是人工准奏，没有可核对的落盘。\n下一步：ziyuan text human approve {project} {block}"
            )));
        };
        let approval = usize::try_from(event.index)
            .ok()
            .and_then(|index| history.get(index))
            .and_then(|e| match e {
                ziyuan_core::CourtEvent::HumanTranslationApproved { approval }
                    if approval.operation_id == operation_id =>
                {
                    Some(approval)
                }
                _ => None,
            })
            .ok_or_else(|| invalid("人工准奏事件不在事件簿的申报位置"))?;
        let disk = self
            .read_translation(project, block)?
            .map(|bytes| sha256_hex(&bytes));
        if disk.as_deref() != Some(approval.request.target.after.as_str()) {
            return Err(invalid(format!(
                "磁盘上的 zh.json 与人工准奏落盘的字节不符（可能被外部改动）。\n\
                 下一步：核对 {} ；如需另起一轮，重新保存草稿后运行 ziyuan text human submit {project} {block}",
                approval.request.target.path
            )));
        }
        Ok(approval.request.request_id.clone())
    }

    /// Before `text recovery finish` appends a human batch whose target is already
    /// written, redo admission under the caller's lock: binding in the fixed prefix,
    /// unchanged predecessor and materials, frozen candidate matching the record and
    /// zh.json holding exactly `after`. Reads only.
    pub fn recheck_human_recovery(
        &self,
        guard: &WriteGuard,
        record: &RecoveryRecordV2,
    ) -> Result<()> {
        let ApplicationV2::Human(app) = &record.application else {
            return Err(invalid("这不是人工准奏的恢复记录"));
        };
        if fs::canonicalize(&self.root)? != fs::canonicalize(guard.workspace().root())? {
            return Err(invalid("复核与所持写锁不属同一工作区"));
        }
        let history = EventLog::new(guard.workspace().root())
            .load()
            .map_err(|e| invalid(e.to_string()))?;
        let prefix = record
            .events
            .expected_events
            .checked_sub(record.events.encoded_events.len() as u64)
            .ok_or_else(|| invalid("恢复记录的事件条数不足以容纳批次"))?;
        if history.len() as u64 != prefix {
            return Err(invalid("事件簿条数与记录的固定前缀不符"));
        }
        let approval = record
            .events
            .encoded_events
            .iter()
            .filter_map(|line| ziyuan_storage::decode_event_line(line).ok())
            .find_map(|event| match event {
                ziyuan_core::CourtEvent::HumanTranslationApproved { approval } => Some(approval),
                _ => None,
            })
            .ok_or_else(|| invalid("恢复记录里没有人工准奏事件"))?;
        let r = &approval.request;
        let binding = usize::try_from(r.binding.index)
            .ok()
            .and_then(|index| history.get(index).map(|event| (index, event)))
            .and_then(|(index, event)| match event {
                ziyuan_core::CourtEvent::HumanSubmissionBound { binding }
                    if binding.request.project == app.project
                        && binding.request.block == app.block
                        && binding.request.request_id == r.submission_id
                        && ziyuan_storage::event_reference(index as u64, event) == r.binding =>
                {
                    Some(binding.as_ref())
                }
                _ => None,
            })
            .ok_or_else(|| invalid("送审绑定不在记录声明的前缀位置，或与准奏引用不符"))?;
        let court =
            ziyuan_governance::Court::replay(&history).map_err(|e| invalid(e.to_string()))?;
        court
            .propose_human_translation_approved((*approval).clone())
            .map_err(|e| invalid(format!("准奏事件已不能入簿：{e}")))?;
        let current = super::resolve_translation_source(&history, &app.project, &app.block)?;
        if current.source != app.previous {
            return Err(invalid("当前来源已不是记录里的前一来源（已有新的准奏）"));
        }
        // The disk translation is expected to be this transaction's `after`; the frozen
        // basis must instead name the record's `before`.
        let mut basis = self.draft_basis(&app.project, &app.block)?;
        basis.translation_hash = binding.request.basis.translation_hash.clone();
        if basis != binding.request.basis {
            return Err(invalid("送审后原文、上下文或朱批已变"));
        }
        if binding.request.basis.translation_hash != app.before {
            return Err(invalid("送审时的旧译稿摘要与记录的准奏前摘要不符"));
        }
        let target = ReviewCandidate::from_binding(binding)?.approved_target()?;
        if target.format() != app.target_format || sha256_hex(&target.bytes()?) != app.after {
            return Err(invalid("冻结候选的批准目标与记录不符"));
        }
        let disk = self
            .read_translation(&app.project, &app.block)?
            .map(|bytes| sha256_hex(&bytes));
        if disk.as_deref() != Some(app.after.as_str()) {
            return Err(invalid("磁盘上的 zh.json 不是记录中的新内容"));
        }
        Ok(())
    }
}

impl Translations {
    /// Copy the current validated local materials draft into the production context draft.
    /// This only prepares the file; approval stays with `text context approve`.
    pub fn prepare_material_production(&self, guard: &WriteGuard, project: &str) -> Result<usize> {
        let workspace = guard.workspace();
        if fs::canonicalize(&self.root)? != fs::canonicalize(workspace.root())? {
            return Err(invalid("准备生产资料与所持写锁不属同一工作区"));
        }
        workspace
            .ensure_writable()
            .map_err(|e| invalid(e.to_string()))?;
        if workspace
            .pending_record_any()
            .map_err(|e| invalid(e.to_string()))?
            .is_some()
        {
            return Err(invalid("准备生产资料前须先收尾全部恢复记录"));
        }
        let draft = self
            .material_draft(project)
            .map_err(|e| invalid(e.to_string()))?
            .ok_or_else(|| invalid("尚无本地资料草稿，请先在本地资料编辑器保存"))?;
        let project_file = self.project(project)?;
        let glossary = super::Glossary::Structured(draft.material);
        glossary.validate_project(&project_file)?;
        let bytes = super::pretty_json(&glossary)?;
        let dir = super::ensure_plain_dirs(&self.root, &["translations", project, "context"])?;
        super::write_plain(&dir.join("glossary.json"), &bytes)?;
        Ok(bytes.len())
    }
}

fn parse_chapter_para(id: &str) -> Option<(usize, usize)> {
    let rest = id.strip_prefix('c')?;
    let (ch, p) = rest.split_once("-p")?;
    if ch.is_empty()
        || !ch.bytes().all(|b| b.is_ascii_digit())
        || p.is_empty()
        || !p.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some((ch.parse().ok()?, p.parse().ok()?))
}

fn format_paragraph_refs(refs: &[(Option<&str>, Option<usize>)]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut prev_chapter: Option<usize> = None;
    let mut i = 0usize;
    while i < refs.len() {
        let (id, pos) = refs[i];
        if let Some(id_str) = id {
            if let Some((ch, start_p)) = parse_chapter_para(id_str) {
                let mut end_p = start_p;
                while i + 1 < refs.len() {
                    if let Some((next_ch, next_p)) = refs[i + 1].0.and_then(parse_chapter_para)
                        && next_ch == ch
                        && next_p == end_p + 1
                    {
                        end_p = next_p;
                        i += 1;
                    } else {
                        break;
                    }
                }
                let prefix = if prev_chapter == Some(ch) {
                    String::new()
                } else {
                    prev_chapter = Some(ch);
                    format!("第{ch}章 · ")
                };
                if end_p > start_p {
                    parts.push(format!("{prefix}第{start_p}～{end_p}段"));
                } else {
                    parts.push(format!("{prefix}第{start_p}段"));
                }
            } else {
                prev_chapter = None;
                parts.push(id_str.to_owned());
            }
        } else if let Some(p) = pos {
            prev_chapter = None;
            parts.push(format!("第 {p} 项"));
        }
        i += 1;
    }
    const LIMIT: usize = 6;
    if parts.len() > LIMIT {
        format!("{} 等共 {} 处", parts[..LIMIT].join("、"), refs.len())
    } else {
        parts.join("、")
    }
}

pub(crate) fn format_validation_issues(issues: &[ValidationIssue]) -> String {
    if issues.is_empty() {
        return "人工送审结构检查未通过".into();
    }
    let mut empty = Vec::new();
    let mut missing = Vec::new();
    let mut duplicate = Vec::new();
    let mut out_of_order = Vec::new();
    let mut unknown = Vec::new();
    let mut others = Vec::new();
    for issue in issues {
        let loc = (issue.paragraph_id.as_deref(), issue.position);
        match issue.kind {
            IssueKind::EmptyText => empty.push(loc),
            IssueKind::Missing => missing.push(loc),
            IssueKind::Duplicate => duplicate.push(loc),
            IssueKind::OutOfOrder => out_of_order.push(loc),
            IssueKind::Unknown => unknown.push(loc),
            IssueKind::WrongBlock
            | IssueKind::InvalidJson
            | IssueKind::NewerVersion
            | IssueKind::UnsupportedVersion => others.push(issue.message.clone()),
        }
    }
    let mut parts = Vec::new();
    if !empty.is_empty() {
        let locs = format_paragraph_refs(&empty);
        if empty.len() == 1 {
            parts.push(format!(
                "以下段落尚未填写译文（不能为空或仅含空白）：{locs}"
            ));
        } else {
            parts.push(format!(
                "以下 {} 段尚未填写译文（不能为空或仅含空白）：{locs}",
                empty.len()
            ));
        }
    }
    if !missing.is_empty() {
        let locs = format_paragraph_refs(&missing);
        if missing.len() == 1 {
            parts.push(format!("缺少译文段落：{locs}"));
        } else {
            parts.push(format!("缺少 {} 段译文：{locs}", missing.len()));
        }
    }
    if !duplicate.is_empty() {
        parts.push(format!(
            "存在重复段落：{}",
            format_paragraph_refs(&duplicate)
        ));
    }
    if !out_of_order.is_empty() {
        parts.push(format!(
            "以下段落顺序与原文不一致：{}",
            format_paragraph_refs(&out_of_order)
        ));
    }
    if !unknown.is_empty() {
        parts.push(format!(
            "存在不属于本组的段落：{}",
            format_paragraph_refs(&unknown)
        ));
    }
    parts.extend(others);
    format!(
        "人工送审结构检查未通过（共 {} 处）：{}",
        issues.len(),
        parts.join("；")
    )
}

#[cfg(test)]
mod tests {
    use super::format_validation_issues;
    use crate::text_project::{IssueKind, ValidationIssue};

    fn issue(kind: IssueKind, pid: Option<&str>, pos: Option<usize>, msg: &str) -> ValidationIssue {
        ValidationIssue {
            kind,
            chapter_id: "c0001".into(),
            block_id: "c0001-b0001".into(),
            paragraph_id: pid.map(str::to_owned),
            position: pos,
            message: msg.into(),
        }
    }

    #[test]
    fn formats_contiguous_empty_paragraphs_cleanly() {
        let issues: Vec<_> = (3..=14)
            .map(|n| {
                issue(
                    IssueKind::EmptyText,
                    Some(&format!("c0001-p{n:04}")),
                    Some(n),
                    "候选段落文本不能为空或仅有空白",
                )
            })
            .collect();
        let text = format_validation_issues(&issues);
        assert_eq!(
            text,
            "人工送审结构检查未通过（共 12 处）：以下 12 段尚未填写译文（不能为空或仅含空白）：第1章 · 第3～14段"
        );
        assert!(!text.contains("ValidationIssue"));
    }

    #[test]
    fn formats_mixed_and_non_standard_issues_cleanly() {
        let issues = vec![
            issue(IssueKind::EmptyText, Some("c0001-p0002"), Some(2), "空"),
            issue(IssueKind::EmptyText, Some("c0001-p0004"), Some(4), "空"),
            issue(IssueKind::Missing, Some("p-custom"), None, "缺少"),
            issue(IssueKind::WrongBlock, None, None, "预期 b1，收到 b2"),
        ];
        let text = format_validation_issues(&issues);
        assert_eq!(
            text,
            "人工送审结构检查未通过（共 4 处）：以下 2 段尚未填写译文（不能为空或仅含空白）：第1章 · 第2段、第4段；缺少译文段落：p-custom；预期 b1，收到 b2"
        );
    }
}
