//! 诏令：下诏、入队、评定、准奏落盘与回报。

use std::io;
use std::path::{Path, PathBuf};
use ziyuan_core::{
    AgentId, CHANGELOG, Capability, CourtEvent, Department, Edict, FileChange, Seal, TaskId,
    Template, Threat, TokenUsage, Verdict,
};
use ziyuan_governance::{ChangeState, Court, GovernanceError, Suspicion, Task, TaskStatus};
use ziyuan_runtime::guard;
use ziyuan_storage::recovery::{
    ApplicationSource, EventBatch, FileState, RecoveryRecord, TargetState,
};
use ziyuan_storage::{EventStore, LockScope, RecordState, Workspace, WorkspaceError};

use crate::cabinet::precedent;
use crate::{AppError, Session, truncate};

/// 新的驿传幂等键（16 位十六进制）：`RandomState` 每进程随机播种，再混入诏令号与时刻。
fn new_op(task: TaskId) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(task.0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    hasher.write_u128(now);
    hasher.write_u32(std::process::id());
    format!("{:016x}", hasher.finish())
}

/// 各处改动的统一 diff（每段前后各留 3 行上下文），一处一份。段号跨文件连续编号、从 1 起，
/// 即逐段准奏所用的段号。
pub fn render_diffs(files: &[FileChange]) -> Vec<String> {
    let mut number = 0;
    files
        .iter()
        .map(|change| {
            let mut out = match &change.before {
                Some(_) => format!("--- a/{0}\n+++ b/{0}\n", change.path),
                None => format!("--- /dev/null\n+++ b/{}\n", change.path),
            };
            for hunk in change.hunks() {
                number += 1;
                out.push_str(&format!("@@ 第 {number} 段 · 原第 {} 行 @@\n", hunk.start));
                for (mark, text) in hunk.lines {
                    out.push_str(&format!("{mark}{text}\n"));
                }
            }
            out
        })
        .collect()
}

/// 准奏时驳回了哪些段（全部准奏为空串）。
pub fn skipped_note(skipped: &[u32]) -> String {
    if skipped.is_empty() {
        return String::new();
    }
    let list: Vec<_> = skipped.iter().map(u32::to_string).collect();
    format!("（驳回第 {} 段）", list.join("、"))
}

/// 修道期间司礼监代批回奏的批语。
pub const PROXY_REVIEW_NOTE: &str = "司礼监代批：照准";

/// 默认玉玺不额外显示，保持输出简洁。
pub fn seal_note(seal: &Seal) -> String {
    if *seal == Seal::default() {
        String::new()
    } else {
        format!("〔玉玺 {seal}〕")
    }
}

/// 截取前 `max` 个字符，超出时注明。
/// 礼部修 CHANGELOG 时的史官记。
pub const CHANGELOG_NOTE: &str =
    "史官记：洪武二年诏修《元史》，次年成书——前朝之事本朝修之，本版之事礼部记之。";

/// 礼部修 CHANGELOG 的诏令内容：体例、版本、提交摘要与最近的实录（至多 20 件）。
pub fn changelog_text(court: &Court, version: &str, commits: &[String]) -> String {
    let mut text = format!(
        "礼部修 CHANGELOG：在 {CHANGELOG} 顶部（若有总标题，则在其下）新增「## {version}」一节，\
         依下列提交与实录，分「新增」「改进」「修复」等小节，每条一行、写给使用者看，不写内部细节，\
         不要编造提交中没有的事。保留文件原有内容不动；文件不存在则新建，首行为「# Changelog」。\
         先用 read_file 读原文，再以 write_file 整体写回。\n\n提交（新者在前）：\n"
    );
    for c in commits.iter().take(200) {
        text.push_str(&format!("- {}\n", truncate(c.trim(), 200)));
    }
    let records = court.records();
    if !records.is_empty() {
        text.push_str("\n翰林院实录（最近的已结诏令，供参考）：\n");
        for r in records.iter().rev().take(20) {
            text.push_str(&format!("- {}\n", precedent(r)));
        }
    }
    text
}

