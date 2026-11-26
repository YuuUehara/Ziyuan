//! 工作区与单目录临时文件扫描报告：递归深度控制、跳过目录与 JSON 脱敏序列化。

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::{
    Item, Kind, Outcome, Problem, decide_candidate, decide_lease, is_candidate_name, lease_entries,
    lease_id_of,
};

/// 两种模式：预览只报“本可删除”，删数为 0。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Preview,
    Apply,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Apply => "apply",
        }
    }
}

/// 某一类对象的计数：可清理数与实际删除数分列，预览才说得清。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Counts {
    pub candidates: usize,
    pub removable: usize,
    pub removed: usize,
    pub kept: usize,
}

/// 一轮扫描的报告：计数与逐项明细都出自同一次判定，不再二次统计。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Report {
    pub mode: Mode,
    /// 扫描范围：调用方给的相对标签，工作区根是空串。报告里的路径也相对同一个根。
    pub root: PathBuf,
    /// 报告里的“范围”：`root` 的标签，空标签按 `.` 呈现。
    scope: String,
    pub recursive: bool,
    pub items: Vec<Item>,
    pub problems: Vec<Problem>,
    /// 读不了的目录：既不算扫描成功，也不中止整轮。
    pub unscanned: Vec<PathBuf>,
    /// 主动不进入的目录：相对 `root` 的目录路径（根层就是名字本身），`/` 分隔。
    pub skipped: Vec<String>,
    /// 因深度上限主动停止下探的目录（相对路径）。
    pub capped: Vec<String>,
}

impl Report {
    pub(super) fn new(mode: Mode, root: &Path, scope: &str, recursive: bool) -> Self {
        Self {
            mode,
            root: root.to_path_buf(),
            scope: scope.to_owned(),
            recursive,
            items: Vec::new(),
            problems: Vec::new(),
            unscanned: Vec::new(),
            skipped: Vec::new(),
            capped: Vec::new(),
        }
    }

    fn finish(&mut self) {
        for list in [&mut self.skipped, &mut self.capped] {
            list.sort();
            list.dedup();
        }
        self.unscanned.sort();
        self.unscanned.dedup();
    }

    /// 报告里的路径形式：相对 `root`、`/` 分隔。绝对路径不外泄。
    pub fn display_relative(&self, path: &Path) -> String {
        let relative = path.strip_prefix(&self.root).unwrap_or(path);
        normalize_relative(relative)
    }

    /// 扫描范围：调用方给的相对标签（`sweep_dir` 的 `relative_dir`），根记作 `.`。
    pub fn scope(&self) -> String {
        if self.scope.is_empty() {
            ".".to_owned()
        } else {
            self.scope.clone()
        }
    }

    /// 报告里不出现绝对外部路径：消息里若嵌了扫描根，换成 `.`。
    pub fn scrub_message(&self, message: &str) -> String {
        let root = self.root.display().to_string();
        if root.is_empty() {
            return message.to_owned();
        }
        message.replace(&root, ".")
    }

    pub fn counts(&self, kind: Kind) -> Counts {
        let mut counts = Counts::default();
        for item in self.items.iter().filter(|item| item.kind == kind) {
            counts.candidates += 1;
            match item.outcome {
                Outcome::Remove => {
                    counts.removable += 1;
                    if item.acted {
                        counts.removed += 1;
                    }
                }
                Outcome::Keep(_) => counts.kept += 1,
            }
        }
        counts
    }

    /// 本次实际删掉的对象数（候选与租约都算）。
    pub fn removed_total(&self) -> usize {
        self.items.iter().filter(|item| item.acted).count()
    }

    /// 按设计的退出码判定：只有扫描／删除的 I/O 失败、或读不了的目录才算“需要处理”。
    pub fn needs_attention(&self) -> bool {
        !self.problems.is_empty() || !self.unscanned.is_empty()
    }

    /// 报告只列原因名与相对路径，不带内部细节。
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::json;
        let describe = |kind: Kind| {
            let counts = self.counts(kind);
            json!({
                "candidates": counts.candidates,
                "removable": counts.removable,
                "removed": counts.removed,
                "kept": counts.kept,
            })
        };
        json!({
            "mode": self.mode.as_str(),
            "scope": self.scope(),
            "recursive": self.recursive,
            "temps": describe(Kind::Temp),
            "leases": describe(Kind::Lease),
            "removed_total": self.removed_total(),
            "skipped": self.skipped,
            "capped": self.capped,
            "items": self.items.iter().map(|item| json!({
                "path": self.display_relative(&item.path),
                "kind": match item.kind { Kind::Temp => "temp", Kind::Lease => "lease" },
                "outcome": match item.outcome {
                    Outcome::Remove => "remove",
                    Outcome::Keep(_) => "keep",
                },
                "reason": item.reason_name(),
                "acted": item.acted,
            })).collect::<Vec<_>>(),
            "problems": self.problems.iter().map(|problem| json!({
                "path": self.display_relative(&problem.path),
                "message": self.scrub_message(&problem.message),
            })).collect::<Vec<_>>(),
            "unscanned": self.unscanned
                .iter()
                .map(|path| self.display_relative(path))
                .collect::<Vec<_>>(),
        })
    }
}

