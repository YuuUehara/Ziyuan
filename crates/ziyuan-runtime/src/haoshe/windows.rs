//! Windows 原生号舍编排：W2 `run_tests`、只读工具链快照准入、W3 长连接 stdio 外藩与命名管道门吏。
//!
//! 本模块保持 `unsafe_code = "forbid"`，所有 Win32 FFI 严格封装在 `ziyuan-sandbox-windows` 中。

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

use super::{
    Network, SandboxUnavailable, ShellRunOutput, ShellRunSpec, WINDOWS_DOOR_PIPE_ENV, proxy,
    windows_sandbox_available,
};
use crate::BoxError;

/// 这些系统与用户目录变量由 `ziyuan-sandbox-windows` 统一注入为 `SystemRoot` 派生值与任务私有路径，
/// 禁止沿用宿主值，以免覆盖容器私有环境或触发保留变量拒绝。
const WINDOWS_RESERVED_ENV: &[&str] = &[
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "SYSTEMDRIVE",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "HOME",
    "APPDATA",
    "HOMEDRIVE",
    "HOMEPATH",
];

const MAX_TOOLCHAIN_DIRS: usize = 8;
const MAX_TOOLCHAIN_FILES: usize = 4096;
const MAX_TOOLCHAIN_BYTES: u64 = 512 * 1024 * 1024;
const MAX_TOOLCHAIN_DEPTH: usize = 16;

const FORBIDDEN_TOOLCHAIN_COMPONENTS: &[&str] = &[
    ".ssh", ".aws", ".gnupg", ".kube", ".docker", ".ziyuan", ".git", ".jj", ".azure", ".gcloud",
    ".config", ".netrc",
];

struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn windows_path_key(path: &Path) -> String {
    path.to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase()
}

fn windows_paths_overlap(a: &Path, b: &Path) -> bool {
    let a = windows_path_key(a);
    let b = windows_path_key(b);
    a == b
        || a.strip_prefix(&b)
            .is_some_and(|rest| rest.starts_with('\\') || rest.starts_with('/'))
        || b.strip_prefix(&a)
            .is_some_and(|rest| rest.starts_with('\\') || rest.starts_with('/'))
}

fn is_forbidden_toolchain_name(name: &std::ffi::OsStr) -> bool {
    let lower = name.to_string_lossy().to_ascii_lowercase();
    lower.starts_with(".env") || FORBIDDEN_TOOLCHAIN_COMPONENTS.contains(&lower.as_str())
}

fn validate_windows_toolchain_dir(
    raw: &Path,
    work_dir: &Path,
    sandbox_base: &Path,
    hidden: &[PathBuf],
) -> Result<PathBuf, BoxError> {
    let reject = |reason: String| -> BoxError {
        SandboxUnavailable::new(
            "run_tests",
            format!("只读工具链准入被拒（{}）：{reason}", raw.display()),
        )
        .into()
    };
    if !raw.is_absolute() {
        return Err(reject("必须是绝对路径".into()));
    }
    let meta =
        fs::symlink_metadata(raw).map_err(|e| reject(format!("目录不存在或无法访问：{e}")))?;
    if is_windows_reparse_point(raw) {
        return Err(reject("拒绝准入重解析点／符号链接／目录联接".into()));
    }
    if !meta.is_dir() {
        return Err(reject("目标不是目录".into()));
    }
    let norm =
        normalize_windows_long_path(raw).map_err(|e| reject(format!("无法规范化目录路径：{e}")))?;
    if norm.parent().is_none() {
        return Err(reject("禁止准入磁盘根目录".into()));
    }
    for comp in norm.components() {
        if let std::path::Component::Normal(part) = comp
            && is_forbidden_toolchain_name(part)
        {
            return Err(reject(format!(
                "路径包含敏感或受控目录段 `{}`",
                part.to_string_lossy()
            )));
        }
    }
    for var in [
        "USERPROFILE",
        "HOME",
        "WINDIR",
        "SYSTEMROOT",
        "APPDATA",
        "LOCALAPPDATA",
        "PROGRAMDATA",
    ] {
        if let Some(val) = std::env::var_os(var)
            && !val.is_empty()
            && let Ok(sensitive) = normalize_windows_long_path(Path::new(&val))
        {
            let norm_key = windows_path_key(&norm);
            let sens_key = windows_path_key(&sensitive);
            let norm_contains_sens = norm_key == sens_key
                || sens_key
                    .strip_prefix(&norm_key)
                    .is_some_and(|rest| rest.starts_with('\\') || rest.starts_with('/'));
            if norm_contains_sens {
                return Err(reject(format!(
                    "禁止准入系统或用户根目录（与 %{var}% 重叠），请改为指定具体工具链子目录"
                )));
            }
        }
    }
    let mut blocked_roots = Vec::with_capacity(hidden.len() + 2);
    if let Ok(p) = normalize_windows_long_path(work_dir) {
        blocked_roots.push(p);
    }
    if let Ok(p) = normalize_windows_long_path(sandbox_base) {
        blocked_roots.push(p);
    }
    for h in hidden {
        if let Ok(p) = normalize_windows_long_path(h) {
            blocked_roots.push(p);
        } else {
            blocked_roots.push(h.clone());
        }
    }
    if blocked_roots
        .iter()
        .any(|b| windows_paths_overlap(&norm, b))
    {
        return Err(reject("不能与测试副本、沙箱工作区或隐藏目录重叠".into()));
    }
    Ok(norm)
}

