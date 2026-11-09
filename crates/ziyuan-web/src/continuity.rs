use crate::state::{ApiError, AppState};
use axum::extract::Path as UrlPath;
use axum::{
    Json,
    body::Bytes,
    extract::{Query, State},
    http::header,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ziyuan_app::translation::{Translations, backup};

fn error(e: impl std::fmt::Display) -> ApiError {
    ApiError::bad(e.to_string())
}

#[derive(Deserialize)]
pub(crate) struct SearchQuery {
    query: String,
    scope: Option<String>,
}
#[derive(Deserialize)]
pub(crate) struct DocumentQuery {
    format: String,
}
pub(crate) async fn document_import(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
    Query(query): Query<DocumentQuery>,
    bytes: Bytes,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let result=block(move||Ok(Json(json!({"imported":Translations::new(root).import_document_bytes(&project,&query.format,&bytes).map_err(error)?})))).await?;
    app.emit("changed", json!({}));
    Ok(result)
}
pub(crate) async fn document_export(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
    Query(query): Query<DocumentQuery>,
) -> Result<Response, ApiError> {
    let root = app.0.config.root.clone();
    block(move || {
        let bytes = Translations::new(root)
            .export_document_bytes(&project, &query.format)
            .map_err(error)?;
        let name = format!("attachment; filename=finished.{}", query.format);
        Ok((
            [
                (header::CONTENT_TYPE, "application/octet-stream"),
                (header::CONTENT_DISPOSITION, name.as_str()),
            ],
            bytes,
        )
            .into_response())
    })
    .await
}
pub(crate) async fn search(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    block(move || {
        Ok(Json(json!(
            Translations::new(root)
                .search_text(
                    &project,
                    &query.query,
                    query.scope.as_deref().unwrap_or("all")
                )
                .map_err(error)?
        )))
    })
    .await
}
pub(crate) async fn draft_replace(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
    Json(input): Json<ziyuan_app::translation::DraftReplace>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let result = block(move || {
        Ok(Json(json!(
            Translations::new(root)
                .replace_draft_text(&project, &input)
                .map_err(error)?
        )))
    })
    .await?;
    app.emit("changed", json!({}));
    Ok(result)
}
#[derive(Deserialize)]
pub(crate) struct TmWrite {
    source: String,
    target: String,
}
#[derive(Deserialize)]
pub(crate) struct TmxInput {
    xml: String,
    fingerprint: Option<String>,
}
pub(crate) async fn tmx_preview(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
    Json(input): Json<TmxInput>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    block(move || {
        Ok(Json(json!(
            Translations::new(root)
                .preview_tmx(&project, input.xml.as_bytes())
                .map_err(error)?
        )))
    })
    .await
}
pub(crate) async fn tmx_import(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
    Json(input): Json<TmxInput>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let result=block(move||{let expected=input.fingerprint.ok_or_else(||error("须先预览TMX并确认"))?;Ok(Json(json!({"added":Translations::new(root).import_tmx(&project,input.xml.as_bytes(),&expected).map_err(error)?})))}).await?;
    app.emit("changed", json!({}));
    Ok(result)
}
pub(crate) async fn tmx_export(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
) -> Result<Response, ApiError> {
    let root = app.0.config.root.clone();
    block(move || {
        Ok((
            [
                (header::CONTENT_TYPE, "application/xml; charset=utf-8"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=translation-memory.tmx",
                ),
            ],
            Translations::new(root)
                .export_tmx(&project)
                .map_err(error)?,
        )
            .into_response())
    })
    .await
}
pub(crate) async fn tm_update(
    State(app): State<AppState>,
    UrlPath((project, id)): UrlPath<(String, String)>,
    Json(input): Json<TmWrite>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    block(move || {
        Ok(Json(json!(
            Translations::new(root)
                .tm_update(&project, &id, &input.source, &input.target)
                .map_err(error)?
        )))
    })
    .await
}
pub(crate) async fn tm_delete(
    State(app): State<AppState>,
    UrlPath((project, id)): UrlPath<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    block(move || {
        Translations::new(root)
            .tm_delete(&project, &id)
            .map_err(error)?;
        Ok(Json(json!({"deleted":true})))
    })
    .await
}
pub(crate) async fn tm_build(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    block(move || {
        Ok(Json(
            json!({"added":Translations::new(root).tm_build(&project).map_err(error)?}),
        ))
    })
    .await
}
pub(crate) async fn comment_reopen(
    State(app): State<AppState>,
    UrlPath((project, id)): UrlPath<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    block(move || {
        Ok(Json(json!(
            Translations::new(root)
                .comment_resolve(&project, &id, false)
                .map_err(error)?
        )))
    })
    .await
}
async fn block<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
}
#[derive(Deserialize)]
pub(crate) struct BackupRequest {
    fingerprint: Option<String>,
}
fn fingerprint(bytes: &[u8]) -> String {
    ziyuan_storage::content_sha256(bytes)
}
pub(crate) async fn backup(
    State(app): State<AppState>,
    Json(input): Json<BackupRequest>,
) -> Result<Response, ApiError> {
    let root = app.0.config.root.clone();
    block(move || {
        let bundle = Translations::new(root).backup_workspace().map_err(error)?;
        let digest = fingerprint(&bundle.bytes);
        if let Some(expected) = input.fingerprint {
            if expected != digest { return Err(error("备份材料已变化，请重新预览")); }
            Ok(([(header::CONTENT_TYPE, "application/x-tar"), (header::CONTENT_DISPOSITION, "attachment; filename=ziyuan-workspace.tar")], bundle.bytes).into_response())
        } else {
            Ok(Json(json!({"manifest":bundle.manifest,"fingerprint":digest,"scope":"全部文本项目与共用朝廷历史（可能含代码任务）；不含登录令牌和模型凭据"})).into_response())
        }
    }).await
}
#[derive(Deserialize, Serialize)]
pub(crate) struct RestoreQuery {
    destination: Option<String>,
    fingerprint: Option<String>,
}
pub(crate) async fn restore(
    State(app): State<AppState>,
    Query(query): Query<RestoreQuery>,
    bytes: Bytes,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    block(move || {
        let digest = fingerprint(&bytes);
        let manifest = backup::inspect_backup(&bytes).map_err(error)?;
        if let Some(destination) = query.destination {
            if destination.is_empty()
                || !destination
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            {
                return Err(error("恢复目录名只能含英文字母、数字、横线和下划线"));
            }
            if query.fingerprint.as_deref() != Some(&digest) {
                return Err(error("须先校验备份并确认"));
            }
            backup::restore_backup(&bytes, &root.join(&destination)).map_err(error)?;
            Ok(Json(json!({"restored":destination,"manifest":manifest})))
        } else {
            Ok(Json(json!({"manifest":manifest,"fingerprint":digest})))
        }
    })
    .await
}
#[derive(Deserialize)]
pub(crate) struct UpdateRequest {
    #[serde(flatten)]
    input: ziyuan_app::translation::SourceUpdate,
    fingerprint: Option<String>,
}
pub(crate) async fn update(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
    Json(request): Json<UpdateRequest>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let value = block(move || {
        let t = Translations::new(root);
        if let Some(expected) = request.fingerprint {
            Ok(json!(
                t.apply_source_update(&project, &request.input, &expected)
                    .map_err(error)?
            ))
        } else {
            Ok(json!(
                t.preview_source_update(&project, &request.input)
                    .map_err(error)?
            ))
        }
    })
    .await?;
    app.emit("changed", json!({}));
    Ok(Json(value))
}
#[derive(Deserialize)]
pub(crate) struct AddIssue {
    input: ziyuan_app::translation::IssueInput,
    fingerprint: String,
}
pub(crate) async fn issues(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    block(move || {
        Ok(Json(json!(
            Translations::new(root).issues(&project).map_err(error)?
        )))
    })
    .await
}
pub(crate) async fn add_issue(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
    Json(input): Json<AddIssue>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let result = block(move || {
        Ok(Json(json!(
            Translations::new(root)
                .add_issue(&project, &input.input, &input.fingerprint)
                .map_err(error)?
        )))
    })
    .await?;
    app.emit("changed", json!({}));
    Ok(result)
}
pub(crate) async fn review_issue(
    State(app): State<AppState>,
    UrlPath((project, id)): UrlPath<(String, u64)>,
    Json(input): Json<ziyuan_app::translation::IssueReview>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let result = block(move || {
        Ok(Json(json!(
            Translations::new(root)
                .review_issue(&project, id, &input)
                .map_err(error)?
        )))
    })
    .await?;
    app.emit("changed", json!({}));
    Ok(result)
}
#[derive(Deserialize, Default)]
pub(crate) struct ReadingQuery {
    chapter: Option<String>,
    #[serde(default)]
    format: ziyuan_app::translation::ExportFormat,
}
pub(crate) async fn reading(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
    Query(query): Query<ReadingQuery>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    block(move || {
        Ok(Json(json!(
            Translations::new(root)
                .read_chapter(&project, query.chapter.as_deref())
                .map_err(error)?
        )))
    })
    .await
}
pub(crate) async fn delivery(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
    Query(query): Query<ReadingQuery>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    block(move || {
        let options = ziyuan_app::translation::ExportOptions {
            chapters: query.chapter.into_iter().collect(),
            format: query.format,
            ..Default::default()
        };
        Ok(Json(json!(
            Translations::new(root)
                .delivery_preflight(&project, &options)
                .map_err(error)?
        )))
    })
    .await
}
