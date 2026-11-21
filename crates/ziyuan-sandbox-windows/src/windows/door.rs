//! Zero-network-capability per-task SID-restricted Named Pipe door and intra-SID loopback bridge.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader},
    mem::{size_of, zeroed},
    os::windows::io::{AsRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

use windows_sys::Win32::{
    Foundation::{
        ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_OPERATION_ABORTED, ERROR_PIPE_CONNECTED,
        ERROR_PIPE_NOT_CONNECTED, GetLastError, HANDLE, HANDLE_FLAG_INHERIT, LocalFree,
        SetHandleInformation, WAIT_OBJECT_0,
    },
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES, TOKEN_USER, TokenIsAppContainer,
        TokenUser,
    },
    Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, ReadFile,
        WriteFile,
    },
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        JobObjects::AssignProcessToJobObject,
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
        },
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateEventW,
            CreateProcessW, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
            PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            PROCESS_INFORMATION, ResetEvent, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW,
            SetEvent, WaitForMultipleObjects,
        },
        WindowsProgramming::PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT,
    },
};

use crate::PipeDoorStream;

use super::{
    Attributes, Child, PrivateDesktop, Profile, aap_access, build_environment, checked,
    create_anonymous_pipe, in_job, normalize_win32_long_path, own, quote_windows_arg,
    reject_reparse_point, sid_text, token_capability_count, token_flag, token_information, wide,
};

/// Maximum concurrent client connections accepted by a single per-task Named Pipe door.
const MAX_DOOR_CONNS: usize = 64;
const DOOR_PIPE_BUFFER: u32 = 64 * 1024;

static DOOR_SEQ: AtomicU64 = AtomicU64::new(1);

struct PipeDoorHandle {
    handle: OwnedHandle,
    write_closed: AtomicBool,
}

pub(crate) struct PipeDoorStreamInner {
    inner: Arc<PipeDoorHandle>,
}

impl PipeDoorStreamInner {
    fn new(handle: OwnedHandle) -> Self {
        Self {
            inner: Arc::new(PipeDoorHandle {
                handle,
                write_closed: AtomicBool::new(false),
            }),
        }
    }

    pub(crate) fn try_clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }

    pub(crate) fn close_write(&self) {
        self.inner.write_closed.store(true, Ordering::Release);
        // SAFETY: `self.inner.handle` is a live kernel pipe handle owned by the Arc;
        // canceling pending overlapped I/O wakes any blocked concurrent reader cleanly.
        unsafe {
            let _ = CancelIoEx(self.inner.handle.as_raw_handle(), null());
        }
    }

    pub(crate) fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        overlapped_read(self.inner.handle.as_raw_handle(), buf)
    }

    pub(crate) fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.inner.write_closed.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "pipe door write side closed",
            ));
        }
        if buf.is_empty() {
            return Ok(0);
        }
        overlapped_write(self.inner.handle.as_raw_handle(), buf)
    }
}

fn create_manual_event() -> io::Result<OwnedHandle> {
    // SAFETY: creates an unnamed, non-inheritable, manual-reset, initially non-signaled event.
    unsafe { own(CreateEventW(null(), 1, 0, null())) }
}

fn overlapped_read(handle: HANDLE, buf: &mut [u8]) -> io::Result<usize> {
    let event = create_manual_event()?;
    // SAFETY: POD Win32 struct permits zero initialization.
    let mut ov: OVERLAPPED = unsafe { zeroed() };
    ov.hEvent = event.as_raw_handle();
    let to_read = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    let mut transferred = 0u32;
    // SAFETY: `handle` is an overlapped pipe handle; `buf` and stack `ov` remain pinned and valid
    // until `GetOverlappedResult(..., TRUE)` confirms the kernel has finished accessing both.
    unsafe {
        let ok = ReadFile(handle, buf.as_mut_ptr(), to_read, &mut transferred, &mut ov);
        if ok != 0 {
            return Ok(transferred as usize);
        }
        let err = GetLastError();
        if err == ERROR_IO_PENDING {
            let done = GetOverlappedResult(handle, &ov, &mut transferred, 1);
            if done != 0 {
                return Ok(transferred as usize);
            }
            let ov_err = GetLastError();
            if matches!(
                ov_err,
                ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_OPERATION_ABORTED
            ) {
                return Ok(0);
            }
            return Err(io::Error::from_raw_os_error(ov_err as i32));
        }
        if matches!(
            err,
            ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_OPERATION_ABORTED
        ) {
            return Ok(0);
        }
        Err(io::Error::from_raw_os_error(err as i32))
    }
}

