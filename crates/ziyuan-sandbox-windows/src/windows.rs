//! Production Windows sandbox FFI boundary (AppContainer / LPAC + Job Object + PrivateDesktop).
//!
//! All production `unsafe` calls for `ziyuan-sandbox-windows` are confined inside this module tree,
//! exposing only safe Rust APIs to [`crate`] and `ziyuan-runtime`.

use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io,
    mem::{size_of, size_of_val, zeroed},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::PathBuf,
    ptr::{null, null_mut},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use windows_sys::Win32::{
    Foundation::*,
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        Isolation::*,
        *,
    },
    System::{
        Com::CoTaskMemFree,
        JobObjects::*,
        LibraryLoader::{GetProcAddress, LoadLibraryW},
        Threading::*,
        WindowsProgramming::PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT,
    },
};

use crate::{
    DEFAULT_MAX_OUTPUT_BYTES, DoorSpec, ExitOutcome, LaunchSpec, RunResult, SandboxCapability,
    SandboxMode, ShellSpec, StdioSpec,
};

mod acl;
mod door;
mod env;

pub(super) use acl::{admit_dependency, low_integrity};
use acl::{admit_directories, normalize_win32_long_path, reject_reparse_point};
pub(crate) use door::PipeDoorStreamInner;
use door::{NamedPipeDoor, PROXY_ENV_VARS, ProfileDirs, spawn_door_bridge};
use env::{build_environment, create_anonymous_pipe, quote_windows_arg, read_tail_bounded};

pub(super) static PROFILE_SEQ: AtomicU64 = AtomicU64::new(1);

pub(super) fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

#[track_caller]
pub(super) fn checked(value: i32) -> io::Result<()> {
    if value == 0 {
        let error = io::Error::last_os_error();
        Err(io::Error::new(
            error.kind(),
            format!("{}: {error}", std::panic::Location::caller()),
        ))
    } else {
        Ok(())
    }
}

fn hresult(value: i32) -> io::Result<()> {
    if value < 0 {
        Err(io::Error::other(format!("HRESULT {value:#x}")))
    } else {
        Ok(())
    }
}

// SAFETY at callers: transfer only newly created, uniquely owned kernel handles.
pub(super) unsafe fn own(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: caller transfers ownership; OwnedHandle closes exactly once.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
}

pub(super) struct Profile {
    name: Vec<u16>,
    pub(super) sid: PSID,
    pub(super) path: PathBuf,
    pub(super) registered: bool,
}

impl Profile {
    pub(super) fn create() -> io::Result<Self> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let seq = PROFILE_SEQ.fetch_add(1, Ordering::Relaxed);
        let name = wide(format!("Ziyuan.W1.{}.{seq}.{stamp}", std::process::id()));
        let mut sid = null_mut();
        // SAFETY: three terminated strings live through the call; no capabilities;
        // successful creation returns a unique SID allocated by the API.
        unsafe {
            hresult(CreateAppContainerProfile(
                name.as_ptr(),
                name.as_ptr(),
                name.as_ptr(),
                null(),
                0,
                &mut sid,
            ))?;
        }
        let mut profile = Self {
            name,
            sid,
            path: PathBuf::new(),
            registered: true,
        };
        let mut sid_string = null_mut();
        let mut folder = null_mut();
        // SAFETY: SID lives in profile. String allocations use the matching
        // LocalFree / CoTaskMemFree APIs, including failure of the folder query.
        unsafe {
            checked(ConvertSidToStringSidW(profile.sid, &mut sid_string))?;
            let result = GetAppContainerFolderPath(sid_string, &mut folder);
            LocalFree(sid_string.cast());
            hresult(result)?;
            let mut len = 0;
            while *folder.add(len) != 0 {
                len += 1;
            }
            profile.path =
                PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(folder, len)));
            CoTaskMemFree(folder.cast());
        }
        Ok(profile)
    }

    pub(super) fn cleanup(&mut self) -> io::Result<()> {
        if self.registered {
            // SAFETY: profile owns the terminated name and only deletes its own
            // successful registration; no existing profile is reused.
            unsafe {
                hresult(DeleteAppContainerProfile(self.name.as_ptr()))?;
            }
            if !self.path.as_os_str().is_empty() && self.path.try_exists()? {
                return Err(io::Error::other("profile storage remains after deletion"));
            }
            self.registered = false;
        }
        Ok(())
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!(
                "W1 profile cleanup failed for {}: {error}",
                String::from_utf16_lossy(&self.name[..self.name.len() - 1])
            );
        }
        // SAFETY: CreateAppContainerProfile allocated this SID; free it once.
        unsafe {
            FreeSid(self.sid);
        }
    }
}

// SAFETY: `Profile` uniquely owns the API-allocated SID (`FreeSid`) and profile registration
// (`DeleteAppContainerProfile`); neither has Win32 thread affinity.
unsafe impl Send for Profile {}

// Attribute-list storage requires native allocation alignment. This fixed-size
// element provides 16-byte alignment without relying on Vec<u8>'s allocator.
#[repr(align(16))]
#[derive(Clone, Copy)]
pub(super) struct Aligned {
    _bytes: [u8; 16],
}

pub(super) struct Attributes {
    storage: Vec<Aligned>,
}

