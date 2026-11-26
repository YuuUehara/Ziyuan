//! 受管写入（准奏落盘）中断恢复与陈旧临时文件清理（`ziyuan text recovery`）。

use std::env;
use std::error::Error;
use std::io::Read;
use ziyuan_app::translation::Translations;
use ziyuan_storage::recovery::{ApplicationV2, DecodedRecovery, SceneEvidence, classify_scene};
use ziyuan_storage::{LockScope, RecordState, Untouched, Workspace};

use super::{RecoveryAction, RecoveryPending, print_json};

/// 只读报告或收尾一次先前遗留的受管写入。这里不执行业务写入，也不再落盘任何项目文件。
pub(super) fn run_recovery(action: RecoveryAction, json: bool) -> Result<(), Box<dyn Error>> {
    let workspace = Workspace::open(env::current_dir()?)?;
    match action {
        RecoveryAction::Temps { apply } => return run_temps(&workspace, apply, json),
        RecoveryAction::Status => {
            let generation = workspace.generation()?;
            let record = workspace.pending_record_any()?;
            let state = match &record {
                None => "idle",
                Some(DecodedRecovery::V1(record)) => state_code(record.state),
                Some(DecodedRecovery::V2(record)) => state_code(record.state),
            };
            let next: String = match &record {
                None if generation % 2 != 0 => {
                    "上一次受管写入只留下奇数代次、没有恢复记录；请人工核对工作区，不要手工改代次"
                        .into()
                }
                None => "无待处理的受管写入".into(),
                Some(DecodedRecovery::V2(record)) => second_format_next(record),
                Some(DecodedRecovery::V1(record)) => {
                    let op = &record.operation_id;
                    match record.state {
                        RecordState::Prepared | RecordState::Abandoned => format!(
                            "可用 ziyuan text recovery finish {op} --yes 收尾；若目标仍是旧内容，也可用 ziyuan text recovery abandon {op} --yes 放弃"
                        ),
                        RecordState::Published => format!(
                            "须核对目标后用 ziyuan text recovery finish {op} --yes 收尾；目标被改过时不可 abandon"
                        ),
                        RecordState::Completed => {
                            format!("用 ziyuan text recovery finish {op} --yes 推进世代并清理记录")
                        }
                    }
                }
            };
            if json {
                let value = match &record {
                    None => serde_json::json!({ "generation": generation, "state": state }),
                    Some(DecodedRecovery::V1(record)) => serde_json::json!({
                        "generation": generation,
                        "format": 1,
                        "state": state,
                        "operation_id": record.operation_id,
                        "task": record.application.task,
                        "skipped": record.application.skipped,
                        "targets": record.targets.iter().map(|t| t.path.clone()).collect::<Vec<_>>(),
                        "next": next,
                    }),
                    Some(DecodedRecovery::V2(record)) => {
                        let mut value = serde_json::json!({
                            "generation": generation,
                            "format": 2,
                            "state": state,
                            "operation_id": record.operation_id,
                            "targets": record.targets.iter().map(|t| t.path.clone()).collect::<Vec<_>>(),
                            "next": next,
                        });
                        match &record.application {
                            ApplicationV2::Model(app) => {
                                value["kind"] = "model".into();
                                value["task"] = app.task.into();
                                value["skipped"] = serde_json::json!(app.skipped);
                            }
                            ApplicationV2::Human(app) => {
                                value["kind"] = "human".into();
                                value["project"] = app.project.clone().into();
                                value["block"] = app.block.clone().into();
                                value["approval_request_id"] =
                                    app.approval_request_id.clone().into();
                                value["target_format"] = app.target_format.clone().into();
                                value["logical_path"] = app.logical_path.clone().into();
                            }
                        }
                        value
                    }
                };
                print_json(value)?;
            } else {
                println!("世代号：{generation}（{}）", even_odd(generation));
                let suffix = match &record {
                    Some(DecodedRecovery::V2(_)) => "（格式第二版）",
                    _ => "",
                };
                println!("恢复记录：{state}{suffix}");
                match &record {
                    Some(DecodedRecovery::V1(record)) => {
                        println!("  操作 ID：{}", record.operation_id);
                        println!("  任务：#{}", record.application.task);
                        println!("  目标：{} 个", record.targets.len());
                        for target in &record.targets {
                            println!("    {}", target.path);
                        }
                    }
                    Some(DecodedRecovery::V2(record)) => {
                        println!("  操作 ID：{}", record.operation_id);
                        match &record.application {
                            ApplicationV2::Model(app) => {
                                println!("  来源：模型诏令来源（任务 #{}）", app.task);
                            }
                            ApplicationV2::Human(app) => {
                                println!("  来源：人工准奏来源");
                                println!("  项目：{}  块：{}", app.project, app.block);
                                println!("  准奏请求：{}", app.approval_request_id);
                                println!("  目标格式：{}", app.target_format);
                            }
                        }
                        println!("  目标：{} 个", record.targets.len());
                        for target in &record.targets {
                            println!("    {}", target.path);
                        }
                    }
                    None => {}
                }
                println!("下一步：{next}");
            }
            // 尚未收尾（世代为奇数）或仍有未清理的记录时，退出码 1：脚本据此判断还能不能写入。
            let unfinished = generation % 2 != 0 || record.is_some();
            if unfinished {
                return Err(RecoveryPending.into());
            }
        }
        RecoveryAction::Finish {
            operation_id,
            confirm_prepared,
        } => {
            finish_recovery(&workspace, &operation_id, confirm_prepared, json)?;
        }
        RecoveryAction::Abandon {
            operation_id,
            confirm_prepared,
        } => {
            let mut guard = workspace.lock(LockScope::Workspace)?;
            // 放弃的前提是「什么都没发生」：目标必须仍是旧内容，事件也一条没进事件簿。
            // 只靠 CLI 信任调用者不行，这里逐项核对；任何一项不成立都不放弃。
            let pending = workspace
                .pending_record_any()?
                .ok_or("没有待收尾的恢复记录；用 ziyuan text recovery status 查看")?;
            let record = match pending {
                DecodedRecovery::V1(_) if confirm_prepared => {
                    return Err(confirm_prepared_misused(
                        "abandon",
                        &operation_id,
                        "第一版恢复记录",
                    ));
                }
                DecodedRecovery::V1(record) => record,
                DecodedRecovery::V2(record) => {
                    return abandon_second_format(
                        &workspace,
                        &mut guard,
                        &record,
                        &operation_id,
                        confirm_prepared,
                        json,
                    );
                }
            };
            if record.operation_id != operation_id {
                return Err(format!(
                    "操作 ID 不符：磁盘上是 {}，不是 {operation_id}",
                    record.operation_id
                )
                .into());
            }
            for target in &record.targets {
                let actual = current_state(workspace.root(), &target.path)?;
                if actual != target.before {
                    return Err(format!(
                        "目标 {} 已不是记录中的旧内容，这次写入可能已经落盘：不能放弃。\n\
                         请改用 ziyuan text recovery finish {operation_id} --yes 收尾。",
                        target.path
                    )
                    .into());
                }
            }
            if events_present(workspace.root(), &record)? {
                return Err(format!(
                    "这次写入的事件已经在事件簿中：不能放弃。\n\
                     请改用 ziyuan text recovery finish {operation_id} --yes 收尾。"
                )
                .into());
            }
            if record.state != RecordState::Prepared && record.state != RecordState::Published {
                return Err(format!("记录状态为 {}，不能放弃", state_name(record.state)).into());
            }
            // 放弃之前再核一遍目标：锁只挡其他紫垣进程，挡不住外部编辑器。
            for target in &record.targets {
                if current_state(workspace.root(), &target.path)? != target.before {
                    return Err(format!(
                        "目标 {} 在核对之后被改动，放弃已中止；请先运行 finish 或重新核对。",
                        target.path
                    )
                    .into());
                }
            }
            let record = match record.state {
                RecordState::Published => {
                    guard.abandon_published(&operation_id, Untouched::confirmed())?
                }
                _ => guard.abandon(&operation_id)?,
            };
            guard.finish()?;
            guard.remove_record()?;
            match json {
                true => print_json(serde_json::json!({
                    "operation_id": record.operation_id,
                    "state": "abandoned",
                }))?,
                false => println!("已放弃操作 {operation_id}：目标未被改动，世代号已推进。"),
            }
        }
    }
    Ok(())
}

