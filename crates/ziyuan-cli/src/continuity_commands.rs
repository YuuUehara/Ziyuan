use std::{error::Error, path::PathBuf};
use ziyuan_app::translation::{Translations, inspect_backup, restore_backup};
pub(crate) const HELP: &str = "text backup [--output <新文件.tar>]：预览或导出全部译书项目与共用审批历史（不含凭据）；text restore <备份.tar> <新目录> [--yes]：默认校验预览，加 --yes 恢复。text source-update <项目> <原文.txt|md|docx|epub> [--chapter <章>] [--confirm <指纹>]；text issue list|add|review；text read <项目> [--chapter <章>]；text preflight <项目> [--format txt|md|html] [--chapter <章>]；text search <项目> <片段> [--scope all|source|translation|draft|history]：字面检索，默认 all；text draft-replace <项目> <request.json>：默认只预览人工草稿替换，确认时在请求中带预览 fingerprint 与 confirm:true；text tmx export <项目> [--output <文件.tmx>]；text tmx preview <项目> <文件.tmx>；text tmx import <项目> <文件.tmx> --confirm <预览指纹>。以上命令默认打印中文摘要；加 --json 打印完整 JSON。tmx export 不写 --output 时仍把 TMX 正文写到标准输出，--json 不改变这份正文。";
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
                    _ => return Err("用法: text source-update <项目> <新原文.txt|md|docx|epub> [--chapter <修订章ID>] [--confirm <预览指纹>]；不指定章节则追加".into()),
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
pub(crate) fn run(action: Action, json: bool) -> Result<(), Box<dyn Error>> {
    let root = std::env::current_dir()?;
    match action {
        Action::Tmx(operation, project, file, expected) => {
            let t = Translations::new(&root);
            if operation == "export" {
                let bytes = t.export_tmx(&project)?;
                if let Some(file) = file {
                    ziyuan_app::translation::backup::publish_backup(&root, &file, &bytes)?;
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "project": project,
                                "file": file,
                                "bytes": bytes.len(),
                            }))?
                        );
                    } else {
                        println!(
                            "已导出 TMX：{}（{} 字节）。未改译稿，也未批准。",
                            file.display(),
                            bytes.len()
                        );
                    }
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
            if operation == "preview" {
                let preview = t.preview_tmx(&project, &bytes)?;
                return print_report(
                    json,
                    serde_json::to_string_pretty(&preview),
                    &tmx_preview_prose(&preview),
                );
            }
            let added = t.import_tmx(&project, &bytes, &expected.ok_or("须先预览并确认")?)?;
            let result = serde_json::json!({"added": added});
            print_report(
                json,
                serde_json::to_string_pretty(&result),
                &format!("已合并 {added} 条到本地翻译记忆。未改译稿，也未批准。"),
            )
        }
        Action::Search(project, query, scope) => {
            let found = Translations::new(&root).search_text(&project, &query, &scope)?;
            print_report(
                json,
                serde_json::to_string_pretty(&found),
                &search_prose(&found),
            )
        }
        Action::Replace(project, file) => {
            let input = serde_json::from_value(read_json(&file)?)?;
            let preview = Translations::new(&root).replace_draft_text(&project, &input)?;
            print_report(
                json,
                serde_json::to_string_pretty(&preview),
                &replace_prose(&preview),
            )
        }
        Action::Read(project, chapter) => {
            let reading = Translations::new(&root).read_chapter(&project, chapter.as_deref())?;
            print_report(
                json,
                serde_json::to_string_pretty(&reading),
                &reading_prose(&reading),
            )
        }
        Action::Delivery(project, options) => {
            let report = Translations::new(&root).delivery_preflight(&project, &options)?;
            print_report(
                json,
                serde_json::to_string_pretty(&report),
                &preflight_prose(&report),
            )
        }
        Action::Issues(project) => {
            let issues = Translations::new(&root).issues(&project)?;
            print_report(
                json,
                serde_json::to_string_pretty(&issues),
                &issue_list_prose(&issues),
            )
        }
        Action::AddIssue(project, file, expected) => {
            let input = serde_json::from_value(read_json(&file)?)?;
            let record = Translations::new(&root).add_issue(&project, &input, &expected)?;
            print_report(
                json,
                serde_json::to_string_pretty(&record),
                &issue_record_prose("已登记待查", &record),
            )
        }
        Action::ReviewIssue(project, id, file) => {
            let input = serde_json::from_value(read_json(&file)?)?;
            let record = Translations::new(&root).review_issue(&project, id, &input)?;
            print_report(
                json,
                serde_json::to_string_pretty(&record),
                &issue_record_prose("已更新待查", &record),
            )
        }
        Action::Update(project, file, chapter, confirm) => {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(&file)?
                .take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            let name = file
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or("文件名不是UTF-8")?
                .to_owned();
            let t = Translations::new(&root);
            let source_language = t.project(&project).ok().map(|p| p.metadata.source_language);
            let input =
                ziyuan_app::translation::Translations::source_update_from_bytes_for_language(
                    chapter,
                    name,
                    &bytes,
                    source_language.as_deref(),
                )?;
            if let Some(confirm) = confirm {
                let project = t.apply_source_update(&project, &input, &confirm)?;
                let prose = format!(
                    "已写入原文变化。项目 {} 现有 {} 章。旧译和批准没有改掉。",
                    project.id,
                    project.chapters.len()
                );
                return print_report(json, serde_json::to_string_pretty(&project), &prose);
            }
            let preview = t.preview_source_update(&project, &input)?;
            print_report(
                json,
                serde_json::to_string_pretty(&preview),
                &source_preview_prose(&preview),
            )
        }
        Action::Backup(output) => {
            let bundle = Translations::new(&root).backup_workspace()?;
            let wrote = output.is_some();
            if let Some(output) = output {
                ziyuan_app::translation::backup::publish_backup(&root, &output, &bundle.bytes)?;
            }
            let prose = backup_prose(&bundle.manifest, wrote);
            print_report(json, serde_json::to_string_pretty(&bundle.manifest), &prose)
        }
        Action::Restore(file, target, apply) => {
            let bytes = ziyuan_app::translation::backup::read_backup(&file)?;
            let manifest = if apply {
                restore_backup(&bytes, &root.join(&target))?
            } else {
                inspect_backup(&bytes)?
            };
            let prose = if apply {
                format!(
                    "已恢复到 {}。项目 {} 个，文件 {} 个。",
                    target.display(),
                    manifest.projects.len(),
                    manifest.files.len()
                )
            } else {
                format!(
                    "备份校验通过，尚未恢复。项目 {} 个，文件 {} 个。确认时加 --yes。",
                    manifest.projects.len(),
                    manifest.files.len()
                )
            };
            print_report(json, serde_json::to_string_pretty(&manifest), &prose)
        }
    }
}

