#![cfg(windows)]

use std::process::Command;

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
    for argument in ["--file-probe", "--readonly-dependency"] {
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
    }
}
