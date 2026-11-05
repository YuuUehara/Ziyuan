//! 翰林院译书：诏令与译书块的关联，及御览定稿与退回重修。

use crate::{Court, GovernanceError, Task, TaskStatus};
use serde::{Deserialize, Serialize};
use ziyuan_core::{
    Capability, CourtEvent, Department, HumanApproval, HumanApprovalRequest, HumanBindingRequest,
    HumanFinalization, HumanSubmissionBinding, ReferenceBinding, TaskId, TranslationLink,
    TranslationSource,
};

/// Domain, representation version, then declaration-order request fields.
/// Candidate bytes and their digest are both covered; no event refers to itself.
pub fn human_binding_request_hash(request: &HumanBindingRequest) -> String {
    let bytes = serde_json::to_vec(&("ziyuan/human-submission-bind", 1, request))
        .expect("binding request contains only JSON-serializable fields");
    sha256(&bytes)
}

/// Domain, representation version, then declaration-order request fields.
/// The request carries no operation identity, which belongs to the write itself.
pub fn human_approval_request_hash(request: &HumanApprovalRequest) -> String {
    let bytes = serde_json::to_vec(&("ziyuan/human-approval", 1, request))
        .expect("approval request contains only JSON-serializable fields");
    sha256(&bytes)
}

fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

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
    /// 人工定稿：来源可辨，未运行模型双审。
    HumanFinalized {
        translation_hash: String,
        context_version: u32,
        zhupi_hash: String,
        operation_id: String,
    },
}

fn invalid(why: impl Into<String>) -> GovernanceError {
    GovernanceError::Translation(why.into())
}

fn check_target_format(format: &str) -> Result<(), GovernanceError> {
    let ok = (1..=64).contains(&format.len())
        && format
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'/' | b'_'));
    if ok {
        Ok(())
    } else {
        Err(invalid("目标格式须为 1–64 位字母、数字或 . - / _"))
    }
}

fn check_translation_source(source: &TranslationSource) -> Result<(), GovernanceError> {
    let event = match source {
        TranslationSource::None {} => None,
        TranslationSource::Model {
            task,
            changes_version,
            event,
        } => {
            if task.0 == 0 {
                return Err(invalid("模型来源须为真实诏令，不得借用零号"));
            }
            check_hash(
                "模型改动版本",
                changes_version
                    .strip_prefix("sha256:")
                    .ok_or_else(|| invalid("模型改动版本须沿用 sha256: 前缀"))?,
            )?;
            Some(event)
        }
        TranslationSource::Human {
            operation_id,
            event,
        } => {
            if !is_lower_hex(operation_id, 32) {
                return Err(invalid("人工准奏操作须为 32 位小写十六进制"));
            }
            Some(event)
        }
    };
    if let Some(event) = event {
        check_hash("来源事件摘要", &event.hash)?;
    }
    Ok(())
}

fn check_request_id(what: &str, id: &str) -> Result<(), GovernanceError> {
    if (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        Ok(())
    } else {
        Err(invalid(format!(
            "{what}须为 1–128 位字母、数字、短横线或下划线"
        )))
    }
}

