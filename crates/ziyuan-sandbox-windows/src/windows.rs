//! Private FFI boundary for Windows LPAC + Job Object isolation (ADR-0001).
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    mem::{size_of, zeroed},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::*,
    Networking::WinSock::{WSACleanup, WSADATA, WSAEACCES, WSASYSCALLFAILURE, WSAStartup},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
        },
        Isolation::*,
        *,
    },
    Storage::FileSystem::*,
    System::{
        Com::CoTaskMemFree,
        JobObjects::*,
        SystemServices::{SYSTEM_MANDATORY_LABEL_ACE_TYPE, SYSTEM_MANDATORY_LABEL_NO_WRITE_UP},
        Threading::*,
        WindowsProgramming::PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT,
    },
};

use crate::{ExitOutcome, LaunchSpec, RunResult, SandboxCapability};

static PROFILE_SEQ: AtomicU64 = AtomicU64::new(1);

fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

#[track_caller]
fn checked(value: i32) -> io::Result<()> {
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
unsafe fn own(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: caller transfers ownership; OwnedHandle closes exactly once.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
}

/// Rejects any path where the target or any non-root ancestor is a Win32 reparse point
/// (symlink, junction, or mount point). Fails closed if attributes cannot be queried.
fn reject_reparse_point(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path must be absolute: {}", path.display()),
        ));
    }
    for ancestor in path.ancestors() {
        if ancestor.parent().is_none() {
            break;
        }
        // SAFETY: `wide(ancestor)` produces a NUL-terminated UTF-16 path living through the call.
        let attrs = unsafe { GetFileAttributesW(wide(ancestor).as_ptr()) };
        if attrs == INVALID_FILE_ATTRIBUTES {
            return Err(io::Error::last_os_error());
        }
        if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "refusing reparse point in sandbox path: {}",
                    ancestor.display()
                ),
            ));
        }
    }
    Ok(())
}

struct Profile {
    name: Vec<u16>,
    sid: PSID,
    path: PathBuf,
    registered: bool,
}

