#[path = "support/temp_lease_probe.rs"]
mod support;

use std::fs::OpenOptions;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

fn continue_and_finish(child: &mut support::ChildGuard, stream: &mut TcpStream) {
    support::send_message(
        stream,
        "CONTINUE",
        Instant::now() + support::DEFAULT_TIMEOUT,
        "parent experiment CONTINUE",
    )
    .unwrap();
    child.finish(support::DEFAULT_TIMEOUT).unwrap();
}

#[derive(Clone, Copy, Debug)]
enum LiveOperation {
    Rename,
    Delete,
}

fn observe_live_operation(operation: LiveOperation) {
    // Two genuinely different Windows share modes: narrowed (read|write, no
    // delete) and std's default (read|write|delete). Other platforms have no
    // share-mode dimension, so they run the plain case once.
    let mut cases = vec![
        "live-restricted-share-lock-first",
        "live-restricted-share-prewritten",
    ];
    #[cfg(windows)]
    cases.extend([
        "live-share-delete-lock-first",
        "live-share-delete-prewritten",
    ]);
    let cases = cases;
    let mut attempted_operations = 0;
    let mut refused_writes = 0;
    for case in &cases {
        for existing_target in [false, true] {
            let scenario = if existing_target {
                "existing-old"
            } else {
                "absent"
            };
            let mut child = support::ChildGuard::spawn(case).unwrap();
            let source = child.root().join("temp");
            let target = child.root().join("target");
            if existing_target {
                std::fs::write(&target, b"old").unwrap();
            }
            let mut stream = child
                .ready("live-handle", support::DEFAULT_TIMEOUT)
                .unwrap();
            let write = support::WriteObservation::receive(&mut stream).unwrap();
            eprintln!(
                "native {operation:?}: mode={case}, target={scenario}, write-observation={write:?}"
            );
            let lock = support::probe_lock(support::open_probe(&source).unwrap()).unwrap();
            assert!(
                matches!(lock, support::ProbeLock::Busy),
                "{case}: live lease lost"
            );
            drop(lock);
            if let support::WriteObservation::Blocked { kind, raw } = write {
                assert!(
                    case.ends_with("-lock-first"),
                    "prewritten case cannot skip writing"
                );
                assert!(cfg!(windows) && raw == 33);
                eprintln!(
                    "native {operation:?}: mode={case}, target={scenario}, NOT ATTEMPTED: write kind={kind:?}, raw_os_error={raw}; separate prewritten case required"
                );
                refused_writes += 1;
                continue_and_finish(&mut child, &mut stream);
                assert_eq!(std::fs::read(&source).unwrap(), b"");
                if existing_target {
                    assert_eq!(std::fs::read(&target).unwrap(), b"old");
                } else {
                    assert!(!target.exists());
                }
                continue;
            }

            attempted_operations += 1;
            let result = match operation {
                LiveOperation::Rename => std::fs::rename(&source, &target),
                LiveOperation::Delete => std::fs::remove_file(&source),
            };
            match &result {
                Ok(()) => eprintln!(
                    "native {operation:?}: mode={case}, target={scenario}, success, kind=None, raw_os_error=None"
                ),
                Err(error) => {
                    eprintln!(
                        "native {operation:?}: mode={case}, target={scenario}, failure, kind={:?}, raw_os_error={:?}",
                        error.kind(),
                        error.raw_os_error()
                    );
                    assert!(
                        cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33)),
                        "unexpected native {operation:?} failure: {error:?}"
                    );
                }
            }
            // No byte reads while the child's Windows byte-range lock exists.
            let succeeded = result.is_ok();
            continue_and_finish(&mut child, &mut stream);
            if succeeded {
                assert!(!source.exists());
                if matches!(operation, LiveOperation::Rename) {
                    assert_eq!(std::fs::read(&target).unwrap(), support::COMPLETE_NEW);
                    continue;
                }
            } else {
                assert_eq!(std::fs::read(&source).unwrap(), support::COMPLETE_NEW);
            }
            if existing_target {
                assert_eq!(std::fs::read(&target).unwrap(), b"old");
            } else {
                assert!(!target.exists());
            }
        }
    }
    // Every share-mode/scenario pair must either yield a real rename/delete
    // observation or a narrowly typed refused same-handle write. Nothing may
    // be silently skipped.
    assert_eq!(
        attempted_operations + refused_writes,
        cases.len() * 2,
        "some share-mode scenarios were skipped"
    );
}

#[test]
fn rename_with_live_handle_records_native_semantics() {
    observe_live_operation(LiveOperation::Rename);
}

#[test]
fn delete_with_live_handle_records_native_semantics() {
    observe_live_operation(LiveOperation::Delete);
}

