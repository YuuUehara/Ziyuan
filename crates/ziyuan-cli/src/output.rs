//! 输出：流式回奏、票拟、报告与 `--json`。

use std::collections::BTreeSet;
use std::error::Error;
use std::io::Write;
use ziyuan_app::{
    Session, TaskReport, estimate_cost, format_time, history_entries, skipped_note, tz_offset,
};
use ziyuan_core::TaskId;
use ziyuan_governance::{ChangeState, Task};
use ziyuan_runtime::Outcome;
use ziyuan_storage::EventLog;

use crate::args::Command;
use crate::backend::{resolved_backend, worker_backend, workspace_price_for};

/// 把模型的流式输出实时写到 stderr，每行以 `#诏令ID│` 开头，并发任务不会混在同一行。
/// 最终回奏仍由 [`print_report`] 写到 stdout，便于脚本处理。
pub(crate) struct StreamPrinter<W: Write> {
    out: W,
    /// 当前行未结束的诏令。
    open: Option<TaskId>,
    /// 行被其他诏令打断（已代为换行）的诏令：其后紧接的换行符不再重复输出。
    interrupted: BTreeSet<TaskId>,
}

pub(crate) fn print_draft(session: &Session<EventLog>, task: TaskId) {
    let court = session.court();
    let Some(entry) = court.task(task) else {
        return;
    };
    let Some(draft) = &entry.draft else {
        return;
    };
    let name = court.agent(draft.worker).map_or("?", |a| a.name.as_str());
    println!("诏令 #{} 票拟：拟派 #{} {name}", task.0, draft.worker.0);
    for line in draft.note.lines().filter(|l| !l.trim().is_empty()) {
        println!("  {line}");
    }
}

pub(crate) fn print_report(report: &TaskReport) {
    println!("诏令 #{}：{}", report.edict.id.0, report.edict.text);
    if !report.path.is_empty() {
        println!("路由：{}", report.path.join(" → "));
    }
    match &report.outcome {
        Outcome::Completed(summary) => println!("回奏：{summary}"),
        Outcome::Failed(reason) => println!("失败：{reason}"),
        Outcome::Cancelled => println!("已撤销（执行中被中断）"),
        Outcome::NotStarted => println!("未开始：仍在队列中，可再次 `ziyuan run`"),
    }
    // 演示后端不调用模型；服务端未报告用量时也不显示。
    if report.usage.total() > 0 {
        let model = report.model.as_deref().unwrap_or("?");
        let cost = workspace_price_for(model)
            .map_or(String::new(), |p| format!("，{}", p.describe(report.usage)));
        println!("用量：{}（{model}）{cost}", report.usage);
    }
}

/// 显示用的承办模型（与 [`worker_backend`] 的选择一致）：演示后端不调用模型。
/// 按各诏令实际所用模型估算的花费（单价见 `ziyuan_llm::price_for`）；查不到单价时为空。
pub(crate) fn cost_note<'a>(tasks: impl IntoIterator<Item = &'a Task>) -> String {
    let default = resolved_backend().map(|m| m.model_label);
    estimate_cost(tasks, default.as_deref(), workspace_price_for)
        .map_or(String::new(), |c| format!("，{c}"))
}

/// 呈报改动的状态，如「；改动 2 个文件待准奏」；没有改动时为空。
pub(crate) fn changes_note(task: &Task) -> String {
    let Some(changes) = &task.changes else {
        return String::new();
    };
    let state = match changes.state {
        ChangeState::Proposed => "待准奏",
        ChangeState::Applied => "已落盘",
        ChangeState::Discarded => "已驳回",
    };
    let hunks: usize = changes.files.iter().map(|f| f.hunks().len()).sum();
    format!(
        "；改动 {} 个文件 {hunks} 段{state}{}",
        changes.files.len(),
        skipped_note(&changes.skipped)
    )
}

