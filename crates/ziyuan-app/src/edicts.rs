//! 诏令：下诏、入队、评定、准奏落盘与回报。

use std::path::Path;
use ziyuan_core::{
    AgentId, CHANGELOG, Capability, CourtEvent, Department, Edict, FileChange, Seal, TaskId,
    Template, Threat, TokenUsage, Verdict,
};
use ziyuan_governance::{ChangeState, Court, GovernanceError, Suspicion, Task, TaskStatus};
use ziyuan_runtime::guard;
use ziyuan_storage::EventStore;

use crate::cabinet::precedent;
use crate::{AppError, Session, truncate};

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
        let committed = self.commit(|court| {
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
            Some(CourtEvent::EdictIssued(edict)) => Ok(*edict),
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
    pub fn apply_changes(
        &mut self,
        task: TaskId,
        root: &Path,
        skipped: Vec<u32>,
    ) -> Result<(), AppError> {
        self.reload()?;
        let event = self.court.propose_application(task, skipped)?;
        let CourtEvent::ChangesApplied { skipped, .. } = &event else {
            unreachable!("准奏只产生 ChangesApplied");
        };
        let files = &self.court.task(task).expect("已确认存在");
        let files = &files.changes.as_ref().expect("已确认有待决改动").files;
        let selected = selected_files(files, skipped);
        ziyuan_runtime::tools::apply_changes(root, &selected)
            .map_err(|e| AppError::Apply(e.to_string()))?;
        self.commit(|_| Ok(vec![event.clone()]))?;
        Ok(())
    }

    /// 驿站回报前的校验（见 `Court::propose_courier_reply`），以免先发到外界、再记不进事件簿。
    pub fn check_reply(&mut self, task: TaskId, pull_request: bool) -> Result<(), AppError> {
        self.reload()?;
        self.court
            .propose_courier_reply(task, pull_request, "-")
            .map_err(AppError::from)
    }

    /// 记下驿站已把回奏（或草稿 PR）送回来处。
    pub fn record_reply(
        &mut self,
        task: TaskId,
        pull_request: bool,
        url: String,
    ) -> Result<(), AppError> {
        self.commit(|court| {
            court.propose_courier_reply(task, pull_request, &url)?;
            Ok(vec![CourtEvent::CourierReplied {
                task,
                pull_request,
                url: url.clone(),
            }])
        })?;
        Ok(())
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
