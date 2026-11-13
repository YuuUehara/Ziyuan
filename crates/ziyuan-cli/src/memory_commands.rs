//! 翻译记忆：只在本机查已定稿的原译文，不调用聊天服务。
use std::collections::BTreeSet;
use std::error::Error;

pub(crate) const MEMORY_HELP: &str = r#"翻译记忆（均支持 --json）：
  ziyuan text memory status <项目ID> [--provider local|deterministic|remote]
  ziyuan text memory build <项目ID> [--project <其他项目ID>]... [--provider local|deterministic|remote]
                                                    从当前有效定稿重建索引，不改原文或译稿
  ziyuan text memory search <项目ID> --query <原文> [--project <其他项目ID>]... [--limit 5] [--provider ...]
                                                    查找参考译法，最多 50 条。只供参考，不自动写入译稿，也不自动批准
  ziyuan text memory authorize <项目ID> --endpoint <URL> --model <模型> --revision <修订> --dim <维度> [--project <其他项目ID>]... [--confirm <指纹>]
                                                    预览或确认远程嵌入。不确认就不写授权、不联网
    默认只查当前项目；其他项目必须逐个给出 --project，不支持 --all。
    搜索 JSON 的 hits[].selection 可放入选择文件的 selections 数组；不要复制参考正文。
    使用跨项目参考还须在选择文件 authorized_projects 中显式授权；搜索时允许的项目不代替下诏授权。
    默认不启用。local 与 deterministic 只在本机计算，都不理解意思。
    remote 还要设置 ZIYUAN_MEMORY_EMBED_URL、MODEL、REVISION、DIM；密钥是 ZIYUAN_MEMORY_EMBED_KEY。
    这些都不从 ZIYUAN_LLM_* 读取。没授权的项目不会把原文发出去。
    建库时发送已授权项目里已定稿段落的原文；每次查询只发送这一次的查询文字。译文留在本机。"#;

pub(crate) struct MemoryAction {
    pub command: String,
    pub project: String,
    pub projects: BTreeSet<String>,
    pub query: Option<String>,
    pub limit: usize,
    pub provider: Option<String>,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub revision: Option<String>,
    pub dim: Option<usize>,
    pub confirm: Option<String>,
}

pub(crate) fn parse(args: &[String]) -> Result<MemoryAction, Box<dyn Error>> {
    let usage = "用法: ziyuan text memory status|build|search <项目ID>；搜索须给 --query <原文>，用 text help 查看完整说明";
    let [command, project, rest @ ..] = args else {
        return Err(usage.into());
    };
    if !matches!(
        command.as_str(),
        "status" | "build" | "search" | "authorize"
    ) || project.trim().is_empty()
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
        provider: None,
        endpoint: None,
        model: None,
        revision: None,
        dim: None,
        confirm: None,
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
            "--provider" if command != "authorize" && action.provider.is_none() => {
                action.provider = Some(value.clone());
            }
            "--endpoint" if command == "authorize" && action.endpoint.is_none() => {
                action.endpoint = Some(value.clone());
            }
            "--model" if command == "authorize" && action.model.is_none() => {
                action.model = Some(value.clone());
            }
            "--revision" if command == "authorize" && action.revision.is_none() => {
                action.revision = Some(value.clone());
            }
            "--dim" if command == "authorize" && action.dim.is_none() => {
                let dim = value.parse::<usize>().map_err(|_| "--dim 须为正整数")?;
                if dim == 0 {
                    return Err("--dim 须为正整数".into());
                }
                action.dim = Some(dim);
            }
            "--confirm" if command == "authorize" && action.confirm.is_none() => {
                action.confirm = Some(value.clone());
            }
            _ => return Err(usage.into()),
        }
    }
    if command == "search" && action.query.is_none() {
        return Err(usage.into());
    }
    if command == "authorize"
        && (action.endpoint.is_none()
            || action.model.is_none()
            || action.revision.is_none()
            || action.dim.is_none())
    {
        return Err("授权须同时给出 --endpoint、--model、--revision 和 --dim".into());
    }
    Ok(action)
}

fn provider_for(
    action: &MemoryAction,
) -> Result<Box<dyn ziyuan_memory::EmbeddingProvider>, Box<dyn Error>> {
    if action.command == "authorize" {
        return Err("授权命令不装配嵌入提供方".into());
    }
    let selection = ziyuan_app::memory_provider::selection_from_env(action.provider.as_deref())?;
    Ok(ziyuan_app::memory_provider::open(
        &std::env::current_dir()?,
        &action.projects,
        selection,
    )?)
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
        "authorize" => {
            let endpoint = ziyuan_app::memory_provider::RemoteEndpoint {
                endpoint: action.endpoint.clone().unwrap_or_default(),
                model: action.model.clone().unwrap_or_default(),
                revision: action.revision.clone().unwrap_or_default(),
                dim: action.dim.unwrap_or(0),
                api_key: String::new(),
            };
            let preview = if let Some(confirm) = &action.confirm {
                ziyuan_app::memory_provider::store_remote_auth(
                    &root,
                    &endpoint,
                    &action.projects,
                    confirm,
                )?
            } else {
                ziyuan_app::memory_provider::preview_remote_auth(&endpoint, &action.projects)?
            };
            if !json {
                println!(
                    "{}\n指纹：{}\n确认前不会发送原文。",
                    preview.note, preview.fingerprint
                );
            }
            serde_json::to_value(preview)?
        }
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
            let provider = provider_for(&action)?;
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
            let provider = provider_for(&action)?;
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
        _ => unreachable!("解析只接收 status、build、search 和 authorize"),
    };
    if json {
        crate::note_commands::print_json(value)?;
    }
    Ok(())
}