/// 失败项的文本行：路径与消息都过报告的脱敏出口（与 JSON 同一个函数）。
pub(super) fn problem_line(
    report: &ziyuan_runtime::tools::temp_cleanup::Report,
    problem: &ziyuan_runtime::tools::temp_cleanup::Problem,
) -> String {
    format!(
        "  失败：{}：{}",
        report.display_relative(&problem.path),
        report.scrub_message(&problem.message)
    )
}

/// 手工清理遗留临时文件。默认预览；`--yes` 才真删。
fn run_temps(workspace: &Workspace, apply: bool, json: bool) -> Result<(), Box<dyn Error>> {
    use ziyuan_runtime::tools::temp_cleanup::{Kind, MAX_DEPTH, Outcome, sweep_workspace};

    let protected = workspace
        .protected_targets()
        .map_err(|e| format!("读取恢复记录失败，本次未清理任何临时文件：{e}"))?;
    let report = sweep_workspace(workspace.root(), &protected, apply);
    if json {
        print_json(report.to_json())?;
    } else {
        let temps = report.counts(Kind::Temp);
        let leases = report.counts(Kind::Lease);
        println!("范围：{}（含子目录）", report.scope());
        println!(
            "模式：{}",
            if apply {
                "删除"
            } else {
                "预览，加 --yes 才删"
            }
        );
        println!(
            "临时文件：候选 {}，可清理 {}，已删除 {}，未删除 {}",
            temps.candidates, temps.removable, temps.removed, temps.kept
        );
        println!(
            "租约：候选 {}，可清理 {}，已删除 {}，未删除 {}",
            leases.candidates, leases.removable, leases.removed, leases.kept
        );
        if !report.skipped.is_empty() {
            println!(
                "  未进入的目录（按路径跳过）：{}",
                report.skipped.join("、")
            );
        }
        if !report.capped.is_empty() {
            println!(
                "  未扫描（超过 {MAX_DEPTH} 层深度上限）：{}",
                report.capped.join("、")
            );
            println!("  这些目录里的遗留文件不在本次报告范围内。");
        }
        for item in &report.items {
            let reason = item.reason_name();
            let status = match item.outcome {
                Outcome::Remove if item.acted => "已删除",
                Outcome::Remove => "可清理",
                Outcome::Keep(_) => "未删除",
            };
            println!(
                "  {status} {reason:<22} {}",
                report.display_relative(&item.path)
            );
        }
        for problem in &report.problems {
            println!("{}", problem_line(&report, problem));
        }
        for dir in &report.unscanned {
            println!("  读不了的目录：{}", report.display_relative(dir));
        }
    }
    if report.needs_attention() {
        return Err(match json {
            true => "临时文件清理未能完成：见报告中的 problems 与 unscanned".into(),
            false => "临时文件清理未能完成；见上面的逐项报告".into(),
        });
    }
    Ok(())
}

