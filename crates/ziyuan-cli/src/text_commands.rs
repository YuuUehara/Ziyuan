//! `ziyuan text`：离线文本项目与译书命令分派。不读模型配置、不联网。

use std::env;
use std::error::Error;
use std::io::Read;
use std::path::PathBuf;
use ziyuan_app::Session;
use ziyuan_app::text_project::{
    ImportOptions, ImportPreview, ImportSource, NewProject, Project, Provenance, TextProjects,
    ValidationReport,
};
use ziyuan_app::translation::{DraftSave, ExportFormat, ExportLayout, ExportOptions, Translations};
use ziyuan_storage::EventLog;
use ziyuan_storage::recovery::{ApplicationV2, DecodedRecovery, SceneEvidence, classify_scene};
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
  ziyuan text preview <项目ID> <文件>... [--ruby] [--heading-level <1|2>]
                     [--chapter-title <章名>]...
                                       只读预览 TXT/MD，显示章序、原文字节范围、段落、提示块及警告和指纹
  ziyuan text import <项目ID> <文件>... [--ruby] [--heading-level <1|2>]
                    [--chapter-title <章名>]... --confirm <预览指纹>
                                       MD（.md/.markdown）、ruby 或新解析选项须先 preview 再确认；
                                       TXT 使用新解析/BOM 处理也须 preview + --confirm；不加选项仍走旧 TXT 导入
                                       重复 --chapter-title 按章序覆盖全部章名；确认时须保持文件顺序与选项不变
  ziyuan text show <项目ID>            核对原文副本后列出章节、段落与提示块
  ziyuan text usage <项目ID>           只读汇总四阶段用量记录、状态与分币种估价；需已有朝廷，不调用模型
                                       缺失用量为未知；用量记录和工具调用不是 HTTP 请求数，估价不是账单
                                       未设本件预算不等于免费，也不取消朝廷或官员限额
  ziyuan text export-finished <项目ID> [--chapter <章ID>]... [--format txt|md|html]
                            [--layout chapters|book] [--output <new.tar>]
                                       导出有效定稿，默认 TXT、按章；缺块或失效则整批拒绝，需要朝廷
  ziyuan text export-originals <项目ID> [--chapter <章ID>]... [--output <new.tar>]
                                       原件按原字节导出，不依赖朝廷，不接受 --format/--layout
    导出不调用模型或网络；不加 --output 只读预览文件名称、大小、警告与清单（--json 也不输出字节）。
    加 --output 重新核验后写单个 TAR：内含 TXT/MD/HTML 定稿或原件及 manifest 清单；不覆盖现有路径，
    不创建父目录。未选章时按项目章序导出全部；--chapter 可重复选择不同章节。
  ziyuan text check <项目ID> --candidate <译稿.json> [--block <提示块ID>]
                                       检查候选译稿的结构（漏段、重复、顺序、陌生段号、空文本）；
                                       提示块默认取自译稿的 block_id。只查结构，不评判译文质量；
                                       未通过时仍输出报告，退出码 1
  ziyuan text import-docx <项目ID> <文件.docx>
                                       导入 DOCX 文字（保留原件，供回填粗体/斜体/表格/图片）
  ziyuan text import-epub <项目ID> <文件.epub>
                                       导入 EPUB 正文（保留原件，供回填行内标记与图片）
  ziyuan text import-srt <项目ID> <文件.srt>
                                       导入 SRT 字幕文字，时间轴另存 srt.json
  ziyuan text export-srt <项目ID> <文件.srt>
                                       按现行译稿导出 SRT；段数与字幕数不一致即拒绝
  ziyuan text export-docx <项目ID> <输出.docx>
                                       按已定稿译稿回填原 DOCX，不覆盖已有文件
  ziyuan text export-epub <项目ID> <输出.epub>
                                       按已定稿译稿回填原 EPUB，不覆盖已有文件
  ziyuan text human show <项目ID> <块ID>
                                       只读查看本块的人工草稿与修订号
  ziyuan text human save <项目ID> <块ID> --file <草稿.json>
                                       保存人工草稿（只写草稿，不动译稿与事件簿）
  ziyuan text human submit <项目ID> <块ID>
                                       冻结当前修订号的草稿为候选并送审（入簿送审绑定）
  ziyuan text human approve <项目ID> <块ID>
                                       准奏最新送审：在一次受管写入里写入 zh.json 并入簿准奏；
                                       中途失败时保留恢复记录，用 text recovery 核对收尾
  ziyuan text human finalize <项目ID> <块ID>
                                       人工定稿：要求磁盘已是准奏目标，只记定稿事实
  ziyuan text human apply <项目ID> <块ID>
                                       只读核对准奏时已落盘的 zh.json（不再写文件）
  ziyuan text recovery status [--json]
                                       只读报告工作目录的受管写入世代号与恢复记录；
                                       仍有未收尾的写入时退出码 1
  ziyuan text recovery finish <操作ID> --yes [--confirm-prepared]
                                       推进世代并清理已完成（或仅发布未落盘）的恢复记录；
                                       发布前中断（Prepared）须加 --confirm-prepared 才继续；
                                       不重做业务写入，也不重算目标文件
  ziyuan text recovery abandon <操作ID> --yes [--confirm-prepared]
                                       仅在目标仍是旧状态、未落盘任何字节时放弃那次写入；
                                       发布前中断（Prepared）须加 --confirm-prepared 才放弃
  ziyuan text recovery temps [--yes]
                                       列出工作目录里遗留的 .ziyuan-tmp-* 与其租约；
                                       默认只预览，加 --yes 才删除可证明陈旧的项
  以上命令均可加 --json，以 JSON 输出到标准输出。项目存于 .ziyuan/text-projects/<项目ID>/。"#;

pub(crate) enum TextAction {
    Continuity(crate::continuity_commands::Action),
    Help,
    Memory(crate::memory_commands::MemoryAction),
    Translation(crate::translation_commands::TranslationAction),
    Verification(crate::verification_commands::Action),
    Recovery(RecoveryAction),
    Init(NewProject),
    Import(String, Vec<PathBuf>),
    ImportDocx(String, PathBuf),
    ImportSrt(String, PathBuf),
    ExportSrt(String, PathBuf),
    ImportEpub(String, PathBuf),
    ExportDocx(String, PathBuf),
    ExportEpub(String, PathBuf),
    Preview(String, Vec<ImportSource>, ImportOptions),
    ImportPrepared(String, Vec<ImportSource>, ImportOptions, String),
    Show(String),
    Usage(String),
    Export {
        project: String,
        options: ExportOptions,
        originals: bool,
        output: Option<PathBuf>,
    },
    Check {
        id: String,
        candidate: PathBuf,
        block: Option<String>,
    },
    HumanShow {
        project: String,
        block: String,
    },
    HumanSave {
        project: String,
        block: String,
        file: PathBuf,
    },
    HumanSubmit {
        project: String,
        block: String,
    },
    HumanApprove {
        project: String,
        block: String,
    },
    HumanFinalize {
        project: String,
        block: String,
    },
    HumanApply {
        project: String,
        block: String,
    },
}

const INIT_USAGE: &str = "用法: ziyuan text init --title <标题> --language <ja|en> --source <来源> --rights <权利声明> [--author <作者>] [--version <版本>] [--date <取得日期>] [--scope <使用范围>]";
const IMPORT_USAGE: &str = "用法: ziyuan text import <项目ID> <文件>... [--ruby] [--heading-level <1|2>] [--chapter-title <章名>]... [--confirm <预览指纹>]；MD 或解析选项须先 text preview 再 --confirm";
const PREVIEW_USAGE: &str = "用法: ziyuan text preview <项目ID> <文件>... [--ruby] [--heading-level <1|2>] [--chapter-title <章名>]...";
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
const EXPORT_FINISHED_USAGE: &str = "用法: ziyuan text export-finished <项目ID> [--chapter <章ID>]... [--format txt|md|html] [--layout chapters|book] [--output <new.tar>]";
const EXPORT_ORIGINALS_USAGE: &str =
    "用法: ziyuan text export-originals <项目ID> [--chapter <章ID>]... [--output <new.tar>]";
