//! 先限定授权范围，再以字符双元组、余弦相似度和 RRF 混合排序。
//!
//! 分数只表示排序，不是译文质量；不设质量阈值。本地字符哈希也不是语义模型。

use std::collections::BTreeSet;

use crate::{Embedding, EmbeddingProvider, MemoryError, MemoryIndex, MemoryRecord};

/// 调用方明确授予的检索范围。当前项目也必须列在授权集合内。
/// 授权只在同一工作区、语种对与内容种类内生效；本模块不自行扩大范围。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchScope {
    pub workspace_id: String,
    pub current_project: String,
    pub authorized_projects: BTreeSet<String>,
    pub source_language: String,
    pub target_language: String,
    pub content_type: String,
}

/// 一条命中及其排序依据。两个名次从 1 开始；分数不是质量保证。
#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit<'a> {
    pub record: &'a MemoryRecord,
    pub keyword_score: f64,
    pub cosine_score: f64,
    pub keyword_rank: usize,
    pub vector_rank: usize,
    pub score: f64,
}

/// 在授权候选集上分别排序后，用 `1/(60+名次)` 的和融合，最后取 `limit` 条。
///
/// `limit` 须在 1..=50；空查询与未授权的当前项目均报错。无候选时仍检查提供方，
/// 默认的不可用提供方绝不会被当成「没有命中」。只把范围内的原文交给提供方。
/// 同分按项目、章节、块、段落、工作区的身份顺序排列，与索引输入顺序无关。
pub fn search<'a>(
    index: &'a MemoryIndex,
    provider: &dyn EmbeddingProvider,
    query: &str,
    scope: &SearchScope,
    limit: usize,
) -> Result<Vec<SearchHit<'a>>, MemoryError> {
    if !(1..=50).contains(&limit) {
        return Err(MemoryError::Invalid("检索条数须在 1 到 50 之间".into()));
    }
    if query.trim().is_empty() {
        return Err(MemoryError::Invalid("检索内容不能为空".into()));
    }
    for value in [
        &scope.workspace_id,
        &scope.current_project,
        &scope.source_language,
        &scope.target_language,
        &scope.content_type,
    ] {
        if value.trim().is_empty() {
            return Err(MemoryError::Invalid("检索范围字段不能为空".into()));
        }
    }
    if !scope.authorized_projects.contains(&scope.current_project)
        || scope
            .authorized_projects
            .iter()
            .any(|p| p.trim().is_empty())
    {
        return Err(MemoryError::Invalid(
            "当前项目须在明确授权的项目集合内，项目名不能为空".into(),
        ));
    }

    // 先调用查询嵌入：Unavailable 的占位签名并不合法，但真正原因是没有提供方。
    let query_vectors = provider.embed(&[query])?;
    let signature = provider.signature();
    signature.validate().map_err(MemoryError::Config)?;
    if signature.metric != "cosine" {
        return Err(MemoryError::Config("混合检索只支持 cosine 度量".into()));
    }
    index.signature.validate().map_err(MemoryError::Corrupt)?;
    if &index.signature != signature {
        return Err(MemoryError::SignatureMismatch {
            expected: signature_label(signature),
            found: signature_label(&index.signature),
        });
    }
    let query_vectors = checked_vectors(query_vectors, 1, signature.dim)?;
    let query_vector = &query_vectors[0];

    let mut candidates: Vec<_> = index
        .records
        .iter()
        .filter(|record| {
            record.identity.workspace_id == scope.workspace_id
                && scope.authorized_projects.contains(&record.identity.project)
                && record.source_language == scope.source_language
                && record.target_language == scope.target_language
                && record.content_type == scope.content_type
        })
        .collect();
    candidates.sort_by_key(|record| identity_key(record));
    for (i, record) in candidates.iter().enumerate() {
        record.validate_format()?;
        record.verify_digests()?;
        if !signature.matches(&record.embedding) {
            return Err(MemoryError::Corrupt(
                "候选记录的嵌入签名与索引不一致".into(),
            ));
        }
        if i > 0 && record.identity == candidates[i - 1].identity {
            return Err(MemoryError::Corrupt("候选记录的段落身份重复".into()));
        }
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let texts: Vec<_> = candidates.iter().map(|r| r.source.text.as_str()).collect();
    let vectors = checked_vectors(provider.embed(&texts)?, texts.len(), signature.dim)?;
    let query_terms = bigrams(query);
    let mut hits: Vec<_> = candidates
        .into_iter()
        .zip(vectors)
        .map(|(record, vector)| SearchHit {
            record,
            keyword_score: jaccard(&query_terms, &bigrams(&record.source.text)),
            cosine_score: cosine(query_vector, &vector),
            keyword_rank: 0,
            vector_rank: 0,
            score: 0.0,
        })
        .collect();
    hits.sort_by(|a, b| {
        b.keyword_score
            .total_cmp(&a.keyword_score)
            .then_with(|| identity_key(a.record).cmp(&identity_key(b.record)))
    });
    for (rank, hit) in hits.iter_mut().enumerate() {
        hit.keyword_rank = rank + 1;
    }
    hits.sort_by(|a, b| {
        b.cosine_score
            .total_cmp(&a.cosine_score)
            .then_with(|| identity_key(a.record).cmp(&identity_key(b.record)))
    });
    for (rank, hit) in hits.iter_mut().enumerate() {
        hit.vector_rank = rank + 1;
        hit.score = 1.0 / (60.0 + hit.keyword_rank as f64) + 1.0 / (60.0 + hit.vector_rank as f64);
    }
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| identity_key(a.record).cmp(&identity_key(b.record)))
    });
    hits.truncate(limit);
    Ok(hits)
}

