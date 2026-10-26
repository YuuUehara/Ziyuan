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
    ABOLITION_NOTE, EXECUTOR_BUSY, HistoryFilter, RunUpdate, Session, TaskReport, WatchOptions,
    WatchUpdate, demotion_note, describe_record, duzhi_report, estimate_cost, format_time,
    history_entries, kaocheng_grade, kaocheng_report, parse_time, render_diffs, seal_note,
    skipped_note, suspicion_status, tz_offset,
};
use ziyuan_core::{
    AgentId, BudgetScope, Capability, Department, GovernanceMode, Office, Persona, Record, Seal,
    TaskId, Template, Verdict, missing_sections, search_records,
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
  ziyuan edict [--template <模板>] [--dept works] [--scope <路径>]... [--tools <工具>] [--mcp <授权>] [--queue] <内容>
                                       下诏并立即执行；--queue 只入队（默认模拟后端；见下方环境变量）
  ziyuan run [--jobs 4] [--timeout 300]
                                       并发执行全部未结束的诏令；Ctrl-C 撤销执行中的诏令
  ziyuan run --watch [--poll 5] [--no-censor]
                                       常朝：常驻执行，驿传与入队的诏令随到随办，办完自动请都察院复核；Ctrl-C 退朝
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
  ziyuan ceiling <官职> [--scope <路径>]... [--tools <工具>] [--mcp <授权>] | default
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
  ziyuan apply <诏令ID> [--skip 1,3]    准奏：把呈报的改动写入项目（文件在此期间被改过则拒绝）；
                                       --skip 逐段驳回 diff 中所列段号，其余落盘
  ziyuan discard <诏令ID>               驳回呈报的改动，不写入
  ziyuan review <诏令ID> <accept|reject> [备注]
                                       评定已完成的诏令（吏部考绩只采信评定）
  ziyuan censor <诏令ID|all>            都察院复核回奏：核验引用与测试，御史背书或弹劾
  ziyuan budget [court|<官职>|<诏令ID> <tokens|none>]
                                       户部度支：设朝廷总额、官职每件或某件诏令的 token 上限，用尽即停支；
                                       不带参数则出具度支奏销（按用途、模型、官员汇总花费）
  ziyuan post [--dept works] [--station 驿站] [--urgent] <内容|->
                                         兵部驿传：外部事件（CI 失败、新 issue）入队为诏令，`-` 读标准输入；
                                         默认玉玺（只读），仍须批红、受度支约束；只入队，不执行
  ziyuan urgent <诏令ID>                 八百里加急：插队先行，时限加倍
  ziyuan template                        翰林院：列出诏令模板（内置「审查unsafe」「补测试」「写文档」）
  ziyuan template save <名> [--dept works] [--scope <路径>]... [--tools <工具,…>|none] --criteria <甲；乙> <内容>
                                         存模板（同名覆盖，可覆盖内置的）；验收标准以「；」分条
  ziyuan template rm <名>                撤去自定的模板（内置的恢复原样）
  ziyuan edict --template <名> [补充内容]  按模板下诏；--dept/--scope/--tools 可覆盖模板的默认值
  ziyuan exam <官员ID>                   礼部科举：官员应试 .ziyuan/exam/ 下的考题，按要点判分，记入履历
  ziyuan exam --palace <题目>            殿试：天子亲策，在任官员各答一卷
  ziyuan crown <官员ID>                  钦点状元（须为最近一次殿试的答卷者）
  ziyuan nominate <官职> [--lots]        廷推：官缺时吏部会同九卿各推一人（只是建议）；--lots 枚卜，从被推者中抽签任命
  ziyuan cane <官员ID>                   廷杖：屡遭弹劾（两次以上）者降一级留用，收回 write_file、run_tests，三件获采纳即复
  ziyuan pardon <官员ID>                 宽宥受杖者，复其品级与玉玺
  ziyuan flags [诏令ID]                  刑部与锦衣卫的密报：外来文书中的劫持指令、回奏中的越权与索取机密
  ziyuan mcp                            外藩名册：.ziyuan/mcp.json 登记的 MCP 服务及其工具
  ziyuan changelog [--version 0.5.0] [--since v0.4.0]
                                       礼部修 CHANGELOG：据提交与实录起草，只可写 CHANGELOG.md，须准奏落盘（只入队）
  ziyuan github pull [--label ziyuan] [--dept works]
                                       兵部驿站·GitHub：带标签的 issue/PR 及成员的 `/ziyuan` 评论入队（去重）
  ziyuan github reply <诏令ID|all>      已评定、无未平反密报的回奏写回 issue/PR 评论
  ziyuan github pr <诏令ID>             准奏的改动在远端开为草稿 PR（先核对远端原文；不碰本地版本库）
  ziyuan recheck <诏令ID|all>            刑部复勘：刑部官员（模型）逐处判断检出是真有其事还是误报，只是意见
  ziyuan acquit <诏令ID> [序号,…|all]    平反：不给序号则平反刑部判为误报的各处；平反后不再妨碍代批
  ziyuan kaocheng                      考成簿：在任官员的功过与等第（仿张居正考成法）
  ziyuan examine <官员ID>               吏部考绩：经评定与复核的履历
  ziyuan recommend <官职>               吏部荐举：按履历排列可任该职者（不自动任命）
  ziyuan history [--task ID] [--official ID] [--kind 类型] [--since 时间] [--until 时间]
                                       查看起居注（事件记录）；时间如 30m、2h、3d、2026-10-25、2026-10-25 14:30，
                                       按 ZIYUAN_TZ（如 +08:00，默认 UTC）
  ziyuan records [检索词]                翰林院实录：已结诏令的诏令、回奏与评定（只增不改）；给检索词则查前例
  ziyuan <court|officials|tasks|history|records> --json
                                       以 JSON 输出（court 与 Web 的 /api/state 同构），便于脚本使用
  ziyuan completions <bash|zsh|fish>   输出 shell 补全脚本（如 `ziyuan completions bash > ~/.local/share/bash-completion/completions/ziyuan`）
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
  --mcp <授权>     外藩 MCP 工具（.ziyuan/mcp.json 登记的服务）：服务、服务/工具 或 * 的逗号列表；
                   默认不授。外部工具不受 --scope 约束，只授信得过的服务
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
  ZIYUAN_LLM_RETRIES   网络错误、429、5xx 时的重试次数（默认 3，指数退避，遵守 Retry-After）
  ZIYUAN_MAX_STEPS     单件诏令最多的模型调用步数（默认 12）；用尽后官员不带工具据已查阅的内容回奏

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

/// 单件诏令默认最多的模型调用步数（`ZIYUAN_MAX_STEPS` 可改）。
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
    Apply(TaskId, Vec<u32>),
    Discard(TaskId),
    Recommend(Office),
    History(HistoryFilter),
    /// 翰林院实录：检索词为空则列出全部。
    Records(String),
    /// 输出 shell 补全脚本。
    Completions(Shell),
}