fn copy_toolchain_bounded(
    src: &Path,
    dst: &Path,
    depth: usize,
    files: &mut usize,
    bytes: &mut u64,
) -> Result<(), BoxError> {
    if depth > MAX_TOOLCHAIN_DEPTH {
        return Err(SandboxUnavailable::new(
            "run_tests",
            format!(
                "只读工具链目录嵌套超过上限（{MAX_TOOLCHAIN_DEPTH} 层）：{}",
                src.display()
            ),
        )
        .into());
    }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if is_windows_reparse_point(&path) {
            return Err(SandboxUnavailable::new(
                "run_tests",
                format!(
                    "只读工具链内包含重解析点／符号链接，失败关闭：{}",
                    path.display()
                ),
            )
            .into());
        }
        if is_forbidden_toolchain_name(&name) {
            return Err(SandboxUnavailable::new(
                "run_tests",
                format!(
                    "只读工具链内包含敏感目录或文件 `{}`，失败关闭",
                    name.to_string_lossy()
                ),
            )
            .into());
        }
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            copy_toolchain_bounded(&path, &dst.join(&name), depth + 1, files, bytes)?;
        } else if meta.is_file() {
            *files += 1;
            if *files > MAX_TOOLCHAIN_FILES {
                return Err(SandboxUnavailable::new(
                    "run_tests",
                    format!("只读工具链文件数超过上限（{MAX_TOOLCHAIN_FILES}）"),
                )
                .into());
            }
            *bytes = bytes.saturating_add(meta.len());
            if *bytes > MAX_TOOLCHAIN_BYTES {
                return Err(SandboxUnavailable::new(
                    "run_tests",
                    format!(
                        "只读工具链总大小超过上限（{} MiB）",
                        MAX_TOOLCHAIN_BYTES / (1024 * 1024)
                    ),
                )
                .into());
            }
            fs::copy(&path, dst.join(&name))?;
        }
    }
    Ok(())
}

struct StagedToolchains {
    _guard: Option<TempDirGuard>,
    readonly_dirs: Vec<PathBuf>,
    path_entries: Vec<PathBuf>,
}

