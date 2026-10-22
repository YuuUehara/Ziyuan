//! 紫垣命令行：用户扮演皇帝，改制/任免为显式用户操作。
//!
//! 本模块只负责解析参数与呈现结果；用例流程在 `ziyuan-app::Session` 中。
use std::env;
use std::error::Error;
use std::process::ExitCode;
use ziyuan_app::Session;
use ziyuan_core::{AgentId, CourtEvent, Department, GovernanceMode, Office};
use ziyuan_runtime::DemoBackend;
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

enum Command {
    Help,
    Init,
    Court,
    Officials,
    Recruit(String, Department),
    Edict(Department, String),
    Reform(GovernanceMode),
    Dismiss(AgentId),
    Appoint(AgentId, Office),
    Examine(AgentId),
    History,
}

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
    let command = parse(env::args().skip(1).collect())?;
    let log = EventLog::new(&env::current_dir()?);
    match command {
        Command::Help => println!("{HELP}"),
        Command::Init => {
            Session::found(log)?;
            println!("紫垣已建立。当前制度：丞相制。请执行 `ziyuan court`。");
        }
        command => execute(&mut Session::open(log)?, command)?,
    }
    Ok(())
}

fn parse(mut args: Vec<String>) -> Result<Command, Box<dyn Error>> {
    if args.is_empty() {
        return Ok(Command::Help);
    }
    let name = args.remove(0);
    let command = match name.as_str() {
        "help" | "--help" => Command::Help,
        "init" => no_args(args, Command::Init)?,
        "court" => no_args(args, Command::Court)?,
        "officials" => no_args(args, Command::Officials)?,
        "history" => no_args(args, Command::History)?,
        "recruit" => match <[String; 2]>::try_from(args) {
            Ok([name, dept]) => Command::Recruit(name, dept.parse()?),
            Err(_) => return Err("用法: ziyuan recruit <姓名> <部门>".into()),
        },
        "edict" => {
            let (department, text) = parse_edict(args)?;
            Command::Edict(department, text)
        }
        "reform" => match <[String; 1]>::try_from(args) {
            Ok([mode]) => Command::Reform(mode.parse()?),
            Err(_) => return Err("用法: ziyuan reform <chancellor|direct|cabinet>".into()),
        },
        "dismiss" => Command::Dismiss(parse_id(&args, "ziyuan dismiss <官员ID>")?),
        "appoint" => match <[String; 2]>::try_from(args) {
            Ok([id, office]) => Command::Appoint(agent_id(&id)?, office.parse()?),
            Err(_) => return Err("用法: ziyuan appoint <官员ID> <官职>".into()),
        },
        "examine" => Command::Examine(parse_id(&args, "ziyuan examine <官员ID>")?),
        _ => return Err(format!("未知命令：{name}。运行 ziyuan help 查看帮助").into()),
    };
    Ok(command)
}

fn execute(session: &mut Session<EventLog>, command: Command) -> Result<(), Box<dyn Error>> {
    match command {
        Command::Help | Command::Init => unreachable!("在 run 中处理"),
        Command::Court => {
            let court = session.court();
            println!("朝廷制度：{} ({})", court.mode().label(), court.mode());
            println!(
                "在册官员：{}；有职官员：{}；已完成任务：{}",
                court.agents().count(),
                court.appointed_count(),
                court.completed_count()
            );
            println!(
                "调度中枢：{}",
                match court.mode() {
                    GovernanceMode::Chancellor => "丞相",
                    GovernanceMode::DirectRule => "皇帝直达六部",
                    GovernanceMode::Cabinet => "首辅",
                }
            );
        }
        Command::Officials => {
            let court = session.court();
            for agent in court.agents() {
                let office = court
                    .office_of(agent.id)
                    .map(|o| o.label())
                    .unwrap_or("未任官职".into());
                println!(
                    "#{:<3} {:<8} {:<12} {} (完成 {} 件)",
                    agent.id.0,
                    agent.name,
                    office,
                    agent.speciality.label(),
                    court.recorded_completions(agent.id)
                );
            }
        }
        Command::Recruit(name, department) => {
            let id = session.recruit(name.clone(), department)?;
            println!("诏令已颁：招募官员 #{} {}（未授官职）。", id.0, name);
        }
        Command::Edict(department, text) => {
            // 模拟后端不能执行真实修改。
            let (edict, report) = session.issue_edict(department, text, &DemoBackend)?;
            println!("诏令 #{}：{}", edict.id.0, edict.text);
            println!("路由：{}", report.path.join(" → "));
            println!("回奏：{}", report.summary);
        }
        Command::Reform(mode) => {
            session.reform(mode)?;
            println!(
                "诏令已颁：制度改为{}。原丞相/首辅职务自动撤销。",
                mode.label()
            );
            if mode != GovernanceMode::DirectRule {
                println!("提示：新制度如无协调者，请先任命。 ");
            }
        }
        Command::Dismiss(id) => {
            session.dismiss(id)?;
            println!("诏令已颁：官员 #{} 已罢免现职（身份与履历保留）。", id.0);
        }
        Command::Appoint(id, office) => {
            session.appoint(id, office)?;
            println!("诏令已颁：任命官员 #{} 为{}。", id.0, office.label());
        }
        Command::Examine(id) => {
            let court = session.court();
            let agent = court.agent(id).ok_or("官员不存在")?;
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
        Command::History => {
            for (i, event) in session.history().iter().enumerate() {
                println!("{:>3}: {}", i + 1, describe_event(event));
            }
        }
    }
    Ok(())
}

fn no_args(args: Vec<String>, command: Command) -> Result<Command, Box<dyn Error>> {
    if args.is_empty() {
        Ok(command)
    } else {
        Err("该命令不接受额外参数".into())
    }
}

fn agent_id(input: &str) -> Result<AgentId, Box<dyn Error>> {
    Ok(AgentId(input.parse().map_err(|_| "官员 ID 必须是数字")?))
}

fn parse_id(args: &[String], usage: &str) -> Result<AgentId, Box<dyn Error>> {
    match args {
        [id] => agent_id(id),
        _ => Err(format!("用法: {usage}").into()),
    }
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