impl Profile {
    fn create() -> io::Result<Self> {
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

    fn cleanup(&mut self) -> io::Result<()> {
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

// Attribute-list storage requires native allocation alignment. This fixed-size
// element provides 16-byte alignment without relying on Vec<u8>'s allocator.
#[repr(align(16))]
#[derive(Clone, Copy)]
struct Aligned {
    _bytes: [u8; 16],
}
struct Attributes {
    storage: Vec<Aligned>,
}
impl Attributes {
    fn new() -> io::Result<Self> {
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
    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
    fn set<T>(&mut self, attribute: u32, value: &T) -> io::Result<()> {
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

struct Child {
    process: OwnedHandle,
    thread: OwnedHandle,
}
impl Child {
    fn stop(&self) -> io::Result<()> {
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

fn token_flag(process: HANDLE, class: TOKEN_INFORMATION_CLASS) -> io::Result<u32> {
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

fn token_capability_count(process: HANDLE) -> io::Result<u32> {
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

fn terminate_job_and_drain(job: HANDLE, timeout: Duration) -> io::Result<u32> {
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
fn aap_access(process: HANDLE) -> io::Result<bool> {
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

fn in_job(process: HANDLE, job: HANDLE) -> io::Result<bool> {
    let mut result = 0;
    // SAFETY: process/job are live handles (null job asks about any Job).
    unsafe {
        checked(IsProcessInJob(process, job, &mut result))?;
    }
    Ok(result != 0)
}

fn handle_available(process: HANDLE, handle: HANDLE) -> io::Result<bool> {
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

fn child_probe(ordinary: bool) -> io::Result<()> {
    writeln!(io::stdout(), "CHILD_ENTERED=1")?;
    io::stdout().flush()?;
    // SAFETY: borrowed pseudo handle, never closed.
    let process = unsafe { GetCurrentProcess() };
    let app = token_flag(process, TokenIsAppContainer)?;
    let aap = aap_access(process)?;
    let job = in_job(process, null_mut())?;
    let directory = std::env::current_dir()?;
    let environment_ok = std::env::var_os("SystemRoot").is_some_and(|value| !value.is_empty())
        && std::env::var_os("LOCALAPPDATA").is_some_and(|value| {
            // Confirms task-private LOCALAPPDATA supplied by the launcher.
            PathBuf::from(value).starts_with(&directory)
        })
        && std::env::var_os("ZIYUAN_W1A_HOST_ONLY").is_none();
    writeln!(
        io::stdout(),
        "APP={app} AAP_ACCESS={} JOB={} ENV={}",
        u8::from(aap),
        u8::from(job),
        u8::from(environment_ok)
    )?;
    if app != 1 || aap != ordinary || !job || !environment_ok {
        return Err(io::Error::other(
            "child identity / environment check failed",
        ));
    }
    Ok(())
}

fn sid_text(sid: PSID) -> io::Result<String> {
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

fn token_information(class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<Aligned>> {
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

fn admit_dependency(root: &Path, app_sid: PSID, writable: bool) -> io::Result<()> {
    reject_reparse_point(root)?;
    let storage = token_information(TokenUser)?;
    // SAFETY: exact API class, aligned live storage including the pointed-to SID.
    let user = unsafe { sid_text((*storage.as_ptr().cast::<TOKEN_USER>()).User.Sid)? };
    let app = sid_text(app_sid)?;
    // Only this exclusively created dependency directory is changed. Protect
    // against parent inheritance, keep host cleanup rights, and suppress implicit
    // owner WRITE_DAC: the LPAC still carries the host user's owner identity.
    // Install object-specific masks, not generic bits, in existing file ACLs.
    let access = if writable { "FA" } else { "FRFX" };
    let sddl = wide(format!(
        "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{user})(A;OICI;RC;;;OW)(A;OICI;{access};;;{app})"
    ));
    let mut descriptor = null_mut();
    // SAFETY: converted descriptor and embedded DACL remain live until the
    // setting call ends. Its inheritable ACEs also protect child files.
    unsafe {
        checked(ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        ))?;
        let result = (|| {
            let mut present = 0;
            let mut defaulted = 0;
            let mut dacl = null_mut();
            checked(GetSecurityDescriptorDacl(
                descriptor,
                &mut present,
                &mut dacl,
                &mut defaulted,
            ))?;
            if present == 0 || dacl.is_null() {
                return Err(io::Error::other("dependency DACL missing"));
            }
            let error = SetNamedSecurityInfoW(
                wide(root).as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                dacl,
                null(),
            );
            if error != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            Ok(())
        })();
        LocalFree(descriptor);
        result
    }
}

fn low_integrity(path: &Path) -> io::Result<()> {
    let mut descriptor = null_mut();
    // SAFETY: label only our owned synthetic objects. No audit/host SACL is
    // replaced. All pointers live through the call and use matching LocalFree.
    unsafe {
        checked(ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide("S:(ML;;NW;;;LW)").as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        ))?;
        let result = (|| {
            let mut present = 0;
            let mut defaulted = 0;
            let mut sacl = null_mut();
            checked(GetSecurityDescriptorSacl(
                descriptor,
                &mut present,
                &mut sacl,
                &mut defaulted,
            ))?;
            if present == 0 || sacl.is_null() {
                return Err(io::Error::other("dependency integrity label missing"));
            }
            let error = SetNamedSecurityInfoW(
                wide(path).as_ptr(),
                SE_FILE_OBJECT,
                LABEL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null(),
                sacl,
            );
            if error != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            Ok(())
        })();
        LocalFree(descriptor);
        result
    }
}

/// Admits a task-private directory tree with protected DACL + explicit low IL,
/// rejecting any reparse point encountered within the tree.
fn admit_tree(root: &Path, app_sid: PSID, writable: bool) -> io::Result<()> {
    admit_dependency(root, app_sid, writable)?;
    low_integrity(root)?;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        if depth > 16 {
            return Err(io::Error::other(
                "admitted directory tree exceeds max depth",
            ));
        }
        for entry in fs::read_dir(&dir)? {
            visited += 1;
            if visited > 2048 {
                return Err(io::Error::other(
                    "admitted directory tree exceeds max entry count",
                ));
            }
            let path = entry?.path();
            reject_reparse_point(&path)?;
            low_integrity(&path)?;
            if path.is_dir() {
                stack.push((path, depth + 1));
            }
        }
    }
    Ok(())
}

fn check_low_integrity(root: &Path) -> io::Result<()> {
    for path in [
        root.to_owned(),
        root.join("data.txt"),
        root.join("probe.exe"),
    ] {
        let mut descriptor = null_mut();
        let mut sacl = null_mut();
        // SAFETY: Windows returns an owned valid descriptor. Embedded ACL / ACE
        // and SID are inspected only while that allocation is live.
        unsafe {
            let error = GetNamedSecurityInfoW(
                wide(&path).as_ptr(),
                SE_FILE_OBJECT,
                LABEL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null_mut(),
                &mut sacl,
                &mut descriptor,
            );
            if error != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            let result = (|| {
                if sacl.is_null() || (*sacl).AceCount != 1 {
                    return Err(io::Error::other("expected one explicit integrity label"));
                }
                let mut ace = null_mut();
                checked(GetAce(sacl, 0, &mut ace))?;
                let header = &*ace.cast::<ACE_HEADER>();
                if u32::from(header.AceType) != SYSTEM_MANDATORY_LABEL_ACE_TYPE
                    || (header.AceSize as usize) < size_of::<SYSTEM_MANDATORY_LABEL_ACE>()
                {
                    return Err(io::Error::other("unexpected integrity ACE type / size"));
                }
                let label = &*ace.cast::<SYSTEM_MANDATORY_LABEL_ACE>();
                if label.Mask != SYSTEM_MANDATORY_LABEL_NO_WRITE_UP {
                    return Err(io::Error::other("unexpected integrity ACE"));
                }
                let sid = (&label.SidStart as *const u32).cast_mut().cast();
                if sid_text(sid)? != "S-1-16-4096" {
                    return Err(io::Error::other("dependency integrity is not low"));
                }
                Ok(())
            })();
            LocalFree(descriptor);
            result?;
        }
    }
    Ok(())
}

fn dependency_rights(root: &Path, denied: bool) -> io::Result<()> {
    let requests = [
        ("write", FILE_WRITE_DATA),
        ("append", FILE_APPEND_DATA),
        ("extended_attributes", FILE_WRITE_EA),
        ("attributes", FILE_WRITE_ATTRIBUTES),
        ("delete", DELETE),
        ("dacl", WRITE_DAC),
        ("owner", WRITE_OWNER),
    ];
    for (path, directory) in [
        (root.to_owned(), true),
        (root.join("data.txt"), false),
        (root.join("probe.exe"), false),
    ] {
        for (name, right) in requests
            .into_iter()
            .chain(directory.then_some(("delete_child", FILE_DELETE_CHILD)))
        {
            // SAFETY: live terminated path; no creation, truncation, mutation or
            // inherited handle. Request rights individually so one denied bit
            // cannot hide a grant of another. All share modes avoid fake denials.
            let handle = unsafe {
                CreateFileW(
                    wide(&path).as_ptr(),
                    right,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS,
                    null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                let error = io::Error::last_os_error();
                if !denied || error.raw_os_error() != Some(ERROR_ACCESS_DENIED as i32) {
                    return Err(io::Error::other(format!("dependency {name}: {error}")));
                }
            } else {
                // SAFETY: CreateFileW returned a new uniquely owned handle.
                drop(unsafe { own(handle)? });
                if denied {
                    return Err(io::Error::other(format!(
                        "dependency {name} unexpectedly granted"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn check_dependency_image(root: &Path) -> io::Result<()> {
    // Hold all read handles throughout comparison: identity cannot be recycled
    // between queries. No DOS-volume path lookup or extra grants. The control
    // has identical bytes on the same volume but is a separately created file.
    let running = File::open(std::env::current_exe()?)?;
    let admitted = File::open(root.join("probe.exe"))?;
    let control = File::open(root.join("control/probe.exe"))?;
    let mut identities = [FILE_ID_INFO::default(); 3];
    for (file, identity) in [&running, &admitted, &control]
        .into_iter()
        .zip(&mut identities)
    {
        // SAFETY: owned live file handles; exact initialized, aligned output
        // structure. Query failure never counts as an identity match.
        unsafe {
            checked(GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                (identity as *mut FILE_ID_INFO).cast(),
                size_of::<FILE_ID_INFO>() as u32,
            ))?;
        }
    }
    if identities[0].VolumeSerialNumber != identities[1].VolumeSerialNumber
        || identities[0].FileId.Identifier != identities[1].FileId.Identifier
        || identities[0].VolumeSerialNumber != identities[2].VolumeSerialNumber
        || identities[0].FileId.Identifier == identities[2].FileId.Identifier
    {
        return Err(io::Error::other(
            "dependency image identity / mismatch control failed",
        ));
    }
    Ok(())
}

const DEPENDENCY_EVIDENCE: &str = "DEPENDENCY_IMAGE_ID=1 IMAGE_MISMATCH_REJECTED=1\n\
DEPENDENCY_TOKEN_IL=4096 OBJECT_IL=4096 CONTROL_RIGHTS=1\n\
DEPENDENCY_READ_EXECUTE=1 RIGHTS_DENIED=1 DELETE_RENAME_CREATE_DENIED=1";

fn dependency_child(host: &Path) -> io::Result<()> {
    let root = crate::files::dependency_path(host);
    check_dependency_image(&root)
        .map_err(|error| io::Error::other(format!("dependency image identity: {error}")))?;
    crate::files::read_dependency(&root)?;
    let storage = token_information(TokenIntegrityLevel)?;
    // SAFETY: exact token class with aligned live structure and inline SID.
    let integrity = unsafe {
        sid_text(
            (*storage.as_ptr().cast::<TOKEN_MANDATORY_LABEL>())
                .Label
                .Sid,
        )?
    };
    if integrity != "S-1-16-4096" {
        return Err(io::Error::other(format!(
            "expected low-integrity LPAC, got {integrity}"
        )));
    }
    let control = root.join("control");
    check_low_integrity(&root)
        .map_err(|error| io::Error::other(format!("read-only dependency label query: {error}")))?;
    check_low_integrity(&control)
        .map_err(|error| io::Error::other(format!("writable dependency label query: {error}")))?;
    dependency_rights(&control, false)?;
    crate::files::writable_dependency_control(&control)?;
    dependency_rights(&root, true)?;
    // The non-image data file avoids mistaking a mapped executable's delete
    // restriction for our policy. Successful mutation is always a failed probe.
    for (name, result) in [
        ("delete", fs::remove_file(root.join("data.txt"))),
        (
            "rename",
            fs::rename(root.join("data.txt"), root.join("replaced.txt")),
        ),
        (
            "create",
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(root.join("new.txt"))
                .map(drop),
        ),
    ] {
        let code = result.err().and_then(|error| error.raw_os_error());
        if code != Some(ERROR_ACCESS_DENIED as i32) {
            return Err(io::Error::other(format!(
                "dependency {name}: expected access denied, got {code:?}"
            )));
        }
    }
    println!("{DEPENDENCY_EVIDENCE}");
    Ok(())
}

fn quote_windows_arg(arg: &str) -> io::Result<String> {
    if arg.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "argument contains NUL byte",
        ));
    }
    if !arg.is_empty()
        && !arg
            .chars()
            .any(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '"'))
    {
        return Ok(arg.to_owned());
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                for _ in 0..backslashes * 2 + 1 {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(ch);
            }
        }
    }
    for _ in 0..backslashes * 2 {
        out.push('\\');
    }
    out.push('"');
    Ok(out)
}

fn build_environment(
    profile_dir: &Path,
    temp_dir: &Path,
    extra: &[(String, String)],
) -> io::Result<Vec<u16>> {
    let root = std::env::var_os("SystemRoot")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            io::Error::other("SystemRoot unavailable; refusing to inherit the host environment")
        })?;
    let mut map: BTreeMap<String, (String, OsString)> = BTreeMap::new();
    map.insert(
        "LOCALAPPDATA".into(),
        ("LOCALAPPDATA".into(), profile_dir.as_os_str().to_owned()),
    );
    map.insert("SYSTEMROOT".into(), ("SystemRoot".into(), root));
    map.insert(
        "TEMP".into(),
        ("TEMP".into(), temp_dir.as_os_str().to_owned()),
    );
    map.insert(
        "TMP".into(),
        ("TMP".into(), temp_dir.as_os_str().to_owned()),
    );
    const RESERVED: [&str; 8] = [
        "SYSTEMROOT",
        "LOCALAPPDATA",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "APPDATA",
        "HOMEDRIVE",
        "HOMEPATH",
    ];
    for (k, v) in extra {
        if k.is_empty() || k.contains('=') || k.contains('\0') || v.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid sandbox environment variable name/value: {k:?}"),
            ));
        }
        let upper = k.to_ascii_uppercase();
        if RESERVED.contains(&upper.as_str()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("refusing override of reserved sandbox env var: {k}"),
            ));
        }
        map.insert(upper, (k.clone(), OsString::from(v)));
    }
    let mut block = Vec::new();
    for (key, value) in map.into_values() {
        let mut entry = OsString::from(key);
        entry.push("=");
        entry.push(value);
        block.extend(wide(entry));
    }
    block.push(0);
    Ok(block)
}

fn create_kill_on_close_job() -> io::Result<OwnedHandle> {
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

pub(crate) fn run_sandboxed(
    spec: &LaunchSpec,
    cancel: Option<&AtomicBool>,
) -> io::Result<RunResult> {
    run_sandboxed_inner(spec, cancel, false)
}

fn run_sandboxed_inner(
    spec: &LaunchSpec,
    cancel: Option<&AtomicBool>,
    ordinary_control: bool,
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
    if let Some(cwd) = &spec.cwd {
        reject_reparse_point(cwd)?;
        if !cwd.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("sandbox cwd is not a directory: {}", cwd.display()),
            ));
        }
    }
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
    let temp_dir = profile.path.join("tmp");
    fs::create_dir(&temp_dir)?;

    for dir in &spec.writable_dirs {
        admit_tree(dir, profile.sid, true)?;
    }
    for dir in &spec.readonly_dirs {
        admit_tree(dir, profile.sid, false)?;
    }

    let output_path = profile.path.join("child-output.txt");
    let output_file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&output_path)?;
    let input = File::open("NUL")?;
    let job = create_kill_on_close_job()?;

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

    let application = wide(&spec.program);
    let cwd = spec.cwd.as_deref().unwrap_or(&profile.path);
    let directory = wide(cwd);
    let program_arg = quote_windows_arg(&spec.program.to_string_lossy())?;
    let mut cmdline = program_arg;
    for arg in &spec.args {
        cmdline.push(' ');
        cmdline.push_str(&quote_windows_arg(arg)?);
    }
    let mut command = wide(cmdline);
    let environment = build_environment(&profile.path, &temp_dir, &spec.env)?;

    // SAFETY: extended startup cb, valid aligned attributes, mutable command buffer,
    // allowlisted Unicode environment, all living through CreateProcessW.
    let child = unsafe {
        let mut startup: STARTUPINFOEXW = zeroed();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
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
        // SAFETY: process is suspended; Job membership, LPAC identity, zero capabilities,
        // and handle allowlist are verified before any user code can execute.
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
    drop(job);
    drop(attributes);
    drop(input);
    drop(output_file);

    let output = fs::read(&output_path).unwrap_or_default();
    let mut cleanup_warnings = Vec::new();
    if let Err(err) = fs::remove_file(&output_path) {
        cleanup_warnings.push(format!("output file cleanup: {err}"));
    }
    if let Err(err) = fs::remove_dir_all(&temp_dir) {
        cleanup_warnings.push(format!("temp dir cleanup: {err}"));
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
        active_processes_after,
        cleanup_warnings,
    })
}

fn winsock_child_probe(tcp4: &str, udp4: &str, tcp6: &str) -> io::Result<()> {
    // SAFETY: borrowed pseudo handle, never closed.
    let caps = token_capability_count(unsafe { GetCurrentProcess() })?;
    if caps != 0 {
        return Err(io::Error::other(format!(
            "expected 0 token capabilities in LPAC, got {caps}"
        )));
    }
    // Probe WSAStartup directly before calling std::net so LPAC's kernel-level
    // Winsock initialization denial (WSASYSCALLFAILURE / WSAEACCES) is observed
    // cleanly rather than triggering std::net's internal assert_eq!(ret, 0) panic.
    // SAFETY: POD WSADATA permits zero initialization; valid version 2.2 pointer.
    let mut wsa: WSADATA = unsafe { zeroed() };
    let wsa_ret = unsafe { WSAStartup(0x0202, &mut wsa) };
    if wsa_ret == WSASYSCALLFAILURE || wsa_ret == WSAEACCES {
        println!("NET_WINSOCK_INIT_DENIED={wsa_ret}");
        println!("{}", crate::network::NETWORK_EVIDENCE);
        io::stdout().flush()?;
        return Ok(());
    }
    if wsa_ret != 0 {
        return Err(io::Error::other(format!(
            "unexpected WSAStartup error inside LPAC: {wsa_ret}"
        )));
    }
    let result = crate::network::child_probe(tcp4, udp4, tcp6, caps);
    // SAFETY: matches successful WSAStartup above.
    unsafe {
        WSACleanup();
    }
    result
}

fn run_lifecycle_probe(mode: &str) -> io::Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let seq = PROFILE_SEQ.fetch_add(1, Ordering::Relaxed);
    let stage =
        std::env::temp_dir().join(format!("ziyuan-w1d-{}-{seq}-{stamp}", std::process::id()));
    fs::create_dir(&stage)?;
    let program = stage.join("w1d-probe.exe");
    fs::copy(std::env::current_exe()?, &program)?;
    let child_mode = if mode == "--lifecycle-orphan" {
        "orphan"
    } else {
        "hold"
    };
    let cancel_flag = AtomicBool::new(false);
    let started = Instant::now();
    let spec = LaunchSpec {
        program: program.clone(),
        args: vec![
            "--child".into(),
            "true".into(),
            "--grandchild-hold".into(),
            child_mode.into(),
        ],
        cwd: None,
        writable_dirs: Vec::new(),
        readonly_dirs: vec![stage.clone()],
        env: Vec::new(),
        timeout: match mode {
            "--lifecycle-timeout" => Duration::from_millis(1200),
            "--lifecycle-cancel" => Duration::from_secs(15),
            "--lifecycle-orphan" => Duration::from_secs(15),
            _ => Duration::from_secs(5),
        },
    };
    let result = if mode == "--lifecycle-cancel" {
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(700));
                cancel_flag.store(true, Ordering::SeqCst);
            });
            run_sandboxed_inner(&spec, Some(&cancel_flag), true)
        })?
    } else {
        run_sandboxed_inner(&spec, None, true)?
    };
    let elapsed = started.elapsed();
    let _ = fs::remove_dir_all(&stage);

