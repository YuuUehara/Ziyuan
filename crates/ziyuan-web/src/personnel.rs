//! 人事与改制的 API：修道、改制、官职设置、户部度支、罢相、科举与任免。

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use ziyuan_app::ABOLITION_NOTE;
use ziyuan_core::{
    AgentId, BudgetScope, Capability, Department, GovernanceMode, Office, Persona, Seal, TaskId,
};

use crate::state::{ApiError, AppState, parse, str_field, string_list};

/// 修道（`{on: true}`）或回朝（`{on: false}`）。
pub(crate) async fn retreat(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let on = body["on"]
        .as_bool()
        .ok_or_else(|| ApiError::bad("缺少布尔字段 on"))?;
    app.mutate(move |s| Ok(json!({ "note": s.retreat(on)? })))
        .await
}

pub(crate) async fn reform(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let mode = parse::<GovernanceMode>(str_field(&body, "mode")?)?;
    app.mutate(move |s| Ok(json!({ "note": s.reform(mode)? })))
        .await
}

/// `{ office, model }`；`model` 为 null 或空串时恢复默认模型。
pub(crate) async fn office_model(
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
pub(crate) async fn office_ceiling(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let office = parse::<Office>(str_field(&body, "office")?)?;
    let ceiling = if body["clear"].as_bool() == Some(true) {
        None
    } else {
        // 与 CLI 一致：未给外藩上限即不设限（`*`），诏令仍须逐件授权；`["none"]` 为一律不许。
        let mcp = string_list(&body["mcp"]);
        let mut seal = Seal {
            mcp: match mcp.as_slice() {
                [] => ["*".to_owned()].into(),
                [none] if none == "none" => BTreeSet::new(),
                _ => mcp.into_iter().collect(),
            },
            ..Seal::default()
        };
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

/// 一键朱元璋：罢中书省，株连百官。
pub(crate) async fn abolish(State(app): State<AppState>) -> Result<Json<Value>, ApiError> {
    app.mutate(|s| {
        let summary = s.abolish_chancellery()?;
        Ok(json!({ "summary": summary, "note": ABOLITION_NOTE }))
    })
    .await
}

/// 礼部科举（会等待模型作答）：应试 `.ziyuan/exam/` 下全部考题，按要点判分。
pub(crate) async fn exam(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    let backend = app.0.config.backend.clone();
    let root = app.0.config.root.clone();
    app.mutate(move |s| {
        let mut results = Vec::new();
        s.exam(AgentId(id), &root, backend.as_ref(), |paper, score| {
            results.push(json!({ "exam": paper.name, "score": score }));
        })?;
        Ok(json!({ "results": results }))
    })
    .await
}

/// 殿试（会等待全部答卷）：`{ question }`。每份答卷以 SSE `palace` 事件实时推送。
pub(crate) async fn palace(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let question = str_field(&body, "question")?.to_owned();
    let backend = app.0.config.backend.clone();
    let live = app.clone();
    app.mutate(move |s| {
        let stopped = s.palace_exam(&question, backend.as_ref(), |agent, answer| {
            live.emit(
                "palace",
                json!({
                    "name": agent.name,
                    "office": answer.office.label(),
                    "persona": Persona::of(agent.id).label(),
                    "text": answer.text,
                }),
            )
        })?;
        Ok(json!({ "stopped": stopped }))
    })
    .await
}

/// 钦点状元，返回传胪史官记。
pub(crate) async fn crown(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    app.mutate(move |s| Ok(json!({ "note": s.crown(AgentId(id))? })))
        .await
}

pub(crate) async fn purge(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    app.mutate(move |s| {
        s.purge(AgentId(id))?;
        Ok(json!({ "ok": true }))
    })
    .await
}

pub(crate) async fn recruit(
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

pub(crate) async fn appoint(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let office = parse::<Office>(str_field(&body, "office")?)?;
    app.mutate(move |s| Ok(json!({ "note": s.appoint(AgentId(id), office)? })))
        .await
}

/// 廷推：九卿各举一人；`lots` 则再于得票者中枚卜，径直任命。
pub(crate) async fn nominate(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let office = parse::<Office>(str_field(&body, "office")?)?;
    let lots = body.get("lots").and_then(Value::as_bool).unwrap_or(false);
    let backend = app.0.config.backend.clone();
    app.mutate(move |s| {
        let mut votes = Vec::new();
        s.nominate(office, backend.as_ref(), |voter, vote| {
            votes.push(
                json!({ "voter": voter.name, "candidate": vote.candidate.0, "note": vote.note }),
            );
        })?;
        let court = s.court();
        let name = |id: AgentId| court.agent(id).map(|a| a.name.clone());
        for vote in &mut votes {
            let id = AgentId(vote["candidate"].as_u64().unwrap_or_default());
            vote["candidate_name"] = json!(name(id));
        }
        let tally: Vec<_> = court
            .nomination_tally(office)
            .into_iter()
            .map(|(id, n)| json!({ "id": id.0, "name": name(id), "votes": n }))
            .collect();
        let note = if lots {
            Some(s.draw_lots(office)?.1)
        } else {
            None
        };
        Ok(json!({ "votes": votes, "tally": tally, "note": note }))
    })
    .await
}
/// `{ name?, courtesy?, art? }`：留空或缺省的一项不变。
pub(crate) async fn rename(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let field = |key: &str| {
        body[key]
            .as_str()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let (name, courtesy, art) = (field("name"), field("courtesy"), field("art"));
    app.mutate(move |s| Ok(json!({ "note": s.rename(AgentId(id), name, courtesy, art)? })))
        .await
}
pub(crate) async fn cane(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    app.mutate(move |s| Ok(json!({ "note": s.cane(AgentId(id))? })))
        .await
}
pub(crate) async fn pardon(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    app.mutate(move |s| Ok(json!({ "note": s.pardon(AgentId(id))? })))
        .await
}
pub(crate) async fn dismiss(
    State(app): State<AppState>,
    UrlPath(id): UrlPath<u64>,
) -> Result<Json<Value>, ApiError> {
    app.mutate(move |s| {
        s.dismiss(AgentId(id))?;
        Ok(json!({ "ok": true }))
    })
    .await
}

/// 户部度支：`{ "scope": "court" | "office" | "task", "office"?, "task"?, "tokens": 数或 null }`。
pub(crate) async fn budget(
    State(app): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let scope = match str_field(&body, "scope")? {
        "court" => BudgetScope::Court,
        "office" => BudgetScope::Office(parse::<Office>(str_field(&body, "office")?)?),
        "task" => BudgetScope::Task(TaskId(
            body["task"]
                .as_u64()
                .ok_or_else(|| ApiError::bad("缺少诏令 ID"))?,
        )),
        other => return Err(ApiError::bad(format!("度支范围无效：{other}"))),
    };
    let tokens = match &body["tokens"] {
        Value::Null => None,
        value => Some(
            value
                .as_u64()
                .ok_or_else(|| ApiError::bad("额度须为正整数（tokens）"))?,
        ),
    };
    app.mutate(move |s| {
        s.set_budget(scope, tokens)?;
        Ok(json!({ "ok": true }))
    })
    .await
}
