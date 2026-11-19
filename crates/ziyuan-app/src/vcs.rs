//! 准奏落盘进版本库：把一件诏令落盘的文件记为一个 jj 或 git 提交。
//! 只提交这件诏令改动的文件，工作区里别的改动原样留着。
use crate::{AppError, Session, selected_files};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// 控制并发写诏令是否启用临时 jj workspace 隔离：`auto`（默认，>=2 并发写诏令启用）、`always`、`off`。
pub const JJ_WORKSPACE_ENV: &str = "ZIYUAN_JJ_WORKSPACE";

const EPHEMERAL_WS_PREFIX: &str = "ziyuan-ws-";
static JJ_WORKSPACE_LOCK: Mutex<()> = Mutex::new(());
static JJ_WORKSPACE_SEQ: AtomicU64 = AtomicU64::new(0);

/// 并发写诏令的 jj 工作副本隔离模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JjWorkspaceMode {
    /// 默认：当同一批并发执行 >= 2 件写诏令且项目在 jj 版本库中时启用。
    Auto,
    /// 即使单件写诏令也在临时 jj 工作副本中执行（项目须在 jj 版本库中）。
    Always,
    /// 显式关闭临时 jj 工作副本隔离。
    Off,
}

/// 读取 `ZIYUAN_JJ_WORKSPACE` 环境变量，决定工作副本隔离模式。
pub fn jj_workspace_mode() -> JjWorkspaceMode {
    match std::env::var(JJ_WORKSPACE_ENV)
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("off" | "0" | "false") => JjWorkspaceMode::Off,
        Some("always" | "1" | "true") => JjWorkspaceMode::Always,
        _ => JjWorkspaceMode::Auto,
    }
}

/// 判断 `root` 是否可用 jj 临时工作副本隔离（未被 `ZIYUAN_JJ_WORKSPACE=off` 关闭、
/// 目录下存在 `.jj` 且 `jj root` 可正常执行）。
pub fn supports_jj_workspace(root: &Path) -> bool {
    if jj_workspace_mode() == JjWorkspaceMode::Off {
        return false;
    }
    if !root.join(".jj").is_dir() {
        return false;
    }
    run(root, "jj", &["root"]).is_ok()
}

/// 一件写诏令专用的临时 jj 工作副本（位于系统临时目录，与主工作目录隔离）。
///
/// 析构时自动 `jj workspace forget` 并删除临时目录，避免残留工作副本或污染提交图。
#[derive(Debug)]
pub struct EphemeralJjWorkspace {
    repo_root: PathBuf,
    name: String,
    path: PathBuf,
    cleaned: AtomicBool,
}

