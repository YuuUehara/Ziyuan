//! 翻译记忆库的派生索引：记录契约、按 ID 配对、嵌入接口与可重建索引。
//!
//! 记忆库是**可重建的派生数据**，不是译稿的事实来源：删掉不损失任何权威资料，
//! 索引只能从当前有效的定稿重建。本 crate 只含纯数据与纯函数，唯一的例外是
//! [`index`] 里索引自身的发布与读取（只碰工作区根下的 `.ziyuan/memory/`）——
//! 它不联网、不读事件簿、不读译稿，也不依赖 `ziyuan-app`；属于 app 的权威状态与磁盘
//! 事实由适配层传入。

use std::fmt;

pub mod embed;
pub mod index;
pub mod local;
pub mod pair;
pub mod record;
pub mod watermark;

pub use embed::{Embedding, EmbeddingProvider, ModelSignature, Unavailable};
pub use index::{FORMAT_VERSION, MemoryIndex};
pub use local::LocalProvider;
pub use pair::{PairingProblem, ParaPairInput, ParagraphPair, PromptBlockView, pair_paragraphs};
pub use record::{
    Digests, EmbeddingSignature, Identity, MemoryRecord, Origin, Text, Watermark, sha256_hex,
    workspace_id_from_canonical_text,
};
pub use watermark::{WatermarkReader, require_publishable};

/// 记忆库错误。消息只含固定原因、字段名与版本号，不含原文、译文或查询正文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryError {
    /// 记录或输入不合法。
    ///
    /// 只用于**记录与输入字段**的问题：字段形状、摘要格式、身份唯一性。
    /// 配置（如嵌入提供方取值）走 [`Self::Config`]，文件系统形状的问题走 [`Self::Io`]。
    Invalid(String),
    /// 嵌入提供方的配置不对（取值不认识、彼此矛盾）。
    ///
    /// 与 [`Self::Invalid`] 分开：调用方据此知道该去改配置，而不是去查记录。
    Config(String),
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
    /// 工作区有未收尾的受管写入（世代为奇数），本次不得发布。
    ///
    /// 与 [`Self::Stale`] 分开：资料面可能一字未动，缺的是「先把上一次写入收尾」这一步，
    /// 下一步该做什么也不一样。世代号原样带出，便于对上 `ziyuan text recovery status`。
    RecoveryPending { generation: u64 },
    /// 读写索引或译稿时出错。只保留原因文本。
    ///
    /// 不保留 `std::io::Error` 本身：`io::Error` 不是 `Eq`，留下源值就无法让
    /// 本错误类型继续 `Clone, PartialEq, Eq`，测试与会话也就没法直接比对错误。
    Io { reason: String },
}
impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "记忆库记录不合规: {reason}"),
            Self::Config(reason) => write!(
                f,
                "嵌入提供方配置有误: {reason}；可用值：none（默认，不联网）、local（本地确定性模型）"
            ),
            Self::Unavailable => write!(
                f,
                "记忆库不可用: 未配置嵌入提供方；默认不外发，须显式设置 {env}",
                env = embed::PROVIDER_ENV
            ),
            Self::Corrupt(reason) => {
                write!(f, "记忆库索引损坏: {reason}；索引可删，重建即可")
            }
            Self::Newer { found, supported } => write!(
                f,
                "记忆库索引格式 {found} 由更新版本的紫垣写入，本版只支持 {supported}，请升级 ziyuan 后重建索引"
            ),
            Self::SignatureMismatch { expected, found } => write!(
                f,
                "嵌入签名不符: 索引为 {found}，当前为 {expected}，须重建索引"
            ),
            Self::Stale { reason } => {
                write!(
                    f,
                    "记忆库资料在建库期间有变（{reason}），本次不发布索引；请重试"
                )
            }
            Self::RecoveryPending { generation } => write!(
                f,
                "工作区世代 {generation} 有未收尾的受管写入，本次不发布索引；先运行 ziyuan text recovery status 收尾"
            ),
            Self::Io { reason } => write!(f, "记忆库读写失败: {reason}；索引损坏可删去，重建即可"),
        }
    }
}
impl std::error::Error for MemoryError {}
impl From<std::io::Error> for MemoryError {
    fn from(e: std::io::Error) -> Self {
        Self::Io {
            reason: e.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 逐条钉住消息用的字：这些行会进日志与命令行输出，措辞随改动漂移就算回归。
    /// 冒号一律是 ASCII `: `，与 `RecoveryError` 一致；每条都要说清下一步。
    #[test]
    fn display_messages_pin_each_variant() {
        assert_eq!(
            MemoryError::Invalid("b 不能为空".into()).to_string(),
            "记忆库记录不合规: b 不能为空"
        );
        assert_eq!(
            MemoryError::Config("ZIYUAN_MEMORY_PROVIDER 为 \"remote\"，无法识别".into())
                .to_string(),
            "嵌入提供方配置有误: ZIYUAN_MEMORY_PROVIDER 为 \"remote\"，无法识别；可用值：none（默认，不联网）、local（本地确定性模型）"
        );
        assert_eq!(
            MemoryError::Unavailable.to_string(),
            "记忆库不可用: 未配置嵌入提供方；默认不外发，须显式设置 ZIYUAN_MEMORY_PROVIDER"
        );
        assert_eq!(
            MemoryError::Corrupt("integrity".into()).to_string(),
            "记忆库索引损坏: integrity；索引可删，重建即可"
        );
        assert_eq!(
            MemoryError::Newer {
                found: 2,
                supported: 1
            }
            .to_string(),
            "记忆库索引格式 2 由更新版本的紫垣写入，本版只支持 1，请升级 ziyuan 后重建索引"
        );
        assert_eq!(
            MemoryError::SignatureMismatch {
                expected: "p/m/1".into(),
                found: "p/m/0".into()
            }
            .to_string(),
            "嵌入签名不符: 索引为 p/m/0，当前为 p/m/1，须重建索引"
        );
        assert_eq!(
            MemoryError::Stale {
                reason: "translation_edited".into()
            }
            .to_string(),
            "记忆库资料在建库期间有变（translation_edited），本次不发布索引；请重试"
        );
        assert_eq!(
            MemoryError::RecoveryPending { generation: 7 }.to_string(),
            "工作区世代 7 有未收尾的受管写入，本次不发布索引；先运行 ziyuan text recovery status 收尾"
        );
        assert_eq!(
            MemoryError::Io {
                reason: "not found".into()
            }
            .to_string(),
            "记忆库读写失败: not found；索引损坏可删去，重建即可"
        );
    }

    /// 消息是「固定前缀 + 原因」，不另加引号、路径或事件内容，也不重复原因。
    #[test]
    fn display_adds_nothing_beyond_the_reason() {
        let reason = "记录无法解码: EOF while parsing";
        let message = MemoryError::Corrupt(reason.into()).to_string();
        assert_eq!(
            message,
            format!("记忆库索引损坏: {reason}；索引可删，重建即可")
        );
        assert_eq!(message.matches(reason).count(), 1, "{message}");
        assert!(!message.contains(['/', '\\', '"']), "{message}");
    }

    /// `From<io::Error>` 只留原因文本：不保留源值，错误类型才继续是 `Eq`／`Clone`。
    #[test]
    fn io_errors_become_a_comparable_reason() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let error = MemoryError::from(io);
        assert_eq!(
            error,
            MemoryError::Io {
                reason: "gone".into()
            }
        );
        assert_eq!(error.clone(), error);
        assert!(std::error::Error::source(&error).is_none());
    }
}
