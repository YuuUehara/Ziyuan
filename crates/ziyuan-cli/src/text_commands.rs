//! `ziyuan text`：离线文本项目与译书命令分派。不读模型配置、不联网。

mod parse;
mod recovery;

pub(crate) use parse::parse;

use std::env;
use std::error::Error;
use std::io::Read;
use std::path::PathBuf;
use ziyuan_app::Session;
use ziyuan_app::text_project::{
    ImportOptions, ImportPreview, ImportSource, NewProject, Project, SourceFormat, TextProjects,
    ValidationReport,
};
use ziyuan_app::translation::{DraftSave, ExportOptions, Translations};
use ziyuan_storage::EventLog;

use recovery::run_recovery;

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
  ziyuan text init --title <标题> --language <ja|en|ko|fr|de|ru|es|it> --source <来源> --rights <权利声明>
                   [--author <作者>] [--version <版本>] [--date <取得日期>] [--scope <使用范围>]
                                       建立项目（译入 zh-CN），打印项目 ID；来源与权利为用户自述，不作核验
  ziyuan text import <项目ID> <章节.txt>...
                                       按给出的顺序一次性导入 TXT（支持 UTF-8 及受支持旧编码：JIS／EUC-KR／Windows-1252；每项目只导入一次）
  ziyuan text preview <项目ID> <文件>... [--ruby] [--heading-level <1|2>]
                     [--text-paragraphs <blank_lines|lines>] [--split-long-paragraphs]
                     [--chapter-title <章名>]...
                                       只读预览 TXT/MD，显示章序、原文字节范围、段落、提示块及警告和指纹
  ziyuan text import <项目ID> <文件>... [--ruby] [--heading-level <1|2>]
                    [--text-paragraphs <blank_lines|lines>] [--split-long-paragraphs]
                    [--chapter-title <章名>]... --confirm <预览指纹>
                                       MD（.md/.markdown）、ruby 或新解析选项须先 preview 再确认；
                                       TXT 使用新解析/BOM 处理也须 preview + --confirm；不加选项仍走旧 TXT 导入
                                       重复 --chapter-title 按章序覆盖全部章名；确认时须保持文件顺序与选项不变
  ziyuan text show <项目ID>            核对原文副本后列出章节、段落与提示块
  ziyuan text usage <项目ID>           只读汇总四阶段用量记录、状态与分币种估价；需已有朝廷，不调用模型
                                       缺失用量为未知；用量记录和工具调用不是 HTTP 请求数，估价不是账单
                                       未设本件预算不等于免费，也不取消朝廷或官员限额
  ziyuan text export-finished <项目ID> [--chapter <章ID>]... [--format txt|md|html]
                            [--layout chapters|book] [--bilingual] [--output <new.tar>]
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
            let format = match parsing.format {
                SourceFormat::Text => "TXT",
                SourceFormat::Markdown => "Markdown",
            };
            println!(
                "    标题：{} · 格式：{format} · 原文字节范围：{}..{}",
                parsing.title, parsing.byte_start, parsing.byte_end
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

pub(super) fn print_json(value: serde_json::Value) -> Result<(), Box<dyn Error>> {
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

#[cfg(test)]
mod tests {
    use super::recovery::problem_line;
    use super::*;
    use ziyuan_runtime::tools::temp_cleanup::{Problem, sweep_workspace};

    #[test]
    fn problem_line_scrubs_the_workspace_root() {
        let root = env::temp_dir().join(format!("ziyuan-cli-problem-line-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let report = sweep_workspace(&root, &std::collections::BTreeSet::new(), false);
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
