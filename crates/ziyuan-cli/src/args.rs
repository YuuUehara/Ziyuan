//! 命令行参数解析：把参数解析为 `Command`。

use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::io::{self};
use std::time::Duration;
use ziyuan_app::{HistoryFilter, WatchOptions, parse_time, tz_offset};
use ziyuan_core::{
    AgentId, BudgetScope, Capability, Department, GovernanceMode, Office, Seal, TaskId, Template,
    Verdict,
};
use ziyuan_runtime::Limits;

use crate::backend::DEFAULT_LIMITS;
use crate::help::Shell;

pub(crate) enum Command {
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
        /// `--template`：按翰林院模板下诏；未显式给出的部门、工具、范围取模板的。
        template: Option<TemplateChoice>,
    },
    /// 翰林院：列出模板。
    Templates,
    /// 外藩名册。
    Mcp,
    /// 翰林院存模板。
    SaveTemplate {
        name: String,
        template: Template,
    },
    /// 翰林院撤去自定的模板。
    RemoveTemplate(String),
    /// `watch` 为常朝的设定（`None` 只执行当前队列）。
    Run(Limits, Option<WatchOptions>),
    Tasks,
    Serve {
        port: u16,
        bind: std::net::IpAddr,
        /// 重新生成令牌（旧令牌作废）。
        rotate: bool,
        /// 同时启用只读的观政令牌。
        viewer: bool,
    },
    Tui,
    /// 检查事件簿：本版能否读、至少要哪个版本才能打开。
    Migrate,
    /// 登录 ChatGPT 订阅；`true` 只列出可选模型。
    Login(bool),
    Logout,
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
    /// `None` 列出全部诏令的密报。
    Flags(Option<TaskId>),
    /// 兵部驿站 · GitHub。
    GitHub(GitHubAction),
    /// 礼部修 CHANGELOG：版本名、起点（`None` 为最近的 tag）。
    Changelog {
        version: String,
        since: Option<String>,
    },
    /// 刑部复勘；`None` 表示全部有待复勘检出的诏令。
    Recheck(Option<TaskId>),
    /// 平反：序号从 0 起；`None` 为刑部判为误报的各处，`Some(空)` 为全部未平反的。
    Acquit(TaskId, Option<Vec<u32>>),
    /// 兵部驿传：外部事件入队为诏令（只入队，不执行）。
    Post {
        department: Department,
        station: String,
        urgent: bool,
        text: String,
    },
    /// 八百里加急。
    Urgent(TaskId),
    /// 礼部科举：官员应试 `.ziyuan/exam/` 中的考题。
    Exam(AgentId),
    /// 殿试：皇帝出题，在任官员作答。
    Palace(String),
    /// 钦点状元。
    Crown(AgentId),
    /// 廷推（`lots` 为枚卜：从最近一次廷推的被推者中抽签任命）。
    Nominate {
        office: Office,
        lots: bool,
    },
    /// 廷杖。
    Cane(AgentId),
    /// 宽宥受杖者。
    Pardon(AgentId),
    /// `None` 出具度支奏销；否则设定度支（`None` 额度为取消）。
    Budget(Option<(BudgetScope, Option<u64>)>),
    /// 准奏（驳回其中若干段）。
    /// 准奏：逐段驳回的段号；`true` 落盘后记为一个 jj/git 提交。
    Apply(TaskId, Vec<u32>, bool),
    Discard(TaskId),
    Recommend(Office),
    History(HistoryFilter),
    /// 翰林院实录：检索词为空则列出全部。
    Records(String),
    /// 输出 shell 补全脚本。
    Completions(Shell),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GitHubAction {
    /// 带标签的 issue / PR 及成员的 `/ziyuan` 评论入队（交给 `department`，盖 `seal`）。
    Pull {
        label: String,
        department: Department,
        seal: Seal,
    },
    /// 回奏写回评论；`None` 为全部可回报的。
    Reply(Option<TaskId>),
    /// 准奏的改动开为草稿 PR。
    PullRequest(TaskId),
}