#[test]
fn close_before_rename_exposes_unprotected_interval() {
    for case in ["close-before-rename", "close-prewritten-before-rename"] {
        let mut child = support::ChildGuard::spawn(case).unwrap();
        let mut stream = child
            .ready("closed-before-rename", support::DEFAULT_TIMEOUT)
            .unwrap();
        let write = support::WriteObservation::receive(&mut stream).unwrap();
        let path = child.root().join("temp");
        let lock = support::probe_lock(support::open_probe(&path).unwrap()).unwrap();
        assert!(
            matches!(lock, support::ProbeLock::Acquired(_)),
            "closed handle must expose a lockable interval: {lock:?}"
        );
        eprintln!(
            "evidence window only: {case}, write={write:?} -> close -> parent Acquired -> child rename if complete; not a protected replacement protocol"
        );
        drop(lock);
        continue_and_finish(&mut child, &mut stream);
        let target = child.root().join("target");
        match write {
            support::WriteObservation::Complete => {
                assert_eq!(std::fs::read(&target).unwrap(), support::COMPLETE_NEW);
                assert!(!path.exists());
            }
            support::WriteObservation::Blocked { .. } => {
                assert_eq!(case, "close-before-rename");
                assert_eq!(std::fs::read(&path).unwrap(), b"");
                assert!(!target.exists(), "refused write must not be renamed");
            }
        }
    }
}

#[test]
fn create_before_lock_exposes_unprotected_interval() {
    let mut child = support::ChildGuard::spawn("create-before-lock").unwrap();
    let mut stream = child
        .ready("created-before-lock", support::DEFAULT_TIMEOUT)
        .unwrap();
    let path = child.root().join("temp");
    let lock = support::probe_lock(support::open_probe(&path).unwrap()).unwrap();
    assert!(
        matches!(lock, support::ProbeLock::Acquired(_)),
        "created but unleased file must be lockable: {lock:?}"
    );
    eprintln!(
        "evidence window only: child create -> parent Acquired -> child prewrite/lock after CONTINUE; no deletion, no stale inference"
    );
    drop(lock);
    continue_and_finish(&mut child, &mut stream);
    assert_eq!(std::fs::read(&path).unwrap(), support::COMPLETE_NEW);
    assert!(!child.root().join("target").exists());
}

#[test]
fn legacy_unleased_file_is_never_proven_stale_by_lock_alone() {
    // Test-only evidence model: none of these decisions authorizes deletion.
    // Rule encoded as a comment, not asserted as a mechanism: a file with no
    // lease record is never stale, whatever its lock says.
    let mut child = support::ChildGuard::spawn("legacy-unleased").unwrap();
    let mut stream = child
        .ready("legacy-unleased", support::DEFAULT_TIMEOUT)
        .unwrap();
    let path = child.root().join("temp");
    let lock = support::probe_lock(support::open_probe(&path).unwrap()).unwrap();
    let support::ProbeLock::Acquired(ref file) = lock else {
        panic!("legacy open-but-unleased writer should allow lock acquisition");
    };
    // Metadata is safe under Windows locking; defer byte reads until both the
    // parent lock and the child legacy holder are closed.
    assert_eq!(
        file.metadata().unwrap().len(),
        support::COMPLETE_NEW.len() as u64
    );
    // The legacy writer is still running (it waits for CONTINUE) while the
    // parent holds an exclusive lock on the very file it has open. That is the
    // evidence the rule needs: "lock acquirable" says nothing about staleness,
    // so without a lease record the decision stays OwnershipUnknown and
    // nothing may be deleted.
    assert_eq!(
        child.reap(Duration::from_millis(1)).unwrap_err().kind(),
        io::ErrorKind::TimedOut,
        "legacy writer must still be running"
    );
    eprintln!(
        "legacy active open holder: parent Acquired while the writer is alive and no lease record exists -> OwnershipUnknown; no mutation/deletion"
    );
    drop(lock);
    continue_and_finish(&mut child, &mut stream);
    assert_eq!(std::fs::read(&path).unwrap(), support::COMPLETE_NEW);
    assert_eq!(std::fs::read_dir(child.root()).unwrap().count(), 1);
}

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
    // Windows was observed to report Busy for both; other platforms may share
    // instead, which is why the assertion is platform-scoped. Either outcome
    // is still no evidence of staleness, and production cleaning needs an
    // in-process active set regardless.
    #[cfg(windows)]
    {
        assert!(
            matches!(independent, support::ProbeLock::Busy),
            "Windows: independent same-process handle must observe the lock"
        );
        assert!(
            matches!(cloned, support::ProbeLock::Busy),
            "Windows: cloned handle must observe the lock"
        );
    }
    // Keep acquired handles alive; never explicitly unlock a clone:
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