fn overlapped_write(handle: HANDLE, buf: &[u8]) -> io::Result<usize> {
    let event = create_manual_event()?;
    // SAFETY: POD Win32 struct permits zero initialization.
    let mut ov: OVERLAPPED = unsafe { zeroed() };
    ov.hEvent = event.as_raw_handle();
    let to_write = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    let mut transferred = 0u32;
    // SAFETY: `handle` is an overlapped pipe handle; `buf` and stack `ov` remain live until
    // `GetOverlappedResult(..., TRUE)` completes.
    unsafe {
        let ok = WriteFile(handle, buf.as_ptr(), to_write, &mut transferred, &mut ov);
        if ok != 0 {
            return Ok(transferred as usize);
        }
        let err = GetLastError();
        if err == ERROR_IO_PENDING {
            let done = GetOverlappedResult(handle, &ov, &mut transferred, 1);
            if done != 0 {
                return Ok(transferred as usize);
            }
            return Err(io::Error::from_raw_os_error(GetLastError() as i32));
        }
        Err(io::Error::from_raw_os_error(err as i32))
    }
}

fn create_door_pipe_instance(
    pipe_name_wide: &[u16],
    sddl_wide: &[u16],
    first_instance: bool,
) -> io::Result<OwnedHandle> {
    let mut descriptor = null_mut();
    // SAFETY: `sddl_wide` is NUL-terminated; `descriptor` is freed with `LocalFree` after
    // `CreateNamedPipeW` completes; handle is non-inheritable (`bInheritHandle = 0`).
    unsafe {
        checked(ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_wide.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        ))?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
        if first_instance {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        let pipe_mode =
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
        let raw = CreateNamedPipeW(
            pipe_name_wide.as_ptr(),
            open_mode,
            pipe_mode,
            PIPE_UNLIMITED_INSTANCES,
            DOOR_PIPE_BUFFER,
            DOOR_PIPE_BUFFER,
            0,
            &sa,
        );
        let result = own(raw);
        LocalFree(descriptor);
        result
    }
}

struct ActiveDoorConnGuard(Arc<AtomicUsize>);

impl Drop for ActiveDoorConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Host-side per-task Named Pipe acceptor bound exclusively to one AppContainer SID.
pub(super) struct NamedPipeDoor {
    pub(super) pipe_name: String,
    stop_event: Arc<OwnedHandle>,
    acceptor: Option<JoinHandle<()>>,
}