pub(crate) fn parse(mut args: Vec<String>) -> Result<Command, Box<dyn Error>> {
    if args.is_empty() {
        return Ok(Command::Help);
    }
    let name = args.remove(0);
    let command = match name.as_str() {
        "help" | "--help" => Command::Help,
        "init" => no_args(args, Command::Init)?,
        "court" => no_args(args, Command::Court)?,
        "officials" => no_args(args, Command::Officials)?,
        "history" => parse_history(args)?,
        "records" => {
            if let Some(flag) = args.iter().find(|a| a.starts_with("--")) {
                return Err(
                    if ["--edit", "--amend", "--delete", "--rm"].contains(&flag.as_str()) {
                        "实录只增不改。史官记：靖难之后重修《太祖实录》，建文四年遂成「洪武三十五年」——紫垣不开此例。"
                        .into()
                    } else {
                        "用法: ziyuan records [检索词]".into()
                    },
                );
            }
            Command::Records(args.join(" "))
        }
        "recruit" => match args.as_slice() {
            [dept] => Command::Recruit(None, dept.parse()?),
            [name, dept] => Command::Recruit(Some(name.clone()), dept.parse()?),
            _ => return Err("用法: ziyuan recruit [姓名] <部门>".into()),
        },
        "edict" => parse_edict(args)?,
        "template" => parse_template(args)?,
        "run" => parse_run(args)?,
        "tasks" => no_args(args, Command::Tasks)?,
        "tui" => no_args(args, Command::Tui)?,
        "migrate" => no_args(args, Command::Migrate)?,
        "login" => match args.as_slice() {
            [] => Command::Login(false),
            [flag] if flag == "--models" => Command::Login(true),
            _ => return Err("用法: ziyuan login [--models]".into()),
        },
        "logout" => no_args(args, Command::Logout)?,
        "serve" => {
            const USAGE: &str =
                "用法: ziyuan serve [--port 8080] [--bind 127.0.0.1] [--new-token] [--viewer]";
            let (mut port, mut bind, mut rotate, mut viewer) =
                (8080, std::net::Ipv4Addr::LOCALHOST.into(), false, false);
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
                    "--viewer" => viewer = true,
                    _ => return Err(USAGE.into()),
                }
            }
            Command::Serve {
                port,
                bind,
                rotate,
                viewer,
            }
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
        "post" => parse_post(args)?,
        "urgent" => match <[String; 1]>::try_from(args) {
            Ok([id]) => Command::Urgent(TaskId(id.parse().map_err(|_| "诏令 ID 必须是数字")?)),
            Err(_) => return Err("用法: ziyuan urgent <诏令ID>".into()),
        },
        "exam" => match args.as_slice() {
            [flag, rest @ ..] if flag == "--palace" && !rest.is_empty() => {
                Command::Palace(rest.join(" "))
            }
            [id] => Command::Exam(agent_id(id)?),
            _ => return Err("用法: ziyuan exam <官员ID> | ziyuan exam --palace <题目>".into()),
        },
        "crown" => Command::Crown(parse_id(&args, "ziyuan crown <官员ID>")?),
        "nominate" => match args.as_slice() {
            [office] => Command::Nominate {
                office: office.parse()?,
                lots: false,
            },
            [office, flag] if flag == "--lots" => Command::Nominate {
                office: office.parse()?,
                lots: true,
            },
            _ => return Err("用法: ziyuan nominate <官职> [--lots]".into()),
        },
        "cane" => Command::Cane(parse_id(&args, "ziyuan cane <官员ID>")?),
        "pardon" => Command::Pardon(parse_id(&args, "ziyuan pardon <官员ID>")?),
        "flags" => match args.as_slice() {
            [] => Command::Flags(None),
            [id] => Command::Flags(Some(TaskId(id.parse().map_err(|_| "诏令 ID 必须是数字")?))),
            _ => return Err("用法: ziyuan flags [诏令ID]".into()),
        },
        "budget" => {
            const USAGE: &str = "用法: ziyuan budget [court|<官职>|<诏令ID> <tokens|default>]";
            match args.as_slice() {
                [] => Command::Budget(None),
                [scope, amount] => {
                    let scope = match scope.as_str() {
                        "court" => BudgetScope::Court,
                        s if s.bytes().all(|b| b.is_ascii_digit()) => {
                            BudgetScope::Task(TaskId(s.parse().map_err(|_| USAGE)?))
                        }
                        s => BudgetScope::Office(s.parse()?),
                    };
                    let tokens = match amount.as_str() {
                        // 「恢复默认」统一写 default；none 是 v0.6 及以前的写法，仍然接受。
                        "default" | "none" => None,
                        n => Some(n.parse().map_err(|_| "额度须为 tokens 数或 default")?),
                    };
                    Command::Budget(Some((scope, tokens)))
                }
                _ => return Err(USAGE.into()),
            }
        }
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
        "mcp" => no_args(args, Command::Mcp)?,
        "changelog" => {
            const USAGE: &str = "用法: ziyuan changelog [--version <版本>] [--since <提交或tag>]";
            let (mut version, mut since) = ("Unreleased".to_owned(), None);
            let mut rest = args.iter();
            while let Some(flag) = rest.next() {
                let value = rest.next().ok_or(USAGE)?;
                match flag.as_str() {
                    "--version" => version = value.clone(),
                    "--since" => since = Some(value.clone()),
                    _ => return Err(USAGE.into()),
                }
            }
            Command::Changelog { version, since }
        }
        "github" => {
            const USAGE: &str = "用法: ziyuan github pull [--label ziyuan] [--dept works] [--tools <工具,…>] [--scope <路径>]... | reply <诏令ID|all> | pr <诏令ID>";
            let id = |s: &str| -> Result<TaskId, Box<dyn Error>> {
                Ok(TaskId(s.parse().map_err(|_| "诏令 ID 必须是数字")?))
            };
            match args.first().map(String::as_str) {
                Some("pull") => {
                    let (mut label, mut department) =
                        (ziyuan_github::DEFAULT_LABEL.to_owned(), Department::Works);
                    let (mut seal, mut scope) = (Seal::default(), Vec::new());
                    let mut rest = args[1..].iter();
                    while let Some(flag) = rest.next() {
                        let value = rest.next().ok_or(USAGE)?;
                        match flag.as_str() {
                            "--label" => label = value.clone(),
                            "--dept" => department = value.parse()?,
                            "--tools" => seal.capabilities = parse_capabilities(value)?,
                            "--scope" => scope.push(value.clone()),
                            _ => return Err(USAGE.into()),
                        }
                    }
                    if !scope.is_empty() {
                        seal.scope = scope;
                    }
                    Command::GitHub(GitHubAction::Pull {
                        label,
                        department,
                        seal,
                    })
                }
                Some("reply") => match &args[1..] {
                    [all] if all == "all" => Command::GitHub(GitHubAction::Reply(None)),
                    [task] => Command::GitHub(GitHubAction::Reply(Some(id(task)?))),
                    _ => return Err(USAGE.into()),
                },
                Some("pr") => match &args[1..] {
                    [task] => Command::GitHub(GitHubAction::PullRequest(id(task)?)),
                    _ => return Err(USAGE.into()),
                },
                _ => return Err(USAGE.into()),
            }
        }
        "recheck" => match args.as_slice() {
            [all] if all == "all" => Command::Recheck(None),
            [id] => Command::Recheck(Some(TaskId(
                id.parse().map_err(|_| "诏令 ID 必须是数字或 all")?,
            ))),
            _ => return Err("用法: ziyuan recheck <诏令ID|all>".into()),
        },
        "acquit" => {
            const USAGE: &str = "用法: ziyuan acquit <诏令ID> [序号,…|all]";
            match args.as_slice() {
                [id] => Command::Acquit(task_id(std::slice::from_ref(id), USAGE)?, None),
                [id, all] if all == "all" => {
                    Command::Acquit(task_id(std::slice::from_ref(id), USAGE)?, Some(Vec::new()))
                }
                [id, list] => Command::Acquit(
                    task_id(std::slice::from_ref(id), USAGE)?,
                    Some(
                        list.split([',', '，'])
                            .filter(|n| !n.trim().is_empty())
                            .map(|n| match n.trim().parse::<u32>() {
                                Ok(n) if n >= 1 => Ok(n - 1),
                                _ => Err(format!("序号必须是从 1 起的数字：{n}")),
                            })
                            .collect::<Result<_, _>>()?,
                    ),
                ),
                _ => return Err(USAGE.into()),
            }
        }
        "censor" => match args.as_slice() {
            [all] if all == "all" => Command::Censor(None),
            [id] => Command::Censor(Some(TaskId(
                id.parse().map_err(|_| "诏令 ID 必须是数字或 all")?,
            ))),
            _ => return Err("用法: ziyuan censor <诏令ID|all>".into()),
        },
        "apply" => {
            const USAGE: &str = "用法: ziyuan apply <诏令ID> [--skip <段号,…>] [--commit]";
            let commit = args.iter().any(|a| a == "--commit");
            args.retain(|a| a != "--commit");
            match args.as_slice() {
                [id] => Command::Apply(
                    task_id(std::slice::from_ref(id), USAGE)?,
                    Vec::new(),
                    commit,
                ),
                [id, flag, list] if flag == "--skip" => Command::Apply(
                    task_id(std::slice::from_ref(id), USAGE)?,
                    list.split([',', '，'])
                        .filter(|n| !n.trim().is_empty())
                        .map(|n| n.trim().parse().map_err(|_| format!("段号必须是数字：{n}")))
                        .collect::<Result<_, _>>()?,
                    commit,
                ),
                _ => return Err(USAGE.into()),
            }
        }
        "discard" => Command::Discard(task_id(&args, "ziyuan discard <诏令ID>")?),
        "review" => {
            if args.len() < 2 {
                return Err("用法: ziyuan review <诏令ID> <accept|reject> [备注]".into());
            }
            let task = TaskId(args[0].parse().map_err(|_| "诏令 ID 必须是数字")?);
            let verdict = args[1].parse::<Verdict>()?;
            Command::Review(task, verdict, args[2..].join(" "))
        }
        "completions" => match args.as_slice() {
            [shell] if shell == "bash" => Command::Completions(Shell::Bash),
            [shell] if shell == "zsh" => Command::Completions(Shell::Zsh),
            [shell] if shell == "fish" => Command::Completions(Shell::Fish),
            _ => return Err("用法: ziyuan completions <bash|zsh|fish>".into()),
        },
        "recommend" => match <[String; 1]>::try_from(args) {
            Ok([office]) => Command::Recommend(office.parse()?),
            Err(_) => return Err("用法: ziyuan recommend <官职>".into()),
        },
        _ => return Err(format!("未知命令：{name}。运行 ziyuan help 查看帮助").into()),
    };
    Ok(command)
}

