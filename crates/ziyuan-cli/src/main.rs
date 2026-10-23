//! 紫垣命令行：用户扮演皇帝，改制/任免为显式用户操作。
//!
//! 本模块只负责解析参数与呈现结果；用例流程在 `ziyuan-app::Session` 中。
use std::env;
use std::error::Error;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use ziyuan_app::{Session, TaskReport};
use ziyuan_core::{AgentId, CourtEvent, Department, GovernanceMode, Office, TaskId};
use ziyuan_governance::TaskStatus;
use ziyuan_llm::OpenAiCompatible;
use ziyuan_runtime::{
    AgentBackend, CancelToken, DemoBackend, Limits, Outcome, Scheduler, WorkerBackend,
    tools::read_only_tools,
};
use ziyuan_storage::{EventLog, ExecutorLock};

const HELP: &str = r#"紫垣 ZiYuan 0.1 — 朝廷式多 Agent 框架（演示版）

用法：
  ziyuan init                          初始化朝廷（在当前目录建立 .ziyuan/）
  ziyuan court                         查看制度与朝廷状态
  ziyuan officials                     查看官员、官职及任务完成数
  ziyuan recruit <姓名> <部门>          招募新官（须另行 appoint 任职）
  ziyuan edict [--dept works] [--queue] <内容>
                                       下诏并立即执行；--queue 只入队（默认模拟后端；见下方环境变量）
  ziyuan run [--jobs 4] [--timeout 300]
                                       并发执行全部未结束的诏令；Ctrl-C 撤销执行中的诏令
  ziyuan tasks                         查看诏令及其状态
  ziyuan cancel <诏令ID>                撤销未结束的诏令
  ziyuan reform <chancellor|direct|cabinet>
  ziyuan dismiss <官员ID>               罢免现任官职
  ziyuan appoint <官员ID> <官职>         任命（如 first-secretary / minister:works）
  ziyuan examine <官员ID>               吏部基础考绩：完成任务数
  ziyuan history                       查看事件记录
  ziyuan help

默认官职：chancellor、first-secretary、minister:works、clerk:personnel 等
部门：personnel / revenue / rites / war / justice / works

真实模型（OpenAI 兼容 Chat Completions API）：
  ZIYUAN_LLM_MODEL     模型名；设置后 edict 由模型以只读工具查阅当前目录
  ZIYUAN_LLM_BASE_URL  默认 https://api.openai.com/v1（如 https://api.deepseek.com/v1、http://localhost:11434/v1）
  ZIYUAN_LLM_API_KEY   API Key（本地 Ollama 可不设）

示例：
  ziyuan init
  ziyuan edict --dept works "检查 Rust 项目的 unsafe 代码"
  ziyuan reform direct
  ziyuan edict --dept works "重新检查"
  ziyuan reform cabinet
  ziyuan dismiss 6
  ziyuan appoint 6 first-secretary
  ziyuan edict --dept works "请内阁协调检查"
  ziyuan edict --queue "检查 storage crate"
  ziyuan edict --queue "检查 runtime crate"
  ziyuan run --jobs 2 --timeout 120
"#;

/// 单件诏令最多的模型调用轮数。
const MAX_AGENT_STEPS: usize = 12;
const DEFAULT_LIMITS: Limits = Limits {
    max_concurrent: 4,
    timeout: Duration::from_secs(300),
};

