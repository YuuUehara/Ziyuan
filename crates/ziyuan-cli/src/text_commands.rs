//! `ziyuan text`：离线文本项目与译书命令分派。不读模型配置、不联网。

use std::env;
use std::error::Error;
use std::io::Read;
use std::path::PathBuf;
use ziyuan_app::text_project::{NewProject, Project, Provenance, TextProjects, ValidationReport};
use ziyuan_storage::{LockScope, RecordState, Untouched, Workspace};

/// `text recovery status` 发现未收尾的受管写入：状态本身读成功，但退出码仍为 1，
/// 好让脚本据此判断还能不能写入。
#[derive(Debug)]
pub(crate) struct RecoveryPending;
impl std::fmt::Display for RecoveryPending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("工作目录仍有未收尾的受管写入，写入会被拒绝")
    }
}
impl Error for RecoveryPending {}

pub(crate) const TEXT_HELP: &str = r#"文本项目（离线；不需要 ziyuan init，不调用模型或网络）：
  ziyuan text init --title <标题> --language <ja|en> --source <来源> --rights <权利声明>
                   [--author <作者>] [--version <版本>] [--date <取得日期>] [--scope <使用范围>]
                                       建立项目（译入 zh-CN），打印项目 ID；来源与权利为用户自述，不作核验
  ziyuan text import <项目ID> <章节.txt>...
                                       按给出的顺序一次性导入 UTF-8 TXT（每个项目只能导入一次）
  ziyuan text show <项目ID>            核对原文副本后列出章节、段落与提示块
  ziyuan text check <项目ID> --candidate <译稿.json> [--block <提示块ID>]
                                       检查候选译稿的结构（漏段、重复、顺序、陌生段号、空文本）；
                                       提示块默认取自译稿的 block_id。只查结构，不评判译文质量；
                                       未通过时仍输出报告，退出码 1
  ziyuan text recovery status [--json]
                                       只读报告工作目录的受管写入世代号与恢复记录；
                                       仍有未收尾的写入时退出码 1
  ziyuan text recovery finish <操作ID> --yes
                                       推进世代并清理已完成（或仅发布未落盘）的恢复记录；
                                       不重做业务写入，也不重算目标文件
  ziyuan text recovery abandon <操作ID> --yes
                                       仅在目标仍是旧状态、未落盘任何字节时放弃那次写入
  以上命令均可加 --json，以 JSON 输出到标准输出。项目存于 .ziyuan/text-projects/<项目ID>/。"#;

pub(crate) enum TextAction {
    Help,
    Translation(crate::translation_commands::TranslationAction),
    Recovery(RecoveryAction),
    Init(NewProject),
    Import(String, Vec<PathBuf>),
    Show(String),
    Check {
        id: String,
        candidate: PathBuf,
        block: Option<String>,
    },
}

const INIT_USAGE: &str = "用法: ziyuan text init --title <标题> --language <ja|en> --source <来源> --rights <权利声明> [--author <作者>] [--version <版本>] [--date <取得日期>] [--scope <使用范围>]";
const IMPORT_USAGE: &str = "用法: ziyuan text import <项目ID> <章节.txt>...";
const SHOW_USAGE: &str = "用法: ziyuan text show <项目ID>";
const CHECK_USAGE: &str =
    "用法: ziyuan text check <项目ID> --candidate <译稿.json> [--block <提示块ID>]";
const RECOVERY_USAGE: &str = "用法: ziyuan text recovery status [--json] | ziyuan text recovery finish <操作ID> --yes | ziyuan text recovery abandon <操作ID> --yes";

/// 受管写入（准奏落盘）的恢复操作：只处理先前遗留的恢复记录，不执行业务写入。
pub(crate) enum RecoveryAction {
    Status,
    Finish { operation_id: String },
    Abandon { operation_id: String },
}
/// 候选译稿的读取上限：远大于单个提示块（48 KiB），只为挡住误给的巨型文件。
const CANDIDATE_BYTES: u64 = 16 * 1024 * 1024;

