#![cfg(windows)]

use std::process::Command;

#[test]
fn startup_is_lpac_in_our_job_with_only_selected_handles() {
    let output = Command::new(env!("CARGO_BIN_EXE_ziyuan-sandbox-windows"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("{stdout}{stderr}");
    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("APP=1 LPAC=1 JOB=1 SECRET=0"));
    assert!(stdout.contains("PROFILE_CLEANED=1"));
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
    assert!(stdout.contains("FAULT_BEFORE_RESUME=1 CHILD_BYTES=0 PROFILE_CLEANED=1"));
    assert!(!stdout.contains("APP=1"));
}
