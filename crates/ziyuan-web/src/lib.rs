//! 奉天门：紫垣的 Web 界面（与终端界面同属紫禁城一层）。
//!
//! 页面是内嵌的单个 HTML 文件；数据经 JSON API 读写，执行中的流式输出与结局经 SSE 推送。
//! Web 界面同样代表皇帝，因此每个 API 请求都必须带令牌：浏览器经 `?token=` 或登录框换取
//! HttpOnly、SameSite=Strict 的 Cookie，脚本可用 `Authorization: Bearer`。令牌存于
//! `.ziyuan/web-token`，重启后不变，可重新生成。
//!
//! 请求共用一个缓存的 [`Session`](ziyuan_app::Session)（每次只增量读取新事件），同时有多个请求时另开会话；
//! 与 CLI 共用同一套多进程规则：写入以乐观并发提交，执行诏令前取得执行者锁。
mod auth;
mod edicts;
mod json;
mod personnel;
mod run;
mod state;
mod subscriptions;
mod text;
mod timer;
mod views;

pub use auth::{load_or_create_token, load_or_create_viewer_token, random_token};
pub use json::{chronicle_json, court_json, mark_stale};

use axum::Router;
use axum::middleware::{self};
use axum::routing::{get, post};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use ziyuan_runtime::{Limits, WorkerBackend};

use crate::auth::{index, login, require_token};
use crate::edicts::{
    acquit, apply_changes, approve, cancel, censor, changelog, courier, debate, discard_changes,
    draft, github_pr, github_pull, github_reply, issue, recheck, redraft, remove_template, review,
    save_template, split, urgent,
};
use crate::personnel::{
    abolish, appoint, budget, cane, crown, dismiss, exam, nominate, office_ceiling, office_model,
    palace, pardon, purge, recruit, reform, rename, retreat,
};
use crate::run::{run, stop_run};
use crate::state::{AppState, Inner};
use crate::views::{
    changes_delta, duzhi, events_stream, history, kaocheng, recommend, records, state,
};
const INDEX_HTML: &str = include_str!("index.html");

/// 随界面内置的字体（JetBrains Mono，SIL OFL 1.1，许可证见 `fonts/JetBrainsMono-OFL.txt`）。
/// 中文与日文字体不内置，见 docs/guide.md。
const FONTS: &[(&str, &[u8])] = &[
    (
        "JetBrainsMono-Regular.woff2",
        include_bytes!("fonts/JetBrainsMono-Regular.woff2"),
    ),
    (
        "JetBrainsMono-Bold.woff2",
        include_bytes!("fonts/JetBrainsMono-Bold.woff2"),
    ),
    (
        "JetBrainsMono-OFL.txt",
        include_bytes!("fonts/JetBrainsMono-OFL.txt"),
    ),
];

