//! 翰林院 · 译书与御笔札记（武英殿卡片）的 API：项目与块材料、上下文、朱批、御览、札记
//! 与人工译稿（草稿、送审、准奏、定稿）。
//!
//! 材料读写（项目、上下文草稿、朱批、札记、人工草稿）不经朝廷；块进度、待准修订与人工
//! 准奏／定稿事实须朝廷回放，未建朝时 `founded` 为假、`statuses` / `status` / `pending`
//! 为空，而不是报错。材料只经应用层 API 读写；导出目录里的 `source.json`、`context.json`
//! 等只读。人工入口沿用 CLI 的同一套应用层接口：草稿只写草稿文件，送审只发布绑定，
//! 准奏才落盘 `zh.json`，定稿只记定稿事实。

use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use ziyuan_app::notes::{NewNote, NoteEdit, NoteFilter, NoteKind, Notes};
use ziyuan_app::text_project::{ImportOptions, ImportSource, NewProject, Project, TextProjects};
use ziyuan_app::translation::{
    BlockState, BlockStatus, DraftSave, DraftView, EvidenceInput, ExportFormat, ExportLayout,
    ExportOptions, ReferenceRequest, ReferenceSubmission, TranslationError, Translations,
    VerificationInput, human_submit_request_id,
};
use ziyuan_app::{AppError, selected_files};
use ziyuan_core::{CourtEvent, TranslationKind};
use ziyuan_governance::{ChangeState, Court};
use ziyuan_storage::{EventLog, EventStore, LockScope, Workspace};

use crate::state::{ApiError, AppState, parse, str_field};

/// 译书材料与项目错误一律 400，报错文字与 CLI 相同。
fn material(error: TranslationError) -> ApiError {
    ApiError::from(AppError::from(error))
}

fn missing_block(project: &str, block: &str) -> ApiError {
    material(TranslationError::Invalid(format!(
        "项目 {project} 没有块 {block}"
    )))
}

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
    axum::extract::Query(query): axum::extract::Query<VerificationAssociationsQuery>,
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
        && (query.format != ExportFormat::Txt || query.layout != ExportLayout::Chapters)
    {
        return Err(ApiError::bad(
            "原件按归档字节导出，不接受成稿格式或排版选项",
        ));
    }
    let options = ExportOptions {
        chapters,
        format: query.format,
        layout: query.layout,
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

/// 在阻塞线程里读写文件（材料与札记都不经事件簿）。
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
}

/// 请求体里的可选文字：空串按未给处理。
fn opt_field(body: &Value, key: &str) -> Option<String> {
    body.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
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

/// 项目摘要（列表与详情共用）：章、块为件数，另附现行上下文版本。
fn project_json(t: &Translations, p: &Project) -> Result<Value, ApiError> {
    Ok(json!({
        "id": p.id,
        "title": p.metadata.title,
        "source_language": p.metadata.source_language,
        "target_language": p.metadata.target_language,
        "provenance": p.metadata.provenance,
        "chapters": p.chapters.len(),
        "blocks": p.chapters.iter().map(|c| c.blocks.len()).sum::<usize>(),
        "context_version": t.context_version(&p.id).map_err(material)?,
    }))
}

/// 块状态：应用层的 `BlockStatus`，另加一个中文标签供页面直接显示。
fn status_json(status: &BlockStatus) -> Value {
    let label = status.state.label();
    let mut value = serde_json::to_value(status).unwrap_or(Value::Null);
    value["label"] = json!(label);
    value
}

/// 待准奏的译稿改动：最终会落盘的 `zh.json`（逐段取舍之后）与所属诏令。
fn pending_json(court: &Court, project: &str, block: &str) -> Option<Value> {
    let task = court
        .translation_tasks(project, block)
        .filter(|t| t.translation.as_ref().is_some_and(|l| l.kind.writes()))
        .filter(|t| {
            t.changes
                .as_ref()
                .is_some_and(|c| c.state == ChangeState::Proposed)
        })
        .last()?;
    let link = task.translation.as_ref()?;
    let changes = task.changes.as_ref()?;
    let file = selected_files(&changes.files, &changes.skipped)
        .into_iter()
        .find(|f| f.path == link.output_path)?;
    let (output, error) = match serde_json::from_str::<Value>(&file.after) {
        Ok(value) => (Some(value), None),
        Err(e) => (None, Some(e.to_string())),
    };
    Some(json!({
        "task": task.edict.id.0,
        "kind": link.kind.code(),
        "label": link.kind.label(),
        "output": output,
        "parse_error": error,
        "text": file.after,
    }))
}

/// 材料目录下的一份 JSON：`(值, 解析错误, 原文)`；文件不存在时全为空（不合法 UTF-8 也照给原文）。
fn material_file(dir: &Path, name: &str) -> (Option<Value>, Option<String>, String) {
    let Ok(bytes) = std::fs::read(dir.join(name)) else {
        return (None, None, String::new());
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => {
            // context.json 可能被手工改写，不能把第二版对象当旧数组交给页面。
            if name == "context.json"
                && let Some(glossary) = value.get("glossary")
            {
                let result = serde_json::to_vec(glossary)
                    .map_err(|e| e.to_string())
                    .and_then(|bytes| {
                        ziyuan_app::translation::parse_glossary(&bytes).map_err(|e| e.to_string())
                    });
                if let Err(error) = result {
                    return (None, Some(error), text);
                }
            }
            (Some(value), None, text)
        }
        Err(e) => (None, Some(e.to_string()), text),
    }
}

/// 新建空文本项目：不导入原文，导入仍在既有预览／确认流程里完成。
/// 来源与权利沿用 CLI 的要求（用户自述，不核验），避免建出无出处的项目。
pub(crate) async fn create_project(
    State(app): State<AppState>,
    Json(mut body): Json<NewProject>,
) -> Result<Json<Value>, ApiError> {
    body.title = body.title.trim().to_owned();
    body.provenance.source = body.provenance.source.trim().to_owned();
    body.provenance.rights = body.provenance.rights.trim().to_owned();
    body.target_language = "zh-CN".to_owned();
    if body.title.is_empty() {
        return Err(ApiError::bad("项目标题不能为空。"));
    }
    if !matches!(body.source_language.as_str(), "ja" | "en") {
        return Err(ApiError::bad("原文语言只支持 ja 或 en。"));
    }
    if body.provenance.source.is_empty() || body.provenance.rights.is_empty() {
        return Err(ApiError::bad(
            "来源与权利声明不能为空；二者只是用户自述，不作核验。",
        ));
    }
    let root = app.0.config.root.clone();
    let view = blocking(move || {
        let projects = TextProjects::new(&root);
        let project = projects
            .create(body)
            .map_err(|e| material(TranslationError::from(e)))?;
        let t = Translations::new(&root);
        project_json(&t, &project)
    })
    .await?;
    app.emit("changed", json!({}));
    Ok(Json(json!({ "project": view })))
}

/// 全部文本项目；没有项目（或尚未建项目）时为空数组。
pub(crate) async fn list(State(app): State<AppState>) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let projects = blocking(move || {
        let t = Translations::new(&root);
        let mut projects = Vec::new();
        for p in t
            .texts()
            .list()
            .map_err(|e| material(TranslationError::from(e)))?
        {
            projects.push(project_json(&t, &p)?);
        }
        Ok(json!({ "projects": projects }))
    })
    .await?;
    Ok(Json(projects))
}

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

