//! Independent metadata-only API. All disk locks and source requests run off-runtime.
#[cfg(test)]
#[path = "subscriptions_tests.rs"]
mod tests;
use crate::state::{ApiError, AppState};
use axum::Json;
use axum::extract::{Query, State};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use ziyuan_app::subscriptions::{
    CheckMode, CheckStatus, CheckSummary, SourceError, Subscription, SubscriptionError,
    Subscriptions, SyosetuSource, WorkSource,
};
use ziyuan_app::{AppError, FollowCourier};

#[derive(Clone)]
pub(crate) struct Service {
    root: PathBuf,
    source: Arc<dyn WorkSource + Send + Sync>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}
// Keep the app API's Sized source contract without exposing configurable URLs.
struct SharedSource(Arc<dyn WorkSource + Send + Sync>);
impl WorkSource for SharedSource {
    fn fetch(
        &self,
        codes: &[String],
    ) -> Result<Vec<ziyuan_app::subscriptions::ObservedWork>, SourceError> {
        self.0.fetch(codes)
    }
}
impl Service {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            source: Arc::new(SyosetuSource::new()),
            clock: Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            }),
        }
    }
    async fn store<T: Send + 'static>(
        &self,
        f: impl FnOnce(Subscriptions) -> Result<T, SubscriptionError> + Send + 'static,
    ) -> Result<T, ApiError> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || f(Subscriptions::new(root)))
            .await
            .map_err(|_| ApiError::internal("作品关注操作未完成，请重试"))?
            .map_err(|e| match e {
                SubscriptionError::InvalidInput => {
                    ApiError::bad("作品代码或设置无效；检查间隔至少 15 分钟")
                }
                _ => ApiError::internal("作品关注状态无法读取或保存，请检查本地文件；未重置状态"),
            })
    }
    pub(crate) async fn check(&self, mode: CheckMode) -> Result<CheckSummary, ApiError> {
        let source = SharedSource(self.source.clone());
        let now = (self.clock)();
        self.store(move |s| s.check(&source, now, mode)).await
    }
}

/// 检查之后：开启「新章节自动入队」时经缓存的会话入兵部驿站（不在联网期间持会话）。
/// 出错只写在报告里；有新入队的诏令时通知页面刷新。
pub(crate) async fn courier(app: &AppState) -> FollowCourier {
    let failed = |e: ApiError| FollowCourier::from_result(Err(AppError::External(e.1)));
    match app.0.subscriptions.store(|s| s.settings()).await {
        Ok(settings) if !settings.queue_new_chapters => return FollowCourier::disabled(),
        Ok(_) => {}
        Err(e) => return failed(e),
    }
    let root = app.0.subscriptions.root.clone();
    let report = match app
        .session_result(move |session| session.follow_intake(&Subscriptions::new(root)))
        .await
    {
        Ok(result) => FollowCourier::from_result(result),
        Err(e) => failed(e),
    };
    if !report.queued.is_empty() {
        app.emit("changed", json!({}));
    }
    report
}

/// 定时器每一拍：到期的作品检查完后再入队。没有检查任何作品时不打开朝廷。
pub(crate) async fn scheduled_tick(
    app: &AppState,
) -> Result<(CheckSummary, FollowCourier), ApiError> {
    let summary = app.0.subscriptions.check(CheckMode::Scheduled).await?;
    let courier = if summary.status == CheckStatus::Complete {
        courier(app).await
    } else {
        FollowCourier::disabled()
    };
    Ok((summary, courier))
}