impl NamedPipeDoor {
    pub(super) fn start(
        app_sid: PSID,
        on_connect: Arc<dyn Fn(PipeDoorStream) + Send + Sync + 'static>,
    ) -> io::Result<Self> {
        let storage = token_information(TokenUser)?;
        // SAFETY: exact API class, aligned live storage including the pointed-to SID.
        let user = unsafe { sid_text((*storage.as_ptr().cast::<TOKEN_USER>()).User.Sid)? };
        let app = sid_text(app_sid)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let seq = DOOR_SEQ.fetch_add(1, Ordering::Relaxed);
        let pipe_name = format!(r"\\.\pipe\ziyuan-door-{}-{seq}-{stamp}", std::process::id());
        let pipe_name_wide = wide(&pipe_name);
        // Protected DACL: SYSTEM and host user get GA (required to create additional server pipe
        // instances); ONLY this task's AppContainer SID gets GRGW (client open/read/write, no
        // FILE_CREATE_PIPE_INSTANCE, no WRITE_DAC); Low IL SACL permits AppContainer connect.
        let sddl_wide = wide(format!(
            "D:P(A;;GA;;;SY)(A;;GA;;;{user})(A;;GRGW;;;{app})S:(ML;;NW;;;LW)"
        ));

        // Create the first server instance synchronously BEFORE resuming the child so the pipe
        // already exists when child code runs (eliminates startup races and name squatting).
        let first_instance = create_door_pipe_instance(&pipe_name_wide, &sddl_wide, true)?;
        let stop_event = Arc::new(create_manual_event()?);
        let stop_for_thread = Arc::clone(&stop_event);
        let active = Arc::new(AtomicUsize::new(0));

        let acceptor = thread::spawn(move || {
            let Ok(connect_event) = create_manual_event() else {
                return;
            };
            let mut instance = first_instance;
            loop {
                // SAFETY: manual-reset event reset before each ConnectNamedPipe call.
                unsafe {
                    let _ = ResetEvent(connect_event.as_raw_handle());
                }
                // SAFETY: POD Win32 struct permits zero initialization.
                let mut ov: OVERLAPPED = unsafe { zeroed() };
                ov.hEvent = connect_event.as_raw_handle();
                // SAFETY: `instance` is an overlapped server pipe handle; `ov` remains valid
                // until `GetOverlappedResult(..., TRUE)` completes on either connect or cancel.
                let connected = unsafe {
                    let ok = ConnectNamedPipe(instance.as_raw_handle(), &mut ov);
                    if ok != 0 {
                        true
                    } else {
                        match GetLastError() {
                            ERROR_PIPE_CONNECTED => true,
                            ERROR_IO_PENDING => {
                                let handles = [
                                    stop_for_thread.as_raw_handle(),
                                    connect_event.as_raw_handle(),
                                ];
                                let wait = WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE);
                                if wait == WAIT_OBJECT_0 + 1 {
                                    let mut dummy = 0u32;
                                    GetOverlappedResult(
                                        instance.as_raw_handle(),
                                        &ov,
                                        &mut dummy,
                                        1,
                                    ) != 0
                                } else {
                                    let _ = CancelIoEx(instance.as_raw_handle(), &ov);
                                    let mut dummy = 0u32;
                                    let _ = GetOverlappedResult(
                                        instance.as_raw_handle(),
                                        &ov,
                                        &mut dummy,
                                        1,
                                    );
                                    break;
                                }
                            }
                            _ => break,
                        }
                    }
                };
                if !connected {
                    break;
                }
                // Replace the accepted instance with a fresh listening instance before dispatching
                // the client stream so rapid sequential requests never hit ERROR_PIPE_BUSY.
                let next_instance = create_door_pipe_instance(&pipe_name_wide, &sddl_wide, false);
                let prev = active.fetch_add(1, Ordering::AcqRel);
                if prev < MAX_DOOR_CONNS {
                    let guard = ActiveDoorConnGuard(Arc::clone(&active));
                    let stream = PipeDoorStream {
                        inner: PipeDoorStreamInner::new(instance),
                    };
                    let handler = Arc::clone(&on_connect);
                    thread::spawn(move || {
                        let _guard = guard;
                        handler(stream);
                    });
                } else {
                    active.fetch_sub(1, Ordering::AcqRel);
                    drop(instance);
                }
                match next_instance {
                    Ok(next) => instance = next,
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            pipe_name,
            stop_event,
            acceptor: Some(acceptor),
        })
    }

    pub(super) fn stop(&mut self) {
        // SAFETY: `stop_event` is an owned live Win32 Event handle.
        unsafe {
            let _ = SetEvent(self.stop_event.as_raw_handle());
        }
        if let Some(handle) = self.acceptor.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for NamedPipeDoor {
    fn drop(&mut self) {
        self.stop();
    }
}

pub(super) const PROXY_ENV_VARS: [&str; 3] = ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"];

/// Pure .NET BCL in-container TCP→NamedPipe bridge (zero PowerShell cmdlets, zero `Add-Type`).
/// Runs inside the SAME zero-capability AppContainer SID & Job Object as the main child so
/// standard HTTP proxy clients (`curl`, `cargo`, `node`, `python`, `ureq`) can reach the host
/// domain-allowlist proxy via `HTTP_PROXY=http://127.0.0.1:<port>` with zero loopback exemptions.
const DOOR_BRIDGE_SCRIPT: &str = r#"$ErrorActionPreference = 'Stop'
$rawPipe = $env:ZIYUAN_BRIDGE_PIPE
$shortPipe = $rawPipe.Substring(9)
$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
$listener.Start()
$port = ([System.Net.IPEndPoint]$listener.LocalEndpoint).Port
[Console]::Out.WriteLine("READY:$port")
[Console]::Out.Flush()
$clock = [System.Diagnostics.Stopwatch]::StartNew()
$active = [System.Collections.Generic.List[object]]::new()
while ($true) {
  while ($listener.Pending()) {
    $tcp = $listener.AcceptTcpClient()
    try {
      $tcp.NoDelay = $true
      $pipe = [System.IO.Pipes.NamedPipeClientStream]::new(
        '.',
        $shortPipe,
        [System.IO.Pipes.PipeDirection]::InOut,
        [System.IO.Pipes.PipeOptions]::Asynchronous
      )
      $pipe.Connect(5000)
      $net = $tcp.GetStream()
      $t1 = $net.CopyToAsync($pipe)
      $t2 = $pipe.CopyToAsync($net)
      $active.Add(@{
        Tcp = $tcp
        Pipe = $pipe
        T1 = $t1
        T2 = $t2
        SentFin = $false
        FinMs = [int64]0
      })
    } catch {
      try { $tcp.Close() } catch {}
    }
  }
  $now = $clock.ElapsedMilliseconds
  for ($i = $active.Count - 1; $i -ge 0; $i--) {
    $c = $active[$i]
    $t1Done = $c.T1.IsCompleted
    $t2Done = $c.T2.IsCompleted
    if ($t2Done -and -not $c.SentFin) {
      try { $c.Tcp.Client.Shutdown([System.Net.Sockets.SocketShutdown]::Send) } catch {}
      $c.SentFin = $true
      $c.FinMs = $now
    }
    $done = ($t1Done -and $t2Done) -or ($c.SentFin -and (($now - $c.FinMs) -ge 500))
    if ($done) {
      if ($c.T1.IsFaulted) { $null = $c.T1.Exception }
      if ($c.T2.IsFaulted) { $null = $c.T2.Exception }
      try { $c.Pipe.Dispose() } catch {}
      try { $c.Tcp.Close() } catch {}
      $active.RemoveAt($i)
    }
  }
  [System.Threading.Thread]::Sleep(5)
}
"#;

pub(super) struct ProfileDirs<'a> {
    pub(super) profile_dir: &'a Path,
    pub(super) temp_dir: &'a Path,
    pub(super) home_dir: &'a Path,
    pub(super) appdata_dir: &'a Path,
}

/// Launches the in-container TCP→NamedPipe bridge inside the exact same AppContainer SID,
/// Job Object, and optional PrivateDesktop as the main child, returning `(Child, port, script)`.
pub(super) fn spawn_door_bridge(
    profile: &Profile,
    dirs: &ProfileDirs<'_>,
    job: &OwnedHandle,
    desktop: Option<&mut PrivateDesktop>,
    ordinary_control: bool,
    pipe_name: &str,
) -> io::Result<(Child, u16, PathBuf)> {
    let ProfileDirs {
        profile_dir,
        temp_dir,
        home_dir,
        appdata_dir,
    } = *dirs;
    let system_root = std::env::var_os("SystemRoot")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            io::Error::other("SystemRoot unavailable; refusing to resolve powershell.exe")
        })?;
    let ps_exe = PathBuf::from(system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    reject_reparse_point(&ps_exe)?;
    let ps_exe = normalize_win32_long_path(&ps_exe)?;

    let script_path = profile_dir.join("door-bridge.ps1");
    fs::write(&script_path, DOOR_BRIDGE_SCRIPT)?;

    let (parent_stdout_read, child_stdout_write) = create_anonymous_pipe()?;
    let stdin_nul = File::open("NUL")?;
    let stderr_nul = OpenOptions::new().write(true).open("NUL")?;

    // SAFETY: mark only the child stdio handles inheritable for the bridge process.
    unsafe {
        for handle in [
            stdin_nul.as_raw_handle(),
            child_stdout_write.as_raw_handle(),
            stderr_nul.as_raw_handle(),
        ] {
            checked(SetHandleInformation(
                handle,
                HANDLE_FLAG_INHERIT,
                HANDLE_FLAG_INHERIT,
            ))?;
        }
    }

    let capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: profile.sid,
        Capabilities: null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    let policy = PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT;
    let handles = [
        stdin_nul.as_raw_handle(),
        child_stdout_write.as_raw_handle(),
        stderr_nul.as_raw_handle(),
    ];
    let mut attributes = Attributes::new()?;
    attributes.set(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, &capabilities)?;
    if !ordinary_control {
        attributes.set(
            PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
            &policy,
        )?;
    }
    attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)?;

