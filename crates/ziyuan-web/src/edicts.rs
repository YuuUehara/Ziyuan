//! 诏令相关的 API：下诏、模板、评定、准奏、票拟、廷议、复核与驿站。

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use ziyuan_app::CHANGELOG_NOTE;
use ziyuan_core::{AgentId, Capability, Department, Persona, Seal, TaskId, Template, Verdict};
use ziyuan_governance::ChangeState;

use crate::state::{ApiError, AppState, parse, str_field, string_list};

/// 兵部驿站：外部事件（CI、issue 机器人等）凭令牌（`Authorization: Bearer`）投递，入队为诏令。
/// `{ text, department?（默认 works）, station?（默认「驿站」）, urgent? }`；只入队，不执行。
pub(crate) async fn courier(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let department = parse::<Department>(body["department"].as_str().unwrap_or("works"))?;
    let text = str_field(&body, "text")?.trim().to_owned();
    let station = body["station"].as_str().unwrap_or("驿站").to_owned();
    let urgent = body["urgent"].as_bool().unwrap_or(false);
    app.mutate(move |s| {
        let edict = s.post(department, text, Seal::default(), &station, urgent)?;
        let task = s.court().task(edict.id).expect("刚入队");
        Ok(json!({
            "id": edict.id.0,
            "suspicions": task.suspicions.len(),
            "needs_approval": s.court().needs_approval(task),
        }))
    })
    .await
}

/// 八百里加急。
pub(crate) async fn urgent(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    app.mutate(move |s| {
        s.expedite(TaskId(id))?;
        Ok(json!({ "ok": true }))
    })
    .await
}

pub(crate) async fn issue(
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
    seal.mcp = string_list(&body["mcp"]).into_iter().collect();
    let scope = string_list(&body["scope"]);
    if !scope.is_empty() {
        // 越出项目的路径由治理层拒绝；这里尽早指出不存在的路径。
        if let Some(missing) = scope.iter().find(|p| !app.0.config.root.join(p).exists()) {
            return Err(ApiError::bad(format!("查阅范围不存在: {missing}")));
        }
        seal.scope = scope;
    }
    // 按翰林院模板下诏：部门、内容与玉玺以页面所填为准，验收标准取模板的。
    let template = body["template"]
        .as_str()
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    app.mutate(move |s| {
        let edict = match &template {
            Some(name) => s.issue_templated(name, department, text, seal)?,
            None => s.issue(department, text, seal)?,
        };
        Ok(json!({ "id": edict.id.0 }))
    })
    .await
}

/// 翰林院存模板：`{ name, department, text, tools?, scope?, criteria: [..] }`（同名覆盖）。
pub(crate) async fn save_template(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let name = str_field(&body, "name")?.trim().to_owned();
    let mut template = Template {
        department: parse::<Department>(str_field(&body, "department")?)?,
        text: str_field(&body, "text")?.trim().to_owned(),
        seal: Seal::default(),
        criteria: string_list(&body["criteria"]),
    };
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        template.seal.capabilities = tools
            .iter()
            .map(|t| parse::<Capability>(t.as_str().unwrap_or_default()))
            .collect::<Result<BTreeSet<_>, _>>()?;
    }
    let scope = string_list(&body["scope"]);
    if !scope.is_empty() {
        template.seal.scope = scope;
    }
    app.mutate(move |s| {
        s.save_template(&name, template)?;
        Ok(json!({ "ok": true }))
    })
    .await
}

/// 翰林院撤去自定的模板：`{ name }`。
pub(crate) async fn remove_template(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let name = str_field(&body, "name")?.to_owned();
    app.mutate(move |s| {
        s.remove_template(&name)?;
        Ok(json!({ "ok": true }))
    })
    .await
}

pub(crate) async fn cancel(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    app.mutate(move |s| {
        s.cancel(TaskId(id))?;
        Ok(json!({ "ok": true }))
    })
    .await
}

/// 准奏：把呈报的改动写入项目；`skip` 为逐段驳回的段号，`commit` 为真时落盘后记为一个 jj/git 提交，
/// `version` 为预览所示的改动版本（可省；给了而不符即 409）。
pub(crate) async fn apply_changes(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let root = app.0.config.root.clone();
    let skipped = match body.get("skip") {
        None | Some(Value::Null) => Vec::new(),
        Some(list) => list
            .as_array()
            .and_then(|l| {
                l.iter()
                    .map(|n| n.as_u64().and_then(|n| u32::try_from(n).ok()))
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or_else(|| ApiError::bad("skip 须为段号数组"))?,
    };
    let commit = body.get("commit").and_then(Value::as_bool).unwrap_or(false);
    let version = match body.get("version") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .ok_or_else(|| ApiError::bad("version 须为字符串"))?
                .to_owned(),
        ),
    };
    app.mutate(move |s| {
        s.reload()?;
        let applied = s
            .court()
            .task(TaskId(id))
            .and_then(|t| t.changes.as_ref())
            .is_some_and(|c| c.state == ChangeState::Applied);
        // 已落盘而上次未能提交：带 commit 再请求只补提交。
        if !(commit && skipped.is_empty() && applied) {
            s.apply_reviewed(TaskId(id), &root, skipped, version.as_deref())?;
        }
        if !commit {
            return Ok(json!({ "ok": true }));
        }
        // 已落盘、已记入事件簿：提交失败只报告，不当作准奏失败。
        Ok(match s.commit_applied(TaskId(id), &root) {
            Ok(made) => json!({ "ok": true, "vcs": made.vcs, "commit": made.id }),
            Err(e) => json!({ "ok": true, "commit_error": e.to_string() }),
        })
    })
    .await
}