pub(crate) fn parse(args: Vec<String>) -> Result<TextAction, Box<dyn Error>> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(TextAction::Help);
    }
    let Some((action, rest)) = args.split_first() else {
        return Ok(TextAction::Help);
    };
    Ok(match action.as_str() {
        "help" => TextAction::Help,
        "init" => parse_init(rest)?,
        "import" => match rest {
            [id, files @ ..] if !files.is_empty() && !rest.iter().any(|a| a.starts_with("--")) => {
                TextAction::Import(id.clone(), files.iter().map(PathBuf::from).collect())
            }
            _ => return Err(IMPORT_USAGE.into()),
        },
        "show" => match rest {
            [id] if !id.starts_with("--") => TextAction::Show(id.clone()),
            _ => return Err(SHOW_USAGE.into()),
        },
        "check" => parse_check(rest)?,
        "recovery" => parse_recovery(rest)?,
        "translate" | "review" | "revise" | "zhupi" | "finalize" | "reopen" | "context"
        | "status" | "resume" => {
            TextAction::Translation(crate::translation_commands::parse(&args)?)
        }
        other => {
            return Err(
                format!("未知的 text 子命令：{other}。运行 ziyuan text help 查看用法").into(),
            );
        }
    })
}

fn parse_init(args: &[String]) -> Result<TextAction, Box<dyn Error>> {
    let (mut title, mut language) = (None, None);
    let mut provenance = Provenance::default();
    let (mut source, mut rights) = (false, false);
    let mut given = Vec::new();
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = rest.next().ok_or(INIT_USAGE)?.clone();
        if given.contains(flag) {
            return Err(format!("{flag} 只能给一次").into());
        }
        given.push(flag.clone());
        match flag.as_str() {
            "--title" => title = Some(value),
            "--language" => language = Some(value),
            "--source" => {
                provenance.source = value;
                source = true;
            }
            "--rights" => {
                provenance.rights = value;
                rights = true;
            }
            "--author" => provenance.author = value,
            "--version" => provenance.version = value,
            "--date" => provenance.date = value,
            "--scope" => provenance.scope = value,
            _ => return Err(INIT_USAGE.into()),
        }
    }
    let language = language.ok_or(INIT_USAGE)?;
    if !matches!(language.as_str(), "ja" | "en") {
        return Err(format!("原文语言仅支持 ja 或 en：{language}").into());
    }
    let (Some(title), true, true) = (title, source, rights) else {
        return Err(INIT_USAGE.into());
    };
    Ok(TextAction::Init(NewProject {
        title,
        source_language: language,
        target_language: "zh-CN".to_owned(),
        provenance,
    }))
}

fn parse_check(args: &[String]) -> Result<TextAction, Box<dyn Error>> {
    let (id, flags) = args.split_first().ok_or(CHECK_USAGE)?;
    if id.starts_with("--") {
        return Err(CHECK_USAGE.into());
    }
    let (mut candidate, mut block) = (None, None);
    let mut rest = flags.iter();
    while let Some(flag) = rest.next() {
        let slot = match flag.as_str() {
            "--candidate" => &mut candidate,
            "--block" => &mut block,
            _ => return Err(CHECK_USAGE.into()),
        };
        let value = rest.next().filter(|v| !v.starts_with("--"));
        if slot.is_some() {
            return Err(format!("{flag} 只能给一次").into());
        }
        *slot = Some(value.ok_or(CHECK_USAGE)?.clone());
    }
    Ok(TextAction::Check {
        id: id.clone(),
        candidate: PathBuf::from(candidate.ok_or(CHECK_USAGE)?),
        block,
    })
}

fn parse_recovery(args: &[String]) -> Result<TextAction, Box<dyn Error>> {
    let action = match args {
        [action] if action == "status" => RecoveryAction::Status,
        // 收尾与放弃都会改变状态，必须显式带 `--yes`，避免误敲一次就解锁。
        [action, id, yes] if !id.starts_with("--") && yes == "--yes" && action == "finish" => {
            RecoveryAction::Finish {
                operation_id: id.clone(),
            }
        }
        [action, id, yes] if !id.starts_with("--") && yes == "--yes" && action == "abandon" => {
            RecoveryAction::Abandon {
                operation_id: id.clone(),
            }
        }
        _ => return Err(RECOVERY_USAGE.into()),
    };
    Ok(TextAction::Recovery(action))
}

