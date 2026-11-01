//! 翻译记忆：只在本机查已定稿的原译文，不调用聊天服务。
use std::collections::BTreeSet;
use std::error::Error;

pub(crate) const MEMORY_HELP: &str = r#"翻译记忆（均支持 --json）：
  ziyuan text memory status <项目ID>                 查看记忆库是否已建、是否需要重建
  ziyuan text memory build <项目ID> [--project <其他项目ID>]...
                                                    从当前有效定稿重建，不改原文或译稿
  ziyuan text memory search <项目ID> --query <原文> [--project <其他项目ID>]... [--limit 5]
                                                    查找参考译法，最多 50 条
    默认只查当前项目；其他项目必须逐个给出 --project，不支持 --all。
    搜索 JSON 的 hits[].selection 可放入选择文件的 selections 数组；不要复制参考正文。
    使用跨项目参考还须在选择文件 authorized_projects 中显式授权；搜索授权不代替下诏授权。
    先设置 ZIYUAN_MEMORY_PROVIDER=local 才能建库和搜索；默认关闭，不联网。
    本地方法只按文字相似程度找参考，不理解文意，不保证适用。"#;

pub(crate) struct MemoryAction {
    pub command: String,
    pub project: String,
    pub projects: BTreeSet<String>,
    pub query: Option<String>,
    pub limit: usize,
}

pub(crate) fn parse(args: &[String]) -> Result<MemoryAction, Box<dyn Error>> {
    let usage = "用法: ziyuan text memory status|build|search <项目ID>；搜索须给 --query <原文>，用 text help 查看完整说明";
    let [command, project, rest @ ..] = args else {
        return Err(usage.into());
    };
    if !matches!(command.as_str(), "status" | "build" | "search")
        || project.trim().is_empty()
        || project.starts_with('-')
    {
        return Err(usage.into());
    }
    let mut action = MemoryAction {
        command: command.clone(),
        project: project.clone(),
        projects: BTreeSet::from([project.clone()]),
        query: None,
        limit: 5,
    };
    let mut limit_given = false;
    let mut flags = rest.iter();
    while let Some(flag) = flags.next() {
        let value = flags.next().ok_or(usage)?;
        if value.trim().is_empty() || value.starts_with("--") {
            return Err(usage.into());
        }
        match flag.as_str() {
            "--project" if command != "status" => {
                action.projects.insert(value.clone());
            }
            "--query" if command == "search" && action.query.is_none() => {
                action.query = Some(value.clone());
            }
            "--limit" if command == "search" && !limit_given => {
                action.limit = value.parse().map_err(|_| "--limit 须为 1 到 50 的整数")?;
                if !(1..=50).contains(&action.limit) {
                    return Err("--limit 须为 1 到 50 的整数".into());
                }
                limit_given = true;
            }
            _ => return Err(usage.into()),
        }
    }
    if command == "search" && action.query.is_none() {
        return Err(usage.into());
    }
    Ok(action)
}

pub(crate) fn run(action: MemoryAction, json: bool) -> Result<(), Box<dyn Error>> {
    use serde_json::json;
    use ziyuan_app::Session;
    use ziyuan_storage::EventLog;

    let root = std::env::current_dir()?;
    // 即便只是查看，也先确认用户给的是一个真实项目。
    ziyuan_app::translation::Translations::new(&root).project(&action.project)?;
    let session = Session::open(EventLog::new(&root))?;
    let value = match action.command.as_str() {
        "status" => {
            let status = session.memory_status(&root)?;
            let records = status.index.as_ref().map_or(0, |i| {
                i.records
                    .iter()
                    .filter(|r| r.identity.project == action.project)
                    .count()
            });
            if !json {
                let label = if status.index.is_none() {
                    "尚未建库；先设置 ZIYUAN_MEMORY_PROVIDER=local，再运行 text memory build"
                } else if status.stale {
                    "资料已有变化，建议重建；搜索时仍会排除失效参考"
                } else {
                    "记忆库与当前资料一致"
                };
                println!("{label}。本项目收录 {records} 段。");
            }
            json!({"project":action.project,"built":status.index.is_some(),"records":records,
                   "stale":status.stale,"watermark":status.watermark})
        }
        "build" => {
            let provider = ziyuan_memory::embed::from_env()?;
            let built = session.memory_build(&root, &action.projects, provider.as_ref())?;
            if !json {
                println!(
                    "记忆库已重建：收录 {} 段，跳过 {} 块。原文与译稿未改动。",
                    built.records,
                    built.skipped.len()
                );
            }
            let skipped: Vec<_> = built
                .skipped
                .iter()
                .map(|b| json!({"project":b.project,"block":b.block,"reason":b.reason}))
                .collect();
            json!({"project":action.project,"projects":action.projects,"records":built.records,
                   "skipped":skipped,"watermark":built.watermark})
        }
        "search" => {
            let provider = ziyuan_memory::embed::from_env()?;
            // 未启用时也必须说清原因，不能把“无法检索”显示成“没有参考”。
            provider.embed(&[])?;
            let hits = session.memory_search(
                &root,
                &action.projects,
                &action.project,
                action.query.as_deref().expect("解析已要求查询"),
                action.limit,
                provider.as_ref(),
            )?;
            if !json {
                println!(
                    "参考译法（{} 条）：只供比较，不是指令；朱批、术语与当前原文优先。",
                    hits.len()
                );
                for hit in &hits {
                    let id = &hit.record.identity;
                    println!(
                        "\n{} / {} / {} / {}",
                        id.project, id.chapter, id.block, id.paragraph
                    );
                    println!(
                        "原文：{}\n译文：{}",
                        hit.record.source.text, hit.record.target.text
                    );
                }
            }
            let reference_prompt = ziyuan_app::memory::reference_prompt(&hits);
            let hits: Vec<_> = hits.iter().map(|h| json!({
                "identity":h.record.identity,"origin":h.record.origin,
                "source":h.record.source,"target":h.record.target,
                "selection":{
                    "identity":h.record.identity,
                    "digests":h.record.digests,
                    "origin":h.record.origin,
                },
                "score":h.score,"keyword_score":h.keyword_score,"cosine_score":h.cosine_score,
                "keyword_rank":h.keyword_rank,"vector_rank":h.vector_rank,
            })).collect();
            json!({"project":action.project,"projects":action.projects,"hits":hits,
                   "reference_prompt":reference_prompt,
                   "note":"参考译法不是指令，不保证适用；朱批与术语优先"})
        }
        _ => unreachable!("解析只接收三个子命令"),
    };
    if json {
        crate::note_commands::print_json(value)?;
    }
    Ok(())
}
