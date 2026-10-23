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
    ABOLITION_NOTE, RunUpdate, Session, TaskReport, describe_event, describe_record, seal_note,
};
use ziyuan_core::{AgentId, Capability, Department, GovernanceMode, Office, Seal, TaskId, Verdict};
use ziyuan_governance::TaskStatus;
use ziyuan_llm::OpenAiCompatible;
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
  ziyuan serve [--port 8080]           启动紫禁城 Web 界面（仅本机，需访问令牌）
  ziyuan tui                           启动紫禁城终端界面
  ziyuan cancel <诏令ID>                撤销未结束的诏令
  ziyuan reform <chancellor|direct|cabinet>
  ziyuan dismiss <官员ID>               罢免现任官职
  ziyuan purge <官员ID>                 革职：削去仕籍，永不叙用
  ziyuan abolish                       一键朱元璋：革去全部丞相，罢中书省（改为废相亲政）
  ziyuan approve <诏令ID|all>           内阁制下对内阁票拟批红（批红后方可执行）
  ziyuan appoint <官员ID> <官职>         任命（如 first-secretary / minister:works）
  ziyuan review <诏令ID> <accept|reject> [备注]
                                       评定已完成的诏令（吏部考绩只采信评定）
  ziyuan examine <官员ID>               吏部考绩：经评定的履历
  ziyuan recommend <官职>               吏部荐举：按履历排列可任该职者（不自动任命）
  ziyuan history                       查看事件记录
  ziyuan help

官制（品级随制度而变）：
  丞相制（仿元·中书省）  chancellor 中书右丞相、vice-chancellor 中书左丞相
  废相亲政（仿明初）    不设宰辅，皇帝直领六部
  内阁制（仿明中后期）  first-secretary 内阁首辅、second-secretary 内阁次辅
  六部（各制度通用）    minister:<部门> 尚书、vice-minister:<部门> 侍郎、clerk:<部门> 郎中
  正职空缺时由副职署理。
部门：personnel / revenue / rites / war / justice / works

