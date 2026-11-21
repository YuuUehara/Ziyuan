//! 号舍：把 `run_tests` 关进操作系统的沙箱。
//!
//! 取贡院号舍之意：考生锁入单间，与外界隔绝。测试命令可能运行模型写入的代码，入号舍后
//! 不能联网，只能写测试副本所在的临时目录，读不到家目录下常见的凭据（`.ssh`、`.aws` 等）
//! 与项目的 `.ziyuan`。Linux 用 bubblewrap（`bwrap`），macOS 用系统自带的 Seatbelt
//! （`sandbox-exec`）；Windows `run_tests`（W2）与断网 stdio 外藩（W3）用原生 AppContainer＋Job Object（`ziyuan-sandbox-windows`，默认全断网，见 `docs/adr/0001-windows-sandbox.md`）。
//!
//! 由 `ZIYUAN_SANDBOX` 决定：`auto`（默认）与 `require` 都是有则用、无则拒绝运行
//! （[`SandboxUnavailable`]），绝不悄悄退化为未入号舍执行；`off` 为皇帝确认风险后不入号舍，
//! 结果中如实注明。`ZIYUAN_SANDBOX_ALLOW` 列出可访问的域名时，号舍仍断网，只经门吏
//! （[`proxy`]）放行这些域名的 HTTP(S)。
mod proxy;

pub use proxy::Proxy;

use crate::BoxError;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// 家目录下读不得的凭据目录。
const SECRET_DIRS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".docker",
    ".kube",
    ".config/gh",
    ".azure",
];

/// 号舍的用法，取自 `ZIYUAN_SANDBOX`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 默认：有号舍则入，没有则拒绝。
    Auto,
    /// 与 `Auto` 相同，供想写明意图的配置沿用。
    Require,
    /// 不入号舍。
    Off,
}

/// 受保护的执行须入号舍而本机号舍不可用：拒绝运行，不照常执行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxUnavailable {
    /// `std::env::consts::OS`，如 `windows`、`linux`。
    pub platform: &'static str,
    /// 被拒的操作，如 `run_tests` 或 `外藩 名称`。
    pub operation: String,
    pub reason: String,
}

impl SandboxUnavailable {
    pub fn new(operation: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            platform: std::env::consts::OS,
            operation: operation.into(),
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for SandboxUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "号舍不可用（{}），{} 须入号舍，不照常运行：{}。确认风险后可设 ZIYUAN_SANDBOX=off 不入号舍运行",
            self.platform, self.operation, self.reason
        )
    }
}

impl std::error::Error for SandboxUnavailable {}

impl Mode {
    pub fn from_env() -> Result<Self, BoxError> {
        match std::env::var("ZIYUAN_SANDBOX").ok().as_deref() {
            None | Some("" | "auto") => Ok(Self::Auto),
            Some("require") => Ok(Self::Require),
            Some("off") => Ok(Self::Off),
            Some(other) => {
                Err(format!("ZIYUAN_SANDBOX 只认 auto、require、off，而不是「{other}」").into())
            }
        }
    }
}

/// 号舍内的网络。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Network {
    /// 全断。
    #[default]
    Off,
    /// 照常联网。
    Open,
    /// 全断，只经门吏放行这些域名（`example.com` 或 `*.example.com`）的 HTTP(S)。
    Only(Vec<String>),
}

/// 门吏在号舍内的端口（Linux 号舍有自己的网络命名空间，不与外面冲突）。
#[cfg(target_os = "linux")]
const DOOR_PORT: u16 = 3128;

impl Network {
    /// `run_tests` 的网络：`ZIYUAN_SANDBOX_ALLOW` 以逗号或空白分隔域名；未设为全断。
    pub fn from_env() -> Result<Self, BoxError> {
        let list = std::env::var("ZIYUAN_SANDBOX_ALLOW").unwrap_or_default();
        let domains: Vec<String> = list
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|d| !d.is_empty())
            .map(str::to_owned)
            .collect();
        if domains.is_empty() {
            return Ok(Self::Off);
        }
        Self::only(domains).map_err(|e| format!("ZIYUAN_SANDBOX_ALLOW：{e}").into())
    }

    /// 按名单放行；名单项须为域名或 `*.` 加域名。
    pub fn only(domains: Vec<String>) -> Result<Self, String> {
        for domain in &domains {
            let bare = domain.strip_prefix("*.").unwrap_or(domain);
            let ok = !bare.is_empty()
                && bare.contains('.')
                && bare
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
            if !ok {
                return Err(format!(
                    "「{domain}」不是域名（写 example.com 或 *.example.com）"
                ));
            }
        }
        Ok(Self::Only(domains))
    }

    fn describe(&self) -> String {
        match self {
            Self::Off => "不可联网".into(),
            Self::Open => "可联网".into(),
            Self::Only(domains) => format!("只放行 {}", domains.join("、")),
        }
    }
}

/// 本机可用的号舍；没有时为 `Err(原因)`。只探测一次。
fn available() -> &'static Result<PathBuf, String> {
    static FOUND: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    FOUND.get_or_init(|| {
        if cfg!(target_os = "macos") {
            let exec = PathBuf::from("/usr/bin/sandbox-exec");
            return if exec.exists() {
                Ok(exec)
            } else {
                Err("找不到 /usr/bin/sandbox-exec".into())
            };
        }
        if !cfg!(target_os = "linux") {
            return Err("本系统通用命令包装暂仅支持 Linux 与 macOS（Windows run_tests 经 run_shell 使用原生 AppContainer 号舍）".into());
        }
        let Some(bwrap) = find_in_path("bwrap") else {
            return Err("未找到 bubblewrap（bwrap），可用包管理器安装".into());
        };
        // 有些系统禁了非特权用户命名空间：先试一次。
        match Command::new(&bwrap)
            .args(["--ro-bind", "/", "/", "--unshare-net", "true"])
            .output()
        {
            Ok(out) if out.status.success() => Ok(bwrap),
            Ok(out) => Err(format!(
                "bubblewrap 无法运行：{}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(e) => Err(format!("bubblewrap 无法运行：{e}")),
        }
    })
}

/// Windows 原生号舍（AppContainer＋Job Object）可用性探测，只探测一次。
#[cfg(windows)]
fn windows_sandbox_available() -> &'static Result<(), String> {
    static FOUND: OnceLock<Result<(), String>> = OnceLock::new();
    FOUND.get_or_init(|| match ziyuan_sandbox_windows::probe_capability() {
        ziyuan_sandbox_windows::SandboxCapability::FileSystemAndNetworkIsolated => Ok(()),
        ziyuan_sandbox_windows::SandboxCapability::RestrictedProcess => {
            Err("本机仅达 RestrictedProcess 级别，未达文件与网络隔离要求".into())
        }
        ziyuan_sandbox_windows::SandboxCapability::Unavailable { reason } => Err(reason),
    })
}