/// 逐段取舍后实际写入的文件：`skipped` 为驳回的段号（从 1 起，跨文件连续编号）；
/// 取舍后与原文相同（整处驳回）的不列。
pub fn selected_files(files: &[FileChange], skipped: &[u32]) -> Vec<FileChange> {
    let mut first = 0;
    let mut selected = Vec::new();
    for file in files {
        let base = first;
        first += file.hunks().len();
        let after = file.select(|h| !skipped.contains(&((base + h + 1) as u32)));
        if after.as_ref() == file.before.as_ref() {
            continue;
        }
        selected.push(FileChange {
            path: file.path.clone(),
            before: file.before.clone(),
            after: after.unwrap_or_default(),
        });
    }
    selected
}

fn sha256(parts: &[&[u8]]) -> String {
    format!("sha256:{}", sha256_hex(parts))
}

/// 与 [`sha256`] 相同的摘要，但不带 `sha256:` 前缀。
fn sha256_hex(parts: &[&[u8]]) -> String {
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    for part in parts {
        ctx.update(part);
    }
    let hex: String = ctx
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    hex
}

/// 呈报时原文的内容版本（`sha256:` 加十六进制）；新建文件为 `None`。
pub fn base_hash(change: &FileChange) -> Option<String> {
    change.before.as_ref().map(|b| sha256(&[b.as_bytes()]))
}

/// 一份改动的版本：逐文件的路径、原文与改后内容（各带长度前缀）的 SHA-256。预览带上它，
/// 准奏时据以确认审阅的正是记下的这份改动。
pub fn changes_version(files: &[FileChange]) -> String {
    let len = |s: &str| (s.len() as u64).to_le_bytes();
    let mut parts: Vec<Vec<u8>> = Vec::new();
    for f in files {
        parts.push(len(&f.path).to_vec());
        parts.push(f.path.as_bytes().to_vec());
        match &f.before {
            Some(b) => {
                parts.push(vec![1]);
                parts.push(len(b).to_vec());
                parts.push(b.as_bytes().to_vec());
            }
            None => parts.push(vec![0]),
        }
        parts.push(len(&f.after).to_vec());
        parts.push(f.after.as_bytes().to_vec());
    }
    sha256(&parts.iter().map(Vec::as_slice).collect::<Vec<_>>())
}

/// 各文件的原文自呈报以来是否已被别处改动（已是改后内容的不算）；查不了的也算。
/// 有一处为真，准奏就会被拒。
pub fn stale_files(root: &Path, files: &[FileChange]) -> Vec<bool> {
    files
        .iter()
        .map(|f| {
            !matches!(
                ziyuan_runtime::tools::base_state(root, f),
                Ok(ziyuan_runtime::tools::BaseState::Original
                    | ziyuan_runtime::tools::BaseState::Written)
            )
        })
        .collect()
}

/// 送回来处的回奏正文（Markdown）：承办者、评定、复核与回奏全文。
pub fn reply_text(court: &Court, task: &Task) -> String {
    let id = task.edict.id.0;
    let mut out = format!("<!-- ziyuan:task:{id} -->\n**紫垣回奏**（诏令 #{id}）\n\n");
    if let TaskStatus::Completed { worker, summary } = &task.status {
        let name = court.agent(*worker).map_or("?", |a| a.name.as_str());
        let model = task
            .model
            .as_deref()
            .map(|m| format!("，模型 {m}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "- 承办：#{} {name}（{}{model}）\n",
            worker.0,
            task.edict.department.label()
        ));
        if let Some((verdict, note)) = &task.review {
            let note = if note.is_empty() {
                String::new()
            } else {
                format!("——{note}")
            };
            out.push_str(&format!("- 皇帝评定：{}{note}\n", verdict.label()));
        }
        if let Some(c) = &task.censure {
            out.push_str(&format!(
                "- 都察院：{}（{}）\n",
                c.verdict.label(),
                c.evidence
            ));
        }
        if let Some(changes) = &task.changes {
            let state = match (changes.state, &task.pull_request) {
                (ChangeState::Applied, Some(url)) => format!("已准奏，草稿 PR：{url}"),
                (ChangeState::Applied, None) => "已准奏落盘".into(),
                (ChangeState::Proposed, _) => "待准奏".into(),
                (ChangeState::Discarded, _) => "已驳回".into(),
            };
            out.push_str(&format!("- 文件改动：{state}\n"));
        }
        out.push_str(&format!("\n---\n\n{summary}\n"));
    }
    out
}

