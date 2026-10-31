#[path = "support/temp_lease_probe.rs"]
mod support;

use std::fs::OpenOptions;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

fn assert_live_holder_is_busy(child: &support::ChildGuard) {
    let observation = support::probe_lock(
        support::open_probe(&child.root().join("held.lock"))
            .expect("open independent probe handle"),
    )
    .expect("live holder probe IO error");
    assert!(
        matches!(observation, support::ProbeLock::Busy),
        "live child must retain its lock through READY: {observation:?}"
    );
}

#[test]
fn cross_process_lock_blocks_live_holder_and_releases_after_kill() {
    let mut child = support::ChildGuard::spawn("lock-holder").unwrap();
    let _stream = child.ready("locked", support::DEFAULT_TIMEOUT).unwrap();
    assert_live_holder_is_busy(&child);
    child.kill().unwrap();
    assert!(!child.reap(support::DEFAULT_TIMEOUT).unwrap().success());
    let acquired =
        support::acquire_before(&child.root().join("held.lock"), support::DEFAULT_TIMEOUT).unwrap();
    drop(acquired);
}

#[test]
fn cross_process_lock_releases_after_continue() {
    let mut child = support::ChildGuard::spawn("lock-holder").unwrap();
    let mut stream = child.ready("locked", support::DEFAULT_TIMEOUT).unwrap();
    assert_live_holder_is_busy(&child);
    support::send_message(
        &mut stream,
        "CONTINUE",
        Instant::now() + support::DEFAULT_TIMEOUT,
        "parent lock release CONTINUE",
    )
    .unwrap();
    child.finish(support::DEFAULT_TIMEOUT).unwrap();
    let acquired =
        support::acquire_before(&child.root().join("held.lock"), support::DEFAULT_TIMEOUT).unwrap();
    drop(acquired);
}

#[test]
fn same_process_independent_handles_are_observed() {
    // The guard owns scratch and is declared first so all handles close before
    // child teardown/scratch cleanup, including during assertion unwinding.
    let mut child = support::ChildGuard::spawn("handshake").unwrap();
    let mut stream = child.ready("connected", support::DEFAULT_TIMEOUT).unwrap();
    let path = child.root().join("same-process.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let holder = match support::probe_lock(file).unwrap() {
        support::ProbeLock::Acquired(file) => file,
        support::ProbeLock::Busy => panic!("new same-process lock unexpectedly busy"),
    };
    eprintln!("same-process independent OpenOptions handle:");
    let independent = support::probe_lock(support::open_probe(&path).unwrap()).unwrap();
    eprintln!("same-process try_clone handle:");
    let cloned = support::probe_lock(holder.try_clone().unwrap()).unwrap();
    // Either Acquired or Busy is a valid native observation, not evidence of
    // staleness. Keep acquired handles alive; never explicitly unlock a clone:
    // it may share the original lock. Make no lock claims while dropping these.
    drop(cloned);
    drop(independent);
    drop(holder);
    let reacquired = support::acquire_before(&path, support::DEFAULT_TIMEOUT).unwrap();
    drop(reacquired);
    support::send_message(
        &mut stream,
        "CONTINUE",
        Instant::now() + support::DEFAULT_TIMEOUT,
        "parent same-process CONTINUE",
    )
    .unwrap();
    child.finish(support::DEFAULT_TIMEOUT).unwrap();
}

#[test]
fn probe_child_entry() {
    if std::env::var_os("ZIYUAN_TEMP_LEASE_PROBE").is_none() {
        return;
    }
    support::run_child().expect("temp lease child probe failed");
}

#[test]
fn probe_child_handshake_has_deadline() {
    let result = support::handshake(support::DEFAULT_TIMEOUT);
    assert!(result.is_ok(), "bounded child handshake failed: {result:?}");
}

#[test]
fn probe_accept_has_deadline() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let started = Instant::now();
    let error = support::accept_ready(&listener, "unused", "connected", Duration::from_millis(100))
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() < support::DEFAULT_TIMEOUT);
}

#[test]
fn probe_ready_read_has_deadline() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    // Connect but send no newline or data, so accept succeeds and READY stalls.
    let _peer =
        TcpStream::connect_timeout(&listener.local_addr().unwrap(), support::DEFAULT_TIMEOUT)
            .unwrap();
    let started = Instant::now();
    let error = support::accept_ready(&listener, "unused", "connected", Duration::from_millis(100))
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("parent READY"));
    assert!(started.elapsed() >= Duration::from_millis(50));
    assert!(started.elapsed() < support::DEFAULT_TIMEOUT);
}

#[test]
fn probe_wrong_handshake_id_is_rejected() {
    let mut child = support::ChildGuard::spawn("wrong-id").unwrap();
    let error = child
        .ready("connected", support::DEFAULT_TIMEOUT)
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("parent READY"));
}

#[test]
fn probe_withheld_continue_times_out() {
    let started = Instant::now();
    let mut child = support::ChildGuard::spawn("continue-timeout").unwrap();
    let _stream = child.ready("connected", support::DEFAULT_TIMEOUT).unwrap();
    // The child succeeds only if its read hits the explicit short deadline.
    child.finish(support::DEFAULT_TIMEOUT).unwrap();
    assert!(started.elapsed() >= Duration::from_millis(100));
    assert!(started.elapsed() < support::DEFAULT_TIMEOUT);
}

#[test]
fn probe_child_exit_wait_is_bounded() {
    let mut child = support::ChildGuard::spawn("handshake").unwrap();
    let _stream = child.ready("connected", support::DEFAULT_TIMEOUT).unwrap();
    let started = Instant::now();
    let error = child.reap(Duration::from_millis(100)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(started.elapsed() < support::DEFAULT_TIMEOUT);
    child.kill().unwrap();
    assert!(!child.reap(support::DEFAULT_TIMEOUT).unwrap().success());
}

#[test]
fn probe_panic_drops_child_before_scratch() {
    let mut child = support::ChildGuard::spawn("handshake").unwrap();
    let root = child.root().to_path_buf();
    let _stream = child.ready("connected", support::DEFAULT_TIMEOUT).unwrap();
    assert!(root.is_dir());
    let unwind = std::panic::catch_unwind(move || {
        let _child = child;
        panic!("exercise child guard unwind");
    });
    assert!(unwind.is_err());
    assert!(!root.exists());
}

#[test]
fn probe_invalid_case_does_not_mutate_scratch() {
    let mut child = support::ChildGuard::spawn("invalid-case").unwrap();
    assert!(child.finish(support::DEFAULT_TIMEOUT).is_err());
    assert_eq!(std::fs::read_dir(child.root()).unwrap().count(), 0);
}
