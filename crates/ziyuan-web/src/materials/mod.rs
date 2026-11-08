//! 本地资料草稿的 Web 适配：编辑视图、显式保存及只读校验／预览。
//!
//! 只调用应用层资料接口，不经朝廷会话、执行后端或模型。工作区身份只用于会话隔离，
//! 不代替令牌认证。
mod identity;
mod wire;

pub(crate) use identity::workspace_identity;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, State, rejection::BytesRejection};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use ziyuan_app::translation::Translations;
use ziyuan_app::translation::materials::{MaterialError, MaterialIssue};
use ziyuan_storage::{JsonFileError, LockScope, Workspace, WorkspaceError};

use crate::state::AppState;
use wire::{Bound, PreviewBody, SaveBody, ValidateBody};

const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;

/// Deliberately scoped to materials, not the rest of the API.
pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/text/{project}/materials", get(view))
        .route("/text/{project}/materials/draft", post(save))
        .route("/text/{project}/materials/validate", post(validate))
        .route("/text/{project}/materials/preview", post(preview))
        .route("/text/{project}/materials/prepare", post(prepare))
        .route("/text/{project}/materials/diff", get(diff))
        .route(
            "/text/{project}/materials/termbase",
            get(termbase_export).post(termbase_import),
        )
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
}

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
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let (id, request) = match accept::<SaveBody>(&app, &headers, body) {
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
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let (id, request) = match accept::<ValidateBody>(&app, &headers, body) {
        Ok(accepted) => accepted,
        Err(response) => return *response,
    };
    let root = app.0.config.root.clone();
    run(id, move || {
        let mut validation =
            Translations::new(&root).validate_material_input(&project, &request.material)?;
        validation.issues.iter_mut().for_each(sanitize_issue);
        Ok(validation)
    })
    .await
}

