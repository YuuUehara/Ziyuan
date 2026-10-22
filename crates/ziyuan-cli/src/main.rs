//! 紫垣命令行：用户扮演皇帝，改制/任免为显式用户操作。
use std::env;
use std::error::Error;
use std::process::ExitCode;
use ziyuan_core::{AgentId, CourtEvent, Department, GovernanceMode, Office};
use ziyuan_governance::{Court, founding_events};
use ziyuan_runtime::{DemoBackend, dispatch};
use ziyuan_storage::EventLog;

const HELP: &str = r#"紫垣 ZiYuan 0.1 — 朝廷式多 Agent 框架（演示版）

用法：
  ziyuan init                          初始化朝廷（在当前目录建立 .ziyuan/）
  ziyuan court                         查看制度与朝廷状态
  ziyuan officials                     查看官员、官职及任务完成数
  ziyuan recruit <姓名> <部门>          招募新官（须另行 appoint 任职）
  ziyuan edict [--dept works] <内容>    下诏（Worker 为模拟后端）
  ziyuan reform <chancellor|direct|cabinet>
  ziyuan dismiss <官员ID>               罢免现任官职
  ziyuan appoint <官员ID> <官职>         任命（如 first-secretary / minister:works）
  ziyuan examine <官员ID>               吏部基础考绩：完成任务数
  ziyuan history                       查看事件记录
  ziyuan help

默认官职：chancellor、first-secretary、minister:works、clerk:personnel 等
部门：personnel / revenue / rites / war / justice / works

示例：
  ziyuan init
  ziyuan edict --dept works "检查 Rust 项目的 unsafe 代码"
  ziyuan reform direct
  ziyuan edict --dept works "重新检查"
  ziyuan reform cabinet
  ziyuan dismiss 6
  ziyuan appoint 6 first-secretary
  ziyuan edict --dept works "请内阁协调检查"