impl Attributes {
    pub(super) fn new() -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: sizing call has no destination; second call receives aligned,
        // writable storage of at least bytes, retained until DeleteAttributeList.
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), 3, 0, &mut bytes);
            if bytes == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut storage = vec![Aligned { _bytes: [0; 16] }; bytes.div_ceil(16)];
            // Construct the owner only after initialization; on error the
            // allocation is freed without deleting an uninitialized list.
            checked(InitializeProcThreadAttributeList(
                storage.as_mut_ptr().cast(),
                3,
                0,
                &mut bytes,
            ))?;
            Ok(Self { storage })
        }
    }
    pub(super) fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
    pub(super) fn set<T>(&mut self, attribute: u32, value: &T) -> io::Result<()> {
        // SAFETY: caller retains the actual values (and any referenced SID /
        // handle storage) through CreateProcessW; the list copies no ownership.
        unsafe {
            checked(UpdateProcThreadAttribute(
                self.ptr(),
                0,
                attribute as usize,
                (value as *const T).cast(),
                size_of::<T>(),
                null_mut(),
                null(),
            ))
        }
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: storage is initialized, uniquely owned and still allocated.
        unsafe {
            DeleteProcThreadAttributeList(self.ptr());
        }
    }
}

/// Per-task isolated hidden Desktop (Alternate Desktop pattern) granting minimal headless
/// `user32.dll` initialization rights ONLY to this task's AppContainer SID (no switch-desktop,
/// no hooks, no access to the user's interactive desktop). Dynamically loads `user32.dll` so
/// this crate carries zero static `user32.dll` import (keeping strict LPAC binaries desktop-free).
struct PrivateDesktop {
    user32: HMODULE,
    close_desktop: unsafe extern "system" fn(*mut core::ffi::c_void) -> i32,
    handle: *mut core::ffi::c_void,
    lp_desktop: Vec<u16>,
}

impl PrivateDesktop {
    fn create(app_sid: PSID) -> io::Result<Self> {
        type CreateDesktopFn = unsafe extern "system" fn(
            *const u16,
            *const u16,
            *const core::ffi::c_void,
            u32,
            u32,
            *const SECURITY_ATTRIBUTES,
        ) -> *mut core::ffi::c_void;
        type CloseDesktopFn = unsafe extern "system" fn(*mut core::ffi::c_void) -> i32;
        type GetProcessWindowStationFn = unsafe extern "system" fn() -> *mut core::ffi::c_void;
        type GetUserObjectInfoFn = unsafe extern "system" fn(
            *mut core::ffi::c_void,
            i32,
            *mut core::ffi::c_void,
            u32,
            *mut u32,
        ) -> i32;

        let system_root = std::env::var_os("SystemRoot")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| io::Error::other("SystemRoot unavailable"))?;
        let user32_path = PathBuf::from(system_root)
            .join("System32")
            .join("user32.dll");
        let user32_wide = wide(&user32_path);
        // SAFETY: absolute System32 path; returned HMODULE is freed on error or in Drop.
        let user32 = unsafe { LoadLibraryW(user32_wide.as_ptr()) };
        if user32.is_null() {
            return Err(io::Error::last_os_error());
        }
        let build = (|| -> io::Result<Self> {
            // SAFETY: NUL-terminated exported symbol names from System32\user32.dll with exact Win32 ABIs.
            let (create_desktop, close_desktop, get_winsta, get_uoi) = unsafe {
                let p_create = GetProcAddress(user32, c"CreateDesktopW".as_ptr().cast())
                    .ok_or_else(io::Error::last_os_error)?;
                let p_close = GetProcAddress(user32, c"CloseDesktop".as_ptr().cast())
                    .ok_or_else(io::Error::last_os_error)?;
                let p_winsta = GetProcAddress(user32, c"GetProcessWindowStation".as_ptr().cast())
                    .ok_or_else(io::Error::last_os_error)?;
                let p_uoi = GetProcAddress(user32, c"GetUserObjectInformationW".as_ptr().cast())
                    .ok_or_else(io::Error::last_os_error)?;
                type RawProc = unsafe extern "system" fn() -> isize;
                (
                    std::mem::transmute::<RawProc, CreateDesktopFn>(p_create),
                    std::mem::transmute::<RawProc, CloseDesktopFn>(p_close),
                    std::mem::transmute::<RawProc, GetProcessWindowStationFn>(p_winsta),
                    std::mem::transmute::<RawProc, GetUserObjectInfoFn>(p_uoi),
                )
            };

            let storage = token_information(TokenUser)?;
            // SAFETY: exact API class, aligned live storage including the pointed-to SID.
            let user = unsafe { sid_text((*storage.as_ptr().cast::<TOKEN_USER>()).User.Sid)? };
            let app = sid_text(app_sid)?;
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos();
            let seq = PROFILE_SEQ.fetch_add(1, Ordering::Relaxed);
            let desk_name = format!("Ziyuan.Desk.{}.{seq}.{stamp}", std::process::id());
            let desk_wide = wide(&desk_name);
            // Grant SYSTEM + host user full control; grant this AppContainer SID only
            // READ_CONTROL | DESKTOP_READOBJECTS | DESKTOP_CREATEWINDOW | DESKTOP_ENUMERATE |
            // DESKTOP_WRITEOBJECTS (0x000200C3) -- excluding DESKTOP_SWITCHDESKTOP and hooks.
            // Explicit Low Mandatory Integrity Label (S:(ML;;NW;;;LW)) ensures Low-IL AppContainer
            // tokens can initialize user32.dll even when the host process runs elevated (High IL).
            let sddl = wide(format!(
                "D:P(A;;GA;;;SY)(A;;GA;;;{user})(A;;0x000200C3;;;{app})S:(ML;;NW;;;LW)"
            ));
            let mut descriptor = null_mut();
            // SAFETY: SDDL buffer and descriptor live through CreateDesktopW;
            // LocalFree matches ConvertStringSecurityDescriptorToSecurityDescriptorW on all paths.
            let handle = unsafe {
                checked(ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    null_mut(),
                ))?;
                let sa = SECURITY_ATTRIBUTES {
                    nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: descriptor,
                    bInheritHandle: 0,
                };
                let handle =
                    create_desktop(desk_wide.as_ptr(), null(), null(), 0, GENERIC_ALL, &sa);
                let err = io::Error::last_os_error();
                LocalFree(descriptor);
                if handle.is_null() {
                    return Err(err);
                }
                handle
            };

            // UOI_NAME == 2
            let winsta = unsafe {
                let ws = get_winsta();
                if ws.is_null() {
                    None
                } else {
                    let mut buf = [0u16; 256];
                    let mut needed = 0u32;
                    if get_uoi(
                        ws,
                        2,
                        buf.as_mut_ptr().cast(),
                        (buf.len() * size_of::<u16>()) as u32,
                        &mut needed,
                    ) == 0
                    {
                        None
                    } else {
                        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                        Some(String::from_utf16_lossy(&buf[..len]))
                    }
                }
            }
            .unwrap_or_else(|| "WinSta0".to_owned());
            let lp_desktop = wide(format!("{winsta}\\{desk_name}"));
            Ok(Self {
                user32,
                close_desktop,
                handle,
                lp_desktop,
            })
        })();
        if build.is_err() {
            // SAFETY: user32 was loaded above and is not transferred on error.
            unsafe {
                FreeLibrary(user32);
            }
        }
        build
    }
}