/// 将经校验的宿主工具链目录复制到独立的任务私有只读快照（位于 `sandbox_base` 之外，绝不修改宿主 ACL）。
fn stage_windows_toolchains(spec: &ShellRunSpec<'_>) -> Result<StagedToolchains, BoxError> {
    if spec.readonly_toolchains.is_empty() {
        return Ok(StagedToolchains {
            _guard: None,
            readonly_dirs: Vec::new(),
            path_entries: Vec::new(),
        });
    }
    if spec.readonly_toolchains.len() > MAX_TOOLCHAIN_DIRS {
        return Err(SandboxUnavailable::new(
            "run_tests",
            format!("只读工具链目录数超过上限（最多 {MAX_TOOLCHAIN_DIRS} 个）"),
        )
        .into());
    }
    let mut validated = Vec::with_capacity(spec.readonly_toolchains.len());
    let mut seen = std::collections::BTreeSet::new();
    for raw in spec.readonly_toolchains {
        let norm =
            validate_windows_toolchain_dir(raw, spec.work_dir, spec.sandbox_base, spec.hidden)?;
        if seen.insert(windows_path_key(&norm)) {
            validated.push(norm);
        }
    }
    static NEXT_TC: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tc_root = std::env::temp_dir().join(format!(
        "ziyuan-tc-{}-{}",
        std::process::id(),
        NEXT_TC.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&tc_root)?;
    let guard = TempDirGuard(tc_root.clone());
    let mut readonly_dirs = Vec::with_capacity(validated.len());
    let mut path_entries = Vec::new();
    let mut total_files = 0usize;
    let mut total_bytes = 0u64;
    for (idx, src) in validated.iter().enumerate() {
        let dst = tc_root.join(format!("t{idx}"));
        copy_toolchain_bounded(src, &dst, 0, &mut total_files, &mut total_bytes)?;
        let dst_norm = normalize_windows_long_path(&dst)?;
        let bin = dst_norm.join("bin");
        if bin.is_dir() {
            path_entries.push(bin);
        }
        path_entries.push(dst_norm.clone());
        readonly_dirs.push(dst_norm);
    }
    Ok(StagedToolchains {
        _guard: Some(guard),
        readonly_dirs,
        path_entries,
    })
}

fn prepend_windows_path(env: &mut Vec<(String, String)>, prefixes: &[PathBuf]) {
    if prefixes.is_empty() {
        return;
    }
    let prefix_str = prefixes
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(";");
    if let Some((_, existing)) = env.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case("PATH")) {
        if existing.is_empty() {
            *existing = prefix_str;
        } else {
            *existing = format!("{prefix_str};{existing}");
        }
    } else {
        let sys_default = std::env::var_os("SystemRoot")
            .filter(|v| !v.is_empty())
            .map(|root| {
                let r = root.to_string_lossy();
                format!("{r}\\System32;{r}")
            });
        let value = match sys_default {
            Some(sys) => format!("{prefix_str};{sys}"),
            None => prefix_str,
        };
        env.push(("PATH".into(), value));
    }
}

const WINDOWS_DOOR_PROXY_ENV: &[&str] = &["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"];

