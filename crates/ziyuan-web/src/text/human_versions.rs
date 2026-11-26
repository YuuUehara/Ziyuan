//! 人工译稿（查看、进度、草稿保存、送审、准奏、定稿）与三态译稿版本快照（列表、保存、对比、清理）。

use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use ziyuan_app::AppError;
use ziyuan_app::text_project::Project;
use ziyuan_app::translation::{
    BlockState, DraftSave, DraftView, TranslationError, Translations, human_submit_request_id,
};
use ziyuan_core::CourtEvent;
use ziyuan_governance::Court;
use ziyuan_storage::{EventLog, EventStore, LockScope, Workspace};

use super::{blocking, material, status_json};
use crate::state::{ApiError, AppState};

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
        "下一步：请先在页面点「保存人工草稿」。"
    } else if message.contains("尚未送审") {
        "下一步：请先点「送审当前已保存修订」。"
    } else if message.contains("不是人工准奏") {
        "下一步：请先点「人工准奏」，再点「人工御览定稿」。"
    } else if message.contains("须先完成工作区换版") || message.contains("恢复记录") {
        "下一步：请先完成工作区恢复，再重试。"
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
            "人工草稿所据的原文、上下文或朱批已变，本次未做任何改动。请点击「刷新人工状态」或重新读取后再保存。".into(),
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
fn human_json(
    court: &Court,
    project: &str,
    block: &str,
    revision: Option<&str>,
    state: Option<&BlockState>,
) -> Value {
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
    let submitted = revision.is_some_and(|rev| {
        court
            .human_submission_binding(project, block, &human_submit_request_id(rev))
            .is_some()
    });
    json!({
        "submitted": submitted,
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
        "submitted": false,
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
    let (p, view): (Project, DraftView) = blocking({
        let (root, project, block) = (root.clone(), project.clone(), block.clone());
        move || {
            let t = Translations::new(&root);
            let p = t.project(&project).map_err(material)?;
            let view = t
                .peek_human_draft_in(&p, &block)
                .map_err(human_read_error)?;
            Ok::<_, ApiError>((p, view))
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
            let (root, project, block, revision) = (
                root.clone(),
                project.clone(),
                block.clone(),
                revision.clone(),
            );
            move |s| {
                let status = s
                    .translation_status_for(&root, &p)?
                    .into_iter()
                    .find(|b| b.block == block);
                let human = human_json(
                    s.court(),
                    &project,
                    &block,
                    revision.as_deref(),
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
pub(crate) async fn human_progress(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let (project, chapters, drafted) = blocking({
        let (root, id) = (root.clone(), id.clone());
        move || {
            let t = Translations::new(&root);
            let project = t.project(&id).map_err(material)?;
            let mut drafted = BTreeSet::new();
            let mut chapters = Vec::new();
            for chapter in &project.chapters {
                let mut blocks = Vec::new();
                for block in &chapter.blocks {
                    if t.human_draft_written_in(&project, &block.block_id)
                        .map_err(human_read_error)?
                    {
                        drafted.insert(block.block_id.clone());
                    }
                    blocks.push(block.block_id.clone());
                }
                chapters.push((chapter.id.clone(), blocks));
            }
            Ok::<_, ApiError>((project, chapters, drafted))
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
                for status in s.translation_status_for(&root, &project)? {
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
                "message": "已保存人工草稿。下一步：点击「送审当前已保存修订」。",
            }))
            .into_response())
        }
        Err(TranslationError::DraftConflict { current }) => Ok((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "人工草稿修订冲突：这份草稿已不是最新，本次未保存任何内容。",
                "current": current,
                "message": "请点击「刷新人工状态」或载入最新稿件后再保存，并保留本次输入。",
            })),
        )
            .into_response()),
        Err(TranslationError::DraftBasisChanged) => Ok((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "原文、上下文或朱批已变，本次未保存任何内容。请重新读取后再保存。",
                "current": serde_json::Value::Null,
                "message": "请先核对最新参考材料（或刷新人工状态）后再保存。",
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
                ApiError::bad("尚无人工草稿，无从送审。下一步：请先在页面点「保存人工草稿」。")
            })?;
            let request_id = human_submit_request_id(&draft.revision);
            let binding = translations
                .prepare_web_human_submission(
                    &project,
                    &block,
                    &request_id,
                    &request_id,
                    &draft.revision,
                )
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
                    "已送审并入簿。下一步：点击「人工准奏」。"
                } else {
                    "这次送审此前已入簿，未重复追加。下一步：点击「人工准奏」。"
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
                    "已准奏并落盘：译稿已写入成稿文件，准奏事实已入簿。下一步：点击「人工御览定稿」。"
                } else {
                    "这次准奏此前已入簿并落盘，未重复追加。下一步：点击「人工御览定稿」。"
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
                    "已御览定稿并入簿，现在可以前往「通读交稿」导出成品。"
                } else {
                    "这次定稿此前已入簿，未重复追加。可以直接前往「通读交稿」导出成品。"
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
