//! Local human drafts only: no submission, court events, model calls or approved writes.
//! Current text and durable retry receipts share one atomic JSON publication.
use super::{
    Result, TranslationError, Translations, ensure_plain_dirs, find_block, invalid, sha256_hex,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io};
use ziyuan_storage::{JsonFile, LockScope, Workspace};

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 256 * 1024;

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
}

impl DraftBook {
    fn check(&self, project: &str, block: &str) -> Result<()> {
        let hash = sha256_hex(&serde_json::to_vec(&self.draft)?);
        if self.draft.project != project
            || self.draft.block != block
            || !self.requests.values().any(|r| {
                r.receipt.revision == self.draft.revision && r.receipt.content_hash == hash
            })
        {
            return Err(ziyuan_storage::JsonFileError::Corrupt(
                "人工草稿位置或现行回执摘要不符".into(),
            )
            .into());
        }
        Ok(())
    }
}

impl Translations {
    fn draft_file(&self, project: &str, block: &str) -> Result<JsonFile<DraftBook>> {
        // Membership is checked before using either identifier as a path component.
        find_block(&self.project(project)?, block)?;
        let dir = ensure_plain_dirs(
            &self.root,
            &[".ziyuan", "text-projects", project, "human-drafts"],
        )?;
        let file = JsonFile::new(dir.join(format!("{block}.json")), 1, MAX_FILE_BYTES);
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
        if request.request_id.is_empty()
            || request.request_id.len() > 128
            || !request
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(invalid("草稿请求身份须为 1–128 位 ASCII 字母数字、- 或 _"));
        }
        if request
            .paragraphs
            .iter()
            .map(|p| p.text.len())
            .sum::<usize>()
            > MAX_TEXT_BYTES
        {
            return Err(invalid("人工草稿正文超过 256 KiB"));
        }
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
        let mut requests = current.map(|s| s.value.requests).unwrap_or_default();
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
            Ok((DraftBook { draft, requests }, ()))
        })?;
        Ok(receipt)
    }
}
