//! 嵌入接口：模型签名、向量与默认的「不可用」提供方。
//!
//! 嵌入**默认零外发**。唯一的配置入口是 [`PROVIDER_ENV`]（`ZIYUAN_MEMORY_PROVIDER`）：
//! 未设置、空串或 `none` 一律是 [`Unavailable`]，任何调用都返回
//! [`MemoryError::Unavailable`]，绝不回落到任何默认端点。本模块**不读** `ZIYUAN_LLM_*`
//! ——那是聊天模型的前缀，缺 `ZIYUAN_LLM_BASE_URL` 时会落到远程 URL，与「零外发」相悖。
//!
//! 没有可用提供方时必须明确报不可用，不能静默降级成「没有参考也不说」。

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::MemoryError;
use crate::record::EmbeddingSignature;

/// 提供方环境变量：本 crate 唯一的配置入口。
pub const PROVIDER_ENV: &str = "ZIYUAN_MEMORY_PROVIDER";

/// 一条文本的嵌入向量。
///
/// 由提供方决定维度；首版本地提供方为 256 维、已 L2 归一化（见 [`crate::local`]）。
#[derive(Clone, Debug, PartialEq)]
pub struct Embedding {
    pub vector: Vec<f32>,
}

/// 模型签名：提供方、模型、修订、维度、度量，任一项不同即索引不可再用。
///
/// 与记录里的 [`EmbeddingSignature`] 只差 `input_sha256`：那一项属于**这一条记录**的输入，
/// 不属于模型；因此比对用 [`Self::matches`]。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ModelSignature {
    pub provider: String,
    pub model: String,
    /// 模型修订或可用版本信息。
    pub revision: String,
    pub dim: usize,
    pub metric: String,
}

impl ModelSignature {
    /// 与记录里的嵌入签名逐项比对（不含 `input_sha256`）。
    pub fn matches(&self, other: &EmbeddingSignature) -> bool {
        self.provider == other.provider
            && self.model == other.model
            && self.revision == other.revision
            && self.dim == other.dim
            && self.metric == other.metric
    }

    /// 五项都要可用：字符串非空（不全是空白）、维度非 0。口径与
    /// [`crate::MemoryRecord::validate_format`] 的 `embedding.*` 各项一致。
    pub fn validate(&self) -> Result<(), MemoryError> {
        for (name, value) in [
            ("provider", self.provider.as_str()),
            ("model", self.model.as_str()),
            ("revision", self.revision.as_str()),
            ("metric", self.metric.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(MemoryError::Invalid(format!("embedding.{name} 不得为空")));
            }
        }
        if self.dim == 0 {
            return Err(MemoryError::Invalid("embedding.dim 不得为 0".into()));
        }
        Ok(())
    }
}

/// 嵌入提供方：给出签名，并批量转向量。
pub trait EmbeddingProvider {
    fn signature(&self) -> &ModelSignature;
    /// 批量嵌入；**空输入返回空**，不报错。任何一条不合法即整批报错，不返回部分结果。
    fn embed(&self, texts: &[&str]) -> Result<Vec<Embedding>, MemoryError>;
}

/// 默认提供方：任何嵌入请求都报不可用，**绝不触网**。
///
/// 它的签名是 `provider = "none"`、`model = ""`、`dim = 0`。这不是「一个很弱的模型」，
/// 而是「没有模型」的类型级证明：[`crate::MemoryRecord::validate_format`] 拒绝
/// `embedding.dim == 0`，因此**任何记录都不可能由本提供方的签名建成并入库**，
/// 「没有提供方」也就漏不进索引。别把 `dim` 改成非 0 来「让测试好过」。
pub struct Unavailable;

impl Unavailable {
    pub fn new() -> Self {
        Self
    }
}

impl Default for Unavailable {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbeddingProvider for Unavailable {
    fn signature(&self) -> &ModelSignature {
        static SIGNATURE: OnceLock<ModelSignature> = OnceLock::new();
        SIGNATURE.get_or_init(|| ModelSignature {
            provider: "none".into(),
            model: String::new(),
            revision: String::new(),
            dim: 0,
            metric: String::new(),
        })
    }