impl Drop for PrivateDesktop {
    fn drop(&mut self) {
        // SAFETY: uniquely owned HDESK closed once before releasing user32 HMODULE.
        unsafe {
            (self.close_desktop)(self.handle);
            FreeLibrary(self.user32);
        }
    }
}

// SAFETY: `PrivateDesktop` uniquely owns its `HDESK` (never assigned to the host thread via
// `SetThreadDesktop`) and `user32.dll` `HMODULE`; `CloseDesktop` and `FreeLibrary` have no
// thread affinity.
unsafe impl Send for PrivateDesktop {}

pub(super) struct Child {
    pub(super) process: OwnedHandle,
    pub(super) thread: OwnedHandle,
}

impl Child {
    pub(super) fn stop(&self) -> io::Result<()> {
        // SAFETY: these handles remain owned/live. Terminate is also attempted
        // for the pre-Job error path. Wait is bounded, including cleanup paths.
        unsafe {
            TerminateProcess(self.process.as_raw_handle(), 125);
            if WaitForSingleObject(self.process.as_raw_handle(), 10_000) != WAIT_OBJECT_0 {
                return Err(io::Error::other("child termination not confirmed"));
            }
        }
        Ok(())
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("W1: {error}");
        }
    }
}

pub(super) fn token_flag(process: HANDLE, class: TOKEN_INFORMATION_CLASS) -> io::Result<u32> {
    let mut token = null_mut();
    let mut flag = 0u32;
    let mut size = 0;
    // SAFETY: live process (or pseudo handle), unique token output; requested
    // classes below return DWORDs, buffer size is exact, never TOKEN_USER etc.
    unsafe {
        checked(OpenProcessToken(process, TOKEN_QUERY, &mut token))?;
        let token = own(token)?;
        checked(GetTokenInformation(
            token.as_raw_handle(),
            class,
            (&mut flag as *mut u32).cast(),
            size_of::<u32>() as u32,
            &mut size,
        ))?;
    }
    Ok(flag)
}

pub(super) fn token_capability_count(process: HANDLE) -> io::Result<u32> {
    let mut token = null_mut();
    let mut bytes = 0u32;
    // SAFETY: live process (or pseudo handle), unique owned token; sizing query
    // determines exact aligned buffer size for `TOKEN_GROUPS` (`TokenCapabilities`).
    unsafe {
        checked(OpenProcessToken(process, TOKEN_QUERY, &mut token))?;
        let token = own(token)?;
        let ok = GetTokenInformation(
            token.as_raw_handle(),
            TokenCapabilities,
            null_mut(),
            0,
            &mut bytes,
        );
        if ok != 0 {
            return Ok(0);
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
            return Err(err);
        }
        if (bytes as usize) < size_of::<u32>() {
            return Err(io::Error::other("TokenCapabilities buffer too small"));
        }
        let mut storage = vec![Aligned { _bytes: [0; 16] }; (bytes as usize).div_ceil(16)];
        checked(GetTokenInformation(
            token.as_raw_handle(),
            TokenCapabilities,
            storage.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        ))?;
        Ok((*storage.as_ptr().cast::<TOKEN_GROUPS>()).GroupCount)
    }
}

