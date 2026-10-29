//! 号舍：把 `run_tests` 关进操作系统的沙箱。
//!
//! 取贡院号舍之意：考生锁入单间，与外界隔绝。测试命令可能运行模型写入的代码，入号舍后
//! 不能联网，只能写测试副本所在的临时目录，读不到家目录下常见的凭据（`.ssh`、`.aws` 等）
//! 与项目的 `.ziyuan`。Linux 用 bubblewrap（`bwrap`），macOS 用系统自带的 Seatbelt
//! （`sandbox-exec`）；Windows 暂无，如实注明。
//!
//! 由 `ZIYUAN_SANDBOX` 决定：`auto`（默认，有则用，无则照常运行并注明）、`require`（无则拒绝
//! 运行）、`off`（不用）。`ZIYUAN_SANDBOX_ALLOW` 列出可访问的域名时，号舍仍断网，只经门吏
//! （[`proxy`]）放行这些域名的 HTTP(S)。
mod proxy;

pub use proxy::Proxy;

use crate::BoxError;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

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
    Auto,
    Require,
    Off,
}

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
/// `extra_writable`，如 `CARGO_TARGET_DIR`；`hidden` 读不到）。
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
    match confine("/bin/sh", &args, &rules) {
        Ok(confined) => Ok(confined),
        Err(reason) if mode == Mode::Require => {
            Err(format!("号舍不可用，ZIYUAN_SANDBOX=require 不许照常运行：{reason}").into())
        }
        Err(reason) => Ok(plain(format!("未入号舍：{reason}"))),
    }
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
        let Confined {
            command: mut child,
            note,
            ..
        } = shell(
            &script,
            Mode::Auto,
            Network::Off,
            &work,
            &[],
            std::slice::from_ref(&hidden),
        )
        .unwrap();
        if note.starts_with("未入号舍") {
            // 本机没有号舍（如 Windows、未装 bubblewrap）：只确认如实注明。CI 的 Linux 与 macOS 装有号舍。
            eprintln!("跳过：{note}");
            std::fs::remove_dir_all(base).unwrap();
            return;
        }
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
}