fn signature_label(signature: &crate::ModelSignature) -> String {
    format!(
        "{}/{}/{}/{}/{}",
        signature.provider, signature.model, signature.revision, signature.dim, signature.metric
    )
}

fn identity_key(record: &MemoryRecord) -> (&str, &str, &str, &str, &str) {
    let id = &record.identity;
    (
        &id.project,
        &id.chapter,
        &id.block,
        &id.paragraph,
        &id.workspace_id,
    )
}

fn checked_vectors(
    vectors: Vec<Embedding>,
    count: usize,
    dim: usize,
) -> Result<Vec<Vec<f64>>, MemoryError> {
    if vectors.len() != count {
        return Err(MemoryError::Invalid("嵌入提供方返回的向量条数不符".into()));
    }
    vectors
        .into_iter()
        .map(|embedding| {
            if embedding.vector.len() != dim || embedding.vector.iter().any(|v| !v.is_finite()) {
                return Err(MemoryError::Invalid(
                    "嵌入向量维度不符或含非有限数值".into(),
                ));
            }
            let mut vector: Vec<f64> = embedding.vector.into_iter().map(f64::from).collect();
            let norm = vector.iter().map(|v| v * v).sum::<f64>().sqrt();
            if norm == 0.0 {
                return Err(MemoryError::Invalid("嵌入向量不能为零向量".into()));
            }
            for value in &mut vector {
                *value /= norm;
            }
            Ok(vector)
        })
        .collect()
}

fn cosine(a: &[f64], b: &[f64]) -> f64 {
    a.iter()
        .zip(b)
        .map(|(a, b)| a * b)
        .sum::<f64>()
        .clamp(-1.0, 1.0)
}

fn bigrams(text: &str) -> BTreeSet<String> {
    let chars: Vec<_> = text.chars().collect();
    if chars.len() == 1 {
        return BTreeSet::from([text.to_owned()]);
    }
    chars.windows(2).map(|pair| pair.iter().collect()).collect()
}

fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        0.0
    } else {
        a.intersection(b).count() as f64 / union as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Digests, EmbeddingSignature, FORMAT_VERSION, Identity, ModelSignature, Origin, Text,
        Unavailable, Watermark, sha256_hex,
    };
    use std::cell::RefCell;

    struct Provider {
        signature: ModelSignature,
        calls: RefCell<Vec<Vec<String>>>,
        bad_on_call: Option<(usize, Vec<Embedding>)>,
        fail_on_call: Option<usize>,
    }

    impl Provider {
        fn new() -> Self {
            Self {
                signature: ModelSignature {
                    provider: "test".into(),
                    model: "fixed".into(),
                    revision: "1".into(),
                    dim: 2,
                    metric: "cosine".into(),
                },
                calls: RefCell::new(Vec::new()),
                bad_on_call: None,
                fail_on_call: None,
            }
        }
    }

    impl EmbeddingProvider for Provider {
        fn signature(&self) -> &ModelSignature {
            &self.signature
        }
        fn embed(&self, texts: &[&str]) -> Result<Vec<Embedding>, MemoryError> {
            self.calls
                .borrow_mut()
                .push(texts.iter().map(|s| (*s).into()).collect());
            let call = self.calls.borrow().len();
            if self.fail_on_call == Some(call) {
                return Err(MemoryError::Unavailable);
            }
            if let Some((bad_call, vectors)) = &self.bad_on_call
                && *bad_call == call
            {
                return Ok(vectors.clone());
            }
            Ok(texts
                .iter()
                .map(|text| Embedding {
                    vector: if text.starts_with('远') {
                        vec![-1.0, 0.0]
                    } else {
                        vec![1.0, 0.0]
                    },
                })
                .collect())
        }
    }

    fn scope() -> SearchScope {
        SearchScope {
            workspace_id: "a".repeat(32),
            current_project: "p1".into(),
            authorized_projects: BTreeSet::from(["p1".into()]),
            source_language: "ja".into(),
            target_language: "zh".into(),
            content_type: "paragraph".into(),
        }
    }

    fn record(project: &str, paragraph: &str, source: &str) -> MemoryRecord {
        let signature = Provider::new().signature;
        MemoryRecord {
            identity: Identity {
                workspace_id: scope().workspace_id,
                project: project.into(),
                chapter: "c1".into(),
                block: "b1".into(),
                paragraph: paragraph.into(),
            },
            source_language: "ja".into(),
            target_language: "zh".into(),
            content_type: "paragraph".into(),
            source: Text::new(source),
            target: Text::new("译文"),
            digests: Digests {
                block_source_sha256: sha256_hex(b"source"),
                block_target_sha256: sha256_hex(b"target"),
            },
            origin: Origin {
                finalized_events: vec![0],
                translation_hash: sha256_hex(b"target"),
                context_version: 1,
                zhupi_version: 1,
            },
            embedding: EmbeddingSignature {
                provider: signature.provider,
                model: signature.model,
                revision: signature.revision,
                dim: signature.dim,
                metric: signature.metric,
                input_sha256: sha256_hex(source.as_bytes()),
            },
            watermark: Watermark {
                events: 1,
                history_sha256: sha256_hex(b"history"),
                history_bytes: 1,
                generation: 2,
            },
        }
    }

    fn index(records: Vec<MemoryRecord>) -> MemoryIndex {
        MemoryIndex {
            format_version: FORMAT_VERSION,
            signature: Provider::new().signature,
            watermark: record("p1", "a", "原文").watermark,
            records,
        }
    }

    #[test]
    fn scope_is_applied_before_embedding_ranking_and_limit() {
        let mut records = vec![record("p1", "b", "远方"), record("p2", "a", "查询")];
        let mut wrong_workspace = record("p1", "c", "查询");
        wrong_workspace.identity.workspace_id = "b".repeat(32);
        let mut wrong_language = record("p1", "d", "查询");
        wrong_language.target_language = "en".into();
        let mut wrong_source = record("p1", "e", "查询");
        wrong_source.source_language = "en".into();
        let mut wrong_kind = record("p1", "f", "查询");
        wrong_kind.content_type = "code".into();
        records.extend([wrong_workspace, wrong_language, wrong_source, wrong_kind]);
        let index = index(records);
        let provider = Provider::new();
        let hits = search(&index, &provider, "查询", &scope(), 1).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].record.identity.paragraph, "b");
        assert_eq!(hits[0].keyword_score, 0.0);
        assert_eq!(hits[0].cosine_score, -1.0); // 没有质量阈值。
        assert_eq!(*provider.calls.borrow(), vec![vec!["查询"], vec!["远方"]]);
        let mut cross_project = scope();
        cross_project.authorized_projects.insert("p2".into());
        let hits = search(&index, &Provider::new(), "查询", &cross_project, 1).unwrap();
        assert_eq!(hits[0].record.identity.project, "p2");
    }

    #[test]
    fn current_project_must_be_explicitly_authorized() {
        let index = index(vec![]);
        let provider = Provider::new();
        let mut scope = scope();
        scope.authorized_projects = BTreeSet::from(["p2".into()]);
        assert!(matches!(
            search(&index, &provider, "查询", &scope, 1),
            Err(MemoryError::Invalid(_))
        ));
        assert!(provider.calls.borrow().is_empty());
    }

    #[test]
    fn limits_are_validated_and_cap_only_after_ranking() {
        let records = (0..55)
            .map(|i| record("p1", &format!("p{i:02}"), "原文"))
            .collect();
        let index = index(records);
        for limit in [0, 51, usize::MAX] {
            assert!(matches!(
                search(&index, &Provider::new(), "查询", &scope(), limit),
                Err(MemoryError::Invalid(_))
            ));
        }
        for limit in [1, 50] {
            assert_eq!(
                search(&index, &Provider::new(), "查询", &scope(), limit)
                    .unwrap()
                    .len(),
                limit
            );
        }
    }

    #[test]
    fn ties_and_fused_scores_are_deterministic_under_input_permutation() {
        let mut index = index(vec![
            record("p1", "c", "相同"),
            record("p1", "a", "相同"),
            record("p1", "b", "相同"),
        ]);
        let snapshot = |index: &MemoryIndex| {
            search(index, &Provider::new(), "查询", &scope(), 50)
                .unwrap()
                .iter()
                .map(|hit| {
                    (
                        hit.record.identity.paragraph.clone(),
                        hit.keyword_rank,
                        hit.vector_rank,
                        hit.score,
                    )
                })
                .collect::<Vec<_>>()
        };
        let before = snapshot(&index);
        index.records.reverse();
        assert_eq!(before, snapshot(&index));
        assert_eq!(
            before.iter().map(|h| h.0.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert_eq!(before[0].3, 2.0 / 61.0);
    }

    #[test]
    fn character_bigrams_use_unicode_sets_and_jaccard() {
        assert_eq!(jaccard(&bigrams("春风又绿"), &bigrams("春风又来")), 0.5);
        assert_eq!(jaccard(&bigrams("哈哈哈"), &bigrams("哈哈")), 1.0);
        assert_eq!(jaccard(&bigrams("春"), &bigrams("春")), 1.0);
        assert_eq!(jaccard(&bigrams("春"), &bigrams("风")), 0.0);
    }

    #[test]
    fn rrf_uses_both_rankings_not_either_raw_score() {
        let index = index(vec![
            record("p1", "a", "远查询"),
            record("p1", "b", "查询之风"),
            record("p1", "c", "无关文本"),
        ]);
        let hits = search(&index, &Provider::new(), "查询", &scope(), 50).unwrap();
        assert_eq!(
            hits.iter()
                .map(|h| h.record.identity.paragraph.as_str())
                .collect::<Vec<_>>(),
            ["b", "a", "c"]
        );
        assert_eq!((hits[0].keyword_rank, hits[0].vector_rank), (2, 1));
        assert_eq!((hits[1].keyword_rank, hits[1].vector_rank), (1, 3));
        assert_eq!(hits[0].score, 1.0 / 62.0 + 1.0 / 61.0);
        assert_eq!(hits[1].keyword_score, 0.5);
    }

    #[test]
    fn real_local_provider_and_empty_query() {
        let provider = crate::LocalProvider::new();
        let signature = provider.signature();
        let mut record = record("p1", "a", "夜が明けた");
        record.embedding = EmbeddingSignature {
            provider: signature.provider.clone(),
            model: signature.model.clone(),
            revision: signature.revision.clone(),
            dim: signature.dim,
            metric: signature.metric.clone(),
            input_sha256: record.source.sha256.clone(),
        };
        let mut index = index(vec![record]);
        index.signature = signature.clone();
        let hits = search(&index, &provider, "夜が明けた", &scope(), 1).unwrap();
        assert_eq!(hits[0].keyword_score, 1.0);
        assert!((hits[0].cosine_score - 1.0).abs() < 1e-12);
        for query in ["", " \n\t"] {
            assert!(matches!(
                search(&index, &provider, query, &scope(), 1),
                Err(MemoryError::Invalid(_))
            ));
        }
    }

    #[test]
    fn query_and_candidate_bad_vectors_or_counts_are_rejected() {
        let index = index(vec![record("p1", "a", "原文")]);
        for call in [1, 2] {
            for vectors in [
                vec![],
                vec![Embedding { vector: vec![1.0] }],
                vec![Embedding {
                    vector: vec![0.0, 0.0],
                }],
                vec![Embedding {
                    vector: vec![f32::NAN, 1.0],
                }],
                vec![Embedding {
                    vector: vec![1.0, f32::INFINITY],
                }],
                vec![
                    Embedding {
                        vector: vec![1.0, 0.0]
                    };
                    2
                ],
            ] {
                let mut provider = Provider::new();
                provider.bad_on_call = Some((call, vectors));
                assert!(matches!(
                    search(&index, &provider, "查询", &scope(), 1),
                    Err(MemoryError::Invalid(_))
                ));
            }
        }
        // f32 的最大值也不应使范数溢出；余弦在 f64 中归一化。
        let vectors = checked_vectors(
            vec![Embedding {
                vector: vec![f32::MAX, f32::MAX],
            }],
            1,
            2,
        )
        .unwrap();
        assert!((cosine(&vectors[0], &vectors[0]) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn unavailable_and_provider_failures_are_never_empty_success() {
        for records in [vec![], vec![record("p1", "a", "原文")]] {
            let index = index(records);
            assert_eq!(
                search(&index, &Unavailable, "查询", &scope(), 1),
                Err(MemoryError::Unavailable)
            );
        }
        let index = index(vec![record("p1", "a", "原文")]);
        for call in [1, 2] {
            let mut provider = Provider::new();
            provider.fail_on_call = Some(call);
            assert_eq!(
                search(&index, &provider, "查询", &scope(), 1),
                Err(MemoryError::Unavailable)
            );
        }
        assert!(
            search(&self::index(vec![]), &Provider::new(), "查询", &scope(), 1)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn every_model_signature_component_and_record_signature_is_checked() {
        let base = index(vec![record("p1", "a", "原文")]);
        for field in ["provider", "model", "revision", "dim", "metric"] {
            let mut index = base.clone();
            match field {
                "provider" => index.signature.provider = "other".into(),
                "model" => index.signature.model = "other".into(),
                "revision" => index.signature.revision = "other".into(),
                "dim" => index.signature.dim = 3,
                _ => index.signature.metric = "dot".into(),
            }
            assert!(matches!(
                search(&index, &Provider::new(), "查询", &scope(), 1),
                Err(MemoryError::SignatureMismatch { .. })
            ));
        }
        let mut index = base;
        index.records[0].embedding.revision = "other".into();
        assert!(matches!(
            search(&index, &Provider::new(), "查询", &scope(), 1),
            Err(MemoryError::Corrupt(_))
        ));
        let mut provider = Provider::new();
        provider.signature.metric = "dot".into();
        assert!(matches!(
            search(&index, &provider, "查询", &scope(), 1),
            Err(MemoryError::Config(_))
        ));
    }
}