fn job_active_processes(job: HANDLE) -> io::Result<u32> {
    // SAFETY: POD Win32 struct permits zero initialization.
    let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
    // SAFETY: `job` is a live Job handle; `info` is zero-initialized POD of exact expected size.
    unsafe {
        checked(QueryInformationJobObject(
            job,
            JobObjectBasicAccountingInformation,
            (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
            size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            null_mut(),
        ))?;
    }
    Ok(info.ActiveProcesses)
}

pub(super) fn terminate_job_and_drain(job: HANDLE, timeout: Duration) -> io::Result<u32> {
    // SAFETY: `job` is a live Job handle; terminates all processes and descendants in the Job.
    unsafe {
        checked(TerminateJobObject(job, 125))?;
    }
    let deadline = Instant::now() + timeout;
    loop {
        let active = job_active_processes(job)?;
        if active == 0 || Instant::now() >= deadline {
            return Ok(active);
        }
        thread::sleep(Duration::from_millis(15));
    }
}

// A version-scoped behavioral probe, NOT a generic "is LPAC" token API.
// Everyone permits the user-side check; AC (ALL_APPLICATION_PACKAGES) permits
// the package-side check for ordinary AppContainers, but not for LPAC opt-out.
// Native tests require host + ordinary-container grants and LPAC denial.
pub(super) fn aap_access(process: HANDLE) -> io::Result<bool> {
    let mut token = null_mut();
    let mut duplicate = null_mut();
    let descriptor = wide("O:SYG:SYD:(A;;0x1;;;WD)(A;;0x1;;;AC)");
    let mut security = null_mut();
    // SAFETY: borrow the live process, own both returned token handles; the
    // impersonation duplicate satisfies AccessCheck's token contract. The SDDL
    // includes owner/group and grants one non-generic synthetic access bit.
    unsafe {
        checked(OpenProcessToken(
            process,
            TOKEN_QUERY | TOKEN_DUPLICATE,
            &mut token,
        ))?;
        let token = own(token)?;
        checked(DuplicateToken(
            token.as_raw_handle(),
            SecurityImpersonation,
            &mut duplicate,
        ))?;
        let duplicate = own(duplicate)?;
        checked(ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor.as_ptr(),
            SDDL_REVISION_1,
            &mut security,
            null_mut(),
        ))?;
        let mapping = GENERIC_MAPPING {
            GenericRead: 1,
            GenericWrite: 1,
            GenericExecute: 1,
            GenericAll: 1,
        };
        let mut privileges: PRIVILEGE_SET = zeroed();
        let mut bytes = size_of_val(&privileges) as u32;
        let mut granted = 0;
        let mut allowed = 0;
        // SAFETY: API-owned descriptor lives through AccessCheck; typed aligned
        // output buffers have exact supplied sizes. An insufficient privilege
        // buffer is an API failure, never interpreted as access denial.
        let result = checked(AccessCheck(
            security,
            duplicate.as_raw_handle(),
            1,
            &mapping,
            &mut privileges,
            &mut bytes,
            &mut granted,
            &mut allowed,
        ));
        // Match the SDDL conversion allocator even when AccessCheck failed,
        // after preserving the original error (LocalFree may change last error).
        LocalFree(security);
        result?;
        Ok(allowed != 0)
    }
}

pub(super) fn in_job(process: HANDLE, job: HANDLE) -> io::Result<bool> {
    let mut result = 0;
    // SAFETY: process/job are live handles (null job asks about any Job).
    unsafe {
        checked(IsProcessInJob(process, job, &mut result))?;
    }
    Ok(result != 0)
}

pub(super) fn handle_available(process: HANDLE, handle: HANDLE) -> io::Result<bool> {
    let mut duplicate = null_mut();
    // SAFETY: the source process stays owned and suspended through both checks.
    // Duplicate into the parent, without inheritance or closing the source.
    // A successful duplicate is uniquely owned and closed here. Only an explicit
    // invalid-handle result proves absence; access or other errors fail closed.
    unsafe {
        if DuplicateHandle(
            process,
            handle,
            GetCurrentProcess(),
            &mut duplicate,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        ) == 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_INVALID_HANDLE as i32) {
                return Ok(false);
            }
            return Err(error);
        }
        drop(own(duplicate)?);
    }
    Ok(true)
}

pub(super) fn sid_text(sid: PSID) -> io::Result<String> {
    let mut text = null_mut();
    // SAFETY: callers supply API-validated SIDs whose owners outlive this call.
    // The returned terminated allocation is read before matching LocalFree.
    unsafe {
        checked(ConvertSidToStringSidW(sid, &mut text))?;
        let mut len = 0;
        while *text.add(len) != 0 {
            len += 1;
        }
        let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
        LocalFree(text.cast());
        Ok(value)
    }
}

pub(super) fn token_information(class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<Aligned>> {
    let mut token = null_mut();
    let mut bytes = 0;
    // SAFETY: query only our token. Size discovery is checked; aligned storage
    // covers the requested structure and its inline SID until the caller is done.
    unsafe {
        checked(OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY,
            &mut token,
        ))?;
        let token = own(token)?;
        if GetTokenInformation(token.as_raw_handle(), class, null_mut(), 0, &mut bytes) != 0
            || io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
        {
            return Err(io::Error::other("token information sizing failed"));
        }
        let mut storage = vec![Aligned { _bytes: [0; 16] }; (bytes as usize).div_ceil(16)];
        checked(GetTokenInformation(
            token.as_raw_handle(),
            class,
            storage.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        ))?;
        Ok(storage)
    }
}

pub(super) fn create_kill_on_close_job() -> io::Result<OwnedHandle> {
    // SAFETY: unnamed Job handle transferred to OwnedHandle; POD limit struct omits breakaway.
    unsafe {
        let job = own(CreateJobObjectW(null(), null()))?;
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        checked(SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of_val(&limits) as u32,
        ))?;
        Ok(job)
    }
}