fn memory_provider(
    root: &std::path::Path,
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
            Ok(json!({ "records": build.records, "skipped": build.skipped.len() }))
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

/// 一个项目：项目摘要、上下文版本、章块目录（`outline`）与各块状态。
pub(crate) async fn project(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let (summary, versions, outline) = blocking({
        let root = root.clone();
        let id = id.clone();
        move || {
            let t = Translations::new(&root);
            let p = t.project(&id).map_err(material)?;
            let versions = t.context_versions(&id).map_err(material)?;
            let outline = p
                .chapters
                .iter()
                .map(|c| {
                    json!({
                        "chapter": c.id,
                        "source_name": c.source_name,
                        "title": c.parsing.as_ref().map(|p| p.title.as_str()).unwrap_or(&c.source_name),
                        "blocks": c.blocks.iter().map(|b| b.block_id.clone()).collect::<Vec<_>>(),
                    })
                })
                .collect::<Vec<_>>();
            Ok((project_json(&t, &p)?, versions, outline))
        }
    })
    .await?;
    let statuses = app
        .session_result({
            let root = root.clone();
            let id = id.clone();
            move |s| s.translation_status(&root, &id)
        })
        .await?;
    let (founded, statuses) = match statuses {
        Ok(statuses) => (
            true,
            statuses.iter().map(status_json).collect::<Vec<Value>>(),
        ),
        Err(AppError::NotFounded) => (false, Vec::new()),
        Err(e) => return Err(ApiError::from(e)),
    };
    let version = versions.last().copied().unwrap_or(0);
    Ok(Json(json!({
        "project": summary,
        "context": { "version": version, "versions": versions },
        "outline": outline,
        "statuses": statuses,
        "founded": founded,
    })))
}

/// 一块的全部材料：原文、译稿与结构检查、朱批、上下文与前后衔接、审校回奏、建议、
/// 状态与待准修订（后两者须朝廷）。
pub(crate) async fn block(
    State(app): State<AppState>,
    UrlPath((id, block)): UrlPath<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let loaded = blocking({
        let root = root.clone();
        let id = id.clone();
        let block = block.clone();
        move || {
            let t = Translations::new(&root);
            let p = t.project(&id).map_err(material)?;
            let chapter = p
                .chapters
                .iter()
                .find_map(|c| {
                    c.blocks
                        .iter()
                        .find(|b| b.block_id == block)
                        .map(|b| (c.id.clone(), b))
                })
                .ok_or_else(|| missing_block(&id, &block))?;
            let dir = t.block_dir(&id, &block);
            // 首次下诏前还没有导出的材料：原文取项目内的块副本（内容与 source.json 逐字节相同）。
            let (source, origin) = match material_file(&dir, "source.json") {
                (Some(value), _, _) => (value, "exported"),
                _ => {
                    let bytes = chapter
                        .1
                        .serialized()
                        .map_err(|e| material(TranslationError::from(e)))?;
                    let value = serde_json::from_slice(&bytes)
                        .map_err(|e| material(TranslationError::Invalid(e.to_string())))?;
                    (value, "project")
                }
            };
            let bytes = t.read_translation(&id, &block).map_err(material)?;
            let (translation, translation_error) = match &bytes {
                None => (None, None),
                Some(bytes) => match serde_json::from_slice::<Value>(bytes) {
                    Ok(value) => (Some(value), None),
                    Err(e) => (None, Some(e.to_string())),
                },
            };
            let check = match &bytes {
                None => None,
                Some(bytes) => {
                    let report = t
                        .texts()
                        .check(&id, &block, bytes)
                        .map_err(|e| material(TranslationError::from(e)))?;
                    Some(serde_json::to_value(report).unwrap_or(Value::Null))
                }
            };
            let zhupi = t.zhupi(&id, &block).map_err(material)?;
            let mut proposals = Vec::new();
            if let Ok(entries) = std::fs::read_dir(dir.join("proposals")) {
                for name in entries
                    .flatten()
                    .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                {
                    proposals.push(name);
                }
            }
            proposals.sort();
            Ok((
                chapter.0,
                source,
                origin,
                translation,
                translation_error,
                check,
                zhupi,
                material_file(&dir, "context.json"),
                material_file(&dir, "neighbors.json"),
                material_file(&dir, "reviews.json"),
                material_file(&dir, "zhupi.json"),
                proposals,
            ))
        }
    })
    .await?;
    let (
        chapter,
        source,
        origin,
        translation,
        translation_error,
        check,
        zhupi,
        context,
        neighbors,
        reviews,
        zhupi_export,
        proposals,
    ) = loaded;
    let extra = app
        .session_result({
            let root = root.clone();
            let id = id.clone();
            let block = block.clone();
            move |s| {
                let status = s
                    .translation_status(&root, &id)?
                    .into_iter()
                    .find(|b| b.block == block);
                let pending = pending_json(s.court(), &id, &block);
                Ok((status, pending))
            }
        })
        .await?;
    let (founded, status, pending) = match extra {
        Ok((status, pending)) => (true, status.as_ref().map(status_json), pending),
        Err(AppError::NotFounded) => (false, None, None),
        Err(e) => return Err(ApiError::from(e)),
    };
    // 导出材料里的解析失败照实给出（如 reviews.json 被人手改坏），页面只显示、不改。
    let pair = |value: Option<Value>, error: Option<String>, text: String| json!({ "value": value, "error": error, "text": text });
    Ok(Json(json!({
        "project": id,
        "block": block,
        "chapter": chapter,
        "founded": founded,
        "status": status,
        "pending": pending,
        "source": source,
        "source_origin": origin,
        "translation": translation,
        "translation_error": translation_error,
        "check": check,
        "zhupi": zhupi,
        "context": pair(context.0, context.1, context.2),
        "neighbors": pair(neighbors.0, neighbors.1, neighbors.2),
        "reviews": pair(reviews.0, reviews.1, reviews.2),
        "zhupi_export": pair(zhupi_export.0, zhupi_export.1, zhupi_export.2),
        "proposals": proposals,
    })))
}

/// 批准上下文草稿：先按项目建好空草稿（缺的术语表与文体），再存为新快照。
pub(crate) async fn approve_context(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let version = blocking(move || {
        let t = Translations::new(&root);
        t.ensure_context_draft(&id).map_err(material)?;
        t.approve_context(&id).map_err(material)
    })
    .await?;
    app.emit("changed", json!({}));
    Ok(Json(json!({ "version": version })))
}

/// 逐段朱批：`{ paragraph, text }`；`text` 为空（或只有空白）即清除该段朱批。
pub(crate) async fn zhupi(
    State(app): State<AppState>,
    UrlPath((id, block)): UrlPath<(String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let paragraph = str_field(&body, "paragraph")?.trim().to_owned();
    let text = body
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let root = app.0.config.root.clone();
    let result = blocking(move || {
        let t = Translations::new(&root);
        if text.trim().is_empty() {
            t.clear_zhupi(&id, &block, &paragraph).map_err(material)?;
        } else {
            t.set_zhupi(&id, &block, &paragraph, &text)
                .map_err(material)?;
        }
        let zhupi = t.zhupi(&id, &block).map_err(material)?;
        Ok(json!({ "version": zhupi.version, "items": zhupi.items }))
    })
    .await?;
    app.emit("changed", json!({}));
    Ok(Json(result))
}

/// 御览定稿：返回定稿的译稿哈希。
pub(crate) async fn finalize(
    State(app): State<AppState>,
    UrlPath((id, block)): UrlPath<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let (snapshot_root, snapshot_id, snapshot_block) = (root.clone(), id.clone(), block.clone());
    let hash = app
        .with_session(move |s| s.finalize_translation(&root, &id, &block))
        .await?;
    let snapshot = blocking(move || {
        Ok::<_, ApiError>(Translations::new(&snapshot_root).auto_snapshot(
            &snapshot_id,
            &snapshot_block,
            ziyuan_app::translation::VersionState::Finalized,
        ))
    })
    .await
    .ok()
    .flatten();
    app.emit("changed", json!({}));
    let mut value = json!({ "translation_hash": hash });
    if let Some(snapshot) = snapshot {
        value["snapshot"] = serde_json::to_value(snapshot).unwrap_or(Value::Null);
    }
    Ok(Json(value))
}

/// 退回重修：`{ reason }`。
pub(crate) async fn reopen(
    State(app): State<AppState>,
    UrlPath((id, block)): UrlPath<(String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let reason = str_field(&body, "reason")?.trim().to_owned();
    if reason.is_empty() {
        return Err(ApiError::bad("退回须写明理由"));
    }
    let root = app.0.config.root.clone();
    app.with_session(move |s| s.reopen_translation(&root, &id, &block, &reason))
        .await?;
    app.emit("changed", json!({}));
    Ok(Json(json!({ "ok": true })))
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

/// 下诏：`{ kind: "translate" | "review" | "revise" }`，只入队不执行。审校一次下两件（忠实、文风）。
pub(crate) async fn issue(
    State(app): State<AppState>,
    UrlPath((id, block)): UrlPath<(String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let kind: TranslationKind = parse::<TranslationKind>(str_field(&body, "kind")?)?;
    let root = app.0.config.root.clone();
    let tasks = app
        .with_session(move |s| {
            let ids = match kind {
                TranslationKind::Translate => {
                    vec![s.issue_translation(&root, &id, &block)?.id.0]
                }
                TranslationKind::Revise => vec![s.issue_revise(&root, &id, &block)?.id.0],
                TranslationKind::ReviewFidelity | TranslationKind::ReviewStyle => s
                    .issue_review(&root, &id, &block)?
                    .iter()
                    .map(|e| e.id.0)
                    .collect(),
            };
            Ok(ids)
        })
        .await?;
    app.emit("changed", json!({}));
    Ok(Json(json!({ "tasks": tasks })))
}

/// 御笔札记：`?project=`、`?kind=`、`?search=` 筛选（都可省）。
pub(crate) async fn notes_list(
    State(app): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let get = |key: &str| {
        query
            .get(key)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let kind = match get("kind") {
        None => None,
        Some(kind) => Some(parse::<NoteKind>(&kind)?),
    };
    let filter = NoteFilter {
        project: get("project"),
        kind,
        search: get("search"),
    };
    let root = app.0.config.root.clone();
    let notes = blocking(move || Notes::new(&root).list(&filter).map_err(material)).await?;
    Ok(Json(json!(notes)))
}

/// 记一条札记：`{ kind, source, zh, remark?, project?, block?, paragraph? }`。
pub(crate) async fn note_add(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let kind = parse::<NoteKind>(body["kind"].as_str().unwrap_or("word"))?;
    let new = NewNote {
        kind,
        source: str_field(&body, "source")?.to_owned(),
        zh: str_field(&body, "zh")?.to_owned(),
        remark: body
            .get("remark")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        project: opt_field(&body, "project"),
        block: opt_field(&body, "block"),
        paragraph: opt_field(&body, "paragraph"),
    };
    let root = app.0.config.root.clone();
    let note = blocking(move || Notes::new(&root).add(new).map_err(material)).await?;
    Ok(Json(json!({ "note": note })))
}

/// 改一条札记：只改给了的字段（`remark` 给空串即清空）。
pub(crate) async fn note_edit(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let kind = match opt_field(&body, "kind") {
        None => None,
        Some(kind) => Some(parse::<NoteKind>(&kind)?),
    };
    let edit = NoteEdit {
        kind,
        source: opt_field(&body, "source"),
        zh: opt_field(&body, "zh"),
        remark: body
            .get("remark")
            .and_then(Value::as_str)
            .map(str::to_owned),
    };
    let root = app.0.config.root.clone();
    let note = blocking(move || Notes::new(&root).edit(&id, edit).map_err(material)).await?;
    Ok(Json(json!({ "note": note })))
}

/// 删一条札记。
pub(crate) async fn note_remove(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let removed = blocking(move || Notes::new(&root).remove(&id).map_err(material)).await?;
    Ok(Json(json!({ "ok": true, "id": removed.id })))
}

/// 并入项目上下文草稿：`{ project }`；之后须 `text context approve` 才生效。
pub(crate) async fn note_promote(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let project = str_field(&body, "project")?.trim().to_owned();
    let root = app.0.config.root.clone();
    let promotion =
        blocking(move || Notes::new(&root).promote(&id, &project).map_err(material)).await?;
    Ok(Json(json!({
        "note": promotion.note,
        "path": promotion.path,
        "added": promotion.added,
        "requires_context_approval": true,
    })))
}

// ---- 人工译稿：只读查看、草稿保存、送审、准奏、定稿 ----------------------------------
//
// 一律经应用层的同一批接口（CLI `ziyuan text human …` 用的是这些）：查看只读、不取写锁；
// 保存只写草稿文件；送审只发布绑定（并冻结送审快照）；准奏才在受管写入里落盘 `zh.json`；
// 定稿只记定稿事实。没有朝廷时送审／准奏／定稿都用同一句“先开国”提示，不伪造 Founded。

const HUMAN_SUBMIT_NO_COURT: &str = "人工送审需要先开国（ziyuan init）：先建朝，送审与准奏事实才有事件簿可记。下一步：运行 ziyuan init 后再送审。";
const HUMAN_APPROVE_NO_COURT: &str = "人工准奏需要先开国（ziyuan init）：准奏事实要入事件簿、译稿要落盘。下一步：运行 ziyuan init 后再准奏。";
const HUMAN_FINALIZE_NO_COURT: &str = "人工定稿需要先开国（ziyuan init）：定稿事实要入事件簿。下一步：运行 ziyuan init 后再御览定稿。";

/// 在报错文字后补一条网页上的下一步；认不出就用原话。
fn human_hint(message: &str) -> String {
    let next = if message.contains("尚无人工草稿") || message.contains("尚无可提交的人工草稿")
    {
        "下一步：先保存草稿（POST /api/text/{项目}/blocks/{块}/human/draft）。"
    } else if message.contains("尚未送审") {
        "下一步：先送审（POST /api/text/{项目}/blocks/{块}/human/submit）。"
    } else if message.contains("不是人工准奏") {
        "下一步：先准奏（POST /api/text/{项目}/blocks/{块}/human/approve），再御览定稿。"
    } else if message.contains("须先完成工作区换版") || message.contains("恢复记录") {
        "下一步：先运行 ziyuan text recovery status 收尾恢复记录，再重试。"
    } else if message.contains("结构检查") || message.contains("段落归属与完整性") {
        "下一步：补齐或改正草稿段落后重新保存，再送审。"
    } else {
        return message.to_owned();
    };
    format!("{message}\n{next}")
}

/// 读侧（查看、进度）的译书错误：材料问题一律 400，报错文字与 CLI 相同。
fn human_read_error(error: TranslationError) -> ApiError {
    match error {
        TranslationError::Invalid(message) => ApiError::bad(human_hint(&message)),
        other => material(other),
    }
}

/// 写侧的译书错误：修订／材料／请求号冲突都是 409，其余按材料的 400 归类。
fn human_write_error(error: TranslationError) -> ApiError {
    match error {
        TranslationError::DraftConflict { current } => ApiError(
            StatusCode::CONFLICT,
            format!(
                "人工草稿修订冲突（现行修订为 {}），本次未保存任何内容；请保留输入并重新读取后再保存。",
                current.unwrap_or_else(|| "无（尚无草稿）".into())
            ),
        ),
        TranslationError::DraftBasisChanged => ApiError(
            StatusCode::CONFLICT,
            "人工草稿所据的原文、上下文或朱批已变，本次未做任何改动。请重新读取后再保存（GET /api/text/{项目}/blocks/{块}/human）。".into(),
        ),
        TranslationError::DraftRequestConflict => ApiError(
            StatusCode::CONFLICT,
            "这个请求号已被另一份内容占用，本次未做任何改动。下一步：换一个新的 request_id 重新保存。".into(),
        ),
        TranslationError::Invalid(message) => ApiError::bad(human_hint(&message)),
        other => material(other),
    }
}

/// 空事件簿才是未开国；读取失败单独报告，不泄露底层路径。
fn founded(root: &Path) -> Result<bool, ApiError> {
    EventLog::new(root)
        .load()
        .map(|history| !history.is_empty())
        .map_err(|_| ApiError::bad("事件簿无法读取，本次未做任何改动。请核对事件簿与文件权限，并运行 ziyuan text recovery status 检查恢复现场。"))
}

/// 人工准奏／定稿事实：只从回放得出，不取锁、不写盘。
fn human_json(court: &Court, project: &str, block: &str, state: Option<&BlockState>) -> Value {
    let (approved, finalized, operation_id, stale) = match state {
        Some(BlockState::HumanApproved { operation_id }) => {
            (true, false, Some(operation_id.clone()), false)
        }
        Some(BlockState::HumanFinalized { operation_id, .. }) => {
            (true, true, Some(operation_id.clone()), false)
        }
        // 版本失效（人工）：准奏事实仍在，但不当作可导出；理由由 status 给出。
        Some(BlockState::HumanStale { operation_id, .. }) => {
            (true, false, Some(operation_id.clone()), true)
        }
        _ => (false, false, None, false),
    };
    let approval = operation_id.as_deref().and_then(|id| {
        court
            .human_approvals(project, block)
            .find(|a| a.operation_id == id)
    });
    let target_hash = approval
        .map(|a| a.request.target.after.clone())
        .or_else(|| match state {
            Some(BlockState::HumanFinalized {
                translation_hash, ..
            }) => Some(translation_hash.clone()),
            _ => None,
        });
    json!({
        "approved": approved,
        "finalized": finalized,
        "stale": stale,
        "operation_id": operation_id,
        "target_format": approval.map(|a| a.request.target.format.clone()),
        "target_hash": target_hash,
    })
}

/// 没有朝廷时的人工事实占位。
fn no_human() -> Value {
    json!({
        "approved": false,
        "finalized": false,
        "stale": false,
        "operation_id": null,
        "target_format": null,
        "target_hash": null,
    })
}

/// 一块的人工稿（只读）：现行材料基准、草稿（可为空）、状态与准奏／定稿事实。
pub(crate) async fn human_view(
    State(app): State<AppState>,
    UrlPath((project, block)): UrlPath<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let view: DraftView = blocking({
        let (root, project, block) = (root.clone(), project.clone(), block.clone());
        move || {
            Translations::new(&root)
                .peek_human_draft(&project, &block)
                .map_err(human_read_error)
        }
    })
    .await?;
    let basis = serde_json::to_value(&view.basis).unwrap_or(Value::Null);
    let revision = view.draft.as_ref().map(|d| d.revision.clone());
    let draft = view
        .draft
        .as_ref()
        .map(|d| serde_json::to_value(d).unwrap_or(Value::Null));
    let has_draft = view.draft.is_some();
    drop(view);
    let extra = app
        .session_result({
            let (root, project, block) = (root.clone(), project.clone(), block.clone());
            move |s| {
                let status = s
                    .translation_status(&root, &project)?
                    .into_iter()
                    .find(|b| b.block == block);
                let human = human_json(
                    s.court(),
                    &project,
                    &block,
                    status.as_ref().map(|b| &b.state),
                );
                Ok((status.map(|b| status_json(&b)), human))
            }
        })
        .await?;
    let (founded, status, human) = match extra {
        Ok((status, human)) => (true, status, human),
        Err(AppError::NotFounded) => (false, None, no_human()),
        Err(e) => return Err(ApiError::from(e)),
    };
    Ok(Json(json!({
        "project": project,
        "block": block,
        "founded": founded,
        "basis": basis,
        "draft": draft,
        "revision": revision,
        "message": if has_draft {
            Value::Null
        } else {
            json!("尚无草稿（只读）：先保存人工草稿，再送审、准奏与御览定稿。")
        },
        "status": status,
        "human": human,
    })))
}

/// 人工进度：按章计数、全项目合计与下一块待草稿的块。
///
/// 口径：`drafted` 以块存在非全空的草稿为准；`submitted` 以事件簿里有该块的送审绑定为准；
/// `approved` / `finalized` 取块状态的 `human_approved` / `human_finalized`（人工版本失效
/// 计为已准奏，理由见该块状态）；`not_started` 是还没有草稿的块数。各计数可重叠。
pub(crate) async fn human_progress(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let (chapters, drafted) = blocking({
        let (root, id) = (root.clone(), id.clone());
        move || {
            let t = Translations::new(&root);
            let project = t.project(&id).map_err(material)?;
            let mut drafted = BTreeSet::new();
            let mut chapters = Vec::new();
            for chapter in &project.chapters {
                let mut blocks = Vec::new();
                for block in &chapter.blocks {
                    if t.human_draft_written(&id, &block.block_id)
                        .map_err(human_read_error)?
                    {
                        drafted.insert(block.block_id.clone());
                    }
                    blocks.push(block.block_id.clone());
                }
                chapters.push((chapter.id.clone(), blocks));
            }
            Ok::<_, ApiError>((chapters, drafted))
        }
    })
    .await?;
    let extra = app
        .session_result({
            let (root, id) = (root.clone(), id.clone());
            move |s| {
                let history = s.history()?;
                let submitted: BTreeSet<String> = history
                    .iter()
                    .filter_map(|event| match event {
                        CourtEvent::HumanSubmissionBound { binding }
                            if binding.request.project == id =>
                        {
                            Some(binding.request.block.clone())
                        }
                        _ => None,
                    })
                    .collect();
                let mut approved = BTreeSet::new();
                let mut finalized = BTreeSet::new();
                for status in s.translation_status(&root, &id)? {
                    match status.state {
                        BlockState::HumanApproved { .. } | BlockState::HumanStale { .. } => {
                            approved.insert(status.block);
                        }
                        BlockState::HumanFinalized { .. } => {
                            approved.insert(status.block.clone());
                            finalized.insert(status.block);
                        }
                        _ => {}
                    }
                }
                Ok((submitted, approved, finalized))
            }
        })
        .await?;
    let (founded, submitted, approved, finalized) = match extra {
        Ok((submitted, approved, finalized)) => (true, submitted, approved, finalized),
        Err(AppError::NotFounded) => (false, BTreeSet::new(), BTreeSet::new(), BTreeSet::new()),
        Err(e) => return Err(ApiError::from(e)),
    };
    let counts = |blocks: &[String]| {
        let drafted = blocks.iter().filter(|b| drafted.contains(*b)).count();
        json!({
            "total": blocks.len(),
            "drafted": drafted,
            "submitted": blocks.iter().filter(|b| submitted.contains(*b)).count(),
            "approved": blocks.iter().filter(|b| approved.contains(*b)).count(),
            "finalized": blocks.iter().filter(|b| finalized.contains(*b)).count(),
            "not_started": blocks.len() - drafted,
        })
    };
    let mut totals: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut per_chapter = Vec::new();
    let mut next_untranslated = None;
    for (chapter, blocks) in &chapters {
        let mut entry = json!({ "chapter": chapter });
        if let (Some(entry), Some(counts)) = (entry.as_object_mut(), counts(blocks).as_object()) {
            entry.extend(counts.clone());
        }
        for key in [
            "total",
            "drafted",
            "submitted",
            "approved",
            "finalized",
            "not_started",
        ] {
            *totals.entry(key).or_default() += entry[key].as_u64().unwrap_or(0) as usize;
        }
        if next_untranslated.is_none() {
            next_untranslated = blocks.iter().find(|b| !drafted.contains(*b)).cloned();
        }
        per_chapter.push(entry);
    }
    Ok(Json(json!({
        "project": id,
        "founded": founded,
        "chapters": per_chapter,
        "totals": totals,
        "next_untranslated": next_untranslated,
    })))
}

/// 保存人工草稿：请求体即 `DraftSave`（request_id、expected_revision、basis、paragraphs）。
/// 修订冲突、材料变化与请求号占用都回 409，并给出下一步；旧稿保持不动。
pub(crate) async fn human_draft_save(
    State(app): State<AppState>,
    UrlPath((project, block)): UrlPath<(String, String)>,
    Json(body): Json<DraftSave>,
) -> Result<Response, ApiError> {
    let root = app.0.config.root.clone();
    let saved = blocking({
        let (root, project, block) = (root.clone(), project.clone(), block.clone());
        move || {
            Ok::<_, ApiError>(Translations::new(&root).save_human_draft(&project, &block, &body))
        }
    })
    .await?;
    match saved {
        Ok(receipt) => {
            app.emit("changed", json!({}));
            Ok(Json(json!({
                "revision": receipt.revision,
                "content_hash": receipt.content_hash,
                "message": "已保存人工草稿。下一步：送审（POST /api/text/{项目}/blocks/{块}/human/submit）。",
            }))
            .into_response())
        }
        Err(TranslationError::DraftConflict { current }) => {
            Ok((StatusCode::CONFLICT, Json(json!({
                "error": "人工草稿修订冲突：这份草稿已不是最新，本次未保存任何内容。",
                "current": current,
                "message": "请重新读取（GET /api/text/{项目}/blocks/{块}/human）后再保存，并保留本次输入。",
            })))
            .into_response())
        }
        Err(TranslationError::DraftBasisChanged) => Ok((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "原文、上下文或朱批已变，本次未保存任何内容。请重新读取后再保存。",
                "current": serde_json::Value::Null,
                "message": "重新读取（GET /api/text/{项目}/blocks/{块}/human）取新的基准后再保存。",
            })),
        )
            .into_response()),
        Err(TranslationError::DraftRequestConflict) => Ok((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "这个请求号已被另一份内容占用，本次未保存任何内容。",
                "current": serde_json::Value::Null,
                "message": "换一个新的 request_id 重新保存（同号同内容的重复提交才返回原回执）。",
            })),
        )
            .into_response()),
        Err(other) => Err(human_write_error(other)),
    }
}

