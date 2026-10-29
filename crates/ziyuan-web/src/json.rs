//! 把朝廷状态、实录与回奏转成前端用的 JSON。

use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use ziyuan_app::{
    TaskReport, base_hash, changes_version, demotion_note, estimate_cost, kaocheng_grade,
    render_diffs, stale_files,
};
use ziyuan_core::{
    AgentId, BudgetScope, Department, GovernanceMode, Office, Persona, Seal, TaskId, TokenUsage,
    missing_sections,
};
use ziyuan_governance::{ChangeState, Court, ServiceRecord, Task, TaskStatus};
use ziyuan_runtime::{Outcome, WorkerBackend};

/// 给待准奏改动的各文件标上 `stale`：原文自呈报以来已被别处改动，准奏会被拒。
/// 要读项目文件，故不在 [`court_json`] 里做。
pub fn mark_stale(value: &mut Value, court: &Court, root: &Path) {
    let Some(tasks) = value["tasks"].as_array_mut() else {
        return;
    };
    for entry in tasks {
        let Some(changes) = entry["id"]
            .as_u64()
            .and_then(|id| court.task(TaskId(id)))
            .and_then(|t| t.changes.as_ref())
            .filter(|c| c.state == ChangeState::Proposed)
        else {
            continue;
        };
        let stale = stale_files(root, &changes.files);
        if let Some(files) = entry["changes"]["files"].as_array_mut() {
            for (file, stale) in files.iter_mut().zip(stale) {
                file["stale"] = json!(stale);
            }
        }
    }
}

/// 一件实录的 JSON（`ziyuan records --json` 同此）。
pub fn chronicle_json(r: &ziyuan_core::Record) -> Value {
    json!({
        "task": r.task.0,
        "department": r.department.code(),
        "edict": r.edict,
        "worker": r.worker.0,
        "handler": r.handler,
        "summary": &*r.summary,
        "verdict": r.verdict,
    })
}

pub(crate) fn office_json(office: Office, mode: GovernanceMode) -> Value {
    json!({ "code": office.code(), "label": office.label(), "rank": office.rank(mode) })
}

pub(crate) fn usage_json(usage: TokenUsage) -> Value {
    json!({ "input": usage.input, "output": usage.output })
}

pub(crate) fn record_json(r: &ServiceRecord) -> Value {
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
        "exams": r.exams,
        "exams_passed": r.exams_passed,
        "zhuangyuan": r.zhuangyuan,
        "nominated": r.nominated,
        "caned": r.caned,
        "grade": kaocheng_grade(r),
        "usage": usage_json(r.usage),
    })
}

pub(crate) fn outcome_json(outcome: &Outcome) -> Value {
    match outcome {
        Outcome::Completed(summary) => json!({ "kind": "completed", "summary": summary }),
        Outcome::Failed(reason) => json!({ "kind": "failed", "reason": reason }),
        Outcome::Cancelled => json!({ "kind": "cancelled" }),
        Outcome::NotStarted => json!({ "kind": "not_started" }),
    }
}

pub(crate) fn report_json(report: &TaskReport) -> Value {
    json!({
        "task": report.edict.id.0,
        "path": report.path,
        "outcome": outcome_json(&report.outcome),
        "usage": usage_json(report.usage),
    })
}

/// 朝廷全貌（Web 的 `/api/state`，亦供 CLI `--json`）。
/// 页面默认列出的已了结诏令件数：上万件时整页 JSON 达数十 MB，每次刷新都要秒级渲染。
pub(crate) const RECENT_TASKS: usize = 50;

