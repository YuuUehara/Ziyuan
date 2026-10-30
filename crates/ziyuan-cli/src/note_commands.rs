//! 御笔札记：独立记录，不开朝廷、不调用模型。
use std::collections::BTreeMap;
use std::error::Error;
use ziyuan_app::notes::{NewNote, Note, NoteEdit, NoteFilter, NoteKind, Notes};

pub(crate) const NOTE_HELP: &str = r#"御笔札记（不需要 ziyuan init；不自动交给模型）：
  ziyuan note add --source <原文> --zh <译法> [--kind word|passage] [--remark <备注>]
                  [--project <项目ID> [--block <块ID> [--para <段号>]]]
  ziyuan note list [--project <项目ID>] [--search <检索词>] [--kind word|passage]
  ziyuan note show <札记ID>
  ziyuan note edit <札记ID> [--source <原文>] [--zh <译法>] [--kind word|passage] [--remark <备注>]
  ziyuan note remove <札记ID>
  ziyuan note promote <札记ID> --project <项目ID>
    word 并入术语表草稿，passage 并入文体范例草稿；重复并入不重复写。
    仅写草稿，须再执行 ziyuan text context approve <项目ID> 才生效。
  以上命令均支持 --json；札记存于 .ziyuan/notes.json。"#;

pub(crate) enum NoteAction {
    Help,
    Add(NewNote),
    List(NoteFilter),
    Show(String),
    Edit(String, NoteEdit),
    Remove(String),
    Promote(String, String),
}

/// 严格的单值选项：拒绝未知项、缺值与重复项，空字符串留给应用层校验。
pub(crate) fn flags(
    args: &[String],
    allowed: &[&str],
) -> Result<BTreeMap<String, String>, Box<dyn Error>> {
    let mut values = BTreeMap::new();
    for pair in args.chunks(2) {
        let [flag, value] = pair else {
            return Err("选项缺少参数；请用 --help 查看用法".into());
        };
        if !allowed.contains(&flag.as_str()) || value.starts_with("--") {
            return Err(format!("未知选项或缺少参数：{flag}；请用 --help 查看用法").into());
        }
        if values.insert(flag.clone(), value.clone()).is_some() {
            return Err(format!("{flag} 只能给一次").into());
        }
    }
    Ok(values)
}

pub(crate) fn parse(args: Vec<String>) -> Result<NoteAction, Box<dyn Error>> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        return Ok(NoteAction::Help);
    }
    let (action, rest) = args.split_first().expect("非空");
    let usage =
        "用法: ziyuan note add|list|show|edit|remove|promote；运行 ziyuan note help 查看参数";
    let id_flags = || -> Result<(String, &[String]), Box<dyn Error>> {
        let (id, flags) = rest.split_first().ok_or(usage)?;
        if id.starts_with("--") {
            return Err(usage.into());
        }
        Ok((id.clone(), flags))
    };
    Ok(match action.as_str() {
        "help" => NoteAction::Help,
        "add" => {
            let mut f = flags(
                rest,
                &[
                    "--source",
                    "--zh",
                    "--kind",
                    "--remark",
                    "--project",
                    "--block",
                    "--para",
                ],
            )?;
            NoteAction::Add(NewNote {
                source: f.remove("--source").ok_or(usage)?,
                zh: f.remove("--zh").ok_or(usage)?,
                kind: f
                    .remove("--kind")
                    .map(|v| v.parse())
                    .transpose()?
                    .unwrap_or(NoteKind::Word),
                remark: f.remove("--remark").unwrap_or_default(),
                project: f.remove("--project"),
                block: f.remove("--block"),
                paragraph: f.remove("--para"),
            })
        }
        "list" => {
            let mut f = flags(rest, &["--project", "--kind", "--search"])?;
            NoteAction::List(NoteFilter {
                project: f.remove("--project"),
                kind: f.remove("--kind").map(|v| v.parse()).transpose()?,
                search: f.remove("--search"),
            })
        }
        "show" | "remove" => {
            let (id, rest) = id_flags()?;
            if !rest.is_empty() {
                return Err(usage.into());
            }
            if action == "show" {
                NoteAction::Show(id)
            } else {
                NoteAction::Remove(id)
            }
        }
        "edit" => {
            let (id, rest) = id_flags()?;
            let mut f = flags(rest, &["--source", "--zh", "--kind", "--remark"])?;
            if f.is_empty() {
                return Err(usage.into());
            }
            NoteAction::Edit(
                id,
                NoteEdit {
                    source: f.remove("--source"),
                    zh: f.remove("--zh"),
                    kind: f.remove("--kind").map(|v| v.parse()).transpose()?,
                    remark: f.remove("--remark"),
                },
            )
        }
        "promote" => {
            let (id, rest) = id_flags()?;
            let mut f = flags(rest, &["--project"])?;
            NoteAction::Promote(id, f.remove("--project").ok_or(usage)?)
        }
        other => {
            return Err(
                format!("未知的 note 子命令：{other}。运行 ziyuan note help 查看用法").into(),
            );
        }
    })
}

pub(crate) fn run(action: NoteAction, json: bool) -> Result<(), Box<dyn Error>> {
    let notes = Notes::new(std::env::current_dir()?);
    let note = match action {
        NoteAction::Help => {
            println!("{NOTE_HELP}");
            return Ok(());
        }
        NoteAction::Add(new) => notes.add(new)?,
        NoteAction::Show(id) => notes.get(&id)?,
        NoteAction::Edit(id, edit) => notes.edit(&id, edit)?,
        NoteAction::Remove(id) => {
            let note = notes.remove(&id)?;
            if !json {
                println!("已删除札记：{id}");
            }
            note
        }
        NoteAction::List(filter) => {
            let list = notes.list(&filter)?;
            if json {
                print_json(serde_json::to_value(&list)?)?;
            } else {
                println!("御笔札记（{} 条）：", list.len());
                for note in &list {
                    print_note(note);
                }
            }
            return Ok(());
        }
        NoteAction::Promote(id, project) => {
            let p = notes.promote(&id, &project)?;
            if json {
                print_json(
                    serde_json::json!({"note":p.note,"path":p.path,"added":p.added,"requires_context_approval":true}),
                )?;
            } else {
                println!(
                    "{}：{}",
                    if p.added {
                        "已并入草稿"
                    } else {
                        "草稿已含此札记，未重复写入"
                    },
                    p.path
                );
                println!("须再执行 ziyuan text context approve {project} 才生效。");
            }
            return Ok(());
        }
    };
    if json {
        print_json(serde_json::to_value(&note)?)?;
    } else {
        print_note(&note);
    }
    Ok(())
}

pub(crate) fn print_json(value: serde_json::Value) -> Result<(), Box<dyn Error>> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn print_note(note: &Note) {
    println!(
        "{} [{}] {} → {}",
        note.id,
        note.kind.label(),
        note.source,
        note.zh
    );
    if !note.remark.is_empty() {
        println!("  备注：{}", note.remark);
    }
    if let Some(project) = &note.project {
        println!(
            "  出处：{project} {} {}",
            note.block.as_deref().unwrap_or(""),
            note.paragraph.as_deref().unwrap_or("")
        );
    }
}
