//! 确定性本地嵌入提供方：零外发、跨进程跨平台得到同一向量。
//!
//! 签名钉死为 `provider = "local"`、`model = "char-hash-v1"`、`revision = "1"`、
//! `dim = 256`、`metric = "cosine"`。**算法本身就是契约**：改了算法就必须改
//! `revision`（或 `model`），否则旧索引会被当成同一个模型算出的向量继续用，
//! 而那种错配没有任何别的地方能发现。
//!
//! # 算法（不得改动）
//!
//! 1. 空文本或只有空白的文本报 [`MemoryError::Invalid`]，绝不返回零向量。
//! 2. 按**字符**（`char`，不是字节）遍历，第 `i` 个字符 `c` 取
//!    `sha256_hex(format!("{i}:{c}"))`。位置进哈希，同一个字符在不同位置落到不同桶。
//! 3. 前 4 字节按大端解成 `u32`，对 256 取模得桶号；第 5 字节（下标 4）`% 2` 定符号：
//!    偶数 `+1.0`，奇数 `-1.0`。
//! 4. 累加到 `[f32; 256]`，再 L2 归一化。
//!
//! 归一化后余弦相似度即点积，检索侧不必再假设别的范数。
//!
//! # 不做任何文本规范化（这是契约，不是疏漏）
//!
//! 本提供方**不做 Unicode 规范化，也不裁掉首尾空白**：输入按 `char` 逐个哈希，
//! 完全相同的字符序列才得到相同向量。因此 `が`（U+304C，单字符）与 `か` + `゚`
//! （U+304B + U+3099，组合序列）哈希结果不同，`"夜が明けた "`（带尾随空格）与
//! `"夜が明けた"` 也不同。
//!
//! 推论：**查询与资料来源必须在同一个形态里**。若资料侧做过 NFKC／NFD 规范化或
//! 首尾裁剪，检索侧就必须用同一种形态；否则同义的两串字会被算成无关的向量，
//! 而这种「检索不到」在索引里看不出任何异常。规范化的责任在调用方，不在本模块——
//! 在这里偷偷规范化会让索引与查询各自按不同规则变脸，反而更难对齐。

use crate::MemoryError;
use crate::embed::{Embedding, EmbeddingProvider, ModelSignature};

/// 提供方名。
pub const PROVIDER: &str = "local";
/// 模型名。
pub const MODEL: &str = "char-hash-v1";
/// 模型修订：算法一改就必须改这里。
pub const REVISION: &str = "1";
/// 维度。
pub const DIM: usize = 256;
/// 度量：向量已归一化，余弦即点积。
pub const METRIC: &str = "cosine";

/// 确定性本地提供方。
pub struct LocalProvider {
    signature: ModelSignature,
}

impl LocalProvider {
    pub fn new() -> Self {
        Self {
            signature: ModelSignature {
                provider: PROVIDER.into(),
                model: MODEL.into(),
                revision: REVISION.into(),
                dim: DIM,
                metric: METRIC.into(),
            },
        }
    }
}

