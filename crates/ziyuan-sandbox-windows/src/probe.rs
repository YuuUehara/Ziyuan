//! Standalone W1a/W1b/W1c/W1d/W2 verification probe CLI.
//!
//! Separated from [`crate::windows`] so the production Win32 sandbox FFI core
//! remains small and directly auditable without probe harness code.

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    mem::{size_of, zeroed},
    os::windows::io::AsRawHandle,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::*,
    Networking::WinSock::{WSACleanup, WSADATA, WSAEACCES, WSASYSCALLFAILURE, WSAStartup},
    Security::{
        Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT},
        *,
    },
    Storage::FileSystem::*,
    System::{
        JobObjects::AssignProcessToJobObject,
        SystemServices::{SYSTEM_MANDATORY_LABEL_ACE_TYPE, SYSTEM_MANDATORY_LABEL_NO_WRITE_UP},
        Threading::*,
        WindowsProgramming::PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT,
    },
};

use crate::{
    ExitOutcome, LaunchSpec, SandboxMode,
    windows::{
        Attributes, Child, PROFILE_SEQ, Profile, aap_access, admit_dependency, checked,
        create_kill_on_close_job, handle_available, in_job, low_integrity, own,
        run_sandboxed_with_mode, sid_text, terminate_job_and_drain, token_capability_count,
        token_flag, token_information, wide,
    },
};

const DEPENDENCY_EVIDENCE: &str = "DEPENDENCY_IMAGE_ID=1 IMAGE_MISMATCH_REJECTED=1\n\
DEPENDENCY_TOKEN_IL=4096 OBJECT_IL=4096 CONTROL_RIGHTS=1\n\
DEPENDENCY_READ_EXECUTE=1 RIGHTS_DENIED=1 DELETE_RENAME_CREATE_DENIED=1";

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

fn winsock_child_probe(tcp4: &str, udp4: &str, tcp6: &str, ordinary: bool) -> io::Result<()> {
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
    let result = crate::network::child_probe(tcp4, udp4, tcp6, caps, ordinary);
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
        timeout: if mode == "--lifecycle-timeout" {
            Duration::from_millis(900)
        } else {
            Duration::from_secs(15)
        },
        max_output_bytes: None,
    };
    let res = if mode == "--lifecycle-cancel" {
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(700));
                cancel_flag.store(true, Ordering::SeqCst);
            });
            run_sandboxed_with_mode(
                &spec,
                SandboxMode::AppContainerChildCapable,
                Some(&cancel_flag),
            )
        })?
    } else {
        run_sandboxed_with_mode(&spec, SandboxMode::AppContainerChildCapable, None)?
    };
    let _ = fs::remove_dir_all(&stage);
    let text = String::from_utf8_lossy(&res.output);
    if !text.contains("GRANDCHILD_SPAWNED=1") {
        return Err(io::Error::other(format!(
            "grandchild marker missing in lifecycle output: {text:?}"
        )));
    }
    if res.active_processes_after != 0 {
        return Err(io::Error::other(format!(
            "expected 0 active processes after Job drain, got {}",
            res.active_processes_after
        )));
    }
    if !res.cleanup_warnings.is_empty() {
        return Err(io::Error::other(format!(
            "unexpected cleanup warnings: {:?}",
            res.cleanup_warnings
        )));
    }
    match (mode, res.outcome) {
        ("--lifecycle-timeout", ExitOutcome::TimedOut) => {
            println!(
                "LIFECYCLE_TIMEOUT=1 GRANDCHILD_SPAWNED=1 JOB_ACTIVE_AFTER=0 PROFILE_CLEANED=1 ELAPSED_MS={}",
                started.elapsed().as_millis()
            );
        }
        ("--lifecycle-cancel", ExitOutcome::Cancelled) => {
            println!(
                "LIFECYCLE_CANCEL=1 GRANDCHILD_SPAWNED=1 JOB_ACTIVE_AFTER=0 PROFILE_CLEANED=1 ELAPSED_MS={}",
                started.elapsed().as_millis()
            );
        }
        ("--lifecycle-orphan", ExitOutcome::Exited(0)) => {
            println!(
                "LIFECYCLE_ORPHAN=1 GRANDCHILD_SPAWNED=1 JOB_ACTIVE_AFTER=0 PROFILE_CLEANED=1 ELAPSED_MS={}",
                started.elapsed().as_millis()
            );
        }
        (m, outcome) => {
            return Err(io::Error::other(format!(
                "unexpected outcome {outcome:?} for {m}; output={text:?}"
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
    if args.len() == 1 && args[0] == "--shell-child-check" {
        // SAFETY: borrowed pseudo handle, never closed.
        let process = unsafe { GetCurrentProcess() };
        let app = token_flag(process, TokenIsAppContainer)?;
        let aap = aap_access(process)?;
        let job = in_job(process, null_mut())?;
        let caps = token_capability_count(process)?;
        let no_host_secret = std::env::var_os("ZIYUAN_W1A_HOST_ONLY").is_none();
        let has_extra = std::env::var("ZIYUAN_W2_TEST").ok().as_deref() == Some("ok");
        let profile_root = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .and_then(|p| p.parent().map(Path::to_path_buf));
        let private_dirs_ok = profile_root.as_ref().is_some_and(|base| {
            let base_lower = base.to_string_lossy().to_ascii_lowercase();
            base_lower.contains("\\packages\\ziyuan.w1.")
                && [
                    "LOCALAPPDATA",
                    "USERPROFILE",
                    "HOME",
                    "APPDATA",
                    "TEMP",
                    "TMP",
                ]
                .into_iter()
                .all(|key| {
                    std::env::var_os(key).is_some_and(|v| {
                        v.to_string_lossy()
                            .to_ascii_lowercase()
                            .starts_with(&base_lower)
                    })
                })
        });
        println!(
            "SHELL_CHILD_OK=1 APP={app} AAP_ACCESS={} JOB={} CAPS={caps} CLEAN_ENV={} EXTRA_ENV={} PRIVATE_ENV={}",
            u8::from(aap),
            u8::from(job),
            u8::from(no_host_secret),
            u8::from(has_extra),
            u8::from(private_dirs_ok),
        );
        if app != 1
            || !aap
            || !job
            || caps != 0
            || !no_host_secret
            || !has_extra
            || !private_dirs_ok
        {
            return Err(io::Error::other("shell child sandbox invariant failed"));
        }
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
    if args.len() == 6
        && args[0] == "--child"
        && matches!(args[1].as_str(), "false" | "true")
        && args[2] == "--network-probe"
    {
        let ordinary: bool = args[1].parse().map_err(io::Error::other)?;
        child_probe(ordinary)?;
        winsock_child_probe(&args[3], &args[4], &args[5], ordinary)?;
        return Ok(());
    }
    if args.len() == 5
        && args[0] == "--child"
        && matches!(args[1].as_str(), "false" | "true")
        && matches!(
            args[2].as_str(),
            "--file-probe" | "--readonly-dependency" | "--boundary-probe"
        )
    {
        let ordinary: bool = args[1].parse().map_err(io::Error::other)?;
        child_probe(ordinary)?;
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
    let ordinary_file = args == ["--ordinary-file-probe"];
    let ordinary_net = args == ["--ordinary-network-probe"];
    let ordinary = args == ["--ordinary-appcontainer-control"] || ordinary_file || ordinary_net;
    let readonly = args == ["--readonly-dependency"];
    let boundary = args == ["--boundary-probe"];
    let network_probe = args == ["--network-probe"] || ordinary_net;
    let file_probe = args == ["--file-probe"] || ordinary_file || readonly || boundary;
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