fn source_error(error: &SourceError) -> &'static str {
    match error {
        SourceError::NotFound => {
            "官方接口未找到作品（可能为 R18、搜索排除或已不可用）；保留上次记录"
        }
        SourceError::Timeout => "官方接口请求超时，请稍后重试",
        SourceError::RateLimited => "官方接口限流，稍后自动重试",
        SourceError::Unavailable => "官方接口暂时无法连接，未能判断是否更新",
        SourceError::Malformed => "官方接口返回的信息不完整，保留上次记录",
        SourceError::Suspicious => "官方信息出现异常倒退，保留上次可信记录",
    }
}
fn item_view(item: Subscription) -> Value {
    json!({
        "id": item.id, "code": item.code, "added_at": item.added_at,
        "title": item.baseline.as_ref().map(|w| &w.title),
        "author": item.baseline.as_ref().map(|w| &w.author),
        "chapter_count": item.baseline.as_ref().map(|w| w.chapter_count),
        "last_published": item.baseline.as_ref().map(|w| w.last_published),
        "last_attempt": item.last_attempt, "next_check": item.next_check,
        "last_error": item.last_error.as_ref().map(source_error),
        "first_check": item.baseline.is_none()
    })
}
fn fields<'a>(
    body: &'a Value,
    allowed: &[&str],
) -> Result<&'a serde_json::Map<String, Value>, ApiError> {
    let object = body
        .as_object()
        .ok_or_else(|| ApiError::bad("请求须为 JSON 对象"))?;
    if object.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(ApiError::bad("请求包含未知字段"));
    }
    Ok(object)
}
fn string(body: &Value, key: &str) -> Result<String, ApiError> {
    body.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ApiError::bad(format!("字段 {key} 须为非空文本")))
}
pub(crate) async fn list(State(app): State<AppState>) -> Result<Json<Value>, ApiError> {
    app.0
        .subscriptions
        .store(|s| {
            Ok(json!({
                "items": s.list()?.into_iter().map(item_view).collect::<Vec<_>>(),
                "settings": s.settings()?
            }))
        })
        .await
        .map(Json)
}
pub(crate) async fn updates(
    State(app): State<AppState>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let all = match query.get("all").map(String::as_str) {
        None | Some("false" | "0") => false,
        Some("true" | "1") => true,
        _ => return Err(ApiError::bad("all 须为 true 或 false")),
    };
    app.0
        .subscriptions
        .store(move |s| Ok(json!({"updates":s.updates(!all)?})))
        .await
        .map(Json)
}
pub(crate) async fn add(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    fields(&body, &["input"])?;
    let input = string(&body, "input")?;
    let now = (app.0.subscriptions.clock)();
    app.0
        .subscriptions
        .store(move |s| Ok(json!({"item":item_view(s.add(&input, now)?)})))
        .await
        .map(Json)
}
pub(crate) async fn remove(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let object = fields(&body, &["input", "code"])?;
    if object.len() != 1 {
        return Err(ApiError::bad("请提供 input 或 code 之一"));
    }
    let input = string(
        &body,
        if object.contains_key("input") {
            "input"
        } else {
            "code"
        },
    )?;
    app.0
        .subscriptions
        .store(move |s| Ok(json!({"found":s.remove(&input)?})))
        .await
        .map(Json)
}
pub(crate) async fn check(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    fields(&body, &[])?;
    let mut value = json!(app.0.subscriptions.check(CheckMode::Manual).await?);
    value["courier"] = json!(courier(&app).await);
    Ok(Json(value))
}
pub(crate) async fn read(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    fields(&body, &["id"])?;
    let id = string(&body, "id")?;
    app.0
        .subscriptions
        .store(move |s| Ok(json!({"found":s.read(&id)?})))
        .await
        .map(Json)
}
pub(crate) async fn settings(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    fields(
        &body,
        &["enabled", "interval_seconds", "queue_new_chapters"],
    )?;
    let enabled = body["enabled"]
        .as_bool()
        .ok_or_else(|| ApiError::bad("enabled 须为布尔值"))?;
    let interval_seconds = body["interval_seconds"]
        .as_u64()
        .ok_or_else(|| ApiError::bad("interval_seconds 须为正整数"))?;
    // 未给出时保留原值，旧页面保存检查设置不会关掉入队。
    let queue = match body.get("queue_new_chapters") {
        None => None,
        Some(v) => Some(
            v.as_bool()
                .ok_or_else(|| ApiError::bad("queue_new_chapters 须为布尔值"))?,
        ),
    };
    app.0
        .subscriptions
        .store(move |s| {
            let settings = s.update_settings(|settings| {
                settings.enabled = enabled;
                settings.interval_seconds = interval_seconds;
                if let Some(queue) = queue {
                    settings.queue_new_chapters = queue;
                }
            })?;
            Ok(json!({"settings":settings}))
        })
        .await
        .map(Json)
}