/// 送审：冻结当前草稿并发布绑定。重复发布返回 `published:false`（此前已入簿），不重复追加。
pub(crate) async fn human_submit(
    State(app): State<AppState>,
    UrlPath((project, block)): UrlPath<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let report = blocking({
        let (root, project, block) = (root.clone(), project.clone(), block.clone());
        move || {
            if !founded(&root)? {
                return Err(ApiError::bad(HUMAN_SUBMIT_NO_COURT));
            }
            let translations = Translations::new(&root);
            let view = translations
                .peek_human_draft(&project, &block)
                .map_err(human_read_error)?;
            let draft = view.draft.ok_or_else(|| {
                ApiError::bad(
                    "尚无人工草稿，无从送审。下一步：先保存草稿（POST /api/text/{项目}/blocks/{块}/human/draft）。",
                )
            })?;
            let request_id = human_submit_request_id(&draft.revision);
            let binding = translations
                .prepare_web_human_submission(&project, &block, &request_id, &request_id, &draft.revision)
                .map_err(human_write_error)?;
            let workspace = Workspace::open(&root)
                .map_err(|e| ApiError::bad(format!("工作区打不开（{e}）；请核对后再送审。")))?;
            let mut guard = workspace
                .lock(LockScope::Workspace)
                .map_err(|e| ApiError::bad(format!("{e}；送审未做任何改动。")))?;
            guard
                .upgrade_write_protocol()
                .map_err(|e| ApiError::bad(format!("{e}；送审未做任何改动。")))?;
            translations
                .texts()
                .upgrade_project_protocol(&project, &guard)
                .map_err(|e| ApiError::bad(human_hint(&format!("{e}；送审未做任何改动。"))))?;
            let published = translations
                .publish_web_human_binding(&guard, &binding)
                .map_err(human_write_error)?;
            Ok(json!({
                "published": published,
                "binding_hash": binding.request_hash,
                "submit_request_id": request_id,
                "message": if published {
                    "已送审并入簿。下一步：准奏（POST /api/text/{项目}/blocks/{块}/human/approve）。"
                } else {
                    "这次送审此前已入簿，未重复追加。下一步：准奏（POST /api/text/{项目}/blocks/{块}/human/approve）。"
                },
            }))
        }
    })
    .await?;
    app.emit("changed", json!({}));
    Ok(Json(report))
}

