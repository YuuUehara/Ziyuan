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
use ziyuan_app::{
    ABOLITION_NOTE, AppError, RunUpdate, Session, TaskReport, describe_event, estimate_cost,
    kaocheng_grade, kaocheng_report, render_diff,
};
use ziyuan_core::{
    AgentId, Capability, Department, GovernanceMode, Office, Seal, TaskId, TokenUsage, Verdict,
};
use ziyuan_governance::{ChangeState, Court, ServiceRecord, Task, TaskStatus};
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
        .route("/kaocheng", get(kaocheng))
        .route("/events", get(events_stream))
        .route("/edicts", post(issue))
        .route("/run", post(run))
        .route("/run/stop", post(stop_run))
        .route("/tasks/{id}/cancel", post(cancel))
        .route("/tasks/{id}/review", post(review))
        .route("/tasks/{id}/approve", post(approve))
        .route("/tasks/{id}/draft", post(draft))
        .route("/tasks/{id}/redraft", post(redraft))
        .route("/tasks/{id}/censor", post(censor))
        .route("/tasks/{id}/split", post(split))
        .route("/tasks/{id}/changes/apply", post(apply_changes))
        .route("/tasks/{id}/changes/discard", post(discard_changes))
        .route("/reform", post(reform))
        .route("/offices/model", post(office_model))
        .route("/offices/ceiling", post(office_ceiling))
        .route("/abolish", post(abolish))
        .route("/officials", post(recruit))
        .route("/officials/{id}/appoint", post(appoint))
        .route("/officials/{id}/dismiss", post(dismiss))
        .route("/officials/{id}/purge", post(purge))
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
            AppError::NotFounded | AppError::AlreadyFounded | AppError::Apply(_) => {
                StatusCode::CONFLICT
            }
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
    let backend = app.0.config.backend.clone();
    let mut value = app
        .with_session(move |s| Ok(court_json(s.court(), backend.as_ref())))
        .await?;
    value["running"] = json!(running);
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

