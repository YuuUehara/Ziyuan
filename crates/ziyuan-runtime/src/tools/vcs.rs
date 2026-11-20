//! 只读版本库上下文工具：`vcs_status`、`vcs_diff`、`vcs_log`。
//!
//! 随 `Capability::ReadFile` 一并授予，不新增玉玺代码、不改变事件簿格式。
//! 优先检测 Jujutsu（`jj`），否则回退 Git（`git`）；严格只读、禁止任意参数或 shell 注入，
//! 子进程仅继承无害环境变量（不传 API Key / GitHub Token），并对输出按玉玺范围与禁区
//! （`.ziyuan`、`.git`、`.jj`、`.env*`）二次过滤，最大返回 32 KiB。

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::BoxError;
use crate::agent_loop::{Tool, ToolSpec};

use super::{
    Sandbox, TEST_ENV, is_denied, object_args, optional_bool, optional_i64, optional_string,
};

/// 单次 VCS 工具最多返回的字节数（UTF-8 字符边界截断）。
const MAX_VCS_BYTES: usize = 32 * 1024;
/// 单次只读 VCS 命令的超时上限。
const VCS_TIMEOUT: Duration = Duration::from_secs(15);
/// `vcs_log` 默认与最大提交条数。
const DEFAULT_LOG_LIMIT: usize = 10;
const MAX_LOG_LIMIT: usize = 30;
/// 允许的 revision 标识最大长度。
const MAX_REV_LEN: usize = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VcsKind {
    Jj,
    Git,
}

impl VcsKind {
    fn label(self) -> &'static str {
        match self {
            Self::Jj => "jj",
            Self::Git => "git",
        }
    }
}

/// 校验 revision 参数，严防选项注入（`-` 开头）、路径穿越（`:` 语法如 `HEAD:.env`）或空白/元字符。
fn validate_rev(rev: &str) -> Result<&str, BoxError> {
    let rev = rev.trim();
    if rev.is_empty() {
        return Err("rev 不能为空".into());
    }
    if rev.len() > MAX_REV_LEN {
        return Err(format!("rev 过长（最多 {MAX_REV_LEN} 字符）").into());
    }
    if rev.starts_with('-') {
        return Err(format!("非法 rev（禁止以 `-` 开头）：{rev}").into());
    }
    if !rev
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '^' | '@' | '/' | '-'))
    {
        return Err(format!("非法 rev（含不允许字符）：{rev}").into());
    }
    Ok(rev)
}

/// 在 `cwd` 下以最小环境与严格超时执行只读命令，返回 `(成功与否, stdout, stderr)`。
fn run_readonly(
    program: &str,
    args: &[String],
    cwd: &Path,
) -> Result<(bool, String, String), BoxError> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in TEST_ENV {
        if let Ok(val) = std::env::var(key) {
            cmd.env(key, val);
        }
    }
    // 禁止 git/jj 调用交互提示或外部 pager。
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GIT_PAGER", "cat");
    cmd.env("PAGER", "cat");

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("无法执行 `{program}`：{e}"))?;
    let mut out_pipe = child.stdout.take().expect("stdout piped");
    let mut err_pipe = child.stderr.take().expect("stderr piped");
    let out_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        buf
    });
    let err_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        buf
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() >= VCS_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "`{program}` 执行超时（超过 {} 秒）",
                        VCS_TIMEOUT.as_secs()
                    )
                    .into());
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(format!("等待 `{program}` 失败：{e}").into()),
        }
    };

    let stdout = String::from_utf8_lossy(&out_reader.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err_reader.join().unwrap_or_default()).into_owned();
    Ok((status.success(), stdout, stderr))
}

fn detect_vcs(root: &Path) -> Result<VcsKind, BoxError> {
    if let Ok((true, out, _)) = run_readonly(
        "jj",
        &["--no-pager".into(), "--color=never".into(), "root".into()],
        root,
    ) && !out.trim().is_empty()
    {
        return Ok(VcsKind::Jj);
    }
    if let Ok((true, out, _)) =
        run_readonly("git", &["rev-parse".into(), "--show-toplevel".into()], root)
        && !out.trim().is_empty()
    {
        return Ok(VcsKind::Git);
    }
    Err("当前项目不在受支持的版本库中（未检测到 jj 或 git 仓库）".into())
}

fn clip_utf8(text: &str, max_bytes: usize, hint: &str) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n…（输出已截断至 {} KiB；{hint}）",
        &text[..end],
        max_bytes / 1024
    )
}