impl<S: EventStore> Session<S> {
    pub fn redraft(&mut self, task: TaskId, worker: AgentId, note: String) -> Result<(), AppError> {
        let note = if note.trim().is_empty() {
            "御笔改票".to_owned()
        } else {
            format!("御笔改票：{}", note.trim())
        };
        self.commit(|court| {
            Ok(vec![court.propose_draft(
                task,
                worker,
                note.clone(),
                None,
                TokenUsage::default(),
            )?])
        })?;
        Ok(())
    }

    /// 下诏入队。现行制度下无法路由（如职位空缺）时拒绝，不留记录。
    /// `seal` 是承办官员的权限令牌（玉玺），随诏令记入事件簿。
    pub fn issue(
        &mut self,
        department: Department,
        text: String,
        seal: Seal,
    ) -> Result<Edict, AppError> {
        let committed = self.commit(|court| {
            let edict = court.new_edict(department, text.clone(), seal.clone())?;
            court.route(&edict)?;
            Ok(vec![CourtEvent::EdictIssued(Box::new(edict))])
        })?;
        match committed.into_iter().next() {
            Some(CourtEvent::EdictIssued(edict)) => Ok(*edict),
            _ => unreachable!("issue 只产生 EdictIssued"),
        }
    }

    /// 礼部修 CHANGELOG：下一件礼部诏令（只入队），据 `commits`（提交摘要，新者在前）与翰林院实录
    /// 起草 `version` 一节。玉玺只授 read_file 与 write_file，范围只限 CHANGELOG.md；改动照常呈报，
    /// 须皇帝准奏才落盘。
    pub fn draft_changelog(
        &mut self,
        version: &str,
        commits: &[String],
    ) -> Result<Edict, AppError> {
        if commits.is_empty() {
            return Err(GovernanceError::Court("礼部不准：没有可记的提交".into()).into());
        }
        let text = changelog_text(&self.court, version, commits);
        let seal = Seal {
            capabilities: [Capability::ReadFile, Capability::WriteFile].into(),
            scope: vec![CHANGELOG.into()],
            mcp: Default::default(),
        };
        self.issue(Department::Rites, text, seal)
    }

    /// 翰林院存模板（同名覆盖）。
    pub fn save_template(&mut self, name: &str, template: Template) -> Result<(), AppError> {
        self.commit(|court| {
            Ok(vec![
                court.propose_template(name.to_owned(), template.clone())?,
            ])
        })?;
        Ok(())
    }