pub(super) fn run_shell_windows(spec: &ShellRunSpec<'_>) -> Result<ShellRunOutput, BoxError> {
    if spec.network == Network::Open {
        return Err(SandboxUnavailable::new(
            "run_tests",
            "Windows 原生号舍保持零网络能力（ADR-0001），不支持开放联网；如需访问特定域名请改用 ZIYUAN_SANDBOX_ALLOW",
        )
        .into());
    }
    if !spec.extra_writable.is_empty() {
        return Err(SandboxUnavailable::new(
            "run_tests",
            "Windows 原生号舍不继承外部 CARGO_TARGET_DIR 等宿主可写目录（ADR-0001 禁止修改宿主 ACL），请取消该变量后重试",
        )
        .into());
    }
    if let Err(reason) = windows_sandbox_available() {
        return Err(SandboxUnavailable::new("run_tests", reason.clone()).into());
    }

    let staged = stage_windows_toolchains(spec)?;
    let toolchain_count = staged.readonly_dirs.len();
    let (door, door_audit) = match &spec.network {
        Network::Only(domains) => {
            let (door_spec, audit) =
                proxy::start_windows_door(domains.clone(), WINDOWS_DOOR_PIPE_ENV);
            (Some(door_spec), Some(audit))
        }
        Network::Off => (None, None),
        Network::Open => unreachable!("已前置拦截 Network::Open"),
    };

    let mut writable_dirs = vec![spec.sandbox_base.to_path_buf()];
    if spec.work_dir != spec.sandbox_base && !spec.work_dir.starts_with(spec.sandbox_base) {
        writable_dirs.push(spec.work_dir.to_path_buf());
    }
    let strip_proxy = door.is_some();
    let mut env: Vec<(String, String)> = spec
        .env
        .iter()
        .filter(|(k, _)| {
            let upper = k.to_ascii_uppercase();
            !WINDOWS_RESERVED_ENV.contains(&upper.as_str())
                && upper != WINDOWS_DOOR_PIPE_ENV
                && (!strip_proxy || !WINDOWS_DOOR_PROXY_ENV.contains(&upper.as_str()))
        })
        .map(|(k, v)| (k.clone(), v.to_string_lossy().into_owned()))
        .collect();
    prepend_windows_path(&mut env, &staged.path_entries);
    let capture_limit = spec.max_output.max(spec.max_failure_scan);
    let result = ziyuan_sandbox_windows::run_shell_sandboxed_with_door(
        &ziyuan_sandbox_windows::ShellSpec {
            command: spec.command.to_owned(),
            cwd: spec.work_dir.to_path_buf(),
            writable_dirs,
            readonly_dirs: staged.readonly_dirs,
            env,
            timeout: spec.timeout,
            max_output_bytes: Some(capture_limit),
        },
        spec.cancel,
        door,
    )
    .map_err(|err| -> BoxError {
        if err.kind() == io::ErrorKind::InvalidInput {
            err.into()
        } else {
            SandboxUnavailable::new("run_tests", format!("Windows 原生号舍启动失败：{err}")).into()
        }
    })?;

    let passed = matches!(
        result.outcome,
        ziyuan_sandbox_windows::ExitOutcome::Exited(0)
    );
    let verdict = match result.outcome {
        ziyuan_sandbox_windows::ExitOutcome::Cancelled => "已取消（被主动终止）".to_owned(),
        ziyuan_sandbox_windows::ExitOutcome::TimedOut => {
            format!("超时（{} 秒）被终止", spec.timeout.as_secs())
        }
        ziyuan_sandbox_windows::ExitOutcome::Exited(0) => "通过（退出码 0）".to_owned(),
        ziyuan_sandbox_windows::ExitOutcome::Exited(code) => format!("未通过（退出码 {code}）"),
    };
    let tail_start = result
        .output
        .len()
        .saturating_sub(usize::try_from(spec.max_output).unwrap_or(usize::MAX));
    let tail = String::from_utf8_lossy(&result.output[tail_start..]).into_owned();
    let failure_scan = if passed {
        None
    } else {
        let scan_start = result
            .output
            .len()
            .saturating_sub(usize::try_from(spec.max_failure_scan).unwrap_or(usize::MAX));
        Some(String::from_utf8_lossy(&result.output[scan_start..]).into_owned())
    };
    let mut haoshe_note = format!(
        "号舍（Windows AppContainer＋Job）：{}，只可写测试副本与任务私有目录",
        spec.network.describe()
    );
    if toolchain_count > 0 {
        haoshe_note.push_str(&format!("；准入只读工具链 {toolchain_count} 处"));
    }
    if let Some(audit) = door_audit {
        let denied = audit.denied();
        if !denied.is_empty() {
            haoshe_note.push_str(&format!("；门吏已拦下：{}", denied.join("、")));
        }
    }
    if !result.cleanup_warnings.is_empty() {
        haoshe_note.push_str(&format!(
            "；清理提示：{}",
            result.cleanup_warnings.join("；")
        ));
    }
    Ok(ShellRunOutput {
        passed,
        verdict,
        haoshe_note,
        tail,
        tail_truncated: result.total_output_bytes > spec.max_output,
        failure_scan,
    })
}

/// Windows 原生号舍 W3（长连接断网 stdio 外藩）启动规格。
pub struct StdioRunSpec<'a> {
    /// 操作名称，供失败关闭错误消息使用（如 `外藩 docs`）。
    pub operation: &'a str,
    pub program: &'a str,
    pub args: &'a [String],
    /// 宿主项目根目录：只复制进任务私有只读快照（跳过 `.ziyuan`、`.git`、`.jj`、`.env*` 与构建产物），绝不修改宿主 ACL。
    pub root: &'a Path,
    pub network: &'a Network,
    /// 名册中请求的额外可写路径：Windows 下仅允许项目内相对子目录（映射到任务私有快照子目录，
    /// 绝不修改宿主目录 ACL，拒绝 `~/`、绝对路径、`..`、重解析点与敏感目录段）。
    pub extra_write: &'a [String],
    /// 已合并白名单与名册展开值的环境变量。
    pub env: &'a [(String, String)],
}

/// Windows 原生号舍 W3 守护的双向 stdio 子进程句柄；停服或 `Drop` 时先排空整棵 Job 进程树，再清理任务私有目录。
pub struct WindowsStdioChild {
    sandbox: ziyuan_sandbox_windows::SandboxedStdio,
    base_dir: PathBuf,
    door_audit: Option<proxy::PipeDoorAudit>,
}

impl WindowsStdioChild {
    pub fn take_stdin(&mut self) -> Option<fs::File> {
        self.sandbox.take_stdin()
    }

    pub fn take_stdout(&mut self) -> Option<fs::File> {
        self.sandbox.take_stdout()
    }