pub(crate) fn no_args(args: Vec<String>, command: Command) -> Result<Command, Box<dyn Error>> {
    if args.is_empty() {
        Ok(command)
    } else {
        Err("该命令不接受额外参数".into())
    }
}

pub(crate) fn agent_id(input: &str) -> Result<AgentId, Box<dyn Error>> {
    Ok(AgentId(input.parse().map_err(|_| "官员 ID 必须是数字")?))
}

pub(crate) fn parse_id(args: &[String], usage: &str) -> Result<AgentId, Box<dyn Error>> {
    match args {
        [id] => agent_id(id),
        _ => Err(format!("用法: {usage}").into()),
    }
}

pub(crate) fn task_id(args: &[String], usage: &str) -> Result<TaskId, Box<dyn Error>> {
    match args {
        [id] => Ok(TaskId(id.parse().map_err(|_| "诏令 ID 必须是数字")?)),
        _ => Err(format!("用法: {usage}").into()),
    }
}

pub(crate) fn parse_history(args: Vec<String>) -> Result<Command, Box<dyn Error>> {
    const USAGE: &str = "用法: ziyuan history [--task <诏令ID>] [--official <官员ID>] [--kind <类型>] \\
                         [--since <时间>] [--until <时间>]";
    let mut filter = HistoryFilter::default();
    let (now, offset) = (
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
        tz_offset(),
    );
    for pair in args.chunks(2) {
        let [flag, value] = pair else {
            return Err(USAGE.into());
        };
        match flag.as_str() {
            "--task" => {
                filter.task = Some(TaskId(value.parse().map_err(|_| "诏令 ID 必须是数字")?))
            }
            "--official" => filter.agent = Some(agent_id(value)?),
            "--kind" => filter.kind = Some(value.to_lowercase()),
            "--since" => filter.since = Some(parse_time(value, now, offset)?),
            "--until" => filter.until = Some(parse_time(value, now, offset)?),
            _ => return Err(USAGE.into()),
        }
    }
    Ok(Command::History(filter))
}