/// 扫一个目录（不递归）：候选项逐项决策，再逐个处理孤儿租约。
pub(crate) fn sweep_dir(
    dir: &Path,
    relative_dir: &str,
    protected: &BTreeSet<String>,
    apply: bool,
) -> Report {
    let mut report = Report::new(
        if apply { Mode::Apply } else { Mode::Preview },
        dir,
        relative_dir,
        false,
    );
    scan_into(&mut report, dir, relative_dir, protected, apply);
    report.finish();
    report
}

/// 不进入的目录：控制目录与常见构建产物。
const SKIPPED_DIRS: [&str; 5] = [".ziyuan", ".git", ".jj", "target", "node_modules"];

/// 递归深度上限：到达即不再下探，并把跳过的目录记进 `report.capped`。
pub const MAX_DEPTH: usize = 32;

/// 手工入口用：从工作区根开始递归清理。
pub fn sweep_workspace(root: &Path, protected: &BTreeSet<String>, apply: bool) -> Report {
    let mut report = Report::new(
        if apply { Mode::Apply } else { Mode::Preview },
        root,
        "",
        true,
    );
    walk(&mut report, root, "", protected, apply, 0);
    report.finish();
    report
}

/// 深度优先扫完整棵子树；`depth` 是 `dir` 相对根的层数（根为 0）。
pub(super) fn walk(
    report: &mut Report,
    dir: &Path,
    relative_dir: &str,
    protected: &BTreeSet<String>,
    apply: bool,
    depth: usize,
) {
    if depth >= MAX_DEPTH {
        report.capped.push(relative_dir.to_owned());
        return;
    }
    scan_into(report, dir, relative_dir, protected, apply);
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut children = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                report.unscanned.push(dir.to_path_buf());
                report.problems.push(Problem {
                    path: dir.to_path_buf(),
                    message: e.to_string(),
                });
                return;
            }
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            report.unscanned.push(entry.path());
            continue;
        };
        let meta = match fs::symlink_metadata(entry.path()) {
            Ok(meta) => meta,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
            Err(_) => {
                report.problems.push(Problem {
                    path: entry.path(),
                    message: "无法读取目录项元数据".into(),
                });
                continue;
            }
        };
        let file_type = meta.file_type();
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        if SKIPPED_DIRS.contains(&name) {
            report.skipped.push(join_relative(relative_dir, name));
            continue;
        }
        children.push((entry.path(), join_relative(relative_dir, name)));
    }
    children.sort();
    for (path, relative) in children {
        walk(report, &path, &relative, protected, apply, depth + 1);
    }
}

fn scan_into(
    report: &mut Report,
    dir: &Path,
    relative_dir: &str,
    protected: &BTreeSet<String>,
    apply: bool,
) {
    let mut names: Vec<String> = Vec::new();
    let mut leases: Vec<(PathBuf, String)> = Vec::new();
    match fs::read_dir(dir) {
        Err(e) => {
            report.unscanned.push(dir.to_path_buf());
            report.problems.push(Problem {
                path: dir.to_path_buf(),
                message: e.to_string(),
            });
            return;
        }
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Err(e) => {
                        report.problems.push(Problem {
                            path: dir.to_path_buf(),
                            message: e.to_string(),
                        });
                        report.unscanned.push(dir.to_path_buf());
                        return;
                    }
                    Ok(entry) => {
                        let file_name = entry.file_name();
                        let Some(name) = file_name.to_str() else {
                            report.unscanned.push(entry.path());
                            continue;
                        };
                        names.push(name.to_owned());
                        if let Some(id) = lease_id_of(name) {
                            leases.push((entry.path(), id.to_owned()));
                        }
                    }
                }
            }
        }
    }
    names.sort();
    leases.sort_by(|left, right| left.0.cmp(&right.0));
    let candidates: Vec<String> = names
        .into_iter()
        .filter(|name| is_candidate_name(name))
        .collect();
    for name in candidates {
        let relative = join_relative(relative_dir, &name);
        let item = decide_candidate(
            dir,
            &name,
            &relative,
            &leases,
            protected,
            apply,
            &mut report.problems,
        );
        report.items.push(item);
    }
    let leases = match lease_entries(dir) {
        Ok(leases) => leases,
        Err(e) => {
            report.unscanned.push(dir.to_path_buf());
            report.problems.push(Problem {
                path: dir.to_path_buf(),
                message: e.to_string(),
            });
            return;
        }
    };
    for (path, id) in leases {
        let item = decide_lease(dir, &path, &id, apply, &mut report.problems);
        report.items.push(item);
    }
}

/// 路径 → `/` 分隔的普通组件串：只保留 `Normal` 组件，因此盘符、根与前导 `..` 都被滤掉。
pub(crate) fn normalize_relative(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// 相对路径一律用 `/` 分隔，与恢复记录同一形式；根目录本身是空串。
fn join_relative(relative_dir: &str, name: &str) -> String {
    if relative_dir.is_empty() {
        name.to_owned()
    } else {
        format!("{relative_dir}/{name}")
    }
}