    /// 若启用按域名放行门吏，返回至今被门吏拦下的域名列表。
    pub fn denied_domains(&self) -> Vec<String> {
        self.door_audit
            .as_ref()
            .map_or_else(Vec::new, proxy::PipeDoorAudit::denied)
    }

    pub fn shutdown(&mut self) {
        let _ = self.sandbox.shutdown();
        let _ = fs::remove_dir_all(&self.base_dir);
    }
}

impl Drop for WindowsStdioChild {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn is_windows_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    fs::symlink_metadata(path)
        .map(|meta| meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .unwrap_or(false)
}

fn normalize_windows_long_path(path: &Path) -> io::Result<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    if let Some(raw) = canonical.to_str()
        && let Some(stripped) = raw.strip_prefix(r"\\?\")
    {
        let bytes = stripped.as_bytes();
        if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\'
        {
            return Ok(PathBuf::from(stripped));
        }
    }
    Ok(canonical)
}

fn resolve_windows_program(
    operation: &str,
    program: &str,
    snapshot_dir: &Path,
) -> Result<PathBuf, BoxError> {
    let trimmed = program.trim();
    if trimmed.is_empty() {
        return Err(format!("{operation} 启动失败：command 不能为空").into());
    }
    let raw = Path::new(trimmed);
    let check_candidate = |candidate: PathBuf| -> Option<Result<PathBuf, String>> {
        if !candidate.exists() {
            return None;
        }
        if is_windows_reparse_point(&candidate) {
            return Some(Err(format!(
                "可执行文件是重解析点／应用别名（{}），Windows AppContainer 拒绝直接启动",
                candidate.display()
            )));
        }
        if candidate.is_file() {
            Some(Ok(candidate))
        } else {
            None
        }
    };

    if raw.is_absolute() {
        let mut attempts = vec![raw.to_path_buf()];
        if raw.extension().is_none() {
            attempts.push(raw.with_extension("exe"));
        }
        let mut reparse_err = None;
        for candidate in attempts {
            match check_candidate(candidate) {
                Some(Ok(found)) => return Ok(found),
                Some(Err(reason)) => reparse_err = Some(reason),
                None => {}
            }
        }
        if let Some(reason) = reparse_err {
            return Err(SandboxUnavailable::new(operation, reason).into());
        }
        return Err(format!("{operation} 启动失败（{program}）：找不到可执行文件").into());
    }

    if trimmed.contains(['\\', '/']) {
        let joined = snapshot_dir.join(raw);
        let mut attempts = vec![joined.clone()];
        if joined.extension().is_none() {
            attempts.push(joined.with_extension("exe"));
        }
        for candidate in attempts {
            if let Some(res) = check_candidate(candidate) {
                let found = res.map_err(|reason| SandboxUnavailable::new(operation, reason))?;
                let norm_snap = normalize_windows_long_path(snapshot_dir)?;
                let norm_found = normalize_windows_long_path(&found)?;
                if !norm_found.starts_with(&norm_snap) {
                    return Err(SandboxUnavailable::new(
                        operation,
                        format!("相对路径 {program} 越出项目快照目录"),
                    )
                    .into());
                }
                return Ok(norm_found);
            }
        }
        return Err(
            format!("{operation} 启动失败（{program}）：项目快照中找不到该可执行文件").into(),
        );
    }

    let has_ext = raw.extension().is_some();
    let mut names = Vec::new();
    if has_ext {
        names.push(trimmed.to_owned());
    } else {
        names.push(format!("{trimmed}.exe"));
        names.push(format!("{trimmed}.com"));
        names.push(trimmed.to_owned());
    }
    let mut reparse_err = None;
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for name in &names {
                let candidate = dir.join(name);
                match check_candidate(candidate) {
                    Some(Ok(found)) => return Ok(found),
                    Some(Err(reason)) if reparse_err.is_none() => {
                        reparse_err = Some(reason);
                    }
                    Some(Err(_)) | None => {}
                }
            }
        }
    }
    if let Some(reason) = reparse_err {
        return Err(SandboxUnavailable::new(operation, reason).into());
    }
    Err(format!("{operation} 启动失败（{program}）：在 PATH 中找不到可执行文件").into())
}