    /// 翰林院撤去自定的模板。
    pub fn remove_template(&mut self, name: &str) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_template_removal(name.to_owned())?]))?;
        Ok(())
    }

    /// 按模板「`name`」下诏：部门、内容与玉玺由调用方给出（通常取自模板，可改），
    /// 验收标准取当时的模板，执行时附在诏令之后，复核时交都察院逐条核验。
    pub fn issue_templated(
        &mut self,
        name: &str,
        department: Department,
        text: String,
        seal: Seal,
    ) -> Result<Edict, AppError> {
        let committed = self.commit(|court| {
            let edict = court.new_edict(department, text.clone(), seal.clone())?;
            court.route(&edict)?;
            let issued = CourtEvent::EdictIssued(Box::new(edict.clone()));
            let mut next = court.clone();
            next.apply(&issued)?;
            Ok(vec![issued, next.propose_template_use(edict.id, name)?])
        })?;
        match committed.into_iter().next() {
            Some(CourtEvent::EdictIssued(edict)) => Ok(*edict),
            _ => unreachable!("issue_templated 先记 EdictIssued"),
        }
    }

    /// 兵部驿传：外部事件（CI 失败、新 issue 等）入队为诏令，记下来处 `station`，可八百里加急。
    /// 玉玺由皇帝给定（通常为默认的只读）；入队后与亲下的诏令一样须批红、受度支约束。
    pub fn post(
        &mut self,
        department: Department,
        text: String,
        seal: Seal,
        station: &str,
        urgent: bool,
    ) -> Result<Edict, AppError> {
        let posted = self.post_inner(department, text, seal, station, urgent, false)?;
        Ok(posted.expect("post 不去重，总会入队"))
    }

    /// 同 [`post`](Self::post)，但已有来自同一驿站 `station` 的诏令时不入队，返回 `None`。
    /// 去重在提交内检查，写入冲突重试时基于最新状态重新检查，因此并发入队也只入一次。
    pub fn post_unique(
        &mut self,
        department: Department,
        text: String,
        seal: Seal,
        station: &str,
        urgent: bool,
    ) -> Result<Option<Edict>, AppError> {
        self.post_inner(department, text, seal, station, urgent, true)
    }

    fn post_inner(
        &mut self,
        department: Department,
        text: String,
        seal: Seal,
        station: &str,
        urgent: bool,
        unique: bool,
    ) -> Result<Option<Edict>, AppError> {
        let committed = self.commit(|court| {
            if unique && court.tasks().any(|t| t.courier.as_deref() == Some(station)) {
                return Ok(Vec::new());
            }
            let edict = court.new_edict(department, text.clone(), seal.clone())?;
            court.route(&edict)?;
            let issued = CourtEvent::EdictIssued(Box::new(edict.clone()));
            // 驿传与加急基于入队后的状态校验。
            let mut next = court.clone();
            next.apply(&issued)?;
            let mut events = vec![issued, next.propose_courier(edict.id, station.to_owned())?];
            if urgent {
                events.push(next.propose_expedite(edict.id)?);
            }
            // 驿传内容来自朝外：锦衣卫入队即查，检出记下，修道期间司礼监不代批红。
            events.extend(guard::scan(&edict.text, &Threat::ALL).into_iter().map(|f| {
                CourtEvent::SuspicionRaised {
                    task: edict.id,
                    source: Suspicion::COURIER.into(),
                    threat: f.threat,
                    excerpt: f.excerpt,
                }
            }));
            Ok(events)
        })?;
        match committed.into_iter().next() {
            None => Ok(None),
            Some(CourtEvent::EdictIssued(edict)) => Ok(Some(*edict)),
            _ => unreachable!("post 先记 EdictIssued"),
        }
    }

    /// 八百里加急：未结束的诏令插队先行，执行时限放宽为两倍。
    pub fn expedite(&mut self, task: TaskId) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_expedite(task)?]))?;
        Ok(())
    }

    /// 皇帝评定一件已完成的诏令。吏部考绩只采信这些评定。
    pub fn review(&mut self, task: TaskId, verdict: Verdict, note: String) -> Result<(), AppError> {
        self.commit(|court| {
            if court.in_retreat() {
                return Err(GovernanceError::InRetreat);
            }
            Ok(vec![court.propose_review(task, verdict, note.clone())?])
        })?;
        Ok(())
    }

    /// 准奏：把诏令呈报的改动写入 `root` 下的项目，再记入事件簿。`skipped` 为逐段准奏时
    /// 驳回的段号（见 [`render_diffs`]），空即全部准奏。写入前逐一核对文件自呈报以来
    /// 未被改动，否则一处也不写、不留记录。
    ///
    /// 写入是一笔受管事务：先取工作区锁、推进世代号、写下「已发布」的恢复记录，再写文件、
    /// 记事件，最后收尾（见 [`Session::write_transaction`]）。真的动到文件之后失败时，
    /// 记录与奇数世代号留在盘上，之后的写入一律被拒，须先经 `ziyuan text recovery` 收尾。
    pub fn apply_changes(
        &mut self,
        task: TaskId,
        root: &Path,
        skipped: Vec<u32>,
    ) -> Result<(), AppError> {
        self.apply_reviewed(task, root, skipped, None)
    }

    /// 同 [`apply_changes`](Self::apply_changes)；`version` 为审阅时预览所示的
    /// [`changes_version`]，与记下的改动不符（如页面来自别的项目或旧的事件簿）即拒绝，一处也不写。
    pub fn apply_reviewed(
        &mut self,
        task: TaskId,
        root: &Path,
        skipped: Vec<u32>,
        version: Option<&str>,
    ) -> Result<(), AppError> {
        self.reload()?;
        let event = self.court.propose_application(task, skipped)?;
        if let Some(seen) = version {
            let files = &self.court.task(task).expect("已确认存在").changes;
            let files = &files.as_ref().expect("已确认有待决改动").files;
            if changes_version(files) != seen {
                return Err(AppError::Apply(format!(
                    "诏令 #{} 的改动与审阅时所见的版本不符，请刷新后重新审阅",
                    task.0
                )));
            }
        }
        let CourtEvent::ChangesApplied { skipped, .. } = &event else {
            unreachable!("准奏只产生 ChangesApplied");
        };
        let skipped = skipped.clone();
        let entry = self.court.task(task).expect("已确认存在");
        let files = &entry.changes.as_ref().expect("已确认有待决改动").files;
        let selected = selected_files(files, &skipped);
        if let Some(link) = entry.translation.as_ref().filter(|l| l.kind.writes()) {
            crate::translation::check_gate(root, &self.court, entry, link, &selected)?;
        }
        // 事件的条数与内容取自既有的事件簿：追记后应得的条数即已知条数加这一条。
        let known = self.len;
        self.write_transaction(root, &selected, task, skipped, known, event)?;
        Ok(())
    }

    pub fn check_reply(&mut self, task: TaskId, pull_request: bool) -> Result<(), AppError> {
        self.reload()?;
        self.court
            .propose_courier_reply(task, pull_request, "-")
            .map_err(AppError::from)
    }

    /// 记下驿站已把回奏（或草稿 PR）送回来处；`op` 为外发前所记意图的幂等键。
    pub fn record_reply(
        &mut self,
        task: TaskId,
        pull_request: bool,
        url: String,
        op: Option<String>,
    ) -> Result<(), AppError> {
        self.commit(|court| {
            court.propose_courier_reply(task, pull_request, &url)?;
            court.check_courier_op(task, pull_request, op.as_deref())?;
            Ok(vec![CourtEvent::CourierReplied {
                task,
                pull_request,
                url: url.clone(),
                op: op.clone(),
            }])
        })?;
        Ok(())
    }

    /// 驿站外发前的幂等键：有待确认的意图则沿用（续作），否则新起一个并先记入事件簿。
    pub fn courier_dispatch(
        &mut self,
        task: TaskId,
        pull_request: bool,
    ) -> Result<String, AppError> {
        self.check_reply(task, pull_request)?;
        if let Some(op) = self.court.courier_op(task, pull_request) {
            return Ok(op.to_owned());
        }
        let op = new_op(task);
        self.commit(|court| {
            Ok(vec![court.propose_courier_dispatch(
                task,
                pull_request,
                &op,
            )?])
        })?;
        Ok(op)
    }

    /// 驳回诏令呈报的改动：不写入项目。
    pub fn discard_changes(&mut self, task: TaskId) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_changes_decision(task, false)?]))?;
        Ok(())
    }

    /// 撤销尚未结束的诏令。
    ///
    /// 若该诏令正由另一进程执行，执行不会被打断，但其结果会被作废、不再记录。
    pub fn cancel(&mut self, task: TaskId) -> Result<(), AppError> {
        self.commit(|court| {
            let cancel = court.propose_cancellation(task)?;
            // 撤销已分办的原诏令时，未结束的子诏令一并撤销。
            let mut events: Vec<_> = court
                .task(task)
                .and_then(|t| t.split.as_ref())
                .map_or(&[][..], |s| &s.subtasks)
                .iter()
                .filter(|id| court.task(**id).is_some_and(|t| t.status.is_open()))
                .map(|&id| CourtEvent::TaskCancelled { task: id })
                .collect();
            events.push(cancel);
            Ok(events)
        })?;
        Ok(())
    }
}

