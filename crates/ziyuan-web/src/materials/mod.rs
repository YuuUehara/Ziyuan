//! 本地资料草稿的 Web 适配：编辑视图、显式保存及只读校验／预览。
//!
//! 只调用应用层资料接口，不经朝廷会话、执行后端或模型。工作区身份只用于会话隔离，
//! 不代替令牌认证。
mod identity;
mod wire;

pub(crate) use identity::workspace_identity;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use ziyuan_app::translation::Translations;
use ziyuan_app::translation::materials::MaterialError;
use ziyuan_storage::{JsonFileError, WorkspaceError};

use crate::state::AppState;
use wire::{Bound, PreviewBody, SaveBody, ValidateBody};

#[derive(Serialize)]
struct WithWorkspace<'a, T> {
    workspace_id: &'a str,
    #[serde(flatten)]
    value: T,
}

pub(crate) async fn view(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
) -> Response {
    let id = match workspace(&app) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let root = app.0.config.root.clone();
    run(id, move || Translations::new(&root).material_view(&project)).await
}

pub(crate) async fn save(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (id, request) = match accept::<SaveBody>(&app, &headers, &body) {
        Ok(accepted) => accepted,
        Err(response) => return *response,
    };
    let root = app.0.config.root.clone();
    run(id, move || {
        Translations::new(&root).save_material_draft(&project, &request.into_save())
    })
    .await
}

pub(crate) async fn validate(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (id, request) = match accept::<ValidateBody>(&app, &headers, &body) {
        Ok(accepted) => accepted,
        Err(response) => return *response,
    };
    let root = app.0.config.root.clone();
    run(id, move || {
        Translations::new(&root).validate_material_input(&project, &request.material)
    })
    .await
}

pub(crate) async fn preview(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (id, mut request) = match accept::<PreviewBody>(&app, &headers, &body) {
        Ok(accepted) => accepted,
        Err(response) => return *response,
    };
    let root = app.0.config.root.clone();
    run(id, move || {
        let associations = request.associations();
        Translations::new(&root).preview_material(
            &project,
            &request.material,
            &request.block,
            &associations,
        )
    })
    .await
}

/// Rejections are boxed: `Response` is large enough to trip `clippy::result_large_err`.
fn workspace(app: &AppState) -> Result<String, Box<Response>> {
    app.0.workspace.clone().map_err(|message| {
        Box::new(
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": message })),
            )
                .into_response(),
        )
    })
}

/// The workspace binding is checked before any application call, so a `null` revision
/// from another workspace's page can never create a draft here.
fn accept<T: DeserializeOwned + Bound>(
    app: &AppState,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(String, T), Box<Response>> {
    let id = workspace(app)?;
    if !is_json(headers) {
        return Err(Box::new(reply(
            &id,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "资料请求须为 application/json".into(),
            None,
        )));
    }
    let request: T = wire::decode(body)
        .map_err(|message| Box::new(reply(&id, StatusCode::BAD_REQUEST, message, None)))?;
    if request.workspace_id() != id {
        return Err(Box::new(reply(
            &id,
            StatusCode::CONFLICT,
            "资料请求来自其他工作区或旧页面，请重新读取本工作区资料；当前输入请自行复制保留".into(),
            None,
        )));
    }
    Ok((id, request))
}

fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_ascii_lowercase())
        .is_some_and(|m| {
            m == "application/json" || (m.starts_with("application/") && m.ends_with("+json"))
        })
}

async fn run<T: Serialize + Send + 'static>(
    id: String,
    f: impl FnOnce() -> Result<T, MaterialError> + Send + 'static,
) -> Response {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(value)) => Json(WithWorkspace {
            workspace_id: &id,
            value,
        })
        .into_response(),
        Ok(Err(error)) => failure(&id, error),
        Err(_) => reply(
            &id,
            StatusCode::INTERNAL_SERVER_ERROR,
            "资料请求处理中断，请重试".into(),
            None,
        ),
    }
}

fn reply(id: &str, status: StatusCode, error: String, extra: Option<(&str, Value)>) -> Response {
    let mut body = json!({ "error": error, "workspace_id": id });
    if let Some((key, value)) = extra {
        body[key] = value;
    }
    (status, Json(body)).into_response()
}

/// Lock and workspace-safety diagnostics can name absolute paths, so those get fixed text.
fn failure(id: &str, error: MaterialError) -> Response {
    use MaterialError as E;
    let message = error.to_string();
    let (status, extra) = match &error {
        E::Issue(issue) => (StatusCode::BAD_REQUEST, Some(("issue", json!(issue)))),
        E::Project(_) | E::Visibility(_) => (StatusCode::BAD_REQUEST, None),
        E::Conflict { current } => (StatusCode::CONFLICT, Some(("current", json!(current)))),
        E::RequestConflict | E::ProjectChanged => (StatusCode::CONFLICT, None),
        E::Storage(JsonFileError::LockTimeout { .. }) => {
            return reply(
                id,
                StatusCode::CONFLICT,
                "资料草稿文件正被其他进程使用，请稍后重试".into(),
                None,
            );
        }
        E::Storage(JsonFileError::TooLarge { .. }) => (StatusCode::CONFLICT, None),
        E::Workspace(
            WorkspaceError::Io(_) | WorkspaceError::Unsafe(_) | WorkspaceError::Corrupt(_),
        ) => {
            return reply(
                id,
                StatusCode::INTERNAL_SERVER_ERROR,
                "工作区受管写入状态不可用，请先用命令行检查工作区后重试".into(),
                None,
            );
        }
        E::Workspace(_) => (StatusCode::CONFLICT, None),
        E::Storage(_) | E::Io(_) | E::Path(_) => (StatusCode::INTERNAL_SERVER_ERROR, None),
    };
    reply(id, status, message, extra)
}

#[cfg(test)]
mod tests;
