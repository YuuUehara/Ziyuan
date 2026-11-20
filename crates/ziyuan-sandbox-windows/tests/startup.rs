#![cfg(windows)]

use std::{
    fs, io,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use ziyuan_sandbox_windows::{
    ExitOutcome, LaunchSpec, SandboxCapability, SandboxMode, ShellSpec, probe_capability,
    run_sandboxed, run_sandboxed_with_mode, run_shell_sandboxed,
};

static TEST_SEQ: AtomicU64 = AtomicU64::new(1);

fn unique_temp_dir(label: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "ziyuan-w1-test-{label}-{}-{seq}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&dir).unwrap();
    dir
}

#[test]
fn startup_distinguishes_aap_policy_and_excludes_unselected_handles() {
    for (argument, expected) in [
        (
            Some("--ordinary-appcontainer-control"),
            "APP=1 AAP_ACCESS=1 JOB=1 ENV=1",
        ),
        (None, "APP=1 AAP_ACCESS=0 JOB=1 ENV=1"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_ziyuan-sandbox-windows"))
            .args(argument)
            .env("ZIYUAN_W1A_HOST_ONLY", "synthetic-host-secret")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        println!("{stdout}{stderr}");
        assert!(output.status.success(), "{stdout}{stderr}");
        assert!(stdout.contains("HOST_AAP_ACCESS=1"));
        assert!(stdout.contains("CHILD_OUTPUT_HANDLE=1 CHILD_SENTINEL_ABSENT=1"));
        assert!(stdout.contains("CHILD_ENTERED=1"));
        assert!(stdout.contains(expected), "{stdout}{stderr}");
        assert!(stdout.contains("PROFILE_CLEANED=1"));
    }
}

#[test]
fn rejected_startup_never_runs_the_suspended_child() {
    let output = Command::new(env!("CARGO_BIN_EXE_ziyuan-sandbox-windows"))
        .arg("--fail-before-resume")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("{stdout}{stderr}");
    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("CHILD_OUTPUT_HANDLE=1 CHILD_SENTINEL_ABSENT=1"));
    assert!(stdout.contains("FAULT_BEFORE_RESUME=1 CHILD_BYTES=0 PROFILE_CLEANED=1"));
    assert!(!stdout.contains("CHILD_ENTERED=1"));
    assert!(!stdout.contains("APP=1"));
}

#[test]
fn private_files_work_while_host_and_peer_files_are_access_denied() {
    for argument in ["--file-probe", "--readonly-dependency", "--boundary-probe"] {
        let output = Command::new(env!("CARGO_BIN_EXE_ziyuan-sandbox-windows"))
            .arg(argument)
            .env("ZIYUAN_W1A_HOST_ONLY", "synthetic-host-secret")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        println!("{stdout}{stderr}");
        assert!(output.status.success(), "{stdout}{stderr}");
        assert!(stdout.contains("APP=1 AAP_ACCESS=0 JOB=1 ENV=1"));
        assert!(stdout.contains("HOST_FILES_READ_WRITE=1"));
        assert!(stdout.contains("FILE_PRIVATE_READ=1 FILE_PRIVATE_WRITE=1 FILE_TEMP_WRITE=1"));
        for label in ["source", "journal", "credential", "vcs", "peer"] {
            assert!(
                stdout.contains(&format!("FILE_DENIED={label} READ_WIN32=5 WRITE_WIN32=5")),
                "{stdout}{stderr}"
            );
        }
        assert!(stdout.contains("HOST_FILES_UNCHANGED=1 PRIVATE_OUTPUT_VERIFIED=1"));
        assert!(stdout.contains("FIXTURES_CLEANED=1 PEER_PROFILE_CLEANED=1"));
        assert!(stdout.contains("PROFILE_CLEANED=1"));
        if argument == "--readonly-dependency" {
            assert!(stdout.contains("HOST_DEPENDENCY_RIGHTS=1"));
            assert!(stdout.contains("DEPENDENCY_IMAGE_ID=1 IMAGE_MISMATCH_REJECTED=1"));
            assert!(stdout.contains("DEPENDENCY_TOKEN_IL=4096 OBJECT_IL=4096 CONTROL_RIGHTS=1"));
            assert!(stdout.contains(
                "DEPENDENCY_READ_EXECUTE=1 RIGHTS_DENIED=1 DELETE_RENAME_CREATE_DENIED=1"
            ));
            assert!(stdout.contains("DEPENDENCY_UNCHANGED=1 DEPENDENCY_CLEANED=1"));
        }
        if argument == "--boundary-probe" {
            assert!(
                stdout.contains(
                    "BOUNDARY_CASE_DENIED=1 BOUNDARY_UNICODE_RW=1 BOUNDARY_LONG_PATH_RW=1"
                ),
                "{stdout}{stderr}"
            );
        }
    }
}

#[test]
fn lpac_blocks_tcp4_udp4_and_ipv6_loopback_with_zero_host_hits() {
    let output = Command::new(env!("CARGO_BIN_EXE_ziyuan-sandbox-windows"))
        .arg("--network-probe")
        .env("ZIYUAN_W1A_HOST_ONLY", "synthetic-host-secret")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("{stdout}{stderr}");
    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("HOST_NET_CONTROLS=1"));
    assert!(stdout.contains("APP=1 AAP_ACCESS=0 JOB=1 ENV=1"));
    assert!(stdout.contains("NET_CAPS=0 NET_TCP4_DENIED=1 NET_UDP4_DENIED=1 NET_IPV6_CHECKED=1"));
    assert!(stdout.contains("HOST_NET_ZERO_HITS=1"));
    assert!(stdout.contains("PROFILE_CLEANED=1"));
}