/// `--json`：朝廷全貌与 Web 的 `/api/state` 同构；`officials`、`tasks` 取其中一项；
/// `history` 为 `[{seq, text}]`（`seq` 从 1 起，与 `ziyuan history` 的序号一致）。
pub(crate) fn json_output(
    session: &Session<EventLog>,
    command: &Command,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let state = || -> Result<serde_json::Value, Box<dyn Error>> {
        let mut value = ziyuan_web::court_json(session.court(), worker_backend()?.as_ref(), None);
        ziyuan_web::mark_stale(&mut value, session.court(), &std::env::current_dir()?);
        Ok(value)
    };
    Ok(match command {
        Command::Court => state()?,
        Command::Officials => state()?["officials"].take(),
        Command::Tasks => state()?["tasks"].take(),
        Command::History(filter) => {
            let offset = tz_offset();
            history_entries(session.history()?, filter)?
                .into_iter()
                .map(|e| {
                    serde_json::json!({
                        "seq": e.seq,
                        "at": e.at,
                        "time": e.at.map(|t| format_time(t, offset)),
                        "kind": e.kind,
                        "text": e.text,
                    })
                })
                .collect()
        }
        Command::Records(query) => {
            let records = session.court().records();
            ziyuan_core::find_records(&records, query)
                .into_iter()
                .map(ziyuan_web::chronicle_json)
                .collect()
        }
        // 刑部与锦衣卫的密报，逐处展开；index 即 `acquit` 所用的序号。
        Command::Flags(only) => {
            let mut tasks = state()?["tasks"].take();
            let mut flags = Vec::new();
            for task in tasks.as_array_mut().into_iter().flatten() {
                let id = task["id"].as_u64();
                if only.is_some_and(|o| Some(o.0) != id) {
                    continue;
                }
                for (i, s) in task["suspicions"]
                    .take()
                    .as_array_mut()
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    s["task"] = serde_json::json!(id);
                    s["index"] = serde_json::json!(i + 1);
                    flags.push(s.take());
                }
            }
            flags.into()
        }
        Command::ModelList => {
            let cwd = std::env::current_dir()?;
            let config = ziyuan_llm::ModelsConfig::load(&cwd).map_err(|e| e.to_string())?;
            let resolved = resolved_backend();
            let profiles: Vec<_> = config
                .map(|c| c.summaries())
                .unwrap_or_default()
                .into_iter()
                .map(|p| {
                    serde_json::json!({
                        "name": p.name,
                        "provider": p.provider,
                        "provider_label": p.provider_label,
                        "model": p.model,
                        "base_url": p.base_url,
                        "api_key_env": p.api_key_env,
                        "api_key_ready": p.api_key_ready,
                        "is_default": p.is_default,
                    })
                })
                .collect();
            let court = session.court();
            let office_overrides: Vec<_> = ziyuan_core::Office::all()
                .into_iter()
                .filter_map(|o| {
                    court.office_model(o).map(|m| {
                        serde_json::json!({
                            "office": o.code(),
                            "label": o.label(),
                            "model": m,
                        })
                    })
                })
                .collect();
            serde_json::json!({
                "default_model": resolved.as_ref().map(|r| r.model_label.clone()),
                "default_provider": resolved.as_ref().map(|r| r.provider_kind.code()),
                "profiles": profiles,
                "office_overrides": office_overrides,
            })
        }
        Command::Model(..) | Command::ModelDept(..) => {
            return Err("--json 仅支持 `ziyuan model list`".into());
        }
        Command::Mcp => ziyuan_runtime::mcp::survey(&std::env::current_dir()?)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|(name, tools)| match tools {
                Ok(tools) => serde_json::json!({
                    "name": name,
                    "ok": true,
                    "tools": tools.into_iter().map(|t| serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    })).collect::<Vec<_>>(),
                }),
                Err(error) => serde_json::json!({ "name": name, "ok": false, "error": error }),
            })
            .collect(),
        _ => unreachable!("run 已检查 JSON_COMMANDS"),
    })
}

impl<W: Write> StreamPrinter<W> {
    pub(crate) fn new(out: W) -> Self {
        Self {
            out,
            open: None,
            interrupted: BTreeSet::new(),
        }
    }

