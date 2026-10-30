use super::Result;
use serde::{Deserialize, Serialize};

/// User statements, not a license verification or a grant of rights.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub author: String,
    pub version: String,
    pub source: String,
    pub date: String,
    pub rights: String,
    pub scope: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewProject {
    pub title: String,
    pub source_language: String,
    pub target_language: String,
    pub provenance: Provenance,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub metadata: NewProject,
    pub block_bytes: usize,
    pub chapters: Vec<Chapter>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chapter {
    pub id: String,
    pub source_name: String,
    pub original_path: String,
    pub sha256: String,
    pub byte_len: usize,
    pub paragraphs: Vec<Paragraph>,
    pub blocks: Vec<PromptBlock>,
}
/// Half-open UTF-8 byte range in the unchanged original; gaps retain blank-line layout.
/// Paragraph text normalizes CRLF to LF, but does not trim nonblank lines.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paragraph {
    pub id: String,
    pub byte_start: usize,
    pub byte_end: usize,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockParagraph {
    pub id: String,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptBlock {
    pub schema_version: u32,
    pub block_id: String,
    pub paragraphs: Vec<BlockParagraph>,
}
impl PromptBlock {
    /// Exact on-disk/prompt representation, including the final newline. Budget these bytes,
    /// not character counts. Candidate JSON uses the same shape with translated text.
    pub fn serialized(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}
