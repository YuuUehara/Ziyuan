//! 紫禁城（Forbidden City）：紫垣的 Web 界面。
//!
//! 页面是内嵌的单个 HTML 文件；数据经 JSON API 读写，执行中的流式输出与结局经 SSE 推送。
//! Web 界面同样代表皇帝，因此每个 API 请求都必须携带启动时生成的令牌。
//!
//! 每个请求各自打开一个 [`Session`]，与 CLI 共用同一套多进程规则：写入以乐观并发提交，
//! 执行诏令前取得执行者锁。
use axum::extract::{Path as UrlPath, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::{self, Stream};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::convert::Infallible;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use ziyuan_app::{AppError, RunUpdate, Session, TaskReport, describe_event};
use ziyuan_core::{
    AgentId, Capability, Department, GovernanceMode, Office, Seal, TaskId, TokenUsage, Verdict,
};
use ziyuan_governance::{Court, ServiceRecord, TaskStatus};
use ziyuan_runtime::{CancelToken, Limits, Outcome, Scheduler, WorkerBackend};
use ziyuan_storage::{EventLog, StorageError};

const INDEX_HTML: &str = include_str!("index.html");

pub struct Config {
    /// 朝廷所在目录（含 `.ziyuan/`），也是玉玺查阅范围的根。
    pub root: PathBuf,
    /// API 令牌；页面 URL 以 `?token=` 携带。
    pub token: String,
    pub backend: Arc<dyn WorkerBackend>,
    pub limits: Limits,
}

/// 生成 32 位十六进制随机令牌。
pub fn random_token() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// 在 `listener` 上提供 Web 界面，直到出错。
pub async fn serve(listener: TcpListener, config: Config) -> io::Result<()> {
    axum::serve(listener, router(config)).await
}

pub fn router(config: Config) -> Router {
    let (events, _) = broadcast::channel(1024);
    let app = AppState(Arc::new(Inner {
        config,
        events,
        run: Mutex::new(None),
    }));
    let api = Router::new()
        .route("/state", get(state))
        .route("/history", get(history))
        .route("/recommend", get(recommend))
        .route("/events", get(events_stream))
        .route("/edicts", post(issue))
        .route("/run", post(run))
        .route("/run/stop", post(stop_run))
        .route("/tasks/{id}/cancel", post(cancel))
        .route("/tasks/{id}/review", post(review))
        .route("/reform", post(reform))
        .route("/officials", post(recruit))
        .route("/officials/{id}/appoint", post(appoint))
        .route("/officials/{id}/dismiss", post(dismiss))
        .route_layer(middleware::from_fn_with_state(app.clone(), require_token));
    Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .nest("/api", api)
        .with_state(app)
}

#[derive(Clone)]
struct AppState(Arc<Inner>);

struct Inner {
    config: Config,
    /// (SSE 事件名, JSON 数据)。
    events: broadcast::Sender<(&'static str, String)>,
    /// 本进程正在进行的执行；`Some` 时拒绝再次启动。
    run: Mutex<Option<CancelToken>>,
}

impl AppState {
    fn emit(&self, event: &'static str, data: Value) {
        let _ = self.0.events.send((event, data.to_string()));
    }

    fn running(&self) -> bool {
        self.0.run.lock().expect("锁未中毒").is_some()
    }

    /// 在阻塞线程中打开会话并执行 `f`（事件簿读写持文件锁，会阻塞）。
    async fn with_session<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Session<EventLog>) -> Result<T, AppError> + Send + 'static,
    ) -> Result<T, ApiError> {
        let root = self.0.config.root.clone();
        tokio::task::spawn_blocking(move || {
            let mut session = Session::open(EventLog::new(&root))?;
            f(&mut session)
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(ApiError::from)
    }

    /// 执行变更，成功后通知页面刷新。
    async fn mutate(
        &self,
        f: impl FnOnce(&mut Session<EventLog>) -> Result<Value, AppError> + Send + 'static,
    ) -> Result<Json<Value>, ApiError> {
        let result = self.with_session(f).await?;
        self.emit("changed", json!({}));
        Ok(Json(result))
    }
}

// ---- 错误与鉴权 ----

struct ApiError(StatusCode, String);

impl ApiError {
    fn bad(message: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, message.into())
    }
    fn internal(message: impl Into<String>) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, message.into())
    }
}