玉玺（承办官员的权限，随诏令记入事件簿）：
  --scope <路径>   可查阅的项目路径（目录或文件，可重复）；默认整个项目
  --tools <工具>   read_file、search_code 的逗号列表，或 none；默认全部
  无论玉玺如何，.ziyuan、.git、.jj 与 .env* 均不可读。

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
    Serve(u16),
    Tui,
    Cancel(TaskId),
    Reform(GovernanceMode),
    Dismiss(AgentId),
    Purge(AgentId),
    Abolish,
    /// `None` 表示全部待批红的诏令。
    Approve(Option<TaskId>),
    Appoint(AgentId, Office),
    Examine(AgentId),
    Review(TaskId, Verdict, String),
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
        "serve" => match args.as_slice() {
            [] => Command::Serve(8080),
            [flag, port] if flag == "--port" => {
                Command::Serve(port.parse().map_err(|_| "端口必须是 0–65535 的数字")?)
            }
            _ => return Err("用法: ziyuan serve [--port 8080]".into()),
        },
        "cancel" => match <[String; 1]>::try_from(args) {
            Ok([id]) => Command::Cancel(TaskId(id.parse().map_err(|_| "诏令 ID 必须是数字")?)),
            Err(_) => return Err("用法: ziyuan cancel <诏令ID>".into()),
        },
        "reform" => match <[String; 1]>::try_from(args) {
            Ok([mode]) => Command::Reform(mode.parse()?),
            Err(_) => return Err("用法: ziyuan reform <chancellor|direct|cabinet>".into()),
        },
        "dismiss" => Command::Dismiss(parse_id(&args, "ziyuan dismiss <官员ID>")?),
        "purge" => Command::Purge(parse_id(&args, "ziyuan purge <官员ID>")?),
        "abolish" => no_args(args, Command::Abolish)?,
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
            println!(
                "在册官员：{}；有职官员：{}；已完成任务：{}",
                court.agents().count(),
                court.appointed_count(),
                court.completed_count()
            );
            println!("承办模型：{}", model_label());
            println!("模型用量：{}", court.total_usage());
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
            let model = model_label();
            for agent in court.agents() {
                let office = match court.office_of(agent.id) {
                    Some(o) => format!("{}（{}）", o.label(), o.rank(court.mode())),
                    None if court.is_purged(agent.id) => "已革职".into(),
                    None => "未任官职".into(),
                };
                println!(
                    "#{:<3} {:<6} {:<18} {} 模型 {} (完成 {} 件)",
                    agent.id.0,
                    agent.name,
                    office,
                    agent.speciality.label(),
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
            if !queue && session.court().mode() == GovernanceMode::Cabinet {
                // 皇帝亲自下诏并立即执行：内阁票拟后当即批红。
                session.approve(edict.id)?;
                println!("诏令 #{} 经内阁票拟，朱笔批红。", edict.id.0);
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
                run_tasks(session, &[edict.id], limits)?;
            }
            drop(executor);
        }
        Command::Run(limits) => {
            let _executor = lock_executor()?;
            let court = session.court();
            let tasks = court.runnable_tasks();
            let waiting = court.open_tasks().len() - tasks.len();
            if waiting > 0 {
                eprintln!(
                    "另有 {waiting} 件诏令待批红（`ziyuan approve <诏令ID|all>`），本次不执行。"
                );
            }
            if tasks.is_empty() {
                println!("没有未结束的诏令。");
            } else {
                run_tasks(session, &tasks, limits)?;
            }
        }
        Command::Serve(port) => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            let config = ziyuan_web::Config {
                root: env::current_dir()?,
                token: ziyuan_web::random_token()?,
                backend: worker_backend()?,
                limits: DEFAULT_LIMITS,
            };
            runtime.block_on(async {
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
                let addr = listener.local_addr()?;
                println!("紫禁城已开启：http://{addr}/?token={}", config.token);
                println!("（仅监听本机；令牌即皇帝身份，请勿外传。Ctrl-C 关闭。）");
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
                    TaskStatus::Pending if court.needs_approval(task) => {
                        match court.route(&task.edict) {
                            Ok(route) => format!("待批红（票拟：{}）", route.path.join(" → ")),
                            Err(e) => format!("待批红（票拟不成：{e}）"),
                        }
                    }
                    TaskStatus::Pending => "待办".to_owned(),
                    TaskStatus::Assigned(w) => format!("已指派 {}（未结束）", name(*w)),
                    TaskStatus::Completed { worker, .. } => match &task.review {
                        Some((verdict, _)) => {
                            format!("已完成（{}），{}", name(*worker), verdict.label())
                        }
                        None => format!("已完成（{}），待评定", name(*worker)),
                    },
                    TaskStatus::Failed { reason, .. } => format!("失败：{reason}"),
                    TaskStatus::Cancelled => "已撤销".to_owned(),
                };
                let usage = if task.usage.total() > 0 {
                    format!("（{} tokens）", task.usage.total())
                } else {
                    String::new()
                };
                println!(
                    "#{:<3} {} {}  —  {}{usage}",
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
        Command::Purge(id) => {
            session.purge(id)?;
            println!("诏令已颁：官员 #{} 革职，削去仕籍，永不叙用。", id.0);
        }
        Command::Abolish => {
            let purged = session.abolish_chancellery()?;
            let court = session.court();
            let names: Vec<_> = purged
                .iter()
                .filter_map(|&id| court.agent(id))
                .map(|a| format!("#{} {}", a.id.0, a.name))
                .collect();
            if names.is_empty() {
                println!("中书省本无丞相在任。");
            } else {
                println!("丞相 {} 革职削籍。", names.join("、"));
            }
            println!("诏令已颁：罢中书省，制度改为废相亲政。");
            println!("史官记：{ABOLITION_NOTE}");
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
            println!("  {}", describe_record(&r));
            println!(
                "  工具调用 {} 次，其中失败 {} 次（含越出玉玺的尝试）",
                r.tool_calls, r.tool_failures
            );
            println!("  模型用量：{}", r.usage);
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
        let mut stream = StreamPrinter::new(io::stderr());
        session
            .run(tasks, &scheduler, &cancel, |update| match update {
                RunUpdate::Assigned(_) => {}
                RunUpdate::Text { task, text } => stream.text(task, text),
                RunUpdate::Finished(report) => {
                    stream.finish(report.edict.id);
                    if !matches!(report.outcome, Outcome::Completed(_)) {
                        unfinished += 1;
                    }
                    print_report(report);
                }
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
        println!("用量：{}", report.usage);
    }
}

/// 显示用的承办模型（与 [`worker_backend`] 的选择一致）：演示后端不调用模型。
fn model_label() -> String {
    OpenAiCompatible::from_env().map_or("演示（未接模型）".into(), |m| m.model().to_owned())
}

/// 设置了 `ZIYUAN_LLM_MODEL` 时使用真实模型与只读工具（以当前目录为项目根），否则使用演示后端。
fn worker_backend() -> Result<Arc<dyn WorkerBackend>, Box<dyn Error>> {
    let Some(model) = OpenAiCompatible::from_env() else {
        return Ok(Arc::new(DemoBackend));
    };
    eprintln!(
        "执行后端：{}（只读工具与查阅范围由各诏令的玉玺决定）",
        model.model()
    );
    Ok(Arc::new(AgentBackend::new(
        Box::new(model),
        env::current_dir()?,
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
    const USAGE: &str = "用法: ziyuan edict [--dept works] [--scope <路径>]... [--tools read_file,search_code|none] [--queue] <诏令内容>";
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
