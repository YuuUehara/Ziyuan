//! 兵部驿站的外界往来：GitHub 入队与回报、礼部修 CHANGELOG 所据的 `git log`。
//! CLI 与 Web 共用；GitHub 凭据只在 [`GitHub`] 中，不进事件簿与任何模型提示。
use crate::{AppError, Session, commit_message, reply_text, selected_files};
use std::path::Path;
use ziyuan_core::{Department, Edict, Seal, TaskId};
use ziyuan_github::{GitHub, intake, parse_station};
use ziyuan_storage::EventStore;

fn external(e: impl std::fmt::Display) -> AppError {
    AppError::External(e.to_string())
}

/// `github pull` 的结果：带标签的 issue/PR 件数与新入队的诏令（及其驿站名、密报条数）。
#[derive(Debug)]
pub struct PullReport {
    pub issues: usize,
    pub queued: Vec<(Edict, String, usize)>,
}

impl<S: EventStore> Session<S> {
    /// 带 `label` 的 issue/PR 及成员的 `/ziyuan` 评论入队（交给 `department`，盖 `seal`）；
    /// 已入队的不再入队。要让来件能开草稿 PR，须皇帝在此授工部写权。
    pub fn github_pull(
        &mut self,
        hub: &GitHub,
        label: &str,
        department: Department,
        seal: &Seal,
    ) -> Result<PullReport, AppError> {
        let issues = hub.labeled(label).map_err(external)?;
        let mut queued = Vec::new();
        for issue in &issues {
            let comments = hub.comments(issue.number).map_err(external)?;
            for d in intake(hub.repo(), issue, &comments) {
                let Some(edict) =
                    self.post_unique(department, d.text, seal.clone(), &d.station, false)?
                else {
                    continue;
                };
                let flags = self
                    .court()
                    .task(edict.id)
                    .map_or(0, |t| t.suspicions.len());
                queued.push((edict, d.station, flags));
            }
        }
        Ok(PullReport {
            issues: issues.len(),
            queued,
        })
    }

    /// 诏令来自本仓库的哪件 issue/PR。
    fn github_number(&self, hub: &GitHub, task: TaskId) -> Result<u64, AppError> {
        let station = self
            .court()
            .task(task)
            .and_then(|t| t.courier.clone())
            .unwrap_or_default();
        match parse_station(&station) {
            Some((repo, number)) if repo == hub.repo() => Ok(number),
            Some((repo, _)) => Err(external(format!(
                "诏令 #{} 来自 {repo}，与 ZIYUAN_GITHUB_REPO 不符",
                task.0
            ))),
            None => Err(external(format!(
                "诏令 #{} 来自「{station}」，不是 GitHub 驿站",
                task.0
            ))),
        }
    }

    /// 现在可以写回 GitHub 评论的诏令（本仓库驿传入队、已评定、无未平反密报、尚未回报）。
    pub fn github_replyable(&mut self, hub: &GitHub) -> Result<Vec<TaskId>, AppError> {
        self.reload()?;
        let ids: Vec<TaskId> = self.court().tasks().map(|t| t.edict.id).collect();
        Ok(ids
            .into_iter()
            .filter(|id| {
                self.court().propose_courier_reply(*id, false, "-").is_ok()
                    && self.github_number(hub, *id).is_ok()
            })
            .collect())
    }

    /// 把回奏写回来处的评论，记入事件簿；返回 issue/PR 编号与评论网址。外发前先记意图
    /// （幂等键），上次发出而未及记账的，这次认出已发的评论只补记，不重发。
    pub fn github_reply(&mut self, hub: &GitHub, task: TaskId) -> Result<(u64, String), AppError> {
        self.check_reply(task, false)?;
        let number = self.github_number(hub, task)?;
        let op = self.courier_dispatch(task, false)?;
        let entry = self.court().task(task).expect("已校验");
        let url = hub
            .comment(number, &reply_text(self.court(), entry), &op)
            .map_err(external)?;
        self.record_reply(task, false, url.clone(), Some(op))?;
        Ok((number, url))
    }

    /// 把准奏落盘的改动在远端开为草稿 PR，记入事件簿；返回 PR 网址。外发前先记意图
    /// （幂等键）；上次中断的，按同一键接着建分支或开 PR，已开的只补记。
    pub fn github_pull_request(&mut self, hub: &GitHub, task: TaskId) -> Result<String, AppError> {
        self.check_reply(task, true)?;
        let number = self.github_number(hub, task)?;
        let op = self.courier_dispatch(task, true)?;
        let entry = self.court().task(task).expect("已校验");
        let changes = entry.changes.as_ref().expect("已校验有准奏的改动");
        let files = selected_files(&changes.files, &changes.skipped);
        // 标题即提交说明的首行；本地以 `apply --commit` 提交过的，两者说明相同。
        let message = commit_message(self.court(), entry);
        let title = message.lines().next().unwrap_or_default().to_owned();
        let body = format!(
            "Refs #{number}\n\n由紫垣开出的草稿 PR：改动已经皇帝准奏，共 {} 个文件。\n\n{}",
            files.len(),
            reply_text(self.court(), entry)
        );
        let url = hub
            .draft_pull_request(
                &format!("ziyuan/task-{}", task.0),
                &title,
                &body,
                &message,
                &files,
                &op,
            )
            .map_err(external)?;
        self.record_reply(task, true, url.clone(), Some(op))?;
        Ok(url)
    }
}

/// 礼部修 CHANGELOG 所据的提交（`root` 下 `git log`，新者在前，至多 200 条，不含合并提交）
/// 及所取范围。未给起点时取最近的 tag 以来；没有 tag 则取全部。
pub fn changelog_commits(
    root: &Path,
    since: Option<String>,
) -> Result<(String, Vec<String>), AppError> {
    let git = |args: &[&str]| -> Result<String, AppError> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .map_err(|e| external(format!("无法运行 git：{e}")))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(external(format!(
                "git {} 失败：{}",
                args.join(" "),
                err.trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let since = match since {
        Some(since) => Some(since),
        None => git(&["describe", "--tags", "--abbrev=0"])
            .ok()
            .map(|t| t.trim().to_owned()),
    };
    let range = since.map_or("HEAD".to_owned(), |s| format!("{s}..HEAD"));
    let log = git(&["log", "--no-merges", "-n", "200", "--format=%h %s", &range])?;
    let commits: Vec<String> = log
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect();
    if commits.is_empty() {
        return Err(external(format!("{range} 没有可记的提交")));
    }
    Ok((range, commits))
}