impl From<AppError> for ApiError {
    fn from(error: AppError) -> Self {
        let status = match &error {
            AppError::Governance(_) => StatusCode::BAD_REQUEST,
            AppError::NotFounded | AppError::AlreadyFounded => StatusCode::CONFLICT,
            AppError::Storage(StorageError::Conflict { .. }) => StatusCode::CONFLICT,
            AppError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self(status, error.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

/// `Authorization: Bearer <令牌>`，或 `?token=<令牌>`（EventSource 无法设置请求头）。
async fn require_token(State(app): State<AppState>, request: Request, next: Next) -> Response {
    let expected = app.0.config.token.as_bytes();
    let header_token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let query_token = request
        .uri()
        .query()
        .and_then(|q| q.split('&').find_map(|pair| pair.strip_prefix("token=")));
    let ok = [header_token, query_token]
        .into_iter()
        .flatten()
        .any(|t| constant_time_eq(t.as_bytes(), expected));
    if ok {
        next.run(request).await
    } else {
        ApiError(StatusCode::UNAUTHORIZED, "缺少或错误的访问令牌".into()).into_response()
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---- 读取 ----

async fn state(State(app): State<AppState>) -> Result<Json<Value>, ApiError> {
    let running = app.running();
    let mut value = app.with_session(|s| Ok(court_json(s.court()))).await?;
    value["running"] = json!(running);
    value["model"] = json!(app.0.config.backend.model_name());
    Ok(Json(value))
}

async fn history(State(app): State<AppState>) -> Result<Json<Value>, ApiError> {
    let lines = app
        .with_session(|s| Ok(s.history().iter().map(describe_event).collect::<Vec<_>>()))
        .await?;
    Ok(Json(json!(lines)))
}

async fn recommend(
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

async fn events_stream(
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

// ---- 变更 ----

async fn issue(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let department = parse::<Department>(str_field(&body, "department")?)?;
    let text = str_field(&body, "text")?.to_owned();
    let mut seal = Seal::default();
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        seal.capabilities = tools
            .iter()
            .map(|t| parse::<Capability>(t.as_str().unwrap_or_default()))
            .collect::<Result<BTreeSet<_>, _>>()?;
    }
    let scope: Vec<String> = body
        .get("scope")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_str().map(str::trim))
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect();
    if !scope.is_empty() {
        // 越出项目的路径由治理层拒绝；这里尽早指出不存在的路径。
        if let Some(missing) = scope.iter().find(|p| !app.0.config.root.join(p).exists()) {
            return Err(ApiError::bad(format!("查阅范围不存在: {missing}")));
        }
        seal.scope = scope;
    }
    app.mutate(move |s| {
        let edict = s.issue(department, text, seal)?;
        Ok(json!({ "id": edict.id.0 }))
    })
    .await
}

/// 在后台执行全部未结束的诏令；执行期间新入队的诏令在本轮结束后接着执行。
async fn run(State(app): State<AppState>) -> Result<Json<Value>, ApiError> {
    {
        let mut run = app.0.run.lock().expect("锁未中毒");
        if run.is_some() {
            return Err(ApiError(StatusCode::CONFLICT, "已在执行中".into()));
        }
        let lock = EventLog::new(&app.0.config.root)
            .try_lock_executor()
            .map_err(|e| ApiError::internal(e.to_string()))?
            .ok_or_else(|| {
                ApiError(
                    StatusCode::CONFLICT,
                    "另一个 ziyuan 进程正在执行诏令，请待其结束".into(),
                )
            })?;
        let cancel = CancelToken::new();
        *run = Some(cancel.clone());
        let worker = app.clone();
        let token = cancel.clone();
        tokio::spawn(async move {
            let _lock = lock;
            if let Err(error) = run_open_tasks(&worker, &token).await {
                worker.emit("run_error", json!({ "error": error.1 }));
            }
            *worker.0.run.lock().expect("锁未中毒") = None;
            worker.emit("run", json!({ "running": false }));
            worker.emit("changed", json!({}));
        });
    }
    app.emit("run", json!({ "running": true }));
    Ok(Json(json!({ "ok": true })))
}

async fn run_open_tasks(app: &AppState, cancel: &CancelToken) -> Result<(), ApiError> {
    let scheduler = Scheduler::new(app.0.config.backend.clone(), app.0.config.limits);
    let root = app.0.config.root.clone();
    let mut session = tokio::task::spawn_blocking(move || Session::open(EventLog::new(&root)))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))??;
    session
        .run_open(&scheduler, cancel, |update| match update {
            RunUpdate::Assigned(_) => app.emit("changed", json!({})),
            RunUpdate::Text { task, text } => {
                app.emit("text", json!({ "task": task.0, "text": text }));
            }
            RunUpdate::Finished(report) => {
                app.emit("finished", report_json(report));
                app.emit("changed", json!({}));
            }
        })
        .await?;
    Ok(())
}

async fn stop_run(State(app): State<AppState>) -> Json<Value> {
    let stopped = match app.0.run.lock().expect("锁未中毒").as_ref() {
        Some(cancel) => {
            cancel.cancel();
            true
        }
        None => false,
    };
    Json(json!({ "stopped": stopped }))
}

async fn cancel(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    app.mutate(move |s| {
        s.cancel(TaskId(id))?;
        Ok(json!({ "ok": true }))
    })
    .await
}

async fn review(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let verdict = parse::<Verdict>(str_field(&body, "verdict")?)?;
    let note = body["note"].as_str().unwrap_or_default().trim().to_owned();
    app.mutate(move |s| {
        s.review(TaskId(id), verdict, note)?;
        Ok(json!({ "ok": true }))
    })
    .await
}

async fn reform(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let mode = parse::<GovernanceMode>(str_field(&body, "mode")?)?;
    app.mutate(move |s| {
        s.reform(mode)?;
        Ok(json!({ "ok": true }))
    })
    .await
}

async fn recruit(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    // 姓名留空则随机取名。
    let name = body["name"]
        .as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_owned);
    let department = parse::<Department>(str_field(&body, "department")?)?;
    app.mutate(move |s| {
        let id = s.recruit(name, department)?;
        let name = &s.court().agent(id).expect("刚招募").name;
        Ok(json!({ "id": id.0, "name": name }))
    })
    .await
}

async fn appoint(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let office = parse::<Office>(str_field(&body, "office")?)?;
    app.mutate(move |s| {
        s.appoint(AgentId(id), office)?;
        Ok(json!({ "ok": true }))
    })
    .await
}

async fn dismiss(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    app.mutate(move |s| {
        s.dismiss(AgentId(id))?;
        Ok(json!({ "ok": true }))
    })
    .await
}

// ---- JSON 呈现 ----

fn str_field<'a>(body: &'a Value, key: &str) -> Result<&'a str, ApiError> {
    body.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad(format!("缺少字段 `{key}`")))
}

