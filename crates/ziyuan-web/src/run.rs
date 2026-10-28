//! 执行与常朝：开始、停止执行，转发进度到 SSE。

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::{Value, json};
use ziyuan_app::{EXECUTOR_BUSY, RunUpdate, Session, WatchOptions, WatchUpdate};
use ziyuan_runtime::{CancelToken, Scheduler};
use ziyuan_storage::EventLog;

use crate::json::report_json;
use crate::state::{ApiError, AppState};

/// 在后台执行全部未结束的诏令；执行期间新入队的诏令在本轮结束后接着执行。
/// 执行待办诏令：`{ watch?: bool, censor?: bool }`。`watch` 为真即开常朝：常驻执行，随到随办，
/// 办完（`censor` 不为假时）自动请都察院复核，直到 `POST /run/stop`。
pub(crate) async fn run(
    State(app): State<AppState>,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, ApiError> {
    let body = body.map(|b| b.0).unwrap_or_default();
    let watch = body["watch"].as_bool().unwrap_or(false);
    let censor = body["censor"].as_bool().unwrap_or(true);
    {
        let mut run = app.0.run.lock().expect("锁未中毒");
        if run.is_some() {
            return Err(ApiError(StatusCode::CONFLICT, "已在执行中".into()));
        }
        let lock = EventLog::new(&app.0.config.root)
            .try_lock_executor()
            .map_err(|e| ApiError::internal(e.to_string()))?
            .ok_or_else(|| ApiError(StatusCode::CONFLICT, EXECUTOR_BUSY.into()))?;
        let cancel = CancelToken::new();
        *run = Some((cancel.clone(), watch));
        let worker = app.clone();
        let token = cancel.clone();
        tokio::spawn(async move {
            let _lock = lock;
            let result = if watch {
                watch_open_tasks(&worker, &token, censor).await
            } else {
                run_open_tasks(&worker, &token).await
            };
            if let Err(error) = result {
                worker.emit("run_error", json!({ "error": error.1 }));
            }
            *worker.0.run.lock().expect("锁未中毒") = None;
            worker.emit("run", json!({ "running": false }));
            worker.emit("changed", json!({}));
        });
    }
    app.emit("run", json!({ "running": true, "watching": watch }));
    Ok(Json(json!({ "ok": true })))
}

pub(crate) async fn run_open_tasks(app: &AppState, cancel: &CancelToken) -> Result<(), ApiError> {
    let scheduler = Scheduler::new(app.0.config.backend.clone(), app.0.config.limits);
    let root = app.0.config.root.clone();
    let mut session = tokio::task::spawn_blocking(move || Session::open(EventLog::new(&root)))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))??;
    session
        .run_open(&scheduler, cancel, |update| forward(app, update))
        .await?;
    Ok(())
}

pub(crate) fn forward(app: &AppState, update: RunUpdate<'_>) {
    match update {
        RunUpdate::Assigned(_) => app.emit("changed", json!({})),
        RunUpdate::Text { task, text } => {
            app.emit("text", json!({ "task": task.0, "text": text }));
        }
        RunUpdate::Finished(report) => {
            app.emit("finished", report_json(report));
            app.emit("changed", json!({}));
        }
    }
}

/// 常朝：同 CLI `run --watch`，每 5 秒查看一次队列；提示经 `notice` 事件推给页面。
pub(crate) async fn watch_open_tasks(
    app: &AppState,
    cancel: &CancelToken,
    censor: bool,
) -> Result<(), ApiError> {
    let backend = app.0.config.backend.clone();
    let scheduler = Scheduler::new(backend.clone(), app.0.config.limits);
    let root = app.0.config.root.clone();
    let options = WatchOptions {
        poll: std::time::Duration::from_secs(5),
        censor: censor.then(|| root.clone()),
    };
    let mut session = tokio::task::spawn_blocking(move || Session::open(EventLog::new(&root)))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))??;
    session
        .watch(&scheduler, backend.as_ref(), cancel, &options, |update| {
            let notice = match update {
                WatchUpdate::Run(update) => return forward(app, update),
                WatchUpdate::Censured { task, verdict } => {
                    app.emit("changed", json!({}));
                    format!("常朝：诏令 #{} 都察院{}", task.0, verdict.label())
                }
                WatchUpdate::Notice(note) => format!("常朝：{note}"),
                WatchUpdate::Neglect(note) => format!("史官记：{note}"),
            };
            app.emit("notice", json!({ "text": notice }));
        })
        .await?;
    Ok(())
}

pub(crate) async fn stop_run(State(app): State<AppState>) -> Json<Value> {
    let stopped = match app.0.run.lock().expect("锁未中毒").as_ref() {
        Some((cancel, _)) => {
            cancel.cancel();
            true
        }
        None => false,
    };
    Json(json!({ "stopped": stopped }))
}
