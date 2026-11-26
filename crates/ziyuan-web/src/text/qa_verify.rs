//! 六事逐段勘校、词证登记、确定性 QA（单块／整书／误报白名单）与段旁实时译文记忆推荐。

use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use ziyuan_app::AppError;
use ziyuan_app::translation::{
    EvidenceInput, TmSuggestion, Translations, VerificationInput, merge_tm_suggestions,
    score_tm_similarity,
};

use super::memory_refs::memory_provider;
use super::{blocking, material};
use crate::state::{ApiError, AppState};

fn verification_error(error: impl std::fmt::Display) -> ApiError {
    if error.to_string().contains("人物／术语资料尚未开放") {
        ApiError::bad("人物／术语资料尚未开放")
    } else {
        ApiError::bad("词证或复核不可用：请检查段落、关联依据与材料；材料变化后请重新读取再复核。")
    }
}

pub(crate) async fn evidence_add(
    State(app): State<AppState>,
    UrlPath((project, block)): UrlPath<(String, String)>,
    Json(body): Json<EvidenceInput>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let value = blocking(move || {
        Translations::new(&root)
            .add_evidence(&project, &block, &body)
            .map(|v| json!(v))
            .map_err(verification_error)
    })
    .await?;
    app.emit("changed", json!({}));
    Ok(Json(value))
}

pub(crate) async fn verification(
    State(app): State<AppState>,
    UrlPath((project, block)): UrlPath<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let result = app
        .session_result({
            let (root, project, block) = (root.clone(), project.clone(), block.clone());
            move |s| Ok(Translations::new(&root).verification(Some(s.court()), &project, &block)?)
        })
        .await?;
    let view = match result {
        Ok(view) => view,
        Err(AppError::NotFounded) => {
            blocking(move || {
                Translations::new(&root)
                    .verification(None, &project, &block)
                    .map_err(verification_error)
            })
            .await?
        }
        Err(e) => return Err(verification_error(e)),
    };
    Ok(Json(json!(view)))
}

#[derive(serde::Deserialize)]
pub(crate) struct VerificationAssociationsQuery {
    #[serde(default)]
    associations: Option<String>,
}

pub(crate) async fn verification_associations(
    State(app): State<AppState>,
    UrlPath((project, block)): UrlPath<(String, String)>,
    Query(query): Query<VerificationAssociationsQuery>,
) -> Result<Json<Value>, ApiError> {
    let associations: Vec<ziyuan_app::translation::visibility::ParagraphAssociations> =
        match query.associations {
            Some(text) => serde_json::from_str(&text)
                .map_err(|_| ApiError::bad("复核关联不是有效 JSON 数组"))?,
            None => Vec::new(),
        };
    let root = app.0.config.root.clone();
    let result = app
        .session_result({
            let (root, project, block, associations) = (
                root.clone(),
                project.clone(),
                block.clone(),
                associations.clone(),
            );
            move |s| {
                Ok(Translations::new(&root).verification_with_associations(
                    Some(s.court()),
                    &project,
                    &block,
                    &associations,
                )?)
            }
        })
        .await?;
    let view = match result {
        Ok(view) => view,
        Err(AppError::NotFounded) => {
            blocking(move || {
                Translations::new(&root)
                    .verification_with_associations(None, &project, &block, &associations)
                    .map_err(verification_error)
            })
            .await?
        }
        Err(e) => return Err(verification_error(e)),
    };
    Ok(Json(json!(view)))
}

pub(crate) async fn qa(
    State(app): State<AppState>,
    UrlPath((project, block)): UrlPath<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let value = blocking(move || {
        Translations::new(&root)
            .qa(&project, &block)
            .map(|v| json!(v))
            .map_err(verification_error)
    })
    .await?;
    Ok(Json(value))
}

/// 整书级批量确定性 QA 汇总（纯只读，观政可读，不调用模型、不落盘）。
pub(crate) async fn qa_report(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let statuses = app
        .session_result({
            let root = root.clone();
            let project = project.clone();
            move |s| s.translation_status(&root, &project)
        })
        .await?;
    let statuses = match statuses {
        Ok(statuses) => Some(statuses),
        Err(AppError::NotFounded) => None,
        Err(e) => return Err(ApiError::from(e)),
    };
    let value = blocking(move || {
        Translations::new(&root)
            .project_qa_report(&project, statuses.as_deref())
            .map(|v| json!(v))
            .map_err(verification_error)
    })
    .await?;
    Ok(Json(value))
}

