//! Frozen human review material. None of these facts constitutes approval.
use crate::TaskId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftBasis {
    pub project_hash: String,
    pub source_hash: String,
    pub context_version: u32,
    pub zhupi_hash: String,
    /// Disk bytes, not an assertion of approval.
    pub translation_hash: Option<String>,
}

/// Zero-based log index and SHA-256 of the canonical event line, without LF.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventReference {
    pub index: u64,
    pub hash: String,
}

/// Claimed predecessor, to be resolved against the log before approval opens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TranslationSource {
    None {},
    Model {
        task: TaskId,
        changes_version: String,
        event: EventReference,
    },
    Human {
        operation_id: String,
        event: EventReference,
    },
}

/// Authenticated entry channel, not a claim of personal identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanActor {
    Local,
    Web,
}

/// Field order is part of the binding request's canonical encoding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanBindingRequest {
    pub project: String,
    pub block: String,
    pub request_id: String,
    pub actor: HumanActor,
    pub submission_id: String,
    pub revision: String,
    pub basis: DraftBasis,
    /// Exact ReviewCandidate::serialized() UTF-8, including its final LF.
    /// Opaque here: only the application owns the candidate/markup schema.
    pub candidate: String,
    pub candidate_hash: String,
    pub previous: TranslationSource,
}

/// Single logical output for one approval. The format identity is owned by the
/// application; governance only checks its shape and the path inside the project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanApprovalTarget {
    pub format: String,
    /// Project-relative path with `/` separators; no absolute paths.
    pub path: String,
    /// Digest of the target bytes before this approval; `None` when absent.
    pub before: Option<String>,
    /// Digest of the bytes this approval authorizes.
    pub after: String,
}

/// Field order is part of the approval request's canonical encoding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanApprovalRequest {
    pub project: String,
    pub block: String,
    pub request_id: String,
    pub actor: HumanActor,
    /// Request ID of the frozen `HumanSubmissionBound` this approval relies on.
    pub submission_id: String,
    /// Zero-based log index and canonical line digest of that binding event.
    pub binding: EventReference,
    pub previous: TranslationSource,
    pub target: HumanApprovalTarget,
}

/// Durable approval fact. It authorizes, but does not itself write the target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanApproval {
    pub request: HumanApprovalRequest,
    pub request_hash: String,
    /// Managed-write operation identity; also the approval identity.
    pub operation_id: String,
}

/// Durable human finalization fact. It references an approval receipt, not a model task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanFinalization {
    pub project: String,
    pub block: String,
    pub approval_request_id: String,
    pub approval_operation_id: String,
    pub approval_event: EventReference,
    pub submission_id: String,
    pub candidate_hash: String,
    pub source_hash: String,
    pub context_version: u32,
    pub zhupi_hash: String,
    pub target_format: String,
    pub target_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanSubmissionBinding {
    pub request: HumanBindingRequest,
    pub request_hash: String,
}
