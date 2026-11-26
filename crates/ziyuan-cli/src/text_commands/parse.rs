//! `ziyuan text` 命令行参数解析。

use std::error::Error;
use std::path::PathBuf;
use ziyuan_app::text_project::{
    ImportOptions, ImportSource, NewProject, Provenance, TextParagraphs,
};
use ziyuan_app::translation::{ExportFormat, ExportLayout, ExportOptions};

use super::{RecoveryAction, TextAction};

const INIT_USAGE: &str = "用法: ziyuan text init --title <标题> --language <ja|en|ko|fr|de|ru|es|it> --source <来源> --rights <权利声明> [--author <作者>] [--version <版本>] [--date <取得日期>] [--scope <使用范围>]";
const IMPORT_USAGE: &str = "用法: ziyuan text import <项目ID> <文件>... [--ruby] [--heading-level <1|2>] [--text-paragraphs <blank_lines|lines>] [--split-long-paragraphs] [--chapter-title <章名>]... [--confirm <预览指纹>]；MD 或解析选项须先 text preview 再 --confirm";
const PREVIEW_USAGE: &str = "用法: ziyuan text preview <项目ID> <文件>... [--ruby] [--heading-level <1|2>] [--text-paragraphs <blank_lines|lines>] [--split-long-paragraphs] [--chapter-title <章名>]...";
const SHOW_USAGE: &str = "用法: ziyuan text show <项目ID>";
const HUMAN_SHOW_USAGE: &str = "用法: ziyuan text human show <项目ID> <块ID> [--json]（只读）";
const HUMAN_SAVE_USAGE: &str =
    "用法: ziyuan text human save <项目ID> <块ID> --file <草稿.json> [--json]（只写草稿）";
const HUMAN_SUBMIT_USAGE: &str =
    "用法: ziyuan text human submit <项目ID> <块ID> [--json]（冻结候选并发布送审绑定）";
const HUMAN_APPROVE_USAGE: &str =
    "用法: ziyuan text human approve <项目ID> <块ID> [--json]（准奏最新送审并写入 zh.json）";
const HUMAN_FINALIZE_USAGE: &str = "用法: ziyuan text human finalize <项目ID> <块ID> [--json]（只记定稿事实；要求磁盘已是准奏目标）";
const HUMAN_APPLY_USAGE: &str =
    "用法: ziyuan text human apply <项目ID> <块ID> [--json]（只读核对准奏时已落盘的 zh.json）";
const EXPORT_FINISHED_USAGE: &str = "用法: ziyuan text export-finished <项目ID> [--chapter <章ID>]... [--format txt|md|html] [--layout chapters|book] [--bilingual] [--output <new.tar>]";
const EXPORT_ORIGINALS_USAGE: &str =
    "用法: ziyuan text export-originals <项目ID> [--chapter <章ID>]... [--output <new.tar>]";
const CHECK_USAGE: &str =
    "用法: ziyuan text check <项目ID> --candidate <译稿.json> [--block <提示块ID>]";
const RECOVERY_USAGE: &str = "用法: ziyuan text recovery status [--json] | ziyuan text recovery finish <操作ID> --yes [--confirm-prepared] | ziyuan text recovery abandon <操作ID> --yes [--confirm-prepared] | ziyuan text recovery temps [--json] [--yes]";