impl EphemeralJjWorkspace {
    /// 为 `task` 从当前 `@` 快照检出一个独立 jj 工作副本。
    pub fn create(repo_root: &Path, task: TaskId) -> Result<Self, AppError> {
        let _guard = JJ_WORKSPACE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let seq = JJ_WORKSPACE_SEQ.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            "{EPHEMERAL_WS_PREFIX}t{}-{}-{seq}",
            task.0,
            std::process::id()
        );
        let path = std::env::temp_dir().join(&name);
        if path.exists() {
            let _ = std::fs::remove_dir_all(&path);
        }
        let dest = path.to_string_lossy().into_owned();
        if let Err(err) = run(
            repo_root,
            "jj",
            &["workspace", "add", "--name", &name, "-r", "@", &dest],
        ) {
            let _ = std::fs::remove_dir_all(&path);
            return Err(err);
        }
        Ok(Self {
            repo_root: repo_root.to_path_buf(),
            name,
            path,
            cleaned: AtomicBool::new(false),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 幂等地注销该临时工作副本并删除临时目录。
    pub fn cleanup(&self) {
        if self.cleaned.swap(true, Ordering::SeqCst) {
            return;
        }
        let _guard = JJ_WORKSPACE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = run(&self.repo_root, "jj", &["workspace", "forget", &self.name]);
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl Drop for EphemeralJjWorkspace {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// 清理异常中断遗留的孤立 `ziyuan-ws-*` 工作副本（只清理临时目录已不存在的记录，不影响仍在执行的任务）。
pub fn prune_stale_workspaces(repo_root: &Path) -> usize {
    let _guard = JJ_WORKSPACE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Ok(list) = run(
        repo_root,
        "jj",
        &["workspace", "list", "-T", r#"name ++ "\n""#],
    ) else {
        return 0;
    };
    let tmp = std::env::temp_dir();
    let mut pruned = 0;
    for line in list.lines() {
        let name = line.trim();
        if !name.starts_with(EPHEMERAL_WS_PREFIX) {
            continue;
        }
        if !tmp.join(name).exists() && run(repo_root, "jj", &["workspace", "forget", name]).is_ok()
        {
            pruned += 1;
        }
    }
    pruned
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

    fn init_temp_jj_repo(tag: &str) -> Option<PathBuf> {
        if run(&std::env::temp_dir(), "jj", &["--version"]).is_err() {
            return None;
        }
        let root = std::env::temp_dir().join(format!("ziyuan-jj-ws-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
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
        Some(root)
    }

    #[test]
    fn ephemeral_jj_workspace_isolates_cleans_up_and_prunes_orphans() {
        let non_jj = std::env::temp_dir().join(format!("ziyuan-no-jj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&non_jj);
        std::fs::create_dir_all(&non_jj).unwrap();
        assert!(!supports_jj_workspace(&non_jj));
        let _ = std::fs::remove_dir_all(&non_jj);

        let Some(root) = init_temp_jj_repo("lifecycle") else {
            return;
        };
        std::fs::write(root.join("a.txt"), "主副本原文\n").unwrap();
        assert!(supports_jj_workspace(&root));

        let ws_path;
        let ws_name;
        {
            let ws = EphemeralJjWorkspace::create(&root, TaskId(42)).unwrap();
            ws_path = ws.path().to_path_buf();
            ws_name = ws.name().to_owned();
            assert!(ws_path.is_dir());
            assert_eq!(
                std::fs::read_to_string(ws_path.join("a.txt")).unwrap(),
                "主副本原文\n"
            );
            // 改写临时工作副本不影响主工作目录。
            std::fs::write(ws_path.join("a.txt"), "临时改动\n").unwrap();
            assert_eq!(
                std::fs::read_to_string(root.join("a.txt")).unwrap(),
                "主副本原文\n"
            );
            let list = run(&root, "jj", &["workspace", "list", "-T", "name ++ \"\n\""]).unwrap();
            assert!(list.lines().any(|l| l.trim() == ws_name), "{list}");
        }
        // Drop 后自动注销并删除临时目录。
        assert!(!ws_path.exists());
        let list = run(&root, "jj", &["workspace", "list", "-T", "name ++ \"\n\""]).unwrap();
        assert!(!list.lines().any(|l| l.trim() == ws_name), "{list}");

        // 模拟进程异常中断遗留的孤立工作副本：目录已删但 jj 仍记录。
        let active = EphemeralJjWorkspace::create(&root, TaskId(43)).unwrap();
        let orphan_name = format!("{EPHEMERAL_WS_PREFIX}orphan-{}", std::process::id());
        let orphan_dir = std::env::temp_dir().join(&orphan_name);
        let _ = std::fs::remove_dir_all(&orphan_dir);
        run(
            &root,
            "jj",
            &[
                "workspace",
                "add",
                "--name",
                &orphan_name,
                "-r",
                "@",
                orphan_dir.to_str().unwrap(),
            ],
        )
        .unwrap();
        std::fs::remove_dir_all(&orphan_dir).unwrap();
        assert_eq!(prune_stale_workspaces(&root), 1);
        let list = run(&root, "jj", &["workspace", "list", "-T", "name ++ \"\n\""]).unwrap();
        assert!(!list.lines().any(|l| l.trim() == orphan_name), "{list}");
        assert!(list.lines().any(|l| l.trim() == active.name()), "{list}");
        drop(active);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn parallel_write_jobs_use_isolated_jj_workspaces_and_detect_conflicts() {
        use std::collections::BTreeSet;
        use std::sync::{Arc, Barrier};
        use std::time::Duration;
        use ziyuan_core::{AgentId, Capability, Department, Seal};
        use ziyuan_runtime::{
            BoxError, CancelToken, ExecutionHooks, Job, Limits, Outcome, Scheduler, WorkerBackend,
            tools,
        };
        use ziyuan_storage::EventLog;

        let Some(root) = init_temp_jj_repo("parallel-run") else {
            return;
        };
        std::fs::write(root.join("a.txt"), "A0\n").unwrap();
        std::fs::write(root.join("b.txt"), "B0\n").unwrap();
        std::fs::write(root.join("shared.txt"), "S0\n").unwrap();

        struct ParallelWriteBackend {
            root: PathBuf,
            barrier: Arc<Barrier>,
            seen_roots: Mutex<Vec<PathBuf>>,
        }

        impl WorkerBackend for ParallelWriteBackend {
            fn workspace_root(&self) -> Option<&Path> {
                Some(&self.root)
            }

            fn execute(&self, job: &Job, hooks: &dyn ExecutionHooks) -> Result<String, BoxError> {
                let effective_root = job.workspace_override.as_deref().unwrap_or(&self.root);
                self.seen_roots
                    .lock()
                    .unwrap()
                    .push(effective_root.to_path_buf());
                // 等待两件写任务真正并发进入独立工作副本。
                self.barrier.wait();
                let sealed = tools::sealed_tools(effective_root, &job.edict.seal, None)?;
                let write_tool = sealed
                    .tools
                    .iter()
                    .find(|t| t.spec().name == "write_file")
                    .ok_or("缺少 write_file 工具")?;
                let (file, content) = if job.edict.text.contains("改A") {
                    ("a.txt", "A1\n")
                } else if job.edict.text.contains("改B") {
                    ("b.txt", "B1\n")
                } else if job.edict.text.contains("冲突甲") {
                    ("shared.txt", "S-甲\n")
                } else {
                    ("shared.txt", "S-乙\n")
                };
                write_tool.execute(
                    &serde_json::json!({ "path": file, "content": content }).to_string(),
                )?;
                let changes = sealed.changes();
                if !changes.is_empty() {
                    hooks.changes_proposed(changes);
                }
                Ok("结论：已改。\n依据：工具。\n未办之事：无。".into())
            }
        }

        let mut session = Session::found(EventLog::new(&root)).unwrap();
        let write_seal = Seal {
            capabilities: [Capability::ReadFile, Capability::WriteFile].into(),
            scope: vec![".".into()],
            mcp: Default::default(),
        };
        let t1 = session
            .issue(Department::Works, "改A".into(), write_seal.clone())
            .unwrap();
        let t2 = session
            .issue(Department::Works, "改B".into(), write_seal.clone())
            .unwrap();

        let backend = Arc::new(ParallelWriteBackend {
            root: root.clone(),
            barrier: Arc::new(Barrier::new(2)),
            seen_roots: Mutex::new(Vec::new()),
        });
        let scheduler = Scheduler::new(
            backend.clone(),
            Limits {
                max_concurrent: 2,
                timeout: Duration::from_secs(30),
            },
        );
        let mut isolated_notes = 0usize;
        session
            .run(&[t1.id, t2.id], &scheduler, &CancelToken::new(), |u| {
                if let crate::RunUpdate::Finished(report) = u {
                    assert!(
                        matches!(report.outcome, Outcome::Completed(_)),
                        "{report:?}"
                    );
                    if report.path.iter().any(|p| p.contains("独立 jj 工作副本")) {
                        isolated_notes += 1;
                    }
                }
            })
            .await
            .unwrap();
        assert_eq!(isolated_notes, 2);

        // 两件写任务各自拿到不同的独立 jj 工作副本，且都不等于主目录。
        let seen = backend.seen_roots.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().all(|p| p != &root));
        assert_eq!(seen.iter().collect::<BTreeSet<_>>().len(), 2);

        // 主目录在准奏前完全未被改动。
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "A0\n");
        assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "B0\n");

        // 执行结束且租约释放后，临时目录与 jj workspace 记录应已清空（稍候后台线程析构）。
        for _ in 0..40 {
            let list = run(&root, "jj", &["workspace", "list", "-T", "name ++ \"\n\""]).unwrap();
            if list
                .lines()
                .all(|l| !l.trim().starts_with(EPHEMERAL_WS_PREFIX))
                && seen.iter().all(|p| !p.exists())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let list = run(&root, "jj", &["workspace", "list", "-T", "name ++ \"\n\""]).unwrap();
        assert!(
            list.lines()
                .all(|l| !l.trim().starts_with(EPHEMERAL_WS_PREFIX)),
            "残留工作副本：{list}"
        );
        assert!(seen.iter().all(|p| !p.exists()));

        // 两件互不冲突的改动都可以准奏落盘。
        session.apply_changes(t1.id, &root, Vec::new()).unwrap();
        session.apply_changes(t2.id, &root, Vec::new()).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "A1\n");
        assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "B1\n");

        // 并行写分办：在非 jj 目录下必须拒绝，在 jj 目录下允许并发且落盘时冲突保护生效。
        let parent = session
            .issue(Department::Works, "并行改同文件".into(), write_seal)
            .unwrap();
        let non_jj =
            std::env::temp_dir().join(format!("ziyuan-no-jj-split-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&non_jj);
        std::fs::create_dir_all(&non_jj).unwrap();
        let err = session
            .split_with_options(
                parent.id,
                vec![(AgentId(4), "冲突甲".into()), (AgentId(4), "冲突乙".into())],
                true,
                Some(&non_jj),
            )
            .unwrap_err();
        assert!(err.to_string().contains("jj"), "{err}");
        let _ = std::fs::remove_dir_all(&non_jj);

        let subs = session
            .split_with_options(
                parent.id,
                vec![(AgentId(4), "冲突甲".into()), (AgentId(4), "冲突乙".into())],
                true,
                Some(&root),
            )
            .unwrap();
        assert_eq!(subs.len(), 2);
        // 未强行串行：两件子诏令都无前置依赖，可直接并发执行。
        assert!(
            session
                .court()
                .blocked_by(session.court().task(subs[0]).unwrap())
                .is_none()
        );
        assert!(
            session
                .court()
                .blocked_by(session.court().task(subs[1]).unwrap())
                .is_none()
        );

        let backend2 = Arc::new(ParallelWriteBackend {
            root: root.clone(),
            barrier: Arc::new(Barrier::new(2)),
            seen_roots: Mutex::new(Vec::new()),
        });
        let scheduler2 = Scheduler::new(
            backend2,
            Limits {
                max_concurrent: 2,
                timeout: Duration::from_secs(30),
            },
        );
        session
            .run(&subs, &scheduler2, &CancelToken::new(), |_| {})
            .await
            .unwrap();
        // 第一件准奏成功，第二件因 shared.txt 原文已变而被安全拒绝。
        session.apply_changes(subs[0], &root, Vec::new()).unwrap();
        let conflict = session
            .apply_changes(subs[1], &root, Vec::new())
            .unwrap_err();
        assert!(conflict.to_string().contains("shared.txt"), "{conflict}");

        let _ = std::fs::remove_dir_all(&root);
    }
}
