//! 记录契约：一条记忆 = 一个源文段落 ID 的原文／译文对，连同身份、摘要、定稿来源、
//! 嵌入签名与构建水位。
//!
//! 身份（工作目录、项目、章节、块、段落）说明「是哪一段」；版本（内容摘要与定稿来源）
//! 说明「是哪一版」。重新定稿换掉现行版本，旧版本不得继续作为有效命中。
//! 本模块只校验格式自洽，不回读任何文件，也不判断译文质量。摘要能自证的只有
//! `source.sha256`／`target.sha256` 两项（见 [`MemoryRecord::verify_digests`]）；
//! 块摘要与磁盘译稿摘要须由调用方对着真实文件核对。

use serde::{Deserialize, Serialize};

use crate::MemoryError;

/// 逻辑身份：工作目录、项目、章节、块、段落。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Identity {
    /// [`workspace_id_from_canonical_text`] 的结果：只用来发现「记录与当前工作区不匹配」，
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
    ///
    /// 下标只在本记录 [`MemoryRecord::watermark`] 所指的事件条数内才有意义：
    /// 换一条水位，同一个数字就指向另一件事，因此水位不同即不可比。
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
    /// 内容种类。首版只有 `"paragraph"`：一段原文对应一段译文。
    ///
    /// 将来若接入代码等其它适配器，可能用别的值（如 `"code_block"`）。现在就占好位置，
    /// 免得新增一种内容就得改格式版本、逼所有人重建索引；读取方须先看这一项再解释
    /// `source`／`target` 的含义。
    pub content_type: String,
    pub source: Text,
    pub target: Text,
    pub digests: Digests,
    pub origin: Origin,
    pub embedding: EmbeddingSignature,
    pub watermark: Watermark,
}

impl MemoryRecord {
    /// 反序列化并校验格式；索引解码必须走这里，不要直接用 `serde_json::from_slice`。
    pub fn decode(bytes: &[u8]) -> Result<Self, MemoryError> {
        let record: Self = serde_json::from_slice(bytes)
            .map_err(|e| MemoryError::Corrupt(format!("记录无法解码: {e}")))?;
        record.validate_format()?;
        Ok(record)
    }