fn resolve_windows_extra_write_dirs(
    operation: &str,
    root: &Path,
    workspace_dir: &Path,
    extra_write: &[String],
) -> Result<Vec<PathBuf>, BoxError> {
    let reject = |raw: &str, reason: String| -> BoxError {
        SandboxUnavailable::new(
            operation,
            format!("Windows 原生号舍 haoshe.write 被拒（{raw:?}）：{reason}"),
        )
        .into()
    };
    let norm_workspace = normalize_windows_long_path(workspace_dir)?;
    let ws_key = windows_path_key(&norm_workspace);
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    for raw in extra_write {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(reject(raw, "路径不能为空".into()));
        }
        if trimmed == "~" || trimmed.starts_with("~/") || trimmed.starts_with("~\\") {
            return Err(reject(
                raw,
                "ADR-0001 禁止把家目录（~/…）准入可写，以免改写宿主 ACL 或暴露宿主目录".into(),
            ));
        }
        if trimmed.contains('\0')
            || trimmed.chars().any(|c| c.is_control())
            || trimmed.contains(':')
            || trimmed.starts_with(['/', '\\'])
        {
            return Err(reject(
                raw,
                "仅允许项目内相对子目录，拒绝绝对路径、盘符、UNC 或控制字符".into(),
            ));
        }
        let rel = Path::new(trimmed);
        if rel.is_absolute() {
            return Err(reject(raw, "仅允许项目内相对子目录，拒绝绝对路径".into()));
        }
        let mut clean_rel = PathBuf::new();
        for comp in rel.components() {
            match comp {
                std::path::Component::Normal(part) => {
                    let lower = part.to_string_lossy().to_ascii_lowercase();
                    if is_forbidden_toolchain_name(part)
                        || matches!(lower.as_str(), "target" | "node_modules")
                    {
                        return Err(reject(
                            raw,
                            format!(
                                "路径包含敏感或禁止写入的目录段 `{}`",
                                part.to_string_lossy()
                            ),
                        ));
                    }
                    clean_rel.push(part);
                }
                std::path::Component::CurDir => {
                    return Err(reject(
                        raw,
                        "不能把项目快照根目录（`.`）整体设为可写，请指定具体子目录".into(),
                    ));
                }
                std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_) => {
                    return Err(reject(raw, "路径不得包含 `..` 或根前缀".into()));
                }
            }
        }
        if clean_rel.as_os_str().is_empty() {
            return Err(reject(raw, "必须指定项目内具体子目录".into()));
        }

        let mut host_probe = root.to_path_buf();
        for comp in clean_rel.components() {
            host_probe.push(comp.as_os_str());
            if let Ok(meta) = fs::symlink_metadata(&host_probe) {
                if is_windows_reparse_point(&host_probe) {
                    return Err(reject(
                        raw,
                        format!("宿主路径包含重解析点／符号链接：{}", host_probe.display()),
                    ));
                }
                if !meta.is_dir() {
                    return Err(reject(
                        raw,
                        format!("宿主对应路径不是目录：{}", host_probe.display()),
                    ));
                }
            }
        }

        let mut snap_probe = workspace_dir.to_path_buf();
        for comp in clean_rel.components() {
            snap_probe.push(comp.as_os_str());
            if let Ok(meta) = fs::symlink_metadata(&snap_probe) {
                if is_windows_reparse_point(&snap_probe) {
                    return Err(reject(
                        raw,
                        format!("快照路径包含重解析点：{}", snap_probe.display()),
                    ));
                }
                if !meta.is_dir() {
                    return Err(reject(
                        raw,
                        format!(
                            "快照中已存在同名文件，无法作为可写目录：{}",
                            snap_probe.display()
                        ),
                    ));
                }
            } else {
                fs::create_dir(&snap_probe)?;
            }
        }
        let norm_target = normalize_windows_long_path(&snap_probe)?;
        let target_key = windows_path_key(&norm_target);
        if target_key == ws_key
            || !target_key
                .strip_prefix(&ws_key)
                .is_some_and(|rest| rest.starts_with('\\') || rest.starts_with('/'))
        {
            return Err(reject(raw, "目标目录越出任务私有项目快照".into()));
        }
        if seen.insert(target_key) {
            out.push(norm_target);
        }
    }
    Ok(out)
}

