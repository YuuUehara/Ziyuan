//! 号舍：把 `run_tests` 关进操作系统的沙箱。
//!
//! 取贡院号舍之意：考生锁入单间，与外界隔绝。测试命令可能运行模型写入的代码，入号舍后
//! 不能联网，只能写测试副本所在的临时目录，读不到家目录下常见的凭据（`.ssh`、`.aws` 等）
//! 与项目的 `.ziyuan`。Linux 用 bubblewrap（`bwrap`），macOS 用系统自带的 Seatbelt
//! （`sandbox-exec`）；Windows 暂无，如实注明。
//!
//! 由 `ZIYUAN_SANDBOX` 决定：`auto`（默认，有则用，无则照常运行并注明）、`require`（无则拒绝
//! 运行）、`off`（不用）。
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
        let Some(bwrap) = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("bwrap"))
                .find(|p| p.is_file())
        }) else {
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

/// 用系统的 shell 运行 `command`，按 `mode` 关进号舍：只可写 `writable`（及 `extra_writable`，
/// 如 `CARGO_TARGET_DIR`），不可联网；`hidden` 中的目录读不到。返回命令与给审计看的说明。
pub fn shell(
    command: &str,
    mode: Mode,
    writable: &Path,
    extra_writable: &[PathBuf],
    hidden: &[PathBuf],
) -> Result<(Command, String), BoxError> {
    if mode == Mode::Off {
        return Ok((
            plain_shell(command),
            "未入号舍（ZIYUAN_SANDBOX=off）".into(),
        ));
    }
    let program = match available() {
        Ok(program) => program,
        Err(reason) if mode == Mode::Require => {
            return Err(
                format!("号舍不可用，ZIYUAN_SANDBOX=require 不许照常运行：{reason}").into(),
            );
        }
        Err(reason) => return Ok((plain_shell(command), format!("未入号舍：{reason}"))),
    };
    let mut hidden: Vec<PathBuf> = hidden.to_vec();
    if let Some(home) = std::env::var_os("HOME") {
        hidden.extend(SECRET_DIRS.iter().map(|d| Path::new(&home).join(d)));
    }
    let hidden: Vec<PathBuf> = hidden
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .filter(|p| p.is_dir())
        .collect();
    let mut writable_dirs = vec![writable.canonicalize()?];
    writable_dirs.extend(extra_writable.iter().filter_map(|p| p.canonicalize().ok()));
    let mut child = Command::new(program);
    if cfg!(target_os = "macos") {
        child
            .arg("-p")
            .arg(seatbelt_profile(&writable_dirs, &hidden));
        child.args(["/bin/sh", "-c", command]);
        return Ok((child, "号舍（Seatbelt）：不可联网，只可写测试副本".into()));
    }
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
        "--unshare-net",
        "--unshare-pid",
        "--die-with-parent",
        "--new-session",
    ]);
    for dir in &hidden {
        child.arg("--tmpfs").arg(dir);
    }
    for dir in &writable_dirs {
        child.arg("--bind").arg(dir).arg(dir);
    }
    child.args(["/bin/sh", "-c", command]);
    Ok((child, "号舍（bubblewrap）：不可联网，只可写测试副本".into()))
}

/// Seatbelt 的规则：默认放行，再禁网络、禁写（可写目录与 /dev 除外）、禁读凭据目录。
fn seatbelt_profile(writable: &[PathBuf], hidden: &[PathBuf]) -> String {
    let quote = |p: &Path| format!("\"{}\"", p.display().to_string().replace('"', "\\\""));
    let mut profile = String::from(
        "(version 1)\n(allow default)\n(deny network*)\n(deny file-write*)\n\
         (allow file-write* (subpath \"/dev\"))\n",
    );
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
        let (mut child, note) = shell(
            &script,
            Mode::Auto,
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
        let (mut child, _) = shell(&probe, Mode::Off, &work, &[], &[]).unwrap();
        let out = String::from_utf8_lossy(&child.output().unwrap().stdout).into_owned();
        assert!(out.contains("REACHED"), "{out}");
        let (mut child, _) = shell(&probe, Mode::Auto, &work, &[], &[]).unwrap();
        let out = String::from_utf8_lossy(&child.output().unwrap().stdout).into_owned();
        assert!(out.contains("probed") && !out.contains("REACHED"), "{out}");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn off_runs_plainly_and_says_so() {
        let dir = std::env::temp_dir();
        let (mut child, note) = shell("echo plain", Mode::Off, &dir, &[], &[]).unwrap();
        assert!(note.contains("未入号舍"), "{note}");
        assert!(String::from_utf8_lossy(&child.output().unwrap().stdout).contains("plain"));
    }
}