    fn embed(&self, _texts: &[&str]) -> Result<Vec<Embedding>, MemoryError> {
        Err(MemoryError::Unavailable)
    }
}

/// 按 [`PROVIDER_ENV`] 选提供方。
///
/// 只读 `ZIYUAN_MEMORY_PROVIDER`：未设置、空串或 `none` ⇒ [`Unavailable`]；
/// `local` ⇒ [`crate::local::LocalProvider`]；其余值 ⇒ [`MemoryError::Invalid`] 并列出
/// 可接受的值。**绝不**读 `ZIYUAN_LLM_*`，也**绝不**回落到任何 URL。
pub fn from_env() -> Result<Box<dyn EmbeddingProvider>, MemoryError> {
    let raw = std::env::var(PROVIDER_ENV).unwrap_or_default();
    match raw.trim() {
        "" | "none" => Ok(Box::new(Unavailable)),
        "local" => Ok(Box::new(crate::local::LocalProvider::new())),
        other => Err(MemoryError::Invalid(format!(
            "{PROVIDER_ENV} 只能是 none 或 local，而不是 {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{
        Digests, Identity, MemoryRecord, Origin, Text, Watermark, sha256_hex,
        workspace_id_from_canonical_text,
    };

    /// 子进程情形标记：见 [`env_case_in_child`]。
    const ENV_CHILD: &str = "ZIYUAN_MEMORY_ENV_CHILD";

    /// 默认提供方的签名钉在「没有模型」上：`dim == 0`、`provider == "none"`。
    #[test]
    fn unavailable_signature_says_there_is_no_model() {
        let provider = Unavailable::new();
        let signature = provider.signature();
        assert_eq!(signature.provider, "none");
        assert_eq!(signature.model, "");
        assert_eq!(signature.dim, 0);
        // 也绝不是「某个恰好不可用的远程端点」。
        assert!(!signature.provider.contains("http"));
    }

    /// 没有提供方时每次调用都报不可用；空输入也不例外（不会「成功返回空」）。
    #[test]
    fn unavailable_always_errors() {
        let provider = Unavailable::new();
        for texts in [vec![], vec!["夜が明けた"], vec!["a", "b"]] {
            assert_eq!(
                provider.embed(&texts),
                Err(MemoryError::Unavailable),
                "{texts:?}"
            );
        }
    }

    /// 类型级保证：拿 `Unavailable` 的签名建不出合法记录。
    ///
    /// 这条用例是「没有提供方」漏不进索引的唯一证明：`validate_format` 拒绝 `dim == 0`。
    #[test]
    fn unavailable_signature_cannot_build_a_valid_record() {
        let signature = Unavailable::new().signature().clone();
        let record = MemoryRecord {
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
            source: Text::new("夜が明けた"),
            target: Text::new("天亮了"),
            digests: Digests {
                block_source_sha256: sha256_hex(b"block-source"),
                block_target_sha256: sha256_hex(b"block-target"),
            },
            origin: Origin {
                finalized_events: vec![3],
                translation_hash: sha256_hex(b"zh.json"),
                context_version: 2,
                zhupi_version: 1,
            },
            embedding: EmbeddingSignature {
                provider: signature.provider.clone(),
                model: signature.model.clone(),
                revision: signature.revision.clone(),
                dim: signature.dim,
                metric: signature.metric.clone(),
                input_sha256: sha256_hex(b"input"),
            },
            watermark: Watermark {
                events: 12,
                history_sha256: sha256_hex(b"history"),
                history_bytes: 4096,
                generation: 8,
            },
        };
        match record.validate_format() {
            Err(MemoryError::Invalid(reason)) => {
                assert!(reason.starts_with("embedding."), "{reason}");
            }
            other => panic!("预期因嵌入字段被拒，得到 {other:?}"),
        }
        // 把其余空洞补上，`dim == 0` 仍是独立的硬门槛：这条保证不靠「模型名为空」兜底。
        let mut only_dim_missing = signature.clone();
        only_dim_missing.model = "none".into();
        only_dim_missing.revision = "0".into();
        only_dim_missing.metric = "cosine".into();
        match only_dim_missing.validate() {
            Err(MemoryError::Invalid(reason)) => {
                assert!(reason.contains("embedding.dim"), "{reason}");
            }
            other => panic!("预期因 embedding.dim 被拒，得到 {other:?}"),
        }
        assert!(signature.validate().is_err());
    }

    /// 逐项比对：provider/model/revision/dim/metric 任一项不同即不匹配。
    #[test]
    fn signature_matches_compares_all_five_fields() {
        let base = ModelSignature {
            provider: "local".into(),
            model: "char-hash-v1".into(),
            revision: "1".into(),
            dim: 256,
            metric: "cosine".into(),
        };
        let as_record = |s: &ModelSignature| EmbeddingSignature {
            provider: s.provider.clone(),
            model: s.model.clone(),
            revision: s.revision.clone(),
            dim: s.dim,
            metric: s.metric.clone(),
            input_sha256: sha256_hex(b"input"),
        };
        assert!(base.matches(&as_record(&base)));

        let mut other = as_record(&base);
        other.provider = "remote".into();
        assert!(!base.matches(&other));
        let mut other = as_record(&base);
        other.model = "char-hash-v0".into();
        assert!(!base.matches(&other));
        let mut other = as_record(&base);
        other.revision = "2".into();
        assert!(!base.matches(&other));
        let mut other = as_record(&base);
        other.dim = 512;
        assert!(!base.matches(&other));
        let mut other = as_record(&base);
        other.metric = "dot".into();
        assert!(!base.matches(&other));
        // `input_sha256` 不在签名里：它变了不算签名不符。
        let mut other = as_record(&base);
        other.input_sha256 = sha256_hex(b"another input");
        assert!(base.matches(&other));
    }

    /// 子进程入口（三种环境情形由下面三个 `from_env_*` 用例启动）；直接运行时什么都不做。
    ///
    /// 为什么要子进程：本 crate 在 edition 2024 且工作区 `unsafe_code = "forbid"`，而
    /// `std::env::set_var` 在该 edition 是 `unsafe fn`，用不了。照
    /// `crates/ziyuan-storage/src/json_file.rs` 的既有做法，用 `Command::env`／`env_remove`
    /// 起子进程，既不碰父进程环境，也不必把用例串行化。
    #[test]
    fn env_case_in_child() {
        let Some(case) = std::env::var_os(ENV_CHILD) else {
            return;
        };
        let case = case.to_string_lossy().into_owned();
        match case.as_str() {
            "unset" => {
                assert!(
                    std::env::var_os(PROVIDER_ENV).is_none(),
                    "子进程不该带上 {PROVIDER_ENV}"
                );
                let provider = from_env().map_err(|e| e.to_string()).unwrap();
                assert_eq!(provider.signature().provider, "none");
                assert_eq!(
                    provider.embed(&["夜が明けた"]),
                    Err(MemoryError::Unavailable)
                );
            }
            "empty" => {
                assert_eq!(std::env::var(PROVIDER_ENV).unwrap(), "");
                let provider = from_env().map_err(|e| e.to_string()).unwrap();
                assert_eq!(provider.signature().provider, "none");
            }
            "local" => {
                let provider = from_env().map_err(|e| e.to_string()).unwrap();
                assert_eq!(provider.signature().provider, "local");
                assert_eq!(provider.signature().dim, 256);
                let embedded = provider.embed(&["夜が明けた"]).unwrap();
                assert_eq!(embedded.len(), 1);
                assert_eq!(embedded[0].vector.len(), 256);
            }
            "remote" => match from_env() {
                Err(MemoryError::Invalid(reason)) => {
                    // 报错须点名变量与可接受的值，并且回显那个不被接受的值。
                    assert!(reason.contains(PROVIDER_ENV), "{reason}");
                    assert!(reason.contains("none"), "{reason}");
                    assert!(reason.contains("local"), "{reason}");
                    assert!(reason.contains("remote"), "{reason}");
                }
                Err(other) => panic!("预期不接受的提供方被拒，得到错误 {other}"),
                Ok(_) => panic!("预期不接受的提供方被拒，却选出了一个提供方"),
            },
            other => panic!("未知的子进程情形 {other}"),
        }
    }

    fn run_env_case(case: &str, env: &[(&str, &str)], remove: &[&str]) {
        let exe = std::env::current_exe().unwrap();
        let mut command = std::process::Command::new(exe);
        command
            .args([
                "--exact",
                "embed::tests::env_case_in_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ENV_CHILD, case)
            // 父进程若碰巧设了提供方变量，不该漏进子进程。
            .env_remove(PROVIDER_ENV);
        for name in remove {
            command.env_remove(name);
        }
        for (name, value) in env {
            command.env(name, value);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "情形 {case} 失败: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// 未设置、空串即不可用；即便身上带着 `ZIYUAN_LLM_*` 也不许回落到远程端点。
    #[test]
    fn from_env_defaults_to_unavailable() {
        run_env_case(
            "unset",
            &[
                ("ZIYUAN_LLM_BASE_URL", "https://example.invalid"),
                ("ZIYUAN_LLM_PROVIDER", "api"),
            ],
            &[],
        );
        run_env_case("empty", &[(PROVIDER_ENV, "")], &[]);
    }

    /// 不认识的值必须报错并列出可接受的值，不能悄悄当成 `Unavailable`。
    #[test]
    fn from_env_rejects_unknown_provider() {
        run_env_case("remote", &[(PROVIDER_ENV, "remote")], &[]);
    }

    /// `local` 选中确定性本地提供方。
    #[test]
    fn from_env_local_selects_the_local_provider() {
        run_env_case("local", &[(PROVIDER_ENV, "local")], &[]);
    }
}