pub(crate) async fn discard_changes(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    app.mutate(move |s| {
        s.discard_changes(TaskId(id))?;
        Ok(json!({ "ok": true }))
    })
    .await
}

pub(crate) async fn review(
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

/// 内阁票拟（会等待模型返回）。
pub(crate) async fn draft(
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
pub(crate) async fn split(
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

/// 廷议（会等待全部发言）：每段发言以 SSE `speech` 事件实时推送，结束后整场记入事件簿。
/// 请求体可给 `rounds`（1–3，默认 2）与 `members`（官员 ID，默认宰辅、本部长官与御史）。
pub(crate) async fn debate(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let backend = app.0.config.backend.clone();
    let rounds = body["rounds"].as_u64().unwrap_or(2) as usize;
    let members: Vec<AgentId> = body["members"]
        .as_array()
        .map(|ids| ids.iter().filter_map(Value::as_u64).map(AgentId).collect())
        .unwrap_or_default();
    let live = app.clone();
    app.mutate(move |s| {
        let stopped = s.debate(
            TaskId(id),
            &members,
            rounds,
            backend.as_ref(),
            |agent, speech| {
                live.emit(
                    "speech",
                    json!({
                        "task": id,
                        "name": agent.name,
                        "office": speech.office.label(),
                        "persona": Persona::of(agent.id).label(),
                        "text": speech.text,
                    }),
                )
            },
        )?;
        Ok(json!({ "stopped": stopped }))
    })
    .await
}

/// 都察院复核（会等待御史的模型返回）。
pub(crate) async fn censor(
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

/// 刑部复勘该诏令尚未复勘的检出（会等待模型返回）。
pub(crate) async fn recheck(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    let backend = app.0.config.backend.clone();
    app.mutate(move |s| {
        let results = s.recheck(TaskId(id), backend.as_ref())?;
        let results: Vec<Value> = results
            .into_iter()
            .map(|(index, upheld)| json!({ "index": index, "upheld": upheld }))
            .collect();
        Ok(json!({ "results": results }))
    })
    .await
}

/// 平反：`{ index? }`（从 0 起）；不给则平反刑部判为误报的各处。
pub(crate) async fn acquit(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let index = match &body["index"] {
        Value::Null => None,
        v => Some(vec![
            v.as_u64()
                .and_then(|i| u32::try_from(i).ok())
                .ok_or_else(|| ApiError::bad("`index` 须为从 0 起的序号"))?,
        ]),
    };
    app.mutate(move |s| {
        let (done, note) = s.acquit(TaskId(id), index)?;
        Ok(json!({ "acquitted": done, "note": note }))
    })
    .await
}

/// 礼部修 CHANGELOG：`{ version?（默认 Unreleased）, since? }`，据 `git log` 下一件礼部诏令（只入队）。
pub(crate) async fn changelog(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let version = body["version"]
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("Unreleased")
        .to_owned();
    let since = body["since"]
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned);
    let root = app.0.config.root.clone();
    app.mutate(move |s| {
        let (range, commits) = ziyuan_app::changelog_commits(&root, since)?;
        let edict = s.draft_changelog(&version, &commits)?;
        Ok(json!({ "id": edict.id.0, "range": range, "commits": commits.len(), "note": CHANGELOG_NOTE }))
    })
    .await
}

/// 兵部驿站 · GitHub：带标签的 issue/PR 入队，`{ label?（默认 ziyuan）, department?（默认 works） }`。
pub(crate) async fn github_pull(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let hub = app.github()?;
    let label = body["label"]
        .as_str()
        .filter(|l| !l.trim().is_empty())
        .unwrap_or(ziyuan_github::DEFAULT_LABEL)
        .to_owned();
    let department = parse::<Department>(body["department"].as_str().unwrap_or("works"))?;
    app.mutate(move |s| {
        let report = s.github_pull(&hub, &label, department, &Seal::default())?;
        let queued: Vec<Value> = report
            .queued
            .iter()
            .map(|(e, station, flags)| json!({ "id": e.id.0, "station": station, "suspicions": flags }))
            .collect();
        Ok(json!({ "issues": report.issues, "queued": queued }))
    })
    .await
}

/// 回奏写回来处的 GitHub 评论。
pub(crate) async fn github_reply(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    let hub = app.github()?;
    app.mutate(move |s| {
        let (number, url) = s.github_reply(&hub, TaskId(id))?;
        Ok(json!({ "number": number, "url": url }))
    })
    .await
}

/// 准奏落盘的改动在远端开为草稿 PR。
pub(crate) async fn github_pr(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    let hub = app.github()?;
    app.mutate(move |s| {
        let url = s.github_pull_request(&hub, TaskId(id))?;
        Ok(json!({ "url": url }))
    })
    .await
}

/// 御笔改票：`{ worker, note? }`。
pub(crate) async fn redraft(
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

pub(crate) async fn approve(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    app.mutate(move |s| {
        s.approve(TaskId(id))?;
        Ok(json!({ "ok": true }))
    })
    .await
}
