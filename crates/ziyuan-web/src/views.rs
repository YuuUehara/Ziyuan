//! 只读的 API：朝廷、起居注、廷推建议、考成、督职、预算、实录与 SSE 事件流。

use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::{Extension, Json};
use futures_util::stream::{self, Stream};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::convert::Infallible;
use tokio::sync::broadcast;
use ziyuan_app::{
    HistoryFilter, duzhi_report, format_time, history_entries, kaocheng_report, parse_time,
    tz_offset,
};
use ziyuan_core::{AgentId, Office, TaskId};

use crate::auth::Role;
use crate::json::{RECENT_TASKS, chronicle_json, court_json, office_json, record_json};
use crate::state::{ApiError, AppState, parse};

/// 朝廷全貌；诏令只含待办理会的与最近 [`RECENT_TASKS`] 件，`?all=1` 为全部。
pub(crate) async fn state(
    State(app): State<AppState>,
    Extension(role): Extension<Role>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let running = app.running();
    let backend = app.0.config.backend.clone();
    let recent = (query.get("all").map(String::as_str) != Some("1")).then_some(RECENT_TASKS);
    let mut value = app
        .with_session(move |s| Ok(court_json(s.court(), backend.as_ref(), recent)))
        .await?;
    value["running"] = json!(running);
    value["watching"] = json!(app.watching());
    value["github"] = json!(app.0.config.github.as_ref().map(|g| g.repo().to_owned()));
    value["viewer"] = json!(role == Role::Viewer);
    Ok(Json(value))
}

/// 起居注：可按 `task`、`official`、`kind`、`since`、`until`（同 CLI：`2h`、`2026-10-25 14:30`）筛选；
/// 只回最近的 `limit`（默认 500）条，`truncated` 为真表示还有更早的。
pub(crate) async fn history(
    State(app): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let get = |key: &str| query.get(key).map(|v| v.trim()).filter(|v| !v.is_empty());
    let id = |key: &str| -> Result<Option<u64>, ApiError> {
        get(key)
            .map(|v| {
                v.parse()
                    .map_err(|_| ApiError::bad(format!("{key} 须为数字")))
            })
            .transpose()
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let offset = tz_offset();
    let time = |key: &str| -> Result<Option<u64>, ApiError> {
        get(key)
            .map(|v| parse_time(v, now, offset).map_err(ApiError::bad))
            .transpose()
    };
    let limit = id("limit")?.map_or(500, |n| n.max(1) as usize);
    let filter = HistoryFilter {
        task: id("task")?.map(TaskId),
        agent: id("official")?.map(AgentId),
        kind: get("kind").map(str::to_lowercase),
        since: time("since")?,
        until: time("until")?,
        // 多取一条，以知是否还有更早的。
        limit: Some(limit + 1),
    };
    let mut entries = app
        .with_session(move |s| Ok(history_entries(s.history()?, &filter)))
        .await?
        .map_err(ApiError::bad)?;
    let truncated = entries.len() > limit;
    if truncated {
        entries.remove(0);
    }
    let entries: Vec<Value> = entries
        .into_iter()
        .map(|e| {
            json!({
                "seq": e.seq,
                "time": e.at.map(|t| format_time(t, offset)),
                "kind": e.kind,
                "text": e.text,
            })
        })
        .collect();
    Ok(Json(json!({ "entries": entries, "truncated": truncated })))
}

pub(crate) async fn recommend(
    State(app): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let office = parse::<Office>(query.get("office").map_or("", String::as_str))?;
    let ranked = app
        .with_session(move |s| {
            let court = s.court();
            Ok(court
                .recommend(office)?
                .iter()
                .map(|c| {
                    json!({
                        "id": c.agent.id.0,
                        "name": c.agent.name,
                        "office": c.current.map(|o| office_json(o, court.mode())),
                        "record": record_json(&c.record),
                    })
                })
                .collect::<Vec<_>>())
        })
        .await?;
    Ok(Json(json!(ranked)))
}

/// 考成簿（纯文本）。
pub(crate) async fn kaocheng(State(app): State<AppState>) -> Result<Json<Value>, ApiError> {
    let report = app.with_session(|s| Ok(kaocheng_report(s.court()))).await?;
    Ok(Json(json!({ "report": report })))
}

/// 度支奏销（纯文本）。
pub(crate) async fn duzhi(State(app): State<AppState>) -> Result<Json<Value>, ApiError> {
    let backend = app.0.config.backend.clone();
    let report = app
        .with_session(move |s| {
            let default = backend.model_name();
            Ok(duzhi_report(s.court(), default, |m| backend.price(m)))
        })
        .await?;
    Ok(Json(json!({ "report": report })))
}

pub(crate) async fn events_stream(
    State(app): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let receiver = app.0.events.subscribe();
    let stream = stream::unfold(receiver, |mut receiver| async move {
        let event = match receiver.recv().await {
            Ok((name, data)) => Event::default().event(name).data(data),
            // 落后太多时让页面整体刷新。
            Err(broadcast::error::RecvError::Lagged(_)) => {
                Event::default().event("changed").data("{}")
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        };
        Some((Ok(event), receiver))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// 翰林院实录：`?q=` 为检索词，空则全部（新者在前）。
pub(crate) async fn records(
    State(app): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let q = query.get("q").cloned().unwrap_or_default();
    app.with_session(move |s| {
        let records = s.court().records();
        let found: Vec<Value> = ziyuan_core::find_records(&records, &q)
            .into_iter()
            .map(chronicle_json)
            .collect();
        Ok(json!({ "total": records.len(), "records": found }))
    })
    .await
    .map(Json)
}
