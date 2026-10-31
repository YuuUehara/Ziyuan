//! 记录契约：一条记忆 = 一个源文段落 ID 的原文／译文对，连同身份、摘要、定稿来源、
//! 嵌入签名与构建水位。
//!
//! 身份（工作目录、项目、章节、块、段落）说明「是哪一段」；版本（内容摘要与定稿来源）
//! 说明「是哪一版」。重新定稿换掉现行版本，旧版本不得继续作为有效命中。
//! 本模块只校验格式自洽，不回读任何文件，也不判断译文质量。

use serde::{Deserialize, Serialize};

use crate::MemoryError;

/// 逻辑身份：工作目录、项目、章节、块、段落。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Identity {
    /// [`workspace_id`] 的结果：只用来发现「记录与当前工作区不匹配」，
    /// 不存绝对路径，也不据以认定不同机器上的资料相同。
    pub workspace_id: String,
    pub project: String,
    pub chapter: String,
    pub block: String,
    pub paragraph: String,
}

/// 一段文本与其内容摘要（UTF-8 字节的 sha256）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Text {
    pub text: String,
    /// [`sha256_hex`] 的结果：64 位小写十六进制。
    pub sha256: String,
}
impl Text {
    /// 由文本建立一项，摘要一并算出。
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            sha256: sha256_hex(text.as_bytes()),
            text,
        }
    }
}

/// 块级原文／译文摘要：用于发现整块变化，而不只是这一段。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Digests {
    pub block_source_sha256: String,
    pub block_target_sha256: String,
}

/// 定稿来源：哪几次定稿事件立下了这一版，以及当时的上下文与朱批版本。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Origin {
    /// 事件簿位置（`Vec` 下标）。同一块可能被重新定稿多次，这里记全部，按升序去重。
    pub finalized_events: Vec<usize>,
    /// 已定稿译文的摘要：收录前须与磁盘字节的摘要核对。
    pub translation_hash: String,
    pub context_version: u32,
    pub zhupi_version: u32,
}

/// 嵌入签名：换了提供方、模型、修订、维度或度量，索引即不可再用。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct EmbeddingSignature {
    pub provider: String,
    pub model: String,
    /// 模型修订或可用版本信息。
    pub revision: String,
    pub dim: usize,
    pub metric: String,
    /// 嵌入输入（首版为原文）的摘要，用于确认向量对应的正是这段文本。
    pub input_sha256: String,
}

/// 构建水位：索引覆盖到哪一步资料，读取前后不一致即拒绝发布。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Watermark {
    /// 事件条数。
    pub events: usize,
    /// 到 `events` 为止的事件历史摘要。
    pub history_sha256: String,
    pub history_bytes: u64,
    /// 工作区世代：只在写入开始／结束时推进。
    pub generation: u64,
}

/// 一条记忆记录。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MemoryRecord {
    pub identity: Identity,
    pub source_language: String,
    pub target_language: String,
    pub source: Text,
    pub target: Text,
    pub digests: Digests,
    pub origin: Origin,
    pub embedding: EmbeddingSignature,
    pub watermark: Watermark,
}

impl MemoryRecord {
    /// 校验格式自洽。只返回固定的原因与字段名，不回显记录里的文本。
    pub fn validate(&self) -> Result<(), MemoryError> {
        let identity = &self.identity;
        for (name, value) in [
            ("workspace_id", identity.workspace_id.as_str()),
            ("project", identity.project.as_str()),
            ("chapter", identity.chapter.as_str()),
            ("block", identity.block.as_str()),
            ("paragraph", identity.paragraph.as_str()),
            ("source_language", self.source_language.as_str()),
            ("target_language", self.target_language.as_str()),
            ("embedding.provider", self.embedding.provider.as_str()),
            ("embedding.model", self.embedding.model.as_str()),
            ("embedding.metric", self.embedding.metric.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(invalid(format!("{name} 不能为空")));
            }
        }
        for (name, value) in [
            ("source.sha256", self.source.sha256.as_str()),
            ("target.sha256", self.target.sha256.as_str()),
            (
                "digests.block_source_sha256",
                self.digests.block_source_sha256.as_str(),
            ),
            (
                "digests.block_target_sha256",
                self.digests.block_target_sha256.as_str(),
            ),
            (
                "origin.translation_hash",
                self.origin.translation_hash.as_str(),
            ),
            (
                "embedding.input_sha256",
                self.embedding.input_sha256.as_str(),
            ),
            (
                "watermark.history_sha256",
                self.watermark.history_sha256.as_str(),
            ),
        ] {
            if !is_sha256(value) {
                return Err(invalid(format!("{name} 须为 64 位小写十六进制")));
            }
        }
        if self.embedding.dim == 0 {
            return Err(invalid("embedding.dim 不得为 0"));
        }
        if self.source.text.is_empty() {
            return Err(invalid("source.text 不能为空"));
        }
        if self.target.text.trim().is_empty() {
            return Err(invalid("target.text 不能为空或只有空白"));
        }
        // 定稿事件须有据可查，且按升序去重，便于逐条复核。
        if self.origin.finalized_events.is_empty() {
            return Err(invalid("origin.finalized_events 不能为空"));
        }
        if self
            .origin
            .finalized_events
            .windows(2)
            .any(|w| w[0] >= w[1])
        {
            return Err(invalid("origin.finalized_events 须按升序去重"));
        }
        Ok(())
    }
}