pub(crate) fn parse_run(args: Vec<String>) -> Result<Command, Box<dyn Error>> {
    const USAGE: &str = "用法: ziyuan run [--jobs <并发数>] [--timeout <秒>] \
                         [--watch [--poll <秒>] [--no-censor]]（数值均须为正整数）";
    let mut limits = DEFAULT_LIMITS;
    let (mut watch, mut poll, mut censor) = (false, None, true);
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--watch" => watch = true,
            "--no-censor" => censor = false,
            "--jobs" | "--timeout" | "--poll" => {
                let value = rest
                    .next()
                    .and_then(|v| v.parse::<u64>().ok())
                    .filter(|v| *v > 0)
                    .ok_or(USAGE)?;
                match flag.as_str() {
                    "--jobs" => limits.max_concurrent = value as usize,
                    "--timeout" => limits.timeout = Duration::from_secs(value),
                    _ => poll = Some(Duration::from_secs(value)),
                }
            }
            _ => return Err(USAGE.into()),
        }
    }
    if !watch {
        if poll.is_some() || !censor {
            return Err("--poll 与 --no-censor 只用于常朝（--watch）".into());
        }
        return Ok(Command::Run(limits, None));
    }
    let options = WatchOptions {
        poll: poll.unwrap_or(Duration::from_secs(5)),
        censor: if censor {
            Some(env::current_dir()?)
        } else {
            None
        },
    };
    Ok(Command::Run(limits, Some(options)))
}