/// 新的恢复操作编号（32 位小写十六进制）：两次随机哈希，共 128 位。
fn new_operation() -> String {
    use std::hash::{BuildHasher, Hasher};
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let half = |salt: u64| {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(salt);
        hasher.write_u128(stamp);
        hasher.write_u32(std::process::id());
        hasher.finish()
    };
    format!("{:016x}{:016x}", half(0), half(1))
}

/// 记录里存的工作区路径：Windows 上 canonical 的根带 `\\?\` 前缀，恢复记录的编解码器不认它，
/// 这里去掉前缀。`Workspace` 比对记录时两种写法都接受。
fn record_path(root: &Path) -> PathBuf {
    match root.to_string_lossy().strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => root.to_path_buf(),
    }
}

/// 文件当前的内容哈希；不存在为 [`FileState::Absent`]。
fn digest_of(path: &Path) -> io::Result<FileState> {
    Ok(match std::fs::read(path) {
        Ok(bytes) => FileState::Present(sha256_hex(&[&bytes])),
        Err(e) if e.kind() == io::ErrorKind::NotFound => FileState::Absent,
        Err(e) => return Err(e),
    })
}

/// 之后各项写入拒绝的说明：指的是同一件事，给出可执行的下一步。
fn blocked(requirement: String) -> String {
    format!("此工作区上次受管写入未收尾（{requirement}），已拒绝写入：一处也不写、不留记录")
}