#[test]
fn lifecycle_timeout_cancel_and_orphan_grandchild_all_terminate_cleanly() {
    for (flag, expected_marker) in [
        ("--lifecycle-timeout", "LIFECYCLE_TIMEOUT=1"),
        ("--lifecycle-cancel", "LIFECYCLE_CANCEL=1"),
        ("--lifecycle-orphan", "LIFECYCLE_ORPHAN=1"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_ziyuan-sandbox-windows"))
            .arg(flag)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        println!("{stdout}{stderr}");
        assert!(output.status.success(), "{stdout}{stderr}");
        assert!(stdout.contains(expected_marker), "{stdout}{stderr}");
        assert!(stdout.contains("GRANDCHILD_SPAWNED=1"));
        assert!(stdout.contains("JOB_ACTIVE_AFTER=0"));
        assert!(stdout.contains("PROFILE_CLEANED=1"));
    }
}

#[test]
fn safe_api_rejects_reparse_points_and_reserved_env_overrides() {
    let root = unique_temp_dir("reparse");
    let target = root.join("real_target");
    let junction = root.join("junction_link");
    fs::create_dir(&target).unwrap();
    let probe_bin = target.join("probe.exe");
    fs::copy(env!("CARGO_BIN_EXE_ziyuan-sandbox-windows"), &probe_bin).unwrap();

    // Create an NTFS directory junction without requiring administrator privileges.
    let status = Command::new("cmd")
        .args([
            "/C",
            "mklink",
            "/J",
            &junction.to_string_lossy(),
            &target.to_string_lossy(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J failed");

    let spec = LaunchSpec {
        program: junction.join("probe.exe"),
        args: vec!["--child".into(), "false".into()],
        cwd: None,
        writable_dirs: Vec::new(),
        readonly_dirs: vec![junction.clone()],
        env: Vec::new(),
        timeout: Duration::from_secs(5),
    };
    let err = run_sandboxed(&spec, None).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "got: {err}");
    assert!(
        err.to_string().contains("reparse point"),
        "unexpected error: {err}"
    );

    // Reserved environment override must also fail closed.
    let bad_env_spec = LaunchSpec {
        program: probe_bin,
        args: vec!["--child".into(), "false".into()],
        cwd: None,
        writable_dirs: Vec::new(),
        readonly_dirs: vec![target.clone()],
        env: vec![("SystemRoot".into(), "C:\\Bogus".into())],
        timeout: Duration::from_secs(5),
    };
    let env_err = run_sandboxed(&bad_env_spec, None).unwrap_err();
    assert_eq!(
        env_err.kind(),
        io::ErrorKind::PermissionDenied,
        "got: {env_err}"
    );

    // Remove the junction before removing the parent directory.
    let _ = fs::remove_dir(&junction);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn safe_api_probes_capability_and_runs_parallel_isolated_tasks() {
    assert_eq!(
        probe_capability(),
        SandboxCapability::FileSystemAndNetworkIsolated
    );

    let bin = env!("CARGO_BIN_EXE_ziyuan-sandbox-windows");
    let handles: Vec<_> = (0..3)
        .map(|idx| {
            thread::spawn(move || {
                let stage = unique_temp_dir(&format!("parallel-{idx}"));
                let exe = stage.join("probe.exe");
                fs::copy(bin, &exe).unwrap();
                let spec = LaunchSpec {
                    program: exe,
                    args: vec!["--child".into(), "false".into()],
                    cwd: None,
                    writable_dirs: Vec::new(),
                    readonly_dirs: vec![stage.clone()],
                    env: vec![("ZIYUAN_TASK_SLOT".into(), idx.to_string())],
                    timeout: Duration::from_secs(10),
                };
                let result = run_sandboxed(&spec, None).unwrap();
                let _ = fs::remove_dir_all(&stage);
                result
            })
        })
        .collect();

    for handle in handles {
        let res = handle.join().unwrap();
        let text = String::from_utf8_lossy(&res.output);
        assert_eq!(res.outcome, ExitOutcome::Exited(0), "output: {text}");
        assert_eq!(res.active_processes_after, 0);
        assert!(
            res.cleanup_warnings.is_empty(),
            "warnings: {:?}",
            res.cleanup_warnings
        );
        assert!(text.contains("CHILD_ENTERED=1"), "{text}");
        assert!(text.contains("APP=1 AAP_ACCESS=0 JOB=1 ENV=1"), "{text}");
    }
}

#[test]
fn strict_lpac_safe_api_enforces_timeout_and_cancellation() {
    let bin = env!("CARGO_BIN_EXE_ziyuan-sandbox-windows");
    let stage = unique_temp_dir("lpac-lifecycle");
    let exe = stage.join("probe.exe");
    fs::copy(bin, &exe).unwrap();

    // 1. Strict LPAC wall-clock timeout
    let timeout_spec = LaunchSpec {
        program: exe.clone(),
        args: vec!["--child".into(), "false".into(), "--lpac-sleep".into()],
        cwd: None,
        writable_dirs: Vec::new(),
        readonly_dirs: vec![stage.clone()],
        env: Vec::new(),
        timeout: Duration::from_millis(800),
    };
    let res = run_sandboxed(&timeout_spec, None).unwrap();
    let text = String::from_utf8_lossy(&res.output);
    assert_eq!(res.outcome, ExitOutcome::TimedOut, "output: {text}");
    assert_eq!(res.active_processes_after, 0);
    assert!(
        res.cleanup_warnings.is_empty(),
        "{:?}",
        res.cleanup_warnings
    );
    assert!(text.contains("APP=1 AAP_ACCESS=0 JOB=1 ENV=1"), "{text}");
    assert!(text.contains("LPAC_SLEEP_ENTERED=1"), "{text}");

    // 2. Strict LPAC caller cancellation
    let cancel = AtomicBool::new(false);
    let cancel_spec = LaunchSpec {
        program: exe,
        args: vec!["--child".into(), "false".into(), "--lpac-sleep".into()],
        cwd: None,
        writable_dirs: Vec::new(),
        readonly_dirs: vec![stage.clone()],
        env: Vec::new(),
        timeout: Duration::from_secs(15),
    };
    let res = thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(Duration::from_millis(600));
            cancel.store(true, Ordering::SeqCst);
        });
        run_sandboxed(&cancel_spec, Some(&cancel)).unwrap()
    });
    let text = String::from_utf8_lossy(&res.output);
    assert_eq!(res.outcome, ExitOutcome::Cancelled, "output: {text}");
    assert_eq!(res.active_processes_after, 0);
    assert!(
        res.cleanup_warnings.is_empty(),
        "{:?}",
        res.cleanup_warnings
    );
    assert!(text.contains("APP=1 AAP_ACCESS=0 JOB=1 ENV=1"), "{text}");
    assert!(text.contains("LPAC_SLEEP_ENTERED=1"), "{text}");

    let _ = fs::remove_dir_all(&stage);
}

