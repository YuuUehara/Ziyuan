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
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    time::{SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::*,
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
            // This checks the supplied environment value, not filesystem
            // isolation or reparse-point safety (which remain W1b work).
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
    // setting call ends. Its inheritable ACEs also protect the two owned files.
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

const DEPENDENCY_EVIDENCE: &str = "DEPENDENCY_TOKEN_IL=4096 OBJECT_IL=4096 CONTROL_RIGHTS=1\n\
DEPENDENCY_READ_EXECUTE=1 RIGHTS_DENIED=1 DELETE_RENAME_CREATE_DENIED=1";

fn dependency_child(host: &Path) -> io::Result<()> {
    let root = crate::files::dependency_path(host);
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
        ("delete", std::fs::remove_file(root.join("data.txt"))),
        (
            "rename",
            std::fs::rename(root.join("data.txt"), root.join("replaced.txt")),
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

pub(super) fn run() -> io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() == 2 && args[0] == "--child" {
        return child_probe(args[1].parse().map_err(io::Error::other)?);
    }
    if args.len() == 5
        && args[..2] == ["--child", "false"]
        && matches!(args[2].as_str(), "--file-probe" | "--readonly-dependency")
    {
        child_probe(false)?;
        crate::files::child(args[3].as_ref(), args[4].as_ref())?;
        if args[2] == "--readonly-dependency" {
            dependency_child(args[3].as_ref())?;
        }
        return Ok(());
    }
    let fault = args == ["--fail-before-resume"];
    let ordinary = args == ["--ordinary-appcontainer-control"];
    let readonly = args == ["--readonly-dependency"];
    let file_probe = args == ["--file-probe"] || readonly;
    if !args.is_empty() && !fault && !ordinary && !file_probe {
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
        std::fs::copy(std::env::current_exe()?, &program)?;
        program
    };
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
        } else {
            "--file-probe"
        };
        command.push_str(&format!(
            " {mode} \"{}\" \"{}\"",
            files.host_path().display(),
            files.peer_path().display()
        ));
    }
    let mut command = wide(command);
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
        // Check the child's table from the parent: deliberately referencing an
        // absent handle inside a strict-checking child may raise an exception.
        // A valid inherited output handle is the same-API positive control.
        if !handle_available(child.process.as_raw_handle(), output.as_raw_handle())?
            || handle_available(child.process.as_raw_handle(), sentinel.as_raw_handle())?
        {
            return Err(io::Error::other("suspended handle allowlist mismatch"));
        }
        println!("CHILD_OUTPUT_HANDLE=1 CHILD_SENTINEL_ABSENT=1");
        // SAFETY: documented strict-handle policy is one DWORD of flags. The
        // aligned output lives through this read-only query; no policy is changed.
        let mut strict_flags = 0u32;
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
    // Confirm this trusted child has stopped before removing its resources.
    // Failure here deliberately retains the profile for diagnosis, not reuse.
    if let Err(error) = child.stop() {
        eprintln!("W1a retained profile: {}", profile.path.display());
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
    let text = std::fs::read_to_string(&output_path)?;
    if result.is_err() && !text.is_empty() {
        // Only our trusted probe's fixed evidence/error output, never an
        // arbitrary user command or a dump of the host environment.
        eprintln!("W1a child evidence: {text}");
    }
    let files_result = match &fixtures {
        Some(files) if result.is_ok() => files.verify(),
        _ => Ok(()),
    };
    std::fs::remove_file(&program)?;
    if let Some(files) = &mut fixtures {
        files.cleanup()?;
    }
    if let Some(peer) = &mut peer_profile {
        peer.cleanup()?;
    }
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
        files_result?;
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
        if text.trim() != expected {
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
    }
    Ok(())
}