pub(crate) fn probe_capability() -> SandboxCapability {
    let check = (|| -> io::Result<()> {
        if std::env::var_os("SystemRoot").is_none_or(|v| v.is_empty()) {
            return Err(io::Error::other("SystemRoot unavailable"));
        }
        // SAFETY: borrowed pseudo handle, never closed.
        let host = unsafe { GetCurrentProcess() };
        if !aap_access(host)? {
            return Err(io::Error::other("host AAP access probe failed"));
        }
        let mut profile = Profile::create()?;
        let job = create_kill_on_close_job()?;
        let capabilities = SECURITY_CAPABILITIES {
            AppContainerSid: profile.sid,
            Capabilities: null_mut(),
            CapabilityCount: 0,
            Reserved: 0,
        };
        let policy = PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT;
        let mut attributes = Attributes::new()?;
        attributes.set(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, &capabilities)?;
        attributes.set(
            PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
            &policy,
        )?;
        drop(attributes);
        drop(job);
        profile.cleanup()?;
        Ok(())
    })();
    match check {
        Ok(()) => SandboxCapability::FileSystemAndNetworkIsolated,
        Err(error) => SandboxCapability::Unavailable {
            reason: error.to_string(),
        },
    }
}

pub(crate) fn run_sandboxed_with_mode(
    spec: &LaunchSpec,
    mode: SandboxMode,
    cancel: Option<&AtomicBool>,
) -> io::Result<RunResult> {
    let ordinary = matches!(mode, SandboxMode::AppContainerChildCapable);
    run_sandboxed_inner(spec, cancel, ordinary, None, None)
}

pub(crate) fn run_shell_sandboxed_with_door(
    spec: &ShellSpec,
    cancel: Option<&AtomicBool>,
    door: Option<DoorSpec>,
) -> io::Result<RunResult> {
    if spec.command.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "sandbox shell command must not be empty",
        ));
    }
    if spec.command.contains(['\0', '\r', '\n']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "sandbox shell command must not contain NUL, CR, or LF",
        ));
    }
    let system_root = std::env::var_os("SystemRoot")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::other("SystemRoot unavailable; refusing to resolve cmd.exe"))?;
    let cmd_exe = PathBuf::from(system_root).join("System32").join("cmd.exe");
    reject_reparse_point(&cmd_exe)?;
    let cmd_arg = quote_windows_arg(&cmd_exe.to_string_lossy())?;
    // `/D` disables AutoRun registry hooks; `/S /C "..."` strips only the outer quotes so
    // inner quotes and shell operators in `spec.command` are preserved verbatim.
    let raw_cmdline = format!("{cmd_arg} /D /S /C \"{}\"", spec.command);
    let launch = LaunchSpec {
        program: cmd_exe,
        args: Vec::new(),
        cwd: Some(spec.cwd.clone()),
        writable_dirs: spec.writable_dirs.clone(),
        readonly_dirs: spec.readonly_dirs.clone(),
        env: spec.env.clone(),
        timeout: spec.timeout,
        max_output_bytes: spec.max_output_bytes,
    };
    run_sandboxed_inner(&launch, cancel, true, Some(&raw_cmdline), door)
}