fn parse<T: std::str::FromStr>(input: &str) -> Result<T, ApiError>
where
    T::Err: std::fmt::Display,
{
    input
        .parse()
        .map_err(|e: T::Err| ApiError::bad(e.to_string()))
}

fn office_json(office: Office, mode: GovernanceMode) -> Value {
    json!({ "code": office.code(), "label": office.label(), "rank": office.rank(mode) })
}

fn usage_json(usage: TokenUsage) -> Value {
    json!({ "input": usage.input, "output": usage.output })
}

fn record_json(r: &ServiceRecord) -> Value {
    json!({
        "accepted": r.accepted,
        "rejected": r.rejected,
        "unreviewed": r.unreviewed,
        "failed": r.failed,
        "tool_calls": r.tool_calls,
        "tool_failures": r.tool_failures,
        "usage": usage_json(r.usage),
    })
}

fn outcome_json(outcome: &Outcome) -> Value {
    match outcome {
        Outcome::Completed(summary) => json!({ "kind": "completed", "summary": summary }),
        Outcome::Failed(reason) => json!({ "kind": "failed", "reason": reason }),
        Outcome::Cancelled => json!({ "kind": "cancelled" }),
        Outcome::NotStarted => json!({ "kind": "not_started" }),
    }
}

fn report_json(report: &TaskReport) -> Value {
    json!({
        "task": report.edict.id.0,
        "path": report.path,
        "outcome": outcome_json(&report.outcome),
        "usage": usage_json(report.usage),
    })
}

