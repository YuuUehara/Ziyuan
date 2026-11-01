//! Private FFI boundary for a trusted self-reexecution experiment only.
use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io::{self, Write},
    mem::{size_of, zeroed},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::PathBuf,
    ptr::{null, null_mut},
    time::{SystemTime, UNIX_EPOCH},
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
        Com::CoTaskMemFree, JobObjects::*, Threading::*,
        WindowsProgramming::PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT,
    },
};

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
        let name = wide(format!("Ziyuan.W1a.{}.{}", std::process::id(), stamp));
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
            profile.path = PathBuf::from(std::ffi::OsString::from_wide(
                std::slice::from_raw_parts(folder, len),
            ));
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
                "W1a profile cleanup failed for {}: {error}",
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
            eprintln!("W1a: {error}");
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

fn child_probe(secret: usize, ordinary: bool) -> io::Result<()> {
    // SAFETY: pseudo handle only borrowed, not closed; sentinel is deliberately
    // supplied as an untrusted numeric handle. Zero-time wait cannot block.
    let (process, secret_available) = unsafe {
        let process = GetCurrentProcess();
        SetLastError(0);
        let wait = WaitForSingleObject(secret as HANDLE, 0);
        let denied = wait == WAIT_FAILED && GetLastError() == ERROR_INVALID_HANDLE;
        (process, !denied)
    };
    let app = token_flag(process, TokenIsAppContainer)?;
    let aap = aap_access(process)?;
    let job = in_job(process, null_mut())?;
    let directory = std::env::current_dir()?;
    let environment_ok = std::env::var_os("SystemRoot").is_some_and(|value| !value.is_empty())
        && std::env::var_os("LOCALAPPDATA").is_some_and(|value| {
            // This checks the supplied environment value, not filesystem
            // isolation or reparse-point safety (which remain W1b work).
            PathBuf::from(value).starts_with(&directory)
        })
        && std::env::var_os("ZIYUAN_W1A_HOST_ONLY").is_none();
    writeln!(
        io::stdout(),
        "APP={app} AAP_ACCESS={} JOB={} SECRET={} ENV={}",
        u8::from(aap),
        u8::from(job),
        u8::from(secret_available),
        u8::from(environment_ok)
    )?;
    if app != 1 || aap != ordinary || !job || secret_available || !environment_ok {
        return Err(io::Error::other(
            "child identity / handle / environment check failed",
        ));
    }
    Ok(())
}

pub(super) fn run() -> io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() == 3 && args[0] == "--child" {
        return child_probe(
            args[1].parse().map_err(io::Error::other)?,
            args[2].parse().map_err(io::Error::other)?,
        );
    }
    let fault = args == ["--fail-before-resume"];
    let ordinary = args == ["--ordinary-appcontainer-control"];
    if !args.is_empty() && !fault && !ordinary {
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
    let mut profile = Profile::create()?;
    let program = profile.path.join("w1a-probe.exe");
    std::fs::copy(std::env::current_exe()?, &program)?;
    let output_path = profile.path.join("child-output.txt");
    let output = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&output_path)?;
    let input = File::open("NUL")?;
    // SAFETY: newly created unnamed event and Job transfer unique ownership.
    // No named/shared host resources or host ACLs are changed.
    let (sentinel, job) = unsafe {
        (
            own(CreateEventW(null(), 1, 1, null()))?,
            own(CreateJobObjectW(null(), null()))?,
        )
    };
    // SAFETY: POD Win32 structures permit zero initialization. Job flags omit
    // both breakaway variants. All handles stay owned until the child is done.
    unsafe {
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        checked(SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of_val(&limits) as u32,
        ))?;
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
    // Windows filenames cannot contain quotes. Only this known executable path
    // and integer/boolean controls are serialized; arbitrary args aren't offered.
    let mut command = wide(format!(
        "\"{}\" --child {} {}",
        program.display(),
        sentinel.as_raw_handle() as usize,
        ordinary
    ));
    // AppContainer startup also needs LOCALAPPDATA. It is task-private, never
    // copied from the host. Entries are in Unicode name order; each wide call
    // supplies an entry NUL, then push terminates the whole environment block.
    let root = std::env::var_os("SystemRoot")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            io::Error::other("SystemRoot unavailable; refusing to inherit the host environment")
        })?;
    let mut entry = std::ffi::OsString::from("LOCALAPPDATA=");
    entry.push(&profile.path);
    let mut environment = wide(entry);
    let mut entry = std::ffi::OsString::from("SystemRoot=");
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
    // This executable only runs its own trusted single-process probe. Full
    // descendant termination, cancellation and resource leases belong to W1d.
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
        {
            return Err(io::Error::other("suspended identity / Job mismatch"));
        }
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
    // Confirm this trusted child has stopped before removing its resources.
    // Failure here deliberately retains the profile for diagnosis, not reuse.
    if let Err(error) = child.stop() {
        eprintln!("W1a retained profile: {}", profile.path.display());
        profile.registered = false;
        return Err(error);
    }
    drop(child);
    drop(job);
    drop(attributes);
    drop(input);
    drop(output);
    drop(sentinel);
    let text = std::fs::read_to_string(&output_path)?;
    if result.is_err() && !text.is_empty() {
        // Only our trusted probe's fixed evidence/error output, never an
        // arbitrary user command or a dump of the host environment.
        eprintln!("W1a child evidence: {text}");
    }
    std::fs::remove_file(&program)?;
    std::fs::remove_file(&output_path)?;
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
        let expected = format!(
            "APP=1 AAP_ACCESS={} JOB=1 SECRET=0 ENV=1",
            u8::from(ordinary)
        );
        if text.trim() != expected {
            return Err(io::Error::other(format!(
                "unexpected child evidence: {text:?}"
            )));
        }
        println!("HOST_SENTINEL=1 {text}PROFILE_CLEANED=1");
    }
    Ok(())
}
