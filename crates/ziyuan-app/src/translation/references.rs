//! Explicit, versioned reference selection. The event owns the bytes, not the index.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use ziyuan_core::{Edict, ReferenceBinding, TaskId, TranslationKind};
use ziyuan_memory::{Digests, Identity, Origin};
use ziyuan_storage::{EventStore, LockScope, Workspace};

use super::{Translations, block_source_hash, sha256_hex};
use crate::memory::{FinalizedPair, reference_material};
use crate::{AppError, Session};

pub const MAX_REFERENCES: usize = 8;
pub const MAX_REFERENCE_BYTES: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceSelection {
    pub identity: Identity,
    pub digests: Digests,
    pub origin: Origin,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceRequest {
    pub selections: Vec<ReferenceSelection>,
    #[serde(default)]
    pub authorized_projects: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceSubmission {
    pub request: ReferenceRequest,
    pub fingerprint: String,
    pub submission: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferencePreview {
    pub project: String,
    pub block: String,
    pub kind: TranslationKind,
    pub request: ReferenceRequest,
    pub references: Vec<FinalizedPair>,
    /// Exact additional model input, including citation boundaries.
    pub prompt: String,
    pub bytes: usize,
    pub fingerprint: String,
    /// Authoritative file facts, independent of search ranking/cache watermarks.
    pub facts: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReferenceTaskView {
    pub task: u64,
    pub snapshot: ReferencePreview,
    pub current: bool,
    pub reason: Option<String>,
}

pub(super) struct PreparedReferences {
    pub binding: ReferenceBinding,
    pub snapshot: ReferencePreview,
    pub court: Vec<u8>,
}

fn invalid(why: impl Into<String>) -> AppError {
    AppError::External(format!("参考选择不可用：{}", why.into()))
}

fn json<T: Serialize>(value: &T) -> Result<Vec<u8>, AppError> {
    serde_json::to_vec(value).map_err(|_| invalid("无法编码参考快照"))
}

fn writer(kind: TranslationKind) -> Result<(), AppError> {
    if !matches!(kind, TranslationKind::Translate | TranslationKind::Revise) {
        return Err(invalid("参考只用于本次初译或修订，不用于只读审校"));
    }
    Ok(())
}

fn submission_hash(
    project: &str,
    block: &str,
    kind: TranslationKind,
    submission: &ReferenceSubmission,
) -> Result<String, AppError> {
    if submission.submission.is_empty()
        || submission.submission.len() > 128
        || !submission
            .submission
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(invalid("提交身份须为 1 至 128 位 ASCII 字母、数字、- 或 _"));
    }
    Ok(sha256_hex(&json(&(
        project,
        block,
        kind,
        &submission.request,
        &submission.fingerprint,
    ))?))
}

/// Read every selected block's source/draft/context/zhupi, and the target's facts.
/// Existing safe readers reject unsafe paths and modified approved snapshots.
fn file_facts(
    root: &Path,
    snapshot: &ReferencePreview,
) -> Result<BTreeMap<String, String>, AppError> {
    let translations = Translations::new(root);
    let mut blocks = BTreeSet::from([(snapshot.project.clone(), snapshot.block.clone())]);
    blocks.extend(
        snapshot
            .request
            .selections
            .iter()
            .map(|s| (s.identity.project.clone(), s.identity.block.clone())),
    );
    let mut facts = BTreeMap::new();
    for (project, block) in blocks {
        let p = translations.project(&project)?;
        let source = block_source_hash(&p, &block)?;
        let draft = translations.read_translation(&project, &block)?;
        let context = translations.current_context(&project)?;
        let zhupi = translations.block_zhupi_hash(&p, &block)?;
        facts.insert(
            format!("{project}/{block}"),
            sha256_hex(&json(&(
                &p.metadata,
                source,
                draft.map(|bytes| sha256_hex(&bytes)),
                context,
                zhupi,
            ))?),
        );
    }
    Ok(facts)
}

impl PreparedReferences {
    pub(super) fn check_files(&self, root: &Path) -> Result<(), AppError> {
        if file_facts(root, &self.snapshot)? != self.snapshot.facts {
            return Err(invalid("资料在下诏期间变化，请重新预览"));
        }
        Ok(())
    }
}

impl<S: EventStore> Session<S> {
    /// No model calls or business writes. Resolve metadata against current facts,
    /// never accept client-supplied reference prose or trust the derived index.
    pub fn preview_references(
        &self,
        root: &Path,
        project: &str,
        block: &str,
        kind: TranslationKind,
        request: &ReferenceRequest,
    ) -> Result<ReferencePreview, AppError> {
        let workspace = Workspace::open(root).map_err(|_| invalid("无法打开工作区"))?;
        let _lock = workspace
            .lock(LockScope::Workspace)
            .map_err(|_| invalid("无法锁定工作区"))?;
        self.preview_references_locked(root, project, block, kind, request)
    }

    fn preview_references_locked(
        &self,
        root: &Path,
        project: &str,
        block: &str,
        kind: TranslationKind,
        request: &ReferenceRequest,
    ) -> Result<ReferencePreview, AppError> {
        writer(kind)?;
        if request.selections.is_empty() || request.selections.len() > MAX_REFERENCES {
            return Err(invalid("请选择 1 至 8 条参考；不选时使用原来的无参考流程"));
        }
        let before = self.memory_watermark(root)?;
        let target = Translations::new(root).project(project)?;
        super::find_block(&target, block)?;
        let authorized = if request.authorized_projects.is_empty() {
            BTreeSet::from([project.to_owned()])
        } else {
            request.authorized_projects.clone()
        };
        if !authorized.contains(project) {
            return Err(invalid("授权范围必须包含当前项目"));
        }
        let mut identities = BTreeSet::new();
        let mut eligible = BTreeMap::new();
        let mut references = Vec::new();
        for selection in &request.selections {
            let source_project = &selection.identity.project;
            if !authorized.contains(source_project) {
                return Err(invalid("来源项目未明确授权"));
            }
            if !identities.insert(json(&selection.identity)?) {
                return Err(invalid("同一段参考不能重复选择"));
            }
            if !eligible.contains_key(source_project) {
                eligible.insert(
                    source_project.clone(),
                    self.memory_pairs(root, source_project)?,
                );
            }
            let pair = eligible[source_project]
                .pairs
                .iter()
                .find(|p| {
                    p.identity == selection.identity
                        && p.digests == selection.digests
                        && p.origin == selection.origin
                })
                .ok_or_else(|| invalid("参考失效或版本已变，请重新查询、选择和预览"))?;
            if pair.source_language != target.metadata.source_language
                || pair.target_language != target.metadata.target_language
            {
                return Err(invalid("来源与目标语言必须相同"));
            }
            references.push(pair.clone());
        }
        let mut preview = ReferencePreview {
            project: project.into(),
            block: block.into(),
            kind,
            request: request.clone(),
            references,
            prompt: String::new(),
            bytes: 0,
            fingerprint: String::new(),
            facts: BTreeMap::new(),
        };
        preview.facts = file_facts(root, &preview)?;
        // Content-derived boundaries make repeated previews byte-identical. They
        // are citation delimiters, not a security capability or a trust score.
        let nonce = sha256_hex(&json(&preview)?);
        let records: Vec<_> = preview
            .references
            .iter()
            .map(|pair| {
                serde_json::json!({
                    "identity": pair.identity,
                    "origin": pair.origin,
                    "source": pair.source,
                    "translation": pair.target,
                })
            })
            .collect();
        preview.prompt = reference_material(&serde_json::json!(records), &nonce[..16]);
        preview.bytes = preview.prompt.len();
        if preview.bytes > MAX_REFERENCE_BYTES {
            return Err(invalid("完整参考材料超过 8192 字节，请减少选择；不会截断"));
        }
        preview.fingerprint = sha256_hex(&json(&preview)?);
        if self.memory_watermark(root)? != before || file_facts(root, &preview)? != preview.facts {
            return Err(invalid("资料在预览期间变化，请重试"));
        }
        Ok(preview)
    }

    /// Exact retry identity is checked before eligibility: a lost response must
    /// return the original task even if that task has since run or sources changed.
    pub fn issue_with_references(
        &mut self,
        root: &Path,
        project: &str,
        block: &str,
        kind: TranslationKind,
        submission: &ReferenceSubmission,
    ) -> Result<Edict, AppError> {
        writer(kind)?;
        let request_hash = submission_hash(project, block, kind, submission)?;
        let workspace = Workspace::open(root).map_err(|_| invalid("无法打开工作区"))?;
        let _lock = workspace
            .lock(LockScope::Workspace)
            .map_err(|_| invalid("无法锁定工作区"))?;
        workspace
            .ensure_writable()
            .map_err(|_| invalid("工作区写入未收尾"))?;
        self.reload()?;
        if let Some(task) = self.court.tasks().find(|task| {
            task.references
                .as_ref()
                .is_some_and(|r| r.submission == submission.submission)
        }) {
            if task
                .references
                .as_ref()
                .is_some_and(|r| r.request_hash == request_hash)
            {
                return Ok(task.edict.clone());
            }
            return Err(invalid("同一提交身份已用于不同内容，请查询原任务"));
        }
        let snapshot =
            self.preview_references_locked(root, project, block, kind, &submission.request)?;
        if snapshot.fingerprint != submission.fingerprint {
            return Err(invalid("确认指纹已过期，请重新预览完整外发内容"));
        }
        let encoded = String::from_utf8(json(&snapshot)?).expect("JSON is UTF-8");
        if encoded.len() > 32768 {
            return Err(invalid("完整任务快照超过 32768 字节，请减少选择"));
        }
        let prepared = PreparedReferences {
            binding: ReferenceBinding {
                submission: submission.submission.clone(),
                request_hash,
                snapshot: encoded,
            },
            snapshot,
            court: json(&self.court)?,
        };
        self.issue_writer(root, project, block, kind, Some(&prepared))
    }

    /// History remains readable without a memory index/provider and when stale.
    pub fn reference_task(&self, root: &Path, task: TaskId) -> Result<ReferenceTaskView, AppError> {
        let task_entry = self.court.task(task).ok_or_else(|| invalid("没有该任务"))?;
        let binding = task_entry
            .references
            .as_ref()
            .ok_or_else(|| invalid("该任务未选用参考"))?;
        let snapshot: ReferencePreview = serde_json::from_str(&binding.snapshot)
            .map_err(|_| invalid("任务参考快照损坏或版本不支持"))?;
        let link = task_entry
            .translation
            .as_ref()
            .ok_or_else(|| invalid("缺少译书关联"))?;
        if link.project != snapshot.project
            || link.block != snapshot.block
            || link.kind != snapshot.kind
        {
            return Err(invalid("参考快照与目标任务不符"));
        }
        let submission = ReferenceSubmission {
            request: snapshot.request.clone(),
            fingerprint: snapshot.fingerprint.clone(),
            submission: binding.submission.clone(),
        };
        if submission_hash(
            &snapshot.project,
            &snapshot.block,
            snapshot.kind,
            &submission,
        )? != binding.request_hash
        {
            return Err(invalid("参考快照与提交摘要不符"));
        }
        let validity = self
            .preview_references(
                root,
                &snapshot.project,
                &snapshot.block,
                snapshot.kind,
                &snapshot.request,
            )
            .and_then(|current| {
                if current != snapshot {
                    Err(invalid("来源或目标材料已变；此处保留当时确认的内容"))
                } else {
                    Ok(())
                }
            });
        Ok(ReferenceTaskView {
            task: task.0,
            snapshot,
            current: validity.is_ok(),
            reason: validity.err().map(|e| e.to_string()),
        })
    }
}