pub(crate) async fn preview(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let (id, mut request) = match accept::<PreviewBody>(&app, &headers, body) {
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

pub(crate) async fn diff(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
) -> Response {
    let id = match workspace(&app) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let root = app.0.config.root.clone();
    run(id, move || {
        Translations::new(&root)
            .material_diff(&project)
            .map_err(MaterialError::Visibility)
    })
    .await
}

async fn termbase_export(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
) -> Response {
    let id = match workspace(&app) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let root = app.0.config.root.clone();
    match tokio::task::spawn_blocking(move || {
        Translations::new(&root).export_termbase_csv(&project)
    })
    .await
    {
        Ok(Ok(csv)) => ([(header::CONTENT_TYPE, "text/csv; charset=utf-8")], csv).into_response(),
        Ok(Err(error)) => reply(&id, StatusCode::BAD_REQUEST, error.to_string(), None),
        Err(error) => reply(
            &id,
            StatusCode::INTERNAL_SERVER_ERROR,
            error.to_string(),
            None,
        ),
    }
}

async fn termbase_import(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let id = match workspace(&app) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let body = match body {
        Ok(body) => body,
        Err(_) => {
            return reply(&id, StatusCode::BAD_REQUEST, "术语表读取失败".into(), None);
        }
    };
    let root = app.0.config.root.clone();
    run(id, move || {
        Translations::new(&root)
            .import_termbase_csv(&project, &body)
            .map_err(MaterialError::Visibility)
    })
    .await
}

pub(crate) async fn prepare(
    State(app): State<AppState>,
    UrlPath(project): UrlPath<String>,
) -> Response {
    let id = match workspace(&app) {
        Ok(id) => id,
        Err(response) => return *response,
    };
    let root = app.0.config.root.clone();
    run(id, move || {
        let workspace = Workspace::open(&root).map_err(MaterialError::Workspace)?;
        let mut guard = workspace
            .lock(LockScope::Workspace)
            .map_err(MaterialError::Workspace)?;
        guard
            .upgrade_write_protocol()
            .map_err(MaterialError::Workspace)?;
        let translations = Translations::new(&root);
        translations
            .texts()
            .upgrade_project_protocol(&project, &guard)
            .map_err(MaterialError::Project)?;
        translations
            .prepare_material_production(&guard, &project)
            .map_err(MaterialError::Visibility)
            .map(|bytes| json!({ "prepared": true, "bytes": bytes }))
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
    body: Result<Bytes, BytesRejection>,
) -> Result<(String, T), Box<Response>> {
    let id = workspace(app)?;
    let body = body.map_err(|rejection| {
        let (status, message) = if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            (
                StatusCode::PAYLOAD_TOO_LARGE,
                "资料请求至多 2 MiB；请保留输入并缩减资料",
            )
        } else {
            (StatusCode::BAD_REQUEST, "资料请求正文读取失败，请重试")
        };
        Box::new(reply(&id, status, message.into(), None))
    })?;
    if !is_json(headers) {
        return Err(Box::new(reply(
            &id,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "资料请求须为 application/json".into(),
            None,
        )));
    }
    let request: T = wire::decode(&body)
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

/// Keeps the structured location and code, but replaces the message: application
/// diagnostics quote input values such as an unknown chapter or reference ID.
fn sanitize_issue(issue: &mut MaterialIssue) {
    use ziyuan_app::translation::materials::MaterialIssueCode as C;
    issue.message = match issue.code {
        C::Shape => "资料字段格式不对，请检查类型及格式版本",
        C::Required => "资料缺少必填字段，请补全后重试",
        C::Id => "资料身份不合规或重复，请检查标识",
        C::Reference => "资料引用不存在，请检查关联标识",
        C::Position => "资料章段位置不合规，请检查范围",
        C::Evidence => "资料证据不合规，请核对原文及引用",
        C::Limit => "资料超过编辑限制，请保留输入并缩减资料",
    }
    .into();
}

/// Never expose internal Display diagnostics: they may include paths or stored input.
fn failure(id: &str, error: MaterialError) -> Response {
    use MaterialError as E;
    let (status, message, extra) = match &error {
        E::Issue(issue) => {
            let mut issue = issue.clone();
            sanitize_issue(&mut issue);
            return reply(
                id,
                StatusCode::BAD_REQUEST,
                issue.message.clone(),
                Some(("issue", json!(issue))),
            );
        }
        E::Project(_) => (
            StatusCode::BAD_REQUEST,
            "资料所依据的项目不可用，请检查项目后重试",
            None,
        ),
        E::Visibility(_) => (
            StatusCode::BAD_REQUEST,
            "资料预览的目标块或段落关联不合规，请检查选择及关联",
            None,
        ),
        E::Conflict { current } => (
            StatusCode::CONFLICT,
            "资料草稿修订冲突；请保留输入并重新读取",
            Some(("current", json!(current))),
        ),
        E::RequestConflict => (
            StatusCode::CONFLICT,
            "资料草稿请求身份已被不同内容使用",
            None,
        ),
        E::ProjectChanged => (
            StatusCode::CONFLICT,
            "资料所依据的项目已变化，请重新读取并核对",
            None,
        ),
        E::Storage(JsonFileError::LockTimeout { .. }) => (
            StatusCode::CONFLICT,
            "资料草稿文件正被其他进程使用，请稍后重试",
            None,
        ),
        E::Storage(JsonFileError::TooLarge { .. }) => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "资料草稿文件超过大小上限；请保留输入并检查草稿",
            None,
        ),
        E::Storage(JsonFileError::Newer { .. }) => (
            StatusCode::CONFLICT,
            "资料草稿由更新版本写入，请升级紫垣后再打开",
            None,
        ),
        E::Workspace(
            WorkspaceError::Io(_)
            | WorkspaceError::Unsafe(_)
            | WorkspaceError::Corrupt(_)
            | WorkspaceError::Recovery(_),
        ) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "工作区受管写入状态不可用，请先用命令行检查工作区后重试",
            None,
        ),
        E::Workspace(_) => (
            StatusCode::CONFLICT,
            "工作区受管写入尚不可用，请先用命令行检查工作区后重试",
            None,
        ),
        E::Storage(_) | E::Io(_) | E::Path(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "资料草稿读写失败或文件已损坏，请检查草稿后重试",
            None,
        ),
    };
    reply(id, status, message.into(), extra)
}

#[cfg(test)]
mod tests;