pub(crate) fn parse(args: Vec<String>) -> Result<TextAction, Box<dyn Error>> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(TextAction::Help);
    }
    let Some((action, rest)) = args.split_first() else {
        return Ok(TextAction::Help);
    };
    Ok(match action.as_str() {
        "backup" | "restore" | "source-update" | "issue" | "read" | "preflight" | "search"
        | "draft-replace" | "tmx" => {
            TextAction::Continuity(crate::continuity_commands::parse(action, rest)?)
        }
        "help" => TextAction::Help,
        "init" => parse_init(rest)?,
        "import" => parse_import(rest, false)?,
        "import-epub" => {
            let [id, file] = rest else {
                return Err("用法: ziyuan text import-epub <项目ID> <文件.epub>".into());
            };
            TextAction::ImportEpub(id.clone(), PathBuf::from(file.clone()))
        }
        "import-srt" => {
            let [id, file] = rest else {
                return Err("用法: ziyuan text import-srt <项目ID> <文件.srt>".into());
            };
            TextAction::ImportSrt(id.clone(), PathBuf::from(file.clone()))
        }
        "export-srt" => {
            let [id, file] = rest else {
                return Err("用法: ziyuan text export-srt <项目ID> <文件.srt>".into());
            };
            TextAction::ExportSrt(id.clone(), PathBuf::from(file.clone()))
        }
        "export-docx" => {
            let [id, file] = rest else {
                return Err("用法: ziyuan text export-docx <项目ID> <输出.docx>".into());
            };
            if id.starts_with('-') || file.starts_with('-') {
                return Err("用法: ziyuan text export-docx <项目ID> <输出.docx>".into());
            }
            TextAction::ExportDocx(id.clone(), PathBuf::from(file.clone()))
        }
        "export-epub" => {
            let [id, file] = rest else {
                return Err("用法: ziyuan text export-epub <项目ID> <输出.epub>".into());
            };
            if id.starts_with('-') || file.starts_with('-') {
                return Err("用法: ziyuan text export-epub <项目ID> <输出.epub>".into());
            }
            TextAction::ExportEpub(id.clone(), PathBuf::from(file.clone()))
        }
        "import-docx" => {
            let [id, file] = rest else {
                return Err("用法: ziyuan text import-docx <项目ID> <文件.docx>".into());
            };
            if id.starts_with('-') || file.is_empty() || file.starts_with('-') {
                return Err("用法: ziyuan text import-docx <项目ID> <文件.docx>".into());
            }
            TextAction::ImportDocx(id.clone(), PathBuf::from(file.clone()))
        }
        "preview" => parse_import(rest, true)?,
        "export-finished" => parse_export(rest, false)?,
        "export-originals" => parse_export(rest, true)?,
        "show" => match rest {
            [id] if !id.starts_with("--") => TextAction::Show(id.clone()),
            _ => return Err(SHOW_USAGE.into()),
        },
        "human" => match rest {
            [show, project, block]
                if show == "show" && !project.starts_with("--") && !block.starts_with("--") =>
            {
                TextAction::HumanShow {
                    project: project.clone(),
                    block: block.clone(),
                }
            }
            [save, project, block, flag, file]
                if save == "save"
                    && flag == "--file"
                    && !project.starts_with("--")
                    && !block.starts_with("--") =>
            {
                TextAction::HumanSave {
                    project: project.clone(),
                    block: block.clone(),
                    file: PathBuf::from(file),
                }
            }
            [submit, project, block]
                if submit == "submit" && !project.starts_with("--") && !block.starts_with("--") =>
            {
                TextAction::HumanSubmit {
                    project: project.clone(),
                    block: block.clone(),
                }
            }
            [approve, project, block]
                if approve == "approve"
                    && !project.starts_with("--")
                    && !block.starts_with("--") =>
            {
                TextAction::HumanApprove {
                    project: project.clone(),
                    block: block.clone(),
                }
            }
            [finalize, project, block]
                if finalize == "finalize"
                    && !project.starts_with("--")
                    && !block.starts_with("--") =>
            {
                TextAction::HumanFinalize {
                    project: project.clone(),
                    block: block.clone(),
                }
            }
            [apply, project, block]
                if apply == "apply" && !project.starts_with("--") && !block.starts_with("--") =>
            {
                TextAction::HumanApply {
                    project: project.clone(),
                    block: block.clone(),
                }
            }
            _ => {
                return Err(format!(
                    "{HUMAN_SHOW_USAGE}\n{HUMAN_SAVE_USAGE}\n{HUMAN_SUBMIT_USAGE}\n{HUMAN_APPROVE_USAGE}\n{HUMAN_FINALIZE_USAGE}\n{HUMAN_APPLY_USAGE}"
                )
                .into());
            }
        },
        "usage" => match rest {
            [id] if !id.starts_with('-') && !id.trim().is_empty() => TextAction::Usage(id.clone()),
            _ => return Err("用法: ziyuan text usage <项目ID> [--json]".into()),
        },
        "check" => parse_check(rest)?,
        "memory" => TextAction::Memory(crate::memory_commands::parse(rest)?),
        "recovery" => parse_recovery(rest)?,
        "evidence" | "verification" => {
            TextAction::Verification(crate::verification_commands::parse(&args)?)
        }
        "translate" | "review" | "revise" | "zhupi" | "finalize" | "reopen" | "context"
        | "status" | "resume" | "references" | "qa" | "qa-report" | "qa-ignore" | "termbase"
        | "concordance" | "tm" | "comment" | "compare" | "version" => {
            TextAction::Translation(crate::translation_commands::parse(&args)?)
        }
        other => {
            return Err(
                format!("未知的 text 子命令：{other}。运行 ziyuan text help 查看用法").into(),
            );
        }
    })
}