pub(crate) fn run(action: TextAction, json: bool) -> Result<(), Box<dyn Error>> {
    if let TextAction::Help = action {
        println!(
            "{TEXT_HELP}\n\n{}",
            crate::translation_commands::TRANSLATION_HELP
        );
        return Ok(());
    }
    let projects = TextProjects::new(env::current_dir()?);
    match action {
        TextAction::Help => unreachable!("已在上面处理"),
        TextAction::Translation(action) => return crate::translation_commands::run(action, json),
        TextAction::Recovery(action) => return run_recovery(action, json),
        TextAction::Init(metadata) => {
            let project = projects.create(metadata)?;
            match json {
                true => print_json(serde_json::to_value(&project)?)?,
                false => {
                    println!("文本项目已建立：{}", project.id);
                    println!("目录：{}", projects.project_dir(&project.id)?.display());
                    println!("下一步：ziyuan text import {} <章节.txt>...", project.id);
                }
            }
        }
        TextAction::Import(id, files) => {
            let project = projects.import(&id, &files)?;
            match json {
                true => print_json(serde_json::to_value(&project)?)?,
                false => {
                    println!("已导入 {} 章：", project.chapters.len());
                    print_chapters(&projects, &project)?;
                }
            }
        }
        TextAction::Show(id) => {
            let project = projects.show(&id)?;
            match json {
                true => print_json(serde_json::to_value(&project)?)?,
                false => print_project(&projects, &project)?,
            }
        }
        TextAction::Check {
            id,
            candidate,
            block,
        } => {
            let bytes = read_candidate(&candidate)?;
            let block = match block {
                Some(block) => block,
                None => block_of(&bytes)?,
            };
            let report = projects.check(&id, &block, &bytes)?;
            match json {
                true => print_json(serde_json::to_value(&report)?)?,
                false => print_report(&report),
            }
            if !report.valid {
                return Err(
                    format!("候选译稿未通过结构检查（{} 处问题）", report.issues.len()).into(),
                );
            }
        }
    }
    Ok(())
}

fn print_json(value: serde_json::Value) -> Result<(), Box<dyn Error>> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn read_candidate(path: &PathBuf) -> Result<Vec<u8>, Box<dyn Error>> {
    let file =
        std::fs::File::open(path).map_err(|e| format!("读不了候选译稿 {}：{e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(CANDIDATE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > CANDIDATE_BYTES {
        return Err(format!("候选译稿超过 {} MiB", CANDIDATE_BYTES / 1024 / 1024).into());
    }
    Ok(bytes)
}

/// 未给 `--block` 时，提示块只取自候选 JSON 的 `block_id`，不从文件名猜。
fn block_of(bytes: &[u8]) -> Result<String, Box<dyn Error>> {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| v.get("block_id")?.as_str().map(str::to_owned))
        .ok_or_else(|| {
            "候选译稿不是含字符串 block_id 的 JSON；可用 --block <提示块ID> 指定后查看完整报告"
                .into()
        })
}

fn print_project(projects: &TextProjects, project: &Project) -> Result<(), Box<dyn Error>> {
    let m = &project.metadata;
    let p = &m.provenance;
    println!("文本项目 {}", project.id);
    println!("标题：{}", m.title);
    println!("语言：{} → {}", m.source_language, m.target_language);
    println!("来源：{}", p.source);
    println!("权利声明（用户自述，未核验）：{}", p.rights);
    for (label, value) in [
        ("作者", &p.author),
        ("版本", &p.version),
        ("取得日期", &p.date),
        ("使用范围", &p.scope),
    ] {
        if !value.is_empty() {
            println!("{label}：{value}");
        }
    }
    println!("目录：{}", projects.project_dir(&project.id)?.display());
    println!("提示块上限：{} 字节", project.block_bytes);
    if project.chapters.is_empty() {
        println!(
            "尚未导入原文：ziyuan text import {} <章节.txt>...",
            project.id
        );
        return Ok(());
    }
    println!("章节（{}）：", project.chapters.len());
    print_chapters(projects, project)
}

