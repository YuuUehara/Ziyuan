//! Native Windows AppContainer / LPAC + Job Object sandbox boundary (ADR-0001).
//!
//! Exposes a safe Rust API that owns all Win32 handles, AppContainer profile SIDs,
//! attribute lists, alternate hidden desktops, and Job Object lifecycles internally.
//! All production `unsafe` FFI stays confined inside the private `windows` module,
//! separated from the standalone verification probe CLI in `probe`.
//!
//! Production `ziyuan-runtime` uses [`run_shell_sandboxed`] / [`run_shell_sandboxed_with_door`]
//! on Windows for one-shot `run_tests` execution (W2) and [`spawn_stdio_sandboxed`] /
//! [`spawn_stdio_sandboxed_with_door`] for long-lived stdio MCP servers (W3), including
//! task-private nested writable subdirectories and zero-network-capability per-task SID-restricted
//! Win32 Named Pipe door channels. Open-network and host-ACL-mutating policies remain fail-closed.

use std::{
    fmt,
    fs::File,
    io::{self, Read, Write},
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[cfg(windows)]
mod files;
#[cfg(windows)]
mod network;
#[cfg(windows)]
#[allow(unsafe_code)]
mod probe;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

/// Default maximum tail bytes captured from a sandboxed process output file (16 MiB).
pub const DEFAULT_MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;

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

/// Isolation profile for [`run_sandboxed_with_mode`] and [`spawn_stdio_sandboxed`].
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
    /// Maximum tail bytes read into memory from the captured output file.
    /// `None` uses [`DEFAULT_MAX_OUTPUT_BYTES`].
    pub max_output_bytes: Option<u64>,
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
    /// Maximum tail bytes read into memory from the captured output file.
    /// `None` uses [`DEFAULT_MAX_OUTPUT_BYTES`].
    pub max_output_bytes: Option<u64>,
}

/// Safe description of a long-lived bidirectional stdio process to run inside an isolated
/// per-task zero-capability Windows AppContainer + `KILL_ON_JOB_CLOSE` Job Object (W3).
#[derive(Clone, Debug)]
pub struct StdioSpec {
    /// Absolute path to the executable to launch (must not be a reparse point).
    pub program: PathBuf,
    /// Command-line arguments forwarded to `program`.
    pub args: Vec<String>,
    /// Absolute working directory inside an admitted task-private directory.
    /// If `None`, defaults to the freshly created task AppContainer profile directory.
    pub cwd: Option<PathBuf>,
    /// Task-private directories admitted read/write/execute for this container SID only.
    /// Callers must pass only freshly created task-private directories, never host source trees.
    pub writable_dirs: Vec<PathBuf>,
    /// Task-private directories admitted read/execute only (protected DACL + low IL).
    pub readonly_dirs: Vec<PathBuf>,
    /// Extra environment variables (filtered; cannot override `SystemRoot` or private dirs).
    pub env: Vec<(String, String)>,
    /// Isolation mode (use [`SandboxMode::AppContainerChildCapable`] for runtimes/interpreters
    /// that spawn helper processes, or [`SandboxMode::StrictLpac`] for single-process servers).
    pub mode: SandboxMode,
}

/// Connected full-duplex stream on a per-task AppContainer-SID-restricted Win32 Named Pipe door.
///
/// Backed by `FILE_FLAG_OVERLAPPED` I/O inside `windows.rs` so concurrent `read` and `write`
/// across cloned handles never deadlock on Win32 synchronous pipe serialization.
pub struct PipeDoorStream {
    #[cfg(windows)]
    inner: windows::PipeDoorStreamInner,
    #[cfg(not(windows))]
    _unsupported: (),
}

impl PipeDoorStream {
    /// Clones this stream handle so one thread can read while another writes concurrently.
    pub fn try_clone(&self) -> io::Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self {
                inner: self.inner.try_clone(),
            })
        }
        #[cfg(not(windows))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Windows Named Pipe door is only available on Windows",
            ))
        }
    }

    /// Marks the write side closed and cancels any blocked pending read on this pipe handle so
    /// half-close / proxy teardown completes cleanly without hanging.
    pub fn close_write(&self) {
        #[cfg(windows)]
        {
            self.inner.close_write();
        }
    }
}

impl Read for PipeDoorStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(windows)]
        {
            self.inner.read(buf)
        }
        #[cfg(not(windows))]
        {
            let _ = buf;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Windows Named Pipe door is only available on Windows",
            ))
        }
    }
}

impl Write for PipeDoorStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(windows)]
        {
            self.inner.write(buf)
        }
        #[cfg(not(windows))]
        {
            let _ = buf;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Windows Named Pipe door is only available on Windows",
            ))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Optional per-task Win32 Named Pipe door exposed exclusively to this sandbox's AppContainer SID.
///
/// The container stays at zero network capabilities (`TokenCapabilities == 0`) with zero loopback
/// exemptions. The host creates a local-only (`PIPE_REJECT_REMOTE_CLIENTS`) Named Pipe whose
/// protected DACL grants `GENERIC_READ | GENERIC_WRITE` ONLY to this task's freshly generated
/// AppContainer SID (plus host user / SYSTEM) with a Low Integrity SACL, and injects the pipe path
/// into the child environment under `env_var`.
#[derive(Clone)]
pub struct DoorSpec {
    /// Environment variable name injected into the sandboxed child (e.g. `ZIYUAN_HAOSHE_DOOR_PIPE`).
    pub env_var: String,
    /// When `true`, also launches an in-container 0-capability TCP→NamedPipe loopback bridge
    /// inside the same AppContainer SID & Job Object and injects `HTTP_PROXY`/`HTTPS_PROXY`/
    /// `ALL_PROXY` (upper and lower case) pointing at the bridge's `127.0.0.1:<port>`.
    pub http_proxy_bridge: bool,
    /// Callback invoked on a bounded worker thread for each accepted AppContainer client connection.
    pub on_connect: Arc<dyn Fn(PipeDoorStream) + Send + Sync + 'static>,
}

