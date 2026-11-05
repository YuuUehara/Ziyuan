//! Approved targets: the bytes an approval authorizes for `zh.json`.
//!
//! First format is the legacy `PromptBlock` JSON. Second format is the frozen
//! candidate shape with target marks. This module only converts and reads;
//! it never writes a file, opens an approval or touches signoffs.

use super::{Result, ReviewCandidate, invalid};
use crate::text_project::PromptBlock;
use serde::Deserialize;

/// Legacy plain target; byte-compatible with `PromptBlock::serialized`.
pub const TARGET_FORMAT_PLAIN: &str = "zh-json/1";
/// Marked target; the candidate shape with target-only annotations.
pub const TARGET_FORMAT_MARKED: &str = "zh-json/2";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApprovedTarget {
    Plain(PromptBlock),
    Marked(ReviewCandidate),
}

impl ApprovedTarget {
    pub fn format(&self) -> &'static str {
        match self {
            Self::Plain(_) => TARGET_FORMAT_PLAIN,
            Self::Marked(_) => TARGET_FORMAT_MARKED,
        }
    }

    /// Exact bytes this target authorizes, including the final newline.
    pub fn bytes(&self) -> Result<Vec<u8>> {
        match self {
            Self::Plain(block) => Ok(block.serialized()?),
            Self::Marked(candidate) => candidate.serialized(),
        }
    }
}

impl ReviewCandidate {
    /// Normalize one frozen candidate into the target it may approve.
    /// Plain candidates keep the legacy bytes even if they arrived as v2.
    pub fn approved_target(&self) -> Result<ApprovedTarget> {
        self.validate()?;
        if self.paragraphs.iter().any(|p| !p.marks.is_empty()) {
            if self.schema_version != 2 {
                return Err(invalid("含标记的译文候选须为第二版"));
            }
            Ok(ApprovedTarget::Marked(self.clone()))
        } else {
            Ok(ApprovedTarget::Plain(self.text_structure()))
        }
    }
}

#[derive(Deserialize)]
struct Header {
    schema_version: u32,
}

/// Read a `zh.json` target without guessing its format. Unknown versions and
/// inconsistent content are rejected, never downgraded to plain text.
pub fn read_approved_target(bytes: &[u8], block: &str) -> Result<ApprovedTarget> {
    let header: Header = serde_json::from_slice(bytes)?;
    match header.schema_version {
        1 => {
            let value: serde_json::Value = serde_json::from_slice(bytes)?;
            let carries_marks = value
                .get("paragraphs")
                .and_then(|p| p.as_array())
                .is_some_and(|paragraphs| paragraphs.iter().any(|p| p.get("marks").is_some()));
            if carries_marks {
                return Err(invalid("译稿第一版不得带文学标记"));
            }
            let plain: PromptBlock = serde_json::from_slice(bytes)?;
            if plain.block_id != block {
                return Err(invalid(format!(
                    "译稿块 ID {} 与目标 {block} 不符",
                    plain.block_id
                )));
            }
            Ok(ApprovedTarget::Plain(plain))
        }
        2 => {
            let candidate: ReviewCandidate = serde_json::from_slice(bytes)?;
            if candidate.block_id != block {
                return Err(invalid(format!(
                    "译稿块 ID {} 与目标 {block} 不符",
                    candidate.block_id
                )));
            }
            candidate.validate()?;
            if candidate.paragraphs.iter().all(|p| p.marks.is_empty()) {
                return Err(invalid("译稿第二版须至少包含一处译文标记"));
            }
            Ok(ApprovedTarget::Marked(candidate))
        }
        other => Err(invalid(format!(
            "译稿格式第 {other} 版本版读不了，请升级紫垣后再打开"
        ))),
    }
}