/// 列出本项目已忽略的 QA 误报（纯只读，观政可读）。
pub(crate) async fn qa_ignores(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let project_resp = project.clone();
    let ignores = blocking(move || {
        Translations::new(&root)
            .qa_ignores(&project)
            .map_err(material)
    })
    .await?;
    Ok(Json(json!({ "project": project_resp, "ignores": ignores })))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QaIgnoreBody {
    block: String,
    #[serde(default)]
    paragraph: String,
    code: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    remove: bool,
}

/// 新增或移除单条 QA 误报忽略记录（仅改本项目 qa-ignores.json，不落事件簿）。
pub(crate) async fn qa_ignore_update(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
    Json(body): Json<QaIgnoreBody>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let project_resp = project.clone();
    let removed = body.remove;
    let ignores = blocking(move || {
        let t = Translations::new(&root);
        if body.remove {
            t.remove_qa_ignore(&project, &body.block, &body.paragraph, &body.code)
                .map_err(material)
        } else {
            t.add_qa_ignore(
                &project,
                &body.block,
                &body.paragraph,
                &body.code,
                &body.note,
            )
            .map_err(material)
        }
    })
    .await?;
    app.emit("changed", json!({}));
    Ok(Json(json!({
        "project": project_resp,
        "removed": removed,
        "ignores": ignores,
    })))
}

/// 段旁实时译文记忆推荐（纯只读，观政可读，仅当前项目，不自动写入或覆盖译稿）。
pub(crate) async fn tm_suggest(
    State(app): State<AppState>,
    UrlPath((project, block)): UrlPath<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    if query.keys().any(|k| k != "paragraph" && k != "limit") {
        return Err(ApiError::bad(
            "仅支持 paragraph 与 limit 参数，不能扩大检索范围。",
        ));
    }
    let paragraph = query
        .get("paragraph")
        .map_or("", String::as_str)
        .trim()
        .to_owned();
    if paragraph.is_empty() {
        return Err(ApiError::bad("请指定段号（paragraph）。"));
    }
    let limit = match query.get("limit") {
        None => 5usize,
        Some(raw) => match raw.trim().parse::<usize>() {
            Ok(n) if (1..=10).contains(&n) => n,
            _ => return Err(ApiError::bad("limit 须为 1 至 10 的整数。")),
        },
    };
    let root = app.0.config.root.clone();
    let mut out = blocking({
        let root = root.clone();
        let project = project.clone();
        let block = block.clone();
        let paragraph = paragraph.clone();
        move || {
            Translations::new(&root)
                .suggest_tm_for_paragraph(&project, &block, &paragraph, limit)
                .map_err(material)
        }
    })
    .await?;

    if !out.source.trim().is_empty() {
        let source = out.source.clone();
        let memory_hits = app
            .session_result({
                let root = root.clone();
                let project = project.clone();
                let block = block.clone();
                let paragraph = paragraph.clone();
                move |s| {
                    let Ok(provider) = memory_provider(&root, &project) else {
                        return Ok(Vec::new());
                    };
                    let Ok(hits) = s.memory_search(
                        &root,
                        &BTreeSet::new(),
                        &project,
                        &source,
                        limit,
                        provider.as_ref(),
                    ) else {
                        return Ok(Vec::new());
                    };
                    let mut mapped = Vec::new();
                    for hit in hits {
                        if hit.record.identity.block == block
                            && hit.record.identity.paragraph == paragraph
                        {
                            continue;
                        }
                        let text_score = score_tm_similarity(&source, &hit.record.source.text);
                        let cos_score = (hit.cosine_score.clamp(0.0, 1.0) * 99.0).round() as u32;
                        let score = text_score.max(cos_score).min(100);
                        if score >= 25 {
                            mapped.push(TmSuggestion {
                                source: hit.record.source.text,
                                target: hit.record.target.text,
                                score,
                                origin: "memory".into(),
                                origin_block: Some(hit.record.identity.block),
                                origin_paragraph: Some(hit.record.identity.paragraph),
                                tm_id: None,
                            });
                        }
                    }
                    Ok(mapped)
                }
            })
            .await;
        if let Ok(Ok(extra)) = memory_hits
            && !extra.is_empty()
        {
            out.suggestions = merge_tm_suggestions(out.suggestions, extra, limit);
        }
    }

    Ok(Json(json!(out)))
}

pub(crate) async fn verification_record(
    State(app): State<AppState>,
    UrlPath((project, block)): UrlPath<(String, String)>,
    Json(body): Json<VerificationInput>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let result = app
        .session_result({
            let (root, project, block, body) =
                (root.clone(), project.clone(), block.clone(), body.clone());
            move |s| {
                Ok(Translations::new(&root).record_verification(
                    Some(s.court()),
                    &project,
                    &block,
                    &body,
                )?)
            }
        })
        .await?;
    let record = match result {
        Ok(record) => record,
        Err(AppError::NotFounded) => {
            blocking(move || {
                Translations::new(&root)
                    .record_verification(None, &project, &block, &body)
                    .map_err(verification_error)
            })
            .await?
        }
        Err(e) => return Err(verification_error(e)),
    };
    app.emit("changed", json!({}));
    Ok(Json(json!(record)))
}

/// 单项目四阶段模型用量与多币种估算账本（只读，观政可用；未建朝时返回空小计与说明）。
pub(crate) async fn usage(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let backend = app.0.config.backend.clone();
    let check_root = root.clone();
    let check_project = project.clone();
    blocking(move || {
        Translations::new(&check_root)
            .project(&check_project)
            .map(|_| ())
            .map_err(material)
    })
    .await?;

    let result = app
        .session_result({
            let project = project.clone();
            move |s| {
                s.translation_usage(&project, |m| {
                    backend.price(m).map(|p| (p, "当前服务模型配置与参考单价"))
                })
            }
        })
        .await?;

    let (founded, report) = match result {
        Ok(report) => (true, report),
        Err(AppError::NotFounded) => (
            false,
            ziyuan_app::translation::TranslationUsage {
                project,
                stages: ziyuan_core::TranslationKind::ALL
                    .into_iter()
                    .map(|kind| ziyuan_app::translation::StageUsage {
                        kind,
                        totals: ziyuan_app::translation::UsageTotals::default(),
                    })
                    .collect(),
                tasks: Vec::new(),
                totals: ziyuan_app::translation::UsageTotals::default(),
                notes: vec![
                    "当前工作区尚未建朝（ziyuan init），暂无关联的模型诏令事件簿；手工译稿不产生模型 token 用量。"
                        .into(),
                ],
            },
        ),
        Err(e) => return Err(ApiError::from(e)),
    };

    let mut value = serde_json::to_value(report)
        .map_err(|e| ApiError(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("founded".into(), json!(founded));
    }
    Ok(Json(value))
}
