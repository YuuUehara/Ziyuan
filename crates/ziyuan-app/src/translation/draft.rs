//! Local human drafts and immutable review submissions, never approved writes.
//! Text, submissions and durable retry receipts share one atomic JSON publication.
use super::{
    Result, TargetMark, TranslationError, Translations, ensure_plain_dirs, find_block, invalid,
    markup, sha256_hex,
};
use crate::text_project::{BlockParagraph, PromptBlock};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io};
use ziyuan_storage::{JsonFile, LockScope, Workspace};

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftBasis {
    pub project_hash: String,
    pub source_hash: String,
    pub context_version: u32,
    pub zhupi_hash: String,
    /// Current disk bytes only, not a claim that the translation is approved.
    pub translation_hash: Option<String>,
}

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
    /// The legacy candidate format cannot represent target marks. Never flatten
    /// rich drafts into this format, even if a caller constructs a snapshot itself.
    pub fn candidate(&self) -> Result<PromptBlock> {
        if self.draft.paragraphs.iter().any(|p| !p.marks.is_empty()) {
            return Err(invalid(
                "含文学标记的人工稿暂只支持保存／恢复，不能转为纯文本提交",
            ));
        }
        Ok(PromptBlock {
            schema_version: 1,
            block_id: self.draft.block.clone(),
            paragraphs: self
                .draft
                .paragraphs
                .iter()
                .map(|p| BlockParagraph {
                    id: p.id.clone(),
                    text: p.text.clone(),
                    inlines: Vec::new(),
                })
                .collect(),
        })
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
        let file = JsonFile::new(dir.join(format!("{block}.json")), 3, MAX_FILE_BYTES);
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
        let bytes = submission.candidate()?.serialized()?;
        let report = self.texts().check(project, block, &bytes)?;
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
