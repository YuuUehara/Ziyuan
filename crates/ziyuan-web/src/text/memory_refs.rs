//! 本地／远程译文记忆（状态、建库、检索）与显式选用参考（预览、下诏、快照查看）。

use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use ziyuan_app::AppError;
use ziyuan_app::translation::{ReferenceRequest, ReferenceSubmission, Translations};
use ziyuan_core::TranslationKind;

use crate::state::{ApiError, AppState};

/// 当前项目的记忆状态。
/// 只返回当前项目的计数，不把工作区索引或磁盘错误暴露给页面。
pub(crate) async fn memory_status(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let result = app
        .session_result(move |s| {
            Translations::new(&root).project(&id)?;
            let available = ziyuan_app::memory_provider::selection_from_env(None)
                .and_then(|selection| {
                    ziyuan_app::memory_provider::open(
                        &root,
                        &BTreeSet::from([id.clone()]),
                        selection,
                    )
                })
                .is_ok_and(|provider| provider.signature().dim > 0);
            let status = match s.memory_status(&root) {
                Ok(status) => status,
                // 缓存坏了也保留重建入口；不把文件路径或底层解析内容传给浏览器。
                Err(_) => {
                    return Ok(json!({ "project": id, "available": available,
                    "indexed": false, "records": 0, "stale": true,
                    "problem": "记忆缓存无法读取，请重建；若仍失败，请检查本地材料与配置。" }));
                }
            };
            let records = status.index.as_ref().map_or(0, |index| {
                index
                    .records
                    .iter()
                    .filter(|r| r.identity.project == id)
                    .count()
            });
            Ok(json!({ "project": id, "available": available,
            "indexed": status.index.is_some(), "records": records, "stale": status.stale }))
        })
        .await?;
    Ok(Json(result.map_err(memory_error)?))
}

fn memory_error(error: AppError) -> ApiError {
    match error {
        AppError::NotFounded => ApiError::bad("译文记忆须先建朝（ziyuan init）。"),
        AppError::External(message)
            if message.contains("授权")
                || message.contains("不会")
                || message.contains("未联网") =>
        {
            ApiError::bad(message)
        }
        _ => ApiError::bad(
            "译文记忆暂不可用：请检查项目、已定稿材料与本地配置；索引缺失或过期时请重建。",
        ),
    }
}

pub(super) fn memory_provider(
    root: &Path,
    project: &str,
) -> Result<Box<dyn ziyuan_memory::EmbeddingProvider>, AppError> {
    let selection = ziyuan_app::memory_provider::selection_from_env(None)?;
    let provider =
        ziyuan_app::memory_provider::open(root, &BTreeSet::from([project.to_owned()]), selection)?;
    if provider.signature().dim == 0 {
        return Err(AppError::External("未启用嵌入提供方；默认不外发。".into()));
    }
    Ok(provider)
}

pub(crate) async fn memory_build(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let result = app
        .session_result(move |s| {
            let provider = memory_provider(&root, &id)?;
            let build = s.memory_build(&root, &BTreeSet::from([id]), provider.as_ref())?;
            Ok(json!({
                "records": build.records,
                "reused": build.reused,
                "embedded": build.embedded,
                "skipped": build.skipped.len(),
            }))
        })
        .await?;
    Ok(Json(result.map_err(memory_error)?))
}

pub(crate) async fn memory_search(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    // 页面没有跨项目授权入口；拒绝额外参数，不能用 query string 扩大范围。
    if query.keys().any(|k| k != "q") {
        return Err(ApiError::bad("这里只能检索当前项目。"));
    }
    let q = query.get("q").map_or("", String::as_str).trim().to_owned();
    if q.is_empty() || q.chars().count() > 2000 {
        return Err(ApiError::bad("请输入 1 至 2000 字的检索文字。"));
    }
    let root = app.0.config.root.clone();
    let result = app
        .session_result(move |s| {
            let provider = memory_provider(&root, &id)?;
            let hits = s.memory_search(&root, &BTreeSet::new(), &id, &q, 8, provider.as_ref())?;
            Ok(
                json!({ "project": id, "hits": hits.into_iter().map(|hit| json!({
            "project": hit.record.identity.project, "block": hit.record.identity.block,
            "paragraph": hit.record.identity.paragraph, "source": hit.record.source.text,
            "translation": hit.record.target.text,
            "selection": {"identity": hit.record.identity, "digests": hit.record.digests, "origin": hit.record.origin},
        })).collect::<Vec<_>>() }),
            )
        })
        .await?;
    Ok(Json(result.map_err(memory_error)?))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReferencePreviewBody {
    kind: TranslationKind,
    request: ReferenceRequest,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReferenceIssueBody {
    kind: TranslationKind,
    submission: ReferenceSubmission,
}

fn current_references(
    project: &str,
    kind: TranslationKind,
    request: &ReferenceRequest,
) -> Result<(), ApiError> {
    if !matches!(kind, TranslationKind::Translate | TranslationKind::Revise) {
        return Err(ApiError::bad("参考只可用于初译或修订。"));
    }
    if !request.authorized_projects.is_empty()
        || request
            .selections
            .iter()
            .any(|s| s.identity.project != project)
    {
        return Err(ApiError::bad(
            "Web 参考只可选择当前项目，不接受跨项目授权。",
        ));
    }
    Ok(())
}

fn reference_error(error: AppError) -> ApiError {
    match error {
        AppError::External(_) => {
            ApiError::bad("参考选择或确认已失效，请检查材料并重新预览；提交 ID 不可改绑其他内容。")
        }
        other => ApiError::from(other),
    }
}

pub(crate) async fn references_preview(
    State(app): State<AppState>,
    UrlPath((project, block)): UrlPath<(String, String)>,
    Json(body): Json<ReferencePreviewBody>,
) -> Result<Json<Value>, ApiError> {
    current_references(&project, body.kind, &body.request)?;
    let root = app.0.config.root.clone();
    let preview = app
        .session_result(move |s| {
            s.preview_references(&root, &project, &block, body.kind, &body.request)
        })
        .await?
        .map_err(reference_error)?;
    Ok(Json(json!(preview)))
}

pub(crate) async fn references_issue(
    State(app): State<AppState>,
    UrlPath((project, block)): UrlPath<(String, String)>,
    Json(body): Json<ReferenceIssueBody>,
) -> Result<Json<Value>, ApiError> {
    current_references(&project, body.kind, &body.submission.request)?;
    let root = app.0.config.root.clone();
    let task = app
        .session_result(move |s| {
            s.issue_with_references(&root, &project, &block, body.kind, &body.submission)
        })
        .await?
        .map_err(reference_error)?;
    app.emit("changed", json!({}));
    Ok(Json(json!({"tasks": [task.id.0]})))
}

pub(crate) async fn references_task(
    State(app): State<AppState>,
    UrlPath(task): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let view = app
        .session_result(move |s| {
            let task = ziyuan_core::TaskId(task);
            if s.court().task(task).is_none_or(|t| t.references.is_none()) {
                return Ok(None);
            }
            s.reference_task(&root, task).map(Some)
        })
        .await?
        .map_err(reference_error)?
        .ok_or_else(|| {
            ApiError(
                axum::http::StatusCode::NOT_FOUND,
                "该任务没有参考快照。".into(),
            )
        })?;
    Ok(Json(json!(view)))
}