    /// 结束当前未完成的行（属于其他诏令时记为被打断）。
    pub(crate) fn break_line(&mut self, task: Option<TaskId>) {
        if let Some(open) = self.open.take() {
            let _ = writeln!(self.out);
            if Some(open) != task {
                self.interrupted.insert(open);
            }
        }
    }

    pub(crate) fn text(&mut self, task: TaskId, text: &str) {
        for piece in text.split_inclusive('\n') {
            if self.open != Some(task) {
                self.break_line(None);
                if self.interrupted.remove(&task) && piece == "\n" {
                    continue;
                }
                let _ = write!(self.out, "#{}│ ", task.0);
            }
            let _ = write!(self.out, "{piece}");
            self.open = (!piece.ends_with('\n')).then_some(task);
        }
        let _ = self.out.flush();
    }

    pub(crate) fn tool_started(&mut self, task: TaskId, step: usize, tool: &str, summary: &str) {
        self.break_line(None);
        if summary.is_empty() {
            let _ = writeln!(self.out, "#{}│ [第 {step} 步] ▸ {tool}…", task.0);
        } else {
            let _ = writeln!(self.out, "#{}│ [第 {step} 步] ▸ {tool} {summary}…", task.0);
        }
        let _ = self.out.flush();
    }

    pub(crate) fn tool_finished(
        &mut self,
        task: TaskId,
        step: usize,
        tool: &str,
        ok: bool,
        duration_ms: u64,
    ) {
        self.break_line(None);
        let mark = if ok { "✓" } else { "✗" };
        let dur = format_duration_ms(duration_ms);
        let _ = writeln!(
            self.out,
            "#{}│ [第 {step} 步] {mark} {tool} ({dur})",
            task.0
        );
        let _ = self.out.flush();
    }

    /// `task` 已结束：在打印回奏前结束任何未完成的行。
    pub(crate) fn finish(&mut self, task: TaskId) {
        self.break_line(Some(task));
        self.interrupted.remove(&task);
        let _ = self.out.flush();
    }
}

pub(crate) fn format_duration_ms(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        let mins = ms / 60_000;
        let secs = (ms % 60_000) as f64 / 1_000.0;
        format!("{mins}m{secs:.1}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_printer_keeps_concurrent_lines_apart() {
        let mut p = StreamPrinter::new(Vec::new());
        let (a, b) = (TaskId(1), TaskId(2));
        p.text(a, "甲一\n甲");
        p.text(b, "乙");
        // 甲的行被乙打断后，紧接的换行符只是结束那一行，不应产生空行。
        p.text(a, "\n甲三");
        p.text(a, "\n\n");
        p.text(b, "乙二");
        p.finish(a);
        p.text(b, "尾");
        p.finish(b);
        assert_eq!(
            String::from_utf8(p.out).unwrap(),
            "#1│ 甲一\n#1│ 甲\n#2│ 乙\n#1│ 甲三\n#1│ \n#2│ 乙二\n#2│ 尾\n"
        );
    }

    #[test]
    fn stream_printer_formats_tool_progress_and_duration() {
        let mut p = StreamPrinter::new(Vec::new());
        let task = TaskId(3);
        p.text(task, "先查看");
        p.tool_started(task, 1, "read_file", "src/lib.rs:1-40");
        p.tool_finished(task, 1, "read_file", true, 42);
        p.tool_started(task, 2, "run_tests", "");
        p.tool_finished(task, 2, "run_tests", false, 1_450);
        p.finish(task);
        assert_eq!(
            String::from_utf8(p.out).unwrap(),
            "#3│ 先查看\n#3│ [第 1 步] ▸ read_file src/lib.rs:1-40…\n#3│ [第 1 步] ✓ read_file (42ms)\n#3│ [第 2 步] ▸ run_tests…\n#3│ [第 2 步] ✗ run_tests (1.4s)\n"
        );
        assert_eq!(format_duration_ms(65_200), "1m5.2s");
    }
}
