//! 翻译记忆库的派生索引：记录契约与按 ID 配对。
//!
//! 记忆库是**可重建的派生数据**，不是译稿的事实来源：删掉不损失任何权威资料，
//! 索引只能从当前有效的定稿重建。本 crate 只含纯数据与纯函数——不读文件、不联网、
//! 不读事件簿，也不依赖 `ziyuan-app`；属于 app 的权威状态与磁盘事实由适配层传入。

use std::fmt;

pub mod pair;
pub mod record;

pub use pair::{PairingProblem, ParaPairInput, ParagraphPair, PromptBlockView, pair_paragraphs};
pub use record::{
    Digests, EmbeddingSignature, Identity, MemoryRecord, Origin, Text, Watermark, sha256_hex,
    workspace_id,
};

/// 记忆库错误。消息只含固定原因、字段名与版本号，不含原文、译文或查询正文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryError {
    /// 记录或输入不合法。
    Invalid(String),
    /// 没有可用的嵌入提供方；这是默认状态，且绝不回落到任何默认端点。
    Unavailable,
    /// 索引损坏、不完整或无法解码。
    Corrupt(String),
    /// 索引由更新版本的紫垣写入，本版只支持 `supported`。
    Newer { found: u64, supported: u64 },
    /// 索引的嵌入签名与当前提供方不一致，须重建。
    SignatureMismatch { expected: String, found: String },
    /// 资料在构建期间有变，本次不得发布。
    Stale { reason: String },
}
impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "记忆库记录不合法: {reason}"),
            Self::Unavailable => write!(f, "记忆库不可用：未配置可用的嵌入提供方"),
            Self::Corrupt(reason) => write!(f, "记忆库索引损坏: {reason}"),
            Self::Newer { found, supported } => write!(
                f,
                "记忆库索引格式 {found} 由更新版本的紫垣写入，本版只支持 {supported}，请升级 ziyuan 后重建索引"
            ),
            Self::SignatureMismatch { expected, found } => write!(
                f,
                "嵌入签名不符：索引为 {found}，当前为 {expected}，须重建索引"
            ),
            Self::Stale { reason } => {
                write!(f, "记忆库资料已变（{reason}），本次不发布索引")
            }
        }
    }
}
impl std::error::Error for MemoryError {}