/// 考成簿（纯文本）。
async fn kaocheng(State(app): State<AppState>) -> Result<Json<Value>, ApiError> {
    let report = app.with_session(|s| Ok(kaocheng_report(s.court()))).await?;
    Ok(Json(json!({ "report": report })))
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
    let scope = string_list(&body["scope"]);
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

/// 准奏：把呈报的改动写入项目。
async fn apply_changes(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    app.mutate(move |s| {
        s.apply_changes(TaskId(id), &root)?;
        Ok(json!({ "ok": true }))
    })
    .await
}

async fn discard_changes(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    app.mutate(move |s| {
        s.discard_changes(TaskId(id))?;
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
    app.mutate(move |s| Ok(json!({ "note": s.reform(mode)? })))
        .await
}

/// `{ office, model }`；`model` 为 null 或空串时恢复默认模型。
async fn office_model(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let office = parse::<Office>(str_field(&body, "office")?)?;
    let model = body["model"]
        .as_str()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_owned);
    app.mutate(move |s| {
        s.set_office_model(office, model)?;
        Ok(json!({ "ok": true }))
    })
    .await
}

/// `{ office, tools?, scope? }` 设定上限（未给出的一项不设限）；`{ office, clear: true }` 取消。
async fn office_ceiling(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let office = parse::<Office>(str_field(&body, "office")?)?;
    let ceiling = if body["clear"].as_bool() == Some(true) {
        None
    } else {
        let mut seal = Seal::default();
        if let Some(tools) = body.get("tools").and_then(Value::as_array) {
            seal.capabilities = tools
                .iter()
                .map(|t| parse::<Capability>(t.as_str().unwrap_or_default()))
                .collect::<Result<BTreeSet<_>, _>>()?;
        }
        let scope = string_list(&body["scope"]);
        if !scope.is_empty() {
            seal.scope = scope;
        }
        Some(seal)
    };
    app.mutate(move |s| {
        s.set_office_ceiling(office, ceiling)?;
        Ok(json!({ "ok": true }))
    })
    .await
}

/// 去掉空白项的字符串数组。
fn string_list(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_str().map(str::trim))
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 一键朱元璋：罢中书省，株连百官。
async fn abolish(State(app): State<AppState>) -> Result<Json<Value>, ApiError> {
    app.mutate(|s| {
        let summary = s.abolish_chancellery()?;
        Ok(json!({ "summary": summary, "note": ABOLITION_NOTE }))
    })
    .await
}

/// 内阁票拟（会等待模型返回）。
async fn draft(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    let backend = app.0.config.backend.clone();
    app.mutate(move |s| {
        let worker = s.draft(TaskId(id), backend.as_ref())?;
        let name = &s.court().agent(worker).expect("拟派在册官员").name;
        Ok(json!({ "worker": worker.0, "name": name }))
    })
    .await
}

/// 尚书分办（会等待模型返回）。
async fn split(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    let backend = app.0.config.backend.clone();
    app.mutate(move |s| {
        let subtasks = s.plan_split(TaskId(id), backend.as_ref())?;
        Ok(json!({ "subtasks": subtasks.iter().map(|t| t.0).collect::<Vec<_>>() }))
    })
    .await
}

/// 都察院复核（会等待御史的模型返回）。
async fn censor(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    let backend = app.0.config.backend.clone();
    let root = app.0.config.root.clone();
    app.mutate(move |s| {
        let verdict = s.censor(TaskId(id), &root, backend.as_ref())?;
        Ok(json!({ "verdict": verdict.code(), "label": verdict.label() }))
    })
    .await
}

/// 御笔改票：`{ worker, note? }`。
async fn redraft(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let worker = body["worker"]
        .as_u64()
        .ok_or_else(|| ApiError::bad("缺少字段 `worker`"))?;
    let note = body["note"].as_str().unwrap_or_default().to_owned();
    app.mutate(move |s| {
        s.redraft(TaskId(id), AgentId(worker), note)?;
        Ok(json!({ "ok": true }))
    })
    .await
}

async fn approve(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    app.mutate(move |s| {
        s.approve(TaskId(id))?;
        Ok(json!({ "ok": true }))
    })
    .await
}

async fn purge(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    app.mutate(move |s| {
        s.purge(AgentId(id))?;
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
        "endorsed": r.endorsed,
        "impeached": r.impeached,
        "verified": r.verified,
        "grade": kaocheng_grade(r),
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

fn court_json(court: &Court, backend: &dyn WorkerBackend) -> Value {
    let name = |id: AgentId| court.agent(id).map(|a| a.name.clone());
    // 按各诏令实际所用模型估算花费；演示后端为 null。
    let default_model = backend.model_name();
    let cost = |tasks: Vec<&Task>| estimate_cost(tasks, default_model, |m| backend.price(m));
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
            value["model"] = json!(court.office_model(o));
            value["ceiling"] = json!(court.office_ceiling(o).map(|c| c.to_string()));
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
                "purged": court.is_purged(a.id),
                "model": court.model_of(a.id).or(default_model),
                "ceiling": court.office_of(a.id).and_then(|o| court.office_ceiling(o)).map(|c| c.to_string()),
                "record": record_json(&court.service_record(a.id)),
                "cost": cost(court.tasks().filter(|t| t.handler() == Some(a.id)).collect()),
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
                "needs_approval": court.needs_approval(t),
                "draft": t.draft.as_ref().map(|d| json!({
                    "worker": d.worker.0,
                    "name": name(d.worker),
                    "note": d.note,
                    "model": d.model,
                })),
                // 待批红的诏令附上将要执行的派工路线（按票拟，未票拟则按旧例）与可拟派的官员。
                "preview": court.needs_approval(t).then(|| match court.route_task(t) {
                    Ok(route) => json!({ "route": route.path }),
                    Err(e) => json!({ "error": e.to_string() }),
                }),
                "candidates": court.needs_approval(t).then(|| {
                    court
                        .draft_candidates(t.edict.department)
                        .iter()
                        .map(|(a, o)| json!({ "id": a.id.0, "name": a.name, "office": o.label() }))
                        .collect::<Vec<_>>()
                }),
                "changes": t.changes.as_ref().map(|c| json!({
                    "state": match c.state {
                        ChangeState::Proposed => "proposed",
                        ChangeState::Applied => "applied",
                        ChangeState::Discarded => "discarded",
                    },
                    "files": c.files.iter().map(|f| json!({
                        "path": f.path,
                        "created": f.before.is_none(),
                        "diff": render_diff(f),
                    })).collect::<Vec<_>>(),
                })),
                "worker": worker.and_then(name),
                "detail": detail,
                "review": t.review.as_ref().map(|(v, note)| json!({
                    "verdict": v.code(), "label": v.label(), "note": note,
                })),
                "parent": t.parent.map(|p| p.0),
                "split": t.split.as_ref().map(|s| json!({
                    "note": s.note,
                    "open": court.open_subtasks(t),
                    "subtasks": s.subtasks.iter().filter_map(|id| court.task(*id)).map(|sub| json!({
                        "id": sub.edict.id.0,
                        "text": sub.edict.text,
                        "worker": sub.handler().or(sub.assignee).and_then(name),
                        "status": match sub.status {
                            TaskStatus::Pending => "pending",
                            TaskStatus::Assigned(_) => "assigned",
                            TaskStatus::Completed { .. } => "completed",
                            TaskStatus::Failed { .. } => "failed",
                            TaskStatus::Cancelled => "cancelled",
                        },
                    })).collect::<Vec<_>>(),
                })),
                "censure": t.censure.as_ref().map(|c| json!({
                    "verdict": c.verdict.code(),
                    "label": c.verdict.label(),
                    "censor": name(c.censor),
                    "note": c.note,
                    "evidence": c.evidence.to_string(),
                    "verified": c.evidence.verified(),
                    "model": c.model,
                })),
                "usage": usage_json(t.usage),
                "model": t.model,
                "cost": cost(vec![t]),
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
        "model": default_model,
        "cost": cost(court.tasks().collect()),
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
    async fn offices_can_be_given_models_and_seal_ceilings() {
        let (base, root) = start("offices").await;
        let t = Some(TOKEN);
        let model = json!({ "office": "clerk:works", "model": "deepseek-v4-pro" });
        assert_eq!(call(&base, "/offices/model", t, Some(model)).await.0, 200);
        let bad = json!({ "office": "clerk:works", "model": "two words" });
        assert_eq!(call(&base, "/offices/model", t, Some(bad)).await.0, 400);
        let ceiling =
            json!({ "office": "clerk:works", "scope": ["src"], "tools": ["search_code"] });
        assert_eq!(
            call(&base, "/offices/ceiling", t, Some(ceiling)).await.0,
            200
        );
        let escape = json!({ "office": "clerk:works", "scope": ["../x"] });
        assert_eq!(
            call(&base, "/offices/ceiling", t, Some(escape)).await.0,
            400
        );

        let (_, state) = call(&base, "/state", t, None).await;
        let clerk = state["offices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["code"] == "clerk:works")
            .unwrap();
        assert_eq!(clerk["model"], "deepseek-v4-pro");
        assert_eq!(clerk["ceiling"], "工具：search_code；范围：src");
        let official = state["officials"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["office"]["code"] == "clerk:works")
            .unwrap();
        assert_eq!(official["model"], "deepseek-v4-pro");
        // 演示后端无单价：不显示花费。
        assert!(state["cost"].is_null());

        let clear = json!({ "office": "clerk:works", "clear": true });
        call(&base, "/offices/ceiling", t, Some(clear)).await;
        let (_, state) = call(&base, "/state", t, None).await;
        assert!(state["officials"][3]["ceiling"].is_null());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn abolition_and_cabinet_approval() {
        let (base, root) = start("cabinet").await;
        let t = Some(TOKEN);
        // 革去丞相 #1 并废相；废相后不能再一键朱元璋（本测试末尾在丞相制下一键株连）。
        assert_eq!(
            call(&base, "/officials/1/purge", t, Some(json!({})))
                .await
                .0,
            200
        );
        let direct = json!({ "mode": "direct" });
        assert_eq!(call(&base, "/reform", t, Some(direct)).await.0, 200);
        assert_eq!(call(&base, "/abolish", t, Some(json!({}))).await.0, 400);
        let (_, reformed) = call(&base, "/reform", t, Some(json!({ "mode": "cabinet" }))).await;
        assert!(reformed["note"].as_str().unwrap().contains("四叔"));
        for path in ["/officials/6/dismiss", "/officials/5/purge"] {
            assert_eq!(call(&base, path, t, Some(json!({}))).await.0, 200, "{path}");
        }
        // 已革职者不能再任官。
        let office = json!({ "office": "first-secretary" });
        assert_eq!(
            call(&base, "/officials/1/appoint", t, Some(office.clone()))
                .await
                .0,
            400
        );
        call(&base, "/officials/6/appoint", t, Some(office)).await;
        let edict = json!({ "department": "works", "text": "查" });
        call(&base, "/edicts", t, Some(edict)).await;
        let (_, state) = call(&base, "/state", t, None).await;
        let task = &state["tasks"][0];
        assert_eq!(task["needs_approval"], true);
        assert!(task["draft"].is_null());
        assert!(
            task["preview"]["route"][1]
                .as_str()
                .unwrap()
                .contains("内阁首辅")
        );
        // 演示后端不调用模型：按旧例拟派；改票给他部官员被拒，改给本部尚书则路线随之改变。
        let (status, drafted) = call(&base, "/tasks/1/draft", t, Some(json!({}))).await;
        assert_eq!((status, drafted["worker"].as_u64()), (200, Some(4)));
        let other = json!({ "worker": 3 });
        assert_eq!(call(&base, "/tasks/1/redraft", t, Some(other)).await.0, 400);
        let minister = json!({ "worker": 2, "note": "尚书亲办" });
        assert_eq!(
            call(&base, "/tasks/1/redraft", t, Some(minister)).await.0,
            200
        );
        let (_, state) = call(&base, "/state", t, None).await;
        let task = &state["tasks"][0];
        assert_eq!(task["draft"]["note"], "御笔改票：尚书亲办");
        let route = task["preview"]["route"].as_array().unwrap();
        assert!(
            route
                .last()
                .unwrap()
                .as_str()
                .unwrap()
                .ends_with("（票拟）")
        );
        assert_eq!(task["candidates"].as_array().unwrap().len(), 2);
        assert!(state["officials"][0]["purged"].as_bool().unwrap());
        assert_eq!(
            call(&base, "/tasks/1/approve", t, Some(json!({}))).await.0,
            200
        );
        let (_, state) = call(&base, "/state", t, None).await;
        assert_eq!(state["tasks"][0]["needs_approval"], false);

        // 一键朱元璋：在任官员全部株连革职，无人再有官职。
        let chancellor = json!({ "mode": "chancellor" });
        call(&base, "/reform", t, Some(chancellor)).await;
        let (status, abolished) = call(&base, "/abolish", t, Some(json!({}))).await;
        assert_eq!(status, 200);
        assert!(abolished["summary"].as_str().unwrap().contains("株连所及"));
        let (_, state) = call(&base, "/state", t, None).await;
        assert_eq!(state["mode"]["code"], "direct");
        let officials = state["officials"].as_array().unwrap();
        assert!(
            officials.iter().all(|o| o["office"].is_null()),
            "{officials:?}"
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

        // 都察院：御史空缺时拒绝；任命后复核。演示回奏无可验证之据，按证据例弹劾。
        let (status, error) = call(&base, "/tasks/1/censor", t, Some(json!({}))).await;
        assert!(status == 400 && error["error"].as_str().unwrap().contains("左都御史空缺"));
        let recruit = json!({ "department": "justice" });
        let (_, recruited) = call(&base, "/officials", t, Some(recruit)).await;
        let censor = recruited["id"].as_u64().unwrap();
        let office = json!({ "office": "censor" });
        let appoint = format!("/officials/{censor}/appoint");
        assert_eq!(call(&base, &appoint, t, Some(office)).await.0, 200);
        let (status, censured) = call(&base, "/tasks/1/censor", t, Some(json!({}))).await;
        assert_eq!(
            (status, censured["verdict"].as_str()),
            (200, Some("impeach"))
        );
        let (_, state) = call(&base, "/state", t, None).await;
        let censure = &state["tasks"][0]["censure"];
        assert_eq!(censure["evidence"], "回奏未引用代码位置；未跑测试");
        assert_eq!(censure["verified"], false);
        let (_, kaocheng) = call(&base, "/kaocheng", t, None).await;
        let report = kaocheng["report"].as_str().unwrap();
        assert!(
            report.contains("考成簿") && report.contains("左都御史"),
            "{report}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