fn run_sandboxed_inner(
    spec: &LaunchSpec,
    cancel: Option<&AtomicBool>,
    ordinary_control: bool,
    raw_cmdline: Option<&str>,
    door_spec: Option<DoorSpec>,
) -> io::Result<RunResult> {
    if spec.timeout.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "sandbox timeout must be greater than zero",
        ));
    }
    reject_reparse_point(&spec.program)?;
    if !spec.program.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("sandbox program is not a file: {}", spec.program.display()),
        ));
    }
    let program = normalize_win32_long_path(&spec.program)?;
    let resolved_cwd = if let Some(cwd) = &spec.cwd {
        reject_reparse_point(cwd)?;
        if !cwd.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("sandbox cwd is not a directory: {}", cwd.display()),
            ));
        }
        Some(normalize_win32_long_path(cwd)?)
    } else {
        None
    };
    for dir in spec.writable_dirs.iter().chain(&spec.readonly_dirs) {
        reject_reparse_point(dir)?;
        if !dir.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("admitted path is not a directory: {}", dir.display()),
            ));
        }
    }

    let mut profile = Profile::create()?;
    let profile_dir = normalize_win32_long_path(&profile.path)?;
    let temp_dir = profile_dir.join("tmp");
    let home_dir = profile_dir.join("home");
    let appdata_dir = profile_dir.join("appdata");
    fs::create_dir(&temp_dir)?;
    fs::create_dir(&home_dir)?;
    fs::create_dir(&appdata_dir)?;

    admit_directories(&spec.readonly_dirs, &spec.writable_dirs, profile.sid)?;

    let job = create_kill_on_close_job()?;
    let mut desktop = if ordinary_control {
        PrivateDesktop::create(profile.sid).ok()
    } else {
        None
    };

    let mut door_env = spec.env.clone();
    let mut door_bridge: Option<Child> = None;
    let mut door_bridge_script: Option<PathBuf> = None;
    let mut door = if let Some(ds) = door_spec {
        if ds.env_var.is_empty() || ds.env_var.contains(['=', '\0']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid door env var name: {:?}", ds.env_var),
            ));
        }
        let started = NamedPipeDoor::start(profile.sid, ds.on_connect)?;
        door_env.push((ds.env_var, started.pipe_name.clone()));
        if ds.http_proxy_bridge {
            let dirs = ProfileDirs {
                profile_dir: &profile_dir,
                temp_dir: &temp_dir,
                home_dir: &home_dir,
                appdata_dir: &appdata_dir,
            };
            let (bridge_child, port, script_path) = spawn_door_bridge(
                &profile,
                &dirs,
                &job,
                desktop.as_mut(),
                ordinary_control,
                &started.pipe_name,
            )?;
            let proxy_url = format!("http://127.0.0.1:{port}");
            for key in PROXY_ENV_VARS {
                door_env.push((key.to_owned(), proxy_url.clone()));
            }
            door_bridge = Some(bridge_child);
            door_bridge_script = Some(script_path);
        }
        Some(started)
    } else {
        None
    };

    let output_path = profile.path.join("child-output.txt");
    let output_file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&output_path)?;
    let input = File::open("NUL")?;

    // SAFETY: mark only the two allowlisted stdio handles inheritable.
    unsafe {
        for handle in [output_file.as_raw_handle(), input.as_raw_handle()] {
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
    let handles = [input.as_raw_handle(), output_file.as_raw_handle()];
    let mut attributes = Attributes::new()?;
    attributes.set(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, &capabilities)?;
    if !ordinary_control {
        attributes.set(
            PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
            &policy,
        )?;
    }
    attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)?;

    let application = wide(&program);
    let cwd = resolved_cwd.as_deref().unwrap_or(&profile_dir);
    let directory = wide(cwd);
    let cmdline = if let Some(raw) = raw_cmdline {
        raw.to_owned()
    } else {
        let program_arg = quote_windows_arg(&program.to_string_lossy())?;
        let mut built = program_arg;
        for arg in &spec.args {
            built.push(' ');
            built.push_str(&quote_windows_arg(arg)?);
        }
        built
    };
    let mut command = wide(cmdline);
    let environment =
        build_environment(&profile_dir, &temp_dir, &home_dir, &appdata_dir, &door_env)?;

    // SAFETY: extended startup cb, valid aligned attributes, mutable command buffer,
    // allowlisted Unicode environment, and optional private desktop string live through CreateProcessW.
    let child = unsafe {
        let mut startup: STARTUPINFOEXW = zeroed();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        if let Some(desk) = desktop.as_mut() {
            startup.StartupInfo.lpDesktop = desk.lp_desktop.as_mut_ptr();
        }
        startup.StartupInfo.hStdInput = input.as_raw_handle();
        startup.StartupInfo.hStdOutput = output_file.as_raw_handle();
        startup.StartupInfo.hStdError = output_file.as_raw_handle();
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

    let run_outcome = (|| -> io::Result<ExitOutcome> {
        // SAFETY: process is suspended; Job membership, AppContainer/LPAC identity,
        // zero capabilities, and handle allowlist are verified before any user code runs.
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
            || !handle_available(child.process.as_raw_handle(), output_file.as_raw_handle())?
        {
            return Err(io::Error::other(
                "suspended LPAC / Job / capability / handle verification failed",
            ));
        }
        // SAFETY: primary thread is suspended and all pre-resume invariants passed.
        unsafe {
            if ResumeThread(child.thread.as_raw_handle()) == u32::MAX {
                return Err(io::Error::last_os_error());
            }
        }
        let started = Instant::now();
        loop {
            if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
                return Ok(ExitOutcome::Cancelled);
            }
            if started.elapsed() >= spec.timeout {
                return Ok(ExitOutcome::TimedOut);
            }
            // SAFETY: owned live process handle polled with short bounded wait.
            let wait = unsafe { WaitForSingleObject(child.process.as_raw_handle(), 20) };
            if wait == WAIT_OBJECT_0 {
                let mut code = 0u32;
                // SAFETY: process has exited; query its exit code.
                unsafe {
                    checked(GetExitCodeProcess(child.process.as_raw_handle(), &mut code))?;
                }
                return Ok(ExitOutcome::Exited(code));
            }
            if wait != WAIT_TIMEOUT {
                return Err(io::Error::last_os_error());
            }
        }
    })();

    // Always terminate the full Job tree and drain active descendants before
    // closing handles or cleaning up the profile (covers timeout, cancel, and
    // orphaned grandchildren holding stdout).
    let active_processes_after =
        terminate_job_and_drain(job.as_raw_handle(), Duration::from_secs(5))?;
    let stop_result = child.stop();
    drop(child);
    drop(door_bridge);
    drop(job);
    if let Some(mut d) = door.take() {
        d.stop();
    }
    drop(desktop);
    drop(attributes);
    drop(input);
    drop(output_file);

    let max_output = spec.max_output_bytes.unwrap_or(DEFAULT_MAX_OUTPUT_BYTES);
    let (output, total_output_bytes, output_truncated) =
        read_tail_bounded(&output_path, max_output).unwrap_or_else(|_| (Vec::new(), 0, false));
    let mut cleanup_warnings = Vec::new();
    if let Err(err) = fs::remove_file(&output_path) {
        cleanup_warnings.push(format!("output file cleanup: {err}"));
    }
    if let Some(script_path) = door_bridge_script
        && let Err(err) = fs::remove_file(&script_path)
        && err.kind() != io::ErrorKind::NotFound
    {
        cleanup_warnings.push(format!("door bridge script cleanup: {err}"));
    }
    for (label, dir) in [
        ("temp dir", &temp_dir),
        ("home dir", &home_dir),
        ("appdata dir", &appdata_dir),
    ] {
        if let Err(err) = fs::remove_dir_all(dir) {
            cleanup_warnings.push(format!("{label} cleanup: {err}"));
        }
    }
    if active_processes_after == 0 {
        if let Err(err) = profile.cleanup() {
            cleanup_warnings.push(format!("profile cleanup: {err}"));
        }
    } else {
        profile.registered = false;
        cleanup_warnings.push(format!(
            "retained profile due to {active_processes_after} active processes"
        ));
    }

    stop_result?;
    let outcome = run_outcome?;
    Ok(RunResult {
        outcome,
        output,
        output_truncated,
        total_output_bytes,
        active_processes_after,
        cleanup_warnings,
    })
}