async fn font(axum::extract::Path(name): axum::extract::Path<String>) -> axum::response::Response {
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;
    match FONTS.iter().find(|(n, _)| *n == name) {
        Some((n, bytes)) => {
            let kind = if n.ends_with(".woff2") {
                "font/woff2"
            } else {
                "text/plain; charset=utf-8"
            };
            (
                [
                    (header::CONTENT_TYPE, kind),
                    (header::CACHE_CONTROL, "public, max-age=604800"),
                ],
                *bytes,
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub struct Config {
    /// 朝廷所在目录（含 `.ziyuan/`），也是玉玺查阅范围的根。
    pub root: PathBuf,
    /// 皇帝令牌（见 [`load_or_create_token`]）。
    pub token: String,
    /// 观政令牌（见 [`load_or_create_viewer_token`]）：只能查看朝廷、起居注与实录；`None` 为不启用。
    pub viewer_token: Option<String>,
    pub backend: Arc<dyn WorkerBackend>,
    pub limits: Limits,
    /// 兵部驿站 · GitHub（`ZIYUAN_GITHUB_REPO` 未设时为 `None`）；凭据只在其中。
    pub github: Option<Arc<ziyuan_github::GitHub>>,
}

/// 在 `listener` 上提供 Web 界面，直到出错。
pub async fn serve(listener: TcpListener, config: Config) -> io::Result<()> {
    let service = subscriptions::Service::new(config.root.clone());
    serve_with_subscriptions(listener, config, service).await
}

async fn serve_with_subscriptions(
    listener: TcpListener,
    config: Config,
    service: subscriptions::Service,
) -> io::Result<()> {
    let app = app_state(config, service);
    let _scheduler = timer::Scheduler::start(app.clone());
    axum::serve(listener, router_for(app)).await
}

pub fn router(config: Config) -> Router {
    let service = subscriptions::Service::new(config.root.clone());
    router_for(app_state(config, service))
}

fn app_state(config: Config, subscriptions: subscriptions::Service) -> AppState {
    let (events, _) = broadcast::channel(1024);
    AppState(Arc::new(Inner {
        config,
        subscriptions,
        events,
        run: Mutex::new(None),
        cache: Mutex::new(None),
    }))
}

#[cfg(test)]
fn router_with_subscriptions(config: Config, subscriptions: subscriptions::Service) -> Router {
    router_for(app_state(config, subscriptions))
}

fn router_for(app: AppState) -> Router {
    let api = Router::new()
        .route(
            "/subscriptions",
            get(crate::subscriptions::list).post(crate::subscriptions::add),
        )
        .route("/subscriptions/updates", get(crate::subscriptions::updates))
        .route("/subscriptions/remove", post(crate::subscriptions::remove))
        .route("/subscriptions/check", post(crate::subscriptions::check))
        .route("/subscriptions/read", post(crate::subscriptions::read))
        .route(
            "/subscriptions/settings",
            post(crate::subscriptions::settings),
        )
        .route("/state", get(state))
        .route("/history", get(history))
        .route("/recommend", get(recommend))
        .route("/kaocheng", get(kaocheng))
        .route("/duzhi", get(duzhi))
        .route("/budget", post(budget))
        .route("/events", get(events_stream))
        .route("/edicts", post(issue))
        .route("/courier", post(courier))
        .route("/templates", post(save_template))
        .route("/templates/remove", post(remove_template))
        .route("/tasks/{id}/urgent", post(urgent))
        .route("/run", post(run))
        .route("/run/stop", post(stop_run))
        .route("/tasks/{id}/cancel", post(cancel))
        .route("/tasks/{id}/review", post(review))
        .route("/tasks/{id}/approve", post(approve))
        .route("/tasks/{id}/draft", post(draft))
        .route("/tasks/{id}/redraft", post(redraft))
        .route("/tasks/{id}/censor", post(censor))
        .route("/tasks/{id}/recheck", post(recheck))
        .route("/tasks/{id}/acquit", post(acquit))
        .route("/records", get(records))
        .route("/changelog", post(changelog))
        .route("/github/pull", post(github_pull))
        .route("/tasks/{id}/github/reply", post(github_reply))
        .route("/tasks/{id}/github/pr", post(github_pr))
        .route("/tasks/{id}/split", post(split))
        .route("/tasks/{id}/debate", post(debate))
        .route("/tasks/{id}/changes/delta", get(changes_delta))
        .route("/tasks/{id}/changes/apply", post(apply_changes))
        .route("/tasks/{id}/changes/discard", post(discard_changes))
        .route("/reform", post(reform))
        .route("/retreat", post(retreat))
        .route("/offices/model", post(office_model))
        .route("/offices/ceiling", post(office_ceiling))
        .route("/abolish", post(abolish))
        .route("/officials", post(recruit))
        .route("/officials/{id}/appoint", post(appoint))
        .route("/officials/{id}/dismiss", post(dismiss))
        .route("/officials/{id}/purge", post(purge))
        .route("/officials/{id}/exam", post(exam))
        .route("/officials/{id}/crown", post(crown))
        .route("/officials/{id}/cane", post(cane))
        .route("/officials/{id}/rename", post(rename))
        .route("/officials/{id}/pardon", post(pardon))
        .route("/nominate", post(nominate))
        .route("/palace", post(palace))
        .route("/text", get(text::list))
        .route("/text/{project}", get(text::project))
        .route("/text/{project}/blocks/{block}", get(text::block))
        .route(
            "/text/{project}/context/approve",
            post(text::approve_context),
        )
        .route("/text/{project}/blocks/{block}/zhupi", post(text::zhupi))
        .route(
            "/text/{project}/blocks/{block}/finalize",
            post(text::finalize),
        )
        .route("/text/{project}/blocks/{block}/reopen", post(text::reopen))
        .route("/text/{project}/blocks/{block}/issue", post(text::issue))
        .route("/notes", get(text::notes_list).post(text::note_add))
        .route("/notes/{id}/edit", post(text::note_edit))
        .route("/notes/{id}/remove", post(text::note_remove))
        .route("/notes/{id}/promote", post(text::note_promote))
        .route_layer(middleware::from_fn_with_state(app.clone(), require_token));
    Router::new()
        .route("/", get(index))
        .route("/login", post(login))
        .route("/fonts/{name}", get(font))
        .nest("/api", api)
        .with_state(app)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod text_tests;