/// 准奏：最新一次的送审在一次受管写入里落盘 `zh.json` 并记准奏事实；重复准奏幂等。
pub(crate) async fn human_approve(
    State(app): State<AppState>,
    UrlPath((project, block)): UrlPath<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let report = blocking({
        let (root, project, block) = (root.clone(), project.clone(), block.clone());
        move || {
            if !founded(&root)? {
                return Err(ApiError::bad(HUMAN_APPROVE_NO_COURT));
            }
            let translations = Translations::new(&root);
            let workspace = Workspace::open(&root)
                .map_err(|e| ApiError::bad(format!("工作区打不开（{e}）；请核对后再准奏。")))?;
            let mut guard = workspace
                .lock(LockScope::Workspace)
                .map_err(|e| ApiError::bad(format!("{e}；准奏未做任何改动。")))?;
            guard
                .upgrade_write_protocol()
                .map_err(|e| ApiError::bad(format!("{e}；准奏未做任何改动。")))?;
            translations
                .texts()
                .upgrade_project_protocol(&project, &guard)
                .map_err(|e| ApiError::bad(human_hint(&format!("{e}；准奏未做任何改动。"))))?;
            let approved = translations
                .approve_web_human_submission(&mut guard, &project, &block)
                .map_err(human_write_error)?;
            let snapshot = translations.auto_snapshot(
                &project,
                &block,
                ziyuan_app::translation::VersionState::Approved,
            );
            // 操作身份与准奏目标只从回放读，不再写盘。
            let history = EventLog::new(&root).load().map_err(|e| {
                ApiError::internal(format!("准奏后事件簿读不出来（{e}）；请运行 ziyuan text recovery status。"))
            })?;
            let approval = history.iter().rev().find_map(|event| match event {
                CourtEvent::HumanTranslationApproved { approval }
                    if approval.request.project == project && approval.request.block == block =>
                {
                    Some(approval.as_ref())
                }
                _ => None,
            });
            Ok(json!({
                "approved": approved,
                "operation_id": approval.map(|a| a.operation_id.clone()),
                "target_format": approval.map(|a| a.request.target.format.clone()),
                "target_hash": approval.map(|a| a.request.target.after.clone()),
                "snapshot": snapshot.map(|s| serde_json::to_value(s).unwrap_or(Value::Null)),
                "message": if approved {
                    "已准奏并落盘：译稿写入 zh.json，准奏事实已入簿。下一步：御览定稿（POST /api/text/{项目}/blocks/{块}/human/finalize）。"
                } else {
                    "这次准奏此前已入簿并落盘，未重复追加。下一步：御览定稿（POST /api/text/{项目}/blocks/{块}/human/finalize）。"
                },
            }))
        }
    })
    .await?;
    app.emit("changed", json!({}));
    Ok(Json(report))
}

