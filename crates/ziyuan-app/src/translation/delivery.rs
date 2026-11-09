//! Read-only handoff checks share the actual finished-export gate.
use super::*;
use serde::Serialize;
#[derive(Clone, Debug, Serialize)]
pub struct DeliveryReport {
    pub project: String,
    pub format: ExportFormat,
    pub export_ready: bool,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    pub open_issues: usize,
    pub blocks: Vec<BlockStatus>,
    pub files: Vec<String>,
}
impl Translations {
    pub fn delivery_preflight(
        &self,
        project: &str,
        options: &ExportOptions,
    ) -> Result<DeliveryReport> {
        let material = self.reading_materials(project)?;
        let p = self.project(project)?;
        let mut selected = std::collections::BTreeSet::new();
        for chapter in &options.chapters {
            if !selected.insert(chapter) || !p.chapters.iter().any(|c| &c.id == chapter) {
                return Err(invalid("章节选择重复或不存在"));
            }
        }
        let issues = self.issues(project)?;
        let mut report = DeliveryReport {
            project: project.into(),
            format: options.format,
            export_ready: false,
            blockers: Vec::new(),
            warnings: material.reading.warnings,
            open_issues: 0,
            blocks: Vec::new(),
            files: Vec::new(),
        };
        for issue in issues.issues {
            if (!selected.is_empty()
                && !issue.anchors.iter().any(|a| selected.contains(&a.chapter)))
                || (issue.current
                    && matches!(
                        issue.record.state,
                        IssueState::Intentional | IssueState::Resolved
                    ))
            {
                continue;
            }
            report.open_issues += 1;
            report.warnings.push(format!(
                "待查 {}：{}{}",
                issue.record.issue_id,
                if issue.current {
                    ""
                } else {
                    "旧处置已过期；"
                },
                issue.record.input.body
            ));
        }
        match crate::Session::open(ziyuan_storage::EventLog::new(self.root())) {
            Ok(mut session) => {
                report.blocks = session
                    .translation_status(self.root(), project)
                    .map_err(|e| invalid(e.to_string()))?;
                report
                    .blocks
                    .retain(|b| selected.is_empty() || selected.contains(&b.chapter));
                for block in &report.blocks {
                    if !matches!(
                        block.state,
                        BlockState::Finalized { .. } | BlockState::HumanFinalized { .. }
                    ) {
                        report
                            .blockers
                            .push(format!("{}：{}", block.block, block.state.label()));
                    }
                }
                match session.export_finished(self.root(), project, options) {
                    Ok(bundle) => {
                        report.files = bundle.files.into_iter().map(|f| f.name).collect();
                        report.warnings.extend(bundle.warnings);
                        report.export_ready = report.blockers.is_empty();
                    }
                    Err(error) => report.blockers.push(error.to_string()),
                }
            }
            Err(crate::AppError::NotFounded) => {
                report.blockers.push("尚无审批事件，不能导出成稿".into())
            }
            Err(error) => return Err(invalid(error.to_string())),
        }
        if material.reading.fingerprint != self.reading_materials(project)?.reading.fingerprint {
            return Err(invalid("预检期间材料已变化，请重试"));
        }
        Ok(report)
    }
}
