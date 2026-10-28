//! 紫垣命令行：用户扮演皇帝，改制/任免为显式用户操作。
//!
//! 本模块只负责解析参数与呈现结果；用例流程在 `ziyuan-app::Session` 中。
mod args;
mod backend;
mod commands;
mod help;
mod output;

use std::env;
use std::error::Error;
use std::process::ExitCode;
use std::sync::Arc;
use ziyuan_app::Session;
use ziyuan_storage::EventLog;

use crate::args::{Command, parse};
use crate::backend::{DEFAULT_LIMITS, login, logout, worker_backend};
use crate::commands::execute;
use crate::help::{HELP, JSON_COMMANDS, completions};
use crate::output::json_output;

/// 用法错误（未知命令、参数不对）：退出码 2，与运行中出错（退出码 1）分开，便于脚本区分。
#[derive(Debug)]
struct Usage(Box<dyn Error>);

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Error for Usage {}

fn main() -> ExitCode {
    quiet_broken_pipe();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("错误：{error}");
            match error.is::<Usage>() {
                true => ExitCode::from(2),
                false => ExitCode::FAILURE,
            }
        }
    }
}

/// 输出被提前关闭（如 `ziyuan history | head`）时安静退出，不报 panic。
///
/// Rust 忽略 SIGPIPE，`println!` 写入断开的管道会 panic；工作区禁用 unsafe，无法恢复默认
/// 信号处理，故在 panic hook 中识别这一种情况，以成功状态退出（与 ripgrep 等工具一致）。
fn quiet_broken_pipe() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("");
        if message.contains("failed printing to stdout") && message.contains("Broken pipe") {
            std::process::exit(0);
        }
        default(info);
    }));
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    args.retain(|a| a != "--json");
    if json
        && !args
            .first()
            .is_some_and(|a| JSON_COMMANDS.contains(&a.as_str()))
    {
        return Err(Usage(format!("--json 只支持 {}", JSON_COMMANDS.join("、")).into()).into());
    }
    let command = parse(args).map_err(Usage)?;
    let log = EventLog::new(&env::current_dir()?);
    if json && matches!(command, Command::Migrate) {
        // 不经 Session：事件簿读不了时也要给出报告。读不了时退出码为 1。
        let report = migrate_json(&log);
        println!("{}", serde_json::to_string_pretty(&report)?);
        if report["readable"] == false {
            return Err("本版读不了这本事件簿".into());
        }
        return Ok(());
    }
    if json {
        let mut session = Session::open(log)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&json_output(&session, &command)?)?
        );
        chronicle(&mut session);
        return Ok(());
    }
    match command {
        Command::Help => println!("{HELP}"),
        Command::Completions(shell) => print!("{}", completions(shell)),
        Command::Init => {
            let session = Session::found(log)?;
            println!(
                "紫垣已建立。当前制度：丞相制；丞相一人，六部各有尚书，吏部、工部另有郎中。请执行 `ziyuan court`。"
            );
            for note in ziyuan_app::vacancies(session.court()) {
                println!("  - {note}");
            }
        }
        // 不经 Session：事件簿读不了时也要能指出是哪一行。
        Command::Migrate => migrate(&log)?,
        Command::Login(models) => login(models).map_err(|e| e as Box<dyn Error>)?,
        Command::Logout => logout().map_err(|e| e as Box<dyn Error>)?,
        // 前端各自打开并缓存会话；这里不开会话（放掉的内存分配器也不还给系统），只看朝廷是否已建立。
        command @ (Command::Serve { .. } | Command::Tui) => {
            let founded = env::current_dir()?
                .join(".ziyuan/court.log")
                .metadata()
                .is_ok_and(|m| m.len() > 0);
            if !founded {
                return Err(ziyuan_app::AppError::NotFounded.into());
            }
            frontend(command)?;
        }
        command => {
            let mut session = Session::open(log)?;
            let result = execute(&mut session, command);
            chronicle(&mut session);
            result?
        }
    }
    Ok(())
}