    let application = wide(&ps_exe);
    let directory = wide(profile_dir);
    let cmdline = format!(
        "{} -NoProfile -NonInteractive -ExecutionPolicy Bypass -File {}",
        quote_windows_arg(&ps_exe.to_string_lossy())?,
        quote_windows_arg(&script_path.to_string_lossy())?,
    );
    let mut command = wide(cmdline);
    let bridge_env = vec![("ZIYUAN_BRIDGE_PIPE".to_owned(), pipe_name.to_owned())];
    let environment = build_environment(profile_dir, temp_dir, home_dir, appdata_dir, &bridge_env)?;

    // SAFETY: extended startup cb, valid aligned attributes, mutable command buffer,
    // allowlisted Unicode environment, and optional private desktop live through CreateProcessW.
    let child = unsafe {
        let mut startup: STARTUPINFOEXW = zeroed();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        if let Some(desk) = desktop {
            startup.StartupInfo.lpDesktop = desk.lp_desktop.as_mut_ptr();
        }
        startup.StartupInfo.hStdInput = stdin_nul.as_raw_handle();
        startup.StartupInfo.hStdOutput = child_stdout_write.as_raw_handle();
        startup.StartupInfo.hStdError = stderr_nul.as_raw_handle();
        startup.lpAttributeList = attributes.ptr();
        let mut info: PROCESS_INFORMATION = zeroed();
        checked(CreateProcessW(
            application.as_ptr(),
            command.as_mut_ptr(),
            null(),
            null(),
            1,
            CREATE_SUSPENDED
                | EXTENDED_STARTUPINFO_PRESENT
                | CREATE_UNICODE_ENVIRONMENT
                | CREATE_NO_WINDOW,
            environment.as_ptr().cast(),
            directory.as_ptr(),
            &startup.StartupInfo,
            &mut info,
        ))?;
        Child {
            process: own(info.hProcess)?,
            thread: own(info.hThread)?,
        }
    };