pub(crate) struct SandboxedStdioInner {
    stdin: Option<File>,
    stdout: Option<File>,
    child: Option<Child>,
    door_bridge: Option<Child>,
    door_bridge_script: Option<PathBuf>,
    job: Option<OwnedHandle>,
    door: Option<NamedPipeDoor>,
    desktop: Option<PrivateDesktop>,
    profile: Option<Profile>,
    temp_dir: PathBuf,
    home_dir: PathBuf,
    appdata_dir: PathBuf,
    cleaned: bool,
    cleanup_warnings: Vec<String>,
}

impl SandboxedStdioInner {
    pub(crate) fn take_stdin(&mut self) -> Option<File> {
        self.stdin.take()
    }

    pub(crate) fn take_stdout(&mut self) -> Option<File> {
        self.stdout.take()
    }

    pub(crate) fn cleanup_warnings(&self) -> &[String] {
        &self.cleanup_warnings
    }

    pub(crate) fn shutdown(&mut self) -> io::Result<u32> {
        if self.cleaned {
            return Ok(0);
        }
        self.cleaned = true;
        drop(self.stdin.take());
        drop(self.stdout.take());

        let mut active_processes_after = 0u32;
        let mut drain_err = None;
        if let Some(job) = self.job.as_ref() {
            match terminate_job_and_drain(job.as_raw_handle(), Duration::from_secs(5)) {
                Ok(active) => active_processes_after = active,
                Err(err) => drain_err = Some(err),
            }
        }
        let stop_result = if let Some(child) = self.child.take() {
            let res = child.stop();
            drop(child);
            res
        } else {
            Ok(())
        };
        drop(self.door_bridge.take());
        drop(self.job.take());
        if let Some(mut door) = self.door.take() {
            door.stop();
        }
        drop(self.desktop.take());

        if let Some(script_path) = self.door_bridge_script.take()
            && let Err(err) = fs::remove_file(&script_path)
            && err.kind() != io::ErrorKind::NotFound
        {
            self.cleanup_warnings
                .push(format!("door bridge script cleanup: {err}"));
        }
        for (label, dir) in [
            ("temp dir", &self.temp_dir),
            ("home dir", &self.home_dir),
            ("appdata dir", &self.appdata_dir),
        ] {
            if let Err(err) = fs::remove_dir_all(dir)
                && err.kind() != io::ErrorKind::NotFound
            {
                self.cleanup_warnings
                    .push(format!("{label} cleanup: {err}"));
            }
        }
        if let Some(mut profile) = self.profile.take() {
            if active_processes_after == 0 && drain_err.is_none() {
                if let Err(err) = profile.cleanup() {
                    self.cleanup_warnings
                        .push(format!("profile cleanup: {err}"));
                }
            } else {
                profile.registered = false;
                self.cleanup_warnings.push(format!(
                    "retained profile due to {active_processes_after} active processes"
                ));
            }
        }

        if let Some(err) = drain_err {
            return Err(err);
        }
        stop_result?;
        Ok(active_processes_after)
    }
}