/// 工作区控制文件的错误一律转成可执行的 [`AppError::Apply`]，不回显内部路径细节。
fn workspace_error(error: WorkspaceError) -> AppError {
    AppError::Apply(match error {
        WorkspaceError::Timeout { .. } => "另一进程正在写入此工作区，请稍候重试".to_string(),
        other => format!("工作区不可用：{other}"),
    })
}

impl<S: EventStore> Session<S> {
    /// 本次准奏要落的文件在磁盘上的现状与改后内容：逐文件的路径、前后哈希。
    /// 已等于改后内容的文件视为已写（`apply_changes` 同样跳过），不必再写，也不入记录。
    fn target_states(
        &self,
        workspace: &Workspace,
        selected: &[FileChange],
    ) -> Result<Vec<TargetState>, AppError> {
        let failed = |e: io::Error| AppError::Apply(format!("无法读取待写的文件：{e}"));
        let mut targets = Vec::with_capacity(selected.len());
        for change in selected {
            let path = workspace.root().join(Path::new(&change.path));
            let before = digest_of(&path).map_err(failed)?;
            let after = FileState::Present(sha256_hex(&[change.after.as_bytes()]));
            if before != after {
                targets.push(TargetState {
                    path: change.path.clone(),
                    before,
                    after,
                });
            }
        }
        Ok(targets)
    }

    /// 本次准奏的恢复记录：本次写入的文件前后哈希，以及追加事件后事件簿应有的样子。
    fn recovery_record(
        &self,
        workspace: &Workspace,
        selected: &[FileChange],
        task: TaskId,
        skipped: &[u32],
        known: usize,
        event: &CourtEvent,
    ) -> Result<RecoveryRecord, AppError> {
        let generation = workspace.generation().map_err(workspace_error)?;
        let targets = self.target_states(workspace, selected)?;
        // 记录所覆盖的是追加之后事件簿的前若干条：基础条数取事件簿本身报出的已知条数；
        // 报不出时退而用本次准奏开头的条数 `known`（`reload` 刚读过，二者通常相同）。
        let base = self
            .store
            .known_events()
            .map_err(|e| AppError::Apply(e.to_string()))?
            .unwrap_or(known);
        // 这一批还可能多记一条时辰（见 `commit`），故按可能的最长批次算。
        let expected_events = base as u64 + 2;
        // 文件事件簿报得出前至多 `expected_events` 条的摘要与整本长度；内存事件簿报不出，
        // 此时记 0 与本次已知历史的摘要（`history` 与事件簿前 `base` 条一致，见 `Session::history`），
        // 记录仍自洽，只是不足以核对文件。
        let history_sha256 = match self
            .store
            .history_sha256_at_most(expected_events as usize)
            .map_err(|e| AppError::Apply(e.to_string()))?
        {
            Some(sha) => sha,
            None => {
                // 与文件事件簿的摘要口径一致：每行以换行符结尾。
                let history = self.history()?;
                let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
                for recorded in history.iter().take(base) {
                    ctx.update(ziyuan_storage::encode_event_line(recorded).as_bytes());
                    ctx.update(b"\n");
                }
                ctx.finish()
                    .as_ref()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect()
            }
        };
        let expected_bytes = self
            .store
            .history_bytes()
            .map_err(|e| AppError::Apply(e.to_string()))?
            .unwrap_or(0);
        let mut encoded_events = Vec::with_capacity(2);
        if self.store.stamps_time() {
            let at = crate::time::now();
            if self.clock.is_none_or(|t| t / 60 != at / 60) {
                encoded_events.push(ziyuan_storage::encode_event_line(&CourtEvent::Clock(at)));
            }
        }
        encoded_events.push(ziyuan_storage::encode_event_line(event));
        Ok(RecoveryRecord {
            operation_id: new_operation(),
            workspace: record_path(workspace.root()),
            generation_before: generation,
            generation_after: generation + 2,
            state: RecordState::Published,
            application: ApplicationSource {
                task: task.0,
                changes_version: "v1".into(),
                skipped: skipped.to_vec(),
            },
            events: EventBatch {
                expected_events,
                expected_bytes,
                history_sha256,
                encoded_events,
            },
            targets,
        })
    }

