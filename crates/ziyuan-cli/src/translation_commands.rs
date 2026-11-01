//! 翰林院译书命令；下诏只入队，执行与准奏沿用 run/apply。
use crate::note_commands::print_json;
use std::error::Error;
use std::io::Read;
use std::path::PathBuf;
use ziyuan_app::Session;
use ziyuan_app::translation::{
    BlockState, BlockStatus, ReferencePreview, ReferenceRequest, ReferenceSubmission,
    ResumeOptions, Translations,
};
use ziyuan_core::{TaskId, TranslationKind};
use ziyuan_storage::EventLog;

pub(crate) const TRANSLATION_HELP: &str = r#"译书（均支持 --json）：
  ziyuan text translate <项目ID> <块ID> [--dept works]    导出材料并下工部初译诏令
  ziyuan text review <项目ID> <块ID>                    下两件礼部只读审校诏令（忠实度、文风）
  ziyuan text revise <项目ID> <块ID>                    按朱批优先下工部统稿诏令
  ziyuan text references preview <项目ID> <块ID> --kind translate|revise --selection <选择.json>
                                                    只读预览完整提示、字节数与确认指纹
  ziyuan text references show <诏令ID>                  查看下诏时的精确快照及当前/失效状态
  ziyuan text translate|revise <项目ID> <块ID> --references <选择.json>
                       --confirm <预览指纹> --submission <提交ID>
                                                    三项须同时给出；提交ID为 1..128 位 ASCII 字母数字、-、_
    选择文件是 ReferenceRequest JSON（selections 身份/摘要/来源，不接受任意参考正文）；
    authorized_projects 默认空仅当前项目，跨项目须在文件中逐个显式授权。
    警告：完整提示含原文与所选参考，执行时将发送给外部模型；预览不发送，下诏仅入队。
  ziyuan text zhupi <项目ID> <块ID> <段号> <意见>         写逐段朱批
  ziyuan text zhupi <项目ID> <块ID> <段号> --clear|--show 清除或查看该段朱批
  ziyuan text zhupi <项目ID> <块ID> [--show]             查看全块朱批
  ziyuan text finalize <项目ID> <块ID>                  御览定稿
  ziyuan text reopen <项目ID> <块ID> --reason <理由>     退回重修
  ziyuan text context approve <项目ID>                 校验并批准 context/ 草稿为不可变快照
  ziyuan text status <项目ID>                          列出各块状态、失效原因与对账提示
  ziyuan text resume <项目ID> [--yes]                   为未开始的块下初译；--yes 同意重试失败/撤销
    朱批与上下文批准不需要朝廷；其他译书命令需要 ziyuan init。
    下诏仅入队，不调用模型；用 ziyuan run 执行，ziyuan apply 准奏。
    草稿在 translations/<项目ID>/context/；proposals/ 不自动生效。
    续做跳过执行中、待准奏、有效已完成块；版本失效或不确定须先对账，不覆盖人修译稿。"#;

pub(crate) enum TranslationAction {
    Translate(String, String),
    ReferencesPreview(String, String, TranslationKind, PathBuf),
    ReferencesShow(TaskId),
    WithReferences {
        project: String,
        block: String,
        kind: TranslationKind,
        selection: PathBuf,
        fingerprint: String,
        submission: String,
    },
    Review(String, String),
    Revise(String, String),
    Finalize(String, String),
    Reopen(String, String, String),
    ContextApprove(String),
    Status(String),
    Resume(String, bool),
    Zhupi {
        project: String,
        block: String,
        paragraph: Option<String>,
        change: Option<Option<String>>,
    },
}