/// One logical target, inside this project and block. The application owns the
/// lossless conversion; governance only fixes the shape recorded by the fact.
fn check_approval_target(r: &HumanApprovalRequest) -> Result<(), GovernanceError> {
    let target = &r.target;
    let format_ok = (1..=64).contains(&target.format.len())
        && target
            .format
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'/' | b'_'));
    if !format_ok {
        return Err(invalid("人工准奏目标格式须为 1–64 位字母、数字或 . - / _"));
    }
    let expected = format!("translations/{}/{}/zh.json", r.project, r.block);
    if target.path != expected {
        return Err(invalid(format!("人工准奏目标路径须为 {expected}")));
    }
    if let Some(before) = &target.before {
        check_hash("准奏前目标摘要", before)?;
    }
    check_hash("准奏后目标摘要", &target.after)?;
    Ok(())
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
    /// Internal pending material only. Caller must validate candidate semantics,
    /// authentication, material freshness and actual log references separately.
    /// A replayed duplicate is rejected, even when its contents are identical.
    pub fn propose_human_submission_binding(
        &self,
        binding: HumanSubmissionBinding,
    ) -> Result<CourtEvent, GovernanceError> {
        let r = &binding.request;
        check_project(&r.project)?;
        check_block(&r.block)?;
        for id in [&r.request_id, &r.submission_id] {
            if !(1..=128).contains(&id.len())
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            {
                return Err(invalid(
                    "人工送审请求标识须为 1–128 位字母、数字、短横线或下划线",
                ));
            }
        }
        if !is_lower_hex(&r.revision, 32) {
            return Err(invalid("人工草稿修订须为 32 位小写十六进制"));
        }
        for hash in [
            &r.basis.project_hash,
            &r.basis.source_hash,
            &r.basis.zhupi_hash,
            &r.candidate_hash,
            &binding.request_hash,
        ] {
            check_hash("人工送审摘要", hash)?;
        }
        if let Some(hash) = &r.basis.translation_hash {
            check_hash("原译稿摘要", hash)?;
        }
        check_translation_source(&r.previous)?;
        if r.candidate.is_empty()
            || sha256(r.candidate.as_bytes()) != r.candidate_hash
            || human_binding_request_hash(r) != binding.request_hash
        {
            return Err(invalid("人工送审候选或请求摘要不符"));
        }
        // Bound the actual hex-encoded event, including JSON escaping, not text length.
        let json = serde_json::to_vec(&binding).expect("binding is JSON-serializable");
        if json.len() > (8 * 1024 * 1024 - "v1\tHUMANBOUND\t".len()) / 2 {
            return Err(invalid("人工送审绑定事件超过 8 MiB"));
        }
        if self
            .human_submission_binding(&r.project, &r.block, &r.request_id)
            .is_some()
        {
            return Err(invalid("人工送审请求已入簿，不得重复发布"));
        }
        Ok(CourtEvent::HumanSubmissionBound {
            binding: Box::new(binding),
        })
    }

    /// Frozen pending receipt; never an approved source or finalized translation.
    pub fn human_submission_binding(
        &self,
        project: &str,
        block: &str,
        request_id: &str,
    ) -> Option<&HumanSubmissionBinding> {
        self.human_bindings
            .get(project)?
            .get(block)?
            .get(request_id)
    }

    /// Durable approval fact. The binding must already be in this court; the
    /// fact records what was approved, not that the target was written.
    /// Log position and canonical line digest still require the write-time
    /// admission check; replay here only fixes the frozen material.
    pub fn propose_human_translation_approved(
        &self,
        approval: HumanApproval,
    ) -> Result<CourtEvent, GovernanceError> {
        let r = &approval.request;
        check_project(&r.project)?;
        check_block(&r.block)?;
        check_request_id("人工准奏请求标识", &r.request_id)?;
        check_request_id("人工送审请求标识", &r.submission_id)?;
        if !is_lower_hex(&approval.operation_id, 32) {
            return Err(invalid("人工准奏操作须为 32 位小写十六进制"));
        }
        check_hash("人工准奏请求摘要", &approval.request_hash)?;
        check_hash("送审绑定事件摘要", &r.binding.hash)?;
        check_translation_source(&r.previous)?;
        check_approval_target(r)?;
        if human_approval_request_hash(r) != approval.request_hash {
            return Err(invalid("人工准奏请求摘要不符"));
        }
        let binding = self
            .human_submission_binding(&r.project, &r.block, &r.submission_id)
            .ok_or_else(|| invalid("人工准奏须引用本块已入簿的送审绑定"))?;
        if binding.request.previous != r.previous {
            return Err(invalid("人工准奏的前一来源与送审绑定时不一致"));
        }
        // Bound the actual hex-encoded event, including JSON escaping, not text length.
        let json = serde_json::to_vec(&approval).expect("approval is JSON-serializable");
        if json.len() > (8 * 1024 * 1024 - "v1\tHUMANAPPROVED\t".len()) / 2 {
            return Err(invalid("人工准奏事件超过 8 MiB"));
        }
        if self
            .human_approval(&r.project, &r.block, &r.request_id)
            .is_some()
        {
            return Err(invalid("人工准奏请求已入簿，不得重复发布"));
        }
        Ok(CourtEvent::HumanTranslationApproved {
            approval: Box::new(approval),
        })
    }

    /// Approval receipt by its own request scope. The same candidate bytes may
    /// be approved again under a new request and must not reuse this identity.
    pub fn human_approval(
        &self,
        project: &str,
        block: &str,
        request_id: &str,
    ) -> Option<&HumanApproval> {
        self.human_approvals
            .get(project)?
            .get(block)?
            .get(request_id)
    }

    /// Approval receipts for one block, ordered by request ID, not by time.
    pub fn human_approvals<'a>(
        &'a self,
        project: &'a str,
        block: &'a str,
    ) -> impl Iterator<Item = &'a HumanApproval> + 'a {
        self.human_approvals
            .get(project)
            .and_then(|blocks| blocks.get(block))
            .into_iter()
            .flat_map(|requests| requests.values())
    }

    /// Bind selected references once, before the linked writer starts execution.
    pub fn propose_translation_references(
        &self,
        task: TaskId,
        binding: ReferenceBinding,
    ) -> Result<CourtEvent, GovernanceError> {
        self.check_translation_references(task, &binding)?;
        Ok(CourtEvent::TranslationReferencesBound { task, binding })
    }

    pub(crate) fn check_translation_references(
        &self,
        task: TaskId,
        binding: &ReferenceBinding,
    ) -> Result<(), GovernanceError> {
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        if entry.status != TaskStatus::Pending {
            return Err(invalid("references require a pending task"));
        }
        if !entry
            .translation
            .as_ref()
            .is_some_and(|link| link.kind.writes())
        {
            return Err(invalid("references require a linked translation writer"));
        }
        if entry.references.is_some() {
            return Err(invalid("translation references already bound"));
        }
        if !(1..=128).contains(&binding.submission.len())
            || !binding
                .submission
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(invalid("invalid reference submission ID"));
        }
        check_hash("reference request hash", &binding.request_hash)?;
        if binding.snapshot.is_empty() || binding.snapshot.len() > 32768 {
            return Err(invalid("reference snapshot must contain 1..32768 bytes"));
        }
        if self.tasks.values().any(|task| {
            task.references
                .as_ref()
                .is_some_and(|other| other.submission == binding.submission)
        }) {
            return Err(invalid("reference submission already bound"));
        }
        Ok(())
    }

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

    /// 人工定稿：必须引用本块已入簿的准奏回执；不改写当前来源，也不伪造模型任务。
    pub fn propose_human_translation_finalized(
        &self,
        finalization: HumanFinalization,
    ) -> Result<CourtEvent, GovernanceError> {
        let f = &finalization;
        check_project(&f.project)?;
        check_block(&f.block)?;
        check_request_id("人工送审请求标识", &f.submission_id)?;
        check_request_id("人工准奏请求标识", &f.approval_request_id)?;
        if !is_lower_hex(&f.approval_operation_id, 32) {
            return Err(invalid("人工准奏操作须为 32 位小写十六进制"));
        }
        check_hash("人工准奏事件摘要", &f.approval_event.hash)?;
        for (what, hash) in [
            ("候选摘要", &f.candidate_hash),
            ("原文摘要", &f.source_hash),
            ("朱批摘要", &f.zhupi_hash),
            ("目标摘要", &f.target_hash),
        ] {
            check_hash(what, hash)?;
        }
        check_target_format(&f.target_format)?;
        let approval = self
            .human_approval(&f.project, &f.block, &f.approval_request_id)
            .ok_or_else(|| invalid("人工定稿须引用本块已入簿的准奏回执"))?;
        if approval.operation_id != f.approval_operation_id
            || approval.request.submission_id != f.submission_id
        {
            return Err(invalid("人工定稿引用的准奏回执不符"));
        }
        Ok(CourtEvent::HumanTranslationFinalized {
            finalization: Box::new(finalization),
        })
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