/// 收尾：只推进世代并清理记录，绝不重做业务写入。
fn finish_recovery(
    workspace: &Workspace,
    operation_id: &str,
    confirm_prepared: bool,
    json: bool,
) -> Result<(), Box<dyn Error>> {
    let mut guard = workspace.lock(LockScope::Workspace)?;
    let pending = workspace
        .pending_record_any()?
        .ok_or("没有待收尾的恢复记录；用 ziyuan text recovery status 查看")?;
    let record = match pending {
        DecodedRecovery::V1(_) if confirm_prepared => {
            return Err(confirm_prepared_misused(
                "finish",
                operation_id,
                "第一版恢复记录",
            ));
        }
        DecodedRecovery::V1(record) => record,
        DecodedRecovery::V2(record) => {
            return finish_second_format(
                workspace,
                &mut guard,
                &record,
                operation_id,
                confirm_prepared,
                json,
            );
        }
    };
    if record.operation_id != operation_id {
        return Err(format!(
            "操作 ID 不符：磁盘上是 {}，不是 {operation_id}",
            record.operation_id
        )
        .into());
    }
    let state = record.state;
    let recorded = events_present(workspace.root(), &record)?;
    let mut written = 0usize;
    let mut pending = Vec::new();
    for target in &record.targets {
        match current_state(workspace.root(), &target.path)? {
            state if state == target.after => written += 1,
            state if state == target.before => pending.push(target.path.clone()),
            _ => {
                return Err(format!(
                    "目标 {} 的当前内容既不是记录中的旧内容、也不是新内容：写入可能只完成了一部分。\n\
                     请先自行核对并修正该文件，再重试；不要用 finish 把不确定的状态盖过去。",
                    target.path
                )
                .into());
            }
        }
    }
    if state == RecordState::Published && !pending.is_empty() {
        let reason = match recorded {
            true => {
                "事件簿里已有本批记录，目标却仍是旧内容：两边互相矛盾，可能被外部改动过。\n\
                 请自行核对项目文件后，再决定收尾或放弃。"
            }
            false => {
                "这既可能是「一个文件都没写」——那是你已批准、却还没落盘的改动；\
                 也可能是「写到一半」。两者在盘上无法区分。"
            }
        };
        return Err(format!(
            "这次写入没有完成：{} 个目标仍是旧内容（{}）。\n\
             已写入的目标：{written} 个。\n\
             {reason}\n\
             本命令不会替你决定，请二选一：\n\
             · 想保留这次批准：重新审阅并再次准奏（目标已是新内容的会自动跳过）；\n\
             · 确认放弃：ziyuan text recovery abandon {operation_id} --yes（前提是目标全部仍是旧内容）。",
            pending.len(),
            pending.join("、")
        )
        .into());
    }
    if state == RecordState::Published {
        for target in &record.targets {
            if current_state(workspace.root(), &target.path)? != target.after {
                return Err(format!(
                    "目标 {} 在核对之后又被改动，收尾已中止；请稍后重试。",
                    target.path
                )
                .into());
            }
        }
        guard.complete(operation_id)?;
    }
    let generation = guard.finish()?;
    guard.remove_record()?;
    match json {
        true => print_json(serde_json::json!({
            "operation_id": operation_id,
            "generation": generation,
        }))?,
        false => println!("已收尾操作 {operation_id}：世代号推进到 {generation}，恢复记录已清理。"),
    }
    Ok(())
}