/// 在 Windows 原生号舍 W3（零网络能力 AppContainer ＋ `KILL_ON_JOB_CLOSE` Job Object）中启动长连接 stdio 外藩。
///
/// 严格遵循 ADR-0001：
/// - 保持 `TokenCapabilities == 0`、零回环豁免、零宿主 ACL 变更；
/// - `Network::Off` 全断网，`Network::Only(domains)` 经按任务 SID 隔离的命名管道门吏＋容器内桥接只放行名单域名 HTTP(S)，`Network::Open` 失败关闭；
/// - 宿主项目根只复制到任务私有只读快照（跳过 `.ziyuan`、`.git`、`.jj`、`.env*` 与构建产物），`extra_write` 经严格校验后仅映射为任务私有快照内对应子目录可写（拒绝 `~/`、绝对路径、`..`、重解析点与敏感目录段）；
/// - 任务私有 `scratch`（同步写入 `TMPDIR`）与容器私有 `TEMP`/`TMP`/`HOME` 可写，停服或 `Drop` 时整树排空清理。
pub fn spawn_stdio_windows(spec: &StdioRunSpec<'_>) -> Result<WindowsStdioChild, BoxError> {
    if *spec.network == Network::Open {
        return Err(SandboxUnavailable::new(
            spec.operation,
            "Windows 原生号舍 W3 保持零网络能力（ADR-0001），不支持开放联网（haoshe.network: true）；请改用域名白名单数组或 false",
        )
        .into());
    }
    if let Err(reason) = windows_sandbox_available() {
        return Err(SandboxUnavailable::new(spec.operation, reason.clone()).into());
    }

    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base_dir = std::env::temp_dir().join(format!(
        "ziyuan-mcp-win-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let workspace_dir = base_dir.join("workspace");
    let scratch_dir = base_dir.join("scratch");

    let launch = (|| -> Result<WindowsStdioChild, BoxError> {
        fs::create_dir_all(&scratch_dir)?;
        crate::tools::copy_project_tree(spec.root, &workspace_dir)?;
        let extra_writable = resolve_windows_extra_write_dirs(
            spec.operation,
            spec.root,
            &workspace_dir,
            spec.extra_write,
        )?;
        let (door, door_audit) = match spec.network {
            Network::Only(domains) => {
                let (door_spec, audit) =
                    proxy::start_windows_door(domains.clone(), WINDOWS_DOOR_PIPE_ENV);
                (Some(door_spec), Some(audit))
            }
            Network::Off => (None, None),
            Network::Open => unreachable!("已前置拦截 Network::Open"),
        };
        let scratch_long = normalize_windows_long_path(&scratch_dir)?;
        let program = resolve_windows_program(spec.operation, spec.program, &workspace_dir)?;
        let cwd = normalize_windows_long_path(&workspace_dir)?;

        let strip_proxy = door.is_some();
        let mut env: Vec<(String, String)> = spec
            .env
            .iter()
            .filter(|(k, _)| {
                let upper = k.to_ascii_uppercase();
                !WINDOWS_RESERVED_ENV.contains(&upper.as_str())
                    && upper != "TMPDIR"
                    && upper != WINDOWS_DOOR_PIPE_ENV
                    && (!strip_proxy || !WINDOWS_DOOR_PROXY_ENV.contains(&upper.as_str()))
            })
            .cloned()
            .collect();
        env.push(("TMPDIR".into(), scratch_long.to_string_lossy().into_owned()));

        let mut writable_dirs = vec![scratch_long];
        writable_dirs.extend(extra_writable);

        let sandbox = ziyuan_sandbox_windows::spawn_stdio_sandboxed_with_door(
            &ziyuan_sandbox_windows::StdioSpec {
                program,
                args: spec.args.to_vec(),
                cwd: Some(cwd.clone()),
                writable_dirs,
                readonly_dirs: vec![cwd],
                env,
                mode: ziyuan_sandbox_windows::SandboxMode::AppContainerChildCapable,
            },
            door,
        )
        .map_err(|err| -> BoxError {
            if err.kind() == io::ErrorKind::InvalidInput {
                err.into()
            } else {
                SandboxUnavailable::new(
                    spec.operation,
                    format!(
                        "Windows 原生号舍 W3 启动失败：{err}（若程序安装在当前用户目录如 AppData，AppContainer 默认无读取权限，请改用系统目录安装的可执行文件）"
                    ),
                )
                .into()
            }
        })?;

        Ok(WindowsStdioChild {
            sandbox,
            base_dir: base_dir.clone(),
            door_audit,
        })
    })();

    if launch.is_err() {
        let _ = fs::remove_dir_all(&base_dir);
    }
    launch
}
