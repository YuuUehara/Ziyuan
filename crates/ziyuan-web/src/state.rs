//! 共享状态：缓存的会话、执行中的诏令与 SSE 通道，以及 API 错误与参数解析。

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use ziyuan_app::{AppError, Session};
use ziyuan_runtime::CancelToken;
use ziyuan_storage::{EventLog, StorageError};

use crate::Config;

#[derive(Clone)]
pub(crate) struct AppState(pub(crate) Arc<Inner>);

pub(crate) struct Inner {
    pub(crate) config: Config,
    pub(crate) subscriptions: crate::subscriptions::Service,
    /// (SSE 事件名, JSON 数据)。
    pub(crate) events: broadcast::Sender<(&'static str, String)>,
    /// 本进程正在进行的执行及其是否为常朝；`Some` 时拒绝再次启动。
    pub(crate) run: Mutex<Option<(CancelToken, bool)>>,
    /// 空闲的会话，下一个请求取走后增量读取新事件，用完放回。
    pub(crate) cache: Mutex<Option<Session<EventLog>>>,
}

#[derive(Debug)]
pub(crate) struct ApiError(pub(crate) StatusCode, pub(crate) String);

impl ApiError {
    pub(crate) fn bad(message: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, message.into())
    }
    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, message.into())
    }
}

impl From<AppError> for ApiError {
    fn from(error: AppError) -> Self {
        let status = match &error {
            AppError::Governance(_) => StatusCode::BAD_REQUEST,
            AppError::NotFounded | AppError::AlreadyFounded | AppError::Apply(_) => {
                StatusCode::CONFLICT
            }
            AppError::Storage(StorageError::Conflict { .. }) => StatusCode::CONFLICT,
            AppError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::External(_) => StatusCode::BAD_GATEWAY,
        };
        Self(status, error.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

/// 去掉空白项的字符串数组。
pub(crate) fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_str().map(str::trim))
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(crate) fn str_field<'a>(body: &'a Value, key: &str) -> Result<&'a str, ApiError> {
    body.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad(format!("缺少字段 `{key}`")))
}

pub(crate) fn parse<T: std::str::FromStr>(input: &str) -> Result<T, ApiError>
where
    T::Err: std::fmt::Display,
{
    input
        .parse()
        .map_err(|e: T::Err| ApiError::bad(e.to_string()))
}

impl AppState {
    pub(crate) fn emit(&self, event: &'static str, data: Value) {
        let _ = self.0.events.send((event, data.to_string()));
    }

    pub(crate) fn running(&self) -> bool {
        self.0.run.lock().expect("锁未中毒").is_some()
    }

    pub(crate) fn watching(&self) -> bool {
        self.0
            .run
            .lock()
            .expect("锁未中毒")
            .as_ref()
            .is_some_and(|r| r.1)
    }

    pub(crate) fn github(&self) -> Result<Arc<ziyuan_github::GitHub>, ApiError> {
        self.0.config.github.clone().ok_or_else(|| {
            ApiError::bad(
                "未设 ZIYUAN_GITHUB_REPO（owner/repo）与 ZIYUAN_GITHUB_TOKEN，驿站不通 GitHub",
            )
        })
    }

    /// 在阻塞线程中取一个会话（缓存的增量刷新，没有则打开）执行 `f`，用完放回缓存
    /// （事件簿读写持文件锁，会阻塞）。
    pub(crate) async fn with_session<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Session<EventLog>) -> Result<T, AppError> + Send + 'static,
    ) -> Result<T, ApiError> {
        let root = self.0.config.root.clone();
        let cached = self.0.cache.lock().expect("锁未中毒").take();
        let (session, result) = tokio::task::spawn_blocking(move || {
            let refreshed = cached.map(|mut s| s.reload().map(|()| s));
            let mut session = match refreshed {
                Some(Ok(session)) => session,
                // 刷新失败（如事件簿被替换）则重新打开。
                _ => Session::open(EventLog::new(&root))?,
            };
            let result = f(&mut session);
            if let Some(note) = session.take_chronicle() {
                eprintln!("{note}");
            }
            Ok::<_, AppError>((session, result))
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))??;
        self.0
            .cache
            .lock()
            .expect("锁未中毒")
            .get_or_insert(session);
        result.map_err(ApiError::from)
    }

    /// 执行变更，成功后通知页面刷新。
    pub(crate) async fn mutate(
        &self,
        f: impl FnOnce(&mut Session<EventLog>) -> Result<Value, AppError> + Send + 'static,
    ) -> Result<Json<Value>, ApiError> {
        let result = self.with_session(f).await?;
        self.emit("changed", json!({}));
        Ok(Json(result))
    }
}
