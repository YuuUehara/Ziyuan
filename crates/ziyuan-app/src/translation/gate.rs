//! 准奏门控：带初译或修订关联的诏令，按逐段取舍后的最终字节检查，不过即不落盘。
use super::{Translations, block_source_hash, output_rel, proposals_rel};
use crate::AppError;
use std::path::Path;
use ziyuan_core::{FileChange, TranslationLink};

fn under_proposals(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix).is_some_and(|rest| {
        !rest.is_empty()
            && !path.contains('\\')
            && !path.chars().any(char::is_control)
            && rest
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
    })
}

/// 门控发现的问题（空即通过）。`selected` 是逐段取舍后实际要写的文件。
pub(crate) fn gate_problems(
    root: &Path,
    link: &TranslationLink,
    selected: &[FileChange],
) -> Vec<String> {
    let mut problems = Vec::new();
    let output = output_rel(&link.project, &link.block);
    let proposals = proposals_rel(&link.project, &link.block);
    if link.output_path != output {
        problems.push(format!("关联的译稿路径 {} 不是 {output}", link.output_path));
    }
    for file in selected {
        if file.path != output && !under_proposals(&file.path, &proposals) {
            problems.push(format!(
                "只可写 {output} 与 {proposals} 下的文件：{}",
                file.path
            ));
        }
    }
    let translations = Translations::new(root);
    match selected.iter().find(|f| f.path == output) {
        None => problems.push(format!("逐段取舍后没有写译稿 {output}")),
        Some(file) => {
            match translations
                .texts()
                .check(&link.project, &link.block, file.after.as_bytes())
            {
                Err(e) => problems.push(format!("无法核对原文项目：{e}")),
                Ok(report) => {
                    problems.extend(report.issues.iter().map(|i| match &i.paragraph_id {
                        Some(id) => format!("{id}：{}", i.message),
                        None => i.message.clone(),
                    }))
                }
            }
        }
    }
    match translations
        .project(&link.project)
        .and_then(|p| block_source_hash(&p, &link.block))
    {
        Ok(hash) if hash == link.source_hash => {}
        Ok(_) => problems.push("版本失效：原文已与下诏时不同".into()),
        Err(e) => problems.push(format!("无法核对原文：{e}")),
    }
    match translations.context_version(&link.project) {
        Ok(current) if current == link.context_version => {}
        Ok(current) => problems.push(format!(
            "版本失效：诏令所用上下文 v{}，现行 v{current}",
            link.context_version
        )),
        Err(e) => problems.push(format!("无法核对上下文版本：{e}")),
    }
    problems
}

/// 门控不过时返回 [`AppError::Apply`]，列出全部问题；一处也不写，不留记录。
pub(crate) fn check_gate(
    root: &Path,
    link: &TranslationLink,
    selected: &[FileChange],
) -> Result<(), AppError> {
    let problems = gate_problems(root, link, selected);
    if problems.is_empty() {
        return Ok(());
    }
    Err(AppError::Apply(format!(
        "{}（{} 块）未过译书门控：{}",
        link.kind.label(),
        link.block,
        problems.join("；")
    )))
}