    /// 一笔受管事务的写入：工作区锁 → 世代号 → 恢复记录 → 写文件 → 记事件 → 收尾。
    ///
    /// 顺序是有意为之：恢复记录先于任何文件落地，所以只要文件可能被写过，盘上就有一份
    /// 带校验和、写着前后内容的记录可供核对。写文件或记事件失败时**不作任何修补**——
    /// 「已发布」的记录与奇数世代号原样留下，之后的写入一律被拒（见 `ensure_writable`），
    /// 由 `ziyuan text recovery` 收尾。反之，在还没动到任何文件之前就能断定的失败
    /// （工作区不可写、先前供改动、路径越界）一律在此之前返回：世代号不推进，也不留记录，
    /// 免得一次本来就没写的准奏把工作区锁死。
    fn write_transaction(
        &mut self,
        root: &Path,
        selected: &[FileChange],
        task: TaskId,
        skipped: Vec<u32>,
        known: usize,
        event: CourtEvent,
    ) -> Result<(), AppError> {
        // 根目录尚不存在时，既有行为会在写文件时建出目录；工作区须先有根才能打开。
        if !root.is_dir() {
            std::fs::create_dir_all(root).map_err(|e| AppError::Apply(e.to_string()))?;
        }
        // 工作区不可用（如给的不是目录）时，沿用落盘失败的说法。
        let workspace = Workspace::open(root).map_err(|e| AppError::Apply(e.to_string()))?;
        // 锁外先查一次：已有未收尾的写入时立刻拒绝，不必等锁；
        // 锁内再查一次：锁外到锁内之间记录或世代号可能已被别的进程改动。
        let unusable = |e: WorkspaceError| AppError::Apply(blocked(e.to_string()));
        workspace.ensure_writable().map_err(unusable)?;
        let mut guard = workspace
            .lock(LockScope::Workspace)
            .map_err(workspace_error)?;
        workspace.ensure_writable().map_err(unusable)?;

        let record = self.recovery_record(&workspace, selected, task, &skipped, known, &event)?;
        let operation = record.operation_id.clone();
        // 写前先按落盘方的规矩核一遍（自呈报以来有没有被改动、路径是否越界）：这一关不过就
        // 一处也不会写，因此连世代号都不必推进，更不留恢复记录——不会因此把工作区卡住。
        // 真正的写入仍由下面的 `apply_changes` 负责，这里只是让必然的失败发生在发布记录之前。
        let preflight = selected.iter().try_for_each(|change| {
            match ziyuan_runtime::tools::base_state(root, change) {
                Ok(ziyuan_runtime::tools::BaseState::Changed) => {
                    Err(format!("{} 自呈报以来已被改动，拒绝落盘", change.path))
                }
                Ok(_) => Ok(()),
                Err(e) => Err(e.to_string()),
            }
        });
        if let Err(reason) = preflight {
            return Err(AppError::Apply(reason));
        }
        // 清掉上次崩溃留下的「已收尾」记录（世代号已回偶数，不挡写入），免得发布时相撞。
        let swept = workspace
            .pending_record()
            .map_err(workspace_error)?
            .is_some();
        if swept {
            guard.remove_record().map_err(workspace_error)?;
        }
        // 先推进世代号，再发布记录：崩溃留下的定是「记录已发布、世代号为奇数」。
        guard.reserve().map_err(workspace_error)?;
        if let Err(error) = guard.publish(&record) {
            // 发布失败：尽力把世代号推回偶数，免得一个没写成的记录把工作区卡死。
            let _ = guard.finish();
            return Err(AppError::Apply(match swept {
                // 已清掉「已收尾」的记录：上一次收尾不完整，须核对后再写。
                true => format!(
                    "收尾上次受管写入后发布本次恢复记录失败（{error}），已恢复世代号；请核对项目文件后重试"
                ),
                false => format!("无法写入恢复记录：{error}"),
            }));
        }

        // 从这里起，盘上已有「已发布」的记录；真的动到文件之后失败（写不进去、记事件不成）
        // 即原样留下，不作修补：下一次写入会因之被拒，须先经 `ziyuan text recovery` 核对并收尾。
        // 守卫随函数结束而释放，世代号保持奇数。
        ziyuan_runtime::tools::apply_changes(root, selected)
            .map_err(|e| AppError::Apply(e.to_string()))?;
        self.commit(|_| Ok(vec![event.clone()]))?;

        // 收尾：记录转为「已完成」、世代号回偶数、删掉记录。
        guard.complete(&operation).map_err(workspace_error)?;
        guard.finish().map_err(workspace_error)?;
        guard.remove_record().map_err(workspace_error)?;
        Ok(())
    }
}
