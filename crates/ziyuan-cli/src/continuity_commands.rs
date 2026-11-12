use std::{error::Error, path::PathBuf};
use ziyuan_app::translation::{Translations, inspect_backup, restore_backup};
pub(crate) const HELP: &str = "text backup [--output <新文件.tar>]：预览或导出全部译书项目与共用审批历史（不含凭据）；text restore <备份.tar> <新目录> [--yes]：默认校验预览，加 --yes 恢复。text source-update <项目> <原文.txt> [--chapter <章>] [--confirm <指纹>]；text issue list|add|review；text read <项目> [--chapter <章>]；text preflight <项目> [--format txt|md|html] [--chapter <章>]；text search <项目> <片段> [--scope all|source|translation|draft|history]：字面检索，默认 all；text draft-replace <项目> <request.json>：默认只预览人工草稿替换，确认时在请求中带预览 fingerprint 与 confirm:true；text tmx export <项目> [--output <文件.tmx>]；text tmx preview <项目> <文件.tmx>；text tmx import <项目> <文件.tmx> --confirm <预览指纹>";
pub(crate) enum Action {
    Tmx(String, String, Option<PathBuf>, Option<String>),
    Search(String, String, String),
    Replace(String, PathBuf),
    Backup(Option<PathBuf>),
    Restore(PathBuf, PathBuf, bool),
    Update(String, PathBuf, Option<String>, Option<String>),
    Issues(String),
    AddIssue(String, PathBuf, String),
    ReviewIssue(String, u64, PathBuf),
    Read(String, Option<String>),
    Delivery(String, ziyuan_app::translation::ExportOptions),
}
pub(crate) fn parse(action: &str, rest: &[String]) -> Result<Action, Box<dyn Error>> {
    Ok(match (action, rest) {
        ("tmx", [operation, project]) if operation == "export" => {
            Action::Tmx(operation.clone(), project.clone(), None, None)
        }
        ("tmx", [operation, project, flag, file])
            if operation == "export" && flag == "--output" =>
        {
            Action::Tmx(operation.clone(), project.clone(), Some(file.into()), None)
        }
        ("tmx", [operation, project, file]) if operation == "preview" => {
            Action::Tmx(operation.clone(), project.clone(), Some(file.into()), None)
        }
        ("tmx", [operation, project, file, flag, fingerprint])
            if operation == "import" && flag == "--confirm" =>
        {
            Action::Tmx(
                operation.clone(),
                project.clone(),
                Some(file.into()),
                Some(fingerprint.clone()),
            )
        }
        ("search", [project, query]) => {
            Action::Search(project.clone(), query.clone(), "all".into())
        }
        ("search", [project, query, flag, scope]) if flag == "--scope" => {
            Action::Search(project.clone(), query.clone(), scope.clone())
        }
        ("draft-replace", [project, file]) => Action::Replace(project.clone(), file.into()),
        ("read", [project]) => Action::Read(project.clone(), None),
        ("read", [project, flag, chapter]) if flag == "--chapter" => {
            Action::Read(project.clone(), Some(chapter.clone()))
        }
        ("preflight", [project, flags @ ..]) => {
            let mut options = ziyuan_app::translation::ExportOptions::default();
            for pair in flags.chunks(2) {
                match pair {
                    [flag, value] if flag == "--chapter" => options.chapters.push(value.clone()),
                    [flag, value] if flag == "--format" => {
                        options.format = match value.as_str() {
                            "txt" => ziyuan_app::translation::ExportFormat::Txt,
                            "md" | "markdown" => ziyuan_app::translation::ExportFormat::Markdown,
                            "html" => ziyuan_app::translation::ExportFormat::Html,
                            _ => return Err("预检格式仅支持txt/md/html".into()),
                        }
                    }
                    _ => {
                        return Err(
                            "用法: text preflight <项目> [--chapter <章>] [--format txt|md|html]"
                                .into(),
                        );
                    }
                }
            }
            Action::Delivery(project.clone(), options)
        }
        ("issue", [op, project]) if op == "list" => Action::Issues(project.clone()),
        ("issue", [op, project, file, flag, fp]) if op == "add" && flag == "--confirm" => {
            Action::AddIssue(project.clone(), file.into(), fp.clone())
        }
        ("issue", [op, project, id, file]) if op == "review" => {
            Action::ReviewIssue(project.clone(), id.parse()?, file.into())
        }
        ("source-update", [project, file, flags @ ..]) => {
            let mut chapter = None;
            let mut confirm = None;
            for flag in flags.chunks(2) {
                match flag {
                    [key,value] if key == "--chapter" && chapter.is_none() => chapter=Some(value.clone()),
                    [key,value] if key == "--confirm" && confirm.is_none() => confirm=Some(value.clone()),
                    _ => return Err("用法: text source-update <项目> <新原文.txt> [--chapter <修订章ID>] [--confirm <预览指纹>]；不指定章节则追加".into()),
                }
            }
            Action::Update(project.clone(), file.into(), chapter, confirm)
        }
        ("backup", []) => Action::Backup(None),
        ("backup", [flag, file]) if flag == "--output" => Action::Backup(Some(file.into())),
        ("restore", [file, destination]) => Action::Restore(file.into(), destination.into(), false),
        ("restore", [file, destination, yes]) if yes == "--yes" => {
            Action::Restore(file.into(), destination.into(), true)
        }
        _ => return Err(HELP.into()),
    })
}
pub(crate) fn run(action: Action, _json: bool) -> Result<(), Box<dyn Error>> {
    let root = std::env::current_dir()?;
    let manifest = match action {
        Action::Tmx(operation, project, file, expected) => {
            let t = Translations::new(&root);
            if operation == "export" {
                let bytes = t.export_tmx(&project)?;
                if let Some(file) = file {
                    ziyuan_app::translation::backup::publish_backup(&root, &file, &bytes)?;
                } else {
                    use std::io::Write;
                    std::io::stdout().write_all(&bytes)?;
                }
                return Ok(());
            }
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(file.ok_or("缺少TMX文件")?)?
                .take(8 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            let result = if operation == "preview" {
                serde_json::to_value(t.preview_tmx(&project, &bytes)?)?
            } else {
                serde_json::json!({"added":t.import_tmx(&project,&bytes,&expected.ok_or("须先预览并确认")?)?})
            };
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
        Action::Search(project, query, scope) => {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &Translations::new(&root).search_text(&project, &query, &scope)?
                )?
            );
            return Ok(());
        }
        Action::Replace(project, file) => {
            let input = serde_json::from_value(read_json(&file)?)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &Translations::new(&root).replace_draft_text(&project, &input)?
                )?
            );
            return Ok(());
        }
        Action::Read(project, chapter) => {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &Translations::new(&root).read_chapter(&project, chapter.as_deref())?
                )?
            );
            return Ok(());
        }
        Action::Delivery(project, options) => {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &Translations::new(&root).delivery_preflight(&project, &options)?
                )?
            );
            return Ok(());
        }
        Action::Issues(project) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&Translations::new(&root).issues(&project)?)?
            );
            return Ok(());
        }
        Action::AddIssue(project, file, expected) => {
            let input = serde_json::from_value(read_json(&file)?)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &Translations::new(&root).add_issue(&project, &input, &expected)?
                )?
            );
            return Ok(());
        }
        Action::ReviewIssue(project, id, file) => {
            let input = serde_json::from_value(read_json(&file)?)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &Translations::new(&root).review_issue(&project, id, &input)?
                )?
            );
            return Ok(());
        }
        Action::Update(project, file, chapter, confirm) => {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(&file)?
                .take(8 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            let input = ziyuan_app::translation::SourceUpdate {
                chapter,
                name: file
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or("文件名不是UTF-8")?
                    .into(),
                text: String::from_utf8(bytes)?,
            };
            let t = Translations::new(&root);
            let result = if let Some(confirm) = confirm {
                serde_json::to_value(t.apply_source_update(&project, &input, &confirm)?)?
            } else {
                serde_json::to_value(t.preview_source_update(&project, &input)?)?
            };
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
        Action::Backup(output) => {
            let bundle = Translations::new(&root).backup_workspace()?;
            if let Some(output) = output {
                ziyuan_app::translation::backup::publish_backup(&root, &output, &bundle.bytes)?;
            }
            bundle.manifest
        }
        Action::Restore(file, target, apply) => {
            let bytes = ziyuan_app::translation::backup::read_backup(&file)?;
            if apply {
                restore_backup(&bytes, &root.join(target))?
            } else {
                inspect_backup(&bytes)?
            }
        }
    };
    println!("{}", serde_json::to_string_pretty(&manifest)?);
    Ok(())
}
fn read_json(path: &std::path::Path) -> Result<serde_json::Value, Box<dyn Error>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(65537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err("输入JSON超过64 KiB".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::HELP;

    #[test]
    fn help_lists_search_draft_replace_and_tmx() {
        for needle in [
            "text search",
            "--scope all|source|translation|draft|history",
            "text draft-replace",
            "text tmx export",
            "text tmx preview",
            "text tmx import",
        ] {
            assert!(HELP.contains(needle), "帮助缺少 {needle}");
        }
    }
}
