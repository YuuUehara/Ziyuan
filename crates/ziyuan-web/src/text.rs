//! 翰林院 · 译书与御笔札记（武英殿卡片）的 API：项目与块材料、上下文、朱批、御览与札记。
//!
//! 材料读写（项目、上下文草稿、朱批、札记）不经朝廷；块进度与待准修订须朝廷回放，
//! 未建朝时 `founded` 为假、`statuses` / `status` / `pending` 为空，而不是报错。
//! 材料只经应用层 API 读写；导出目录里的 `source.json`、`context.json` 等只读。

use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use ziyuan_app::notes::{NewNote, NoteEdit, NoteFilter, NoteKind, Notes};
use ziyuan_app::text_project::Project;
use ziyuan_app::translation::{BlockStatus, TranslationError, Translations};
use ziyuan_app::{AppError, selected_files};
use ziyuan_core::TranslationKind;
use ziyuan_governance::{ChangeState, Court};

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
    match serde_json::from_slice(&bytes) {
        Ok(value) => (Some(value), None, text),
        Err(e) => (None, Some(e.to_string()), text),
    }
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
    let hash = app
        .with_session(move |s| s.finalize_translation(&root, &id, &block))
        .await?;
    app.emit("changed", json!({}));
    Ok(Json(json!({ "translation_hash": hash })))
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