#[derive(Clone, Copy)]
enum Shell {
    Bash,
    Zsh,
    Fish,
}

#[derive(Debug, PartialEq, Eq)]
enum GitHubAction {
    /// 带标签的 issue / PR 及成员的 `/ziyuan` 评论入队（交给 `department`）。
    Pull {
        label: String,
        department: Department,
    },
    /// 回奏写回评论；`None` 为全部可回报的。
    Reply(Option<TaskId>),
    /// 准奏的改动开为草稿 PR。
    PullRequest(TaskId),
}

/// 全部子命令（补全用；测试保证每个都能被 `parse` 识别）。
const SUBCOMMANDS: &[&str] = &[
    "help",
    "init",
    "court",
    "officials",
    "history",
    "records",
    "recruit",
    "edict",
    "template",
    "run",
    "tasks",
    "tui",
    "serve",
    "cancel",
    "retreat",
    "return",
    "reform",
    "dismiss",
    "purge",
    "abolish",
    "model",
    "ceiling",
    "draft",
    "redraft",
    "approve",
    "appoint",
    "examine",
    "diff",
    "kaocheng",
    "post",
    "urgent",
    "exam",
    "crown",
    "nominate",
    "cane",
    "pardon",
    "flags",
    "recheck",
    "acquit",
    "github",
    "changelog",
    "mcp",
    "budget",
    "split",
    "debate",
    "censor",
    "apply",
    "discard",
    "review",
    "recommend",
    "completions",
];

/// 支持 `--json` 的子命令。
const JSON_COMMANDS: &[&str] = &["court", "officials", "tasks", "history", "records"];