    let text = String::from_utf8_lossy(&result.output);
    if !text.contains("GRANDCHILD_SPAWNED=1") {
        return Err(io::Error::other(format!(
            "grandchild spawn evidence missing ({mode}): {text:?}"
        )));
    }
    if result.active_processes_after != 0 {
        return Err(io::Error::other(format!(
            "Job still had {} active processes after termination",
            result.active_processes_after
        )));
    }
    if elapsed > Duration::from_secs(10) {
        return Err(io::Error::other(format!(
            "lifecycle probe hung waiting for grandchild: elapsed={elapsed:?}"
        )));
    }
    match (mode, result.outcome) {
        ("--lifecycle-timeout", ExitOutcome::TimedOut) => {
            println!(
                "LIFECYCLE_TIMEOUT=1 GRANDCHILD_SPAWNED=1 JOB_ACTIVE_AFTER=0 PROFILE_CLEANED=1"
            );
        }
        ("--lifecycle-cancel", ExitOutcome::Cancelled) => {
            println!(
                "LIFECYCLE_CANCEL=1 GRANDCHILD_SPAWNED=1 JOB_ACTIVE_AFTER=0 PROFILE_CLEANED=1"
            );
        }
        ("--lifecycle-orphan", ExitOutcome::Exited(0)) => {
            println!(
                "LIFECYCLE_ORPHAN=1 GRANDCHILD_SPAWNED=1 JOB_ACTIVE_AFTER=0 PROFILE_CLEANED=1"
            );
        }
        (_, other) => {
            return Err(io::Error::other(format!(
                "unexpected outcome for {mode}: {other:?}, output={text:?}"
            )));
        }
    }
    Ok(())
}

