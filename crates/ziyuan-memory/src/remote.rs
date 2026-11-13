//! 远程嵌入。未授权时任何调用都不接触传输层。
//!
//! 传输由调用方注入。本模块不读取 `ZIYUAN_LLM_*`，也不保存译文。

use crate::MemoryError;
use crate::embed::{Embedding, EmbeddingProvider, ModelSignature};

/// 一次嵌入调用。正文只存在于这次借用里，错误信息不得复制它。
pub struct EmbedCall<'a> {
    pub endpoint: &'a str,
    pub model: &'a str,
    pub api_key: &'a str,
    pub texts: &'a [&'a str],
}

pub trait EmbedTransport: Send + Sync {
    fn post_embeddings(&self, request: &EmbedCall<'_>) -> Result<Vec<Vec<f32>>, MemoryError>;
}

pub struct RemoteProvider {
    signature: ModelSignature,
    endpoint: String,
    api_key: String,
    transport: Box<dyn EmbedTransport>,
    armed: bool,
}

impl RemoteProvider {
    pub fn armed(
        endpoint: String,
        model: String,
        revision: String,
        dim: usize,
        api_key: String,
        transport: Box<dyn EmbedTransport>,
    ) -> Result<Self, MemoryError> {
        let signature = ModelSignature {
            provider: "remote".into(),
            model,
            revision,
            dim,
            metric: "cosine".into(),
        };
        signature.validate().map_err(MemoryError::Invalid)?;
        if !(endpoint.starts_with("https://") || endpoint.starts_with("http://")) {
            return Err(MemoryError::Config(
                "远程嵌入端点须以 http:// 或 https:// 开头".into(),
            ));
        }
        Ok(Self {
            signature,
            endpoint,
            api_key,
            transport,
            armed: true,
        })
    }
}

impl EmbeddingProvider for RemoteProvider {
    fn signature(&self) -> &ModelSignature {
        &self.signature
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Embedding>, MemoryError> {
        if !self.armed {
            return Err(MemoryError::Config("远程嵌入未授权，已拒绝外发".into()));
        }
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        if texts.iter().any(|text| text.trim().is_empty()) {
            return Err(MemoryError::Invalid("嵌入输入不能为空或只有空白".into()));
        }
        let vectors = self.transport.post_embeddings(&EmbedCall {
            endpoint: &self.endpoint,
            model: &self.signature.model,
            api_key: &self.api_key,
            texts,
        })?;
        if vectors.len() != texts.len() {
            return Err(MemoryError::Config(
                "远程嵌入返回的条数与原文不符，未保留响应正文".into(),
            ));
        }
        vectors
            .into_iter()
            .map(|vector| {
                if vector.len() != self.signature.dim {
                    return Err(MemoryError::Config(
                        "远程嵌入维度与授权不符，未保留向量正文".into(),
                    ));
                }
                Ok(Embedding { vector })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Shared(Arc<AtomicUsize>);
    impl EmbedTransport for Shared {
        fn post_embeddings(&self, request: &EmbedCall<'_>) -> Result<Vec<Vec<f32>>, MemoryError> {
            assert_eq!(request.texts, ["夜が明けた"]);
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(vec![vec![0.0, 1.0]])
        }
    }

    #[test]
    fn unarmed_embed_does_not_call_transport() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = RemoteProvider {
            signature: ModelSignature {
                provider: "remote".into(),
                model: "m".into(),
                revision: "1".into(),
                dim: 2,
                metric: "cosine".into(),
            },
            endpoint: "https://embed.example".into(),
            api_key: "secret".into(),
            transport: Box::new(Shared(calls.clone())),
            armed: false,
        };
        let error = provider.embed(&["夜が明けた"]).unwrap_err();
        assert!(error.to_string().contains("未授权"), "{error}");
        assert!(error.to_string().contains("拒绝外发"), "{error}");
        assert!(!error.to_string().contains("夜が"), "{error}");
        assert!(!error.to_string().contains("secret"), "{error}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(provider.embed(&[]).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn armed_embed_sends_the_source_once_and_empty_input_does_not() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = RemoteProvider::armed(
            "https://embed.example/v1".into(),
            "m".into(),
            "1".into(),
            2,
            "secret".into(),
            Box::new(Shared(calls.clone())),
        )
        .unwrap();
        assert!(provider.embed(&[]).unwrap().is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            provider.embed(&["夜が明けた"]).unwrap()[0].vector,
            vec![0.0, 1.0]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