/// `edict --template` 的模板名，以及哪些项由命令行显式给出（其余取模板的）。
#[derive(Debug)]
pub(crate) struct TemplateChoice {
    pub(crate) name: String,
    pub(crate) department: bool,
    pub(crate) tools: bool,
    pub(crate) mcp: bool,
    pub(crate) scope: bool,
}

pub(crate) fn parse_edict(mut args: Vec<String>) -> Result<Command, Box<dyn Error>> {
    const USAGE: &str = "用法: ziyuan edict [--template <模板>] [--dept works] [--scope <路径>]... [--tools <工具,…>|none] [--mcp <授权,…>] [--queue] <诏令内容>";
    let mut department = None;
    let mut queue = false;
    let mut seal = Seal::default();
    let mut scope = Vec::new();
    let (mut template, mut tools_given, mut mcp_given) = (None, false, false);
    while let Some(flag) = args.first().cloned() {
        match flag.as_str() {
            "--queue" => {
                args.remove(0);
                queue = true;
            }
            "--dept" | "--scope" | "--tools" | "--template" | "--mcp" => {
                if args.len() < 2 {
                    return Err(USAGE.into());
                }
                args.remove(0);
                let value = args.remove(0);
                match flag.as_str() {
                    "--dept" => department = Some(value.parse::<Department>()?),
                    "--scope" => scope.push(value),
                    "--template" => template = Some(value),
                    "--mcp" => {
                        seal.mcp = parse_mcp(&value);
                        mcp_given = true;
                    }
                    _ => {
                        seal.capabilities = parse_capabilities(&value)?;
                        tools_given = true;
                    }
                }
            }
            _ => break,
        }
    }
    let template = template.map(|name| TemplateChoice {
        name,
        department: department.is_some(),
        tools: tools_given,
        mcp: mcp_given,
        scope: !scope.is_empty(),
    });
    if args.is_empty() && template.is_none() {
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
        template,
    })
}