    /// 校验格式自洽。只返回固定的原因与字段名，不回显记录里的文本。
    ///
    /// 只查「字段形状对不对」，不证明记录自洽：摘要可能与文本毫无关系，
    /// 参见 [`Self::verify_digests`]。
    pub fn validate_format(&self) -> Result<(), MemoryError> {
        let identity = &self.identity;
        for (name, value) in [
            ("project", identity.project.as_str()),
            ("chapter", identity.chapter.as_str()),
            ("block", identity.block.as_str()),
            ("paragraph", identity.paragraph.as_str()),
            ("source_language", self.source_language.as_str()),
            ("target_language", self.target_language.as_str()),
            ("content_type", self.content_type.as_str()),
            ("embedding.provider", self.embedding.provider.as_str()),
            ("embedding.model", self.embedding.model.as_str()),
            // 修订号形状随提供方而异，只要求非空，不假定是数字或日期。
            ("embedding.revision", self.embedding.revision.as_str()),
            ("embedding.metric", self.embedding.metric.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(invalid(format!("{name} 不能为空")));
            }
        }
        if !is_workspace_id(&identity.workspace_id) {
            return Err(invalid("workspace_id 须为 32 位小写十六进制"));
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
        // 事件下标只在本记录的水位内才有意义：越界即指向不存在的事件。
        if let Some(last) = self.origin.finalized_events.last()
            && *last >= self.watermark.events
        {
            return Err(invalid(
                "origin.finalized_events 超出 watermark.events 所覆盖的范围",
            ));
        }
        Ok(())
    }

    /// 重算全部摘要并与记录中的值比对。校验格式不足以证明记录自洽：摘要可能与之无关。
    ///
    /// 只比对**能从记录自身重算**的两项：`source.sha256`、`target.sha256`。
    /// `digests.block_source_sha256`、`digests.block_target_sha256` 是**别的文件**
    /// （块的原文／译文 JSON）的摘要，`origin.translation_hash` 是**磁盘上** `zh.json`
    /// 的摘要，`embedding.input_sha256` 是交给嵌入提供方的输入的摘要——记录里没有那些
    /// 字节，重算不出来，必须由调用方拿真实文件核对。这里不假装核对过它们。
    pub fn verify_digests(&self) -> Result<(), MemoryError> {
        for (name, text) in [("source", &self.source), ("target", &self.target)] {
            if sha256_hex(text.text.as_bytes()) != text.sha256 {
                return Err(invalid(format!("{name}.sha256 与文本不符")));
            }
        }
        Ok(())
    }
}

/// 本 crate 唯一的 SHA-256 助手：摘要的 64 位小写十六进制。
///
/// `ziyuan-storage::content_sha256` 是**逐字相同**的循环，并没有跨进程语义差别；
/// 另抄一份的实在理由只有一条：不想为一段摘要拖进 storage 的整条事件模型依赖。
/// 将来若 storage 拆出独立的摘要模块，应当改为引用它，而不是继续各留一份。
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
/// 路径文本相同不等于内容相同。工作目录迁移后须重新绑定来源或重建索引。
///
/// 本函数**不做**任何规范化，也不碰文件系统。调用方必须先办完这几件事：
///
/// 1. 先规范化：取绝对路径并消掉 `.`、`..`，分隔符统一（Windows 上统一成 `\` 或 `/`），
///    末尾不带分隔符，也绝不把相对路径或未展开的 `~` 递进来；
/// 2. 在 Windows 上按平台的大小写折叠规则处理（`D:\Novels` 与 `d:\novels` 是同一处，
///    但两边算出的摘要必须相同，否则同一工作区在两次运行里互不认账）；
/// 3. 在调用前就把非 UTF-8 的路径与空输入挡掉：非 UTF-8 的路径没法进这个 `&str`，
///    空输入在这里返回 [`MemoryError::Invalid`]，绝不用空串算出一个看似合法的 ID。
///
/// 之所以要求「规范化」而非在这里做，是因为「什么算同一处」属于调用方的策略
/// （大小写、软链接、UNC 前缀），本 crate 不该替它拍板。
pub fn workspace_id_from_canonical_text(root_text: &str) -> Result<String, MemoryError> {
    if root_text.trim().is_empty() {
        return Err(invalid("workspace_id 的输入不能为空"));
    }
    let mut id = sha256_hex(root_text.as_bytes());
    // 全为 ASCII 十六进制，截断落点必在字符边界上。
    id.truncate(32);
    Ok(id)
}

fn invalid(reason: impl Into<String>) -> MemoryError {
    MemoryError::Invalid(reason.into())
}

/// 64 位小写十六进制。
fn is_sha256(value: &str) -> bool {
    value.len() == 64 && is_lower_hex(value)
}

/// 32 位小写十六进制：工作区身份摘要的固定宽度。
fn is_workspace_id(value: &str) -> bool {
    value.len() == 32 && is_lower_hex(value)
}

fn is_lower_hex(value: &str) -> bool {
    value
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
                workspace_id: workspace_id_from_canonical_text("D:/novels/demo").unwrap(),
                project: "p0001".into(),
                chapter: "c0001".into(),
                block: "c0001-b0001".into(),
                paragraph: "c0001-p0001".into(),
            },
            source_language: "ja".into(),
            target_language: "zh".into(),
            content_type: "paragraph".into(),
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
        match record.validate_format() {
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
        record.validate_format().unwrap();
        assert_eq!(MemoryRecord::decode(json.as_bytes()).unwrap(), record);
    }