/// 标准化 VCS 输出中的相对路径，并判断其是否允许展示（非禁区、在玉玺范围内、未越界）。
fn path_allowed(sandbox: &Sandbox, raw: &str) -> bool {
    let cleaned = raw.trim().trim_matches('"').replace('\\', "/");
    let cleaned = cleaned
        .strip_prefix("a/")
        .or_else(|| cleaned.strip_prefix("b/"))
        .unwrap_or(&cleaned);
    let cleaned = cleaned.trim_start_matches("./").trim_end_matches('/');
    if cleaned.is_empty() || cleaned.starts_with('/') {
        return false;
    }
    let rel = Path::new(cleaned);
    if rel
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return false;
    }
    let candidate = sandbox.root.join(rel);
    if is_denied(&sandbox.root, &candidate) {
        return false;
    }
    if !sandbox.scope.iter().any(|s| candidate.starts_with(s)) {
        return false;
    }
    if let Ok(canon) = candidate.canonicalize()
        && (!canon.starts_with(&sandbox.root)
            || is_denied(&sandbox.root, &canon)
            || !sandbox.scope.iter().any(|s| canon.starts_with(s)))
    {
        return false;
    }
    true
}

/// 过滤 `git status --porcelain=v1` 行，仅保留玉玺范围内且非禁区的条目。
fn filter_git_status(sandbox: &Sandbox, raw: &str) -> Vec<String> {
    let mut kept = Vec::new();
    for line in raw.lines() {
        if line.len() < 4 {
            continue;
        }
        let status = &line[..2];
        let rest = &line[3..];
        if let Some((old, new)) = rest.split_once(" -> ") {
            if path_allowed(sandbox, old) && path_allowed(sandbox, new) {
                kept.push(format!("{status} {old} -> {new}"));
            }
        } else if path_allowed(sandbox, rest) {
            kept.push(format!("{status} {rest}"));
        }
    }
    kept
}

/// 过滤 `jj status` 输出：保留状态摘要头，工作副本改动列表逐行按玉玺与禁区过滤。
fn filter_jj_status(sandbox: &Sandbox, raw: &str, specs: &[String]) -> String {
    let mut out = Vec::new();
    let mut in_changes = false;
    let mut shown_changes = 0usize;
    let mut hidden_changes = 0usize;
    for line in raw.lines() {
        if line.starts_with("Working copy changes:") {
            in_changes = true;
            out.push(line.to_owned());
            continue;
        }
        if line.starts_with("Working copy ")
            || line.starts_with("Parent commit")
            || line.starts_with("The working copy has no changes.")
            || line.starts_with("Conflicted bookmarks")
        {
            in_changes = false;
            out.push(line.to_owned());
            continue;
        }
        if in_changes {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                in_changes = false;
                continue;
            }
            let Some((code, path_part)) = trimmed.split_once(' ') else {
                continue;
            };
            let allowed = if code == "R" && path_part.contains(" => ") {
                parse_jj_rename_paths(path_part).is_some_and(|(a, b)| {
                    path_allowed(sandbox, &a)
                        && path_allowed(sandbox, &b)
                        && (matches_specs(&a, specs) || matches_specs(&b, specs))
                })
            } else {
                path_allowed(sandbox, path_part) && matches_specs(path_part, specs)
            };
            if allowed {
                out.push(format!("{code} {path_part}"));
                shown_changes += 1;
            } else {
                hidden_changes += 1;
            }
        }
    }
    if hidden_changes > 0 {
        out.push(format!(
            "（另有 {hidden_changes} 处改动在玉玺范围外或禁区内，已隐去）"
        ));
    }
    if shown_changes == 0 && hidden_changes == 0 && out.is_empty() {
        return "工作区干净（或授权范围内无未提交改动）。".into();
    }
    out.join("\n")
}

fn matches_specs(rel: &str, specs: &[String]) -> bool {
    if specs.is_empty() || specs.iter().any(|s| s == ".") {
        return true;
    }
    let clean = rel.trim().replace('\\', "/");
    let clean = clean.trim_start_matches("./");
    specs.iter().any(|spec| {
        let s = spec.trim_start_matches("./").trim_end_matches('/');
        clean == s || clean.starts_with(&format!("{s}/"))
    })
}