fn print_chapters(projects: &TextProjects, project: &Project) -> Result<(), Box<dyn Error>> {
    let dir = projects.project_dir(&project.id)?;
    for chapter in &project.chapters {
        println!(
            "  {} {}：{} 字节，{} 段，{} 个提示块",
            chapter.id,
            chapter.source_name,
            chapter.byte_len,
            chapter.paragraphs.len(),
            chapter.blocks.len()
        );
        let bundle = chapter.original_path.split('/').next().unwrap_or_default();
        for block in &chapter.blocks {
            let path = dir
                .join(bundle)
                .join("blocks")
                .join(format!("{}.json", block.block_id));
            println!(
                "    {}（{} 段）{}",
                block.block_id,
                block.paragraphs.len(),
                path.display()
            );
        }
    }
    Ok(())
}

/// 只读报告或收尾一次先前遗留的受管写入。这里不执行业务写入，也不再落盘任何项目文件。
fn run_recovery(action: RecoveryAction, json: bool) -> Result<(), Box<dyn Error>> {
    let workspace = Workspace::open(env::current_dir()?)?;
    match action {
        RecoveryAction::Status => {
            let generation = workspace.generation()?;
            let record = workspace.pending_record()?;
            let state = match &record {
                None => "idle",
                Some(record) => match record.state {
                    RecordState::Prepared => "prepared",
                    RecordState::Published => "published",
                    RecordState::Completed => "completed",
                    RecordState::Abandoned => "abandoned",
                },
            };
            let next = match &record {
                None => "无待处理的受管写入",
                Some(record) => match record.state {
                    RecordState::Prepared | RecordState::Abandoned => {
                        "可用 text recovery finish <操作ID> --yes 收尾（若目标仍是旧状态，也可 abandon）"
                    }
                    RecordState::Published => {
                        "须核对目标后 text recovery finish <操作ID> --yes，目标被改过时不可 abandon"
                    }
                    RecordState::Completed => {
                        "text recovery finish <操作ID> --yes 推进世代并清理记录"
                    }
                },
            };
            if json {
                let value = match &record {
                    None => serde_json::json!({ "generation": generation, "state": state }),
                    Some(record) => serde_json::json!({
                        "generation": generation,
                        "state": state,
                        "operation_id": record.operation_id,
                        "task": record.application.task,
                        "skipped": record.application.skipped,
                        "targets": record.targets.iter().map(|t| t.path.clone()).collect::<Vec<_>>(),
                        "next": next,
                    }),
                };
                print_json(value)?;
            } else {
                println!("世代号：{generation}（{}）", even_odd(generation));
                println!("恢复记录：{state}");
                if let Some(record) = &record {
                    println!("  操作 ID：{}", record.operation_id);
                    println!("  任务：#{}", record.application.task);
                    println!("  目标：{} 个", record.targets.len());
                    for target in &record.targets {
                        println!("    {}", target.path);
                    }
                }
                println!("下一步：{next}");
            }
            // 尚未收尾（世代为奇数）或仍有未清理的记录时，退出码 1：脚本据此判断还能不能写入。
            let unfinished = generation % 2 != 0 || record.is_some();
            if unfinished {
                return Err(RecoveryPending.into());
            }
        }
        RecoveryAction::Finish { operation_id } => {
            finish_recovery(&workspace, &operation_id, json)?;
        }
        RecoveryAction::Abandon { operation_id } => {
            let mut guard = workspace.lock(LockScope::Workspace)?;
            // 放弃的前提是「什么都没发生」：目标必须仍是旧内容，事件也一条没进事件簿。
            // 只靠 CLI 信任调用者不行，这里逐项核对；任何一项不成立都不放弃。
            let record = workspace
                .pending_record()?
                .ok_or("没有待收尾的恢复记录；用 ziyuan text recovery status 查看")?;
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
            // 核对已全部通过：把结论显式交给存储层，由它按记录状态选对应的入口。
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

/// 收尾：只推进世代并清理记录，绝不重做业务写入。
fn finish_recovery(
    workspace: &Workspace,
    operation_id: &str,
    json: bool,
) -> Result<(), Box<dyn Error>> {
    let mut guard = workspace.lock(LockScope::Workspace)?;
    let record = workspace
        .pending_record()?
        .ok_or("没有待收尾的恢复记录；用 ziyuan text recovery status 查看")?;
    if record.operation_id != operation_id {
        return Err(format!(
            "操作 ID 不符：磁盘上是 {}，不是 {operation_id}",
            record.operation_id
        )
        .into());
    }
    let state = record.state;
    // 先看事件簿：这批事件在不在，决定「已发布」的记录该怎么收尾。
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
    // 「已发布」是否算完成，以**目标文件**为准；事件簿只当辅证。
    // 事件簿里有一行长得一样，不等于这一批真的追记过——另一笔内容相同的准奏会写出逐字相同的行。
    // 所以只要还有目标停在旧内容上，就不能当作写完：那是用户已批准、却还没落盘的改动。
    if state == RecordState::Published && !pending.is_empty() {
        let reason = match recorded {
            // 事件簿对得上、目标却没写：本事务自己走不到这个状态，只能来自外部改动。
            true => {
                "事件簿里已有本批记录，目标却仍是旧内容：两边互相矛盾，可能被外部改动过。\n\
                 请自行核对项目文件后，再决定收尾或放弃。"
            }
            // 最常见的一种：一个文件都没写，或写到一半。两者在盘上分不出来。
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
    // 目标全部已是新内容：这次写入确实发生过，收尾即可。
    if state == RecordState::Published {
        // 收尾前在锁内再核一遍：工作目录锁只挡住其他紫垣进程，挡不住外部编辑器。
        // 核对与推进世代之间仍有极小的窗口，这一点如实记在验收记录的已知限制里。
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

fn even_odd(generation: u64) -> &'static str {
    match generation % 2 {
        0 => "无进行中的受管写入",
        _ => "上次受管写入未完成",
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

/// 目标路径当前的摘要状态。
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

/// 记录里那批事件是否真的进了事件簿。
///
/// 「最近 N 条长得一样」不足以证明这一点：`ChangesApplied` 这类事件很短，另一笔同样内容的
/// 准奏会写出逐字相同的行。因此不只比对尾部，还须证明本批恰好开始于记录预告的位置——
/// 即事件簿**前 `base` 条**的摘要等于记录里的 `history_sha256`，随后才是本批。
/// 读不到事件簿、或摘要对不上时一律按「没记」处理。
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
    // 本批之前应有的条数。批次长度取自记录本身，故 base = 预期总数 − 批长。
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
    // 一、本批之前的那段历史必须正是记录写下时看到的那段。
    // 摘要口径由存储层统一给出（每行以换行符结尾），与写记录时完全一致。
    let actual = ziyuan_storage::EventStore::history_sha256_at_most(
        &ziyuan_storage::EventLog::new(root),
        base,
    )?;
    if actual.as_deref() != Some(record.events.history_sha256.as_str()) {
        return Ok(false);
    }
    // 二、紧接着的若干条必须正是本批。
    Ok(lines[base..base + batch.len()] == batch[..])
}

fn print_report(report: &ValidationReport) {
    println!(
        "项目 {} · 章节 {} · 提示块 {}（应有 {} 段）",
        report.project_id,
        report.chapter_id,
        report.block_id,
        report.expected_ids.len()
    );
    if report.valid {
        println!("结构检查通过：段号齐全、无重复、顺序一致、文本非空。只查结构，不代表译文质量。");
        return;
    }
    println!("结构检查未通过（{} 处）：", report.issues.len());
    for issue in &report.issues {
        let kind = serde_json::to_value(issue.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let place = match (&issue.paragraph_id, issue.position) {
            (Some(id), Some(pos)) => format!(" {id}（第 {pos} 项）"),
            (Some(id), None) => format!(" {id}"),
            (None, Some(pos)) => format!("（第 {pos} 项）"),
            (None, None) => String::new(),
        };
        println!("  - [{kind}]{place}：{}", issue.message);
    }
}