/// 御览定稿：只记定稿事实（要求磁盘已是准奏目标）；重复定稿幂等。
pub(crate) async fn human_finalize(
    State(app): State<AppState>,
    UrlPath((project, block)): UrlPath<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let report = blocking({
        let (root, project, block) = (root.clone(), project.clone(), block.clone());
        move || {
            if !founded(&root)? {
                return Err(ApiError::bad(HUMAN_FINALIZE_NO_COURT));
            }
            let translations = Translations::new(&root);
            let workspace = Workspace::open(&root)
                .map_err(|e| ApiError::bad(format!("工作区打不开（{e}）；请核对后再定稿。")))?;
            let mut guard = workspace
                .lock(LockScope::Workspace)
                .map_err(|e| ApiError::bad(format!("{e}；定稿未做任何改动。")))?;
            guard
                .upgrade_write_protocol()
                .map_err(|e| ApiError::bad(format!("{e}；定稿未做任何改动。")))?;
            translations
                .texts()
                .upgrade_project_protocol(&project, &guard)
                .map_err(|e| ApiError::bad(human_hint(&format!("{e}；定稿未做任何改动。"))))?;
            let finalized = translations
                .finalize_human_translation(&guard, &project, &block)
                .map_err(human_write_error)?;
            let snapshot = translations.auto_snapshot(
                &project,
                &block,
                ziyuan_app::translation::VersionState::Finalized,
            );
            Ok(json!({
                "finalized": finalized,
                "snapshot": snapshot.map(|s| serde_json::to_value(s).unwrap_or(Value::Null)),
                "message": if finalized {
                    "已御览定稿并入簿，可导出成品（ziyuan text export-finished）。"
                } else {
                    "这次定稿此前已入簿，未重复追加。可直接导出成品（ziyuan text export-finished）。"
                },
            }))
        }
    })
    .await?;
    app.emit("changed", json!({}));
    Ok(Json(report))
}