/// 朝廷全貌。`recent` 为 `Some(n)` 时诏令只列尚待理会的（未结束、待评定、改动待准奏，
/// 及其分办的原诏令与子诏令）与最近 `n` 件；`task_total` / `open_total` 总是全部的件数。
pub fn court_json(court: &Court, backend: &dyn WorkerBackend, recent: Option<usize>) -> Value {
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
            value["budget"] = json!(court.budget(BudgetScope::Office(o)));
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
                "courtesy": court.courtesy_name(a.id),
                "art": court.art_name(a.id),
                "persona": Persona::of(a.id).label(),
                "speciality": { "code": a.speciality.code(), "label": a.speciality.label() },
                "office": court.office_of(a.id).map(|o| office_json(o, court.mode())),
                "purged": court.is_purged(a.id),
                "demotion": demotion_note(court, a.id),
                "model": court.model_of(a.id).or(default_model),
                "ceiling": court.office_of(a.id).and_then(|o| court.office_ceiling(o)).map(|c| c.to_string()),
                "record": record_json(&court.service_record(a.id)),
                "cost": cost(court.tasks().filter(|t| t.handler() == Some(a.id)).collect()),
            })
        })
        .collect();
    let all: Vec<&Task> = court.tasks().collect();
    let open = |t: &Task| matches!(t.status, TaskStatus::Pending | TaskStatus::Assigned(_));
    let pending = |t: &Task| {
        open(t)
            || (matches!(t.status, TaskStatus::Completed { .. }) && t.review.is_none())
            || t.changes
                .as_ref()
                .is_some_and(|c| c.state == ChangeState::Proposed)
    };
    let first_recent = recent.map_or(0, |n| all.len().saturating_sub(n));
    let shown: BTreeSet<TaskId> = all
        .iter()
        .enumerate()
        .filter(|(i, t)| recent.is_none() || *i >= first_recent || pending(t))
        .flat_map(|(_, t)| t.parent.into_iter().chain([t.edict.id]))
        .collect();
    let tasks: Vec<_> = all
        .iter()
        .filter(|t| shown.contains(&t.edict.id) || t.parent.is_some_and(|p| shown.contains(&p)))
        .map(|t| {
            let (kind, worker, detail) = match &t.status {
                TaskStatus::Pending => ("pending", None, None),
                TaskStatus::Assigned(w) => ("assigned", Some(*w), None),
                TaskStatus::Completed { worker, summary } => {
                    ("completed", Some(*worker), Some(&**summary))
                }
                TaskStatus::Failed { worker, reason } => ("failed", *worker, Some(reason.as_str())),
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
                    "mcp": t.edict.seal.mcp,
                },
                "status": kind,
                "needs_approval": court.needs_approval(t),
                // 排程（分办 DAG）：受阻时为第一件未成的依赖，前端据此标「受阻」。
                "blocked_by": court.blocked_by(t).map(|b| b.0),
                "dag": t.dag.as_ref().map(|d| json!({
                    "dependencies": d.dependencies.iter().map(|x| x.0).collect::<Vec<_>>(),
                    "attempts": d.attempts,
                    "deadline": d.deadline,
                    "budget": d.budget,
                    "artifact": d.artifact,
                    "approval_required": d.approval_required,
                    "idempotency_key": d.idempotency_key,
                })),
                "proxied": t.proxied,
                "budget": t.budget,
                "urgent": t.urgent,
                "courier": t.courier,
                "replied": t.replied,
                "pull_request": t.pull_request,
                // 经 GitHub 驿传入队、此刻可回报评论 / 可开草稿 PR。
                "github_reply": t.courier.as_deref().is_some_and(|c| c.starts_with("github:"))
                    && court.propose_courier_reply(t.edict.id, false, "-").is_ok(),
                "github_pr": t.courier.as_deref().is_some_and(|c| c.starts_with("github:"))
                    && court.propose_courier_reply(t.edict.id, true, "-").is_ok(),
                "template": t.template.as_ref().map(|u| json!({
                    "name": u.name,
                    "criteria": u.criteria,
                })),
                "suspicions": t.suspicions.iter().map(|s| json!({
                    "bureau": s.bureau(),
                    "source": s.origin(),
                    "threat": s.threat.label(),
                    "excerpt": s.excerpt,
                    "review": s.review.as_ref().map(|r| json!({
                        "upheld": r.upheld,
                        "label": if r.upheld { "维持" } else { "误报" },
                        "note": r.note,
                        "reviewer": name(r.reviewer),
                        "model": r.model,
                    })),
                    "acquitted": s.acquitted,
                })).collect::<Vec<_>>(),
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
                    "skipped": c.skipped,
                    // 准奏时回传，服务端据以确认审阅的正是记下的这份改动。
                    "version": changes_version(&c.files),
                    "files": c.files.iter().zip(render_diffs(&c.files)).zip(ziyuan_app::preview_changes(&c.files)).map(|((f, diff), preview)| {
                        let count = |kind: char| preview.hunks.iter().flat_map(|h| &h.lines).filter(|l| l.kind == kind).count();
                        json!({
                            "path": f.path,
                            "created": f.before.is_none(),
                            "base_hash": base_hash(f),
                            "additions": count('+'),
                            "deletions": count('-'),
                            "diff": diff,
                            // 着色、带行号与词级标记的预览，段号与 diff 相同；status 为该段的处置。
                            "hunks": preview.hunks.iter().map(|h| {
                                let rejected = c.state == ChangeState::Discarded || c.skipped.contains(&h.number);
                                let status = match c.state {
                                    ChangeState::Proposed => "pending",
                                    _ if rejected => "rejected",
                                    _ => "approved",
                                };
                                json!({ "number": h.number, "status": status, "lines": h.lines })
                            }).collect::<Vec<_>>(),
                        })
                    }).collect::<Vec<_>>(),
                })),
                "worker": worker.and_then(name),
                // 礼部：回奏缺了哪些体例段落（发回重写后仍缺的照收并标注）。
                "missing_sections": match &t.status {
                    TaskStatus::Completed { summary, .. } => missing_sections(summary),
                    _ => Vec::new(),
                },
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
                        // 排程：先办的依赖与受阻的那件（写子诏令自动串行时即前一件）。
                        "deps": sub.dag.as_ref().map(|d| d.dependencies.iter().map(|x| x.0).collect::<Vec<_>>()).unwrap_or_default(),
                        "blocked_by": court.blocked_by(sub).map(|b| b.0),
                    })).collect::<Vec<_>>(),
                })),
                "debates": t.debates.iter().map(|speeches| speeches.iter().map(|sp| json!({
                    "name": name(sp.speaker),
                    "office": sp.office.label(),
                    "persona": Persona::of(sp.speaker).label(),
                    "text": sp.text,
                    "model": sp.model,
                })).collect::<Vec<_>>()).collect::<Vec<_>>(),
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
        "retreat": court.in_retreat(),
        "treasury": court.budget(BudgetScope::Court).map(|limit| json!({
            "limit": limit,
            "remaining": court.treasury_remaining(),
        })),
        "modes": GovernanceMode::ALL.iter().map(|m| json!({ "code": m.code(), "label": m.label() })).collect::<Vec<_>>(),
        "departments": departments,
        "vacancies": ziyuan_app::vacancies(court).iter().map(|v| json!({
            "office": v.office.code(),
            "office_label": v.office.label(),
            "impact": v.impact,
            "candidate": v.candidate.as_ref().map(|(id, name)| json!({ "id": id.0, "name": name })),
        })).collect::<Vec<_>>(),
        "offices": offices,
        "officials": officials,
        "task_total": all.len(),
        "open_total": all.iter().filter(|t| open(t)).count(),
        "tasks": tasks,
        "templates": court.templates().into_iter().map(|(name, t, builtin)| json!({
            "name": name,
            "builtin": builtin,
            "department": t.department.code(),
            "text": t.text,
            "tools": t.seal.capabilities.iter().map(|c| c.code()).collect::<Vec<_>>(),
            "scope": t.seal.scope,
            "mcp": t.seal.mcp,
            "criteria": t.criteria,
        })).collect::<Vec<_>>(),
        // 最近一次殿试。
        "palace": court.palace_exams().last().map(|p| json!({
            "question": p.question,
            "zhuangyuan": p.zhuangyuan.map(|a| a.0),
            "answers": p.answers.iter().map(|a| json!({
                "id": a.speaker.0,
                "name": name(a.speaker),
                "office": a.office.label(),
                "persona": Persona::of(a.speaker).label(),
                "text": a.text,
                "model": a.model,
            })).collect::<Vec<_>>(),
        })),
        "usage": usage_json(court.total_usage()),
        "model": default_model,
        "cost": cost(court.tasks().collect()),
    })
}