/// 奉天门（Web）与紫禁城（TUI）：常驻进程，自己打开会话。
fn frontend(command: Command) -> Result<(), Box<dyn Error>> {
    match command {
        Command::Serve {
            port,
            bind,
            rotate,
            viewer,
        } => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            let root = env::current_dir()?;
            let config = ziyuan_web::Config {
                token: ziyuan_web::load_or_create_token(&root, rotate)?,
                viewer_token: match viewer {
                    true => Some(ziyuan_web::load_or_create_viewer_token(&root, rotate)?),
                    false => None,
                },
                root,
                backend: worker_backend()?,
                limits: DEFAULT_LIMITS,
                github: ziyuan_github::GitHub::from_env()?.map(Arc::new),
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
                if let Some(viewer) = &config.viewer_token {
                    println!(
                        "观政（只读）：http://{host}/?token={viewer}（存于 .ziyuan/web-viewer-token）"
                    );
                }
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
        _ => unreachable!("run 只把 serve 与 tui 交到这里"),
    }
    Ok(())
}

/// 史官对修会典的记述（写到 stderr，不混入 `--json` 与管道里的输出）。
fn chronicle(session: &mut Session<EventLog>) {
    if let Some(note) = session.take_chronicle() {
        eprintln!("{note}");
    }
}

/// `ziyuan migrate --json`：与文字报告同一份检查。
fn migrate_json(log: &EventLog) -> serde_json::Value {
    let version = env!("CARGO_PKG_VERSION");
    let report = match log.inspect() {
        Ok(report) => report,
        Err(e) => {
            return serde_json::json!({ "version": version, "readable": false, "problem": e.to_string() });
        }
    };
    serde_json::json!({
        "version": version,
        "format": 1,
        "events": report.events,
        "readable": report.problem.is_none(),
        "problem": report.problem.as_ref().map(ToString::to_string),
        "readable_since": format!("0.{}.0", report.readable_since),
        "first_needing": report.first_needing.as_ref().map(|(line, kind)| serde_json::json!({ "line": line, "kind": kind })),
        "torn_tail": report.torn_tail,
        "huidian": log.snapshot_header().map(|(events, tag)| serde_json::json!({
            "events": events,
            "tag": tag,
            "current": tag == format!("v{version}"),
        })),
    })
}

fn migrate(log: &EventLog) -> Result<(), Box<dyn Error>> {
    let report = log.inspect()?;
    let version = env!("CARGO_PKG_VERSION");
    println!(
        "事件簿 .ziyuan/court.log：{} 条事件，格式 v1。",
        report.events
    );
    if let Some(problem) = report.problem {
        return Err(format!("{problem}").into());
    }
    let since = report.readable_since;
    match &report.first_needing {
        None => println!("本版 v{version} 可读；v0.1 起的各版本都能打开。"),
        Some((line, kind)) => {
            let older = match since - 1 {
                // v0.4 起遇到不认识的内容会提示升级，更早的版本只报解码失败。
                4.. => "会报「由更新版本的紫垣写入」或解码失败",
                _ => "会报解码失败",
            };
            println!(
                "本版 v{version} 可读；至少要 v0.{since}.0 才能打开（第 {line} 行 {kind} 起），v0.{}.x 及更早的版本{older}。",
                since - 1
            );
        }
    }
    if report.torn_tail {
        println!("末尾有一行未写完（中断的写入），下次写入前自动截掉。");
    }
    match log.snapshot_header() {
        Some((events, tag)) if tag == format!("v{version}") => println!(
            "《大明会典》.ziyuan/huidian：收前 {events} 条事件，打开时从此接着回放。会典只是缓存，可随时删去。"
        ),
        Some((events, tag)) => println!(
            "《大明会典》.ziyuan/huidian：{tag} 所修（收前 {events} 条），本版不沿用，下次打开时重修。"
        ),
        None => println!(
            "尚无《大明会典》：每满 {} 条事件修一部，打开时从会典接着回放。",
            ziyuan_app::HUIDIAN_EVERY
        ),
    }
    println!("无需迁移。史官记：祖宗成法不可轻改。");
    Ok(())
}