/// 解析 `jj` 的改名显示（如 `dir/{a.rs => b.rs}` 或 `{a => b}`），返回 `(旧路径, 新路径)`。
fn parse_jj_rename_paths(text: &str) -> Option<(String, String)> {
    let open = text.find('{')?;
    let close = text[open + 1..].find('}')? + open + 1;
    let prefix = &text[..open];
    let suffix = &text[close + 1..];
    let inside = &text[open + 1..close];
    let (old_mid, new_mid) = inside.split_once(" => ")?;
    Some((
        format!("{prefix}{old_mid}{suffix}"),
        format!("{prefix}{new_mid}{suffix}"),
    ))
}

/// 过滤 `diff --git` 统一差异，移除任何涉及禁区或玉玺范围外文件的区段。
fn filter_git_diff(sandbox: &Sandbox, raw: &str) -> (String, usize) {
    if raw.trim().is_empty() {
        return (String::new(), 0);
    }
    let mut sections: Vec<&str> = Vec::new();
    let mut starts: Vec<usize> = Vec::new();
    for (idx, line) in raw.match_indices("diff --git ") {
        if idx == 0 || raw.as_bytes().get(idx.saturating_sub(1)) == Some(&b'\n') {
            starts.push(idx);
        }
        let _ = line;
    }
    if starts.is_empty() {
        // `--stat` 输出：逐行按 `path | ...` 过滤。
        let mut kept = Vec::new();
        let mut hidden = 0usize;
        for line in raw.lines() {
            if let Some((left, _)) = line.split_once('|') {
                let path = left.trim();
                let ok = if path.contains(" => ") {
                    parse_jj_rename_paths(path)
                        .or_else(|| {
                            path.split_once(" => ")
                                .map(|(a, b)| (a.trim().to_owned(), b.trim().to_owned()))
                        })
                        .is_some_and(|(a, b)| {
                            path_allowed(sandbox, &a) && path_allowed(sandbox, &b)
                        })
                } else {
                    path_allowed(sandbox, path)
                };
                if ok {
                    kept.push(line);
                } else {
                    hidden += 1;
                }
            } else if hidden == 0 {
                // 只有在未隐去任何文件时保留原汇总行，避免泄露被过滤文件的统计。
                kept.push(line);
            }
        }
        return (kept.join("\n"), hidden);
    }

    for i in 0..starts.len() {
        let start = starts[i];
        let end = starts.get(i + 1).copied().unwrap_or(raw.len());
        sections.push(&raw[start..end]);
    }

    let mut kept = String::new();
    let mut hidden = 0usize;
    for sec in sections {
        let header = sec.lines().next().unwrap_or_default();
        let rest = header.strip_prefix("diff --git ").unwrap_or_default();
        // 形如 `a/path b/path`（可能含双引号）
        let allowed = parse_diff_git_header(rest)
            .is_some_and(|(a, b)| path_allowed(sandbox, &a) && path_allowed(sandbox, &b));
        if allowed {
            kept.push_str(sec);
        } else {
            hidden += 1;
        }
    }
    (kept, hidden)
}

fn parse_diff_git_header(rest: &str) -> Option<(String, String)> {
    let rest = rest.trim();
    if let Some((a, b)) = rest.split_once(" b/") {
        return Some((a.to_owned(), format!("b/{b}")));
    }
    if let Some((a, b)) = rest.split_once(" \"b/") {
        return Some((a.to_owned(), format!("\"b/{b}")));
    }
    let mut parts = rest.split_whitespace();
    let a = parts.next()?.to_owned();
    let b = parts.next()?.to_owned();
    Some((a, b))
}

/// 计算要传给 `jj` / `git` 的 pathspec 列表：优先用调用方指定的 `path`，否则用玉玺范围。
fn pathspecs_for(sandbox: &Sandbox, path_arg: Option<String>) -> Result<Vec<String>, BoxError> {
    if let Some(p) = path_arg {
        let trimmed = p.trim();
        if trimmed.is_empty() || trimmed == "." {
            return Ok(sandbox.scope_rel_paths());
        }
        let resolved = sandbox.resolve_vcs_path(trimmed)?;
        return Ok(vec![resolved]);
    }
    Ok(sandbox.scope_rel_paths())
}

pub(super) struct VcsStatus(pub(super) Arc<Sandbox>);

