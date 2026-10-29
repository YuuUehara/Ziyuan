//! 准奏落盘进版本库：把一件诏令落盘的文件记为一个 jj 或 git 提交。
//! 只提交这件诏令改动的文件，工作区里别的改动原样留着。
use crate::{AppError, Session, selected_files};
use std::path::Path;
use std::process::Command;
use ziyuan_core::TaskId;
use ziyuan_governance::{ChangeState, Court, Task};
use ziyuan_storage::EventStore;

/// 记下的提交：所用的版本库与提交 ID（短）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VcsCommit {
    /// `jj` 或 `git`。
    pub vcs: &'static str,
    pub id: String,
}

/// 提交与草稿 PR 共用的说明：首行与 PR 标题相同，末尾的 `Ziyuan-Edict` 让两者对得上。
pub fn commit_message(court: &Court, task: &Task) -> String {
    let id = task.edict.id.0;
    let first: String = task
        .edict
        .text
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(60)
        .collect();
    let mut message = format!("紫垣诏令 #{id}：{first}\n\n");
    if let Some(agent) = task.handler().and_then(|a| court.agent(a)) {
        let office = court
            .office_of(agent.id)
            .map_or(String::new(), |o| format!("（{}）", o.label()));
        message.push_str(&format!("承办：#{} {}{office}\n", agent.id.0, agent.name));
    }
    if let Some(changes) = &task.changes {
        let files = selected_files(&changes.files, &changes.skipped).len();
        let skipped = match changes.skipped.as_slice() {
            [] => String::new(),
            list => format!(
                "，驳回第 {} 段",
                list.iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join("、")
            ),
        };
        message.push_str(&format!("经皇帝准奏：{files} 个文件{skipped}\n"));
    }
    message.push_str(&format!("\nZiyuan-Edict: {id}\n"));
    message
}

