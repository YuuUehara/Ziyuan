//! Experimental native Windows LPAC + Job Object sandbox boundary (ADR-0001).
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
    /// LPAC (no network capabilities, AAP opt-out) + Job Object verified on this host.
    FileSystemAndNetworkIsolated,
}

/// Safe description of a command to run inside an isolated per-task Windows LPAC + Job.
#[derive(Clone, Debug)]
pub struct LaunchSpec {
    /// Absolute path to the executable to launch (must not be a reparse point).
    pub program: PathBuf,
    /// Command-line arguments forwarded to `program`.
    pub args: Vec<String>,
    /// Absolute working directory inside the task-private or admitted writable tree.
    /// If `None`, defaults to the freshly created task AppContainer profile directory.
    pub cwd: Option<PathBuf>,
    /// Task-private directories admitted read/write/execute for this LPAC SID only.
    /// Callers must pass only freshly created task-private directories, never host source trees.
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

/// Completed execution result from [`run_sandboxed`].
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

/// Runs `spec` inside an isolated per-task LPAC + Job Object with zero network capabilities,
/// strict handle allowlisting, suspended Job assignment, and full descendant tree termination.
pub fn run_sandboxed(spec: &LaunchSpec, cancel: Option<&AtomicBool>) -> io::Result<RunResult> {
    #[cfg(windows)]
    {
        windows::run_sandboxed(spec, cancel)
    }
    #[cfg(not(windows))]
    {
        let _ = (spec, cancel);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows LPAC sandbox is only available on Windows",
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