#[cfg(debug_assertions)]
fn interrupt_recovery(point: &str) {
    if std::env::var("ZIYUAN_INTERRUPT_RECOVERY").ok().as_deref() == Some(point) {
        std::process::abort();
    }
}

#[cfg(not(debug_assertions))]
fn interrupt_recovery(_point: &str) {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SecondGeneration {
    Reserved,
    Finished,
}

fn second_generation(
    workspace: &Workspace,
    record: &ziyuan_storage::recovery::RecoveryRecordV2,
    terminal: RecordState,
) -> Result<SecondGeneration, Box<dyn Error>> {
    let current = workspace.generation()?;
    let reserved = record.generation_before + 1;
    if current == reserved {
        return Ok(SecondGeneration::Reserved);
    }
    if current == record.generation_after && record.state == terminal {
        return Ok(SecondGeneration::Finished);
    }
    Err(format!(
        "当前代次 {current} 与恢复记录不符：操作 {} 只能在代次 {reserved} 收尾，或在已{}后的代次 {} 只清理记录。\n\
         请保留现场，运行 ziyuan text recovery status 核对后联系维护者；勿手工改代次或删记录。",
        record.operation_id,
        state_name(terminal).trim_start_matches('已'),
        record.generation_after
    )
    .into())
}

fn finish_second_generation(
    guard: &mut ziyuan_storage::WriteGuard,
    record: &ziyuan_storage::recovery::RecoveryRecordV2,
    phase: SecondGeneration,
) -> Result<u64, Box<dyn Error>> {
    if phase == SecondGeneration::Finished {
        return Ok(record.generation_after);
    }
    let generation = guard.finish()?;
    if generation != record.generation_after {
        return Err(format!(
            "代次推进到 {generation}，与记录的后代次 {} 不符。\n\
             请保留现场，运行 ziyuan text recovery status 核对后联系维护者；勿手工改代次或删记录。",
            record.generation_after
        )
        .into());
    }
    Ok(generation)
}

fn confirm_prepared_misused(action: &str, operation_id: &str, what: &str) -> Box<dyn Error> {
    Box::new(crate::Usage(
        format!(
            "--confirm-prepared 只用于第二版恢复记录的发布前中断（已准备）；这份是{what}，未做任何改动。\n\
             请去掉该参数后重试 ziyuan text recovery {action} {operation_id} --yes。"
        )
        .into(),
    ))
}

const KEEP_SCENE: &str = "请保留现场，运行 ziyuan text recovery status 核对后联系维护者；勿截断事件簿、勿手工改代次或删记录。";

fn finish_second_format(
    workspace: &Workspace,
    guard: &mut ziyuan_storage::WriteGuard,
    record: &ziyuan_storage::recovery::RecoveryRecordV2,
    operation_id: &str,
    confirm_prepared: bool,
    json: bool,
) -> Result<(), Box<dyn Error>> {
    let human = match &record.application {
        ApplicationV2::Human(app) => Some(app),
        ApplicationV2::Model(_) => None,
    };
    if record.operation_id != operation_id {
        return Err(format!(
            "操作 ID 不符：磁盘上是 {}，不是 {operation_id}",
            record.operation_id
        )
        .into());
    }
    if confirm_prepared && record.state != RecordState::Prepared {
        let what = format!("状态为 {} 的第二版记录", state_name(record.state));
        return Err(confirm_prepared_misused("finish", operation_id, &what));
    }
    if !matches!(
        record.state,
        RecordState::Prepared | RecordState::Published | RecordState::Completed
    ) {
        return Err(format!(
            "恢复记录状态为 {}，finish 只处理已准备、已发布或已完成的记录。\n\
             已放弃的记录请改用 ziyuan text recovery abandon {operation_id} --yes 清理；其他情况{KEEP_SCENE}",
            state_name(record.state)
        )
        .into());
    }
    if record.state == RecordState::Prepared && !confirm_prepared {
        return Err(format!(
            "恢复记录是“已准备”（发布前中断），本版默认不动。\n\
             请先运行 ziyuan text recovery status 核对目标与事件现场、确认旧进程已停写，\
             再用 ziyuan text recovery finish {operation_id} --yes --confirm-prepared 继续，\
             或 ziyuan text recovery abandon {operation_id} --yes --confirm-prepared 放弃。"
        )
        .into());
    }
    let phase = second_generation(workspace, record, RecordState::Completed)?;
    let window = read_record_window(workspace, record)?;
    let evidence = classify_scene(record, &window)?;
    let targets_after = |workspace: &Workspace| match human {
        Some(app) => targets_match(workspace, app, &record.targets, TargetSide::After),
        None => generic_targets_match(workspace, &record.targets, TargetSide::After),
    };
    let after_ok = targets_after(workspace)?;
    let recheck_human = |guard: &ziyuan_storage::WriteGuard| -> Result<(), Box<dyn Error>> {
        if human.is_none() {
            return Ok(());
        }
        Translations::new(workspace.root())
            .recheck_human_recovery(guard, record)
            .map_err(|e| {
                format!(
                    "补记前复核未通过：{e}。本次未改动任何文件或事件簿。\n\
                     若 zh.json 仍可还原为旧内容，可还原后用 ziyuan text recovery abandon {operation_id} --yes 放弃；\
                     否则{KEEP_SCENE}"
                )
                .into()
            })
    };
    interrupt_recovery("before-events");
    match (record.state, evidence) {
        (RecordState::Completed, SceneEvidence::ExactAtSlot)
        | (RecordState::Published, SceneEvidence::ExactAtSlot) => {}
        (RecordState::Completed, other) => {
            return Err(format!(
                "已完成记录的事件簿现场为“{}”，与记录不符。\n{KEEP_SCENE}",
                scene_name(other)
            )
            .into());
        }
        (RecordState::Published, SceneEvidence::CleanAbsent) => {
            if !after_ok {
                return Err(format!(
                    "事件簿里没有这批事件，目标也不是记录中的新内容：这次写入没有完成。\n\
                     若目标全为旧内容，可用 ziyuan text recovery abandon {operation_id} --yes 放弃；\
                     不要用 finish 把不确定的状态盖过去。"
                )
                .into());
            }
            recheck_human(guard)?;
            append_recorded_batch(workspace, record, || Ok(()))?;
        }
        (RecordState::Published, other) => {
            return Err(format!(
                "事件簿现场为“{}”，无法确认批次是否已完整追加。\n{KEEP_SCENE}",
                scene_name(other)
            )
            .into());
        }
        (RecordState::Prepared, SceneEvidence::CleanAbsent) => {
            if !after_ok {
                return Err(format!(
                    "发布前中断：目标不是记录中的新内容，这次写入没有完成。\n\
                     若目标全为旧内容，可用 ziyuan text recovery abandon {operation_id} --yes --confirm-prepared 放弃；\
                     否则{KEEP_SCENE}"
                )
                .into());
            }
            recheck_human(guard)?;
            append_recorded_batch(workspace, record, || {
                guard.publish_prepared_second_format(operation_id)?;
                Ok(())
            })?;
        }
        (RecordState::Prepared, other) => {
            return Err(format!(
                "发布前中断的事件簿现场为“{}”，不是“批次未追加”：状态与事件矛盾或位置不明。\n{KEEP_SCENE}",
                scene_name(other)
            )
            .into());
        }
        _ => unreachable!("状态已在前面筛过"),
    }
    interrupt_recovery("after-events");
    if !after_ok {
        return Err(format!("目标仍不是记录中的新内容，不能收尾。\n{KEEP_SCENE}").into());
    }
    if record.state != RecordState::Completed {
        if !targets_after(workspace)? {
            return Err(format!(
                "目标在核对之后又被改动，收尾已中止；事件簿已有本批记录。\n{KEEP_SCENE}"
            )
            .into());
        }
        guard.complete_second_format(operation_id)?;
    }
    let generation = finish_second_generation(guard, record, phase)?;
    interrupt_recovery("after-finish");
    guard.remove_second_format()?;
    match json {
        true => print_json(serde_json::json!({
            "operation_id": operation_id,
            "generation": generation,
        }))?,
        false => println!("已收尾操作 {operation_id}：世代号推进到 {generation}，恢复记录已清理。"),
    }
    Ok(())
}

fn abandon_second_format(
    workspace: &Workspace,
    guard: &mut ziyuan_storage::WriteGuard,
    record: &ziyuan_storage::recovery::RecoveryRecordV2,
    operation_id: &str,
    confirm_prepared: bool,
    json: bool,
) -> Result<(), Box<dyn Error>> {
    let human = match &record.application {
        ApplicationV2::Human(app) => Some(app),
        ApplicationV2::Model(_) => None,
    };
    if record.operation_id != operation_id {
        return Err(format!(
            "操作 ID 不符：磁盘上是 {}，不是 {operation_id}",
            record.operation_id
        )
        .into());
    }
    if confirm_prepared && record.state != RecordState::Prepared {
        let what = format!("状态为 {} 的第二版记录", state_name(record.state));
        return Err(confirm_prepared_misused("abandon", operation_id, &what));
    }
    if !matches!(
        record.state,
        RecordState::Prepared | RecordState::Published | RecordState::Abandoned
    ) {
        return Err(format!(
            "恢复记录状态为 {}，abandon 只处理已准备、已发布或已放弃的记录。\n\
             已完成的记录请改用 ziyuan text recovery finish {operation_id} --yes 收尾；其他情况{KEEP_SCENE}",
            state_name(record.state)
        )
        .into());
    }
    if record.state == RecordState::Prepared && !confirm_prepared {
        return Err(format!(
            "恢复记录是“已准备”（发布前中断），本版默认不动。\n\
             请先运行 ziyuan text recovery status 核对、确认旧进程已停写，\
             再用 ziyuan text recovery abandon {operation_id} --yes --confirm-prepared 放弃。"
        )
        .into());
    }
    let phase = second_generation(workspace, record, RecordState::Abandoned)?;
    let window = read_record_window(workspace, record)?;
    let evidence = classify_scene(record, &window)?;
    if evidence != SceneEvidence::CleanAbsent {
        return Err(format!(
            "事件簿现场为“{}”，不能当作“什么都没发生”放弃。\n\
             请保留现场，改用 ziyuan text recovery finish {operation_id} --yes 核对；勿截断事件簿或手工改代次。",
            scene_name(evidence)
        )
        .into());
    }
    let targets_before = |workspace: &Workspace| match human {
        Some(app) => targets_match(workspace, app, &record.targets, TargetSide::Before),
        None => generic_targets_match(workspace, &record.targets, TargetSide::Before),
    };
    if !targets_before(workspace)? {
        return Err(format!(
            "目标已不是记录中的旧内容，可能已经落盘：不能放弃。\n\
             请改用 ziyuan text recovery finish {operation_id} --yes 核对。"
        )
        .into());
    }
    if record.state != RecordState::Abandoned {
        if !targets_before(workspace)? {
            return Err(format!(
                "目标在核对之后被改动，放弃已中止。\n\
                 请改用 ziyuan text recovery finish {operation_id} --yes 重新核对。"
            )
            .into());
        }
        if record.state == RecordState::Prepared {
            guard.abandon_prepared_second_format(operation_id)?;
        } else {
            guard.abandon_published_second_format(operation_id)?;
        }
    }
    let generation = finish_second_generation(guard, record, phase)?;
    guard.remove_second_format()?;
    match json {
        true => print_json(serde_json::json!({
            "operation_id": operation_id,
            "generation": generation,
        }))?,
        false => println!("已放弃操作 {operation_id}：世代号推进到 {generation}，恢复记录已清理。"),
    }
    Ok(())
}

fn append_recorded_batch(
    workspace: &Workspace,
    record: &ziyuan_storage::recovery::RecoveryRecordV2,
    ready: impl FnOnce() -> Result<(), Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    let batch_len = record.events.encoded_events.len();
    let prefix = record
        .events
        .expected_events
        .checked_sub(batch_len as u64)
        .ok_or_else(|| format!("恢复记录的事件条数不足以容纳批次。\n{KEEP_SCENE}"))?;
    let prefix =
        usize::try_from(prefix).map_err(|_| format!("恢复记录的前缀条数过大。\n{KEEP_SCENE}"))?;
    let mut log = ziyuan_storage::EventLog::new(workspace.root());
    let history = ziyuan_storage::EventStore::load(&log)?;
    if history.len() != prefix {
        return Err(format!("事件簿在核对之后发生变化，已中止。\n{KEEP_SCENE}").into());
    }
    let events = record
        .events
        .encoded_events
        .iter()
        .map(|line| ziyuan_storage::decode_event_line(line))
        .collect::<Result<Vec<_>, String>>()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    log.append_if_unchanged_then(&history, &events, ready)??;
    Ok(())
}

fn second_format_next(record: &ziyuan_storage::recovery::RecoveryRecordV2) -> String {
    let op = &record.operation_id;
    let keep = "不要手工改代次或删记录";
    match record.state {
        RecordState::Prepared => format!(
            "第二版记录发布前中断：先核对目标与事件现场、确认旧进程已停写，再用 ziyuan text recovery finish {op} --yes --confirm-prepared 继续，或 ziyuan text recovery abandon {op} --yes --confirm-prepared 放弃；{keep}"
        ),
        RecordState::Published => format!(
            "第二版记录：核对目标与事件现场后用 ziyuan text recovery finish {op} --yes 收尾；目标仍全为旧内容且事件未追加时可用 ziyuan text recovery abandon {op} --yes 放弃；{keep}"
        ),
        RecordState::Completed => format!(
            "第二版记录已完成：用 ziyuan text recovery finish {op} --yes 核对事件后推进世代并清理记录；{keep}"
        ),
        RecordState::Abandoned => format!(
            "第二版记录已放弃：用 ziyuan text recovery abandon {op} --yes 核对目标与事件后推进世代并清理记录；{keep}"
        ),
    }
}

fn read_record_window(
    workspace: &Workspace,
    record: &ziyuan_storage::recovery::RecoveryRecordV2,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let batch_len = record
        .events
        .encoded_events
        .iter()
        .try_fold(0usize, |sum, line| sum.checked_add(line.len() + 1))
        .ok_or_else(|| format!("恢复记录的事件批过长，暂不支持核对。\n{KEEP_SCENE}"))?;
    let limit = usize::try_from(record.events.expected_bytes)
        .ok()
        .and_then(|prefix| prefix.checked_add(batch_len + 1))
        .ok_or_else(|| format!("恢复记录的前缀过长，暂不支持核对。\n{KEEP_SCENE}"))?;
    read_log_window(workspace.root(), limit)
}

fn generic_targets_match(
    workspace: &Workspace,
    targets: &[ziyuan_storage::recovery::TargetState],
    side: TargetSide,
) -> Result<bool, Box<dyn Error>> {
    for target in targets {
        let expected = match side {
            TargetSide::Before => &target.before,
            TargetSide::After => &target.after,
        };
        if current_state(workspace.root(), &target.path)? != *expected {
            return Ok(false);
        }
    }
    Ok(true)
}

#[derive(Clone, Copy)]
enum TargetSide {
    Before,
    After,
}

fn targets_match(
    workspace: &Workspace,
    app: &ziyuan_storage::recovery::HumanApplication,
    targets: &[ziyuan_storage::recovery::TargetState],
    side: TargetSide,
) -> Result<bool, Box<dyn Error>> {
    use ziyuan_storage::recovery::FileState;
    if targets.is_empty() {
        let want = match side {
            TargetSide::Before => match &app.before {
                Some(hash) => FileState::Present(hash.clone()),
                None => FileState::Absent,
            },
            TargetSide::After => FileState::Present(app.after.clone()),
        };
        return Ok(current_state(workspace.root(), &app.logical_path)? == want);
    }
    for target in targets {
        let expected = match side {
            TargetSide::Before => &target.before,
            TargetSide::After => &target.after,
        };
        if current_state(workspace.root(), &target.path)? != *expected {
            return Ok(false);
        }
    }
    Ok(true)
}

fn read_log_window(root: &std::path::Path, limit: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    const MAX_SCENE_BYTES: usize = 1 << 30;
    if limit > MAX_SCENE_BYTES {
        return Err("恢复记录的前缀超过 1 GiB，暂不支持本版收尾；请保留现场。".into());
    }
    let path = root.join(".ziyuan/court.log");
    match std::fs::File::open(&path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take(limit as u64).read_to_end(&mut bytes)?;
            Ok(bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("读不了事件簿 {}：{error}", path.display()).into()),
    }
}

fn scene_name(evidence: SceneEvidence) -> &'static str {
    match evidence {
        SceneEvidence::CleanAbsent => "批次未追加",
        SceneEvidence::ExactAtSlot => "批次在固定位置",
        SceneEvidence::Partial => "批次只写了一半",
        SceneEvidence::Diverged => "前缀或位置不符",
    }
}

