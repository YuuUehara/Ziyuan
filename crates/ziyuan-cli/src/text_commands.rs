//! `ziyuan text`：离线文本项目与译书命令分派。不读模型配置、不联网。

use std::env;
use std::error::Error;
use std::io::Read;
use std::path::PathBuf;
use ziyuan_app::text_project::{NewProject, Project, Provenance, TextProjects, ValidationReport};

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
  以上命令均可加 --json，以 JSON 输出到标准输出。项目存于 .ziyuan/text-projects/<项目ID>/。"#;

pub(crate) enum TextAction {
    Help,
    Translation(crate::translation_commands::TranslationAction),
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