    drop(attributes);
    drop(stdin_nul);
    drop(child_stdout_write);
    drop(stderr_nul);

    // SAFETY: process is suspended; enforce exact same Job + zero-cap AppContainer invariants.
    unsafe {
        checked(AssignProcessToJobObject(
            job.as_raw_handle(),
            child.process.as_raw_handle(),
        ))?;
    }
    if !in_job(child.process.as_raw_handle(), job.as_raw_handle())?
        || token_flag(child.process.as_raw_handle(), TokenIsAppContainer)? != 1
        || aap_access(child.process.as_raw_handle())? != ordinary_control
        || token_capability_count(child.process.as_raw_handle())? != 0
    {
        return Err(io::Error::other(
            "suspended door bridge AppContainer / Job / capability verification failed",
        ));
    }
    // SAFETY: suspended bridge verified; resume it to bind 127.0.0.1:0.
    unsafe {
        if ResumeThread(child.thread.as_raw_handle()) == u32::MAX {
            return Err(io::Error::last_os_error());
        }
    }

    let mut reader = BufReader::new(File::from(parent_stdout_read));
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Err(io::Error::other(
            "in-container door bridge exited before reporting READY:<port>",
        ));
    }
    let trimmed = line.trim();
    let port: u16 = trimmed
        .strip_prefix("READY:")
        .and_then(|p| p.parse().ok())
        .filter(|&p| p > 0)
        .ok_or_else(|| {
            io::Error::other(format!(
                "in-container door bridge returned unexpected handshake: {trimmed:?}"
            ))
        })?;
    Ok((child, port, script_path))
}
