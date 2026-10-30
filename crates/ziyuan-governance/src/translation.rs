//! 翰林院译书：诏令与译书块的关联，及御览定稿与退回重修。

use crate::{Court, GovernanceError, Task};
use serde::{Deserialize, Serialize};
use ziyuan_core::{Capability, CourtEvent, Department, TaskId, TranslationLink};

/// 退回理由的长度上限（字符）。
const MAX_REASON: usize = 500;
/// 输出路径的长度上限（字节）。
const MAX_OUTPUT_PATH: usize = 512;

/// 一块译稿最近一次御览的结果：定稿或退回（以最后一次为准）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Signoff {
    Finalized {
        translation_hash: String,
        context_version: u32,
        zhupi_version: u32,
    },
    Reopened {
        reason: String,
    },
}

fn invalid(why: impl Into<String>) -> GovernanceError {
    GovernanceError::Translation(why.into())
}

fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn check_project(project: &str) -> Result<(), GovernanceError> {
    if is_lower_hex(project, 32) {
        Ok(())
    } else {
        Err(invalid(format!(
            "项目 ID 须为 32 位小写十六进制：{project:?}"
        )))
    }
}

/// 块 ID 为 `c<章号>-b<块号>`，各至少四位数字（导出时补零到四位）。
fn check_block(block: &str) -> Result<(), GovernanceError> {
    let digits = |s: &str| (4..=9).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit());
    let ok = block
        .strip_prefix('c')
        .and_then(|rest| rest.split_once("-b"))
        .is_some_and(|(chapter, index)| digits(chapter) && digits(index));
    if ok {
        Ok(())
    } else {
        Err(invalid(format!("块 ID 须形如 c0001-b0001：{block:?}")))
    }
}

fn check_hash(what: &str, hash: &str) -> Result<(), GovernanceError> {
    if is_lower_hex(hash, 64) {
        Ok(())
    } else {
        Err(invalid(format!(
            "{what}须为 SHA-256（64 位小写十六进制）：{hash:?}"
        )))
    }
}

/// 译稿只能写在 `translations/<项目>/<块>/` 之下的项目内相对路径。
fn check_output_path(link: &TranslationLink) -> Result<(), GovernanceError> {
    let path = &link.output_path;
    let prefix = format!("translations/{}/{}/", link.project, link.block);
    let rest = path.strip_prefix(&prefix);
    let ok = path.len() <= MAX_OUTPUT_PATH
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && rest.is_some_and(|rest| {
            !rest.is_empty()
                && rest
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != "..")
        });
    if ok {
        Ok(())
    } else {
        Err(invalid(format!(
            "译稿路径须在 {prefix} 之下（以 / 分隔，不含 `..`）：{path:?}"
        )))
    }
}

impl Court {
    /// 译书关联：把待办（或执行中）的诏令 `task` 关联到一块。初译、修订须为工部诏令，
    /// 玉玺含 write_file，译稿写在 `translations/<项目>/<块>/` 之下；审校须只读，不写译稿。
    pub fn propose_translation_link(
        &self,
        task: TaskId,
        link: TranslationLink,
    ) -> Result<CourtEvent, GovernanceError> {
        self.check_translation_link(task, &link)?;
        let TranslationLink {
            project,
            block,
            kind,
            source_hash,
            context_version,
            output_path,
            zhupi_hash,
        } = link;
        Ok(CourtEvent::TranslationLinked {
            task,
            project,
            block,
            kind,
            source_hash,
            context_version,
            output_path,
            zhupi_hash,
        })
    }

    pub(crate) fn check_translation_link(
        &self,
        task: TaskId,
        link: &TranslationLink,
    ) -> Result<(), GovernanceError> {
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        if !entry.status.is_open() {
            return Err(GovernanceError::TaskFinished(task));
        }
        if entry.translation.is_some() {
            return Err(invalid(format!("诏令 #{} 已关联译书块", task.0)));
        }
        check_project(&link.project)?;
        check_block(&link.block)?;
        check_hash("原文哈希", &link.source_hash)?;
        let seal = &entry.edict.seal;
        let writes = seal.capabilities.contains(&Capability::WriteFile);
        if link.kind.writes() {
            if entry.edict.department != Department::Works || !writes {
                return Err(invalid(format!(
                    "{}须为工部诏令，且玉玺授 write_file",
                    link.kind.label()
                )));
            }
            check_output_path(link)?;
            check_hash("朱批哈希", &link.zhupi_hash)?;
        } else {
            if seal.capabilities.iter().any(|c| c.is_works_only()) {
                return Err(invalid(format!(
                    "{}须只读：玉玺不得授 write_file 或 run_tests",
                    link.kind.label()
                )));
            }
            if !link.output_path.is_empty() || !link.zhupi_hash.is_empty() {
                return Err(invalid(format!("{}不写译稿、不附朱批", link.kind.label())));
            }
        }
        Ok(())
    }

    /// 御览定稿：签署一块的译稿。
    pub fn propose_translation_finalized(
        &self,
        project: String,
        block: String,
        translation_hash: String,
        context_version: u32,
        zhupi_version: u32,
    ) -> Result<CourtEvent, GovernanceError> {
        check_project(&project)?;
        check_block(&block)?;
        check_hash("译稿哈希", &translation_hash)?;
        Ok(CourtEvent::TranslationFinalized {
            project,
            block,
            translation_hash,
            context_version,
            zhupi_version,
        })
    }

    /// 退回重修：理由须非空、单行，至多 500 字。
    pub fn propose_translation_reopened(
        &self,
        project: String,
        block: String,
        reason: String,
    ) -> Result<CourtEvent, GovernanceError> {
        check_project(&project)?;
        check_block(&block)?;
        if reason.trim().is_empty() {
            return Err(invalid("退回须写明理由"));
        }
        if reason.chars().count() > MAX_REASON {
            return Err(invalid(format!("退回理由至多 {MAX_REASON} 字")));
        }
        if reason.chars().any(char::is_control) {
            return Err(invalid("退回理由须为单行，不含控制字符"));
        }
        Ok(CourtEvent::TranslationReopened {
            project,
            block,
            reason,
        })
    }

    /// 一块最近一次御览的结果；从未定稿或退回为 `None`。
    pub fn translation_signoff(&self, project: &str, block: &str) -> Option<&Signoff> {
        self.translation_signoffs.get(project)?.get(block)
    }

    /// 一个项目内各块最近一次御览的结果，按块 ID 升序。
    pub fn translation_signoffs(&self, project: &str) -> impl Iterator<Item = (&str, &Signoff)> {
        self.translation_signoffs
            .get(project)
            .into_iter()
            .flatten()
            .map(|(block, signoff)| (block.as_str(), signoff))
    }

    /// 关联到某块的诏令，按 ID 升序。
    pub fn translation_tasks<'a>(
        &'a self,
        project: &'a str,
        block: &'a str,
    ) -> impl Iterator<Item = &'a Task> + 'a {
        self.tasks.values().filter(move |t| {
            t.translation
                .as_ref()
                .is_some_and(|l| l.project == project && l.block == block)
        })
    }

    pub(crate) fn record_signoff(&mut self, project: &str, block: &str, signoff: Signoff) {
        self.translation_signoffs
            .entry(project.to_owned())
            .or_default()
            .insert(block.to_owned(), signoff);
    }
}