fn print_report(
    json: bool,
    pretty: Result<String, serde_json::Error>,
    prose: &str,
) -> Result<(), Box<dyn Error>> {
    println!("{}", report_text(json, pretty?, prose));
    Ok(())
}

fn report_text(json: bool, pretty: String, prose: &str) -> String {
    if json { pretty } else { prose.to_owned() }
}

fn tmx_preview_prose(preview: &ziyuan_app::translation::TmxPreview) -> String {
    format!(
        "TMX 预览：{} 条，重复 {} 条。语种 {} → {}。\n指纹：{}\n{}\n确认导入时带上同一指纹。未改译稿，也未批准。",
        preview.entries,
        preview.duplicates,
        preview.source_language,
        preview.target_language,
        preview.fingerprint,
        lines_of("警告", &preview.warnings)
    )
}

fn clip(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = flat.chars().count();
    if count <= 80 {
        flat
    } else {
        format!("{}…", flat.chars().take(80).collect::<String>())
    }
}

fn lines_of(title: &str, items: &[String]) -> String {
    if items.is_empty() {
        format!("{title}：无")
    } else {
        format!("{title}：\n{}", items.join("\n"))
    }
}

fn source_preview_prose(preview: &ziyuan_app::translation::SourceUpdatePreview) -> String {
    let mut lines = vec![
        format!(
            "原文变化预览：项目 {}，章节 {}。",
            preview.project, preview.chapter
        ),
        format!("指纹：{}", preview.fingerprint),
        format!(
            "变化 {} 处，受影响块 {} 个。",
            preview.changes.len(),
            preview.changed_blocks.len()
        ),
    ];
    for change in &preview.changes {
        let body = match (&change.before, &change.after) {
            (None, Some(after)) => format!("新增 {}", clip(after)),
            (Some(before), None) => format!("删除 {}", clip(before)),
            (Some(before), Some(after)) => format!("{} → {}", clip(before), clip(after)),
            (None, None) => "无正文差异".to_owned(),
        };
        lines.push(format!("{}：{body}", change.paragraph));
    }
    lines.push(lines_of("阻塞", &preview.blockers));
    lines.push(lines_of("警告", &preview.warnings));
    if preview.blockers.is_empty() {
        lines.push(
            "没有阻塞项。确认时带上同一指纹：ziyuan text source-update <项目> <文件> --confirm <指纹>"
                .to_owned(),
        );
    }
    lines.join("\n")
}

fn preflight_prose(report: &ziyuan_app::translation::DeliveryReport) -> String {
    let format = match report.format {
        ziyuan_app::translation::ExportFormat::Txt => "TXT",
        ziyuan_app::translation::ExportFormat::Markdown => "Markdown",
        ziyuan_app::translation::ExportFormat::Html => "HTML",
    };
    let verdict = if report.export_ready {
        "可以导出"
    } else {
        "暂不能导出"
    };
    let mut lines = vec![
        format!(
            "交稿预检（{format}）：{verdict}。待查 {} 项。",
            report.open_issues
        ),
        lines_of("阻塞", &report.blockers),
        lines_of("警告", &report.warnings),
    ];
    if !report.files.is_empty() {
        lines.push(format!("预览文件：{}", report.files.join("、")));
    }
    lines.join("\n")
}