fn run(root: &Path, program: &str, args: &[&str]) -> Result<String, AppError> {
    let out = Command::new(program)
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|e| AppError::External(format!("无法运行 {program}：{e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(AppError::External(format!(
            "{program} {} 失败：{}",
            args.first().copied().unwrap_or_default(),
            err.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// jj 文件集中的一个确切路径（相对当前目录），引号与反斜杠转义。
fn fileset(path: &str) -> String {
    format!(
        "file:\"{}\"",
        path.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

/// 往回查找的提交数：诏令准奏后通常随即提交，不必翻遍整个历史。
const LOOKBACK: &str = "200";
const END: &str = "--ziyuan-end--";

/// `log` 输出（每条为短 ID、说明、`END` 行）中最近一条带 `Ziyuan-Edict: {edict}` 尾注的提交。
fn find_trailer(log: &str, edict: u64) -> Option<String> {
    let trailer = format!("Ziyuan-Edict: {edict}");
    log.split(END).find_map(|entry| {
        let mut lines = entry.trim_start_matches(['\r', '\n']).lines();
        let id = lines.next()?.trim();
        (!id.is_empty() && lines.any(|l| l.trim() == trailer)).then(|| id.to_owned())
    })
}

/// 近来已为这件诏令记下的提交（中断后重试时不再重复提交）。查不到或查询出错都当作没有。
pub fn find_edict_commit(root: &Path, edict: u64) -> Option<VcsCommit> {
    if run(root, "jj", &["root"]).is_ok() {
        let template = format!("commit_id.short(12) ++ \"\\n\" ++ description ++ \"\\n{END}\\n\"");
        let revset = format!("ancestors(@, {LOOKBACK})");
        let log = run(
            root,
            "jj",
            &["log", "-r", &revset, "--no-graph", "-T", &template],
        )
        .ok()?;
        return find_trailer(&log, edict).map(|id| VcsCommit { vcs: "jj", id });
    }
    let format = format!("--format=%h%n%B%n{END}");
    let log = run(
        root,
        "git",
        &["log", "-n", LOOKBACK, "--abbrev=12", &format],
    )
    .ok()?;
    find_trailer(&log, edict).map(|id| VcsCommit { vcs: "git", id })
}

/// 在 `root` 所在的版本库里只提交 `paths`（相对 `root`）：有 jj 用 jj，否则用 git。
pub fn commit_files(root: &Path, message: &str, paths: &[String]) -> Result<VcsCommit, AppError> {
    if paths.is_empty() {
        return Err(AppError::Apply("没有落盘的文件可提交".into()));
    }
    if run(root, "jj", &["root"]).is_ok() {
        let mut args = vec![
            "commit".to_owned(),
            "-m".into(),
            message.into(),
            "--".into(),
        ];
        args.extend(paths.iter().map(|p| fileset(p)));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        run(root, "jj", &args)?;
        let id = run(
            root,
            "jj",
            &["log", "-r", "@-", "--no-graph", "-T", "commit_id.short(12)"],
        )?;
        return Ok(VcsCommit { vcs: "jj", id });
    }
    if run(root, "git", &["rev-parse", "--show-toplevel"]).is_err() {
        return Err(AppError::External(
            "项目不在 jj 或 git 版本库中，无从提交".into(),
        ));
    }
    let mut add = vec!["add", "--"];
    add.extend(paths.iter().map(String::as_str));
    run(root, "git", &add)?;
    // `--only`：只提交这些文件，别的已暂存的改动留在暂存区。
    let mut commit = vec!["commit", "--only", "-m", message, "--"];
    commit.extend(paths.iter().map(String::as_str));
    run(root, "git", &commit)?;
    let id = run(root, "git", &["rev-parse", "--short=12", "HEAD"])?;
    Ok(VcsCommit { vcs: "git", id })
}

impl<S: EventStore> Session<S> {
    /// 把已准奏落盘的诏令改动记为一个提交（只含其落盘的文件）。不记入事件簿：
    /// 提交说明末尾的 `Ziyuan-Edict` 足以对应，重试时也凭它认出已有的提交、不再重复提交。
    pub fn commit_applied(&mut self, task: TaskId, root: &Path) -> Result<VcsCommit, AppError> {
        self.reload()?;
        let entry = self
            .court()
            .task(task)
            .ok_or(ziyuan_governance::GovernanceError::UnknownEdict(task))?;
        let changes = entry
            .changes
            .as_ref()
            .filter(|c| c.state == ChangeState::Applied)
            .ok_or_else(|| AppError::Apply(format!("诏令 #{} 没有已准奏落盘的改动", task.0)))?;
        let paths: Vec<String> = selected_files(&changes.files, &changes.skipped)
            .into_iter()
            .map(|f| f.path)
            .collect();
        if let Some(made) = find_edict_commit(root, task.0) {
            return Ok(made);
        }
        commit_files(root, &commit_message(self.court(), entry), &paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailer_lookup_takes_the_newest_exact_match() {
        let log = format!(
            "aaa\n说明\n\nZiyuan-Edict: 12\n{END}\nbbb\n说明\n\nZiyuan-Edict: 1\n{END}\nccc\nZiyuan-Edict: 1\n{END}\n"
        );
        assert_eq!(find_trailer(&log, 1).as_deref(), Some("bbb"));
        assert_eq!(find_trailer(&log, 12).as_deref(), Some("aaa"));
        assert_eq!(find_trailer(&log, 2), None);
        assert_eq!(find_trailer("", 1), None);
    }

    /// jj 版本库（与 git 共存）：只提交所列文件，别的改动留在新的工作副本里。没装 jj 时跳过。
    #[test]
    fn jj_commits_only_the_listed_files() {
        if run(&std::env::temp_dir(), "jj", &["--version"]).is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!("ziyuan-vcs-jj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("docs")).unwrap();
        run(&root, "jj", &["git", "init", "--colocate"]).unwrap();
        run(
            &root,
            "jj",
            &["config", "set", "--repo", "user.name", "紫垣测试"],
        )
        .unwrap();
        run(
            &root,
            "jj",
            &[
                "config",
                "set",
                "--repo",
                "user.email",
                "test@ziyuan.invalid",
            ],
        )
        .unwrap();
        std::fs::write(root.join("docs/新 文.md"), "准\n").unwrap();
        std::fs::write(root.join("other.txt"), "别的\n").unwrap();
        let made = commit_files(
            &root,
            "紫垣诏令 #7：试\n\nZiyuan-Edict: 7\n",
            &["docs/新 文.md".into()],
        )
        .unwrap();
        assert_eq!(made.vcs, "jj");
        // jj 在 Windows 上可能以反斜杠显示路径（0.45.1 实测），归一化后再断言。
        let files = |rev: &str| {
            run(&root, "jj", &["diff", "-r", rev, "--name-only"])
                .unwrap()
                .replace('\\', "/")
        };
        assert_eq!(files("@-"), "docs/新 文.md");
        assert_eq!(files("@"), "other.txt");
        let message = run(
            &root,
            "jj",
            &["log", "-r", "@-", "--no-graph", "-T", "description"],
        )
        .unwrap();
        assert!(message.ends_with("Ziyuan-Edict: 7"), "{message}");
        assert_eq!(find_edict_commit(&root, 7), Some(made));
        assert_eq!(find_edict_commit(&root, 77), None);
        assert!(commit_files(&root, "x", &[]).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