impl Tool for VcsStatus {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "vcs_status".into(),
            description: "只读查看版本库工作区状态（支持 jj 与 git），仅返回玉玺授权范围内、非禁区的未提交改动。".into(),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"可选，限定到单个子目录或文件（相对路径）"}},"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(args_json, &["path"])?;
        let path = optional_string(&args, "path")?;
        let specs = pathspecs_for(&self.0, path)?;
        let kind = detect_vcs(&self.0.root)?;
        match kind {
            VcsKind::Jj => {
                let (ok, stdout, stderr) = run_readonly(
                    "jj",
                    &["--no-pager".into(), "--color=never".into(), "status".into()],
                    &self.0.root,
                )?;
                if !ok {
                    return Err(format!("jj status 失败：{}", stderr.trim()).into());
                }
                let filtered = filter_jj_status(&self.0, &stdout, &specs);
                let out = format!("[vcs: {}]\n{}", kind.label(), filtered);
                Ok(clip_utf8(&out, MAX_VCS_BYTES, "可传 path 缩小范围"))
            }
            VcsKind::Git => {
                let mut argv = vec![
                    "status".into(),
                    "--porcelain=v1".into(),
                    "-uall".into(),
                    "--".into(),
                ];
                argv.extend(specs);
                let (ok, stdout, stderr) = run_readonly("git", &argv, &self.0.root)?;
                if !ok {
                    return Err(format!("git status 失败：{}", stderr.trim()).into());
                }
                let lines = filter_git_status(&self.0, &stdout);
                let body = if lines.is_empty() {
                    "工作区干净（或授权范围内无未提交改动）。".to_owned()
                } else {
                    lines.join("\n")
                };
                let out = format!("[vcs: {}]\n{body}", kind.label());
                Ok(clip_utf8(&out, MAX_VCS_BYTES, "可传 path 缩小范围"))
            }
        }
    }
}

pub(super) struct VcsDiff(pub(super) Arc<Sandbox>);

impl Tool for VcsDiff {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "vcs_diff".into(),
            description: "只读查看版本库 diff（支持 jj 与 git），按玉玺范围与禁区过滤，最多返回 32 KiB。可传 path、rev、staged（仅 git）或 stat_only。".into(),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"可选，限定查看的相对路径或目录"},"rev":{"type":"string","description":"可选，修订标识（如 HEAD~1、main、@-；禁止以 - 开头或含冒号）"},"staged":{"type":"boolean","description":"可选，仅 git 有效：查看已暂存区 diff（默认 false）"},"stat_only":{"type":"boolean","description":"可选，仅返回变更文件与增删行数统计（默认 false）"}},"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(args_json, &["path", "rev", "staged", "stat_only"])?;
        let path = optional_string(&args, "path")?;
        let rev = optional_string(&args, "rev")?;
        let rev = match rev.as_deref() {
            Some(r) => Some(validate_rev(r)?.to_owned()),
            None => None,
        };
        let staged = optional_bool(&args, "staged")?.unwrap_or(false);
        let stat_only = optional_bool(&args, "stat_only")?.unwrap_or(false);
        let specs = pathspecs_for(&self.0, path)?;
        let kind = detect_vcs(&self.0.root)?;

        let (ok, stdout, stderr) = match kind {
            VcsKind::Jj => {
                let mut argv = vec!["--no-pager".into(), "--color=never".into(), "diff".into()];
                if stat_only {
                    argv.push("--stat".into());
                } else {
                    argv.push("--git".into());
                }
                if let Some(r) = &rev {
                    argv.push("-r".into());
                    argv.push(r.clone());
                }
                if !specs.iter().any(|s| s == ".") {
                    argv.push("--".into());
                    argv.extend(specs);
                }
                run_readonly("jj", &argv, &self.0.root)?
            }
            VcsKind::Git => {
                let mut argv = vec!["diff".into(), "--no-color".into(), "--no-ext-diff".into()];
                if staged {
                    argv.push("--cached".into());
                }
                if stat_only {
                    argv.push("--stat".into());
                }
                if let Some(r) = &rev {
                    argv.push(r.clone());
                }
                argv.push("--".into());
                argv.extend(specs);
                run_readonly("git", &argv, &self.0.root)?
            }
        };
        if !ok {
            return Err(format!("{} diff 失败：{}", kind.label(), stderr.trim()).into());
        }
        let (filtered, hidden) = filter_git_diff(&self.0, &stdout);
        let mut out = format!("[vcs: {}]\n", kind.label());
        if filtered.trim().is_empty() {
            out.push_str("无差异（或授权范围内无变动）。");
        } else {
            out.push_str(filtered.trim_end());
        }
        if hidden > 0 {
            out.push_str(&format!(
                "\n（另有 {hidden} 处文件差异在玉玺范围外或禁区内，已隐去）"
            ));
        }
        Ok(clip_utf8(
            &out,
            MAX_VCS_BYTES,
            "可传 path 或 stat_only: true 缩小范围",
        ))
    }
}