impl fmt::Debug for DoorSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DoorSpec")
            .field("env_var", &self.env_var)
            .field("http_proxy_bridge", &self.http_proxy_bridge)
            .finish_non_exhaustive()
    }
}

/// Running sandboxed process with bidirectional anonymous stdio pipes (W3).
///
/// Owns the Job Object, process/thread handles, private desktop, and AppContainer profile.
/// Calling [`SandboxedStdio::shutdown`] or dropping this handle terminates and drains the
/// entire Job process tree and cleans up per-task profile storage.
pub struct SandboxedStdio {
    #[cfg(windows)]
    inner: windows::SandboxedStdioInner,
    #[cfg(not(windows))]
    _unsupported: (),
}

impl SandboxedStdio {
    /// Takes ownership of the parent write end of the child's stdin pipe.
    pub fn take_stdin(&mut self) -> Option<File> {
        #[cfg(windows)]
        {
            self.inner.take_stdin()
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    /// Takes ownership of the parent read end of the child's stdout pipe.
    pub fn take_stdout(&mut self) -> Option<File> {
        #[cfg(windows)]
        {
            self.inner.take_stdout()
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    /// Terminates and drains the entire Job process tree, releases kernel handles and private
    /// desktop, and deletes per-task AppContainer profile storage. Idempotent: subsequent calls
    /// return `Ok(0)`.
    ///
    /// Returns the number of active processes remaining in the Job after draining (expected `0`).
    pub fn shutdown(&mut self) -> io::Result<u32> {
        #[cfg(windows)]
        {
            self.inner.shutdown()
        }
        #[cfg(not(windows))]
        {
            Ok(0)
        }
    }

    /// Non-fatal cleanup diagnostics recorded during [`Self::shutdown`].
    pub fn cleanup_warnings(&self) -> &[String] {
        #[cfg(windows)]
        {
            self.inner.cleanup_warnings()
        }
        #[cfg(not(windows))]
        {
            &[]
        }
    }
}

impl Drop for SandboxedStdio {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
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
    /// Combined stdout/stderr tail captured via the task-private output file (bounded by `max_output_bytes`).
    pub output: Vec<u8>,
    /// Whether the captured output file exceeded `max_output_bytes` and was truncated to its tail.
    pub output_truncated: bool,
    /// Total bytes written to the task-private output file before tail truncation.
    pub total_output_bytes: u64,
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
/// directories are never admitted, `HOME`/`USERPROFILE`/`APPDATA`/`TEMP` point exclusively
/// to task-private profile storage, and the entire descendant process tree is terminated and
/// drained before cleanup.
pub fn run_shell_sandboxed(spec: &ShellSpec, cancel: Option<&AtomicBool>) -> io::Result<RunResult> {
    run_shell_sandboxed_with_door(spec, cancel, None)
}

/// Like [`run_shell_sandboxed`], with an optional per-task SID-restricted Named Pipe door channel.
pub fn run_shell_sandboxed_with_door(
    spec: &ShellSpec,
    cancel: Option<&AtomicBool>,
    door: Option<DoorSpec>,
) -> io::Result<RunResult> {
    #[cfg(windows)]
    {
        windows::run_shell_sandboxed_with_door(spec, cancel, door)
    }
    #[cfg(not(windows))]
    {
        let _ = (spec, cancel, door);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows AppContainer shell sandbox is only available on Windows",
        ))
    }
}

/// Spawns a long-lived bidirectional stdio process inside a zero-capability Windows AppContainer
/// + `KILL_ON_JOB_CLOSE` Job Object (W3).
///
/// Only the child ends of two anonymous pipes (`stdin` read, `stdout` write) and a `NUL` `stderr`
/// handle are inherited via `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`; parent pipe ends are non-inheritable
/// and verified absent from the child before resuming. No network capabilities are granted, host
/// directories are never admitted, and dropping the returned [`SandboxedStdio`] terminates and
/// drains the entire Job tree before cleaning up the AppContainer profile.
pub fn spawn_stdio_sandboxed(spec: &StdioSpec) -> io::Result<SandboxedStdio> {
    spawn_stdio_sandboxed_with_door(spec, None)
}

/// Like [`spawn_stdio_sandboxed`], with an optional per-task SID-restricted Named Pipe door channel.
pub fn spawn_stdio_sandboxed_with_door(
    spec: &StdioSpec,
    door: Option<DoorSpec>,
) -> io::Result<SandboxedStdio> {
    #[cfg(windows)]
    {
        windows::spawn_stdio_sandboxed_with_door(spec, door).map(|inner| SandboxedStdio { inner })
    }
    #[cfg(not(windows))]
    {
        let _ = (spec, door);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows AppContainer stdio sandbox is only available on Windows",
        ))
    }
}

/// Entrypoint for the standalone W1a/W1b/W1c/W1d/W2 verification probe binary.
#[doc(hidden)]
pub fn run_probe_cli() -> io::Result<()> {
    #[cfg(windows)]
    {
        probe::run()
    }
    #[cfg(not(windows))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "W1 probe requires native Windows; no isolation was performed",
        ))
    }
}