#[test]
fn w2_appcontainer_child_capable_preserves_file_and_network_isolation() {
    // 1. Verify W1b file-isolation matrix under 0-capability AppContainer (AAP_ACCESS=1, CAPS=0).
    let file_out = Command::new(env!("CARGO_BIN_EXE_ziyuan-sandbox-windows"))
        .arg("--ordinary-file-probe")
        .env("ZIYUAN_W1A_HOST_ONLY", "synthetic-host-secret")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&file_out.stdout);
    let stderr = String::from_utf8_lossy(&file_out.stderr);
    println!("[w2-ordinary-file]\n{stdout}{stderr}");
    assert!(file_out.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("APP=1 AAP_ACCESS=1 JOB=1 ENV=1"));
    assert!(stdout.contains("FILE_PRIVATE_READ=1 FILE_PRIVATE_WRITE=1 FILE_TEMP_WRITE=1"));
    for label in ["source", "journal", "credential", "vcs", "peer"] {
        assert!(
            stdout.contains(&format!("FILE_DENIED={label} READ_WIN32=5 WRITE_WIN32=5")),
            "{stdout}{stderr}"
        );
    }
    assert!(stdout.contains("HOST_FILES_UNCHANGED=1 PRIVATE_OUTPUT_VERIFIED=1"));
    assert!(stdout.contains("PROFILE_CLEANED=1"));

    // 2. Verify W1c network-isolation matrix under 0-capability AppContainer (AAP_ACCESS=1, CAPS=0).
    let net_out = Command::new(env!("CARGO_BIN_EXE_ziyuan-sandbox-windows"))
        .arg("--ordinary-network-probe")
        .env("ZIYUAN_W1A_HOST_ONLY", "synthetic-host-secret")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&net_out.stdout);
    let stderr = String::from_utf8_lossy(&net_out.stderr);
    println!("[w2-ordinary-net]\n{stdout}{stderr}");
    assert!(net_out.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("HOST_NET_CONTROLS=1"));
    assert!(stdout.contains("APP=1 AAP_ACCESS=1 JOB=1 ENV=1"));
    assert!(stdout.contains("NET_CAPS=0 NET_TCP4_DENIED=1 NET_UDP4_DENIED=1 NET_IPV6_CHECKED=1"));
    assert!(stdout.contains("HOST_NET_ZERO_HITS=1"));
    assert!(stdout.contains("PROFILE_CLEANED=1"));
}