pub(crate) fn parse(args: &[String]) -> Result<TranslationAction, Box<dyn Error>> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let usage = "用法: ziyuan text translate|review|revise|references|zhupi|finalize|reopen|context|status|resume；运行 ziyuan text help 查看参数";
    // 位置参数不接受选项，避免误把缺失的参数当成项目或块。
    let positional = |s: &str| !s.starts_with("--") && !s.trim().is_empty();
    Ok(match a.as_slice() {
        ["references", "show", task] => {
            TranslationAction::ReferencesShow(TaskId(task.parse().map_err(|_| usage)?))
        }
        ["references", "preview", p, b, rest @ ..] if positional(p) && positional(b) => {
            let flags = reference_flags(rest, &["--kind", "--selection"], usage)?;
            TranslationAction::ReferencesPreview(
                (*p).into(),
                (*b).into(),
                reference_kind(flags["--kind"], usage)?,
                flags["--selection"].into(),
            )
        }
        [kind @ ("translate" | "revise"), p, b, rest @ ..]
            if positional(p)
                && positional(b)
                && rest
                    .iter()
                    .any(|v| matches!(*v, "--references" | "--confirm" | "--submission")) =>
        {
            let allowed: &[&str] = if *kind == "translate" {
                &["--references", "--confirm", "--submission", "--dept"]
            } else {
                &["--references", "--confirm", "--submission"]
            };
            let flags = reference_flags(rest, allowed, usage)?;
            if flags.get("--dept").is_some_and(|v| *v != "works") {
                return Err(usage.into());
            }
            let submission = flags.get("--submission").ok_or(usage)?;
            if submission.len() > 128
                || !submission
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            {
                return Err(usage.into());
            }
            TranslationAction::WithReferences {
                project: (*p).into(),
                block: (*b).into(),
                kind: reference_kind(kind, usage)?,
                selection: (*flags.get("--references").ok_or(usage)?).into(),
                fingerprint: (*flags.get("--confirm").ok_or(usage)?).into(),
                submission: (*submission).into(),
            }
        }
        ["translate", p, b] | ["translate", p, b, "--dept", "works"]
            if positional(p) && positional(b) =>
        {
            TranslationAction::Translate((*p).into(), (*b).into())
        }
        ["review", p, b] if positional(p) && positional(b) => {
            TranslationAction::Review((*p).into(), (*b).into())
        }
        ["revise", p, b] if positional(p) && positional(b) => {
            TranslationAction::Revise((*p).into(), (*b).into())
        }
        ["finalize", p, b] if positional(p) && positional(b) => {
            TranslationAction::Finalize((*p).into(), (*b).into())
        }
        ["reopen", p, b, "--reason", reason]
            if positional(p) && positional(b) && positional(reason) =>
        {
            TranslationAction::Reopen((*p).into(), (*b).into(), (*reason).into())
        }
        ["context", "approve", p] if positional(p) => {
            TranslationAction::ContextApprove((*p).into())
        }
        ["status", p] if positional(p) => TranslationAction::Status((*p).into()),
        ["resume", p] if positional(p) => TranslationAction::Resume((*p).into(), false),
        ["resume", p, "--yes"] if positional(p) => TranslationAction::Resume((*p).into(), true),
        ["zhupi", p, b, rest @ ..] if positional(p) && positional(b) => {
            let (paragraph, change) = match rest {
                [] | ["--show"] => (None, None),
                [para] | [para, "--show"] if positional(para) => (Some((*para).into()), None),
                [para, "--clear"] if positional(para) => (Some((*para).into()), Some(None)),
                [para, text] if positional(para) && positional(text) => {
                    (Some((*para).into()), Some(Some((*text).into())))
                }
                _ => return Err(usage.into()),
            };
            TranslationAction::Zhupi {
                project: (*p).into(),
                block: (*b).into(),
                paragraph,
                change,
            }
        }
        _ => return Err(usage.into()),
    })
}

fn reference_kind(value: &str, usage: &str) -> Result<TranslationKind, Box<dyn Error>> {
    match value {
        "translate" => Ok(TranslationKind::Translate),
        "revise" => Ok(TranslationKind::Revise),
        _ => Err(usage.into()),
    }
}

fn reference_flags<'a>(
    args: &[&'a str],
    allowed: &[&str],
    usage: &str,
) -> Result<std::collections::BTreeMap<&'a str, &'a str>, Box<dyn Error>> {
    let mut flags = std::collections::BTreeMap::new();
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let value = rest.next().ok_or(usage)?;
        if !allowed.contains(flag)
            || value.starts_with('-')
            || value.trim().is_empty()
            || flags.insert(*flag, *value).is_some()
        {
            return Err(usage.into());
        }
    }
    for flag in allowed.iter().filter(|f| **f != "--dept") {
        if !flags.contains_key(flag) {
            return Err(usage.into());
        }
    }
    Ok(flags)
}