pub(super) fn run() -> io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() == 1 && args[0] == "--grandchild-sleep" {
        thread::sleep(Duration::from_secs(60));
        return Ok(());
    }
    if args.len() == 2 && args[0] == "--child" {
        return child_probe(args[1].parse().map_err(io::Error::other)?);
    }
    if args.len() == 3 && args[..3] == ["--child", "false", "--lpac-sleep"] {
        child_probe(false)?;
        writeln!(io::stdout(), "LPAC_SLEEP_ENTERED=1")?;
        io::stdout().flush()?;
        thread::sleep(Duration::from_secs(60));
        return Ok(());
    }
    if args.len() == 4 && args[..3] == ["--child", "true", "--grandchild-hold"] {
        child_probe(true)?;
        let exe =
            std::env::current_exe().map_err(|e| io::Error::other(format!("current_exe: {e}")))?;
        let app = wide(&exe);
        let mut cmd = wide(format!("\"{}\" --grandchild-sleep", exe.display()));
        // SAFETY: valid terminated UTF-16 strings; grandchild inherits stdout so
        // Job termination must kill it before the parent can remove output/profile.
        unsafe {
            let mut si: STARTUPINFOW = zeroed();
            si.cb = size_of::<STARTUPINFOW>() as u32;
            si.dwFlags = STARTF_USESTDHANDLES;
            si.hStdInput = io::stdin().as_raw_handle();
            si.hStdOutput = io::stdout().as_raw_handle();
            si.hStdError = io::stderr().as_raw_handle();
            let mut pi: PROCESS_INFORMATION = zeroed();
            checked(CreateProcessW(
                app.as_ptr(),
                cmd.as_mut_ptr(),
                null(),
                null(),
                1,
                CREATE_NO_WINDOW,
                null(),
                null(),
                &si,
                &mut pi,
            ))?;
            drop(own(pi.hThread)?);
            drop(own(pi.hProcess)?);
        }
        writeln!(io::stdout(), "GRANDCHILD_SPAWNED=1")?;
        io::stdout().flush()?;
        if args[3] == "hold" {
            thread::sleep(Duration::from_secs(60));
        }
        // In "orphan" mode, primary exits immediately while grandchild stays alive holding stdout.
        return Ok(());
    }
    if args.len() == 6 && args[..3] == ["--child", "false", "--network-probe"] {
        child_probe(false)?;
        winsock_child_probe(&args[3], &args[4], &args[5])?;
        return Ok(());
    }
    if args.len() == 5
        && args[..2] == ["--child", "false"]
        && matches!(
            args[2].as_str(),
            "--file-probe" | "--readonly-dependency" | "--boundary-probe"
        )
    {
        child_probe(false)?;
        crate::files::child(args[3].as_ref(), args[4].as_ref())?;
        if args[2] == "--readonly-dependency" {
            dependency_child(args[3].as_ref())?;
        }
        if args[2] == "--boundary-probe" {
            crate::files::boundary_child(args[3].as_ref())?;
        }
        return Ok(());
    }
    if args.len() == 1
        && matches!(
            args[0].as_str(),
            "--lifecycle-timeout" | "--lifecycle-cancel" | "--lifecycle-orphan"
        )
    {
        return run_lifecycle_probe(&args[0]);
    }
    let fault = args == ["--fail-before-resume"];
    let ordinary = args == ["--ordinary-appcontainer-control"];
    let readonly = args == ["--readonly-dependency"];
    let boundary = args == ["--boundary-probe"];
    let network_probe = args == ["--network-probe"];
    let file_probe = args == ["--file-probe"] || readonly || boundary;
    if !args.is_empty() && !fault && !ordinary && !file_probe && !network_probe {
        return Err(io::Error::other("unknown probe arguments"));
    }
    // SAFETY: borrowed pseudo handle, never closed or inherited.
    let host = unsafe { GetCurrentProcess() };
    println!(
        "HOST_ELEVATED={} HOST_JOB={}",
        token_flag(host, TokenElevation)?,
        u8::from(in_job(host, null_mut())?)
    );
    if !aap_access(host)? {
        return Err(io::Error::other("host AAP access control failed"));
    }
    println!("HOST_AAP_ACCESS=1");
    let net_listeners = if network_probe {
        let listeners = crate::network::HostListeners::start()?;
        println!("HOST_NET_CONTROLS=1");
        Some(listeners)
    } else {
        None
    };
    let mut profile = Profile::create()?;
    let mut peer_profile = if file_probe {
        Some(Profile::create()?)
    } else {
        None
    };
    let mut fixtures = match &peer_profile {
        Some(peer) => {
            let fixtures = crate::files::Fixtures::create(&profile.path, &peer.path)?;
            println!("HOST_FILES_READ_WRITE=1");
            Some(fixtures)
        }
        None => None,
    };
    let program = if readonly {
        let root = fixtures
            .as_mut()
            .expect("file probe has fixtures")
            .prepare_dependency()?;
        admit_dependency(&root, profile.sid, false)?;
        let control = root.join("control");
        admit_dependency(&control, profile.sid, true)?;
        for directory in [&root, &control] {
            for path in [
                directory.to_owned(),
                directory.join("data.txt"),
                directory.join("probe.exe"),
            ] {
                low_integrity(&path)?;
            }
            check_low_integrity(directory)?;
            dependency_rights(directory, false)?;
        }
        println!("HOST_DEPENDENCY_RIGHTS=1");
        root.join("probe.exe")
    } else {
        let program = profile.path.join("w1a-probe.exe");
        fs::copy(std::env::current_exe()?, &program)?;
        program
    };
    let output_path = profile.path.join("child-output.txt");
    let output = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&output_path)?;
    let input = File::open("NUL")?;
    // SAFETY: newly created unnamed event transfers unique ownership.
    let sentinel = unsafe { own(CreateEventW(null(), 1, 1, null()))? };
    let job = create_kill_on_close_job()?;
    // SAFETY: mark stdio + sentinel inheritable so HANDLE_LIST filtering is tested.
    unsafe {
        for handle in [
            output.as_raw_handle(),
            input.as_raw_handle(),
            sentinel.as_raw_handle(),
        ] {
            checked(SetHandleInformation(
                handle,
                HANDLE_FLAG_INHERIT,
                HANDLE_FLAG_INHERIT,
            ))?;
        }
        if WaitForSingleObject(sentinel.as_raw_handle(), 0) != WAIT_OBJECT_0 {
            return Err(io::Error::other("host sentinel control failed"));
        }
        let mut flags = 0;
        checked(GetHandleInformation(sentinel.as_raw_handle(), &mut flags))?;
        if flags & HANDLE_FLAG_INHERIT == 0 {
            return Err(io::Error::other("host sentinel is not inheritable"));
        }
    }
    // All referenced attribute values outlive CreateProcessW. No capabilities,
    // inherited environment, host working directory, or arbitrary CLI command.
    let capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: profile.sid,
        Capabilities: null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    let policy = PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT;
    let handles = [input.as_raw_handle(), output.as_raw_handle()];
    let mut attributes = Attributes::new()?;
    attributes.set(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, &capabilities)?;
    // Ordinary AppContainer is only a separately requested positive control,
    // never a retry or fallback after a failed LPAC launch.
    if !ordinary {
        attributes.set(
            PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
            &policy,
        )?;
    }
    attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)?;
    let application = wide(&program);
    let directory = wide(&profile.path);
    // These exclusively owned paths contain no quotes or trailing backslashes.
    // No user command or user-supplied arguments are forwarded.
    let mut command = format!("\"{}\" --child {}", program.display(), ordinary);
    if let Some(files) = &fixtures {
        let mode = if readonly {
            "--readonly-dependency"
        } else if boundary {
            "--boundary-probe"
        } else {
            "--file-probe"
        };
        command.push_str(&format!(
            " {mode} \"{}\" \"{}\"",
            files.host_path().display(),
            files.peer_path().display()
        ));
    } else if let Some(listeners) = &net_listeners {
        let tcp6 = listeners
            .tcp6_addr()
            .map_or_else(|| "none".to_owned(), |addr| addr.to_string());
        command.push_str(&format!(
            " --network-probe {} {} {}",
            listeners.tcp4_addr(),
            listeners.udp4_addr(),
            tcp6
        ));
    }
    let mut command = wide(command);
    let root = std::env::var_os("SystemRoot")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            io::Error::other("SystemRoot unavailable; refusing to inherit the host environment")
        })?;
    let mut entry = OsString::from("LOCALAPPDATA=");
    entry.push(&profile.path);
    let mut environment = wide(entry);
    let mut entry = OsString::from("SystemRoot=");
    entry.push(root);
    environment.extend(wide(entry));
    environment.push(0);
    // SAFETY: extended startup cb, valid aligned attributes and mutable command
    // buffer; strings and allowlisted Unicode environment live through creation.
    let child = unsafe {
        let mut startup: STARTUPINFOEXW = zeroed();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = input.as_raw_handle();
        startup.StartupInfo.hStdOutput = output.as_raw_handle();
        startup.StartupInfo.hStdError = output.as_raw_handle();
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
    let mut injected = false;
    let result = (|| -> io::Result<()> {
        // SAFETY: process is suspended; it cannot execute user code before Job
        // membership and token checks succeed. No breakaway retry is permitted.
        unsafe {
            checked(AssignProcessToJobObject(
                job.as_raw_handle(),
                child.process.as_raw_handle(),
            ))?;
        }
        if !in_job(child.process.as_raw_handle(), job.as_raw_handle())?
            || token_flag(child.process.as_raw_handle(), TokenIsAppContainer)? != 1
            || aap_access(child.process.as_raw_handle())? != ordinary
            || token_capability_count(child.process.as_raw_handle())? != 0
        {
            return Err(io::Error::other("suspended identity / Job mismatch"));
        }
        if !handle_available(child.process.as_raw_handle(), output.as_raw_handle())?
            || handle_available(child.process.as_raw_handle(), sentinel.as_raw_handle())?
        {
            return Err(io::Error::other("suspended handle allowlist mismatch"));
        }
        println!("CHILD_OUTPUT_HANDLE=1 CHILD_SENTINEL_ABSENT=1");
        let mut strict_flags = 0u32;
        // SAFETY: documented strict-handle policy is one DWORD of flags.
        unsafe {
            checked(GetProcessMitigationPolicy(
                child.process.as_raw_handle(),
                ProcessStrictHandleCheckPolicy,
                (&mut strict_flags as *mut u32).cast(),
                size_of_val(&strict_flags),
            ))?;
        }
        println!("CHILD_STRICT_HANDLE_FLAGS={strict_flags}");
        if fault {
            injected = true;
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected pre-resume rejection",
            ));
        }
        // SAFETY: owned primary thread is still suspended; only now resume it.
        unsafe {
            if ResumeThread(child.thread.as_raw_handle()) == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            if WaitForSingleObject(child.process.as_raw_handle(), 15_000) != WAIT_OBJECT_0 {
                return Err(io::Error::other("probe did not exit within 15s"));
            }
            let mut code = 0;
            checked(GetExitCodeProcess(child.process.as_raw_handle(), &mut code))?;
            if code != 0 {
                return Err(io::Error::other(format!("child exit {code:#x}")));
            }
        }
        Ok(())
    })();
    let _ = terminate_job_and_drain(job.as_raw_handle(), Duration::from_secs(5));
    if let Err(error) = child.stop() {
        eprintln!("W1 retained profile: {}", profile.path.display());
        profile.registered = false;
        if let Some(peer) = &mut peer_profile {
            eprintln!("W1b retained peer profile: {}", peer.path.display());
            peer.registered = false;
        }
        if let Some(files) = &mut fixtures {
            files.retain();
        }
        return Err(error);
    }
    drop(child);
    drop(job);
    drop(attributes);
    drop(input);
    drop(output);
    drop(sentinel);
    let text = fs::read_to_string(&output_path)?;
    if result.is_err() && !text.is_empty() {
        eprintln!("W1 child evidence: {text}");
    }
    let files_result = match &fixtures {
        Some(files) if result.is_ok() => files.verify(),
        _ => Ok(()),
    };
    let net_result = match &net_listeners {
        Some(listeners) if result.is_ok() => listeners.verify_zero_child_traffic(),
        _ => Ok(()),
    };
    fs::remove_file(&program)?;
    if let Some(files) = &mut fixtures {
        files.cleanup()?;
    }
    if let Some(peer) = &mut peer_profile {
        peer.cleanup()?;
    }
    fs::remove_file(&output_path)?;
    profile.cleanup()?;
    if fault {
        match result {
            Err(error) if injected && error.kind() == io::ErrorKind::PermissionDenied => (),
            other => {
                return Err(io::Error::other(format!(
                    "expected injected rejection, got {other:?}"
                )));
            }
        }
        if !text.is_empty() {
            return Err(io::Error::other(
                "suspended child unexpectedly wrote output",
            ));
        }
        println!("FAULT_BEFORE_RESUME=1 CHILD_BYTES=0 PROFILE_CLEANED=1");
    } else {
        result?;
        files_result?;
        net_result?;
        let mut expected = format!(
            "CHILD_ENTERED=1\nAPP=1 AAP_ACCESS={} JOB=1 ENV=1",
            u8::from(ordinary)
        );
        if file_probe {
            expected.push('\n');
            expected.push_str(crate::files::CHILD_EVIDENCE);
        }
        if readonly {
            expected.push('\n');
            expected.push_str(DEPENDENCY_EVIDENCE);
        }
        if boundary {
            expected.push('\n');
            expected.push_str(crate::files::BOUNDARY_EVIDENCE);
        }
        if network_probe {
            let trimmed = text.trim();
            let has_header = trimmed.starts_with(&expected);
            let has_net = trimmed.ends_with(crate::network::NETWORK_EVIDENCE);
            if !has_header || !has_net {
                return Err(io::Error::other(format!(
                    "unexpected network child evidence: {text:?}"
                )));
            }
        } else if text.trim() != expected {
            return Err(io::Error::other(format!(
                "unexpected child evidence: {text:?}"
            )));
        }
        println!("HOST_SENTINEL=1 {text}PROFILE_CLEANED=1");
        if file_probe {
            println!(
                "HOST_FILES_UNCHANGED=1 PRIVATE_OUTPUT_VERIFIED=1 FIXTURES_CLEANED=1 PEER_PROFILE_CLEANED=1"
            );
        }
        if readonly {
            println!("DEPENDENCY_UNCHANGED=1 DEPENDENCY_CLEANED=1");
        }
        if network_probe {
            println!("HOST_NET_ZERO_HITS=1");
        }
    }
    Ok(())
}