fn even_odd(generation: u64) -> &'static str {
    match generation % 2 {
        0 => "无进行中的受管写入",
        _ => "上次受管写入未完成",
    }
}

fn state_code(state: RecordState) -> &'static str {
    match state {
        RecordState::Prepared => "prepared",
        RecordState::Published => "published",
        RecordState::Completed => "completed",
        RecordState::Abandoned => "abandoned",
    }
}

fn state_name(state: RecordState) -> &'static str {
    match state {
        RecordState::Prepared => "已准备",
        RecordState::Published => "已发布",
        RecordState::Completed => "已完成",
        RecordState::Abandoned => "已放弃",
    }
}

fn current_state(
    root: &std::path::Path,
    target: &str,
) -> Result<ziyuan_storage::recovery::FileState, Box<dyn Error>> {
    use ziyuan_storage::recovery::FileState;
    match std::fs::read(root.join(target)) {
        Ok(bytes) => Ok(FileState::Present(ziyuan_storage::content_sha256(&bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FileState::Absent),
        Err(e) => Err(format!("读不了目标 {target}：{e}").into()),
    }
}

fn events_present(
    root: &std::path::Path,
    record: &ziyuan_storage::RecoveryRecord,
) -> Result<bool, Box<dyn Error>> {
    let lines: Vec<String> =
        match ziyuan_storage::EventStore::load(&ziyuan_storage::EventLog::new(root)) {
            Ok(history) => history
                .iter()
                .map(ziyuan_storage::encode_event_line)
                .collect(),
            Err(_) => return Ok(false),
        };
    let batch = &record.events.encoded_events;
    let Some(base) = record
        .events
        .expected_events
        .checked_sub(batch.len() as u64)
    else {
        return Ok(false);
    };
    let base = base as usize;
    if lines.len() < base + batch.len() {
        return Ok(false);
    }
    let actual = ziyuan_storage::EventStore::history_sha256_at_most(
        &ziyuan_storage::EventLog::new(root),
        base,
    )?;
    if actual.as_deref() != Some(record.events.history_sha256.as_str()) {
        return Ok(false);
    }
    Ok(lines[base..base + batch.len()] == batch[..])
}
