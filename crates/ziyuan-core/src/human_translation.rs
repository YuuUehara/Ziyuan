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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanSubmissionBinding {
    pub request: HumanBindingRequest,
    pub request_hash: String,
}
