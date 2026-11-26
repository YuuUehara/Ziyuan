//! 原件与有效定稿 TAR 导出、TXT／Markdown 预览及确认导入。

use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use ziyuan_app::AppError;
use ziyuan_app::text_project::{ImportOptions, ImportSource};
use ziyuan_app::translation::{
    ExportFormat, ExportLayout, ExportOptions, TranslationError, Translations,
};

use super::{blocking, material};
use crate::state::{ApiError, AppState};

#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum ExportKind {
    #[default]
    Finished,
    Original,
}

#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExportQuery {
    #[serde(default)]
    kind: ExportKind,
    #[serde(default)]
    format: ExportFormat,
    #[serde(default)]
    layout: ExportLayout,
    #[serde(default)]
    bilingual: bool,
    #[serde(default)]
    chapters: String,
    #[serde(default)]
    preview: bool,
    snapshot: Option<String>,
}

/// Read-only: eligibility and fresh snapshots are owned by the application layer.
pub(crate) async fn export(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Query(query): Query<ExportQuery>,
) -> Result<Response, ApiError> {
    let chapters = if query.chapters.is_empty() {
        Vec::new()
    } else {
        let chapters: Vec<_> = query.chapters.split(',').map(str::to_owned).collect();
        if chapters
            .iter()
            .any(|c| c.trim().is_empty() || c.trim() != c)
        {
            return Err(ApiError::bad("章节 ID 不得为空或带首尾空白。"));
        }
        chapters
    };
    if matches!(&query.kind, ExportKind::Original)
        && (query.format != ExportFormat::Txt
            || query.layout != ExportLayout::Chapters
            || query.bilingual)
    {
        return Err(ApiError::bad(
            "原件按归档字节导出，不接受成稿格式、双语或排版选项",
        ));
    }
    let options = ExportOptions {
        chapters,
        format: query.format,
        layout: query.layout,
        bilingual: query.bilingual,
    };
    // Never reflect project titles, chapter titles or filesystem errors into headers/errors.
    let safe_id: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(80)
        .collect();
    let root = app.0.config.root.clone();
    let bundle = match query.kind {
        ExportKind::Original => {
            blocking(move || {
                Translations::new(&root)
                    .export_originals(&id, &options.chapters)
                    .map_err(|_| {
                        ApiError::bad("原件导出失败：请检查项目与章节选择、档案是否完整。")
                    })
            })
            .await?
        }
        ExportKind::Finished => app
            .session_result(move |s| s.export_finished(&root, &id, &options))
            .await?
            .map_err(|e| {
                if let AppError::Governance(ziyuan_governance::GovernanceError::Translation(
                    message,
                )) = &e
                    && (message.starts_with("不能导出成稿")
                        || message.starts_with("双语对照导出仅支持")
                        || message.contains("：原段含行内代码，"))
                {
                    return ApiError::bad(message.clone());
                }
                ApiError(
                    ApiError::from(e).0,
                    "定稿导出失败：所选章节须全部有效定稿；请检查章节选择与材料。".into(),
                )
            })?,
    };
    if let Some(snapshot) = query.snapshot
        && bundle.manifest["snapshot_sha256"].as_str() != Some(snapshot.as_str())
    {
        return Err(ApiError::bad(
            "预览后材料或选择有变，请重新预览，再确认下载。",
        ));
    }
    if query.preview {
        return Ok(([(header::CACHE_CONTROL, "no-store")], Json(json!({
            "manifest": bundle.manifest,
            "warnings": bundle.warnings,
            "files": bundle.files.iter().map(|f| json!({"name": f.name, "length": f.bytes.len()})).collect::<Vec<_>>(),
        }))).into_response());
    }
    let bytes = blocking(move || {
        bundle
            .archive()
            .map_err(|_| ApiError::bad("导出归档失败，请检查材料完整性。"))
    })
    .await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-tar".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"ziyuan-{safe_id}.tar\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        bytes,
    )
        .into_response())
}

// Deliberately not ImportSource: uploads cannot select server filesystem paths.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UploadFile {
    name: String,
    text: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportRequest {
    files: Vec<UploadFile>,
    #[serde(default)]
    options: ImportOptions,
    fingerprint: Option<String>,
}

impl ImportRequest {
    fn sources(self) -> (Vec<ImportSource>, ImportOptions, Option<String>) {
        let sources = self
            .files
            .into_iter()
            .map(|f| ImportSource::Text {
                name: f.name,
                text: f.text,
            })
            .collect();
        (sources, self.options, self.fingerprint)
    }
}

pub(crate) async fn import_preview(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<ImportRequest>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let (sources, options, _) = body.sources();
    let preview = blocking(move || {
        let t = Translations::new(&root);
        t.texts()
            .preview_import(&id, &sources, &options)
            .map_err(|e| material(TranslationError::from(e)))
    })
    .await?;
    Ok(Json(json!(preview)))
}

pub(crate) async fn import_confirm(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<ImportRequest>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let (sources, options, fingerprint) = body.sources();
    let fingerprint = fingerprint
        .filter(|f| !f.is_empty())
        .ok_or_else(|| ApiError::bad("请先预览，再确认完全相同的导入内容。"))?;
    let project = blocking(move || {
        let t = Translations::new(&root);
        t.texts()
            .import_prepared(&id, &sources, &options, &fingerprint)
            .map_err(|e| material(TranslationError::from(e)))
    })
    .await?;
    app.emit("changed", json!({}));
    Ok(Json(json!(project)))
}