fn parse_export(args: &[String], originals: bool) -> Result<TextAction, Box<dyn Error>> {
    let usage = if originals {
        EXPORT_ORIGINALS_USAGE
    } else {
        EXPORT_FINISHED_USAGE
    };
    let (project, args) = args.split_first().ok_or(usage)?;
    if project.starts_with('-') || project.trim().is_empty() {
        return Err(usage.into());
    }
    let mut options = ExportOptions::default();
    let mut output = None;
    let mut given = Vec::new();
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        if flag != "--chapter" && given.contains(flag) {
            return Err(format!("{flag} 只能给一次。{usage}").into());
        }
        given.push(flag.clone());
        if flag == "--bilingual" && !originals {
            options.bilingual = true;
            continue;
        }
        let value = rest
            .next()
            .filter(|value| !value.starts_with('-') && !value.trim().is_empty())
            .ok_or(usage)?;
        match flag.as_str() {
            "--chapter" => options.chapters.push(value.clone()),
            "--output" => output = Some(PathBuf::from(value)),
            "--format" if !originals => {
                options.format = match value.as_str() {
                    "txt" => ExportFormat::Txt,
                    "md" => ExportFormat::Markdown,
                    "html" => ExportFormat::Html,
                    _ => return Err(usage.into()),
                };
            }
            "--layout" if !originals => {
                options.layout = match value.as_str() {
                    "chapters" => ExportLayout::Chapters,
                    "book" => ExportLayout::Book,
                    _ => return Err(usage.into()),
                };
            }
            _ => return Err(usage.into()),
        }
    }
    Ok(TextAction::Export {
        project: project.clone(),
        options,
        originals,
        output,
    })
}

fn parse_import(args: &[String], preview: bool) -> Result<TextAction, Box<dyn Error>> {
    let usage = if preview { PREVIEW_USAGE } else { IMPORT_USAGE };
    let (id, args) = args.split_first().ok_or(usage)?;
    if id.starts_with("--") || id.is_empty() {
        return Err(usage.into());
    }
    let mut options = ImportOptions::default();
    let mut files = Vec::new();
    let mut confirm = None;
    let mut given = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if !arg.starts_with("--") {
            files.push(PathBuf::from(arg));
            continue;
        }
        if arg != "--chapter-title" && given.contains(arg) {
            return Err(format!("{arg} 只能给一次。{usage}").into());
        }
        given.push(arg.clone());
        match arg.as_str() {
            "--ruby" => options.parsing.ruby = true,
            "--split-long-paragraphs" => options.parsing.split_long_paragraphs = true,
            "--heading-level" | "--text-paragraphs" | "--chapter-title" | "--confirm" => {
                let value = rest
                    .next()
                    .filter(|value| !value.starts_with("--") && !value.trim().is_empty())
                    .ok_or(usage)?;
                match arg.as_str() {
                    "--heading-level" => {
                        options.parsing.heading_level = Some(match value.as_str() {
                            "1" => 1,
                            "2" => 2,
                            _ => return Err(usage.into()),
                        });
                    }
                    "--text-paragraphs" => {
                        options.parsing.text_paragraphs = match value.as_str() {
                            "blank_lines" => TextParagraphs::BlankLines,
                            "lines" => TextParagraphs::Lines,
                            _ => return Err(usage.into()),
                        };
                    }
                    "--chapter-title" => options.chapter_titles.push(value.clone()),
                    "--confirm" if !preview => confirm = Some(value.clone()),
                    _ => return Err(usage.into()),
                }
            }
            _ => return Err(usage.into()),
        }
    }
    if files.is_empty() {
        return Err(usage.into());
    }
    if !preview && confirm.is_none() {
        if !given.is_empty() {
            return Err(IMPORT_USAGE.into());
        }
        // No new options means the frozen legacy command, including runtime exit 1 for
        // unsupported inputs. Markdown still cannot import without explicit confirmation.
        return Ok(TextAction::Import(id.clone(), files));
    }
    let sources = files.into_iter().map(ImportSource::File).collect();
    Ok(if preview {
        TextAction::Preview(id.clone(), sources, options)
    } else {
        TextAction::ImportPrepared(id.clone(), sources, options, confirm.ok_or(usage)?)
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
    if !ziyuan_app::text_project::is_supported_source_language(&language) {
        return Err(format!("原文语言仅支持 ja/en/ko/fr/de/ru/es/it：{language}").into());
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
        [action, id, yes, prepared]
            if !id.starts_with("--") && yes == "--yes" && prepared == "--confirm-prepared" =>
        {
            match action.as_str() {
                "finish" => RecoveryAction::Finish {
                    operation_id: id.clone(),
                    confirm_prepared: true,
                },
                "abandon" => RecoveryAction::Abandon {
                    operation_id: id.clone(),
                    confirm_prepared: true,
                },
                _ => return Err(RECOVERY_USAGE.into()),
            }
        }
        [action, id, yes] if !id.starts_with("--") && yes == "--yes" && action == "finish" => {
            RecoveryAction::Finish {
                operation_id: id.clone(),
                confirm_prepared: false,
            }
        }
        [action, id, yes] if !id.starts_with("--") && yes == "--yes" && action == "abandon" => {
            RecoveryAction::Abandon {
                operation_id: id.clone(),
                confirm_prepared: false,
            }
        }
        // 清理同样默认不动手：不加 `--yes` 只预览，删掉的临时文件找不回来。
        [action] if action == "temps" => RecoveryAction::Temps { apply: false },
        [action, yes] if action == "temps" && yes == "--yes" => {
            RecoveryAction::Temps { apply: true }
        }
        _ => return Err(RECOVERY_USAGE.into()),
    };
    Ok(TextAction::Recovery(action))
}
