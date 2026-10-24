//! 紫垣命令行：用户扮演皇帝，改制/任免为显式用户操作。
//!
//! 本模块只负责解析参数与呈现结果；用例流程在 `ziyuan-app::Session` 中。
use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use ziyuan_app::{
    ABOLITION_NOTE, RunUpdate, Session, TaskReport, describe_event, describe_record, estimate_cost,
    kaocheng_grade, kaocheng_report, render_diff, seal_note,
};
use ziyuan_core::{
    AgentId, Capability, Department, GovernanceMode, Office, Persona, Seal, TaskId, Verdict,
};
use ziyuan_governance::{ChangeState, Task, TaskStatus};
use ziyuan_llm::{OpenAiCompatible, price_for};
use ziyuan_runtime::{
    AgentBackend, CancelToken, DemoBackend, Limits, Outcome, Scheduler, WorkerBackend,
};
use ziyuan_storage::{EventLog, ExecutorLock};

const HELP: &str = concat!(
    "紫垣 ZiYuan ",
    env!("CARGO_PKG_VERSION"),
    r#" — 朝廷式多 Agent 框架（演示版）

用法：
  ziyuan init                          初始化朝廷（在当前目录建立 .ziyuan/）
  ziyuan court                         查看制度与朝廷状态
  ziyuan officials                     查看官员、官职、品级、所用模型及任务完成数
  ziyuan recruit [姓名] <部门>          招募新官（不给姓名则随机取名；须另行 appoint 任职）
  ziyuan edict [--dept works] [--scope <路径>]... [--tools <工具>] [--queue] <内容>
                                       下诏并立即执行；--queue 只入队（默认模拟后端；见下方环境变量）
  ziyuan run [--jobs 4] [--timeout 300]
                                       并发执行全部未结束的诏令；Ctrl-C 撤销执行中的诏令
  ziyuan tasks                         查看诏令及其状态
  ziyuan serve [--port 8080] [--bind 127.0.0.1] [--new-token]
                                       启动紫禁城 Web 界面（默认仅本机；令牌存于 .ziyuan/web-token）
  ziyuan tui                           启动紫禁城终端界面
  ziyuan cancel <诏令ID>                撤销未结束的诏令
  ziyuan reform <chancellor|direct|cabinet>
  ziyuan retreat                       修道怠政（昏君模式）：批红、评定由司礼监代批，记入事件簿
  ziyuan return                        回朝亲政
  ziyuan dismiss <官员ID>               罢免现任官职
  ziyuan purge <官员ID>                 革职：削去仕籍，永不叙用
  ziyuan abolish                       一键朱元璋：罢中书省，株连百官（全部在任官员革职削籍）
  ziyuan model <官职> <模型|default>     为官职指定承办模型（同一服务地址与 Key）
  ziyuan ceiling <官职> [--scope <路径>]... [--tools <工具>] | default
                                       设定官职的玉玺上限：该职承办时，诏令玉玺与上限取交集
  ziyuan split <诏令ID> [官员ID:子诏令]...
                                       多官协作：尚书（模型）把待办诏令拆给本部官员分办，或皇帝御定；
                                       子诏令并发执行，全部结束后尚书汇总回奏
  ziyuan debate <诏令ID> [--rounds 2] [官员ID]...
                                       廷议：数位在任官员（默认宰辅、本部长官、御史）按各自性格文风
                                       就待办诏令轮流辩论；纪要供票拟与承办参考，不改变派工与玉玺
  ziyuan draft <诏令ID|all>             内阁票拟：首辅（模型）拟定承办人并附意见
  ziyuan redraft <诏令ID> <官员ID> [理由] 御笔改票：改定拟派的承办人
  ziyuan approve <诏令ID|all>           内阁制下批红（批红后方可执行；未票拟即批红为中旨，按旧例派工）
  ziyuan appoint <官员ID> <官职>         任命（如 first-secretary / minister:works）
  ziyuan diff <诏令ID>                  查看工部呈报的改动（统一 diff）
  ziyuan apply <诏令ID>                 准奏：把呈报的改动写入项目（文件在此期间被改过则拒绝）
  ziyuan discard <诏令ID>               驳回呈报的改动，不写入
  ziyuan review <诏令ID> <accept|reject> [备注]
                                       评定已完成的诏令（吏部考绩只采信评定）
  ziyuan censor <诏令ID|all>            都察院复核回奏：核验引用与测试，御史背书或弹劾
  ziyuan kaocheng                      考成簿：在任官员的功过与等第（仿张居正考成法）
  ziyuan examine <官员ID>               吏部考绩：经评定与复核的履历
  ziyuan recommend <官职>               吏部荐举：按履历排列可任该职者（不自动任命）
  ziyuan history                       查看事件记录
  ziyuan help

官制（品级随制度而变）：
  丞相制（仿元·中书省）  chancellor 中书右丞相、vice-chancellor 中书左丞相
  废相亲政（仿明初）    不设宰辅，皇帝直领六部
  内阁制（仿明中后期）  first-secretary 内阁首辅、second-secretary 内阁次辅
  六部（各制度通用）    minister:<部门> 尚书、vice-minister:<部门> 侍郎、clerk:<部门> 郎中
  都察院（各制度通用）  censor 左都御史（元称御史台御史大夫），不入派工路线
  正职空缺时由副职署理。
部门：personnel / revenue / rites / war / justice / works

玉玺（承办官员的权限，随诏令记入事件簿）：
  --scope <路径>   可查阅的项目路径（目录或文件，可重复）；默认整个项目
  --tools <工具>   read_file、search_code、write_file、run_tests 的逗号列表，或 none；
                   默认 read_file,search_code（只读）
  write_file、run_tests 只能授予工部：改动只暂存，诏令完成后呈报，须 `ziyuan apply` 准奏才落盘；
  run_tests 在项目的临时副本（含暂存改动）中运行 ZIYUAN_TEST_COMMAND，不传 API Key 等环境变量。
  无论玉玺如何，.ziyuan、.git、.jj 与 .env* 均不可读写。

真实模型（OpenAI 兼容 Chat Completions API）：
  ZIYUAN_LLM_MODEL     模型名；设置后 edict 由模型以只读工具查阅当前目录
  ZIYUAN_LLM_BASE_URL  默认 https://api.openai.com/v1（如 https://api.deepseek.com/v1、http://localhost:11434/v1）
  ZIYUAN_LLM_API_KEY   API Key（本地 Ollama 可不设）
  ZIYUAN_TEST_COMMAND  run_tests 运行的测试命令（如 cargo test；由皇帝设定，模型不能更改）
  ZIYUAN_LLM_PRICE     估算花费用的单价「输入,输出[,币种]」（每百万 token）；
                       deepseek-flash / deepseek-v4-pro 内置官方高峰价（估算上限）

示例：
  ziyuan init
  ziyuan edict --dept works "检查 Rust 项目的 unsafe 代码"
  ziyuan reform direct
  ziyuan edict --dept works "重新检查"
  ziyuan reform cabinet
  ziyuan dismiss 6
  ziyuan appoint 6 first-secretary
  ziyuan edict --dept works "请内阁协调检查"
  ziyuan edict --queue --scope crates/ziyuan-storage --tools search_code "检查 storage crate"
  ziyuan edict --queue "检查 runtime crate"
  ziyuan run --jobs 2 --timeout 120
"#
);

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
    Recruit(Option<String>, Department),
    Edict {
        department: Department,
        text: String,
        queue: bool,
        seal: Seal,
    },
    Run(Limits),
    Tasks,
    Serve {
        port: u16,
        bind: std::net::IpAddr,
        /// 重新生成令牌（旧令牌作废）。
        rotate: bool,
    },
    Tui,
    Cancel(TaskId),
    Reform(GovernanceMode),
    /// `true` 修道，`false` 回朝。
    Retreat(bool),
    Dismiss(AgentId),
    Purge(AgentId),
    /// `None` 恢复默认模型。
    Model(Office, Option<String>),
    /// `None` 取消上限。
    Ceiling(Office, Option<Seal>),
    Abolish,
    /// `None` 表示全部待批红的诏令。
    Approve(Option<TaskId>),
    /// `None` 表示全部待批红且未票拟的诏令。
    Draft(Option<TaskId>),
    Redraft(TaskId, AgentId, String),
    Appoint(AgentId, Office),
    Examine(AgentId),
    Review(TaskId, Verdict, String),
    Diff(TaskId),
    /// 空列表表示由尚书拆分。
    Split(TaskId, Vec<(AgentId, String)>),
    /// 诏令、与议官员（空为默认人选）、轮数。
    Debate(TaskId, Vec<AgentId>, usize),
    /// `None` 表示全部已完成、未复核的诏令。
    Censor(Option<TaskId>),
    Kaocheng,
    /// 准奏（`true`）或驳回改动。
    Decide(TaskId, bool),
    Recommend(Office),
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
        "recruit" => match args.as_slice() {
            [dept] => Command::Recruit(None, dept.parse()?),
            [name, dept] => Command::Recruit(Some(name.clone()), dept.parse()?),
            _ => return Err("用法: ziyuan recruit [姓名] <部门>".into()),
        },
        "edict" => parse_edict(args)?,
        "run" => Command::Run(parse_limits(args)?),
        "tasks" => no_args(args, Command::Tasks)?,
        "tui" => no_args(args, Command::Tui)?,
        "serve" => {
            const USAGE: &str = "用法: ziyuan serve [--port 8080] [--bind 127.0.0.1] [--new-token]";
            let (mut port, mut bind, mut rotate) =
                (8080, std::net::Ipv4Addr::LOCALHOST.into(), false);
            let mut rest = args.iter();
            while let Some(flag) = rest.next() {
                match flag.as_str() {
                    "--port" => {
                        let value = rest.next().ok_or(USAGE)?;
                        port = value.parse().map_err(|_| "端口必须是 0–65535 的数字")?;
                    }
                    "--bind" => {
                        let value = rest.next().ok_or(USAGE)?;
                        bind = value
                            .parse()
                            .map_err(|_| "--bind 须为 IP 地址，如 0.0.0.0")?;
                    }
                    "--new-token" => rotate = true,
                    _ => return Err(USAGE.into()),
                }
            }
            Command::Serve { port, bind, rotate }
        }
        "cancel" => match <[String; 1]>::try_from(args) {
            Ok([id]) => Command::Cancel(TaskId(id.parse().map_err(|_| "诏令 ID 必须是数字")?)),
            Err(_) => return Err("用法: ziyuan cancel <诏令ID>".into()),
        },
        "retreat" => no_args(args, Command::Retreat(true))?,
        "return" => no_args(args, Command::Retreat(false))?,
        "reform" => match <[String; 1]>::try_from(args) {
            Ok([mode]) => Command::Reform(mode.parse()?),
            Err(_) => return Err("用法: ziyuan reform <chancellor|direct|cabinet>".into()),
        },
        "dismiss" => Command::Dismiss(parse_id(&args, "ziyuan dismiss <官员ID>")?),
        "purge" => Command::Purge(parse_id(&args, "ziyuan purge <官员ID>")?),
        "abolish" => no_args(args, Command::Abolish)?,
        "model" => match <[String; 2]>::try_from(args) {
            Ok([office, model]) => {
                Command::Model(office.parse()?, (model != "default").then_some(model))
            }
            Err(_) => return Err("用法: ziyuan model <官职> <模型|default>".into()),
        },
        "ceiling" => parse_ceiling(args)?,
        "draft" => match args.as_slice() {
            [all] if all == "all" => Command::Draft(None),
            [id] => Command::Draft(Some(TaskId(
                id.parse().map_err(|_| "诏令 ID 必须是数字或 all")?,
            ))),
            _ => return Err("用法: ziyuan draft <诏令ID|all>".into()),
        },
        "redraft" => {
            if args.len() < 2 {
                return Err("用法: ziyuan redraft <诏令ID> <官员ID> [理由]".into());
            }
            let task = TaskId(args[0].parse().map_err(|_| "诏令 ID 必须是数字")?);
            Command::Redraft(task, agent_id(&args[1])?, args[2..].join(" "))
        }
        "approve" => match args.as_slice() {
            [all] if all == "all" => Command::Approve(None),
            [id] => Command::Approve(Some(TaskId(
                id.parse().map_err(|_| "诏令 ID 必须是数字或 all")?,
            ))),
            _ => return Err("用法: ziyuan approve <诏令ID|all>".into()),
        },
        "appoint" => match <[String; 2]>::try_from(args) {
            Ok([id, office]) => Command::Appoint(agent_id(&id)?, office.parse()?),
            Err(_) => return Err("用法: ziyuan appoint <官员ID> <官职>".into()),
        },
        "examine" => Command::Examine(parse_id(&args, "ziyuan examine <官员ID>")?),
        "diff" => Command::Diff(task_id(&args, "ziyuan diff <诏令ID>")?),
        "kaocheng" => no_args(args, Command::Kaocheng)?,
        "split" => {
            const USAGE: &str = "用法: ziyuan split <诏令ID> [官员ID:子诏令]...";
            let (id, parts) = args.split_first().ok_or(USAGE)?;
            let task = TaskId(id.parse().map_err(|_| USAGE)?);
            let plan = parts
                .iter()
                .map(|part| {
                    let (agent, text) = part.split_once([':', '：']).ok_or(USAGE)?;
                    Ok((agent_id(agent)?, text.to_owned()))
                })
                .collect::<Result<_, Box<dyn Error>>>()?;
            Command::Split(task, plan)
        }
        "debate" => {
            const USAGE: &str = "用法: ziyuan debate <诏令ID> [--rounds 1-3] [官员ID]...";
            let (id, rest) = args.split_first().ok_or(USAGE)?;
            let task = TaskId(id.parse().map_err(|_| USAGE)?);
            let mut rounds = 2;
            let mut members = Vec::new();
            let mut rest = rest.iter();
            while let Some(arg) = rest.next() {
                if arg == "--rounds" {
                    let n = rest.next().and_then(|n| n.parse().ok());
                    rounds = n.filter(|n| (1..=3).contains(n)).ok_or(USAGE)?;
                } else {
                    members.push(agent_id(arg)?);
                }
            }
            Command::Debate(task, members, rounds)
        }
        "censor" => match args.as_slice() {
            [all] if all == "all" => Command::Censor(None),
            [id] => Command::Censor(Some(TaskId(
                id.parse().map_err(|_| "诏令 ID 必须是数字或 all")?,
            ))),
            _ => return Err("用法: ziyuan censor <诏令ID|all>".into()),
        },
        "apply" => Command::Decide(task_id(&args, "ziyuan apply <诏令ID>")?, true),
        "discard" => Command::Decide(task_id(&args, "ziyuan discard <诏令ID>")?, false),
        "review" => {
            if args.len() < 2 {
                return Err("用法: ziyuan review <诏令ID> <accept|reject> [备注]".into());
            }
            let task = TaskId(args[0].parse().map_err(|_| "诏令 ID 必须是数字")?);
            let verdict = args[1].parse::<Verdict>()?;
            Command::Review(task, verdict, args[2..].join(" "))
        }
        "recommend" => match <[String; 1]>::try_from(args) {
            Ok([office]) => Command::Recommend(office.parse()?),
            Err(_) => return Err("用法: ziyuan recommend <官职>".into()),
        },
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
            if court.in_retreat() {
                println!("皇帝：西苑修道中，司礼监代批（`ziyuan return` 回朝）");
            }
            println!(
                "在册官员：{}；有职官员：{}；已完成任务：{}",
                court.agents().count(),
                court.appointed_count(),
                court.completed_count()
            );
            println!("默认承办模型：{}", model_label());
            println!(
                "模型用量：{}{}",
                court.total_usage(),
                cost_note(court.tasks())
            );
            println!(
                "调度中枢：{}",
                match court.mode() {
                    GovernanceMode::Chancellor => "中书省（右丞相，左丞相署理）",
                    GovernanceMode::DirectRule => "皇帝直领六部",
                    GovernanceMode::Cabinet => "内阁（首辅，次辅署理）",
                }
            );
        }
        Command::Officials => {
            let court = session.court();
            let default = model_label();
            for agent in court.agents() {
                let model = court.model_of(agent.id).unwrap_or(&default);
                let office = match court.office_of(agent.id) {
                    Some(o) => format!("{}（{}）", o.label(), o.rank(court.mode())),
                    None if court.is_purged(agent.id) => "已革职".into(),
                    None => "未任官职".into(),
                };
                println!(
                    "#{:<3} {:<6} {:<18} {} {} 模型 {} (完成 {} 件)",
                    agent.id.0,
                    agent.name,
                    office,
                    agent.speciality.label(),
                    Persona::of(agent.id).label(),
                    model,
                    court.recorded_completions(agent.id)
                );
            }
        }
        Command::Recruit(name, department) => {
            let id = session.recruit(name, department)?;
            let name = &session.court().agent(id).expect("刚招募").name;
            println!("诏令已颁：招募官员 #{} {}（未授官职）。", id.0, name);
        }
        Command::Edict {
            department,
            text,
            queue,
            seal,
        } => {
            // 先取得执行者锁再下诏，避免被拒绝的诏令残留在队列中。
            let executor = if queue { None } else { Some(lock_executor()?) };
            let edict = session.issue(department, text, seal)?;
            let backend = if queue { None } else { Some(worker_backend()?) };
            if let Some(backend) = &backend
                && session.court().mode() == GovernanceMode::Cabinet
            {
                // 皇帝亲自下诏并立即执行：内阁票拟后当即批红。
                session.draft(edict.id, backend.as_ref())?;
                print_draft(session, edict.id);
                if session.court().in_retreat() {
                    println!("陛下修道，司礼监代批红。");
                } else {
                    session.approve(edict.id)?;
                    println!("朱笔批红。");
                }
            }
            if queue {
                println!(
                    "诏令 #{} 已入队：{}{}",
                    edict.id.0,
                    edict.text,
                    seal_note(&edict.seal)
                );
            } else {
                let limits = Limits {
                    max_concurrent: 1,
                    ..DEFAULT_LIMITS
                };
                let backend = backend.expect("立即执行时已构造后端");
                run_tasks(session, Some(&[edict.id]), backend, limits)?;
            }
            drop(executor);
        }
        Command::Run(limits) => {
            let _executor = lock_executor()?;
            let court = session.court();
            let tasks = court.runnable_tasks();
            let waiting = court.tasks().filter(|t| court.needs_approval(t)).count();
            if waiting > 0 && !court.in_retreat() {
                eprintln!(
                    "另有 {waiting} 件诏令待批红（`ziyuan approve <诏令ID|all>`），本次不执行。"
                );
            }
            // 修道期间待批红的诏令由司礼监在执行时代批红。
            if tasks.is_empty() && !(court.in_retreat() && waiting > 0) {
                println!("没有未结束的诏令。");
            } else {
                run_tasks(session, None, worker_backend()?, limits)?;
            }
        }
        Command::Serve { port, bind, rotate } => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            let root = env::current_dir()?;
            let config = ziyuan_web::Config {
                token: ziyuan_web::load_or_create_token(&root, rotate)?,
                root,
                backend: worker_backend()?,
                limits: DEFAULT_LIMITS,
            };
            runtime.block_on(async {
                let listener = tokio::net::TcpListener::bind((bind, port)).await?;
                let addr = listener.local_addr()?;
                // 监听全部地址时，地址栏要填本机在网络中的地址。
                let host = match bind.is_unspecified() {
                    true => format!("<本机地址>:{}", addr.port()),
                    false => addr.to_string(),
                };
                println!("奉天门已开：http://{host}/?token={}", config.token);
                println!("令牌存于 .ziyuan/web-token，重启不变；`--new-token` 重新生成。令牌即皇帝身份，请勿外传。");
                if !bind.is_loopback() {
                    println!(
                        "注意：正在监听 {addr}，其他机器可以访问。HTTP 为明文，令牌可能被窃听；\
                         请只在可信网络中使用，或置于 HTTPS 反向代理、SSH 隧道之后。"
                    );
                }
                ziyuan_web::serve(listener, config).await
            })?;
        }
        Command::Tui => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            let config = ziyuan_tui::Config {
                root: env::current_dir()?,
                backend: worker_backend()?,
                limits: DEFAULT_LIMITS,
            };
            runtime.block_on(ziyuan_tui::run(config))?;
            // 阻塞中的模型请求可能仍未返回；不等待它们。
            runtime.shutdown_background();
        }
        Command::Tasks => {
            let court = session.court();
            for task in court.tasks() {
                let name = |id: AgentId| court.agent(id).map_or("?", |a| a.name.as_str());
                let status = match &task.status {
                    TaskStatus::Pending if task.split.is_some() => {
                        match court.open_subtasks(task) {
                            0 => "待尚书汇总".to_owned(),
                            n => format!("分办中（{n} 件未结）"),
                        }
                    }
                    TaskStatus::Pending if court.needs_approval(task) => {
                        let label = if task.draft.is_some() {
                            "票拟"
                        } else {
                            "未票拟，旧例预拟"
                        };
                        match court.route_task(task) {
                            Ok(route) => format!("待批红（{label}：{}）", route.path.join(" → ")),
                            Err(e) => format!("待批红（票拟不成：{e}）"),
                        }
                    }
                    TaskStatus::Pending => "待办".to_owned(),
                    TaskStatus::Assigned(w) => format!("已指派 {}（未结束）", name(*w)),
                    TaskStatus::Completed { worker, .. } => {
                        let review = match &task.review {
                            Some((verdict, _)) if task.proxied => {
                                format!("司礼监代批{}", verdict.label())
                            }
                            Some((verdict, _)) => verdict.label().to_owned(),
                            None => "待评定".to_owned(),
                        };
                        let censure = task
                            .censure
                            .as_ref()
                            .map_or(String::new(), |c| format!("；御史{}", c.verdict.label()));
                        format!(
                            "已完成（{}），{review}{censure}{}",
                            name(*worker),
                            changes_note(task)
                        )
                    }
                    TaskStatus::Failed { reason, .. } => format!("失败：{reason}"),
                    TaskStatus::Cancelled => "已撤销".to_owned(),
                };
                let usage = if task.usage.total() > 0 {
                    format!("（{} tokens{}）", task.usage.total(), cost_note([task]))
                } else {
                    String::new()
                };
                let parent = task
                    .parent
                    .map_or(String::new(), |p| format!("↳ 分办自 #{} ", p.0));
                let debated = match task.debates.len() {
                    0 => String::new(),
                    n => format!("（廷议 {n} 场）"),
                };
                println!(
                    "#{:<3} {parent}{} {}  —  {}{debated}{usage}",
                    task.edict.id.0,
                    task.edict.department.label(),
                    task.edict.text,
                    status
                );
            }
        }
        Command::Diff(task) => {
            let entry = session
                .court()
                .task(task)
                .ok_or(format!("未记录下达的诏令 #{}", task.0))?;
            let Some(changes) = &entry.changes else {
                println!("诏令 #{} 没有呈报改动。", task.0);
                return Ok(());
            };
            println!("诏令 #{} 呈报改动{}：", task.0, changes_note(entry));
            for file in &changes.files {
                print!("{}", render_diff(file));
            }
        }
        Command::Decide(task, apply) => {
            if apply {
                session.apply_changes(task, &env::current_dir()?)?;
                println!("诏令已颁：准奏，诏令 #{} 的改动已写入项目。", task.0);
            } else {
                session.discard_changes(task)?;
                println!("诏令已颁：驳回诏令 #{} 的改动，未写入。", task.0);
            }
        }
        Command::Cancel(task) => {
            session.cancel(task)?;
            println!("诏令 #{} 已撤销。", task.0);
        }
        Command::Retreat(on) => {
            let note = session.retreat(on)?;
            if on {
                println!("诏令已颁：朕移居西苑修道，朝政付内阁与司礼监。");
            } else {
                println!("诏令已颁：朕回朝亲政。");
            }
            println!("史官记：{note}");
        }
        Command::Reform(mode) => {
            let note = session.reform(mode)?;
            println!(
                "诏令已颁：制度改为{}。旧制度的中书省/内阁官职自动撤销。",
                mode.label()
            );
            println!("史官记：{note}");
            if mode != GovernanceMode::DirectRule {
                println!("提示：新制度如无协调者，请先任命。 ");
            }
        }
        Command::Model(office, model) => {
            session.set_office_model(office, model.clone())?;
            match model {
                Some(m) => println!("诏令已颁：{}改用模型 {m}。", office.label()),
                None => println!("诏令已颁：{}恢复默认模型。", office.label()),
            }
        }
        Command::Ceiling(office, ceiling) => {
            session.set_office_ceiling(office, ceiling.clone())?;
            match ceiling {
                Some(seal) => println!("诏令已颁：{}玉玺上限为 {seal}。", office.label()),
                None => println!("诏令已颁：{}取消玉玺上限。", office.label()),
            }
        }
        Command::Purge(id) => {
            session.purge(id)?;
            println!("诏令已颁：官员 #{} 革职，削去仕籍，永不叙用。", id.0);
        }
        Command::Abolish => {
            println!("{}", session.abolish_chancellery()?);
            println!("诏令已颁：罢中书省，制度改为废相亲政。");
            println!("史官记：{ABOLITION_NOTE}");
        }
        Command::Draft(task) => {
            let tasks = match task {
                Some(task) => vec![task],
                None => {
                    let court = session.court();
                    court
                        .tasks()
                        .filter(|t| court.needs_approval(t) && t.draft.is_none())
                        .map(|t| t.edict.id)
                        .collect()
                }
            };
            if tasks.is_empty() {
                println!("没有待票拟的诏令。");
            } else {
                let backend = worker_backend()?;
                for task in tasks {
                    session.draft(task, backend.as_ref())?;
                    print_draft(session, task);
                }
                println!("请 `ziyuan approve <诏令ID|all>` 批红，或 `ziyuan redraft` 改票。");
            }
        }
        Command::Censor(task) => {
            let tasks = match task {
                Some(task) => vec![task],
                None => session
                    .court()
                    .tasks()
                    .filter(|t| {
                        matches!(t.status, TaskStatus::Completed { .. }) && t.censure.is_none()
                    })
                    .map(|t| t.edict.id)
                    .collect(),
            };
            if tasks.is_empty() {
                println!("没有待复核的诏令。");
            }
            let backend = if tasks.is_empty() {
                None
            } else {
                Some(worker_backend()?)
            };
            for task in tasks {
                let backend = backend.as_deref().expect("有诏令时已构造后端");
                session.censor(task, &env::current_dir()?, backend)?;
                let court = session.court();
                let censure = court
                    .task(task)
                    .and_then(|t| t.censure.as_ref())
                    .expect("刚复核");
                let censor = court.agent(censure.censor).map_or("?", |a| a.name.as_str());
                println!(
                    "诏令 #{} 都察院复核：左都御史{censor}{}（{}）",
                    task.0,
                    censure.verdict.label(),
                    censure.evidence
                );
                for line in censure.note.lines().filter(|l| !l.trim().is_empty()) {
                    println!("  {line}");
                }
            }
        }
        Command::Kaocheng => println!("{}", kaocheng_report(session.court())),
        Command::Debate(task, members, rounds) => {
            let backend = worker_backend()?;
            println!("诏令 #{} 廷议：", task.0);
            let stopped =
                session.debate(task, &members, rounds, backend.as_ref(), |agent, speech| {
                    println!(
                        "【{}{}·{}】{}",
                        speech.office.label(),
                        agent.name,
                        Persona::of(agent.id).label(),
                        speech.text
                    );
                    let _ = io::stdout().flush();
                })?;
            if let Some(reason) = stopped {
                println!("廷议中止：{reason}；已有发言照记。");
            }
            println!("纪要已记入事件簿，票拟与承办时附上，供参考；派工与玉玺不变。");
        }
        Command::Split(task, plan) => {
            let subtasks = if plan.is_empty() {
                session.plan_split(task, worker_backend()?.as_ref())?
            } else {
                session.split(task, plan)?
            };
            let court = session.court();
            let note = court
                .task(task)
                .and_then(|t| t.split.as_ref())
                .map_or("", |s| s.note.as_str());
            println!("诏令 #{} 分办（{note}）：", task.0);
            for id in subtasks {
                let sub = court.task(id).expect("刚分办");
                let worker = sub.assignee.and_then(|a| court.agent(a));
                let name = worker.map_or("?", |a| a.name.as_str());
                println!("  #{} → {name}：{}", id.0, sub.edict.text);
            }
            println!("`ziyuan run` 执行；子诏令全部结束后由尚书汇总回奏。");
        }
        Command::Redraft(task, worker, note) => {
            session.redraft(task, worker, note)?;
            print_draft(session, task);
        }
        Command::Approve(task) => {
            let tasks = match task {
                Some(task) => vec![task],
                None => {
                    let court = session.court();
                    court
                        .tasks()
                        .filter(|t| court.needs_approval(t))
                        .map(|t| t.edict.id)
                        .collect()
                }
            };
            if tasks.is_empty() {
                println!("没有待批红的诏令。");
            }
            for task in tasks {
                session.approve(task)?;
                println!("诏令 #{} 已批红。", task.0);
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
            let r = court.service_record(id);
            println!("吏部考绩：{}（#{}）", agent.name, id.0);
            println!("  {}；考成：{}", describe_record(&r), kaocheng_grade(&r));
            println!(
                "  工具调用 {} 次，其中失败 {} 次（含越出玉玺的尝试）",
                r.tool_calls, r.tool_failures
            );
            let handled = court.tasks().filter(|t| t.handler() == Some(id));
            println!("  模型用量：{}{}", r.usage, cost_note(handled));
            if let Some(office) = court.office_of(id) {
                println!(
                    "  承办模型：{}",
                    court.office_model(office).unwrap_or(&model_label())
                );
                if let Some(ceiling) = court.office_ceiling(office) {
                    println!("  {}玉玺上限：{ceiling}", office.label());
                }
            }
            if r.unreviewed > 0 {
                println!(
                    "  另有 {} 件回奏未经评定，不计入考绩。可用 `ziyuan review <诏令ID> accept|reject` 评定。",
                    r.unreviewed
                );
            }
        }
        Command::Review(task, verdict, note) => {
            session.review(task, verdict, note)?;
            println!("诏令 #{} 评定为{}。", task.0, verdict.label());
        }
        Command::Recommend(office) => {
            let court = session.court();
            let candidates = court.recommend(office)?;
            if candidates.is_empty() {
                println!("无人可任{}。可先 `ziyuan recruit`。", office.label());
                return Ok(());
            }
            println!(
                "吏部荐举{}（按 采纳−驳回、失败、工具失败 排序）：",
                office.label()
            );
            for c in &candidates {
                let current = c.current.map_or("未任官职".into(), |o| o.label());
                println!(
                    "#{:<3} {:<8} {:<12} {}",
                    c.agent.id.0,
                    c.agent.name,
                    current,
                    describe_record(&c.record)
                );
            }
            println!("仅为建议：任命须由皇帝 `ziyuan appoint`（如有现职须先 dismiss）。");
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
/// 执行 `tasks`；`None` 则反复执行全部可执行的诏令直到队列清空（分办的汇总随后接上）。
fn run_tasks(
    session: &mut Session<EventLog>,
    tasks: Option<&[TaskId]>,
    backend: Arc<dyn WorkerBackend>,
    limits: Limits,
) -> Result<(), Box<dyn Error>> {
    let scheduler = Scheduler::new(backend, limits);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let mut unfinished = 0;
    let mut finished = Vec::new();
    let result = runtime.block_on(async {
        let cancel = CancelToken::new();
        let on_interrupt = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("收到中断：撤销执行中的诏令，排队中的保留。");
                on_interrupt.cancel();
            }
        });
        let mut stream = StreamPrinter::new(io::stderr());
        let on_update = |update: RunUpdate<'_>| match update {
            RunUpdate::Assigned(_) => {}
            RunUpdate::Text { task, text } => stream.text(task, text),
            RunUpdate::Finished(report) => {
                stream.finish(report.edict.id);
                if !matches!(report.outcome, Outcome::Completed(_)) {
                    unfinished += 1;
                }
                finished.push(report.edict.id);
                print_report(report);
            }
        };
        match tasks {
            Some(tasks) => session.run(tasks, &scheduler, &cancel, on_update).await,
            None => session.run_open(&scheduler, &cancel, on_update).await,
        }
    });
    for &task in &finished {
        let entry = session.court().task(task);
        if let Some(entry) = entry.filter(|t| t.changes.is_some()) {
            println!(
                "诏令 #{} 呈报改动{}：`ziyuan diff {0}` 查看，`ziyuan apply {0}` 准奏落盘，`ziyuan discard {0}` 驳回。",
                task.0,
                changes_note(entry)
            );
        }
    }
    // 超时或撤销后，阻塞中的模型请求可能仍未返回；不等待它们。
    runtime.shutdown_background();
    result?;
    if unfinished > 0 {
        return Err(format!("{unfinished} 件诏令未完成").into());
    }
    Ok(())
}