// ---- 译稿版本快照：列表、手工保存、对比与清理 ----

pub(crate) async fn versions_list(
    State(app): State<AppState>,
    UrlPath((project, block)): UrlPath<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let versions = blocking({
        let (root, project, block) = (root.clone(), project.clone(), block.clone());
        move || {
            Translations::new(&root)
                .list_translation_versions(&project, &block)
                .map_err(material)
        }
    })
    .await?;
    Ok(Json(json!({
        "project": project,
        "block": block,
        "versions": versions,
    })))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VersionSaveBody {
    #[serde(default)]
    label: Option<String>,
}

/// 手工保存当前译稿；观政由令牌中间件挡在 POST 之外。
pub(crate) async fn version_save(
    State(app): State<AppState>,
    UrlPath((project, block)): UrlPath<(String, String)>,
    Json(body): Json<VersionSaveBody>,
) -> Result<Json<Value>, ApiError> {
    let label = body.label.unwrap_or_default();
    let root = app.0.config.root.clone();
    let info = blocking(move || {
        Translations::new(&root)
            .snapshot_translation(&project, &block, &label)
            .map_err(material)
    })
    .await?;
    app.emit("changed", json!({}));
    Ok(Json(json!({ "version": info })))
}

#[derive(serde::Deserialize)]
pub(crate) struct VersionCompareQuery {
    #[serde(default)]
    against: Option<String>,
}

/// 对比快照与现行译稿（`current`，默认）或人工草稿（`draft`）。
pub(crate) async fn version_compare(
    State(app): State<AppState>,
    UrlPath((project, block, id)): UrlPath<(String, String, String)>,
    Query(query): Query<VersionCompareQuery>,
) -> Result<Json<Value>, ApiError> {
    let against_draft = match query.against.as_deref() {
        None | Some("current") => false,
        Some("draft") => true,
        Some(_) => return Err(ApiError::bad("对比对象只接受 current 或 draft")),
    };
    let root = app.0.config.root.clone();
    let changes = blocking({
        let (project, block, id) = (project.clone(), block.clone(), id.clone());
        move || {
            Translations::new(&root)
                .compare_version(&project, &block, &id, against_draft)
                .map_err(material)
        }
    })
    .await?;
    Ok(Json(json!({
        "project": project,
        "block": block,
        "id": id,
        "against": if against_draft { "draft" } else { "current" },
        "changes": changes,
        "message": if changes.is_empty() {
            "没有差异。"
        } else {
            "按段列出差异；标记差异只表示译文相同、标记有变。"
        },
    })))
}

#[derive(serde::Deserialize)]
pub(crate) struct VersionPruneBody {
    #[serde(default)]
    keep: Option<usize>,
    #[serde(default)]
    include_manual: bool,
    /// 默认只预览；页面需再次确认后才删除。
    #[serde(default = "default_true")]
    dry_run: bool,
}

fn default_true() -> bool {
    true
}

/// 清理旧快照：默认只预览，`dry_run:false` 才删除。人工快照默认保留。
pub(crate) async fn versions_prune(
    State(app): State<AppState>,
    UrlPath((project, block)): UrlPath<(String, String)>,
    Json(body): Json<VersionPruneBody>,
) -> Result<Json<Value>, ApiError> {
    let keep = body.keep.unwrap_or(3);
    if keep == 0 || keep > 100 {
        return Err(ApiError::bad("保留份数须在 1–100 之间"));
    }
    let root = app.0.config.root.clone();
    let report = blocking(move || {
        Translations::new(&root)
            .prune_versions(&project, &block, keep, body.include_manual, !body.dry_run)
            .map_err(material)
    })
    .await?;
    app.emit("changed", json!({}));
    Ok(Json(json!(report)))
}