const CHECK_USAGE: &str =
    "用法: ziyuan text check <项目ID> --candidate <译稿.json> [--block <提示块ID>]";
const RECOVERY_USAGE: &str = "用法: ziyuan text recovery status [--json] | ziyuan text recovery finish <操作ID> --yes [--confirm-prepared] | ziyuan text recovery abandon <操作ID> --yes [--confirm-prepared] | ziyuan text recovery temps [--json] [--yes]";

/// 受管写入（准奏落盘）的恢复操作：只处理先前遗留的恢复记录，不执行业务写入。
pub(crate) enum RecoveryAction {
    Status,
    Finish {
        operation_id: String,
        /// 仅对发布前中断（Prepared）的显式确认；默认拒绝。
        confirm_prepared: bool,
    },
    Abandon {
        operation_id: String,
        confirm_prepared: bool,
    },
    /// 清理遗留的 `.ziyuan-tmp-*`：默认只预览，`--yes` 才删。
    Temps {
        apply: bool,
    },
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
        | "status" | "resume" | "references" | "qa" | "termbase" | "concordance" | "tm"
        | "comment" | "compare" | "version" => {
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
            "--heading-level" | "--chapter-title" | "--confirm" => {
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

pub(crate) fn run(action: TextAction, json: bool) -> Result<(), Box<dyn Error>> {
    if let TextAction::Help = action {
        println!(
            "{TEXT_HELP}\n\n{}\n\n{}\n\n{}\n\n{}",
            crate::translation_commands::TRANSLATION_HELP,
            crate::memory_commands::MEMORY_HELP,
            crate::verification_commands::HELP,
            crate::continuity_commands::HELP
        );
        return Ok(());
    }
    let projects = TextProjects::new(env::current_dir()?);
    match action {
        TextAction::Continuity(action) => return crate::continuity_commands::run(action, json),
        TextAction::Help => unreachable!("已在上面处理"),
        TextAction::Memory(action) => return crate::memory_commands::run(action, json),
        TextAction::Translation(action) => return crate::translation_commands::run(action, json),
        TextAction::Verification(action) => return crate::verification_commands::run(action, json),
        TextAction::Recovery(action) => return run_recovery(action, json),
        TextAction::Usage(project) => return run_usage(&project, json),
        TextAction::Export {
            project,
            options,
            originals,
            output,
        } => return run_export(&project, &options, originals, output, json),
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
        TextAction::ImportEpub(id, file) => {
            let translations = ziyuan_app::translation::Translations::new(env::current_dir()?);
            let chapters = translations.import_epub(&id, &file)?;
            if json {
                print_json(serde_json::json!({"project": id, "documents": chapters}))?;
            } else {
                println!("已从 EPUB 导入 {chapters} 个正文文档。");
            }
        }
        TextAction::ImportSrt(id, file) => {
            let translations = ziyuan_app::translation::Translations::new(env::current_dir()?);
            let cues = translations.import_srt(&id, &file)?;
            if json {
                print_json(serde_json::json!({"project": id, "cues": cues}))?;
            } else {
                println!("已从 SRT 导入 {cues} 条字幕（时间轴存于 srt.json）。");
            }
        }
        TextAction::ExportSrt(id, file) => {
            let translations = ziyuan_app::translation::Translations::new(env::current_dir()?);
            let cues = translations.export_srt(&id, &file)?;
            if json {
                print_json(serde_json::json!({"project": id, "cues": cues, "file": file}))?;
            } else {
                println!("已导出 {cues} 条字幕：{}", file.display());
            }
        }
        TextAction::ExportDocx(id, file) => {
            let root = env::current_dir()?;
            // 先按成稿导出规则核对全部块已有效定稿，再回填，不另造一套资格判断。
            Session::open(EventLog::new(&root))?.export_finished(
                &root,
                &id,
                &ExportOptions::default(),
            )?;
            let paragraphs = Translations::new(&root).export_docx(&id, &file)?;
            if json {
                print_json(
                    serde_json::json!({"project": id, "file": file, "paragraphs": paragraphs}),
                )?;
            } else {
                println!(
                    "已回填 DOCX：{}（{paragraphs} 段；粗体、斜体、表格与图片保留）。",
                    file.display()
                );
            }
        }
        TextAction::ExportEpub(id, file) => {
            let root = env::current_dir()?;
            Session::open(EventLog::new(&root))?.export_finished(
                &root,
                &id,
                &ExportOptions::default(),
            )?;
            let paragraphs = Translations::new(&root).export_epub(&id, &file)?;
            if json {
                print_json(
                    serde_json::json!({"project": id, "file": file, "paragraphs": paragraphs}),
                )?;
            } else {
                println!(
                    "已回填 EPUB：{}（{paragraphs} 段；行内粗斜体、表格与图片保留）。",
                    file.display()
                );
            }
        }
        TextAction::ImportDocx(id, file) => {
            let translations = ziyuan_app::translation::Translations::new(env::current_dir()?);
            let lines = translations.import_docx(&id, &file)?;
            if json {
                print_json(serde_json::json!({"project": id, "paragraphs": lines}))?;
            } else {
                println!("已从 DOCX 导入 {lines} 行文本。");
            }
        }
        TextAction::Import(id, files) => {
            if files.iter().any(|file| {
                file.extension().is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown")
                })
            }) {
                return Err("Markdown 导入须先运行 text preview，再以相同文件与选项加 --confirm <预览指纹> 确认".into());
            }
            let project = projects.import(&id, &files)?;
            match json {
                true => print_json(serde_json::to_value(&project)?)?,
                false => {
                    println!("已导入 {} 章：", project.chapters.len());
                    print_chapters(&projects, &project)?;
                }
            }
        }
        TextAction::Preview(id, sources, options) => {
            let preview = projects.preview_import(&id, &sources, &options)?;
            if json {
                print_json(serde_json::to_value(&preview)?)?;
            } else {
                print_preview(&preview);
            }
        }
        TextAction::ImportPrepared(id, sources, options, expected) => {
            let project = projects.import_prepared(&id, &sources, &options, &expected)?;
            if json {
                print_json(serde_json::to_value(&project)?)?;
            } else {
                println!("已导入 {} 章：", project.chapters.len());
                print_chapters(&projects, &project)?;
            }
        }
        TextAction::Show(id) => {
            let project = projects.show(&id)?;
            match json {
                true => print_json(serde_json::to_value(&project)?)?,
                false => print_project(&projects, &project)?,
            }
        }
        TextAction::HumanSave {
            project,
            block,
            file,
        } => {
            let bytes = read_draft(&file)?;
            let save: DraftSave = serde_json::from_slice(&bytes).map_err(|error| {
                format!("草稿文件不是合法的 DraftSave JSON（须有 request_id／expected_revision／basis／paragraphs）：{error}")
            })?;
            let translations = Translations::new(env::current_dir()?);
            let receipt = translations.save_human_draft(&project, &block, &save)?;
            if json {
                print_json(serde_json::to_value(&receipt)?)?;
            } else {
                println!(
                    "已保存人工草稿：{project} / {block}（修订号 {}）",
                    receipt.revision
                );
                println!("下一步：ziyuan text human submit {project} {block}");
            }
        }
        TextAction::HumanApply { project, block } => {
            let translations = Translations::new(env::current_dir()?);
            let approval = translations.apply_human_target(&project, &block)?;
            if json {
                print_json(serde_json::json!({
                    "project": project,
                    "block": block,
                    "applied": false,
                    "already_written": true,
                    "approval_request_id": approval,
                }))?;
            } else {
                println!(
                    "准奏时已落盘，本命令只作核对、未写任何文件：{project} / {block}（准奏 {approval}）"
                );
                println!("下一步：ziyuan text human finalize {project} {block}");
            }
        }
        TextAction::HumanFinalize { project, block } => {
            let root = env::current_dir()?;
            let translations = Translations::new(&root);
            let workspace = ziyuan_storage::Workspace::open(&root).map_err(|e| e.to_string())?;
            let mut guard = workspace
                .lock(ziyuan_storage::LockScope::Workspace)
                .map_err(|e| e.to_string())?;
            guard.upgrade_write_protocol().map_err(|e| e.to_string())?;
            translations
                .texts()
                .upgrade_project_protocol(&project, &guard)?;
            let finalized = translations.finalize_human_translation(&guard, &project, &block)?;
            let snapshot = translations.auto_snapshot(
                &project,
                &block,
                ziyuan_app::translation::VersionState::Finalized,
            );
            if json {
                let mut value = serde_json::json!({
                    "project": project,
                    "block": block,
                    "finalized": finalized,
                });
                if let Some(snapshot) = &snapshot {
                    value["snapshot"] = serde_json::to_value(snapshot)?;
                }
                print_json(value)?;
            } else if finalized {
                println!("已定稿：{project} / {block}");
                crate::translation_commands::print_snapshot(&snapshot);
                println!("下一步：ziyuan text export-finished {project}");
            } else {
                println!("这次定稿此前已入簿，未重复追加：{project} / {block}");
                println!("下一步：ziyuan text export-finished {project}");
            }
        }
        TextAction::HumanApprove { project, block } => {
            let root = env::current_dir()?;
            let translations = Translations::new(&root);
            let workspace = ziyuan_storage::Workspace::open(&root).map_err(|e| e.to_string())?;
            let mut guard = workspace
                .lock(ziyuan_storage::LockScope::Workspace)
                .map_err(|e| e.to_string())?;
            guard.upgrade_write_protocol().map_err(|e| e.to_string())?;
            translations
                .texts()
                .upgrade_project_protocol(&project, &guard)?;
            let approved = translations.approve_human_submission(&mut guard, &project, &block)?;
            let snapshot = translations.auto_snapshot(
                &project,
                &block,
                ziyuan_app::translation::VersionState::Approved,
            );
            if json {
                let mut value = serde_json::json!({
                    "project": project,
                    "block": block,
                    "approved": approved,
                });
                if let Some(snapshot) = &snapshot {
                    value["snapshot"] = serde_json::to_value(snapshot)?;
                }
                print_json(value)?;
            } else if approved {
                println!("已准奏并落盘：{project} / {block}");
                crate::translation_commands::print_snapshot(&snapshot);
                println!("下一步：ziyuan text human finalize {project} {block}");
            } else {
                println!("这次准奏此前已入簿并落盘，未重复追加：{project} / {block}");
                println!("下一步：ziyuan text human finalize {project} {block}");
            }
        }
        TextAction::HumanSubmit { project, block } => {
            let root = env::current_dir()?;
            let translations = Translations::new(&root);
            let view = translations.peek_human_draft(&project, &block)?;
            let draft = view
                .draft
                .ok_or("尚无人工草稿，请先用 ziyuan text human save 保存")?;
            let request_id = ziyuan_app::translation::human_submit_request_id(&draft.revision);
            let binding = translations.prepare_human_submission(
                &project,
                &block,
                &request_id,
                &request_id,
                &draft.revision,
            )?;
            let workspace = ziyuan_storage::Workspace::open(&root).map_err(|e| e.to_string())?;
            let mut guard = workspace
                .lock(ziyuan_storage::LockScope::Workspace)
                .map_err(|e| e.to_string())?;
            guard.upgrade_write_protocol().map_err(|e| e.to_string())?;
            translations
                .texts()
                .upgrade_project_protocol(&project, &guard)?;
            let published = translations.publish_human_binding(&guard, &binding)?;
            if json {
                print_json(serde_json::json!({
                    "project": project,
                    "block": block,
                    "published": published,
                    "binding_hash": binding.request_hash,
                }))?;
            } else if published {
                println!("已送审：{project} / {block}");
                println!("下一步：ziyuan text human approve {project} {block}");
            } else {
                println!("这次送审此前已入簿，未重复追加：{project} / {block}");
                println!("下一步：ziyuan text human approve {project} {block}");
            }
        }
        TextAction::HumanShow { project, block } => {
            let translations = Translations::new(env::current_dir()?);
            let view = translations.peek_human_draft(&project, &block)?;
            if json {
                print_json(serde_json::to_value(&view)?)?;
            } else {
                println!("人工稿：{project} / {block}");
                match &view.draft {
                    None => println!("尚无草稿（只读）"),
                    Some(draft) => {
                        println!("修订号：{}", draft.revision);
                        println!("段落：{}", draft.paragraphs.len());
                        for paragraph in &draft.paragraphs {
                            println!("  {}：{}", paragraph.id, paragraph.text);
                        }
                    }
                }
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

fn run_usage(project: &str, json: bool) -> Result<(), Box<dyn Error>> {
    let root = env::current_dir()?;
    // Validate the project and its original copies before consulting the ledger.
    TextProjects::new(&root).show(project)?;
    let usage = match Session::open(EventLog::new(&root)) {
        Ok(session) => session.translation_usage(project, ziyuan_llm::price_quote)?,
        Err(ziyuan_app::AppError::NotFounded) => {
            return Err("用量账本需要朝廷：请先运行 ziyuan init（本命令不会自动建立朝廷）".into());
        }
        Err(error) => return Err(error.into()),
    };
    if json {
        return print_json(serde_json::to_value(&usage)?);
    }
    println!(
        "项目 {}：译书用量账本（已报告小计，不是完整调用计量）",
        usage.project
    );
    println!(
        "token 是模型计量文本的单位；估价不是账单。未设本件限额不等于免费，也不取消朝廷或官员限额。"
    );
    for stage in &usage.stages {
        println!("阶段 {}：", stage.kind.label());
        print_usage_totals(&stage.totals);
    }
    for task in &usage.tasks {
        let budget = task
            .budget
            .map_or_else(|| "未设本件限额".to_owned(), |n| format!("{n} tokens"));
        let recorded = task
            .recorded_execution_usage
            .map_or_else(|| "未知".to_owned(), |u| u.to_string());
        let status = match task.status.as_str() {
            "pending" => "待办",
            "assigned" => "已指派",
            "completed" => "已完成",
            "failed" => "失败",
            "cancelled" => "已撤销",
            other => other,
        };
        println!(
            "任务 #{} / {} / {}：状态={}；指派尝试={}；本件预算={}；已记录承办用量={}；已记录小计与任务累计相符={}",
            task.task.0,
            task.block,
            task.kind.label(),
            status,
            task.attempts,
            budget,
            recorded,
            if task.usage_reconciled { "是" } else { "否" }
        );
        print_usage_totals(&task.totals);
        for charge in &task.charges {
            let model = charge.model.as_deref().unwrap_or("未知");
            let purpose = match charge.purpose.as_str() {
                "execution" => "承办",
                "draft" => "票拟",
                "split" => "分办",
                "censure" => "都察院审查",
                "recheck" => "复勘",
                "debate" => "廷议",
                other => other,
            };
            println!("  记账项：{}；模型={}；{}", purpose, model, charge.usage);
            match &charge.rate {
                Some(rate) => println!(
                    "    费率：输入={} / 输出={} 每百万 tokens，币种={}；来源={}",
                    rate.input_per_million, rate.output_per_million, rate.currency, rate.source
                ),
                None => println!("    费率与来源：未知（未定价）"),
            }
            match (charge.estimated_cost, charge.rate.as_ref()) {
                (Some(cost), Some(rate)) => {
                    println!("    已定价估算：{cost:.6} {}（非账单）", rate.currency)
                }
                _ => println!("    估算费用：未知"),
            }
        }
    }
    println!("项目合计：");
    print_usage_totals(&usage.totals);
    for note in &usage.notes {
        let note = note
            .replace("缺失 usage 为未知", "未报告用量视为未知")
            .replace("有 usage 的任务", "有用量记录的任务")
            .replace("usage 事件数", "用量记录数")
            .replace("usage", "用量")
            .replace("trace", "追踪标识")
            .replace("全局/官员余额", "朝廷或官员剩余额度")
            .replace("操作者用时", "人工用时");
        println!("说明：{note}");
    }
    Ok(())
}

fn print_usage_totals(totals: &ziyuan_app::translation::UsageTotals) {
    let recorded = totals
        .recorded_usage
        .map_or_else(|| "未知".to_owned(), |u| u.to_string());
    let statuses = totals
        .statuses
        .iter()
        .map(|(status, count)| {
            let label = match status.as_str() {
                "pending" => "待办",
                "assigned" => "已指派",
                "completed" => "已完成",
                "failed" => "失败",
                "cancelled" => "已撤销",
                other => other,
            };
            format!("{label}={count}")
        })
        .collect::<Vec<_>>()
        .join("、");
    println!(
        "  任务={}；状态=[{}]；已记录 tokens={}；缺承办用量的任务={}",
        totals.tasks, statuses, recorded, totals.tasks_without_execution_usage
    );
    println!(
        "  用量记录={}；工具调用={}；工具失败={}（均非 HTTP 请求数）",
        totals.usage_events, totals.tool_calls, totals.tool_failures
    );
    if totals.estimates.is_empty() {
        println!("  已定价小计：无（费用未知，不作零费用承诺）");
    }
    for (currency, amount) in &totals.estimates {
        println!("  已定价小计：{amount:.6} {currency}（估算，非账单）");
    }
    println!("  未定价 tokens：{}", totals.unpriced_usage);
}

fn run_export(
    project: &str,
    options: &ExportOptions,
    originals: bool,
    output: Option<PathBuf>,
    json: bool,
) -> Result<(), Box<dyn Error>> {
    let root = env::current_dir()?;
    // Each invocation builds a fresh, validated bundle; never reuse an earlier preview.
    let bundle = if originals {
        Translations::new(&root).export_originals(project, &options.chapters)?
    } else {
        Session::open(EventLog::new(&root))?.export_finished(&root, project, options)?
    };
    if let Some(destination) = &output {
        bundle.write_archive(&root, destination)?;
    }
    if json {
        let files: Vec<_> = bundle
            .files
            .iter()
            .map(|file| serde_json::json!({"name": file.name, "size": file.bytes.len()}))
            .collect();
        print_json(serde_json::json!({
            "project": project,
            "output": output,
            "files": files,
            "manifest": bundle.manifest,
            "warnings": bundle.warnings,
        }))?;
    } else {
        match output {
            Some(destination) => println!("已导出 TAR：{}", destination.display()),
            None => println!("导出预览（只读；未写归档）：{project}"),
        }
        for warning in &bundle.warnings {
            println!("警告：{warning}");
        }
        for file in &bundle.files {
            println!("  {}：{} 字节", file.name, file.bytes.len());
        }
        println!("清单：{}", serde_json::to_string_pretty(&bundle.manifest)?);
    }
    Ok(())
}

fn print_preview(preview: &ImportPreview) {
    println!(
        "导入预览（只读）：{} · {} 章",
        preview.project,
        preview.chapters.len()
    );
    for (index, chapter) in preview.chapters.iter().enumerate() {
        println!(
            "  {}. {} {}：{} 段，{} 个提示块",
            index + 1,
            chapter.id,
            chapter.source_name,
            chapter.paragraphs.len(),
            chapter.blocks.len()
        );
        if let Some(parsing) = &chapter.parsing {
            println!(
                "    标题：{} · 格式：{:?} · 原文字节范围：{}..{}",
                parsing.title, parsing.format, parsing.byte_start, parsing.byte_end
            );
            for warning in &parsing.warnings {
                println!(
                    "    警告 {}..{}：{}",
                    warning.byte_start, warning.byte_end, warning.message
                );
            }
        }
    }
    println!("预览指纹：{}", preview.fingerprint);
    println!("确认导入：用相同文件顺序与选项运行 text import，并加 --confirm <预览指纹>。");
}

fn print_json(value: serde_json::Value) -> Result<(), Box<dyn Error>> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn read_draft(path: &std::path::Path) -> Result<Vec<u8>, Box<dyn Error>> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("读不了草稿文件 {}：{error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(CANDIDATE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > CANDIDATE_BYTES {
        return Err(format!("草稿文件超过 {CANDIDATE_BYTES} 字节").into());
    }
    Ok(bytes)
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
        RecoveryAction::Temps { apply } => return run_temps(&workspace, apply, json),
        RecoveryAction::Status => {
            let generation = workspace.generation()?;
            let record = workspace.pending_record_any()?;
            let state = match &record {
                None => "idle",
                Some(DecodedRecovery::V1(record)) => state_code(record.state),
                Some(DecodedRecovery::V2(record)) => state_code(record.state),
            };
            let next: String = match &record {
                None if generation % 2 != 0 => {
                    "上一次受管写入只留下奇数代次、没有恢复记录；请人工核对工作区，不要手工改代次"
                        .into()
                }
                None => "无待处理的受管写入".into(),
                Some(DecodedRecovery::V2(record)) => second_format_next(record),
                Some(DecodedRecovery::V1(record)) => {
                    let op = &record.operation_id;
                    match record.state {
                        RecordState::Prepared | RecordState::Abandoned => format!(
                            "可用 ziyuan text recovery finish {op} --yes 收尾；若目标仍是旧内容，也可用 ziyuan text recovery abandon {op} --yes 放弃"
                        ),
                        RecordState::Published => format!(
                            "须核对目标后用 ziyuan text recovery finish {op} --yes 收尾；目标被改过时不可 abandon"
                        ),
                        RecordState::Completed => {
                            format!("用 ziyuan text recovery finish {op} --yes 推进世代并清理记录")
                        }
                    }
                }
            };
            if json {
                let value = match &record {
                    None => serde_json::json!({ "generation": generation, "state": state }),
                    Some(DecodedRecovery::V1(record)) => serde_json::json!({
                        "generation": generation,
                        "format": 1,
                        "state": state,
                        "operation_id": record.operation_id,
                        "task": record.application.task,
                        "skipped": record.application.skipped,
                        "targets": record.targets.iter().map(|t| t.path.clone()).collect::<Vec<_>>(),
                        "next": next,
                    }),
                    Some(DecodedRecovery::V2(record)) => {
                        let mut value = serde_json::json!({
                            "generation": generation,
                            "format": 2,
                            "state": state,
                            "operation_id": record.operation_id,
                            "targets": record.targets.iter().map(|t| t.path.clone()).collect::<Vec<_>>(),
                            "next": next,
                        });
                        match &record.application {
                            ApplicationV2::Model(app) => {
                                value["kind"] = "model".into();
                                value["task"] = app.task.into();
                                value["skipped"] = serde_json::json!(app.skipped);
                            }
                            ApplicationV2::Human(app) => {
                                value["kind"] = "human".into();
                                value["project"] = app.project.clone().into();
                                value["block"] = app.block.clone().into();
                                value["approval_request_id"] =
                                    app.approval_request_id.clone().into();
                                value["target_format"] = app.target_format.clone().into();
                                value["logical_path"] = app.logical_path.clone().into();
                            }
                        }
                        value
                    }
                };
                print_json(value)?;
            } else {
                println!("世代号：{generation}（{}）", even_odd(generation));
                let suffix = match &record {
                    Some(DecodedRecovery::V2(_)) => "（格式第二版）",
                    _ => "",
                };
                println!("恢复记录：{state}{suffix}");
                match &record {
                    Some(DecodedRecovery::V1(record)) => {
                        println!("  操作 ID：{}", record.operation_id);
                        println!("  任务：#{}", record.application.task);
                        println!("  目标：{} 个", record.targets.len());
                        for target in &record.targets {
                            println!("    {}", target.path);
                        }
                    }
                    Some(DecodedRecovery::V2(record)) => {
                        println!("  操作 ID：{}", record.operation_id);
                        match &record.application {
                            ApplicationV2::Model(app) => {
                                println!("  来源：模型诏令来源（任务 #{}）", app.task);
                            }
                            ApplicationV2::Human(app) => {
                                println!("  来源：人工准奏来源");
                                println!("  项目：{}  块：{}", app.project, app.block);
                                println!("  准奏请求：{}", app.approval_request_id);
                                println!("  目标格式：{}", app.target_format);
                            }
                        }
                        println!("  目标：{} 个", record.targets.len());
                        for target in &record.targets {
                            println!("    {}", target.path);
                        }
                    }
                    None => {}
                }
                println!("下一步：{next}");
            }
            // 尚未收尾（世代为奇数）或仍有未清理的记录时，退出码 1：脚本据此判断还能不能写入。
            let unfinished = generation % 2 != 0 || record.is_some();
            if unfinished {
                return Err(RecoveryPending.into());
            }
        }
        RecoveryAction::Finish {
            operation_id,
            confirm_prepared,
        } => {
            finish_recovery(&workspace, &operation_id, confirm_prepared, json)?;
        }
        RecoveryAction::Abandon {
            operation_id,
            confirm_prepared,
        } => {
            let mut guard = workspace.lock(LockScope::Workspace)?;
            // 放弃的前提是「什么都没发生」：目标必须仍是旧内容，事件也一条没进事件簿。
            // 只靠 CLI 信任调用者不行，这里逐项核对；任何一项不成立都不放弃。
            let pending = workspace
                .pending_record_any()?
                .ok_or("没有待收尾的恢复记录；用 ziyuan text recovery status 查看")?;
            let record = match pending {
                DecodedRecovery::V1(_) if confirm_prepared => {
                    return Err(confirm_prepared_misused(
                        "abandon",
                        &operation_id,
                        "第一版恢复记录",
                    ));
                }
                DecodedRecovery::V1(record) => record,
                DecodedRecovery::V2(record) => {
                    return abandon_second_format(
                        &workspace,
                        &mut guard,
                        &record,
                        &operation_id,
                        confirm_prepared,
                        json,
                    );
                }
            };
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

/// 失败项的文本行：路径与消息都过报告的脱敏出口（与 JSON 同一个函数）。
///
/// 单独提出来的理由是可测：报告里的消息今天都不带路径，但契约要求文本与 JSON 走同一条
/// 脱敏路径，所以用例要能直接喂一条带绝对路径的消息。
fn problem_line(
    report: &ziyuan_runtime::tools::temp_cleanup::Report,
    problem: &ziyuan_runtime::tools::temp_cleanup::Problem,
) -> String {
    format!(
        "  失败：{}：{}",
        report.display_relative(&problem.path),
        report.scrub_message(&problem.message)
    )
}

/// 手工清理遗留临时文件。默认预览；`--yes` 才真删。
///
/// 不取工作目录锁：那会把正在写入的进程挡到超时，而非阻塞试探已经足够给出可信判定。
fn run_temps(workspace: &Workspace, apply: bool, json: bool) -> Result<(), Box<dyn Error>> {
    use ziyuan_runtime::tools::temp_cleanup::{Kind, MAX_DEPTH, Outcome, sweep_workspace};

    // 集合构造失败就整轮不清理：宁可什么都不做，也不要误删恢复记录引用的项。
    // 报错文案要说清“一份都没删”，否则用户会以为报告没打全。
    let protected = workspace
        .protected_targets()
        .map_err(|e| format!("读取恢复记录失败，本次未清理任何临时文件：{e}"))?;
    let report = sweep_workspace(workspace.root(), &protected, apply);
    if json {
        print_json(report.to_json())?;
    } else {
        let temps = report.counts(Kind::Temp);
        let leases = report.counts(Kind::Lease);
        println!("范围：{}（含子目录）", report.scope());
        println!(
            "模式：{}",
            if apply {
                "删除"
            } else {
                "预览，加 --yes 才删"
            }
        );
        println!(
            "临时文件：候选 {}，可清理 {}，已删除 {}，未删除 {}",
            temps.candidates, temps.removable, temps.removed, temps.kept
        );
        println!(
            "租约：候选 {}，可清理 {}，已删除 {}，未删除 {}",
            leases.candidates, leases.removable, leases.removed, leases.kept
        );
        // skipped 与 capped 是平级的两类“没进去”，缩进也一致。
        if !report.skipped.is_empty() {
            // 报路径而不是名字：两个子目录里各有一个 `target` 时，光看名字分不出没扫的是哪个。
            println!(
                "  未进入的目录（按路径跳过）：{}",
                report.skipped.join("、")
            );
        }
        if !report.capped.is_empty() {
            // 深度上限不是失败：只说明这一层没进去，不影响退出码。
            println!(
                "  未扫描（超过 {MAX_DEPTH} 层深度上限）：{}",
                report.capped.join("、")
            );
            println!("  这些目录里的遗留文件不在本次报告范围内。");
        }
        for item in &report.items {
            // 原因名与 JSON 的 `reason` 出自同一处：可删除项照样显示
            // WriterDeadLeaseVerified / OrphanLease，不在这里手拼第二套名字。
            let reason = item.reason_name();
            // 已删除与可清理分开写：预览里的“可清理”是待办提示，不是这一轮做了什么。
            // 保留项写“未删除”而不是“保留”：既让三种状态同为三字（列不歪），
            // 也不让“保留 TempAlreadyAbsent”读成自相矛盾。
            let status = match item.outcome {
                Outcome::Remove if item.acted => "已删除",
                Outcome::Remove => "可清理",
                Outcome::Keep(_) => "未删除",
            };
            // 报告只出相对路径：绝对路径不外泄。
            println!(
                "  {status} {reason:<22} {}",
                report.display_relative(&item.path)
            );
        }
        for problem in &report.problems {
            // 与 JSON 同一个脱敏出口：文本模式也不能带出绝对路径。
            println!("{}", problem_line(&report, problem));
        }
        for dir in &report.unscanned {
            println!("  读不了的目录：{}", report.display_relative(dir));
        }
    }
    // 只有扫描/删除的 I/O 失败与读不了的目录才退 1；预览发现可清理项仍算成功，
    // 报告里的“可清理 N”就是待办提示（设计：仅保留活跃或未知项不算执行失败）。
    if report.needs_attention() {
        // 失败提示要说清去哪儿看：`--json` 下标准输出是 JSON，没有“上面”的报告。
        return Err(match json {
            true => "临时文件清理未能完成：见报告中的 problems 与 unscanned".into(),
            false => "临时文件清理未能完成；见上面的逐项报告".into(),
        });
    }
    Ok(())
}

/// 收尾：只推进世代并清理记录，绝不重做业务写入。
fn finish_recovery(
    workspace: &Workspace,
    operation_id: &str,
    confirm_prepared: bool,
    json: bool,
) -> Result<(), Box<dyn Error>> {
    let mut guard = workspace.lock(LockScope::Workspace)?;
    let pending = workspace
        .pending_record_any()?
        .ok_or("没有待收尾的恢复记录；用 ziyuan text recovery status 查看")?;
    let record = match pending {
        DecodedRecovery::V1(_) if confirm_prepared => {
            return Err(confirm_prepared_misused(
                "finish",
                operation_id,
                "第一版恢复记录",
            ));
        }
        DecodedRecovery::V1(record) => record,
        DecodedRecovery::V2(record) => {
            return finish_second_format(
                workspace,
                &mut guard,
                &record,
                operation_id,
                confirm_prepared,
                json,
            );
        }
    };
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

/// 仅调试构建：按环境变量在指定收尾步骤中止，用于真实中断注入。
/// 发布构建没有中断点，也不会读取该变量。
#[cfg(debug_assertions)]
fn interrupt_recovery(point: &str) {
    if std::env::var("ZIYUAN_INTERRUPT_RECOVERY").ok().as_deref() == Some(point) {
        std::process::abort();
    }
}

#[cfg(not(debug_assertions))]
fn interrupt_recovery(_point: &str) {}

/// 第二版记录的代次阶段：本笔奇数代次时可推进；已到本笔偶数后代次时只清理终态记录。
#[derive(Clone, Copy, PartialEq, Eq)]
enum SecondGeneration {
    Reserved,
    Finished,
}

/// 先核代次再动盘：当前代次只能是记录的前代次加一，或（仅终态记录）后代次。
fn second_generation(
    workspace: &Workspace,
    record: &ziyuan_storage::recovery::RecoveryRecordV2,
    terminal: RecordState,
) -> Result<SecondGeneration, Box<dyn Error>> {
    let current = workspace.generation()?;
    let reserved = record.generation_before + 1;
    if current == reserved {
        return Ok(SecondGeneration::Reserved);
    }
    if current == record.generation_after && record.state == terminal {
        return Ok(SecondGeneration::Finished);
    }
    Err(format!(
        "当前代次 {current} 与恢复记录不符：操作 {} 只能在代次 {reserved} 收尾，或在已{}后的代次 {} 只清理记录。\n\
         请保留现场，运行 ziyuan text recovery status 核对后联系维护者；勿手工改代次或删记录。",
        record.operation_id,
        state_name(terminal).trim_start_matches('已'),
        record.generation_after
    )
    .into())
}

/// 推进到记录的后代次；已在后代次时不再推进。
fn finish_second_generation(
    guard: &mut ziyuan_storage::WriteGuard,
    record: &ziyuan_storage::recovery::RecoveryRecordV2,
    phase: SecondGeneration,
) -> Result<u64, Box<dyn Error>> {
    if phase == SecondGeneration::Finished {
        return Ok(record.generation_after);
    }
    let generation = guard.finish()?;
    if generation != record.generation_after {
        return Err(format!(
            "代次推进到 {generation}，与记录的后代次 {} 不符。\n\
             请保留现场，运行 ziyuan text recovery status 核对后联系维护者；勿手工改代次或删记录。",
            record.generation_after
        )
        .into());
    }
    Ok(generation)
}

/// `--confirm-prepared` 只用于第二版的发布前中断；用在别的记录上是用法错误，不静默忽略。
fn confirm_prepared_misused(action: &str, operation_id: &str, what: &str) -> Box<dyn Error> {
    Box::new(crate::Usage(
        format!(
            "--confirm-prepared 只用于第二版恢复记录的发布前中断（已准备）；这份是{what}，未做任何改动。\n\
             请去掉该参数后重试 ziyuan text recovery {action} {operation_id} --yes。"
        )
        .into(),
    ))
}

const KEEP_SCENE: &str = "请保留现场，运行 ziyuan text recovery status 核对后联系维护者；勿截断事件簿、勿手工改代次或删记录。";

/// 第二版记录收尾：事件批在固定位置时只收尾；目标已写而批次未追加时按记录补记。
/// 不改正文、不重做业务；其他现场一律保留。
fn finish_second_format(
    workspace: &Workspace,
    guard: &mut ziyuan_storage::WriteGuard,
    record: &ziyuan_storage::recovery::RecoveryRecordV2,
    operation_id: &str,
    confirm_prepared: bool,
    json: bool,
) -> Result<(), Box<dyn Error>> {
    let human = match &record.application {
        ApplicationV2::Human(app) => Some(app),
        ApplicationV2::Model(_) => None,
    };
    if record.operation_id != operation_id {
        return Err(format!(
            "操作 ID 不符：磁盘上是 {}，不是 {operation_id}",
            record.operation_id
        )
        .into());
    }
    if confirm_prepared && record.state != RecordState::Prepared {
        let what = format!("状态为 {} 的第二版记录", state_name(record.state));
        return Err(confirm_prepared_misused("finish", operation_id, &what));
    }
    if !matches!(
        record.state,
        RecordState::Prepared | RecordState::Published | RecordState::Completed
    ) {
        return Err(format!(
            "恢复记录状态为 {}，finish 只处理已准备、已发布或已完成的记录。\n\
             已放弃的记录请改用 ziyuan text recovery abandon {operation_id} --yes 清理；其他情况{KEEP_SCENE}",
            state_name(record.state)
        )
        .into());
    }
    if record.state == RecordState::Prepared && !confirm_prepared {
        return Err(format!(
            "恢复记录是“已准备”（发布前中断），本版默认不动。\n\
             请先运行 ziyuan text recovery status 核对目标与事件现场、确认旧进程已停写，\
             再用 ziyuan text recovery finish {operation_id} --yes --confirm-prepared 继续，\
             或 ziyuan text recovery abandon {operation_id} --yes --confirm-prepared 放弃。"
        )
        .into());
    }
    let phase = second_generation(workspace, record, RecordState::Completed)?;
    let window = read_record_window(workspace, record)?;
    let evidence = classify_scene(record, &window)?;
    let targets_after = |workspace: &Workspace| match human {
        Some(app) => targets_match(workspace, app, &record.targets, TargetSide::After),
        None => generic_targets_match(workspace, &record.targets, TargetSide::After),
    };
    let after_ok = targets_after(workspace)?;
    // 人工准奏的补记等于替用户把准奏入簿：须在锁内重做准奏时的核对，不能只凭记录。
    let recheck_human = |guard: &ziyuan_storage::WriteGuard| -> Result<(), Box<dyn Error>> {
        if human.is_none() {
            return Ok(());
        }
        Translations::new(workspace.root())
            .recheck_human_recovery(guard, record)
            .map_err(|e| {
                format!(
                    "补记前复核未通过：{e}。本次未改动任何文件或事件簿。\n\
                     若 zh.json 仍可还原为旧内容，可还原后用 ziyuan text recovery abandon {operation_id} --yes 放弃；\
                     否则{KEEP_SCENE}"
                )
                .into()
            })
    };
    interrupt_recovery("before-events");
    match (record.state, evidence) {
        (RecordState::Completed, SceneEvidence::ExactAtSlot)
        | (RecordState::Published, SceneEvidence::ExactAtSlot) => {}
        (RecordState::Completed, other) => {
            return Err(format!(
                "已完成记录的事件簿现场为“{}”，与记录不符。\n{KEEP_SCENE}",
                scene_name(other)
            )
            .into());
        }
        (RecordState::Published, SceneEvidence::CleanAbsent) => {
            if !after_ok {
                return Err(format!(
                    "事件簿里没有这批事件，目标也不是记录中的新内容：这次写入没有完成。\n\
                     若目标全为旧内容，可用 ziyuan text recovery abandon {operation_id} --yes 放弃；\
                     不要用 finish 把不确定的状态盖过去。"
                )
                .into());
            }
            recheck_human(guard)?;
            append_recorded_batch(workspace, record, || Ok(()))?;
        }
        (RecordState::Published, other) => {
            return Err(format!(
                "事件簿现场为“{}”，无法确认批次是否已完整追加。\n{KEEP_SCENE}",
                scene_name(other)
            )
            .into());
        }
        (RecordState::Prepared, SceneEvidence::CleanAbsent) => {
            if !after_ok {
                return Err(format!(
                    "发布前中断：目标不是记录中的新内容，这次写入没有完成。\n\
                     若目标全为旧内容，可用 ziyuan text recovery abandon {operation_id} --yes --confirm-prepared 放弃；\
                     否则{KEEP_SCENE}"
                )
                .into());
            }
            recheck_human(guard)?;
            // 先在事件簿锁内核对条数与前缀，通过后才把记录推进为已发布，再追加；
            // 核对失败时记录保持“已准备”，不留下“拒绝却改盘”的现场。
            append_recorded_batch(workspace, record, || {
                guard.publish_prepared_second_format(operation_id)?;
                Ok(())
            })?;
        }
        (RecordState::Prepared, other) => {
            return Err(format!(
                "发布前中断的事件簿现场为“{}”，不是“批次未追加”：状态与事件矛盾或位置不明。\n{KEEP_SCENE}",
                scene_name(other)
            )
            .into());
        }
        _ => unreachable!("状态已在前面筛过"),
    }
    interrupt_recovery("after-events");
    if !after_ok {
        return Err(format!("目标仍不是记录中的新内容，不能收尾。\n{KEEP_SCENE}").into());
    }
    if record.state != RecordState::Completed {
        // 锁只挡其他紫垣进程，挡不住外部编辑器：标记完成前再核一遍目标。
        if !targets_after(workspace)? {
            return Err(format!(
                "目标在核对之后又被改动，收尾已中止；事件簿已有本批记录。\n{KEEP_SCENE}"
            )
            .into());
        }
        guard.complete_second_format(operation_id)?;
    }
    let generation = finish_second_generation(guard, record, phase)?;
    interrupt_recovery("after-finish");
    guard.remove_second_format()?;
    match json {
        true => print_json(serde_json::json!({
            "operation_id": operation_id,
            "generation": generation,
        }))?,
        false => println!("已收尾操作 {operation_id}：世代号推进到 {generation}，恢复记录已清理。"),
    }
    Ok(())
}

/// 第二版记录放弃：只有批次未追加且目标全是旧内容时才允许。
fn abandon_second_format(
    workspace: &Workspace,
    guard: &mut ziyuan_storage::WriteGuard,
    record: &ziyuan_storage::recovery::RecoveryRecordV2,
    operation_id: &str,
    confirm_prepared: bool,
    json: bool,
) -> Result<(), Box<dyn Error>> {
    let human = match &record.application {
        ApplicationV2::Human(app) => Some(app),
        ApplicationV2::Model(_) => None,
    };
    if record.operation_id != operation_id {
        return Err(format!(
            "操作 ID 不符：磁盘上是 {}，不是 {operation_id}",
            record.operation_id
        )
        .into());
    }
    if confirm_prepared && record.state != RecordState::Prepared {
        let what = format!("状态为 {} 的第二版记录", state_name(record.state));
        return Err(confirm_prepared_misused("abandon", operation_id, &what));
    }
    if !matches!(
        record.state,
        RecordState::Prepared | RecordState::Published | RecordState::Abandoned
    ) {
        return Err(format!(
            "恢复记录状态为 {}，abandon 只处理已准备、已发布或已放弃的记录。\n\
             已完成的记录请改用 ziyuan text recovery finish {operation_id} --yes 收尾；其他情况{KEEP_SCENE}",
            state_name(record.state)
        )
        .into());
    }
    if record.state == RecordState::Prepared && !confirm_prepared {
        return Err(format!(
            "恢复记录是“已准备”（发布前中断），本版默认不动。\n\
             请先运行 ziyuan text recovery status 核对、确认旧进程已停写，\
             再用 ziyuan text recovery abandon {operation_id} --yes --confirm-prepared 放弃。"
        )
        .into());
    }
    let phase = second_generation(workspace, record, RecordState::Abandoned)?;
    let window = read_record_window(workspace, record)?;
    let evidence = classify_scene(record, &window)?;
    if evidence != SceneEvidence::CleanAbsent {
        return Err(format!(
            "事件簿现场为“{}”，不能当作“什么都没发生”放弃。\n\
             请保留现场，改用 ziyuan text recovery finish {operation_id} --yes 核对；勿截断事件簿或手工改代次。",
            scene_name(evidence)
        )
        .into());
    }
    let targets_before = |workspace: &Workspace| match human {
        Some(app) => targets_match(workspace, app, &record.targets, TargetSide::Before),
        None => generic_targets_match(workspace, &record.targets, TargetSide::Before),
    };
    if !targets_before(workspace)? {
        return Err(format!(
            "目标已不是记录中的旧内容，可能已经落盘：不能放弃。\n\
             请改用 ziyuan text recovery finish {operation_id} --yes 核对。"
        )
        .into());
    }
    if record.state != RecordState::Abandoned {
        // 放弃之前再核一遍目标：锁只挡其他紫垣进程，挡不住外部编辑器。
        if !targets_before(workspace)? {
            return Err(format!(
                "目标在核对之后被改动，放弃已中止。\n\
                 请改用 ziyuan text recovery finish {operation_id} --yes 重新核对。"
            )
            .into());
        }
        if record.state == RecordState::Prepared {
            guard.abandon_prepared_second_format(operation_id)?;
        } else {
            guard.abandon_published_second_format(operation_id)?;
        }
    }
    let generation = finish_second_generation(guard, record, phase)?;
    guard.remove_second_format()?;
    match json {
        true => print_json(serde_json::json!({
            "operation_id": operation_id,
            "generation": generation,
        }))?,
        false => println!("已放弃操作 {operation_id}：世代号推进到 {generation}，恢复记录已清理。"),
    }
    Ok(())
}

/// 目标已写好但事件未追加：按记录固定的批次补记。条数与前缀在事件簿锁内核对，
/// 通过后才调用 `ready`（可在此改记录状态），再追加。
fn append_recorded_batch(
    workspace: &Workspace,
    record: &ziyuan_storage::recovery::RecoveryRecordV2,
    ready: impl FnOnce() -> Result<(), Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    let batch_len = record.events.encoded_events.len();
    let prefix = record
        .events
        .expected_events
        .checked_sub(batch_len as u64)
        .ok_or_else(|| format!("恢复记录的事件条数不足以容纳批次。\n{KEEP_SCENE}"))?;
    let prefix =
        usize::try_from(prefix).map_err(|_| format!("恢复记录的前缀条数过大。\n{KEEP_SCENE}"))?;
    let mut log = ziyuan_storage::EventLog::new(workspace.root());
    let history = ziyuan_storage::EventStore::load(&log)?;
    if history.len() != prefix {
        return Err(format!("事件簿在核对之后发生变化，已中止。\n{KEEP_SCENE}").into());
    }
    let events = record
        .events
        .encoded_events
        .iter()
        .map(|line| ziyuan_storage::decode_event_line(line))
        .collect::<Result<Vec<_>, String>>()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    log.append_if_unchanged_then(&history, &events, ready)??;
    Ok(())
}
/// 第二版 status 的下一步按状态区分，并写出真实操作 ID 与完整命令。
fn second_format_next(record: &ziyuan_storage::recovery::RecoveryRecordV2) -> String {
    let op = &record.operation_id;
    let keep = "不要手工改代次或删记录";
    match record.state {
        RecordState::Prepared => format!(
            "第二版记录发布前中断：先核对目标与事件现场、确认旧进程已停写，再用 ziyuan text recovery finish {op} --yes --confirm-prepared 继续，或 ziyuan text recovery abandon {op} --yes --confirm-prepared 放弃；{keep}"
        ),
        RecordState::Published => format!(
            "第二版记录：核对目标与事件现场后用 ziyuan text recovery finish {op} --yes 收尾；目标仍全为旧内容且事件未追加时可用 ziyuan text recovery abandon {op} --yes 放弃；{keep}"
        ),
        RecordState::Completed => format!(
            "第二版记录已完成：用 ziyuan text recovery finish {op} --yes 核对事件后推进世代并清理记录；{keep}"
        ),
        RecordState::Abandoned => format!(
            "第二版记录已放弃：用 ziyuan text recovery abandon {op} --yes 核对目标与事件后推进世代并清理记录；{keep}"
        ),
    }
}

fn read_record_window(
    workspace: &Workspace,
    record: &ziyuan_storage::recovery::RecoveryRecordV2,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let batch_len = record
        .events
        .encoded_events
        .iter()
        .try_fold(0usize, |sum, line| sum.checked_add(line.len() + 1))
        .ok_or_else(|| format!("恢复记录的事件批过长，暂不支持核对。\n{KEEP_SCENE}"))?;
    let limit = usize::try_from(record.events.expected_bytes)
        .ok()
        .and_then(|prefix| prefix.checked_add(batch_len + 1))
        .ok_or_else(|| format!("恢复记录的前缀过长，暂不支持核对。\n{KEEP_SCENE}"))?;
    read_log_window(workspace.root(), limit)
}

/// 第二版 model 记录没有逻辑输出：空目标视为无物理文件，非空逐个核对。
fn generic_targets_match(
    workspace: &Workspace,
    targets: &[ziyuan_storage::recovery::TargetState],
    side: TargetSide,
) -> Result<bool, Box<dyn Error>> {
    for target in targets {
        let expected = match side {
            TargetSide::Before => &target.before,
            TargetSide::After => &target.after,
        };
        if current_state(workspace.root(), &target.path)? != *expected {
            return Ok(false);
        }
    }
    Ok(true)
}

#[derive(Clone, Copy)]
enum TargetSide {
    Before,
    After,
}

/// 目标当前是否全为期望的一侧；零目标时核对逻辑输出。
fn targets_match(
    workspace: &Workspace,
    app: &ziyuan_storage::recovery::HumanApplication,
    targets: &[ziyuan_storage::recovery::TargetState],
    side: TargetSide,
) -> Result<bool, Box<dyn Error>> {
    use ziyuan_storage::recovery::FileState;
    if targets.is_empty() {
        let want = match side {
            TargetSide::Before => match &app.before {
                Some(hash) => FileState::Present(hash.clone()),
                None => FileState::Absent,
            },
            TargetSide::After => FileState::Present(app.after.clone()),
        };
        return Ok(current_state(workspace.root(), &app.logical_path)? == want);
    }
    for target in targets {
        let expected = match side {
            TargetSide::Before => &target.before,
            TargetSide::After => &target.after,
        };
        if current_state(workspace.root(), &target.path)? != *expected {
            return Ok(false);
        }
    }
    Ok(true)
}

fn read_log_window(root: &std::path::Path, limit: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    const MAX_SCENE_BYTES: usize = 1 << 30;
    if limit > MAX_SCENE_BYTES {
        return Err("恢复记录的前缀超过 1 GiB，暂不支持本版收尾；请保留现场。".into());
    }
    let path = root.join(".ziyuan/court.log");
    match std::fs::File::open(&path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take(limit as u64).read_to_end(&mut bytes)?;
            Ok(bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("读不了事件簿 {}：{error}", path.display()).into()),
    }
}

fn scene_name(evidence: SceneEvidence) -> &'static str {
    match evidence {
        SceneEvidence::CleanAbsent => "批次未追加",
        SceneEvidence::ExactAtSlot => "批次在固定位置",
        SceneEvidence::Partial => "批次只写了一半",
        SceneEvidence::Diverged => "前缀或位置不符",
    }
}

fn even_odd(generation: u64) -> &'static str {
    match generation % 2 {
        0 => "无进行中的受管写入",
        _ => "上次受管写入未完成",
    }
}

fn state_code(state: RecordState) -> &'static str {
    match state {
        RecordState::Prepared => "prepared",
        RecordState::Published => "published",
        RecordState::Completed => "completed",
        RecordState::Abandoned => "abandoned",
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

#[cfg(test)]
mod tests {
    use super::*;
    use ziyuan_runtime::tools::temp_cleanup::{Problem, sweep_workspace};

    #[test]
    fn problem_line_scrubs_the_workspace_root() {
        let root = env::temp_dir().join(format!("ziyuan-cli-problem-line-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let report = sweep_workspace(&root, &std::collections::BTreeSet::new(), false);
        // `Report` 没有公开构造器，但 `problems` 是公开字段：直接造一条带扫描根的消息，
        // 就能回答“文本模式会不会把绝对路径打出去”，而不必等某次 fs 错误恰好带上路径。
        let problem = Problem {
            path: root.join("leftover.tmp"),
            message: format!("删除失败：{}", root.display()),
        };
        let line = problem_line(&report, &problem);
        assert!(
            !line.contains(&root.display().to_string()),
            "绝对根漏进文本报告: {line}"
        );
        assert!(line.contains("leftover.tmp"), "{line}");
        assert!(line.contains("删除失败：."), "{line}");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// 译书扩展子命令曾漏接路由，`ziyuan text version` 等会报「未知子命令」。
    #[test]
    fn translation_subcommands_are_routed() {
        for args in [
            vec!["version", "list", "p", "b"],
            vec!["termbase", "export", "p", "f"],
            vec!["termbase", "export-tbx", "p", "f"],
            vec!["tm", "list", "p"],
            vec!["comment", "list", "p"],
            vec!["concordance", "p", "q"],
            vec!["qa", "p", "b"],
            vec!["compare", "p", "b", "f"],
        ] {
            let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
            match parse(args) {
                Ok(TextAction::Translation(_)) => {}
                _ => panic!("译书子命令未路由"),
            }
        }
    }

    #[test]
    fn export_and_continuity_help_match_the_commands() {
        assert!(TEXT_HELP.contains("--format txt|md|html"));
        assert!(TEXT_HELP.contains("TXT/MD/HTML"));
        let help = crate::continuity_commands::HELP;
        for needle in [
            "text search",
            "text draft-replace",
            "text tmx export",
            "text tmx preview",
            "text tmx import",
        ] {
            assert!(help.contains(needle), "帮助缺少 {needle}");
        }
        for args in [
            vec!["search", "p", "片段"],
            vec!["draft-replace", "p", "request.json"],
            vec!["tmx", "export", "p"],
            vec!["tmx", "preview", "p", "memory.tmx"],
            vec!["tmx", "import", "p", "memory.tmx", "--confirm", "fp"],
        ] {
            let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
            match parse(args) {
                Ok(TextAction::Continuity(_)) => {}
                _ => panic!("连续翻译子命令未路由"),
            }
        }
    }
}