#[test]
fn w2_run_shell_sandboxed_spawns_children_isolates_host_and_drains_on_timeout_and_cancel() {
    let stage = unique_temp_dir("w2-shell");
    let work = stage.join("work");
    let host_secret_dir = stage.join("host_secret");
    let ro_tools = stage.join("ro_tools");
    fs::create_dir(&work).unwrap();
    fs::create_dir(&host_secret_dir).unwrap();
    fs::create_dir(&ro_tools).unwrap();

    let host_secret = host_secret_dir.join("secret.txt");
    fs::write(&host_secret, "HOST_TOP_SECRET_VALUE").unwrap();
    let probe_exe = ro_tools.join("probe.exe");
    fs::copy(env!("CARGO_BIN_EXE_ziyuan-sandbox-windows"), &probe_exe).unwrap();

    // 1. cmd.exe /D /S /C can write to its admitted copy, spawn a real child process,
    //    preserve quoted arguments, and is denied reading or writing host_secret.
    let cmd = format!(
        "echo W2_CMD_OK & echo hello_copy > work_out.txt & \"{}\" --shell-child-check & type \"{}\" 2>&1 & echo BAD > \"{}\" 2>&1 & exit /b 0",
        probe_exe.display(),
        host_secret.display(),
        host_secret.display()
    );
    let res = run_shell_sandboxed(
        &ShellSpec {
            command: cmd,
            cwd: work.clone(),
            writable_dirs: vec![work.clone()],
            readonly_dirs: vec![ro_tools.clone()],
            env: vec![("ZIYUAN_W2_TEST".into(), "ok".into())],
            timeout: Duration::from_secs(15),
        },
        None,
    )
    .unwrap();
    let out = String::from_utf8_lossy(&res.output);
    println!("[w2-shell-ok]\n{out}");
    assert_eq!(res.outcome, ExitOutcome::Exited(0), "{out}");
    assert_eq!(res.active_processes_after, 0);
    assert!(
        res.cleanup_warnings.is_empty(),
        "{:?}",
        res.cleanup_warnings
    );
    assert!(out.contains("W2_CMD_OK"), "{out}");
    assert!(
        out.contains("SHELL_CHILD_OK=1 APP=1 AAP_ACCESS=1 JOB=1 CAPS=0 CLEAN_ENV=1 EXTRA_ENV=1"),
        "{out}"
    );
    assert!(
        !out.contains("HOST_TOP_SECRET_VALUE"),
        "host secret leaked: {out}"
    );
    assert_eq!(
        fs::read_to_string(&host_secret).unwrap(),
        "HOST_TOP_SECRET_VALUE",
        "host secret was modified"
    );
    assert!(
        fs::read_to_string(work.join("work_out.txt"))
            .unwrap()
            .contains("hello_copy")
    );

    // 2. Shell command spawning a long-sleeping child terminates cleanly on timeout.
    let sleep_cmd = format!("\"{}\" --grandchild-sleep", probe_exe.display());
    let timeout_res = run_shell_sandboxed(
        &ShellSpec {
            command: sleep_cmd.clone(),
            cwd: work.clone(),
            writable_dirs: vec![work.clone()],
            readonly_dirs: vec![ro_tools.clone()],
            env: Vec::new(),
            timeout: Duration::from_millis(800),
        },
        None,
    )
    .unwrap();
    assert_eq!(timeout_res.outcome, ExitOutcome::TimedOut);
    assert_eq!(timeout_res.active_processes_after, 0);
    assert!(
        timeout_res.cleanup_warnings.is_empty(),
        "{:?}",
        timeout_res.cleanup_warnings
    );

    // 3. Shell command spawning a long-sleeping child terminates cleanly on caller cancellation.
    let cancel = AtomicBool::new(false);
    let cancel_res = thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(Duration::from_millis(500));
            cancel.store(true, Ordering::SeqCst);
        });
        run_shell_sandboxed(
            &ShellSpec {
                command: sleep_cmd,
                cwd: work.clone(),
                writable_dirs: vec![work.clone()],
                readonly_dirs: vec![ro_tools.clone()],
                env: Vec::new(),
                timeout: Duration::from_secs(15),
            },
            Some(&cancel),
        )
        .unwrap()
    });
    assert_eq!(cancel_res.outcome, ExitOutcome::Cancelled);
    assert_eq!(cancel_res.active_processes_after, 0);
    assert!(
        cancel_res.cleanup_warnings.is_empty(),
        "{:?}",
        cancel_res.cleanup_warnings
    );

    // 4. Reject empty/newline shell commands and verify explicit mode API.
    let bad = run_shell_sandboxed(
        &ShellSpec {
            command: "echo a\necho b".into(),
            cwd: work.clone(),
            writable_dirs: vec![work.clone()],
            readonly_dirs: Vec::new(),
            env: Vec::new(),
            timeout: Duration::from_secs(5),
        },
        None,
    )
    .unwrap_err();
    assert_eq!(bad.kind(), io::ErrorKind::InvalidInput);

    let mode_res = run_sandboxed_with_mode(
        &LaunchSpec {
            program: probe_exe,
            args: vec!["--child".into(), "true".into()],
            cwd: None,
            writable_dirs: Vec::new(),
            readonly_dirs: vec![ro_tools.clone()],
            env: Vec::new(),
            timeout: Duration::from_secs(10),
        },
        SandboxMode::AppContainerChildCapable,
        None,
    )
    .unwrap();
    assert_eq!(mode_res.outcome, ExitOutcome::Exited(0));
    assert_eq!(mode_res.active_processes_after, 0);

    let _ = fs::remove_dir_all(&stage);
}