/// 把模型的流式输出实时写到 stderr，每行以 `#诏令ID│` 开头，并发任务不会混在同一行。
/// 最终回奏仍由 [`print_report`] 写到 stdout，便于脚本处理。
struct StreamPrinter<W: Write> {
    out: W,
    /// 当前行未结束的诏令。
    open: Option<TaskId>,
    /// 行被其他诏令打断（已代为换行）的诏令：其后紧接的换行符不再重复输出。
    interrupted: BTreeSet<TaskId>,
}

impl<W: Write> StreamPrinter<W> {
    fn new(out: W) -> Self {
        Self {
            out,
            open: None,
            interrupted: BTreeSet::new(),
        }
    }

    /// 结束当前未完成的行（属于其他诏令时记为被打断）。
    fn break_line(&mut self, task: Option<TaskId>) {
        if let Some(open) = self.open.take() {
            let _ = writeln!(self.out);
            if Some(open) != task {
                self.interrupted.insert(open);
            }
        }
    }

    fn text(&mut self, task: TaskId, text: &str) {
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

    /// `task` 已结束：在打印回奏前结束任何未完成的行。
    fn finish(&mut self, task: TaskId) {
        self.break_line(Some(task));
        self.interrupted.remove(&task);
        let _ = self.out.flush();
    }
}

fn print_draft(session: &Session<EventLog>, task: TaskId) {
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
    // 演示后端不调用模型；服务端未报告用量时也不显示。
    if report.usage.total() > 0 {
        let model = report.model.as_deref().unwrap_or("?");
        let cost =
            price_for(model).map_or(String::new(), |p| format!("，{}", p.describe(report.usage)));
        println!("用量：{}（{model}）{cost}", report.usage);
    }
}

/// 显示用的承办模型（与 [`worker_backend`] 的选择一致）：演示后端不调用模型。
/// 按各诏令实际所用模型估算的花费（单价见 `ziyuan_llm::price_for`）；查不到单价时为空。
fn cost_note<'a>(tasks: impl IntoIterator<Item = &'a Task>) -> String {
    let default = OpenAiCompatible::from_env().map(|m| m.model().to_owned());
    estimate_cost(tasks, default.as_deref(), price_for).map_or(String::new(), |c| format!("，{c}"))
}