/// 本机是否具备 `run_shell` 所需的原生号舍（Linux `bwrap`、macOS `sandbox-exec`、Windows AppContainer）。
#[cfg(test)]
pub(crate) fn shell_sandbox_available() -> bool {
    #[cfg(windows)]
    {
        windows_sandbox_available().is_ok()
    }
    #[cfg(not(windows))]
    {
        available().is_ok()
    }
}

/// 号舍的规矩：可写哪些目录、读不到哪些目录、能否联网。
#[derive(Debug, Clone, Default)]
pub struct Rules {
    /// 只读可见的目录：`/tmp` 换成空的 tmpfs 后，其下的项目等须重新露出。
    pub readable: Vec<PathBuf>,
    pub writable: Vec<PathBuf>,
    /// 另行隐去的目录；家目录下的凭据目录（`SECRET_DIRS`）总是隐去。
    pub hidden: Vec<PathBuf>,
    pub network: Network,
}

/// 号舍里的命令：须在命令结束前留着 `proxy`（按域名放行时的门吏）。
pub struct Confined {
    pub command: Command,
    /// 给审计看的说明。
    pub note: String,
    pub proxy: Option<Proxy>,
}

/// 用系统的 shell 运行 `command`，按 `mode` 关进号舍（网络按 `network`，只可写 `writable` 与
/// `extra_writable`，如 `CARGO_TARGET_DIR`；`hidden` 读不到）。号舍不可用且 `mode` 不是 `Off`
/// 时返回 [`SandboxUnavailable`]。
pub fn shell(
    command: &str,
    mode: Mode,
    network: Network,
    writable: &Path,
    extra_writable: &[PathBuf],
    hidden: &[PathBuf],
) -> Result<Confined, BoxError> {
    let plain = |note: String| Confined {
        command: plain_shell(command),
        note,
        proxy: None,
    };
    if mode == Mode::Off {
        return Ok(plain("未入号舍（ZIYUAN_SANDBOX=off）".into()));
    }
    let mut writable_dirs = vec![writable.to_path_buf()];
    writable_dirs.extend_from_slice(extra_writable);
    let rules = Rules {
        readable: Vec::new(),
        writable: writable_dirs,
        hidden: hidden.to_vec(),
        network,
    };
    let args = ["-c".to_owned(), command.to_owned()];
    confine("/bin/sh", &args, &rules)
        .map_err(|reason| SandboxUnavailable::new("run_tests", reason).into())
}

/// 一次受控 shell 执行的完整规格（W2 所有权边界：由号舍统一拥有沙箱句柄、门吏代理、超时与输出采集）。
pub struct ShellRunSpec<'a> {
    pub command: &'a str,
    pub mode: Mode,
    pub network: Network,
    /// 命令的工作目录（如测试副本根目录）。
    pub work_dir: &'a Path,
    /// 本次执行的任务私有可写根目录（`output.log` 与 Unix `TMPDIR` 亦落于此）。
    pub sandbox_base: &'a Path,
    /// 额外可写目录（如 `CARGO_TARGET_DIR`；Windows 原生号舍首批按 ADR-0001 显式拒绝）。
    pub extra_writable: &'a [PathBuf],
    /// 显式准入的只读工具链目录（由 `ZIYUAN_SANDBOX_TOOLCHAIN` 传入）。
    /// Windows 原生号舍通过任务私有只读快照复制准入并置入容器 `PATH` 头部，绝不改写宿主目录 ACL。
    pub readonly_toolchains: &'a [PathBuf],
    /// 号舍内不可读的目录（如项目 `.ziyuan`）。
    pub hidden: &'a [PathBuf],
    /// 允许继承的环境变量白名单。
    pub env: &'a [(String, OsString)],
    pub timeout: Duration,
    pub max_output: u64,
    pub max_failure_scan: u64,
    /// 可选主动取消信号；触发后立即终止子进程树并返回已取消状态。
    pub cancel: Option<&'a AtomicBool>,
}

/// [`run_shell`] 的执行结果：不向上层泄漏子进程、Job 或门吏代理句柄。
#[derive(Debug, Clone)]
pub struct ShellRunOutput {
    pub passed: bool,
    pub verdict: String,
    pub haoshe_note: String,
    pub tail: String,
    pub tail_truncated: bool,
    /// 仅在 `!passed` 时采集的末尾扫描窗口，供失败／编译诊断摘录使用。
    pub failure_scan: Option<String>,
}

