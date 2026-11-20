//! 号舍：把 `run_tests` 关进操作系统的沙箱。
//!
//! 取贡院号舍之意：考生锁入单间，与外界隔绝。测试命令可能运行模型写入的代码，入号舍后
//! 不能联网，只能写测试副本所在的临时目录，读不到家目录下常见的凭据（`.ssh`、`.aws` 等）
//! 与项目的 `.ziyuan`。Linux 用 bubblewrap（`bwrap`），macOS 用系统自带的 Seatbelt
//! （`sandbox-exec`）；Windows 暂无（见 `docs/adr/0001-windows-sandbox.md`）。
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
            return Err("本系统暂无号舍（仅支持 Linux 与 macOS）".into());
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
    /// 号舍内不可读的目录（如项目 `.ziyuan`）。
    pub hidden: &'a [PathBuf],
    /// 允许继承的环境变量白名单。
    pub env: &'a [(String, OsString)],
    pub timeout: Duration,
    pub max_output: u64,
    pub max_failure_scan: u64,
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
/// 统一负责：平台前置策略核验、沙箱构造、门吏代理生命周期、受限环境注入、有界超时终止与
/// 末尾输出采集；`ziyuan-sandbox-windows` 完成独立审计与 crates.io 首发后，只需在此处接入
/// Windows 受保护分支，上层 `run_tests` 无需再直接操作裸 `Command`/`Child`。
pub fn run_shell(spec: &ShellRunSpec<'_>) -> Result<ShellRunOutput, BoxError> {
    #[cfg(windows)]
    if spec.mode != Mode::Off {
        if spec.network != Network::Off {
            return Err(SandboxUnavailable::new(
                "run_tests",
                "Windows 原生号舍首批仅支持全断网（Network::Off），暂不支持 ZIYUAN_SANDBOX_ALLOW 按域名放行或开放联网",
            )
            .into());
        }
        if !spec.extra_writable.is_empty() {
            return Err(SandboxUnavailable::new(
                "run_tests",
                "Windows 原生号舍首批不继承外部 CARGO_TARGET_DIR 等额外可写目录，请取消该变量后重试",
            )
            .into());
        }
        return Err(SandboxUnavailable::new(
            "run_tests",
            "Windows 原生号舍原型（ziyuan-sandbox-windows）已完成 W1/W2 接口与 AppContainer 隔离实测，待独立 unsafe 审计与 crates.io 首发后接入生产",
        )
        .into());
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
    let mut child = child.spawn()?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if started.elapsed() > spec.timeout {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if let Some(denied) = proxy.map(|p| p.denied()).filter(|d| !d.is_empty()) {
        haoshe_note.push_str(&format!("；拦下 {}", denied.join("、")));
    }
    let mut log = fs::File::open(&log_path)?;
    let len = log.metadata()?.len();
    let tail = read_lossy_from(&mut log, len.saturating_sub(spec.max_output))?;
    let passed = status.is_some_and(|s| s.success());
    let verdict = match status {
        None => format!("超时（{} 秒）被终止", spec.timeout.as_secs()),
        Some(s) if s.success() => "通过（退出码 0）".to_owned(),
        Some(s) => match s.code() {
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
            hidden: &[],
            env: &env,
            timeout: Duration::from_secs(10),
            max_output: 4096,
            max_failure_scan: 4096,
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
    fn run_shell_enforces_windows_w2_fail_closed_policy_checks() {
        let base = std::env::temp_dir().join(format!("ziyuan-win-w2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();

        // 1. Network::Only rejected explicitly under ADR-0001 W2 policy.
        let net_err = run_shell(&ShellRunSpec {
            command: "echo x",
            mode: Mode::Auto,
            network: Network::only(vec!["example.com".into()]).unwrap(),
            work_dir: &base,
            sandbox_base: &base,
            extra_writable: &[],
            hidden: &[],
            env: &[],
            timeout: Duration::from_secs(5),
            max_output: 1024,
            max_failure_scan: 1024,
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
        let extra = vec![base.join("external-target")];
        let dir_err = run_shell(&ShellRunSpec {
            command: "echo x",
            mode: Mode::Require,
            network: Network::Off,
            work_dir: &base,
            sandbox_base: &base,
            extra_writable: &extra,
            hidden: &[],
            env: &[],
            timeout: Duration::from_secs(5),
            max_output: 1024,
            max_failure_scan: 1024,
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

        // 3. Default Network::Off still fails closed until unsafe audit + crates.io publish.
        let default_err = run_shell(&ShellRunSpec {
            command: "echo x",
            mode: Mode::Auto,
            network: Network::Off,
            work_dir: &base,
            sandbox_base: &base,
            extra_writable: &[],
            hidden: &[],
            env: &[],
            timeout: Duration::from_secs(5),
            max_output: 1024,
            max_failure_scan: 1024,
        })
        .unwrap_err();
        let def_unavail = default_err
            .downcast_ref::<SandboxUnavailable>()
            .expect("must return SandboxUnavailable");
        assert_eq!(def_unavail.platform, "windows");
        assert_eq!(def_unavail.operation, "run_tests");
        assert!(
            def_unavail.reason.contains("ziyuan-sandbox-windows"),
            "{}",
            def_unavail.reason
        );

        let _ = fs::remove_dir_all(&base);
    }
}
