//! 测试与离线复现用的固定向量。不联网，也不理解文意。
//!
//! 算法与 `local` 相同，但维度是 32、模型名是 `deterministic-v1`。
//! 因此它不能混用 `char-hash-v1` 的索引；换提供方后必须重建。

use crate::MemoryError;
use crate::embed::{Embedding, EmbeddingProvider, ModelSignature};

pub const PROVIDER: &str = "deterministic";
pub const MODEL: &str = "deterministic-v1";
pub const REVISION: &str = "1";
pub const DIM: usize = 32;
pub const METRIC: &str = "cosine";

pub struct DeterministicProvider {
    signature: ModelSignature,
}

impl DeterministicProvider {
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

impl Default for DeterministicProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbeddingProvider for DeterministicProvider {
    fn signature(&self) -> &ModelSignature {
        &self.signature
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Embedding>, MemoryError> {
        texts.iter().copied().map(embed_text).collect()
    }
}

fn embed_text(text: &str) -> Result<Embedding, MemoryError> {
    if text.trim().is_empty() {
        return Err(MemoryError::Invalid("嵌入输入不能为空或只有空白".into()));
    }
    let mut accumulators = [0.0f32; DIM];
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
    if sum_squares == 0.0 {
        return Err(MemoryError::Invalid("嵌入输入产生了零向量".into()));
    }
    let norm = sum_squares.sqrt();
    Ok(Embedding {
        vector: accumulators.into_iter().map(|value| value / norm).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_text_is_stable_and_blank_is_rejected_without_network() {
        let provider = DeterministicProvider::new();
        let first = provider.embed(&["夜が明けた"]).unwrap();
        let second = provider.embed(&["夜が明けた"]).unwrap();
        assert_eq!(first, second);
        assert_eq!(first[0].vector.len(), DIM);
        assert!(provider.embed(&["  "]).is_err());
        assert!(provider.embed(&[]).unwrap().is_empty());
    }
}