/// 由号舍拥有完整进程生命周期的受控 shell 执行入口（ADR-0001 §平台边界 #2）。
///
/// 统一负责：平台前置策略核验、沙箱构造（Linux `bwrap`／macOS `sandbox-exec`／Windows 原生
/// `AppContainer＋Job`）、门吏代理生命周期、受限环境注入、有界超时／取消终止与末尾输出采集，
/// 上层 `run_tests` 无需直接操作裸 `Command`/`Child` 或平台句柄。
pub fn run_shell(spec: &ShellRunSpec<'_>) -> Result<ShellRunOutput, BoxError> {
    #[cfg(windows)]
    if spec.mode != Mode::Off {
        return run_shell_windows(spec);
    }

    let log_path = spec.sandbox_base.join("output.log");
    let log = fs::File::create(&log_path)?;
    let Confined {
        command: mut child,
        note: mut haoshe_note,
        proxy,
    } = shell(
        spec.command,
        spec.mode,
        spec.network.clone(),
        spec.sandbox_base,
        spec.extra_writable,
        spec.hidden,
    )?;
    child
        .current_dir(spec.work_dir)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    for (key, value) in spec.env {
        child.env(key, value);
    }
    // 临时文件写在副本旁，号舍里也写得了，事后一并清除。
    #[cfg(unix)]
    {
        let tmp = spec.sandbox_base.join("tmp");
        fs::create_dir_all(&tmp)?;
        child.env("TMPDIR", tmp);
    }
    enum WaitOutcome {
        Exited(std::process::ExitStatus),
        TimedOut,
        Cancelled,
    }
    let mut child = child.spawn()?;
    let started = Instant::now();
    let outcome = loop {
        if let Some(status) = child.try_wait()? {
            break WaitOutcome::Exited(status);
        }
        if spec.cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            let _ = child.kill();
            let _ = child.wait();
            break WaitOutcome::Cancelled;
        }
        if started.elapsed() > spec.timeout {
            let _ = child.kill();
            let _ = child.wait();
            break WaitOutcome::TimedOut;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if let Some(denied) = proxy.map(|p| p.denied()).filter(|d| !d.is_empty()) {
        haoshe_note.push_str(&format!("；拦下 {}", denied.join("、")));
    }
    let mut log = fs::File::open(&log_path)?;
    let len = log.metadata()?.len();
    let tail = read_lossy_from(&mut log, len.saturating_sub(spec.max_output))?;
    let passed = matches!(outcome, WaitOutcome::Exited(s) if s.success());
    let verdict = match outcome {
        WaitOutcome::Cancelled => "已取消（被主动终止）".to_owned(),
        WaitOutcome::TimedOut => format!("超时（{} 秒）被终止", spec.timeout.as_secs()),
        WaitOutcome::Exited(s) if s.success() => "通过（退出码 0）".to_owned(),
        WaitOutcome::Exited(s) => match s.code() {
            Some(code) => format!("未通过（退出码 {code}）"),
            None => "未通过（被信号终止）".to_owned(),
        },
    };
    let failure_scan = if passed {
        None
    } else {
        Some(read_lossy_from(
            &mut log,
            len.saturating_sub(spec.max_failure_scan),
        )?)
    };
    Ok(ShellRunOutput {
        passed,
        verdict,
        haoshe_note,
        tail,
        tail_truncated: len > spec.max_output,
        failure_scan,
    })
}

/// 显式只读工具链准入环境变量名（Windows 用 `;` 分隔，Unix 用 `:` 分隔）。
pub const TOOLCHAIN_ENV: &str = "ZIYUAN_SANDBOX_TOOLCHAIN";

/// 从 `ZIYUAN_SANDBOX_TOOLCHAIN` 读取用户显式声明的只读工具链目录列表（跳过空项）。
pub fn toolchain_dirs_from_env() -> Vec<PathBuf> {
    let Some(raw) = std::env::var_os(TOOLCHAIN_ENV) else {
        return Vec::new();
    };
    std::env::split_paths(&raw)
        .filter(|p| !p.as_os_str().is_empty())
        .collect()
}

/// 这些系统与用户目录变量由 `ziyuan-sandbox-windows` 统一注入为 `SystemRoot` 派生值与任务私有路径，
/// 禁止沿用宿主值，以免覆盖容器私有环境或触发保留变量拒绝。
#[cfg(windows)]
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

#[cfg(windows)]
const MAX_TOOLCHAIN_DIRS: usize = 8;
#[cfg(windows)]
const MAX_TOOLCHAIN_FILES: usize = 4096;
#[cfg(windows)]
const MAX_TOOLCHAIN_BYTES: u64 = 512 * 1024 * 1024;
#[cfg(windows)]
const MAX_TOOLCHAIN_DEPTH: usize = 16;

#[cfg(windows)]
const FORBIDDEN_TOOLCHAIN_COMPONENTS: &[&str] = &[
    ".ssh", ".aws", ".gnupg", ".kube", ".docker", ".ziyuan", ".git", ".jj", ".azure", ".gcloud",
    ".config", ".netrc",
];

#[cfg(windows)]
struct TempDirGuard(PathBuf);

#[cfg(windows)]
impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(windows)]
fn windows_path_key(path: &Path) -> String {
    path.to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase()
}

#[cfg(windows)]
fn windows_paths_overlap(a: &Path, b: &Path) -> bool {
    let a = windows_path_key(a);
    let b = windows_path_key(b);
    a == b
        || a.strip_prefix(&b)
            .is_some_and(|rest| rest.starts_with('\\') || rest.starts_with('/'))
        || b.strip_prefix(&a)
            .is_some_and(|rest| rest.starts_with('\\') || rest.starts_with('/'))
}

#[cfg(windows)]
fn is_forbidden_toolchain_name(name: &std::ffi::OsStr) -> bool {
    let lower = name.to_string_lossy().to_ascii_lowercase();
    lower.starts_with(".env") || FORBIDDEN_TOOLCHAIN_COMPONENTS.contains(&lower.as_str())
}

#[cfg(windows)]
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

#[cfg(windows)]
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

#[cfg(windows)]
struct StagedToolchains {
    _guard: Option<TempDirGuard>,
    readonly_dirs: Vec<PathBuf>,
    path_entries: Vec<PathBuf>,
}

/// 将经校验的宿主工具链目录复制到独立的任务私有只读快照（位于 `sandbox_base` 之外，绝不修改宿主 ACL）。
#[cfg(windows)]
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

#[cfg(windows)]
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