/// 本 crate 唯一的 SHA-256 助手：摘要的 64 位小写十六进制。
///
/// 别处不得再抄一份摘要循环：`ziyuan-storage::content_sha256` 带跨进程语义，
/// 引用它反而多一层依赖，所以这里直接用 `ring`。
pub fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut out = String::with_capacity(64);
    for byte in digest.as_ref() {
        out.push(HEX[usize::from(byte >> 4)] as char);
        out.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// 由工作区根的**规范化文本**取身份摘要，截取前 32 位十六进制。
///
/// 只用来发现「记录与当前工作区不匹配」，**绝不**据以认定不同机器上的资料相同：
/// 路径文本相同不等于内容相同。工作目录迁移后须重新绑定来源或重建索引；
/// 这里不收路径、不做规范化，也不读文件系统——规范化由调用方决定。
pub fn workspace_id(root_text: &str) -> String {
    let mut id = sha256_hex(root_text.as_bytes());
    // 全为 ASCII 十六进制，截断落点必在字符边界上。
    id.truncate(32);
    id
}

fn invalid(reason: impl Into<String>) -> MemoryError {
    MemoryError::Invalid(reason.into())
}

/// 64 位小写十六进制。
fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> MemoryRecord {
        let source = Text::new("夜が明けた");
        MemoryRecord {
            identity: Identity {
                workspace_id: workspace_id("D:/novels/demo"),
                project: "p0001".into(),
                chapter: "c0001".into(),
                block: "c0001-b0001".into(),
                paragraph: "c0001-p0001".into(),
            },
            source_language: "ja".into(),
            target_language: "zh".into(),
            source,
            target: Text::new("天亮了"),
            digests: Digests {
                block_source_sha256: sha256_hex(b"block-source"),
                block_target_sha256: sha256_hex(b"block-target"),
            },
            origin: Origin {
                finalized_events: vec![3, 7],
                translation_hash: sha256_hex(b"zh.json"),
                context_version: 2,
                zhupi_version: 1,
            },
            embedding: EmbeddingSignature {
                provider: "local".into(),
                model: "char-hash-v1".into(),
                revision: "1".into(),
                dim: 256,
                metric: "cosine".into(),
                input_sha256: sha256_hex(b"input"),
            },
            watermark: Watermark {
                events: 12,
                history_sha256: sha256_hex(b"history"),
                history_bytes: 4096,
                generation: 8,
            },
        }
    }

    fn assert_invalid(record: &MemoryRecord, needle: &str) {
        match record.validate() {
            Err(MemoryError::Invalid(reason)) => {
                assert!(reason.contains(needle), "{reason}");
            }
            other => panic!("预期不合法（含 {needle}），得到 {other:?}"),
        }
    }

    #[test]
    fn record_round_trips_through_json() {
        let record = good();
        let json = serde_json::to_string(&record).unwrap();
        let back: MemoryRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, back);
        record.validate().unwrap();
    }

    #[test]
    fn record_serializes_with_snake_case_field_names() {
        let json = serde_json::to_string(&good()).unwrap();
        for key in [
            "workspace_id",
            "source_language",
            "block_source_sha256",
            "finalized_events",
            "translation_hash",
            "context_version",
            "zhupi_version",
            "input_sha256",
            "history_sha256",
            "history_bytes",
            "generation",
        ] {
            assert!(json.contains(&format!("\"{key}\"")), "缺少 {key}: {json}");
        }
    }

    #[test]
    fn unknown_field_is_rejected() {
        let json = serde_json::to_string(&good()).unwrap();
        let tampered = json.replacen('{', "{\"unexpected\":1,", 1);
        let err = serde_json::from_str::<MemoryRecord>(&tampered).unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn unknown_nested_field_is_rejected() {
        let json = serde_json::to_string(&good()).unwrap();
        let tampered = json.replacen("\"events\":12", "\"events\":12,\"extra\":true", 1);
        assert!(serde_json::from_str::<MemoryRecord>(&tampered).is_err());
    }

    #[test]
    fn validate_accepts_a_well_formed_record() {
        good().validate().unwrap();
    }

    #[test]
    fn validate_rejects_empty_workspace_id() {
        let mut record = good();
        record.identity.workspace_id.clear();
        assert_invalid(&record, "workspace_id");
    }

    #[test]
    fn validate_rejects_empty_project() {
        let mut record = good();
        record.identity.project.clear();
        assert_invalid(&record, "project");
    }

    #[test]
    fn validate_rejects_empty_paragraph() {
        let mut record = good();
        record.identity.paragraph.clear();
        assert_invalid(&record, "paragraph");
    }

    #[test]
    fn validate_rejects_empty_language() {
        let mut record = good();
        record.source_language.clear();
        assert_invalid(&record, "source_language");
        let mut record = good();
        record.target_language.clear();
        assert_invalid(&record, "target_language");
    }

    #[test]
    fn validate_rejects_empty_provider() {
        let mut record = good();
        record.embedding.provider.clear();
        assert_invalid(&record, "embedding.provider");
    }

    #[test]
    fn validate_rejects_empty_model() {
        let mut record = good();
        record.embedding.model.clear();
        assert_invalid(&record, "embedding.model");
    }

    #[test]
    fn validate_rejects_empty_metric() {
        let mut record = good();
        record.embedding.metric = "  ".into();
        assert_invalid(&record, "embedding.metric");
    }

    #[test]
    fn validate_rejects_zero_dim() {
        let mut record = good();
        record.embedding.dim = 0;
        assert_invalid(&record, "embedding.dim");
    }

    #[test]
    fn validate_rejects_empty_source_text() {
        let mut record = good();
        record.source = Text::new("");
        assert_invalid(&record, "source.text");
    }

    #[test]
    fn validate_rejects_whitespace_target_text() {
        let mut record = good();
        record.target = Text::new(" \r\n　");
        assert_invalid(&record, "target.text");
    }

    #[test]
    fn validate_rejects_short_digest() {
        let mut record = good();
        record.source.sha256 = sha256_hex(b"abc")[..63].to_owned();
        assert_invalid(&record, "source.sha256");
    }

    #[test]
    fn validate_rejects_uppercase_digest() {
        let mut record = good();
        record.digests.block_target_sha256 = sha256_hex(b"abc").to_uppercase();
        assert_invalid(&record, "digests.block_target_sha256");
    }

    #[test]
    fn validate_rejects_bad_origin_digest() {
        let mut record = good();
        record.origin.translation_hash = "sha256:".into();
        assert_invalid(&record, "origin.translation_hash");
    }

    #[test]
    fn validate_rejects_bad_watermark_digest() {
        let mut record = good();
        record.watermark.history_sha256 = String::new();
        assert_invalid(&record, "watermark.history_sha256");
    }

    #[test]
    fn validate_rejects_empty_finalized_events() {
        let mut record = good();
        record.origin.finalized_events.clear();
        assert_invalid(&record, "finalized_events");
    }

    #[test]
    fn validate_rejects_unsorted_finalized_events() {
        let mut record = good();
        record.origin.finalized_events = vec![7, 3];
        assert_invalid(&record, "finalized_events");
        let mut record = good();
        record.origin.finalized_events = vec![3, 3];
        assert_invalid(&record, "finalized_events");
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn workspace_id_is_a_truncated_digest_without_a_path() {
        let id = workspace_id("D:/novels/demo");
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!id.contains('/') && !id.contains("novels"));
        assert_eq!(id, sha256_hex(b"D:/novels/demo")[..32].to_owned());
        assert_eq!(workspace_id("D:/novels/demo"), id);
        assert_ne!(workspace_id("D:/novels/other"), id);
    }
}