impl Default for LocalProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbeddingProvider for LocalProvider {
    fn signature(&self) -> &ModelSignature {
        &self.signature
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Embedding>, MemoryError> {
        texts.iter().map(|text| embed_text(text)).collect()
    }
}

/// 一条文本的向量；算法见模块文档。
fn embed_text(text: &str) -> Result<Embedding, MemoryError> {
    if text.trim().is_empty() {
        return Err(MemoryError::Invalid("嵌入输入不能为空或只有空白".into()));
    }
    let mut accumulators = [0.0f32; DIM];
    // 按字符遍历：`i` 是字符下标（不是字节偏移），位置进哈希。
    for (index, character) in text.chars().enumerate() {
        let digest = ring::digest::digest(
            &ring::digest::SHA256,
            format!("{index}:{character}").as_bytes(),
        );
        let digest = digest.as_ref();
        let bucket =
            u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) as usize % DIM;
        let sign = if digest[4] % 2 == 0 { 1.0 } else { -1.0 };
        accumulators[bucket] += sign;
    }
    let sum_squares: f32 = accumulators.iter().map(|value| value * value).sum();
    // 非空文本必然至少落一个桶，范数不可能为 0；这里只是把「绝不返回零向量」写成硬条件。
    if sum_squares == 0.0 {
        return Err(MemoryError::Invalid("嵌入输入产生了零向量".into()));
    }
    let norm = sum_squares.sqrt();
    Ok(Embedding {
        vector: accumulators.iter().map(|value| value / norm).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 固定输入一：英文两句话，56 个字符。
    const PINNED_INPUT: &str = "The train arrived at the small station just before dawn. Nobody was waiting on the platform.";
    /// 固定输入二：三个汉字，便于逐桶核对。
    const SHORT_INPUT: &str = "天亮了";

    fn provider() -> LocalProvider {
        LocalProvider::new()
    }

    fn one(text: &str) -> Vec<f32> {
        provider().embed(&[text]).unwrap().remove(0).vector
    }

    fn assert_close(actual: &[f32], expected: &[f32], what: &str) {
        assert_eq!(actual.len(), expected.len(), "{what} 长度不符");
        for (index, (a, e)) in actual.iter().zip(expected).enumerate() {
            assert!(
                (a - e).abs() <= 1e-6,
                "{what} 第 {index} 项: 期望 {e}，得到 {a}"
            );
        }
    }

    #[test]
    fn local_signature_is_the_documented_one() {
        let signature = provider().signature().clone();
        assert_eq!(signature.provider, "local");
        assert_eq!(signature.model, "char-hash-v1");
        assert_eq!(signature.revision, "1");
        assert_eq!(signature.dim, 256);
        assert_eq!(signature.metric, "cosine");
        signature.validate().unwrap();
    }

    #[test]
    fn local_embedding_is_deterministic_across_calls() {
        let first = one(PINNED_INPUT);
        let second = one(PINNED_INPUT);
        let third = provider().embed(&[PINNED_INPUT]).unwrap().remove(0).vector;
        assert_eq!(first, second);
        assert_eq!(first, third);
        assert_eq!(first.len(), DIM);
    }

    /// 跨进程／跨平台的保证在这里：断言**具体数值**，不只断言两次相等。
    ///
    /// 期望值由算法独立算出（一次性计算后钉在此处）；改算法、改桶数、改符号位都会失败。
    #[test]
    fn local_embedding_matches_a_pinned_expected_vector() {
        let vector = one(PINNED_INPUT);
        // 前 8 项：第 4、7 桶各 -1/√80，其余为 0。
        assert_close(
            &vector[..8],
            &[0.0, 0.0, 0.0, 0.0, -0.111_803_4, 0.0, 0.0, -0.111_803_4],
            "PINNED_INPUT 前 8 项",
        );

        // 逐桶核对短输入：只有三个桶非零，符号与幅度都钉住。
        let short = one(SHORT_INPUT);
        let nonzero: Vec<(usize, f32)> = short
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, value)| *value != 0.0)
            .collect();
        assert_eq!(nonzero.len(), 3, "{nonzero:?}");
        assert_eq!(nonzero[0].0, 19);
        assert_eq!(nonzero[1].0, 74);
        assert_eq!(nonzero[2].0, 236);
        assert_close(&[nonzero[0].1], &[0.577_350_26], "桶 19");
        assert_close(&[nonzero[1].1], &[-0.577_350_26], "桶 74");
        assert_close(&[nonzero[2].1], &[-0.577_350_26], "桶 236");
        // 前 8 项在这条输入上全为 0：这正是「按字符位置取哈希」的形态。
        assert_close(&short[..8], &[0.0; 8], "SHORT_INPUT 前 8 项");
    }

    #[test]
    fn local_embedding_differs_for_different_text() {
        let pairs = [
            ("夜が明けた", "天亮了"),
            ("The night ended.", "The night ended!"),
            // 字符相同、顺序不同：位置进哈希，向量必须不同。
            ("ab", "ba"),
        ];
        for (left, right) in pairs {
            assert_ne!(one(left), one(right), "{left} 与 {right} 不该相同");
        }
    }

    #[test]
    fn local_embedding_is_normalised() {
        for text in [PINNED_INPUT, SHORT_INPUT, "夜が明けた。町は静かだった。"] {
            let vector = one(text);
            assert_eq!(vector.len(), DIM);
            let sum_squares: f32 = vector.iter().map(|value| value * value).sum();
            assert!(
                (sum_squares - 1.0).abs() < 1e-5,
                "{text} 的范数平方为 {sum_squares}"
            );
        }
    }

    #[test]
    fn local_rejects_empty_and_whitespace() {
        for text in ["", " ", "\t", "\n  \r\n", "　"] {
            match provider().embed(&[text]) {
                Err(MemoryError::Invalid(reason)) => {
                    assert!(reason.contains("嵌入输入"), "{reason}");
                }
                other => panic!("{text:?} 预期被拒，得到 {other:?}"),
            }
        }
        // 批量交付：空输入是空结果，不是错误；一条不合法即整批报错。
        assert_eq!(provider().embed(&[]).unwrap(), vec![]);
        assert!(provider().embed(&["夜が明けた", " "]).is_err());
    }
}