#[cfg(windows)]
fn run_shell_windows(spec: &ShellRunSpec<'_>) -> Result<ShellRunOutput, BoxError> {
    if spec.network != Network::Off {
        return Err(SandboxUnavailable::new(
            "run_tests",
            "Windows 原生号舍 W2 首批仅支持全断网（Network::Off），暂不支持 ZIYUAN_SANDBOX_ALLOW 按域名放行或开放联网",
        )
        .into());
    }
    if !spec.extra_writable.is_empty() {
        return Err(SandboxUnavailable::new(
            "run_tests",
            "Windows 原生号舍 W2 首批不继承外部 CARGO_TARGET_DIR 等额外可写目录，请取消该变量后重试",
        )
        .into());
    }
    if let Err(reason) = windows_sandbox_available() {
        return Err(SandboxUnavailable::new("run_tests", reason.clone()).into());
    }

    let staged = stage_windows_toolchains(spec)?;
    let toolchain_count = staged.readonly_dirs.len();

    let mut writable_dirs = vec![spec.sandbox_base.to_path_buf()];
    if spec.work_dir != spec.sandbox_base && !spec.work_dir.starts_with(spec.sandbox_base) {
        writable_dirs.push(spec.work_dir.to_path_buf());
    }
    let mut env: Vec<(String, String)> = spec
        .env
        .iter()
        .filter(|(k, _)| {
            let upper = k.to_ascii_uppercase();
            !WINDOWS_RESERVED_ENV.contains(&upper.as_str())
        })
        .map(|(k, v)| (k.clone(), v.to_string_lossy().into_owned()))
        .collect();
    prepend_windows_path(&mut env, &staged.path_entries);
    let capture_limit = spec.max_output.max(spec.max_failure_scan);
    let result = ziyuan_sandbox_windows::run_shell_sandboxed(
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
    let mut haoshe_note =
        "号舍（Windows AppContainer＋Job）：不可联网，只可写测试副本与任务私有目录".to_owned();
    if toolchain_count > 0 {
        haoshe_note.push_str(&format!("；准入只读工具链 {toolchain_count} 处"));
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
#[cfg(windows)]
pub struct StdioRunSpec<'a> {
    /// 操作名称，供失败关闭错误消息使用（如 `外藩 docs`）。
    pub operation: &'a str,
    pub program: &'a str,
    pub args: &'a [String],
    /// 宿主项目根目录：只复制进任务私有只读快照（跳过 `.ziyuan`、`.git`、`.jj`、`.env*` 与构建产物），绝不修改宿主 ACL。
    pub root: &'a Path,
    pub network: &'a Network,
    /// 名册中请求的额外可写路径（W3 首批保持失败关闭）。
    pub extra_write: &'a [String],
    /// 已合并白名单与名册展开值的环境变量。
    pub env: &'a [(String, String)],
}

/// Windows 原生号舍 W3 守护的双向 stdio 子进程句柄；停服或 `Drop` 时先排空整棵 Job 进程树，再清理任务私有目录。
#[cfg(windows)]
pub struct WindowsStdioChild {
    sandbox: ziyuan_sandbox_windows::SandboxedStdio,
    base_dir: PathBuf,
}

#[cfg(windows)]
impl WindowsStdioChild {
    pub fn take_stdin(&mut self) -> Option<fs::File> {
        self.sandbox.take_stdin()
    }

    pub fn take_stdout(&mut self) -> Option<fs::File> {
        self.sandbox.take_stdout()
    }

    pub fn shutdown(&mut self) {
        let _ = self.sandbox.shutdown();
        let _ = fs::remove_dir_all(&self.base_dir);
    }
}

#[cfg(windows)]
impl Drop for WindowsStdioChild {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(windows)]
fn is_windows_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    fs::symlink_metadata(path)
        .map(|meta| meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
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

#[cfg(windows)]
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

/// 在 Windows 原生号舍 W3（零网络能力 AppContainer ＋ `KILL_ON_JOB_CLOSE` Job Object）中启动长连接 stdio 外藩。
///
/// 严格遵循 ADR-0001：
/// - 仅支持全断网（`Network::Off`）且不把宿主目录准入可写（`extra_write` 为空），其余策略失败关闭；
/// - 宿主项目根只复制到任务私有只读快照（跳过 `.ziyuan`、`.git`、`.jj`、`.env*` 与构建产物），绝不改写宿主 ACL；
/// - 仅任务私有 `scratch`（同步写入 `TMPDIR`）与容器私有 `TEMP`/`TMP`/`HOME` 可写。
#[cfg(windows)]
pub fn spawn_stdio_windows(spec: &StdioRunSpec<'_>) -> Result<WindowsStdioChild, BoxError> {
    if *spec.network != Network::Off {
        return Err(SandboxUnavailable::new(
            spec.operation,
            "Windows 原生号舍 W3 首批仅支持全断网（haoshe.network: false），暂不支持开放联网或按域名放行",
        )
        .into());
    }
    if !spec.extra_write.is_empty() {
        return Err(SandboxUnavailable::new(
            spec.operation,
            "Windows 原生号舍 W3 首批不支持把宿主项目目录或家目录准入可写（haoshe.write），仅允许任务私有临时目录",
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
        let scratch_long = normalize_windows_long_path(&scratch_dir)?;
        let program = resolve_windows_program(spec.operation, spec.program, &workspace_dir)?;
        let cwd = normalize_windows_long_path(&workspace_dir)?;

        let mut env: Vec<(String, String)> = spec
            .env
            .iter()
            .filter(|(k, _)| {
                let upper = k.to_ascii_uppercase();
                !WINDOWS_RESERVED_ENV.contains(&upper.as_str()) && upper != "TMPDIR"
            })
            .cloned()
            .collect();
        env.push(("TMPDIR".into(), scratch_long.to_string_lossy().into_owned()));

        let sandbox = ziyuan_sandbox_windows::spawn_stdio_sandboxed(
            &ziyuan_sandbox_windows::StdioSpec {
                program,
                args: spec.args.to_vec(),
                cwd: Some(cwd.clone()),
                writable_dirs: vec![scratch_long],
                readonly_dirs: vec![cwd],
                env,
                mode: ziyuan_sandbox_windows::SandboxMode::AppContainerChildCapable,
            },
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
        })
    })();

    if launch.is_err() {
        let _ = fs::remove_dir_all(&base_dir);
    }
    launch
}

fn read_lossy_from(file: &mut fs::File, start: u64) -> io::Result<String> {
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// 把 `program args…` 关进号舍；本机没有号舍时返回原因，由调用者决定照常运行还是拒绝。
pub fn confine(program: &str, args: &[String], rules: &Rules) -> Result<Confined, String> {
    let sandbox = available().clone()?;
    let proxy = match &rules.network {
        Network::Only(domains) => {
            #[cfg(target_os = "linux")]
            if find_in_path("socat").is_none() {
                return Err("按域名放行须装 socat（经它把门吏接入号舍），可用包管理器安装".into());
            }
            Some(Proxy::start(domains.clone()).map_err(|e| format!("号舍门吏无法当值：{e}"))?)
        }
        _ => None,
    };
    let mut hidden: Vec<PathBuf> = rules.hidden.clone();
    if let Some(home) = std::env::var_os("HOME") {
        hidden.extend(SECRET_DIRS.iter().map(|d| Path::new(&home).join(d)));
    }
    let hidden: Vec<PathBuf> = hidden
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .filter(|p| p.is_dir())
        .collect();
    let writable: Vec<PathBuf> = rules
        .writable
        .iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();
    let net = rules.network.describe();
    let mut child = Command::new(sandbox);
    #[cfg(not(target_os = "linux"))]
    {
        let door = proxy.as_ref().map(|p| match p.door {
            proxy::Door::Port(port) => port,
        });
        child
            .arg("-p")
            .arg(seatbelt_profile(&writable, &hidden, &rules.network, door));
        if let Some(port) = door {
            // 调用者常会清空环境，代理设置须随命令带进去。
            child.arg("/usr/bin/env");
            for key in PROXY_VARS {
                child.arg(format!("{key}=http://127.0.0.1:{port}"));
            }
        }
        child.arg(program).args(args);
        Ok(Confined {
            command: child,
            note: format!("号舍（Seatbelt）：{net}，只可写指定目录"),
            proxy,
        })
    }
    #[cfg(target_os = "linux")]
    {
        child.args([
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--tmpfs",
            "/tmp",
            "--unshare-pid",
            "--die-with-parent",
            "--new-session",
        ]);
        if rules.network != Network::Open {
            child.arg("--unshare-net");
        }
        let mut readable: Vec<PathBuf> = rules.readable.clone();
        if let Some(proxy::Door::Socket { dir, .. }) = proxy.as_ref().map(|p| &p.door) {
            readable.push(dir.clone());
            for key in PROXY_VARS {
                child
                    .arg("--setenv")
                    .arg(key)
                    .arg(format!("http://127.0.0.1:{DOOR_PORT}"));
            }
        }
        for dir in readable.iter().filter_map(|p| p.canonicalize().ok()) {
            child.arg("--ro-bind").arg(&dir).arg(&dir);
        }
        for dir in &hidden {
            child.arg("--tmpfs").arg(dir);
        }
        for dir in &writable {
            child.arg("--bind").arg(dir).arg(dir);
        }
        if let Some(proxy::Door::Socket { path, .. }) = proxy.as_ref().map(|p| &p.door) {
            // 号舍内只有回环网卡：socat 在其上开门，转到门吏的 Unix 套接字；等门开了再运行命令。
            let port = format!("{DOOR_PORT:04X}");
            let bridge = format!(
                "socat TCP-LISTEN:{DOOR_PORT},bind=127.0.0.1,fork,reuseaddr UNIX-CONNECT:\"$1\" 2>/dev/null &\n\
             i=0; while [ $i -lt 250 ] && ! grep -q ' 0100007F:{port} 00000000:0000 0A' /proc/net/tcp; \
             do sleep 0.02; i=$((i+1)); done\nshift\nexec \"$@\""
            );
            child.args(["/bin/sh", "-c", &bridge, "sh"]).arg(path);
        }
        child.arg(program).args(args);
        Ok(Confined {
            command: child,
            note: format!("号舍（bubblewrap）：{net}，只可写指定目录"),
            proxy,
        })
    }
}

/// 号舍内的程序经这些变量找到门吏（curl、cargo、pip、npm 等都认）。
const PROXY_VARS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "http_proxy",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
];

fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    })
}