pub(super) struct VcsLog(pub(super) Arc<Sandbox>);

impl Tool for VcsLog {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "vcs_log".into(),
            description: "只读查看近期提交历史（支持 jj 与 git），最多 30 条，可按 path 过滤到授权范围内的文件或目录。".into(),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"可选，限定查看指定文件或目录的提交历史"},"rev":{"type":"string","description":"可选，起始修订或范围（禁止以 - 开头或含冒号）"},"limit":{"type":"integer","description":"可选，最多返回条数（1–30，默认 10）"}},"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(args_json, &["path", "rev", "limit"])?;
        let path = optional_string(&args, "path")?;
        let rev = optional_string(&args, "rev")?;
        let rev = match rev.as_deref() {
            Some(r) => Some(validate_rev(r)?.to_owned()),
            None => None,
        };
        let limit = match optional_i64(&args, "limit")? {
            Some(n) if (1..=MAX_LOG_LIMIT as i64).contains(&n) => n as usize,
            Some(_) => return Err(format!("limit 须在 1–{MAX_LOG_LIMIT} 之间").into()),
            None => DEFAULT_LOG_LIMIT,
        };
        let specs = pathspecs_for(&self.0, path)?;
        let kind = detect_vcs(&self.0.root)?;

        let (ok, stdout, stderr) = match kind {
            VcsKind::Jj => {
                let mut argv = vec![
                    "--no-pager".into(),
                    "--color=never".into(),
                    "log".into(),
                    "--no-graph".into(),
                    "-n".into(),
                    limit.to_string(),
                    "-T".into(),
                    r#"change_id.shortest(8) ++ " " ++ commit_id.shortest(8) ++ " " ++ author.timestamp().local().format("%Y-%m-%d") ++ " " ++ description.first_line() ++ "\n""#.into(),
                ];
                if let Some(r) = &rev {
                    argv.push("-r".into());
                    argv.push(r.clone());
                }
                if !specs.iter().any(|s| s == ".") {
                    argv.push("--".into());
                    argv.extend(specs);
                }
                run_readonly("jj", &argv, &self.0.root)?
            }
            VcsKind::Git => {
                let mut argv = vec![
                    "log".into(),
                    "--no-color".into(),
                    "-n".into(),
                    limit.to_string(),
                    "--date=short".into(),
                    "--format=%h %ad %an %s".into(),
                ];
                if let Some(r) = &rev {
                    argv.push(r.clone());
                }
                argv.push("--".into());
                argv.extend(specs);
                run_readonly("git", &argv, &self.0.root)?
            }
        };
        if !ok {
            return Err(format!("{} log 失败：{}", kind.label(), stderr.trim()).into());
        }
        let body = if stdout.trim().is_empty() {
            "无符合条件的提交记录。".to_owned()
        } else {
            stdout.trim_end().to_owned()
        };
        let out = format!("[vcs: {}]\n{body}", kind.label());
        Ok(clip_utf8(&out, MAX_VCS_BYTES, "可减小 limit 或传 path"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use ziyuan_core::{Capability, Seal};

    fn temp_repo(label: &str) -> std::path::PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ziyuan-vcs-tool-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Tester")
            .env("GIT_AUTHOR_EMAIL", "tester@example.com")
            .env("GIT_COMMITTER_NAME", "Tester")
            .env("GIT_COMMITTER_EMAIL", "tester@example.com")
            .output()
            .expect("git should run");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn validate_rev_rejects_option_injection_and_colon_path_smuggling() {
        assert!(validate_rev("HEAD").is_ok());
        assert!(validate_rev("HEAD~2").is_ok());
        assert!(validate_rev("main..@").is_ok());
        assert!(validate_rev("@-").is_ok());
        assert!(validate_rev("feat/foo_1.2").is_ok());

        assert!(validate_rev("").is_err());
        assert!(validate_rev("--exec=sh").is_err());
        assert!(validate_rev("-n1").is_err());
        assert!(validate_rev("HEAD:.env").is_err());
        assert!(validate_rev("HEAD;ls").is_err());
        assert!(validate_rev("HEAD $(id)").is_err());
    }

    #[test]
    fn git_vcs_tools_enforce_scope_filter_secrets_and_support_diff_and_log() {
        let dir = temp_repo("git-scope");
        git(&dir, &["init", "-q"]);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("secret")).unwrap();
        fs::write(dir.join("src/a.rs"), "fn a() -> i32 { 1 }\n").unwrap();
        fs::write(dir.join("secret/b.rs"), "const B: &str = \"hidden\";\n").unwrap();
        fs::write(dir.join(".env"), "API_KEY=super-secret\n").unwrap();
        git(&dir, &["add", "src/a.rs", "secret/b.rs", ".env"]);
        git(&dir, &["commit", "-q", "-m", "initial commit"]);

        // 修改三个文件：授权目录 src、未授权目录 secret、禁区 .env。
        fs::write(dir.join("src/a.rs"), "fn a() -> i32 { 2 }\n").unwrap();
        fs::write(dir.join("secret/b.rs"), "const B: &str = \"changed\";\n").unwrap();
        fs::write(dir.join(".env"), "API_KEY=leaked\n").unwrap();

        let seal = Seal {
            capabilities: [Capability::ReadFile].into(),
            scope: vec!["src".into()],
            mcp: Default::default(),
        };
        let sealed = super::super::sealed_tools(&dir, &seal, None).unwrap();
        let tool = |name: &str| {
            sealed
                .tools
                .iter()
                .find(|t| t.spec().name == name)
                .unwrap_or_else(|| panic!("missing tool {name}"))
        };

        // 1. vcs_status 只显示 src/a.rs，不泄露 secret/b.rs 与 .env。
        let st = tool("vcs_status").execute("{}").unwrap();
        assert!(st.contains("[vcs: git]"), "{st}");
        assert!(st.contains("src/a.rs"), "{st}");
        assert!(!st.contains("secret/b.rs"), "{st}");
        assert!(!st.contains(".env"), "{st}");

        // 2. vcs_diff 只包含 src/a.rs 的 diff，绝不含 .env 或 secret/b.rs。
        let diff = tool("vcs_diff").execute("{}").unwrap();
        assert!(diff.contains("-fn a() -> i32 { 1 }"), "{diff}");
        assert!(diff.contains("+fn a() -> i32 { 2 }"), "{diff}");
        assert!(!diff.contains("super-secret"), "{diff}");
        assert!(!diff.contains("leaked"), "{diff}");
        assert!(!diff.contains("secret/b.rs"), "{diff}");

        // 3. stat_only 模式与 rev 注入拦截。
        let stat = tool("vcs_diff").execute(r#"{"stat_only": true}"#).unwrap();
        assert!(stat.contains("src/a.rs"), "{stat}");
        assert!(
            tool("vcs_diff")
                .execute(r#"{"rev": "--output=/tmp/x"}"#)
                .is_err()
        );
        assert!(tool("vcs_diff").execute(r#"{"rev": "HEAD:.env"}"#).is_err());
        assert!(tool("vcs_diff").execute(r#"{"path": ".env"}"#).is_err());
        assert!(tool("vcs_diff").execute(r#"{"path": "secret"}"#).is_err());

        // 4. vcs_log 能读到提交历史并校验 limit 边界。
        let log = tool("vcs_log").execute(r#"{"limit": 5}"#).unwrap();
        assert!(log.contains("initial commit"), "{log}");
        assert!(tool("vcs_log").execute(r#"{"limit": 0}"#).is_err());
        assert!(tool("vcs_log").execute(r#"{"limit": 31}"#).is_err());

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn vcs_tools_fail_cleanly_outside_any_repo() {
        let dir = temp_repo("no-repo");
        fs::write(dir.join("a.txt"), "hello\n").unwrap();
        let seal = Seal {
            capabilities: [Capability::ReadFile].into(),
            scope: vec![".".into()],
            mcp: Default::default(),
        };
        let sealed = super::super::sealed_tools(&dir, &seal, None).unwrap();
        let st = sealed
            .tools
            .iter()
            .find(|t| t.spec().name == "vcs_status")
            .unwrap();
        let err = st.execute("{}").unwrap_err().to_string();
        assert!(err.contains("未检测到 jj 或 git"), "{err}");
        let _ = fs::remove_dir_all(dir);
    }
}