"#;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("错误：{error}");
            ExitCode::FAILURE
        }
    }
}
fn run() -> Result<(), Box<dyn Error>> {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || args[0] == "help" || args[0] == "--help" {
        println!("{HELP}");
        return Ok(());
    }
    let command = args.remove(0);
    let root = env::current_dir().map_err(|e| e.to_string())?;
    let log = EventLog::new(&root);
    if command == "init" {
        if log.exists() {
            return Err("朝廷已存在，拒绝覆盖日志".into());
        }
        log.append(&founding_events())?;
        println!("紫垣已建立。当前制度：丞相制。请执行 `ziyuan court`。");
        return Ok(());
    }
    if !log.exists() {
        return Err("朝廷尚未建立，请先在项目目录执行 `ziyuan init`".into());
    }
    let events = log.load()?;
    let mut court = Court::replay(&events)?;
    match command.as_str() {
        "court" => {
            ensure_no_args(&args)?;
            println!("朝廷制度：{} ({})", court.mode.label(), court.mode);
            println!(
                "在册官员：{}；有职官员：{}；已完成任务：{}",
                court.agents.len(),
                court.appointments.len(),
                court.completions.len()
            );
            println!(
                "调度中枢：{}",
                match court.mode {
                    GovernanceMode::Chancellor => "丞相",
                    GovernanceMode::DirectRule => "皇帝直达六部",
                    GovernanceMode::Cabinet => "首辅",
                }
            );
        }
        "officials" => {
            ensure_no_args(&args)?;
            for (id, agent) in &court.agents {
                let office = court
                    .appointments
                    .get(id)
                    .map(|o| o.label())
                    .unwrap_or("未任官职".into());
                println!(
                    "#{:<3} {:<8} {:<12} {} (完成 {} 件)",
                    id.0,
                    agent.name,
                    office,
                    agent.speciality.label(),
                    court.recorded_completions(*id)
                );
            }
        }
        "recruit" => {
            if args.len() != 2 {
                return Err("用法: ziyuan recruit <姓名> <部门>".into());
            }
            let event =
                court.propose_recruitment(args[0].clone(), args[1].parse::<Department>()?)?;
            let agent_id = match &event {
                CourtEvent::AgentRegistered(a) => a.id,
                _ => unreachable!(),
            };
            log.append(&[event])?;
            println!(
                "诏令已颁：招募官员 #{} {}（未授官职）。",
                agent_id.0, args[0]
            );
        }
        "edict" => {
            let (department, text) = parse_edict(args)?;
            let edict = court.new_edict(department, text)?;
            // 所有路由检查先于持久化；模拟后端不能执行真实修改。
            let report = dispatch(&court, &edict, &DemoBackend)?;
            let changes = [
                CourtEvent::EdictIssued(edict.clone()),
                CourtEvent::TaskCompleted {
                    task: edict.id,
                    worker: report.worker,
                    summary: report.summary.clone(),
                },
            ];
            log.append(&changes)?;
            println!("诏令 #{}：{}", edict.id.0, edict.text);
            println!("路由：{}", report.path.join(" → "));
            println!("回奏：{}", report.summary);
        }
        "reform" => {
            if args.len() != 1 {
                return Err("用法: ziyuan reform <chancellor|direct|cabinet>".into());
            }
            let mode = args[0].parse::<GovernanceMode>()?;
            let event = court.propose_reform(mode)?;
            log.append(std::slice::from_ref(&event))?;
            court.apply(&event)?;
            println!(
                "诏令已颁：制度改为{}。原丞相/首辅职务自动撤销。",
                court.mode.label()
            );
            if mode != GovernanceMode::DirectRule {
                println!("提示：新制度如无协调者，请先任命。 ");
            }
        }
        "dismiss" => {
            let id = parse_id(&args, "ziyuan dismiss <官员ID>")?;
            let event = court.propose_dismissal(id)?;
            log.append(&[event])?;
            println!("诏令已颁：官员 #{} 已罢免现职（身份与履历保留）。", id.0);
        }
        "appoint" => {
            if args.len() != 2 {
                return Err("用法: ziyuan appoint <官员ID> <官职>".into());
            }
            let id = AgentId(args[0].parse().map_err(|_| "官员 ID 必须是数字")?);
            let office = args[1].parse::<Office>()?;
            let event = court.propose_appointment(id, office)?;
            log.append(&[event])?;
            println!("诏令已颁：任命官员 #{} 为{}。", id.0, office.label());
        }
        "examine" => {
            let id = parse_id(&args, "ziyuan examine <官员ID>")?;
            let agent = court.agents.get(&id).ok_or("官员不存在")?;
            println!(
                "吏部简报：{}（#{}）已记录 {} 件完成任务。",
                agent.name,
                id.0,
                court.recorded_completions(id)
            );
            println!(
                "注意：当前只是完成次数，不等于实际能力、正确率或晋升资格。模拟后端未验证结果。 "
            );
        }
        "history" => {
            ensure_no_args(&args)?;
            for (i, event) in events.iter().enumerate() {
                println!("{:>3}: {}", i + 1, describe_event(event));
            }
        }
        _ => return Err(format!("未知命令：{command}。运行 ziyuan help 查看帮助").into()),
    }
    Ok(())
}
fn ensure_no_args(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err("该命令不接受额外参数".into())
    }
}
fn parse_id(args: &[String], usage: &str) -> Result<AgentId, Box<dyn Error>> {
    if args.len() != 1 {
        return Err(format!("用法: {usage}").into());
    }
    Ok(AgentId(
        args[0].parse::<u64>().map_err(|_| "官员 ID 必须是数字")?,
    ))
}
fn parse_edict(mut args: Vec<String>) -> Result<(Department, String), Box<dyn Error>> {
    let department = if args.first().map(String::as_str) == Some("--dept") {
        if args.len() < 3 {
            return Err("用法: ziyuan edict --dept works <诏令内容>".into());
        }
        args.remove(0);
        args.remove(0).parse::<Department>()?
    } else {
        let combined = args.join(" ").to_lowercase();
        if combined.contains("任免") || combined.contains("官员") || combined.contains("考绩")
        {
            Department::Personnel
        } else {
            Department::Works
        }
    };
    Ok((department, args.join(" ")))
}
fn describe_event(event: &CourtEvent) -> String {
    match event {
        CourtEvent::Founded(m) => format!("开国：{}", m.label()),
        CourtEvent::AgentRegistered(a) => format!("登记官员 #{} {}", a.id.0, a.name),
        CourtEvent::OfficialAppointed { agent, office } => {
            format!("任命 #{} 为{}", agent.0, office.label())
        }
        CourtEvent::OfficialDismissed { agent } => format!("罢免官员 #{}", agent.0),
        CourtEvent::InstitutionReformed(m) => format!("改制：{}", m.label()),
        CourtEvent::EdictIssued(e) => format!("诏令 #{}：{}", e.id.0, e.text),
        CourtEvent::TaskCompleted { task, worker, .. } => {
            format!("诏令 #{} 由 #{} 回奏", task.0, worker.0)
        }
    }
}