/// Seatbelt 的规则：默认放行，再禁写（可写目录与 /dev 除外）、禁读凭据目录，按需禁网络。
#[cfg(not(target_os = "linux"))]
fn seatbelt_profile(
    writable: &[PathBuf],
    hidden: &[PathBuf],
    network: &Network,
    door: Option<u16>,
) -> String {
    let quote = |p: &Path| format!("\"{}\"", p.display().to_string().replace('"', "\\\""));
    let mut profile = String::from(
        "(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write* (subpath \"/dev\"))\n",
    );
    if *network != Network::Open {
        profile.push_str("(deny network*)\n");
    }
    if let Some(port) = door {
        profile.push_str(&format!(
            "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
        ));
    }
    for dir in writable {
        profile.push_str(&format!("(allow file-write* (subpath {}))\n", quote(dir)));
    }
    for dir in hidden {
        profile.push_str(&format!("(deny file-read* (subpath {}))\n", quote(dir)));
    }
    profile
}

/// 不入号舍：Windows 为 `cmd /C`，其余为 `sh -c`。
fn plain_shell(command: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut child = Command::new("cmd");
        // 原样交给 cmd：按 MSVC 规则转义引号会改变 cmd 看到的命令。
        child.arg("/C").raw_arg(command);
        child
    }
    #[cfg(not(windows))]
    {
        let mut child = Command::new("sh");
        child.arg("-c").arg(command);
        child
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confined_commands_write_only_the_copy_and_cannot_reach_out() {
        let base = std::env::temp_dir().join(format!("ziyuan-haoshe-{}", std::process::id()));
        let (work, outside, hidden) =
            (base.join("work"), base.join("outside"), base.join("hidden"));
        for dir in [&work, &outside, &hidden] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(hidden.join("secret"), "机密").unwrap();
        let script = format!(
            "touch {w}/ok; touch {o}/escaped 2>/dev/null; cat {h}/secret 2>/dev/null; echo done",
            w = work.display(),
            o = outside.display(),
            h = hidden.display()
        );
        let confined = shell(
            &script,
            Mode::Auto,
            Network::Off,
            &work,
            &[],
            std::slice::from_ref(&hidden),
        );
        if available().is_err() {
            // 本机没有号舍（如 Windows、未装 bubblewrap）：auto 与 require 都拒绝，不照常运行。
            // CI 的 Linux 与 macOS 装有号舍，下面的隔离才真测。
            for result in [
                confined,
                shell("echo x", Mode::Require, Network::Off, &work, &[], &[]),
            ] {
                let error = result.err().expect("号舍不可用时须拒绝");
                let unavailable = error
                    .downcast_ref::<SandboxUnavailable>()
                    .expect("稳定的错误类型");
                assert_eq!(unavailable.platform, std::env::consts::OS);
                assert_eq!(unavailable.operation, "run_tests");
                assert!(error.to_string().contains("ZIYUAN_SANDBOX=off"), "{error}");
            }
            std::fs::remove_dir_all(base).unwrap();
            return;
        }
        let Confined {
            command: mut child,
            note,
            ..
        } = confined.unwrap();
        let out = child.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("done"),
            "{note}: {stdout} {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(work.join("ok").exists(), "可写副本应能写");
        assert!(!outside.join("escaped").exists(), "副本以外不可写");
        assert!(!stdout.contains("机密"), "隐去的目录不可读");

        // 不可联网：连本机端口也连不上。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // bash 的 /dev/tcp：Linux 与 macOS 都有 bash，不依赖 nc。
        let probe = format!(
            "bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}' 2>/dev/null && echo REACHED; echo probed"
        );
        // 对照：不入号舍时连得上，可见探测本身有效。
        let mut child = shell(&probe, Mode::Off, Network::Off, &work, &[], &[])
            .unwrap()
            .command;
        let out = String::from_utf8_lossy(&child.output().unwrap().stdout).into_owned();
        assert!(out.contains("REACHED"), "{out}");
        let mut child = shell(&probe, Mode::Auto, Network::Off, &work, &[], &[])
            .unwrap()
            .command;
        let out = String::from_utf8_lossy(&child.output().unwrap().stdout).into_owned();
        assert!(out.contains("probed") && !out.contains("REACHED"), "{out}");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn off_runs_plainly_and_says_so() {
        let dir = std::env::temp_dir();
        let Confined {
            command: mut child,
            note,
            ..
        } = shell("echo plain", Mode::Off, Network::Off, &dir, &[], &[]).unwrap();
        assert!(note.contains("未入号舍"), "{note}");
        assert!(String::from_utf8_lossy(&child.output().unwrap().stdout).contains("plain"));
    }

    #[test]
    fn allowed_domains_pass_through_the_door_and_nothing_else_does() {
        if available().is_err() || (cfg!(target_os = "linux") && find_in_path("socat").is_none()) {
            eprintln!("跳过：本机没有号舍或 socat");
            return;
        }
        // 号舍外的一个小网站，只答一句话。
        let site = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = site.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for mut conn in site.incoming().flatten() {
                let mut buf = [0u8; 1024];
                let _ = conn.read(&mut buf);
                let _ = conn.write_all(b"HTTP/1.0 200 OK\r\n\r\nWELCOME");
            }
        });
        // 只用 bash 的 /dev/tcp 讲代理协议，不依赖 curl 对本机地址的代理例外。
        let script = format!(
            "p=${{HTTP_PROXY##*:}}; ask() {{ exec 3<>/dev/tcp/127.0.0.1/$p; \
             printf \"GET http://$1/ HTTP/1.0\\r\\n\\r\\n\" >&3; cat <&3; exec 3<&-; }}; \
             ask 127.0.0.1:{port}; echo; ask example.com; echo; \
             (exec 4<>/dev/tcp/127.0.0.1/{port}) 2>/dev/null && echo DIRECT; echo end"
        );
        let rules = Rules {
            network: Network::only(vec!["127.0.0.1".into()]).unwrap(),
            ..Rules::default()
        };
        let confined = confine("bash", &["-c".into(), script], &rules).unwrap();
        assert!(
            confined.note.contains("只放行 127.0.0.1"),
            "{}",
            confined.note
        );
        let mut child = confined.command;
        let out = child
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("WELCOME"),
            "名单上的地址经门吏可达：{stdout}"
        );
        assert!(
            stdout.contains("403") && stdout.contains("不放行 example.com"),
            "{stdout}"
        );
        assert!(
            stdout.contains("end") && !stdout.contains("DIRECT"),
            "绕过门吏直连不可达：{stdout}"
        );
        assert_eq!(confined.proxy.unwrap().denied(), ["example.com"]);
        assert!(Network::only(vec!["https://x.com".into()]).is_err());
    }

    #[test]
    fn run_shell_off_captures_output_and_reports_verdict() {
        let base = std::env::temp_dir().join(format!("ziyuan-run-shell-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let env: Vec<(String, OsString)> = ["PATH", "SystemRoot", "COMSPEC"]
            .iter()
            .filter_map(|k| std::env::var_os(k).map(|v| ((*k).to_owned(), v)))
            .collect();
        let out = run_shell(&ShellRunSpec {
            command: "echo run-shell-ok",
            mode: Mode::Off,
            network: Network::Off,
            work_dir: &base,
            sandbox_base: &base,
            extra_writable: &[],
            readonly_toolchains: &[],
            hidden: &[],
            env: &env,
            timeout: Duration::from_secs(10),
            max_output: 4096,
            max_failure_scan: 4096,
            cancel: None,
        })
        .unwrap();
        assert!(out.passed, "{out:?}");
        assert!(out.tail.contains("run-shell-ok"), "{out:?}");
        assert!(out.haoshe_note.contains("未入号舍"), "{out:?}");
        assert!(out.failure_scan.is_none());
        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(windows)]
    #[test]
    fn run_shell_windows_appcontainer_isolates_copy_blocks_host_and_honors_policies() {
        let root = std::env::temp_dir().join(format!("ziyuan-win-w2-{}", std::process::id()));
        let base = root.join("sandbox");
        let work = base.join("project");
        let host = root.join("host-secret");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&work).unwrap();
        fs::create_dir_all(&host).unwrap();
        let secret_file = host.join("secret.txt");
        fs::write(&secret_file, "HOST_TOP_SECRET_131").unwrap();

        // 1. Network::Only rejected explicitly under ADR-0001 W2 policy.
        let net_err = run_shell(&ShellRunSpec {
            command: "echo x",
            mode: Mode::Auto,
            network: Network::only(vec!["example.com".into()]).unwrap(),
            work_dir: &work,
            sandbox_base: &base,
            extra_writable: &[],
            readonly_toolchains: &[],
            hidden: &[],
            env: &[],
            timeout: Duration::from_secs(5),
            max_output: 1024,
            max_failure_scan: 1024,
            cancel: None,
        })
        .unwrap_err();
        let net_unavail = net_err
            .downcast_ref::<SandboxUnavailable>()
            .expect("must return SandboxUnavailable");
        assert!(
            net_unavail.reason.contains("全断网"),
            "{}",
            net_unavail.reason
        );

        // 2. External CARGO_TARGET_DIR / extra_writable rejected explicitly under ADR-0001 W2 policy.
        let extra = vec![root.join("external-target")];
        let dir_err = run_shell(&ShellRunSpec {
            command: "echo x",
            mode: Mode::Require,
            network: Network::Off,
            work_dir: &work,
            sandbox_base: &base,
            extra_writable: &extra,
            readonly_toolchains: &[],
            hidden: &[],
            env: &[],
            timeout: Duration::from_secs(5),
            max_output: 1024,
            max_failure_scan: 1024,
            cancel: None,
        })
        .unwrap_err();
        let dir_unavail = dir_err
            .downcast_ref::<SandboxUnavailable>()
            .expect("must return SandboxUnavailable");
        assert!(
            dir_unavail.reason.contains("CARGO_TARGET_DIR"),
            "{}",
            dir_unavail.reason
        );

        // 3. Default Mode::Auto + Network::Off runs inside native Windows AppContainer + Job,
        //    filters reserved host env vars, allows copy writes, and blocks host secrets.
        let host_env: Vec<(String, OsString)> =
            ["PATH", "SystemRoot", "USERPROFILE", "TEMP", "APPDATA"]
                .iter()
                .filter_map(|k| std::env::var_os(k).map(|v| ((*k).to_owned(), v)))
                .collect();
        let cmd = format!(
            "echo INSIDE_OK>inside.txt && type \"{}\" & echo W2_DONE",
            secret_file.display()
        );
        let out = run_shell(&ShellRunSpec {
            command: &cmd,
            mode: Mode::Auto,
            network: Network::Off,
            work_dir: &work,
            sandbox_base: &base,
            extra_writable: &[],
            readonly_toolchains: &[],
            hidden: std::slice::from_ref(&host),
            env: &host_env,
            timeout: Duration::from_secs(15),
            max_output: 4096,
            max_failure_scan: 4096,
            cancel: None,
        })
        .unwrap();
        assert!(out.passed, "{out:?}");
        assert_eq!(out.verdict, "通过（退出码 0）");
        assert!(
            out.haoshe_note
                .contains("号舍（Windows AppContainer＋Job）"),
            "{out:?}"
        );
        assert!(out.tail.contains("W2_DONE"), "{out:?}");
        assert!(
            !out.tail.contains("HOST_TOP_SECRET_131"),
            "host secret must stay blocked: {out:?}"
        );
        assert!(
            fs::read_to_string(work.join("inside.txt"))
                .unwrap()
                .contains("INSIDE_OK")
        );

        // 4. Cancellation flag terminates the AppContainer Job promptly.
        let cancel = AtomicBool::new(true);
        let cancelled = run_shell(&ShellRunSpec {
            command: "ping -n 6 127.0.0.1 > NUL",
            mode: Mode::Require,
            network: Network::Off,
            work_dir: &work,
            sandbox_base: &base,
            extra_writable: &[],
            readonly_toolchains: &[],
            hidden: &[],
            env: &host_env,
            timeout: Duration::from_secs(10),
            max_output: 1024,
            max_failure_scan: 1024,
            cancel: Some(&cancel),
        })
        .unwrap();
        assert!(!cancelled.passed, "{cancelled:?}");
        assert!(cancelled.verdict.contains("已取消"), "{cancelled:?}");

        // 5. Bounded tail vs larger failure_scan window on failing command.
        let mut big = String::from("EARLY_PANIC_MARKER\n");
        big.push_str(&"0123456789abcdef\n".repeat(40));
        big.push_str("FINAL_LINE\n");
        fs::write(work.join("big.txt"), big).unwrap();
        let failed = run_shell(&ShellRunSpec {
            command: "type big.txt & exit /b 2",
            mode: Mode::Auto,
            network: Network::Off,
            work_dir: &work,
            sandbox_base: &base,
            extra_writable: &[],
            readonly_toolchains: &[],
            hidden: &[],
            env: &host_env,
            timeout: Duration::from_secs(15),
            max_output: 64,
            max_failure_scan: 4096,
            cancel: None,
        })
        .unwrap();
        assert!(!failed.passed, "{failed:?}");
        assert_eq!(failed.verdict, "未通过（退出码 2）");
        assert!(failed.tail_truncated, "{failed:?}");
        assert!(failed.tail.contains("FINAL_LINE"), "{failed:?}");
        assert!(!failed.tail.contains("EARLY_PANIC_MARKER"), "{failed:?}");
        let scan = failed.failure_scan.as_deref().unwrap_or("");
        assert!(scan.contains("EARLY_PANIC_MARKER"), "{scan}");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn run_shell_off_honors_cancellation_flag() {
        let base =
            std::env::temp_dir().join(format!("ziyuan-run-shell-cancel-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let cancel = AtomicBool::new(true);
        let cmd = if cfg!(windows) {
            "ping -n 6 127.0.0.1 > NUL"
        } else {
            "sleep 5"
        };
        let out = run_shell(&ShellRunSpec {
            command: cmd,
            mode: Mode::Off,
            network: Network::Off,
            work_dir: &base,
            sandbox_base: &base,
            extra_writable: &[],
            readonly_toolchains: &[],
            hidden: &[],
            env: &[],
            timeout: Duration::from_secs(10),
            max_output: 1024,
            max_failure_scan: 1024,
            cancel: Some(&cancel),
        })
        .unwrap();
        assert!(!out.passed, "{out:?}");
        assert!(out.verdict.contains("已取消"), "{out:?}");
        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(windows)]
    #[test]
    fn run_shell_windows_admits_readonly_toolchain_snapshot_and_rejects_sensitive_dirs() {
        let root = std::env::temp_dir().join(format!("ziyuan-win-tc-{}", std::process::id()));
        let base = root.join("sandbox");
        let work = base.join("project");
        let tc = root.join("portable-tc");
        let tc_bin = tc.join("bin");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&work).unwrap();
        fs::create_dir_all(&tc_bin).unwrap();
        fs::write(
            tc_bin.join("mytool.cmd"),
            "@echo off\r\necho PORTABLE_TOOL_OK_135\r\n(echo TAMPER>>\"%~dp0ro.txt\") 2>NUL\r\ntype \"%~dp0ro.txt\"\r\n",
        )
        .unwrap();
        fs::write(tc_bin.join("ro.txt"), "ORIGINAL_RO").unwrap();

        let host_env: Vec<(String, OsString)> = ["PATH", "SystemRoot"]
            .iter()
            .filter_map(|k| std::env::var_os(k).map(|v| ((*k).to_owned(), v)))
            .collect();

        // 1. Without explicit admission, mytool is not found / inaccessible.
        let un_admitted = run_shell(&ShellRunSpec {
            command: "mytool",
            mode: Mode::Require,
            network: Network::Off,
            work_dir: &work,
            sandbox_base: &base,
            extra_writable: &[],
            readonly_toolchains: &[],
            hidden: &[],
            env: &host_env,
            timeout: Duration::from_secs(15),
            max_output: 4096,
            max_failure_scan: 4096,
            cancel: None,
        })
        .unwrap();
        assert!(
            !un_admitted.passed,
            "unadmitted tool must fail: {un_admitted:?}"
        );

        // 2. With explicit admission, mytool runs cleanly from private read-only snapshot,
        //    cannot overwrite files in the snapshot, and leaves host dir untouched.
        let admitted = run_shell(&ShellRunSpec {
            command: "mytool",
            mode: Mode::Require,
            network: Network::Off,
            work_dir: &work,
            sandbox_base: &base,
            extra_writable: &[],
            readonly_toolchains: std::slice::from_ref(&tc),
            hidden: &[],
            env: &host_env,
            timeout: Duration::from_secs(15),
            max_output: 4096,
            max_failure_scan: 4096,
            cancel: None,
        })
        .unwrap();
        assert!(admitted.passed, "{admitted:?}");
        assert!(
            admitted.tail.contains("PORTABLE_TOOL_OK_135"),
            "{admitted:?}"
        );
        assert!(admitted.tail.contains("ORIGINAL_RO"), "{admitted:?}");
        assert!(
            !admitted.tail.contains("TAMPER"),
            "read-only snapshot must reject write: {admitted:?}"
        );
        assert!(
            admitted.haoshe_note.contains("准入只读工具链 1 处"),
            "{admitted:?}"
        );
        assert_eq!(
            fs::read_to_string(tc_bin.join("ro.txt")).unwrap(),
            "ORIGINAL_RO"
        );

        // 3. Fail-closed rejections: relative path, missing path, sensitive roots, secret components.
        let mut bad_paths = vec![
            PathBuf::from("relative/tc"),
            root.join("does-not-exist"),
            PathBuf::from(r"C:\"),
        ];
        if let Some(up) = std::env::var_os("USERPROFILE") {
            bad_paths.push(PathBuf::from(up));
        }
        let ssh_dir = root.join(".ssh");
        fs::create_dir_all(&ssh_dir).unwrap();
        bad_paths.push(ssh_dir);

        let nested_env_tc = root.join("bad-tc-env");
        fs::create_dir_all(&nested_env_tc).unwrap();
        fs::write(nested_env_tc.join(".env.local"), "SECRET=1").unwrap();
        bad_paths.push(nested_env_tc);

        for bad in &bad_paths {
            let err = run_shell(&ShellRunSpec {
                command: "echo hi",
                mode: Mode::Require,
                network: Network::Off,
                work_dir: &work,
                sandbox_base: &base,
                extra_writable: &[],
                readonly_toolchains: std::slice::from_ref(bad),
                hidden: &[],
                env: &host_env,
                timeout: Duration::from_secs(5),
                max_output: 1024,
                max_failure_scan: 1024,
                cancel: None,
            })
            .unwrap_err();
            assert!(
                err.downcast_ref::<SandboxUnavailable>().is_some(),
                "expected SandboxUnavailable for {}: {err}",
                bad.display()
            );
        }

        let _ = fs::remove_dir_all(&root);
    }
}