pub(crate) fn spawn_stdio_sandboxed_with_door(
    spec: &StdioSpec,
    door_spec: Option<DoorSpec>,
) -> io::Result<SandboxedStdioInner> {
    let ordinary_control = matches!(spec.mode, SandboxMode::AppContainerChildCapable);
    reject_reparse_point(&spec.program)?;
    if !spec.program.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("sandbox program is not a file: {}", spec.program.display()),
        ));
    }
    let program = normalize_win32_long_path(&spec.program)?;
    let resolved_cwd = if let Some(cwd) = &spec.cwd {
        reject_reparse_point(cwd)?;
        if !cwd.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("sandbox cwd is not a directory: {}", cwd.display()),
            ));
        }
        Some(normalize_win32_long_path(cwd)?)
    } else {
        None
    };
    for dir in spec.writable_dirs.iter().chain(&spec.readonly_dirs) {
        reject_reparse_point(dir)?;
        if !dir.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("admitted path is not a directory: {}", dir.display()),
            ));
        }
    }

    let profile = Profile::create()?;
    let profile_dir = normalize_win32_long_path(&profile.path)?;
    let temp_dir = profile_dir.join("tmp");
    let home_dir = profile_dir.join("home");
    let appdata_dir = profile_dir.join("appdata");
    fs::create_dir(&temp_dir)?;
    fs::create_dir(&home_dir)?;
    fs::create_dir(&appdata_dir)?;

    admit_directories(&spec.readonly_dirs, &spec.writable_dirs, profile.sid)?;

    let job = create_kill_on_close_job()?;
    let mut desktop = if ordinary_control {
        PrivateDesktop::create(profile.sid).ok()
    } else {
        None
    };

    let mut door_env = spec.env.clone();
    let mut door_bridge: Option<Child> = None;
    let mut door_bridge_script: Option<PathBuf> = None;
    let door = if let Some(ds) = door_spec {
        if ds.env_var.is_empty() || ds.env_var.contains(['=', '\0']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid door env var name: {:?}", ds.env_var),
            ));
        }
        let started = NamedPipeDoor::start(profile.sid, ds.on_connect)?;
        door_env.push((ds.env_var, started.pipe_name.clone()));
        if ds.http_proxy_bridge {
            let dirs = ProfileDirs {
                profile_dir: &profile_dir,
                temp_dir: &temp_dir,
                home_dir: &home_dir,
                appdata_dir: &appdata_dir,
            };
            let (bridge_child, port, script_path) = spawn_door_bridge(
                &profile,
                &dirs,
                &job,
                desktop.as_mut(),
                ordinary_control,
                &started.pipe_name,
            )?;
            let proxy_url = format!("http://127.0.0.1:{port}");
            for key in PROXY_ENV_VARS {
                door_env.push((key.to_owned(), proxy_url.clone()));
            }
            door_bridge = Some(bridge_child);
            door_bridge_script = Some(script_path);
        }
        Some(started)
    } else {
        None
    };

    let (child_stdin_read, parent_stdin_write) = create_anonymous_pipe()?;
    let (parent_stdout_read, child_stdout_write) = create_anonymous_pipe()?;
    let stderr_nul = OpenOptions::new().write(true).open("NUL")?;

    // SAFETY: mark ONLY the three child-side stdio handles inheritable; parent pipe ends remain
    // non-inheritable and are verified absent from the suspended child before resuming.
    unsafe {
        for handle in [
            child_stdin_read.as_raw_handle(),
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
        child_stdin_read.as_raw_handle(),
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

    let application = wide(&program);
    let cwd = resolved_cwd.as_deref().unwrap_or(&profile_dir);
    let directory = wide(cwd);
    let mut built = quote_windows_arg(&program.to_string_lossy())?;
    for arg in &spec.args {
        built.push(' ');
        built.push_str(&quote_windows_arg(arg)?);
    }
    let mut command = wide(built);
    let environment =
        build_environment(&profile_dir, &temp_dir, &home_dir, &appdata_dir, &door_env)?;

    // SAFETY: extended startup cb, valid aligned attributes, mutable command buffer,
    // allowlisted Unicode environment, and optional private desktop string live through CreateProcessW.
    let child = unsafe {
        let mut startup: STARTUPINFOEXW = zeroed();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        if let Some(desk) = desktop.as_mut() {
            startup.StartupInfo.lpDesktop = desk.lp_desktop.as_mut_ptr();
        }
        startup.StartupInfo.hStdInput = child_stdin_read.as_raw_handle();
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

    let mut inner = SandboxedStdioInner {
        stdin: Some(File::from(parent_stdin_write)),
        stdout: Some(File::from(parent_stdout_read)),
        child: Some(child),
        door_bridge,
        door_bridge_script,
        job: Some(job),
        door,
        desktop,
        profile: Some(profile),
        temp_dir,
        home_dir,
        appdata_dir,
        cleaned: false,
        cleanup_warnings: Vec::new(),
    };

    let startup_result = (|| -> io::Result<()> {
        let child_ref = inner.child.as_ref().expect("child present");
        let job_ref = inner.job.as_ref().expect("job present");
        let stdin_parent = inner.stdin.as_ref().expect("stdin present").as_raw_handle();
        let stdout_parent = inner
            .stdout
            .as_ref()
            .expect("stdout present")
            .as_raw_handle();
        // SAFETY: process is suspended; Job membership, AppContainer/LPAC identity,
        // zero capabilities, and handle allowlist (including absence of parent pipe ends)
        // are verified before any user code runs.
        unsafe {
            checked(AssignProcessToJobObject(
                job_ref.as_raw_handle(),
                child_ref.process.as_raw_handle(),
            ))?;
        }
        if !in_job(child_ref.process.as_raw_handle(), job_ref.as_raw_handle())?
            || token_flag(child_ref.process.as_raw_handle(), TokenIsAppContainer)? != 1
            || aap_access(child_ref.process.as_raw_handle())? != ordinary_control
            || token_capability_count(child_ref.process.as_raw_handle())? != 0
            || !handle_available(
                child_ref.process.as_raw_handle(),
                child_stdin_read.as_raw_handle(),
            )?
            || !handle_available(
                child_ref.process.as_raw_handle(),
                child_stdout_write.as_raw_handle(),
            )?
            || handle_available(child_ref.process.as_raw_handle(), stdin_parent)?
            || handle_available(child_ref.process.as_raw_handle(), stdout_parent)?
        {
            return Err(io::Error::other(
                "suspended W3 stdio AppContainer / Job / capability / handle verification failed",
            ));
        }
        // SAFETY: primary thread is suspended and all pre-resume invariants passed.
        unsafe {
            if ResumeThread(child_ref.thread.as_raw_handle()) == u32::MAX {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    })();

    // Drop child-side pipe ends and attribute list in the parent immediately so EOF and
    // broken-pipe semantics work identically to std::process::Child.
    drop(attributes);
    drop(child_stdin_read);
    drop(child_stdout_write);
    drop(stderr_nul);

    if let Err(err) = startup_result {
        let _ = inner.shutdown();
        return Err(err);
    }

    Ok(inner)
}