enum Command {
    Help,
    Init,
    Court,
    Officials,
    Recruit(String, Department),
    Edict {
        department: Department,
        text: String,
        queue: bool,
    },
    Run(Limits),
    Tasks,
    Cancel(TaskId),
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
        "edict" => parse_edict(args)?,
        "run" => Command::Run(parse_limits(args)?),
        "tasks" => no_args(args, Command::Tasks)?,
        "cancel" => match <[String; 1]>::try_from(args) {
            Ok([id]) => Command::Cancel(TaskId(id.parse().map_err(|_| "诏令 ID 必须是数字")?)),
            Err(_) => return Err("用法: ziyuan cancel <诏令ID>".into()),
        },
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
        Command::Edict {
            department,
            text,
            queue,
        } => {
            // 先取得执行者锁再下诏，避免被拒绝的诏令残留在队列中。
            let executor = if queue { None } else { Some(lock_executor()?) };
            let edict = session.issue(department, text)?;
            if queue {
                println!("诏令 #{} 已入队：{}", edict.id.0, edict.text);
            } else {
                let limits = Limits {
                    max_concurrent: 1,
                    ..DEFAULT_LIMITS
                };
                run_tasks(session, &[edict.id], limits)?;
            }
            drop(executor);
        }
        Command::Run(limits) => {
            let _executor = lock_executor()?;
            let tasks = session.court().open_tasks();
            if tasks.is_empty() {
                println!("没有未结束的诏令。");
            } else {
                run_tasks(session, &tasks, limits)?;
            }
        }
        Command::Tasks => {
            let court = session.court();
            for task in court.tasks() {
                let name = |id: AgentId| court.agent(id).map_or("?", |a| a.name.as_str());
                let status = match &task.status {
                    TaskStatus::Pending => "待办".to_owned(),
                    TaskStatus::Assigned(w) => format!("已指派 {}（未结束）", name(*w)),
                    TaskStatus::Completed { worker, .. } => format!("已完成（{}）", name(*worker)),
                    TaskStatus::Failed { reason } => format!("失败：{reason}"),
                    TaskStatus::Cancelled => "已撤销".to_owned(),
                };
                println!(
                    "#{:<3} {} {}  —  {}",
                    task.edict.id.0,
                    task.edict.department.label(),
                    task.edict.text,
                    status
                );
            }
        }
        Command::Cancel(task) => {
            session.cancel(task)?;
            println!("诏令 #{} 已撤销。", task.0);
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

/// 同一朝廷同一时刻只有一个进程执行诏令；入队、撤销、任免等不受限制。
fn lock_executor() -> Result<ExecutorLock, Box<dyn Error>> {
    EventLog::new(&env::current_dir()?)
        .try_lock_executor()?
        .ok_or_else(|| {
            "另一个 ziyuan 进程正在执行诏令。可用 `edict --queue` 入队，待其结束后 `ziyuan run`。"
                .into()
        })
}

/// 在 Tokio 上执行 `tasks` 并逐件打印结局；有未完成的诏令时返回错误。
fn run_tasks(
    session: &mut Session<EventLog>,
    tasks: &[TaskId],
    limits: Limits,
) -> Result<(), Box<dyn Error>> {
    let scheduler = Scheduler::new(worker_backend()?, limits);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let mut unfinished = 0;
    let result = runtime.block_on(async {
        let cancel = CancelToken::new();
        let on_interrupt = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("收到中断：撤销执行中的诏令，排队中的保留。");
                on_interrupt.cancel();
            }
        });
        session
            .run(tasks, &scheduler, &cancel, |report| {
                if !matches!(report.outcome, Outcome::Completed(_)) {
                    unfinished += 1;
                }
                print_report(report);
            })
            .await
    });
    // 超时或撤销后，阻塞中的模型请求可能仍未返回；不等待它们。
    runtime.shutdown_background();
    result?;
    if unfinished > 0 {
        return Err(format!("{unfinished} 件诏令未完成").into());
    }
    Ok(())
}

fn print_report(report: &TaskReport) {
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
}

/// 设置了 `ZIYUAN_LLM_MODEL` 时使用真实模型与只读工具（以当前目录为项目根），否则使用演示后端。
fn worker_backend() -> Result<Arc<dyn WorkerBackend>, Box<dyn Error>> {
    let Some(model) = OpenAiCompatible::from_env() else {
        return Ok(Arc::new(DemoBackend));
    };
    eprintln!(
        "执行后端：{}（只读工具：read_file、search_code）",
        model.model()
    );
    let tools = read_only_tools(&env::current_dir()?)?;
    Ok(Arc::new(AgentBackend::new(
        Box::new(model),
        tools,
        MAX_AGENT_STEPS,
    )))
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

fn parse_limits(args: Vec<String>) -> Result<Limits, Box<dyn Error>> {
    const USAGE: &str = "用法: ziyuan run [--jobs <并发数>] [--timeout <秒>]（均须为正整数）";
    let mut limits = DEFAULT_LIMITS;
    for pair in args.chunks(2) {
        let [flag, value] = pair else {
            return Err(USAGE.into());
        };
        let value = value.parse::<u64>().ok().filter(|v| *v > 0).ok_or(USAGE)?;
        match flag.as_str() {
            "--jobs" => limits.max_concurrent = value as usize,
            "--timeout" => limits.timeout = Duration::from_secs(value),
            _ => return Err(USAGE.into()),
        }
    }
    Ok(limits)
}

fn parse_edict(mut args: Vec<String>) -> Result<Command, Box<dyn Error>> {
    const USAGE: &str = "用法: ziyuan edict [--dept works] [--queue] <诏令内容>";
    let mut department = None;
    let mut queue = false;
    loop {
        match args.first().map(String::as_str) {
            Some("--queue") => {
                args.remove(0);
                queue = true;
            }
            Some("--dept") if args.len() >= 2 => {
                args.remove(0);
                department = Some(args.remove(0).parse::<Department>()?);
            }
            Some("--dept") => return Err(USAGE.into()),
            _ => break,
        }
    }
    if args.is_empty() {
        return Err(USAGE.into());
    }
    let department = if let Some(department) = department {
        department
    } else {
        let combined = args.join(" ").to_lowercase();
        if combined.contains("任免") || combined.contains("官员") || combined.contains("考绩")
        {
            Department::Personnel
        } else {
            Department::Works
        }
    };
    Ok(Command::Edict {
        department,
        text: args.join(" "),
        queue,
    })
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
        CourtEvent::TaskAssigned { task, worker } => {
            format!("诏令 #{} 派给 #{}", task.0, worker.0)
        }
        CourtEvent::ToolInvoked {
            task,
            tool,
            args_json,
            ok,
        } => format!(
            "诏令 #{} 调用 {tool} {args_json}（{}）",
            task.0,
            if *ok { "成功" } else { "失败" }
        ),
        CourtEvent::TaskCompleted { task, worker, .. } => {
            format!("诏令 #{} 由 #{} 回奏", task.0, worker.0)
        }
        CourtEvent::TaskFailed { task, reason } => format!("诏令 #{} 失败：{reason}", task.0),
        CourtEvent::TaskCancelled { task } => format!("诏令 #{} 撤销", task.0),
    }
}