fn search_prose(found: &ziyuan_app::translation::TextSearch) -> String {
    let mut lines = vec![format!(
        "找到 {} 处{}。",
        found.hits.len(),
        if found.truncated {
            "，已到 200 处上限"
        } else {
            ""
        }
    )];
    for hit in &found.hits {
        let kind = match hit.kind.as_str() {
            "source" => "原文",
            "translation" => "现行译文",
            "draft" => "人工草稿",
            "history" => "历史译稿",
            other => other,
        };
        lines.push(format!(
            "{} · {} · {}：{}",
            hit.chapter,
            hit.paragraph,
            kind,
            clip(&hit.text)
        ));
    }
    lines.join("\n")
}

fn replace_prose(preview: &ziyuan_app::translation::DraftReplacePreview) -> String {
    if preview.saved {
        format!(
            "已写入人工草稿，出现 {} 处。未送审，也未改已准奏译稿。\n指纹：{}",
            preview.occurrences, preview.fingerprint
        )
    } else {
        format!(
            "替换预览：出现 {} 处，尚未写入。\n指纹：{}\n替换前：{}\n替换后：{}\n确认时在请求里带上 fingerprint 与 confirm:true。",
            preview.occurrences,
            preview.fingerprint,
            clip(&preview.before),
            clip(&preview.after)
        )
    }
}

fn reading_prose(reading: &ziyuan_app::translation::ReadingView) -> String {
    let mut lines = vec![format!(
        "通读 {}，共 {} 段。",
        reading.project,
        reading.paragraphs.len()
    )];
    lines.push(lines_of("警告", &reading.warnings));
    let mut chapter = String::new();
    for paragraph in &reading.paragraphs {
        if chapter != paragraph.chapter {
            chapter = paragraph.chapter.clone();
            lines.push(format!("{} {}", paragraph.chapter, paragraph.title));
        }
        let translation = paragraph
            .translation
            .as_deref()
            .map(clip)
            .unwrap_or_else(|| "尚无已准奏译稿".to_owned());
        lines.push(format!(
            "{} 原文：{}",
            paragraph.paragraph,
            clip(&paragraph.source)
        ));
        lines.push(format!("{} 译文：{translation}", paragraph.paragraph));
    }
    lines.join("\n")
}

fn issue_state(state: &ziyuan_app::translation::IssueState) -> &'static str {
    match state {
        ziyuan_app::translation::IssueState::Pending => "待查",
        ziyuan_app::translation::IssueState::Confirmed => "已确认",
        ziyuan_app::translation::IssueState::Intentional => "有意异译",
        ziyuan_app::translation::IssueState::Resolved => "已解决",
    }
}

fn issue_list_prose(list: &ziyuan_app::translation::IssueList) -> String {
    let mut lines = vec![format!(
        "待查 {} 条。清单指纹：{}",
        list.issues.len(),
        list.fingerprint
    )];
    for issue in &list.issues {
        let fresh = if issue.current { "当前" } else { "已过期" };
        lines.push(format!(
            "#{} {}（{fresh}）：{}",
            issue.record.issue_id,
            issue_state(&issue.record.state),
            clip(&issue.record.input.body)
        ));
    }
    lines.join("\n")
}

fn issue_record_prose(title: &str, record: &ziyuan_app::translation::IssueRecord) -> String {
    format!(
        "{title} #{}，状态{}。{}",
        record.issue_id,
        issue_state(&record.state),
        clip(&record.input.body)
    )
}

fn backup_prose(manifest: &ziyuan_app::translation::BackupManifest, wrote: bool) -> String {
    let lead = if wrote {
        "已写入备份"
    } else {
        "备份预览，尚未写入文件"
    };
    format!(
        "{lead}。项目 {} 个，文件 {} 个。不含登录令牌和模型凭据。",
        manifest.projects.len(),
        manifest.files.len()
    )
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
    use super::{HELP, preflight_prose, report_text};
    use ziyuan_app::translation::{DeliveryReport, ExportFormat};

    #[test]
    fn help_lists_search_draft_replace_and_tmx() {
        for needle in [
            "text search",
            "--scope all|source|translation|draft|history",
            "text draft-replace",
            "text tmx export",
            "text tmx preview",
            "text tmx import",
            "默认打印中文摘要",
            "加 --json 打印完整 JSON",
        ] {
            assert!(HELP.contains(needle), "帮助缺少 {needle}");
        }
    }

    #[test]
    fn preflight_summary_is_prose_and_json_keeps_the_report() {
        let report = DeliveryReport {
            project: "book".into(),
            format: ExportFormat::Txt,
            export_ready: false,
            blockers: vec!["第2章 · 译稿第1组（c0002-b0001）：未开始".into()],
            warnings: Vec::new(),
            open_issues: 0,
            blocks: Vec::new(),
            files: Vec::new(),
        };
        let prose = preflight_prose(&report);
        assert!(prose.contains("暂不能导出"));
        assert!(prose.contains("第2章 · 译稿第1组"));
        assert!(!prose.starts_with('{'));
        let json = report_text(true, serde_json::to_string_pretty(&report).unwrap(), &prose);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["export_ready"], false);
        assert_eq!(report_text(false, String::new(), &prose), prose);
    }
}