    #[test]
    fn record_serializes_with_snake_case_field_names() {
        let json = serde_json::to_string(&good()).unwrap();
        for key in [
            "workspace_id",
            "source_language",
            "target_language",
            "content_type",
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
    fn validate_format_accepts_a_well_formed_record() {
        good().validate_format().unwrap();
    }

    #[test]
    fn validate_format_rejects_empty_workspace_id() {
        let mut record = good();
        record.identity.workspace_id.clear();
        assert_invalid(&record, "workspace_id");
    }

    #[test]
    fn validate_format_rejects_empty_project() {
        let mut record = good();
        record.identity.project.clear();
        assert_invalid(&record, "project");
    }

    #[test]
    fn validate_format_rejects_empty_paragraph() {
        let mut record = good();
        record.identity.paragraph.clear();
        assert_invalid(&record, "paragraph");
    }

    #[test]
    fn validate_format_rejects_empty_language() {
        let mut record = good();
        record.source_language.clear();
        assert_invalid(&record, "source_language");
        let mut record = good();
        record.target_language.clear();
        assert_invalid(&record, "target_language");
    }

    #[test]
    fn validate_format_rejects_empty_provider() {
        let mut record = good();
        record.embedding.provider.clear();
        assert_invalid(&record, "embedding.provider");
    }

    #[test]
    fn validate_format_rejects_empty_model() {
        let mut record = good();
        record.embedding.model.clear();
        assert_invalid(&record, "embedding.model");
    }

    #[test]
    fn validate_format_rejects_empty_metric() {
        let mut record = good();
        record.embedding.metric = "  ".into();
        assert_invalid(&record, "embedding.metric");
    }

    #[test]
    fn validate_format_rejects_zero_dim() {
        let mut record = good();
        record.embedding.dim = 0;
        assert_invalid(&record, "embedding.dim");
    }

    #[test]
    fn validate_format_rejects_empty_source_text() {
        let mut record = good();
        record.source = Text::new("");
        assert_invalid(&record, "source.text");
    }

    #[test]
    fn validate_format_rejects_whitespace_target_text() {
        let mut record = good();
        record.target = Text::new(" \r\n　");
        assert_invalid(&record, "target.text");
    }

    #[test]
    fn validate_format_rejects_short_digest() {
        let mut record = good();
        record.source.sha256 = sha256_hex(b"abc")[..63].to_owned();
        assert_invalid(&record, "source.sha256");
    }

    #[test]
    fn validate_format_rejects_uppercase_digest() {
        let mut record = good();
        record.digests.block_target_sha256 = sha256_hex(b"abc").to_uppercase();
        assert_invalid(&record, "digests.block_target_sha256");
    }

    #[test]
    fn validate_format_rejects_bad_origin_digest() {
        let mut record = good();
        record.origin.translation_hash = "sha256:".into();
        assert_invalid(&record, "origin.translation_hash");
    }

    #[test]
    fn validate_format_rejects_bad_watermark_digest() {
        let mut record = good();
        record.watermark.history_sha256 = String::new();
        assert_invalid(&record, "watermark.history_sha256");
    }

    #[test]
    fn validate_format_rejects_empty_finalized_events() {
        let mut record = good();
        record.origin.finalized_events.clear();
        assert_invalid(&record, "finalized_events");
    }

    #[test]
    fn validate_format_rejects_unsorted_finalized_events() {
        let mut record = good();
        record.origin.finalized_events = vec![7, 3];
        assert_invalid(&record, "finalized_events");
        let mut record = good();
        record.origin.finalized_events = vec![3, 3];
        assert_invalid(&record, "finalized_events");
    }

    #[test]
    fn validate_format_rejects_empty_content_type() {
        let mut record = good();
        record.content_type.clear();
        assert_invalid(&record, "content_type");
        let mut record = good();
        record.content_type = " \r\n".into();
        assert_invalid(&record, "content_type");
    }

    #[test]
    fn validate_format_rejects_empty_revision() {
        let mut record = good();
        record.embedding.revision.clear();
        assert_invalid(&record, "embedding.revision");
        // 形状随提供方而异：不像日期或数字也算合格，只要非空。
        let mut record = good();
        record.embedding.revision = "unknown-but-set".into();
        record.validate_format().unwrap();
    }

    #[test]
    fn validate_format_rejects_short_or_uppercase_workspace_id() {
        let mut record = good();
        record.identity.workspace_id = sha256_hex(b"root")[..31].to_owned();
        assert_invalid(&record, "workspace_id");
        let mut record = good();
        record.identity.workspace_id = sha256_hex(b"root")[..32].to_uppercase();
        assert_invalid(&record, "workspace_id");
        let mut record = good();
        record.identity.workspace_id = sha256_hex(b"root")[..33].to_owned();
        assert_invalid(&record, "workspace_id");
    }

    #[test]
    fn validate_format_rejects_event_index_beyond_the_watermark() {
        let mut record = good();
        record.origin.finalized_events = vec![3, 12];
        assert_invalid(&record, "watermark.events");
        // 正好落在水位上也算越界：水位 12 只覆盖下标 0..11。
        let mut record = good();
        record.origin.finalized_events = vec![11];
        record.validate_format().unwrap();
    }

    #[test]
    fn verify_digests_accepts_a_self_consistent_record() {
        good().verify_digests().unwrap();
    }

    #[test]
    fn verify_digests_catches_a_digest_that_does_not_match_its_text() {
        let mut record = good();
        record.source.sha256 = sha256_hex(b"other source");
        assert_eq!(record.validate_format(), Ok(()));
        assert!(matches!(
            record.verify_digests(),
            Err(MemoryError::Invalid(reason)) if reason.contains("source.sha256")
        ));
        let mut record = good();
        record.target.sha256 = sha256_hex(b"other target");
        assert!(matches!(
            record.verify_digests(),
            Err(MemoryError::Invalid(reason)) if reason.contains("target.sha256")
        ));
    }

    #[test]
    fn verify_digests_does_not_claim_to_check_digests_of_other_files() {
        // 块摘要、定稿译稿摘要与嵌入输入摘要都不是记录能自己重算的：改动它们，
        // verify_digests 仍应通过——这些必须由调用方对着真实文件核对。
        let mut record = good();
        record.digests.block_source_sha256 = sha256_hex(b"edited");
        record.digests.block_target_sha256 = sha256_hex(b"edited");
        record.origin.translation_hash = sha256_hex(b"edited");
        record.embedding.input_sha256 = sha256_hex(b"edited");
        record.verify_digests().unwrap();
    }

    #[test]
    fn decode_rejects_bad_json_and_ill_formed_records() {
        assert!(matches!(
            MemoryRecord::decode(b"not JSON"),
            Err(MemoryError::Corrupt(_))
        ));
        let record = good();
        let json = serde_json::to_string(&record).unwrap();
        let mut broken = good();
        broken.target = Text::new(" ");
        let broken_json = serde_json::to_string(&broken).unwrap();
        assert!(matches!(
            MemoryRecord::decode(broken_json.as_bytes()),
            Err(MemoryError::Invalid(_))
        ));
        // 未走校验的裸反序列化会放行这条记录，正说明 decode 有必要。
        assert!(serde_json::from_str::<MemoryRecord>(&broken_json).is_ok());
        assert_eq!(MemoryRecord::decode(json.as_bytes()).unwrap(), record);
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
        let id = workspace_id_from_canonical_text("D:/novels/demo").unwrap();
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!id.contains('/') && !id.contains("novels"));
        assert_eq!(id, sha256_hex(b"D:/novels/demo")[..32].to_owned());
        assert_eq!(
            workspace_id_from_canonical_text("D:/novels/demo").unwrap(),
            id
        );
        assert_ne!(
            workspace_id_from_canonical_text("D:/novels/other").unwrap(),
            id
        );
    }

    #[test]
    fn workspace_id_rejects_empty_input_instead_of_hashing_it() {
        for empty in ["", " ", "\r\n\t"] {
            assert!(
                matches!(
                    workspace_id_from_canonical_text(empty),
                    Err(MemoryError::Invalid(_))
                ),
                "{empty:?}"
            );
        }
    }
}