fn model_label() -> String {
    OpenAiCompatible::from_env().map_or("演示（未接模型）".into(), |m| m.model().to_owned())
}

/// 呈报改动的状态，如「；改动 2 个文件待准奏」；没有改动时为空。
fn changes_note(task: &Task) -> String {
    let Some(changes) = &task.changes else {
        return String::new();
    };
    let state = match changes.state {
        ChangeState::Proposed => "待准奏",
        ChangeState::Applied => "已落盘",
        ChangeState::Discarded => "已驳回",
    };
    format!("；改动 {} 个文件{state}", changes.files.len())
}

/// 设置了 `ZIYUAN_LLM_MODEL` 时使用真实模型与只读工具（以当前目录为项目根），否则使用演示后端。
fn worker_backend() -> Result<Arc<dyn WorkerBackend>, Box<dyn Error>> {
    let Some(model) = OpenAiCompatible::from_env() else {
        return Ok(Arc::new(DemoBackend));
    };
    eprintln!(
        "执行后端：{}（工具与查阅范围由各诏令的玉玺决定）",
        model.model()
    );
    let catalog = Box::new(model.catalog());
    Ok(Arc::new(
        AgentBackend::new(Box::new(model), env::current_dir()?, MAX_AGENT_STEPS)
            .with_catalog(catalog)
            .with_test_command(env::var("ZIYUAN_TEST_COMMAND").ok()),
    ))
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

fn task_id(args: &[String], usage: &str) -> Result<TaskId, Box<dyn Error>> {
    match args {
        [id] => Ok(TaskId(id.parse().map_err(|_| "诏令 ID 必须是数字")?)),
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
    const USAGE: &str = "用法: ziyuan edict [--dept works] [--scope <路径>]... [--tools <工具,…>|none] [--queue] <诏令内容>";
    let mut department = None;
    let mut queue = false;
    let mut seal = Seal::default();
    let mut scope = Vec::new();
    while let Some(flag) = args.first().cloned() {
        match flag.as_str() {
            "--queue" => {
                args.remove(0);
                queue = true;
            }
            "--dept" | "--scope" | "--tools" => {
                if args.len() < 2 {
                    return Err(USAGE.into());
                }
                args.remove(0);
                let value = args.remove(0);
                match flag.as_str() {
                    "--dept" => department = Some(value.parse::<Department>()?),
                    "--scope" => scope.push(value),
                    _ => seal.capabilities = parse_capabilities(&value)?,
                }
            }
            _ => break,
        }
    }
    if args.is_empty() {
        return Err(USAGE.into());
    }
    if !scope.is_empty() {
        // 越出项目的路径由治理层拒绝；这里尽早指出不存在的路径。
        let root = env::current_dir()?;
        if let Some(missing) = scope.iter().find(|p| !root.join(p).exists()) {
            return Err(format!("查阅范围不存在: {missing}").into());
        }
        seal.scope = scope;
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
        seal,
    })
}

/// `ceiling <官职> default`，或 `--scope`/`--tools`（与诏令玉玺同义；未给出的一项不设限）。
fn parse_ceiling(mut args: Vec<String>) -> Result<Command, Box<dyn Error>> {
    const USAGE: &str =
        "用法: ziyuan ceiling <官职> [--scope <路径>]... [--tools <工具,…>|none] | default";
    if args.is_empty() {
        return Err(USAGE.into());
    }
    let office: Office = args.remove(0).parse()?;
    if args == ["default"] {
        return Ok(Command::Ceiling(office, None));
    }
    let mut seal = Seal::default();
    let mut scope = Vec::new();
    for pair in args.chunks(2) {
        match pair {
            [flag, value] if flag == "--scope" => scope.push(value.clone()),
            [flag, value] if flag == "--tools" => seal.capabilities = parse_capabilities(value)?,
            _ => return Err(USAGE.into()),
        }
    }
    if args.is_empty() {
        return Err(USAGE.into());
    }
    if !scope.is_empty() {
        seal.scope = scope;
    }
    Ok(Command::Ceiling(office, Some(seal)))
}

/// `none` 表示不授予任何工具；否则为逗号分隔的工具名。
fn parse_capabilities(value: &str) -> Result<BTreeSet<Capability>, Box<dyn Error>> {
    if value == "none" {
        return Ok(BTreeSet::new());
    }
    Ok(value
        .split(',')
        .map(str::parse::<Capability>)
        .collect::<Result<_, _>>()?)
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
}
