//! Experimental native Windows AppContainer / LPAC + Job Object sandbox boundary (ADR-0001).
//!
//! Exposes a safe Rust API that owns all Win32 handles, AppContainer profile SIDs,
//! attribute lists, and Job Object lifecycles internally. All `unsafe` FFI stays
//! confined inside the private `windows` module.
//!
//! Production `ziyuan-runtime` remains fail-closed on Windows until independent
//! security review and crates.io trusted-publisher onboarding complete.

use std::{io, path::PathBuf, sync::atomic::AtomicBool, time::Duration};

#[cfg(windows)]
mod files;
#[cfg(windows)]
mod network;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

/// Reported Windows sandbox capability level (ADR-0001).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SandboxCapability {
    /// No verified native Windows isolation is available.
    Unavailable { reason: String },
    /// Process/job restriction only (not full filesystem/network isolation).
    RestrictedProcess,
    /// AppContainer / LPAC (no network capabilities) + Job Object verified on this host.
    FileSystemAndNetworkIsolated,
}

/// Isolation profile for [`run_sandboxed_with_mode`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SandboxMode {
    /// Strict Less Privileged AppContainer (`AAP_ACCESS=0`, `TokenCapabilities=0`) + Job.
    /// Blocks child `CreateProcessW` on Windows 11 (`ERROR_ACCESS_DENIED`); suited for
    /// single-process tools and strict probes.
    #[default]
    StrictLpac,
    /// Zero-capability AppContainer (`AAP_ACCESS=1`, `TokenCapabilities=0`) + Job.
    /// Preserves per-task SID filesystem isolation and zero network capabilities while
    /// permitting child processes (`cmd.exe`, compilers, test runners) inside the Job tree.
    AppContainerChildCapable,
}

/// Safe description of a command to run inside an isolated per-task Windows sandbox + Job.
#[derive(Clone, Debug)]
pub struct LaunchSpec {
    /// Absolute path to the executable to launch (must not be a reparse point).
    pub program: PathBuf,
    /// Command-line arguments forwarded to `program`.
    pub args: Vec<String>,
    /// Absolute working directory inside the task-private or admitted writable tree.
    /// If `None`, defaults to the freshly created task AppContainer profile directory.
    pub cwd: Option<PathBuf>,
    /// Task-private directories admitted read/write/execute for this container SID only.
    /// Callers must pass only freshly created task-private directories, never host source trees.
    pub writable_dirs: Vec<PathBuf>,
    /// Task-private dependency directories admitted read/execute only (protected DACL + low IL).
    pub readonly_dirs: Vec<PathBuf>,
    /// Extra environment variables (filtered; cannot override `SystemRoot` or private dirs).
    pub env: Vec<(String, String)>,
    /// Maximum wall-clock runtime before the entire Job tree is terminated.
    pub timeout: Duration,
}

/// Safe description of a `cmd.exe /D /S /C` command to run inside [`SandboxMode::AppContainerChildCapable`].
#[derive(Clone, Debug)]
pub struct ShellSpec {
    /// Raw shell command executed by `%SystemRoot%\System32\cmd.exe /D /S /C "<command>"`.
    /// Must be non-empty and contain no NUL, CR, or LF bytes.
    pub command: String,
    /// Working directory inside an admitted task-private directory (must not be a reparse point).
    pub cwd: PathBuf,
    /// Task-private directories admitted read/write/execute for this container SID only.
    pub writable_dirs: Vec<PathBuf>,
    /// Task-private dependency directories admitted read/execute only (protected DACL + low IL).
    pub readonly_dirs: Vec<PathBuf>,
    /// Extra environment variables (filtered; cannot override `SystemRoot` or private dirs).
    pub env: Vec<(String, String)>,
    /// Maximum wall-clock runtime before the entire Job tree is terminated.
    pub timeout: Duration,
}

/// How the sandboxed process tree ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitOutcome {
    /// Primary process exited on its own with this Win32 exit code.
    Exited(u32),
    /// Wall-clock timeout elapsed and the Job tree was terminated.
    TimedOut,
    /// Caller set the cancellation flag and the Job tree was terminated.
    Cancelled,
}

/// Completed execution result from [`run_sandboxed`], [`run_sandboxed_with_mode`], or [`run_shell_sandboxed`].
#[derive(Clone, Debug)]
pub struct RunResult {
    pub outcome: ExitOutcome,
    /// Combined stdout/stderr captured via the task-private output file.
    pub output: Vec<u8>,
    /// Number of active processes remaining in the Job after tree termination (expected 0).
    pub active_processes_after: u32,
    /// Non-fatal cleanup diagnostics (e.g. if profile deletion failed).
    pub cleanup_warnings: Vec<String>,
}

/// Probes whether LPAC + Job Object isolation can be initialized on the current host.
pub fn probe_capability() -> SandboxCapability {
    #[cfg(windows)]
    {
        windows::probe_capability()
    }
    #[cfg(not(windows))]
    {
        SandboxCapability::Unavailable {
            reason: "Windows LPAC sandbox is only available on Windows".into(),
        }
    }
}

/// Runs `spec` inside an isolated per-task strict LPAC + Job Object with zero network capabilities,
/// strict handle allowlisting, suspended Job assignment, and full descendant tree termination.
pub fn run_sandboxed(spec: &LaunchSpec, cancel: Option<&AtomicBool>) -> io::Result<RunResult> {
    run_sandboxed_with_mode(spec, SandboxMode::StrictLpac, cancel)
}

/// Runs `spec` inside an isolated per-task Windows sandbox + Job Object using `mode`.
pub fn run_sandboxed_with_mode(
    spec: &LaunchSpec,
    mode: SandboxMode,
    cancel: Option<&AtomicBool>,
) -> io::Result<RunResult> {
    #[cfg(windows)]
    {
        windows::run_sandboxed_with_mode(spec, mode, cancel)
    }
    #[cfg(not(windows))]
    {
        let _ = (spec, mode, cancel);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows LPAC sandbox is only available on Windows",
        ))
    }
}

/// Runs `spec.command` via `%SystemRoot%\System32\cmd.exe /D /S /C` inside a zero-capability
/// [`SandboxMode::AppContainerChildCapable`] container + `KILL_ON_JOB_CLOSE` Job Object.
///
/// AutoRun registry hooks are disabled (`/D`), no network capabilities are granted, host
/// directories are never admitted, and the entire descendant process tree is terminated and
/// drained before cleanup.
pub fn run_shell_sandboxed(spec: &ShellSpec, cancel: Option<&AtomicBool>) -> io::Result<RunResult> {
    #[cfg(windows)]
    {
        windows::run_shell_sandboxed(spec, cancel)
    }
    #[cfg(not(windows))]
    {
        let _ = (spec, cancel);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows AppContainer shell sandbox is only available on Windows",
        ))
    }
}

/// Entrypoint for the standalone W1a/W1b/W1c/W1d verification probe binary.
#[doc(hidden)]
pub fn run_probe_cli() -> io::Result<()> {
    #[cfg(windows)]
    {
        windows::run()
    }
    #[cfg(not(windows))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "W1 probe requires native Windows; no isolation was performed",
        ))
    }
}