fn read_reference_request(path: &std::path::Path) -> Result<ReferenceRequest, Box<dyn Error>> {
    const MAX_BYTES: u64 = 16 * 1024 * 1024;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("参考选择文件超过 16 MiB".into());
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| format!("参考选择文件 {} 无效：{e}", path.display()).into())
}

fn print_reference_preview(preview: &ReferencePreview) -> Result<(), Box<dyn Error>> {
    // Include all target/version/reference metadata, not only the rendered prompt.
    println!("{}", serde_json::to_string_pretty(preview)?);
    println!("完整提示：\n{}", preview.prompt);
    println!(
        "提示字节数：{}\n预览指纹：{}",
        preview.bytes, preview.fingerprint
    );
    println!(
        "警告：完整提示含原文与所选参考，执行时将发送给外部模型；预览不发送，下诏仅入队，不调用模型。"
    );
    Ok(())
}

pub(crate) fn run(action: TranslationAction, json: bool) -> Result<(), Box<dyn Error>> {
    let root = std::env::current_dir()?;
    let translations = Translations::new(&root);
    let action = match action {
        TranslationAction::ContextApprove(project) => {
            translations.ensure_context_draft(&project)?;
            let version = translations.approve_context(&project)?;
            if json {
                print_json(serde_json::json!({"project":project,"version":version}))?;
            } else {
                println!("项目 {project} 上下文已批准：v{version}。后续下诏使用此快照。");
            }
            return Ok(());
        }
        TranslationAction::Zhupi {
            project,
            block,
            paragraph,
            change,
        } => {
            let p = translations.project(&project)?;
            let prompt = p
                .chapters
                .iter()
                .flat_map(|c| &c.blocks)
                .find(|b| b.block_id == block)
                .ok_or("项目没有此块")?;
            if let Some(para) = &paragraph
                && !prompt.paragraphs.iter().any(|p| &p.id == para)
            {
                return Err("块没有此段号".into());
            }
            if let Some(change) = change {
                let para = paragraph.as_deref().expect("修改时已给段号");
                match change {
                    Some(text) => {
                        translations.set_zhupi(&project, &block, para, &text)?;
                    }
                    None => {
                        translations.clear_zhupi(&project, &block, para)?;
                    }
                }
            }
            let mut z = translations.zhupi(&project, &block)?;
            if let Some(para) = paragraph {
                z.items.retain(|id, _| id == &para);
            }
            if json {
                print_json(serde_json::to_value(&z)?)?;
            } else {
                println!("朱批 v{}（{} 条）：", z.version, z.items.len());
                for (para, note) in z.items {
                    println!("  {para}：{}", note.text);
                }
            }
            return Ok(());
        }
        action => action,
    };
    let mut session = Session::open(EventLog::new(&root))?;
    let result = run_session(&mut session, &root, action, json);
    crate::chronicle(&mut session);
    result
}