fn main() -> ExitCode {
    quiet_broken_pipe();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("错误：{error}");
            ExitCode::FAILURE
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
        return Err(format!("--json 只支持 {}", JSON_COMMANDS.join("、")).into());
    }
    let command = parse(args)?;
    let log = EventLog::new(&env::current_dir()?);
    if json {
        let session = Session::open(log)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&json_output(&session, &command)?)?
        );
        return Ok(());
    }
    match command {
        Command::Help => println!("{HELP}"),
        Command::Completions(shell) => print!("{}", completions(shell)),
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
            const USAGE: &str = "用法: ziyuan budget [court|<官职>|<诏令ID> <tokens|none>]";
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
                        "none" => None,
                        n => Some(n.parse().map_err(|_| "额度须为 tokens 数或 none")?),
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
            const USAGE: &str = "用法: ziyuan github pull [--label ziyuan] [--dept works] | reply <诏令ID|all> | pr <诏令ID>";
            let id = |s: &str| -> Result<TaskId, Box<dyn Error>> {
                Ok(TaskId(s.parse().map_err(|_| "诏令 ID 必须是数字")?))
            };
            match args.first().map(String::as_str) {
                Some("pull") => {
                    let (mut label, mut department) =
                        (ziyuan_github::DEFAULT_LABEL.to_owned(), Department::Works);
                    let mut rest = args[1..].iter();
                    while let Some(flag) = rest.next() {
                        let value = rest.next().ok_or(USAGE)?;
                        match flag.as_str() {
                            "--label" => label = value.clone(),
                            "--dept" => department = value.parse()?,
                            _ => return Err(USAGE.into()),
                        }
                    }
                    Command::GitHub(GitHubAction::Pull { label, department })
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
            const USAGE: &str = "用法: ziyuan apply <诏令ID> [--skip <段号,…>]";
            match args.as_slice() {
                [id] => Command::Apply(task_id(std::slice::from_ref(id), USAGE)?, Vec::new()),
                [id, flag, list] if flag == "--skip" => Command::Apply(
                    task_id(std::slice::from_ref(id), USAGE)?,
                    list.split([',', '，'])
                        .filter(|n| !n.trim().is_empty())
                        .map(|n| n.trim().parse().map_err(|_| format!("段号必须是数字：{n}")))
                        .collect::<Result<_, _>>()?,
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

fn execute(session: &mut Session<EventLog>, command: Command) -> Result<(), Box<dyn Error>> {
    match command {
        Command::Help | Command::Init | Command::Completions(_) => {
            unreachable!("在 run 中处理")
        }
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
            if let (Some(limit), Some(left)) =
                (court.budget(BudgetScope::Court), court.treasury_remaining())
            {
                println!("朝廷度支：{limit} tokens，余 {left}（`ziyuan budget` 查看奏销）");
            }
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
                    Some(o) => match demotion_note(court, agent.id) {
                        Some(note) => format!("{}（{}，{note}）", o.label(), o.rank(court.mode())),
                        None => format!("{}（{}）", o.label(), o.rank(court.mode())),
                    },
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
            mut seal,
            template,
        } => {
            // 先取得执行者锁再下诏，避免被拒绝的诏令残留在队列中。
            let executor = if queue { None } else { Some(lock_executor()?) };
            let edict = match template {
                None => session.issue(department, text, seal)?,
                Some(choice) => {
                    let defaults = session.court().template(&choice.name).ok_or(format!(
                        "翰林院没有模板「{}」；`ziyuan template` 查看",
                        choice.name
                    ))?;
                    if !choice.tools {
                        seal.capabilities = defaults.seal.capabilities;
                    }
                    if !choice.mcp {
                        seal.mcp = defaults.seal.mcp;
                    }
                    if !choice.scope {
                        seal.scope = defaults.seal.scope;
                    }
                    let department = if choice.department {
                        department
                    } else {
                        defaults.department
                    };
                    // 命令行给出的内容附在模板内容之后。
                    let text = match defaults.text.chars().last() {
                        _ if text.trim().is_empty() => defaults.text,
                        Some(c) if c.is_ascii_alphanumeric() => format!("{} {text}", defaults.text),
                        _ => format!("{}{text}", defaults.text),
                    };
                    session.issue_templated(&choice.name, department, text, seal)?
                }
            };
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
        Command::Run(limits, Some(options)) => {
            let _executor = lock_executor()?;
            hold_court(session, worker_backend()?, limits, &options)?;
        }
        Command::Run(limits, None) => {
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
                        // 礼部：回奏发回重写后仍不合体例的，照收并标注。
                        let style = match &task.status {
                            TaskStatus::Completed { summary, .. }
                                if !missing_sections(summary).is_empty() =>
                            {
                                "；体例不合"
                            }
                            _ => "",
                        };
                        format!(
                            "已完成（{}），{review}{censure}{style}{}",
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
                let flagged = match task.suspicions.len() {
                    0 => String::new(),
                    n => format!("（密报 {n} 条，`ziyuan flags {}`）", task.edict.id.0),
                };
                let debated = match task.debates.len() {
                    0 => String::new(),
                    n => format!("（廷议 {n} 场）"),
                };
                // 兵部：加急与驿传来处标在部门之前。
                let military = format!(
                    "{}{}",
                    if task.urgent {
                        "【八百里加急】"
                    } else {
                        ""
                    },
                    task.courier
                        .as_ref()
                        .map_or(String::new(), |c| format!("【驿传·{c}】"))
                ) + &task
                    .template
                    .as_ref()
                    .map_or(String::new(), |t| format!("【模板·{}】", t.name));
                println!(
                    "#{:<3} {parent}{military}{} {}  —  {}{flagged}{debated}{usage}",
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
            print!("{}", render_diffs(&changes.files).concat());
            if changes.state == ChangeState::Proposed {
                println!(
                    "逐段准奏：`ziyuan apply {} --skip <段号,…>` 驳回所列各段，其余落盘。",
                    task.0
                );
            }
        }
        Command::Apply(task, skipped) => {
            session.apply_changes(task, &env::current_dir()?, skipped)?;
            let skipped = session
                .court()
                .task(task)
                .and_then(|t| t.changes.as_ref())
                .map_or(String::new(), |c| skipped_note(&c.skipped));
            println!(
                "诏令已颁：准奏，诏令 #{} 的改动已写入项目{skipped}。",
                task.0
            );
        }
        Command::Discard(task) => {
            session.discard_changes(task)?;
            println!("诏令已颁：驳回诏令 #{} 的改动，未写入。", task.0);
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
        Command::Post {
            department,
            station,
            urgent,
            text,
        } => {
            let edict = session.post(department, text, &station, urgent)?;
            let task = session.court().task(edict.id).expect("刚入队");
            let speed = if urgent { "，八百里加急" } else { "" };
            println!(
                "驿传：诏令 #{} 自「{station}」入队（{}{speed}）。",
                edict.id.0,
                department.label()
            );
            if !task.suspicions.is_empty() {
                println!(
                    "锦衣卫密报 {} 条，`ziyuan flags {}` 查看；修道期间司礼监不代批红。",
                    task.suspicions.len(),
                    edict.id.0
                );
            }
            if session.court().needs_approval(task) {
                println!("待批红：`ziyuan approve {}`。", edict.id.0);
            }
        }
        Command::Templates => {
            for (name, t, builtin) in session.court().templates() {
                let origin = if builtin { "内置" } else { "自定" };
                println!(
                    "「{name}」（{origin}，{}，玉玺 {}）",
                    t.department.label(),
                    t.seal
                );
                println!("  {}", t.text);
                for (i, c) in t.criteria.iter().enumerate() {
                    println!("  验收 {}. {c}", i + 1);
                }
            }
            println!(
                "用法：`ziyuan edict --template <名> [补充内容]`；验收标准随诏令附给承办官员，`ziyuan censor` 时都察院逐条核验。"
            );
        }
        Command::SaveTemplate { name, template } => {
            let overrides = session.court().template(&name).is_some();
            session.save_template(&name, template)?;
            let verb = if overrides { "覆盖" } else { "新存" };
            println!("翰林院{verb}模板「{name}」。");
        }
        Command::RemoveTemplate(name) => {
            session.remove_template(&name)?;
            println!("翰林院撤去模板「{name}」。");
        }
        Command::Urgent(task) => {
            session.expedite(task)?;
            println!("诏令 #{} 八百里加急：插队先行，时限加倍。", task.0);
        }
        Command::Exam(agent) => {
            let backend = worker_backend()?;
            let name = session
                .court()
                .agent(agent)
                .map_or("?".to_owned(), |a| a.name.clone());
            println!("礼部开科：#{} {name} 应试。", agent.0);
            let taken = session.exam(
                agent,
                &env::current_dir()?,
                backend.as_ref(),
                |paper, score| {
                    let mark = if score >= 60 { "及格" } else { "不及格" };
                    println!("  「{}」{score} 分（{mark}）", paper.name);
                },
            )?;
            let record = session.court().service_record(agent);
            println!(
                "考毕 {taken} 题；履历：科举及格 {}/{}。成绩供吏部荐举参考，任命仍由皇帝决定。",
                record.exams_passed, record.exams
            );
        }
        Command::Palace(question) => {
            let backend = worker_backend()?;
            println!("殿试，天子亲策：{question}");
            let stopped = session.palace_exam(&question, backend.as_ref(), |agent, answer| {
                println!(
                    "【#{} {}{}·{}】{}",
                    agent.id.0,
                    answer.office.label(),
                    agent.name,
                    Persona::of(agent.id).label(),
                    answer.text
                );
                let _ = io::stdout().flush();
            })?;
            if let Some(reason) = stopped {
                println!("殿试中止：{reason}；已交的答卷照记。");
            }
            println!("请 `ziyuan crown <官员ID>` 钦点状元。");
        }
        Command::Crown(agent) => println!("{}", session.crown(agent)?),
        Command::Flags(only) => {
            let court = session.court();
            let mut any = false;
            for task in court
                .tasks()
                .filter(|t| only.is_none_or(|id| t.edict.id == id))
            {
                for (i, s) in task.suspicions.iter().enumerate() {
                    any = true;
                    let source = s.origin();
                    println!(
                        "诏令 #{} 第 {} 处 {}（{source}）：{}「{}」{}",
                        task.edict.id.0,
                        i + 1,
                        s.bureau(),
                        s.threat.label(),
                        s.excerpt,
                        suspicion_status(s)
                    );
                }
            }
            if !any {
                println!("刑部与锦衣卫无所奏报。");
            } else {
                println!(
                    "（关键词机械检出，可能误报；玉玺不因此改变。可 `ziyuan recheck <诏令ID>` 请刑部复勘，\
                     `ziyuan acquit <诏令ID> [序号]` 平反误报。）"
                );
            }
        }
        Command::GitHub(action) => github(session, action)?,
        Command::Mcp => {
            let servers =
                ziyuan_runtime::mcp::survey(&env::current_dir()?).map_err(|e| e.to_string())?;
            if servers.is_empty() {
                println!(
                    "外藩名册为空：在 {} 登记 MCP 服务，下诏时以 `--mcp <服务>` 授权。",
                    ziyuan_runtime::mcp::CONFIG
                );
            }
            for (name, tools) in servers {
                match tools {
                    Ok(tools) => {
                        println!("外藩「{name}」：{} 件工具", tools.len());
                        for t in tools {
                            let line = t.description.lines().next().unwrap_or_default();
                            println!("  {name}/{}  {line}", t.name);
                        }
                    }
                    Err(e) => println!("外藩「{name}」：未能朝贡（{e}）"),
                }
            }
        }
        Command::Changelog { version, since } => {
            let (range, commits) = changelog_commits(since)?;
            let edict = session.draft_changelog(&version, &commits)?;
            println!(
                "诏令 #{} 已入队：礼部修 CHANGELOG「{version}」，收提交 {} 条（{range}）。玉玺只授 read_file、write_file，范围只限 {}。",
                edict.id.0,
                commits.len(),
                ziyuan_core::CHANGELOG
            );
            if session
                .court()
                .needs_approval(session.court().task(edict.id).expect("刚入队"))
            {
                println!("待批红：`ziyuan approve {}`。", edict.id.0);
            }
            println!(
                "执行 `ziyuan run` 后，`ziyuan diff {0}` 审阅，`ziyuan apply {0}` 准奏落盘。",
                edict.id.0
            );
            println!("{}", ziyuan_app::CHANGELOG_NOTE);
        }
        Command::Recheck(only) => {
            let tasks: Vec<TaskId> = match only {
                Some(task) => vec![task],
                None => session
                    .court()
                    .tasks()
                    .filter(|t| {
                        t.suspicions
                            .iter()
                            .any(|s| s.review.is_none() && !s.acquitted)
                    })
                    .map(|t| t.edict.id)
                    .collect(),
            };
            if tasks.is_empty() {
                println!("没有待复勘的检出。");
            } else {
                let backend = worker_backend()?;
                for task in tasks {
                    session.recheck(task, backend.as_ref())?;
                    let court = session.court();
                    let entry = court.task(task).expect("刚复勘");
                    for (i, s) in entry.suspicions.iter().enumerate() {
                        let Some(r) = &s.review else { continue };
                        let name = court.agent(r.reviewer).map_or("?", |a| a.name.as_str());
                        println!(
                            "诏令 #{} 第 {} 处 {}「{}」：刑部{name}复勘为{}——{}",
                            task.0,
                            i + 1,
                            s.threat.label(),
                            s.excerpt,
                            if r.upheld { "维持" } else { "误报" },
                            r.note
                        );
                    }
                }
                println!(
                    "复勘只是意见：平反请 `ziyuan acquit <诏令ID>`（平反刑部判为误报的各处）。"
                );
            }
        }
        Command::Acquit(task, indices) => {
            let indices = match indices {
                Some(list) if list.is_empty() => {
                    let entry = session
                        .court()
                        .task(task)
                        .ok_or(format!("诏令 #{} 不存在", task.0))?;
                    Some(
                        (0..entry.suspicions.len() as u32)
                            .filter(|&i| !entry.suspicions[i as usize].acquitted)
                            .collect(),
                    )
                }
                other => other,
            };
            let (done, note) = session.acquit(task, indices)?;
            let list: Vec<String> = done.iter().map(|i| (i + 1).to_string()).collect();
            println!(
                "诏令已颁：平反诏令 #{} 第 {} 处检出，不再妨碍司礼监代批。",
                task.0,
                list.join("、")
            );
            println!("{note}");
        }
        Command::Budget(None) => {
            let default = OpenAiCompatible::from_env().map(|m| m.model().to_owned());
            println!(
                "{}",
                duzhi_report(session.court(), default.as_deref(), price_for)
            );
        }
        Command::Budget(Some((scope, tokens))) => {
            session.set_budget(scope, tokens)?;
            match tokens {
                Some(t) => println!("诏令已颁：户部度支 · {}：限 {t} tokens。", scope.label()),
                None => println!("诏令已颁：户部度支 · {}：取消限额。", scope.label()),
            }
        }
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
            let note = session.appoint(id, office)?;
            println!("诏令已颁：任命官员 #{} 为{}。", id.0, office.label());
            if let Some(note) = note {
                println!("{note}");
            }
        }
        Command::Nominate { office, lots: true } => {
            let (agent, note) = session.draw_lots(office)?;
            println!("诏令已颁：枚卜得官员 #{}，任{}。", agent.0, office.label());
            println!("{note}");
        }
        Command::Nominate {
            office,
            lots: false,
        } => {
            let backend = worker_backend()?;
            println!("吏部会同九卿，廷推{}：", office.label());
            let mut lines = Vec::new();
            let votes = session.nominate(office, backend.as_ref(), |voter, vote| {
                lines.push((voter.name.clone(), vote.candidate, vote.note.clone()));
            })?;
            let names = |id: AgentId| {
                session
                    .court()
                    .agent(id)
                    .map_or("?".to_owned(), |a| format!("#{} {}", id.0, a.name))
            };
            for (voter, candidate, note) in lines {
                let note = if note.is_empty() {
                    String::new()
                } else {
                    format!("——{note}")
                };
                println!("  {voter} 推 {}{note}", names(candidate));
            }
            let tally: Vec<String> = session
                .court()
                .nomination_tally(office)
                .into_iter()
                .map(|(id, n)| format!("{} {n} 票", names(id)))
                .collect();
            println!("得票：{}（共 {} 票）", tally.join("、"), votes.len());
            println!(
                "廷推只是建议：`ziyuan appoint <官员ID> {}` 任命（被推者现任他职的径直擢升），或 `ziyuan nominate {} --lots` 枚卜。",
                office.code(),
                office.code()
            );
        }
        Command::Cane(id) => println!("{}", session.cane(id)?),
        Command::Pardon(id) => println!("{}", session.pardon(id)?),
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
        Command::Records(query) => {
            let records = session.court().records();
            let found = found_records(&records, &query);
            if found.is_empty() {
                println!(
                    "{}",
                    if records.is_empty() {
                        "翰林院尚无实录：诏令完成后自动归档。"
                    } else {
                        "实录中无相关前例。"
                    }
                );
            }
            for r in found {
                println!(
                    "#{} {} · #{} {} · {}",
                    r.task.0,
                    r.department.label(),
                    r.worker.0,
                    r.handler,
                    r.verdict
                );
                println!("    诏令：{}", r.edict);
                let summary = r
                    .summary
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("");
                let short: String = summary.trim().chars().take(80).collect();
                let more = if short.len() < summary.trim().len() {
                    "…"
                } else {
                    ""
                };
                println!("    回奏：{short}{more}");
            }
        }
        Command::History(filter) => {
            let offset = tz_offset();
            for entry in history_entries(session.history(), &filter)? {
                let at = entry.at.map_or("—".repeat(16), |t| format_time(t, offset));
                println!("{:>3}: {at}  {}", entry.seq, entry.text);
            }
        }
    }
    Ok(())
}

/// 礼部修 CHANGELOG 所据的提交（`git log`，新者在前，至多 200 条，不含合并提交）及所取范围。
/// 未给起点时取最近的 tag 以来；没有 tag 则取全部。
fn changelog_commits(since: Option<String>) -> Result<(String, Vec<String>), Box<dyn Error>> {
    let git = |args: &[&str]| -> Result<String, Box<dyn Error>> {
        let out = std::process::Command::new("git")
            .args(args)
            .output()
            .map_err(|e| format!("无法运行 git：{e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!("git {} 失败：{}", args.join(" "), err.trim()).into());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let since = match since {
        Some(since) => Some(since),
        None => git(&["describe", "--tags", "--abbrev=0"])
            .ok()
            .map(|t| t.trim().to_owned()),
    };
    let range = since.map_or("HEAD".to_owned(), |s| format!("{s}..HEAD"));
    let log = git(&["log", "--no-merges", "-n", "200", "--format=%h %s", &range])?;
    let commits: Vec<String> = log
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect();
    if commits.is_empty() {
        return Err(format!("{range} 没有可记的提交").into());
    }
    Ok((range, commits))
}

/// 兵部驿站 · GitHub。凭据只在 `ziyuan_github` 中使用，不经朝廷与模型。
fn github(session: &mut Session<EventLog>, action: GitHubAction) -> Result<(), Box<dyn Error>> {
    let hub = ziyuan_github::GitHub::from_env()?
        .ok_or("未设 ZIYUAN_GITHUB_REPO（owner/repo）与 ZIYUAN_GITHUB_TOKEN，驿站不通 GitHub")?;
    match action {
        GitHubAction::Pull { label, department } => {
            let mut queued = 0;
            let issues = hub.labeled(&label)?;
            for issue in &issues {
                let comments = hub.comments(issue.number)?;
                for d in ziyuan_github::intake(hub.repo(), issue, &comments) {
                    let known = session
                        .court()
                        .tasks()
                        .any(|t| t.courier.as_deref() == Some(d.station.as_str()));
                    if known {
                        continue;
                    }
                    let edict = session.post(department, d.text, &d.station, false)?;
                    queued += 1;
                    let flagged = session
                        .court()
                        .task(edict.id)
                        .map_or(0, |t| t.suspicions.len());
                    let note = if flagged > 0 {
                        format!("，锦衣卫密报 {flagged} 条")
                    } else {
                        String::new()
                    };
                    println!("驿传：诏令 #{} 自「{}」入队{note}。", edict.id.0, d.station);
                }
            }
            println!(
                "GitHub {}：标签「{label}」的 issue/PR {} 件，新入队 {queued} 件（只入队，执行请 `ziyuan run`）。",
                hub.repo(),
                issues.len()
            );
        }
        GitHubAction::Reply(only) => {
            let tasks: Vec<TaskId> = match only {
                Some(task) => {
                    session.check_reply(task, false)?;
                    vec![task]
                }
                None => {
                    let ids: Vec<TaskId> = session.court().tasks().map(|t| t.edict.id).collect();
                    ids.into_iter()
                        .filter(|id| session.check_reply(*id, false).is_ok())
                        .collect()
                }
            };
            let mut sent = 0;
            for task in tasks {
                let entry = session.court().task(task).expect("已校验");
                let station = entry.courier.clone().unwrap_or_default();
                let Some((repo, number)) = ziyuan_github::parse_station(&station) else {
                    if only.is_some() {
                        return Err(format!(
                            "诏令 #{} 来自「{station}」，不是 GitHub 驿站",
                            task.0
                        )
                        .into());
                    }
                    continue;
                };
                if repo != hub.repo() {
                    if only.is_some() {
                        return Err(format!(
                            "诏令 #{} 来自 {repo}，与 ZIYUAN_GITHUB_REPO 不符",
                            task.0
                        )
                        .into());
                    }
                    continue;
                }
                let url = hub.comment(number, &ziyuan_app::reply_text(session.court(), entry))?;
                session.record_reply(task, false, url.clone())?;
                sent += 1;
                println!("诏令 #{} 回奏已送回 #{number}：{url}", task.0);
            }
            if sent == 0 {
                println!(
                    "没有可回报的诏令（须经 GitHub 驿传入队、已评定、无未平反的密报、尚未回报）。"
                );
            }
        }
        GitHubAction::PullRequest(task) => {
            session.check_reply(task, true)?;
            let entry = session.court().task(task).expect("已校验");
            let station = entry.courier.clone().unwrap_or_default();
            let number = match ziyuan_github::parse_station(&station) {
                Some((repo, number)) if repo == hub.repo() => number,
                _ => {
                    return Err(format!(
                        "诏令 #{} 来自「{station}」，不是本仓库的 GitHub 驿站",
                        task.0
                    )
                    .into());
                }
            };
            let changes = entry.changes.as_ref().expect("已校验有准奏的改动");
            let files = ziyuan_app::selected_files(&changes.files, &changes.skipped);
            let first: String = entry
                .edict
                .text
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(60)
                .collect();
            let title = format!("紫垣诏令 #{}：{first}", task.0);
            let body = format!(
                "Refs #{number}\n\n由紫垣 `ziyuan github pr` 开出的草稿 PR：改动已经皇帝准奏，共 {} 个文件。\n\n{}",
                files.len(),
                ziyuan_app::reply_text(session.court(), entry)
            );
            let url =
                hub.draft_pull_request(&format!("ziyuan/task-{}", task.0), &title, &body, &files)?;
            session.record_reply(task, true, url.clone())?;
            println!("诏令 #{} 的改动已开为草稿 PR：{url}", task.0);
        }
    }
    Ok(())
}

/// 同一朝廷同一时刻只有一个进程执行诏令；入队、撤销、任免等不受限制。
fn lock_executor() -> Result<ExecutorLock, Box<dyn Error>> {
    EventLog::new(&env::current_dir()?)
        .try_lock_executor()?
        .ok_or_else(|| {
            format!("{EXECUTOR_BUSY}。可用 `edict --queue` 入队：常朝（`run --watch`）中随到随办，否则待其结束后 `ziyuan run`。").into()
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
    let hunks: usize = changes.files.iter().map(|f| f.hunks().len()).sum();
    format!(
        "；改动 {} 个文件 {hunks} 段{state}{}",
        changes.files.len(),
        skipped_note(&changes.skipped)
    )
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
    let steps = match env::var("ZIYUAN_MAX_STEPS") {
        Ok(n) => n
            .parse()
            .ok()
            .filter(|&n| n > 0)
            .ok_or("ZIYUAN_MAX_STEPS 须为正整数")?,
        Err(_) => MAX_AGENT_STEPS,
    };
    Ok(Arc::new(
        AgentBackend::new(Box::new(model), env::current_dir()?, steps)
            .with_catalog(catalog)
            .with_test_command(env::var("ZIYUAN_TEST_COMMAND").ok()),
    ))
}

/// `--json`：朝廷全貌与 Web 的 `/api/state` 同构；`officials`、`tasks` 取其中一项；
/// `history` 为 `[{seq, text}]`（`seq` 从 1 起，与 `ziyuan history` 的序号一致）。
fn json_output(
    session: &Session<EventLog>,
    command: &Command,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let state = || -> Result<serde_json::Value, Box<dyn Error>> {
        Ok(ziyuan_web::court_json(
            session.court(),
            worker_backend()?.as_ref(),
        ))
    };
    Ok(match command {
        Command::Court => state()?,
        Command::Officials => state()?["officials"].take(),
        Command::Tasks => state()?["tasks"].take(),
        Command::History(filter) => {
            let offset = tz_offset();
            history_entries(session.history(), filter)?
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
            found_records(&records, query)
                .into_iter()
                .map(|r| {
                    serde_json::json!({
                        "task": r.task.0,
                        "department": r.department.code(),
                        "edict": r.edict,
                        "worker": r.worker.0,
                        "handler": r.handler,
                        "summary": r.summary,
                        "verdict": r.verdict,
                    })
                })
                .collect()
        }
        _ => unreachable!("run 已检查 JSON_COMMANDS"),
    })
}

/// shell 补全脚本：首词补子命令；官职、部门、制度、工具等参数按所在子命令补全。
fn completions(shell: Shell) -> String {
    let words = |items: Vec<String>| items.join(" ");
    let offices = words(Office::all().into_iter().map(|o| o.code()).collect());
    let departments = words(
        Department::ALL
            .iter()
            .map(|d| d.code().to_owned())
            .collect(),
    );
    let modes = words(
        GovernanceMode::ALL
            .iter()
            .map(|m| m.code().to_owned())
            .collect(),
    );
    let tools = words(
        Capability::ALL
            .iter()
            .map(|c| c.code().to_owned())
            .collect(),
    );
    let commands = SUBCOMMANDS.join(" ");
    let json = JSON_COMMANDS.join(" ");
    let bash = format!(
        r#"_ziyuan() {{
  # 官职代码含冒号，而 bash 默认在冒号处分词：从 COMP_LINE 取完整的当前词。
  local line="${{COMP_LINE:0:COMP_POINT}}" words=""
  local cur="${{line##*[[:space:]]}}" rest="${{line%[[:space:]]*}}"
  local prev="${{rest##*[[:space:]]}}"
  if [ "$COMP_CWORD" -eq 1 ]; then
    COMPREPLY=($(compgen -W "{commands}" -- "$cur")); return
  fi
  case "$prev" in
    --dept) words="{departments}" ;;
    --tools) words="{tools} none" ;;
    *) case "${{COMP_WORDS[1]}}" in
         appoint|model|ceiling|nominate|recommend) words="{offices}" ;;
         recruit) words="{departments}" ;;
         reform) words="{modes}" ;;
         completions) words="bash zsh fish" ;;
         {json_cases}) words="--json" ;;
       esac ;;
  esac
  COMPREPLY=($(compgen -W "$words" -- "$cur"))
  # 冒号前的部分 bash 已视为另一个词，补全结果中去掉。
  local colon="${{cur%"${{cur##*:}}"}}"
  [ -n "$colon" ] && COMPREPLY=("${{COMPREPLY[@]#"$colon"}}")
}}
complete -F _ziyuan ziyuan
"#,
        json_cases = JSON_COMMANDS.join("|")
    );
    match shell {
        Shell::Bash => bash,
        // zsh 借 bashcompinit 复用同一份脚本。
        Shell::Zsh => format!("autoload -U +X bashcompinit && bashcompinit\n{bash}"),
        Shell::Fish => {
            let seen =
                |cmds: &str| format!("complete -c ziyuan -n '__fish_seen_subcommand_from {cmds}'");
            [
                "complete -c ziyuan -f".to_owned(),
                format!("complete -c ziyuan -n __fish_use_subcommand -a '{commands}'"),
                format!(
                    "{} -a '{offices}'",
                    seen("appoint model ceiling nominate recommend")
                ),
                format!("{} -a '{departments}'", seen("recruit")),
                format!("{} -a '{modes}'", seen("reform")),
                format!("{} -a 'bash zsh fish'", seen("completions")),
                format!("{} -l json", seen(&json)),
                format!(
                    "{} -l dept -x -a '{departments}'",
                    seen("edict post template")
                ),
                format!(
                    "{} -l tools -x -a '{tools} none'",
                    seen("edict template ceiling")
                ),
            ]
            .join("\n")
                + "\n"
        }
    }
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

/// `ziyuan records` 列出的实录：无检索词时全部（新者在前），否则按检索词至多 20 件。
fn found_records<'a>(records: &'a [Record], query: &str) -> Vec<&'a Record> {
    if query.trim().is_empty() {
        records.iter().rev().collect()
    } else {
        search_records(records, query, 20)
    }
}

fn parse_history(args: Vec<String>) -> Result<Command, Box<dyn Error>> {
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

fn parse_run(args: Vec<String>) -> Result<Command, Box<dyn Error>> {
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

/// 常朝：常驻执行，直到 Ctrl-C 退朝（执行中的诏令撤销，排队的保留）。
fn hold_court(
    session: &mut Session<EventLog>,
    backend: Arc<dyn WorkerBackend>,
    limits: Limits,
    options: &WatchOptions,
) -> Result<(), Box<dyn Error>> {
    let scheduler = Scheduler::new(backend.clone(), limits);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let censor = if options.censor.is_some() {
        "办完自动请都察院复核"
    } else {
        "不自动复核"
    };
    println!(
        "常朝开始：每 {} 秒查看一次队列，{censor}；采纳与落盘仍须亲裁。Ctrl-C 退朝。",
        options.poll.as_secs()
    );
    let result = runtime.block_on(async {
        let cancel = CancelToken::new();
        let on_interrupt = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("退朝：撤销执行中的诏令，排队中的保留。");
                on_interrupt.cancel();
            }
        });
        let mut stream = StreamPrinter::new(io::stderr());
        let on_update = |update: WatchUpdate<'_>| match update {
            WatchUpdate::Run(RunUpdate::Assigned(_)) => {}
            WatchUpdate::Run(RunUpdate::Text { task, text }) => stream.text(task, text),
            WatchUpdate::Run(RunUpdate::Finished(report)) => {
                stream.finish(report.edict.id);
                print_report(report);
            }
            WatchUpdate::Censured { task, verdict } => {
                println!("诏令 #{} 都察院自动复核：{}", task.0, verdict.label());
            }
            WatchUpdate::Notice(note) => eprintln!("{note}"),
            WatchUpdate::Neglect(note) => println!("史官记：{note}"),
        };
        session
            .watch(&scheduler, backend.as_ref(), &cancel, options, on_update)
            .await
    });
    // 撤销后，阻塞中的模型请求可能仍未返回；不等待它们。
    runtime.shutdown_background();
    result?;
    Ok(())
}

/// `edict --template` 的模板名，以及哪些项由命令行显式给出（其余取模板的）。
#[derive(Debug)]
struct TemplateChoice {
    name: String,
    department: bool,
    tools: bool,
    mcp: bool,
    scope: bool,
}

fn parse_edict(mut args: Vec<String>) -> Result<Command, Box<dyn Error>> {
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
fn parse_template(mut args: Vec<String>) -> Result<Command, Box<dyn Error>> {
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
fn parse_post(mut args: Vec<String>) -> Result<Command, Box<dyn Error>> {
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
fn parse_ceiling(mut args: Vec<String>) -> Result<Command, Box<dyn Error>> {
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
fn parse_mcp(value: &str) -> BTreeSet<String> {
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
    fn every_completed_subcommand_is_recognised() {
        for name in SUBCOMMANDS {
            if let Err(e) = parse(vec![name.to_string()]) {
                assert!(!e.to_string().contains("未知命令"), "{name}: {e}");
            }
        }
        let script = completions(Shell::Bash);
        assert!(script.contains("minister:works") && script.contains("write_file"));
    }

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