/// `template`（列出）、`template save <名> [--dept] [--scope]... [--tools] --criteria <甲；乙> <内容>`、
/// `template rm <名>`。
pub(crate) fn parse_template(mut args: Vec<String>) -> Result<Command, Box<dyn Error>> {
    const USAGE: &str = "用法: ziyuan template | template save <名> [--dept works] [--scope <路径>]... [--tools <工具,…>|none] [--mcp <授权,…>] --criteria <甲；乙> <内容> | template rm <名>";
    match args.first().map(String::as_str) {
        None => return Ok(Command::Templates),
        Some("rm") if args.len() == 2 => return Ok(Command::RemoveTemplate(args.remove(1))),
        Some("save") if args.len() >= 2 => {}
        _ => return Err(USAGE.into()),
    }
    args.remove(0);
    let name = args.remove(0);
    let mut template = Template {
        department: Department::Works,
        text: String::new(),
        seal: Seal::default(),
        criteria: Vec::new(),
    };
    let mut scope = Vec::new();
    while let Some(flag) = args.first().cloned() {
        if !matches!(
            flag.as_str(),
            "--dept" | "--scope" | "--tools" | "--mcp" | "--criteria"
        ) {
            break;
        }
        if args.len() < 2 {
            return Err(USAGE.into());
        }
        args.remove(0);
        let value = args.remove(0);
        match flag.as_str() {
            "--dept" => template.department = value.parse()?,
            "--scope" => scope.push(value),
            "--tools" => template.seal.capabilities = parse_capabilities(&value)?,
            "--mcp" => template.seal.mcp = parse_mcp(&value),
            _ => {
                template.criteria = value
                    .split(['；', ';'])
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .map(str::to_owned)
                    .collect()
            }
        }
    }
    if !scope.is_empty() {
        template.seal.scope = scope;
    }
    template.text = args.join(" ");
    if template.text.trim().is_empty() || template.criteria.is_empty() {
        return Err(USAGE.into());
    }
    Ok(Command::SaveTemplate { name, template })
}

/// `post [--dept works] [--station 驿站] [--urgent] <内容|->`；`-` 从标准输入读取内容。
pub(crate) fn parse_post(mut args: Vec<String>) -> Result<Command, Box<dyn Error>> {
    const USAGE: &str =
        "用法: ziyuan post [--dept works] [--station 驿站] [--urgent] <内容|-（从标准输入读取）>";
    let (mut department, mut station, mut urgent) = (Department::Works, "驿站".to_owned(), false);
    while let Some(flag) = args.first().cloned() {
        match flag.as_str() {
            "--urgent" => {
                args.remove(0);
                urgent = true;
            }
            "--dept" | "--station" if args.len() >= 2 => {
                args.remove(0);
                let value = args.remove(0);
                if flag == "--dept" {
                    department = value.parse()?;
                } else {
                    station = value;
                }
            }
            "--dept" | "--station" => return Err(USAGE.into()),
            _ => break,
        }
    }
    let text = match args.as_slice() {
        [] => return Err(USAGE.into()),
        [dash] if dash == "-" => {
            let mut text = String::new();
            io::Read::read_to_string(&mut io::stdin(), &mut text)?;
            text.trim().to_owned()
        }
        _ => args.join(" "),
    };
    Ok(Command::Post {
        department,
        station,
        urgent,
        text,
    })
}

/// `ceiling <官职> default`，或 `--scope`/`--tools`（与诏令玉玺同义；未给出的一项不设限）。
pub(crate) fn parse_ceiling(mut args: Vec<String>) -> Result<Command, Box<dyn Error>> {
    const USAGE: &str = "用法: ziyuan ceiling <官职> [--scope <路径>]... [--tools <工具,…>|none] [--mcp <授权,…>|none] | default";
    if args.is_empty() {
        return Err(USAGE.into());
    }
    let office: Office = args.remove(0).parse()?;
    if args == ["default"] {
        return Ok(Command::Ceiling(office, None));
    }
    // 未给 --mcp 时外藩不设限（`*`），诏令仍须逐件授权。
    let mut seal = Seal {
        mcp: ["*".to_owned()].into(),
        ..Seal::default()
    };
    let mut scope = Vec::new();
    for pair in args.chunks(2) {
        match pair {
            [flag, value] if flag == "--scope" => scope.push(value.clone()),
            [flag, value] if flag == "--tools" => seal.capabilities = parse_capabilities(value)?,
            [flag, value] if flag == "--mcp" => seal.mcp = parse_mcp(value),
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

/// 外藩授权：`none` 为不授；否则为逗号分隔的 `服务`、`服务/工具` 或 `*`（合法性由治理层校验）。
pub(crate) fn parse_mcp(value: &str) -> BTreeSet<String> {
    if value == "none" {
        return BTreeSet::new();
    }
    value
        .split([',', '，'])
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `none` 表示不授予任何工具；否则为逗号分隔的工具名。
pub(crate) fn parse_capabilities(value: &str) -> Result<BTreeSet<Capability>, Box<dyn Error>> {
    if value == "none" {
        return Ok(BTreeSet::new());
    }
    Ok(value
        .split(',')
        .map(str::parse::<Capability>)
        .collect::<Result<_, _>>()?)
}
