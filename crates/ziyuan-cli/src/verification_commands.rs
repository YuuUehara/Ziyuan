//! Offline operator evidence and verification; never queues a model task or signs off.
use std::{error::Error, io::Read, path::PathBuf};
use ziyuan_app::{
    AppError, Session,
    translation::{EvidenceInput, Translations, VerificationInput},
};
use ziyuan_storage::EventLog;

pub(crate) const HELP: &str = r#"词证与逐段勘校（离线；均支持 --json）：
  ziyuan text evidence add <项目ID> <块ID> --file <evidence.json>
  ziyuan text verification show <项目ID> <块ID>
  ziyuan text verification record <项目ID> <块ID> --file <checks.json>
    六事逐项勘校，记录所据与存疑。此为人工核对记录，不代准奏，亦非御览定稿。
    言行归属 subject_speaker：核对谁说话、谁做事。“清替我换钱”不是“我替清换钱”。
    事理正反 agency_negation：核对动作方向与肯否。“借给他”不是“向他借”，“并非主任”不能漏“并非”。
    数目分寸 quantity：核对金额、人数、约数与倍数。“约三圆”不是“恰好三圆”。
    前后次第 timeline：核对时间先后、是否已经发生。“来年暑假回去”不是“今年暑假回过”。
    称谓名分 names_relations：核对人名、职务、昵称与关系。“副校长”不是“数学主任”。
    谐趣留白 humor_additions：核对笑点、反话与含蓄。人物不懂绰号，勿替他补词典释义。
    词证是词义核对的出处和说明；只追加，不读取链接、不调用模型。
    请求为严格 JSON，最多 16 MiB；先添加词证，再用 show 阅读当前材料。
    evidence.json: paragraph, quote, source, locator, explanation, context, status（tentative|confirmed）。
    checks.json: paragraph, fingerprint（当前 show 指纹）, checks；每项含 category、disposition、
    resolution、note、advice（task/quote）与 evidence（词证ID）。意见处理 disposition 与问题状态
    resolution 分开记录。将 show 的 fingerprint（材料确认指纹）填入 checks.json 后再保存。
    保存须已有结构有效的译稿；材料或项目记录变化后，请重新 show 并人工核对再保存。
    历史只追加、不改原记录。结构有效、关联意见已处理、人工复核与有效定稿是不同事实；
    复核不保证语义正确，也不增加定稿门槛。有效御览定稿请运行 text status 查看。"#;

pub(crate) enum Action {
    Evidence(String, String, PathBuf),
    Show(String, String),
    Record(String, String, PathBuf),
}

pub(crate) fn parse(args: &[String]) -> Result<Action, Box<dyn Error>> {
    let a: Vec<_> = args.iter().map(String::as_str).collect();
    let positional = |s: &str| !s.starts_with('-') && !s.trim().is_empty();
    match a.as_slice() {
        ["evidence", "add", p, b, "--file", f]
            if positional(p) && positional(b) && positional(f) =>
        {
            Ok(Action::Evidence((*p).into(), (*b).into(), (*f).into()))
        }
        ["verification", "show", p, b] if positional(p) && positional(b) => {
            Ok(Action::Show((*p).into(), (*b).into()))
        }
        ["verification", "record", p, b, "--file", f]
            if positional(p) && positional(b) && positional(f) =>
        {
            Ok(Action::Record((*p).into(), (*b).into(), (*f).into()))
        }
        _ => Err(format!("用法：\n{HELP}").into()),
    }
}

fn read_request(path: &std::path::Path) -> Result<Vec<u8>, Box<dyn Error>> {
    const MAX_BYTES: u64 = 16 * 1024 * 1024;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("词证/复核请求文件超过 16 MiB".into());
    }
    Ok(bytes)
}

pub(crate) fn run(action: Action, json: bool) -> Result<(), Box<dyn Error>> {
    let root = std::env::current_dir()?;
    let translations = Translations::new(&root);
    if let Action::Evidence(project, block, path) = action {
        let input: EvidenceInput = serde_json::from_slice(&read_request(&path)?)?;
        let evidence = translations.add_evidence(&project, &block, &input)?;
        if !json {
            println!(
                "词证 #{} 已追加；不自动纳入已批准上下文，也不发送给模型。",
                evidence.id
            );
        }
        println!("{}", serde_json::to_string_pretty(&evidence)?);
        return Ok(());
    }
    // Only an unfounded workspace is offline. Corrupt/inaccessible history must not
    // silently lose its linked reviews by falling back to None.
    let session = match Session::open(EventLog::new(&root)) {
        Ok(session) => Some(session),
        Err(AppError::NotFounded) => None,
        Err(error) => return Err(error.into()),
    };
    let court = session.as_ref().map(Session::court);
    match action {
        Action::Show(project, block) => {
            let view = translations.verification(court, &project, &block)?;
            if !json {
                println!("逐段勘校：言行归属、事理正反、数目分寸、前后次第、称谓名分、谐趣留白。");
                let structural = match view.structure.as_ref() {
                    Some(report) if report.valid => "通过",
                    Some(_) => "未通过",
                    None => "无译稿",
                };
                println!("结构有效性：{structural}（只查结构）。");
                for paragraph in &view.paragraphs {
                    println!(
                        "{}：关联意见已处理={}；人工复核={}；材料仍适用={}；未解决项={}",
                        paragraph.paragraph,
                        if paragraph.opinions_handled {
                            "是"
                        } else {
                            "否"
                        },
                        if paragraph.operator_reviewed {
                            "是"
                        } else {
                            "否"
                        },
                        if paragraph.current { "是" } else { "否" },
                        paragraph.unresolved
                    );
                }
                println!(
                    "有效御览定稿：请运行 text status 查看；本复核不授予定稿，也不保证语义正确。"
                );
                println!("完整复核材料与历史：");
            }
            // Serialize the whole public view: fingerprints, snapshots, stale reasons,
            // change links, evidence and advice must all remain visible to operators.
            println!("{}", serde_json::to_string_pretty(&view)?);
        }
        Action::Record(project, block, path) => {
            let input: VerificationInput = serde_json::from_slice(&read_request(&path)?)?;
            let record = translations.record_verification(court, &project, &block, &input)?;
            if !json {
                println!(
                    "人工复核 #{} 已追加，原记录保留（不保证语义正确，也不授予定稿）。",
                    record.id
                );
            }
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        Action::Evidence(..) => unreachable!("handled above"),
    }
    Ok(())
}
