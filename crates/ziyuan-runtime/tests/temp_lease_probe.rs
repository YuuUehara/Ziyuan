#[path = "support/temp_lease_probe.rs"]
mod support;

use std::io;
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

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