fn court_json(court: &Court) -> Value {
    let name = |id: AgentId| court.agent(id).map(|a| a.name.clone());
    let departments: Vec<_> = Department::ALL
        .iter()
        .map(|&d| {
            // 以一件空白诏令预演路由，展示现行制度下的派工路线。
            let route = court
                .new_edict(d, "预览".into(), Seal::default())
                .and_then(|edict| court.route(&edict));
            json!({
                "code": d.code(),
                "label": d.label(),
                "route": route.as_ref().ok().map(|r| &r.path),
                "vacancy": route.as_ref().err().map(|e| e.to_string()),
            })
        })
        .collect();
    // 现行制度下设置的官职；六部官职带上所属部门，供页面按专长筛选。
    let offices: Vec<_> = Office::all()
        .into_iter()
        .filter(|o| o.exists_in(court.mode()))
        .map(|o| {
            let mut value = office_json(o, court.mode());
            value["department"] = json!(match o {
                Office::Minister(d) | Office::ViceMinister(d) | Office::Clerk(d) => Some(d.code()),
                _ => None,
            });
            value
        })
        .collect();
    let officials: Vec<_> = court
        .agents()
        .map(|a| {
            json!({
                "id": a.id.0,
                "name": a.name,
                "speciality": { "code": a.speciality.code(), "label": a.speciality.label() },
                "office": court.office_of(a.id).map(|o| office_json(o, court.mode())),
                "record": record_json(&court.service_record(a.id)),
            })
        })
        .collect();
    let tasks: Vec<_> = court
        .tasks()
        .map(|t| {
            let (kind, worker, detail) = match &t.status {
                TaskStatus::Pending => ("pending", None, None),
                TaskStatus::Assigned(w) => ("assigned", Some(*w), None),
                TaskStatus::Completed { worker, summary } => {
                    ("completed", Some(*worker), Some(summary.clone()))
                }
                TaskStatus::Failed { worker, reason } => ("failed", *worker, Some(reason.clone())),
                TaskStatus::Cancelled => ("cancelled", None, None),
            };
            json!({
                "id": t.edict.id.0,
                "department": t.edict.department.label(),
                "text": t.edict.text,
                "seal": {
                    "default": t.edict.seal == Seal::default(),
                    "tools": t.edict.seal.capabilities.iter().map(|c| c.code()).collect::<Vec<_>>(),
                    "scope": t.edict.seal.scope,
                },
                "status": kind,
                "worker": worker.and_then(name),
                "detail": detail,
                "review": t.review.as_ref().map(|(v, note)| json!({
                    "verdict": v.code(), "label": v.label(), "note": note,
                })),
                "usage": usage_json(t.usage),
                "tool_calls": t.tool_calls,
                "tool_failures": t.tool_failures,
            })
        })
        .collect();
    json!({
        "mode": { "code": court.mode().code(), "label": court.mode().label() },
        "modes": GovernanceMode::ALL.iter().map(|m| json!({ "code": m.code(), "label": m.label() })).collect::<Vec<_>>(),
        "departments": departments,
        "offices": offices,
        "officials": officials,
        "tasks": tasks,
        "usage": usage_json(court.total_usage()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use ziyuan_runtime::DemoBackend;

    const TOKEN: &str = "test-token";

    /// 在临时目录建朝并启动服务，返回 (基地址, 目录)。
    async fn start(name: &str) -> (String, PathBuf) {
        let root = std::env::temp_dir().join(format!("ziyuan-web-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        Session::found(EventLog::new(&root)).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let config = Config {
            root: root.clone(),
            token: TOKEN.into(),
            backend: Arc::new(DemoBackend),
            limits: Limits {
                max_concurrent: 2,
                timeout: Duration::from_secs(10),
            },
        };
        tokio::spawn(serve(listener, config));
        (base, root)
    }

    /// 阻塞地发送请求，返回 (状态码, JSON)。`token` 为 `None` 时不带令牌。
    async fn call(
        base: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let url = format!("{base}/api{path}");
        let token = token.map(|t| format!("Bearer {t}"));
        tokio::task::spawn_blocking(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .into();
            let mut response = match body {
                Some(body) => {
                    let mut request = agent.post(&url);
                    if let Some(t) = &token {
                        request = request.header("Authorization", t);
                    }
                    request
                        .header("Content-Type", "application/json")
                        .send(body.to_string())
                        .unwrap()
                }
                None => {
                    let mut request = agent.get(&url);
                    if let Some(t) = &token {
                        request = request.header("Authorization", t);
                    }
                    request.call().unwrap()
                }
            };
            let status = response.status().as_u16();
            let text = response.body_mut().read_to_string().unwrap();
            (status, serde_json::from_str(&text).unwrap_or(Value::Null))
        })
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn api_requires_the_token() {
        let (base, root) = start("auth").await;
        assert_eq!(call(&base, "/state", None, None).await.0, 401);
        assert_eq!(call(&base, "/state", Some("wrong"), None).await.0, 401);
        let edict = json!({ "department": "works", "text": "越权" });
        assert_eq!(call(&base, "/edicts", None, Some(edict)).await.0, 401);
        // EventSource 只能用查询参数携带令牌。
        assert_eq!(
            call(&base, &format!("/history?token={TOKEN}"), None, None)
                .await
                .0,
            200
        );
        let (status, state) = call(&base, "/state", Some(TOKEN), None).await;
        assert_eq!(
            (status, state["mode"]["code"].as_str()),
            (200, Some("chancellor"))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn edict_runs_in_background_and_can_be_reviewed() {
        let (base, root) = start("run").await;
        let t = Some(TOKEN);
        for (scope, needle) in [("../x", "查阅范围"), ("missing", "不存在")] {
            let body = json!({ "department": "works", "text": "查", "scope": [scope] });
            let (status, error) = call(&base, "/edicts", t, Some(body)).await;
            assert_eq!(status, 400, "{scope}");
            assert!(error["error"].as_str().unwrap().contains(needle), "{error}");
        }
        let body = json!({ "department": "works", "text": "查 src", "scope": ["src"], "tools": ["search_code"] });
        let (status, issued) = call(&base, "/edicts", t, Some(body)).await;
        assert_eq!((status, issued["id"].as_u64()), (200, Some(1)));

        // 另一个进程（此处为测试本身）持有执行者锁时拒绝执行。
        let held = EventLog::new(&root).try_lock_executor().unwrap().unwrap();
        assert_eq!(call(&base, "/run", t, Some(json!({}))).await.0, 409);
        drop(held);
        assert_eq!(call(&base, "/run", t, Some(json!({}))).await.0, 200);

        let mut task = Value::Null;
        for _ in 0..100 {
            let (_, state) = call(&base, "/state", t, None).await;
            task = state["tasks"][0].clone();
            if task["status"] == "completed" && state["running"] == false {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(task["status"], "completed", "{task}");
        assert!(task["detail"].as_str().unwrap().contains("DEMO ONLY"));
        assert_eq!(task["seal"]["tools"], json!(["search_code"]));

        let review = json!({ "verdict": "accept", "note": "属实" });
        assert_eq!(
            call(&base, "/tasks/1/review", t, Some(review.clone()))
                .await
                .0,
            200
        );
        let (status, error) = call(&base, "/tasks/1/review", t, Some(review)).await;
        assert_eq!(status, 400);
        assert!(
            error["error"].as_str().unwrap().contains("已评定"),
            "{error}"
        );
        let (_, state) = call(&base, "/state", t, None).await;
        let worker = state["officials"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["record"]["accepted"] == 1)
            .expect("承办官员的考绩应记上采纳");
        assert_eq!(worker["name"], task["worker"]);
        std::fs::remove_dir_all(root).unwrap();
    }
}