fn run_session(
    session: &mut Session<EventLog>,
    root: &std::path::Path,
    action: TranslationAction,
    json: bool,
) -> Result<(), Box<dyn Error>> {
    let (project, block, kind, edicts) = match action {
        TranslationAction::ReferencesPreview(project, block, kind, selection) => {
            let request = read_reference_request(&selection)?;
            let preview = session.preview_references(root, &project, &block, kind, &request)?;
            if json {
                print_json(serde_json::to_value(&preview)?)?;
            } else {
                print_reference_preview(&preview)?;
            }
            return Ok(());
        }
        TranslationAction::ReferencesShow(task) => {
            let view = session.reference_task(root, task)?;
            if json {
                print_json(serde_json::to_value(&view)?)?;
            } else {
                println!(
                    "诏令 #{}：{}",
                    view.task,
                    if view.current {
                        "当前有效 (current)"
                    } else {
                        "已失效 (stale)"
                    }
                );
                if let Some(reason) = &view.reason {
                    println!("原因：{reason}");
                }
                print_reference_preview(&view.snapshot)?;
            }
            return Ok(());
        }
        TranslationAction::WithReferences {
            project,
            block,
            kind,
            selection,
            fingerprint,
            submission,
        } => {
            let submission = ReferenceSubmission {
                request: read_reference_request(&selection)?,
                fingerprint,
                submission,
            };
            let edict = session.issue_with_references(root, &project, &block, kind, &submission)?;
            let kind = if kind == TranslationKind::Translate {
                "translate"
            } else {
                "revise"
            };
            (project, block, kind, vec![edict])
        }
        TranslationAction::Status(project) => {
            let statuses = session.translation_status(root, &project)?;
            if json {
                print_json(serde_json::to_value(&statuses)?)?;
            } else {
                print_statuses(&statuses);
            }
            return Ok(());
        }
        TranslationAction::Resume(project, yes) => {
            let statuses = session.translation_status(root, &project)?;
            let outcome = session.resume_translation(
                root,
                &project,
                ResumeOptions {
                    include_failed: yes,
                    include_stale: false,
                },
            )?;
            if json {
                print_json(
                    serde_json::json!({"issued":outcome.issued,"errors":outcome.errors,"statuses":statuses}),
                )?;
            } else {
                print_statuses(&statuses);
                if !yes {
                    println!("失败/撤销未重试；确认后可加 --yes。版本失效与不确定块须先对账。");
                }
                for (block, task) in &outcome.issued {
                    println!("{block}：初译诏令 #{task} 已入队；用 ziyuan run 执行。");
                }
                for (block, error) in &outcome.errors {
                    eprintln!("{block}：{error}");
                }
            }
            if !outcome.errors.is_empty() {
                return Err("部分块续做下诏失败".into());
            }
            return Ok(());
        }
        TranslationAction::Finalize(project, block) => {
            let hash = session.finalize_translation(root, &project, &block)?;
            if json {
                print_json(
                    serde_json::json!({"project":project,"block":block,"translation_hash":hash,"finalized":true}),
                )?;
            } else {
                println!("{project} / {block} 已御览定稿：{hash}");
            }
            return Ok(());
        }
        TranslationAction::Reopen(project, block, reason) => {
            session.reopen_translation(root, &project, &block, &reason)?;
            if json {
                print_json(
                    serde_json::json!({"project":project,"block":block,"reason":reason,"reopened":true}),
                )?;
            } else {
                println!("{project} / {block} 已退回重修：{reason}");
            }
            return Ok(());
        }
        TranslationAction::Translate(p, b) => {
            let edict = session.issue_translation(root, &p, &b)?;
            (p, b, "translate", vec![edict])
        }
        TranslationAction::Revise(p, b) => {
            let edict = session.issue_revise(root, &p, &b)?;
            (p, b, "revise", vec![edict])
        }
        TranslationAction::Review(p, b) => {
            let edicts = session.issue_review(root, &p, &b)?;
            (p, b, "review", edicts)
        }
        _ => unreachable!("离线操作已处理"),
    };
    let tasks: Vec<u64> = edicts.iter().map(|e| e.id.0).collect();
    if json {
        let mut value =
            serde_json::json!({"project":project,"block":block,"kind":kind,"tasks":tasks});
        if tasks.len() == 1 {
            value["task"] = tasks[0].into();
        }
        print_json(value)?;
    } else {
        for edict in edicts {
            println!(
                "诏令 #{} 已入队：{} / {}（{}）",
                edict.id.0,
                project,
                block,
                edict.department.label()
            );
        }
        println!("下一步：ziyuan run；译稿完成后仍须 ziyuan apply 准奏。");
    }
    Ok(())
}

fn print_statuses(statuses: &[BlockStatus]) {
    for s in statuses {
        println!(
            "{} / {}：{}，诏令 {:?}",
            s.chapter,
            s.block,
            s.state.label(),
            s.tasks
        );
        match &s.state {
            BlockState::Stale { reasons, .. } => {
                for reason in reasons {
                    println!("  {}", reason.describe());
                }
            }
            BlockState::Failed { reason, .. } | BlockState::Uncertain { reason } => {
                println!("  {reason}")
            }
            _ => {}
        }
        if let Some(note) = &s.note {
            println!("  {note}");
        }
    }
}
