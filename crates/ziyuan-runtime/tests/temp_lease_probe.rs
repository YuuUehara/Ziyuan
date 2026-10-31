#[path = "support/temp_lease_probe.rs"]
mod support;

use std::fs::OpenOptions;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
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
fn lock_identity_survives_rename_and_hardlink_but_not_recreate() {
    // A lease cannot be "the path currently takes a lock": identity must be the
    // file object. Rename and hardlink keep one object; only delete + create
    // makes a new one that a stale path could point at.
    let mut child = support::ChildGuard::spawn("handshake").unwrap();
    let mut stream = child.ready("connected", support::DEFAULT_TIMEOUT).unwrap();
    let root = child.root().to_path_buf();
    let original = root.join("identity");
    let renamed = root.join("identity-renamed");
    let linked = root.join("identity-linked");

    let holder = match support::probe_lock(
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&original)
            .unwrap(),
    )
    .unwrap()
    {
        support::ProbeLock::Acquired(file) => file,
        support::ProbeLock::Busy => panic!("fresh file must not be locked"),
    };

    std::fs::rename(&original, &renamed).unwrap();
    let via_renamed = support::probe_lock(support::open_probe(&renamed).unwrap()).unwrap();
    eprintln!("identity after rename: {via_renamed:?}");

    std::fs::hard_link(&renamed, &linked).unwrap();
    let via_link = support::probe_lock(support::open_probe(&linked).unwrap()).unwrap();
    eprintln!("identity through hardlink: {via_link:?}");

    // Same-process lock exclusion is platform-specific; only Windows was
    // observed to report Busy, and that is what these assertions encode.
    #[cfg(windows)]
    {
        assert!(
            matches!(via_renamed, support::ProbeLock::Busy),
            "rename must keep the locked file object"
        );
        assert!(
            matches!(via_link, support::ProbeLock::Busy),
            "hardlink must keep the locked file object"
        );
    }
    drop(via_renamed);
    drop(via_link);
    drop(holder);

    // Delete then recreate produces a different object, so a lease keyed only
    // by path would silently describe the wrong file afterwards.
    std::fs::remove_file(&linked).unwrap();
    std::fs::remove_file(&renamed).unwrap();
    let recreated = match support::probe_lock(
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&renamed)
            .unwrap(),
    )
    .unwrap()
    {
        support::ProbeLock::Acquired(file) => file,
        support::ProbeLock::Busy => panic!("recreated file must be lockable"),
    };
    eprintln!("identity after delete + recreate: fresh object, lockable");
    drop(recreated);

    support::send_message(
        &mut stream,
        "CONTINUE",
        Instant::now() + support::DEFAULT_TIMEOUT,
        "parent identity CONTINUE",
    )
    .unwrap();
    child.finish(support::DEFAULT_TIMEOUT).unwrap();
}

fn lease_paths(root: &Path, id: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    (root.join("temp"), root.join(support::lease_file_name(id)))
}

fn dir_names(root: &Path) -> Vec<String> {
    let mut names = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn assert_lease_is_held(lease: &Path) {
    // A live writer must make the lease lock unobtainable. Only a non-blocking
    // probe is ever used; a cleaner is never allowed to wait here.
    let probe =
        support::probe_lock(support::open_probe(lease).expect("open lease probe handle")).unwrap();
    assert!(
        matches!(probe, support::ProbeLock::Busy),
        "lease must be held by the writer: {probe:?}"
    );
}

#[test]
fn lease_covers_close_before_rename_window() {
    let mut child = support::ChildGuard::spawn("lease-close-before-rename").unwrap();
    let mut stream = child
        .ready("lease-held-closed-before-rename", support::DEFAULT_TIMEOUT)
        .unwrap();
    let root = child.root().to_path_buf();
    let id = child.id().to_owned();
    let (temp, lease) = lease_paths(&root, &id);

    // Round one established this window: after the temp handle closes its own
    // lock is gone and an independent probe acquires it.
    let temp_probe = support::probe_lock(support::open_probe(&temp).unwrap()).unwrap();
    assert!(
        matches!(temp_probe, support::ProbeLock::Acquired(_)),
        "closed temp handle must still expose the unprotected interval: {temp_probe:?}"
    );
    drop(temp_probe);

    // The candidate lease must be busy at exactly that moment.
    assert_lease_is_held(&lease);
    eprintln!("native lease window: temp Acquired while lease Busy before rename");

    continue_and_finish(&mut child, &mut stream);
    assert_eq!(
        std::fs::read(root.join("target")).unwrap(),
        support::COMPLETE_NEW
    );
    assert!(!temp.exists());
    assert!(!lease.exists());
    assert_eq!(dir_names(&root), vec!["target".to_owned()]);
}

#[test]
fn cleaner_keeps_temp_while_lease_is_held() {
    let mut child = support::ChildGuard::spawn("lease-close-before-rename").unwrap();
    let mut stream = child
        .ready("lease-held-closed-before-rename", support::DEFAULT_TIMEOUT)
        .unwrap();
    let root = child.root().to_path_buf();
    let id = child.id().to_owned();
    let (temp, lease) = lease_paths(&root, &id);

    let decision = support::recover_temp(&root, &id, "temp").unwrap();
    assert_eq!(
        decision,
        support::LeaseDecision::keep(support::KeepReason::LeaseBusy),
        "a held lease must make the cleaner keep the temp file"
    );
    // The temp handle is already closed, so reading its bytes is safe here.
    assert_eq!(std::fs::read(&temp).unwrap(), support::COMPLETE_NEW);
    assert!(lease.is_file());
    assert_eq!(
        std::fs::metadata(&lease).unwrap().len(),
        support::lease_line("temp", &id).len() as u64
    );

    continue_and_finish(&mut child, &mut stream);
    assert_eq!(
        std::fs::read(root.join("target")).unwrap(),
        support::COMPLETE_NEW
    );
    assert!(!temp.exists());
    // The writer completed its own order: handle closed, then lease deleted.
    assert!(!lease.exists());
    assert_eq!(
        std::fs::metadata(&lease).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    // The name is free and its lock is acquirable again.
    let recreated = support::probe_lock(
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&lease)
            .unwrap(),
    )
    .unwrap();
    assert!(
        matches!(recreated, support::ProbeLock::Acquired(_)),
        "lease name must be acquirable after the writer finished: {recreated:?}"
    );
    drop(recreated);
    std::fs::remove_file(&lease).unwrap();
}

#[test]
fn killed_writer_lease_is_reclaimable() {
    // Verified reclaim: v1, matching temp=, regular temp file.
    {
        let mut child = support::ChildGuard::spawn("lease-held-kill").unwrap();
        let _stream = child.ready("lease-held", support::DEFAULT_TIMEOUT).unwrap();
        let root = child.root().to_path_buf();
        let id = child.id().to_owned();
        let (temp, lease) = lease_paths(&root, &id);
        std::fs::write(root.join("unrelated"), b"keep").unwrap();

        child.kill().unwrap();
        assert!(!child.reap(support::DEFAULT_TIMEOUT).unwrap().success());

        let decision = support::recover_temp(&root, &id, "temp").unwrap();
        assert_eq!(
            decision,
            support::LeaseDecision::remove(support::RemoveReason::WriterDeadLeaseVerified)
        );
        assert!(!temp.exists());
        assert!(!lease.exists());
        // Nothing the cleaner did not name may change.
        assert_eq!(std::fs::read(root.join("unrelated")).unwrap(), b"keep");
        assert_eq!(dir_names(&root), vec!["unrelated".to_owned()]);
    }

    // Retain branch: lease content does not describe this temp file.
    {
        let mut child = support::ChildGuard::spawn("lease-mismatched-kill").unwrap();
        let _stream = child.ready("lease-held", support::DEFAULT_TIMEOUT).unwrap();
        let root = child.root().to_path_buf();
        let id = child.id().to_owned();
        let (temp, lease) = lease_paths(&root, &id);

        child.kill().unwrap();
        assert!(!child.reap(support::DEFAULT_TIMEOUT).unwrap().success());

        let decision = support::recover_temp(&root, &id, "temp").unwrap();
        assert_eq!(
            decision,
            support::LeaseDecision::keep(support::KeepReason::LeaseContentMismatch)
        );
        assert_eq!(std::fs::read(&temp).unwrap(), support::COMPLETE_NEW);
        let content = std::fs::read_to_string(&lease).unwrap();
        assert_eq!(content, support::lease_line("other-temp", &id));
        assert_eq!(dir_names(&root).len(), 2);
    }

    // Retain branch: the referenced temp file is already absent.
    {
        let mut child = support::ChildGuard::spawn("lease-only-kill").unwrap();
        let _stream = child.ready("lease-only", support::DEFAULT_TIMEOUT).unwrap();
        let root = child.root().to_path_buf();
        let id = child.id().to_owned();
        let (temp, lease) = lease_paths(&root, &id);

        child.kill().unwrap();
        assert!(!child.reap(support::DEFAULT_TIMEOUT).unwrap().success());

        let decision = support::recover_temp(&root, &id, "temp").unwrap();
        assert_eq!(
            decision,
            support::LeaseDecision::keep(support::KeepReason::TempAlreadyAbsent)
        );
        assert!(!temp.exists());
        assert_eq!(
            std::fs::read_to_string(&lease).unwrap(),
            support::lease_line("temp", &id)
        );
        assert_eq!(dir_names(&root), vec![support::lease_file_name(&id)]);
    }
}

#[test]
fn killed_before_temp_creation_leaves_only_lease() {
    let mut child = support::ChildGuard::spawn("lease-only-kill").unwrap();
    let _stream = child.ready("lease-only", support::DEFAULT_TIMEOUT).unwrap();
    let root = child.root().to_path_buf();
    let id = child.id().to_owned();
    let (temp, lease) = lease_paths(&root, &id);

    child.kill().unwrap();
    assert!(!child.reap(support::DEFAULT_TIMEOUT).unwrap().success());
    assert!(!temp.exists());
    assert_eq!(dir_names(&root), vec![support::lease_file_name(&id)]);

    let decision = support::reclaim_orphan_lease(&root, &id).unwrap();
    assert_eq!(
        decision,
        support::LeaseDecision::remove(support::RemoveReason::OrphanLease)
    );
    assert!(!lease.exists());
    assert!(dir_names(&root).is_empty());
}

#[test]
fn temp_without_lease_and_foreign_leases_are_retained() {
    let scratch = support::Scratch::create().unwrap();
    let root = scratch.path();
    std::fs::write(root.join("temp"), support::COMPLETE_NEW).unwrap();

    // 1. A correct-looking temp file with no lease record.
    let no_lease = support::new_lease_id();
    let decision = support::recover_temp(root, &no_lease, "temp").unwrap();
    assert_eq!(
        decision,
        support::LeaseDecision::keep(support::KeepReason::TempWithoutLease)
    );
    assert_eq!(
        std::fs::read(root.join("temp")).unwrap(),
        support::COMPLETE_NEW
    );

    // 2a. Unknown version.
    let unknown = support::new_lease_id();
    let unknown_path = root.join(support::lease_file_name(&unknown));
    let unknown_line = format!("ziyuan-lease v2 temp=temp id={unknown}\n");
    std::fs::write(&unknown_path, &unknown_line).unwrap();
    let decision = support::recover_temp(root, &unknown, "temp").unwrap();
    assert_eq!(
        decision,
        support::LeaseDecision::keep(support::KeepReason::LeaseUnknownVersion)
    );
    assert_eq!(
        std::fs::read_to_string(&unknown_path).unwrap(),
        unknown_line
    );

    // 2b. Unparsable content.
    let unparsable = support::new_lease_id();
    let unparsable_path = root.join(support::lease_file_name(&unparsable));
    std::fs::write(&unparsable_path, b"not-a-lease record\n").unwrap();
    let decision = support::recover_temp(root, &unparsable, "temp").unwrap();
    assert_eq!(
        decision,
        support::LeaseDecision::keep(support::KeepReason::LeaseUnparsable)
    );
    assert_eq!(
        std::fs::read(&unparsable_path).unwrap(),
        b"not-a-lease record\n"
    );

    // 2c. Oversized content is not a lease we understand, and must not turn
    // into an I/O failure that aborts the whole cleanup.
    let oversized = support::new_lease_id();
    let oversized_path = root.join(support::lease_file_name(&oversized));
    std::fs::write(&oversized_path, vec![b'a'; 8192]).unwrap();
    let decision = support::recover_temp(root, &oversized, "temp").unwrap();
    assert_eq!(
        decision,
        support::LeaseDecision::keep(support::KeepReason::LeaseUnparsable)
    );
    assert_eq!(std::fs::metadata(&oversized_path).unwrap().len(), 8192);

    // 2d. Non-UTF-8 content takes the same retain path, not an error.
    let binary = support::new_lease_id();
    let binary_path = root.join(support::lease_file_name(&binary));
    std::fs::write(&binary_path, [0xff, 0xfe, 0x00, 0x0a]).unwrap();
    let decision = support::recover_temp(root, &binary, "temp").unwrap();
    assert_eq!(
        decision,
        support::LeaseDecision::keep(support::KeepReason::LeaseUnparsable)
    );
    assert_eq!(
        std::fs::read(&binary_path).unwrap(),
        [0xff, 0xfe, 0x00, 0x0a]
    );

    // 2e. Duplicate fields are ambiguous, so the record is not trusted.
    let duplicate = support::new_lease_id();
    let duplicate_path = root.join(support::lease_file_name(&duplicate));
    std::fs::write(
        &duplicate_path,
        format!("ziyuan-lease v1 temp=temp temp=other id={duplicate}\n"),
    )
    .unwrap();
    let decision = support::recover_temp(root, &duplicate, "temp").unwrap();
    assert_eq!(
        decision,
        support::LeaseDecision::keep(support::KeepReason::LeaseUnparsable)
    );

    // 3. Lease file name and recorded id disagree.
    let name_id = support::new_lease_id();
    let other_id = support::new_lease_id();
    let mismatched_path = root.join(support::lease_file_name(&name_id));
    let mismatched_line = support::lease_line("temp", &other_id);
    std::fs::write(&mismatched_path, &mismatched_line).unwrap();
    let decision = support::recover_temp(root, &name_id, "temp").unwrap();
    assert_eq!(
        decision,
        support::LeaseDecision::keep(support::KeepReason::LeaseIdMismatch)
    );
    assert_eq!(
        std::fs::read_to_string(&mismatched_path).unwrap(),
        mismatched_line
    );

    // 4. A path-bearing name proves nothing about this directory. `C:temp`
    //    would even replace the base path when joined, so it must be rejected.
    let traversal = support::new_lease_id();
    let traversal_path = root.join(support::lease_file_name(&traversal));
    std::fs::write(
        &traversal_path,
        format!("ziyuan-lease v1 temp=C:temp id={traversal}\n"),
    )
    .unwrap();
    let decision = support::recover_temp(root, &traversal, "temp").unwrap();
    assert_eq!(
        decision,
        support::LeaseDecision::keep(support::KeepReason::LeaseUnparsable)
    );
    assert!(traversal_path.is_file());

    // Nothing above authorized any deletion: the temp file and all leases are
    // byte-for-byte untouched.
    assert_eq!(
        std::fs::read(root.join("temp")).unwrap(),
        support::COMPLETE_NEW
    );
    assert_eq!(dir_names(root).len(), 8);
}

#[test]
fn uncovered_keep_branches_are_reachable() {
    let scratch = support::Scratch::create().unwrap();
    let root = scratch.path();

    // TempNotRegular: the recorded name exists but is a directory, which a
    // cleaner must never remove.
    let not_regular = support::new_lease_id();
    std::fs::write(
        root.join(support::lease_file_name(&not_regular)),
        support::lease_line("temp", &not_regular),
    )
    .unwrap();
    std::fs::create_dir(root.join("temp")).unwrap();
    let decision = support::recover_temp(root, &not_regular, "temp").unwrap();
    assert_eq!(
        decision,
        support::LeaseDecision::keep(support::KeepReason::TempNotRegular)
    );
    assert!(root.join("temp").is_dir());
    assert!(root.join(support::lease_file_name(&not_regular)).is_file());

    // TempStillPresent: the lease-garbage path must not delete a lease whose
    // temp file is still there.
    let still_present = support::new_lease_id();
    let still_present_lease = root.join(support::lease_file_name(&still_present));
    std::fs::write(
        &still_present_lease,
        support::lease_line("other", &still_present),
    )
    .unwrap();
    std::fs::write(root.join("other"), support::COMPLETE_NEW).unwrap();
    let decision = support::reclaim_orphan_lease(root, &still_present).unwrap();
    assert_eq!(
        decision,
        support::LeaseDecision::keep(support::KeepReason::TempStillPresent)
    );
    assert!(still_present_lease.is_file());
    assert!(root.join("other").is_file());

    // LeaseAlreadyAbsent: nothing to reclaim.
    let absent = support::new_lease_id();
    let decision = support::reclaim_orphan_lease(root, &absent).unwrap();
    assert_eq!(
        decision,
        support::LeaseDecision::keep(support::KeepReason::LeaseAlreadyAbsent)
    );

    // And the reclaim path does work on a real orphan, so the branch above is
    // not simply unreachable-by-construction.
    std::fs::remove_file(root.join("other")).unwrap();
    let decision = support::reclaim_orphan_lease(root, &still_present).unwrap();
    assert_eq!(
        decision,
        support::LeaseDecision::remove(support::RemoveReason::OrphanLease)
    );
    assert!(!still_present_lease.exists());
}

#[test]
fn recovery_referenced_temp_is_retained() {
    let scratch = support::Scratch::create().unwrap();
    let root = scratch.path();

    // A project file may legitimately carry the temp naming shape: the recovery
    // record then names it by workspace-relative path, and a leaked lease beside
    // it looks exactly like a dead writer's leftovers. Both must survive.
    let temp_name = format!(".ziyuan-tmp-{}-1-1", std::process::id());
    let id = support::new_lease_id();
    let lease = root.join(support::lease_file_name(&id));
    let leak = || {
        std::fs::write(root.join(&temp_name), support::COMPLETE_NEW).unwrap();
        std::fs::write(&lease, support::lease_line(&temp_name, &id)).unwrap();
    };
    leak();

    // The check is load-bearing: the same bytes and the same lease are removed
    // without the protected set, so the assertion below is not vacuous.
    let unprotected = support::recover_temp(root, &id, &temp_name).unwrap();
    assert_eq!(
        unprotected,
        support::LeaseDecision::remove(support::RemoveReason::WriterDeadLeaseVerified)
    );
    assert!(!root.join(&temp_name).exists());

    leak();
    let decision =
        support::recover_temp_protected(root, &id, &temp_name, &temp_name, &[temp_name.as_str()])
            .unwrap();
    assert_eq!(
        decision,
        support::LeaseDecision::keep(support::KeepReason::RecoveryReferenced)
    );
    assert_eq!(
        std::fs::read(root.join(&temp_name)).unwrap(),
        support::COMPLETE_NEW
    );
    // The lease stays too: it is the only remaining evidence of who wrote the
    // file, and the orphan path keeps it anyway while the temp is present.
    assert!(lease.is_file());
    assert_eq!(
        support::reclaim_orphan_lease(root, &id).unwrap(),
        support::LeaseDecision::keep(support::KeepReason::TempStillPresent)
    );

    // A recursive scan sees a nested candidate as `sub/<name>`, so the set must
    // be compared in the same relative form the record uses.
    std::fs::remove_file(&lease).unwrap();
    std::fs::remove_file(root.join(&temp_name)).unwrap();
    let nested = root.join("sub");
    std::fs::create_dir(&nested).unwrap();
    let relative = format!("sub/{temp_name}");
    let nested_lease = |id: &str| {
        std::fs::write(
            nested.join(support::lease_file_name(id)),
            support::lease_line(&temp_name, id),
        )
        .unwrap();
    };
    std::fs::write(nested.join(&temp_name), support::COMPLETE_NEW).unwrap();
    let id = support::new_lease_id();
    nested_lease(&id);
    assert_eq!(
        support::recover_temp_protected(&nested, &id, &relative, &temp_name, &[relative.as_str()])
            .unwrap(),
        support::LeaseDecision::keep(support::KeepReason::RecoveryReferenced)
    );
    assert!(nested.join(&temp_name).is_file());

    // Matching on the bare name instead would miss, and the candidate would be
    // deleted. This is the mismatch the production scan must not have.
    assert_eq!(
        support::recover_temp_protected(&nested, &id, &relative, &temp_name, &[temp_name.as_str()])
            .unwrap(),
        support::LeaseDecision::remove(support::RemoveReason::WriterDeadLeaseVerified)
    );
    assert!(!nested.join(&temp_name).exists());

    // The protected check must run before any lease is opened: with no lease at
    // all, an implementation that opened first would answer TempWithoutLease.
    std::fs::write(nested.join(&temp_name), support::COMPLETE_NEW).unwrap();
    assert_eq!(
        support::recover_temp_protected(
            &nested,
            &support::new_lease_id(),
            &relative,
            &temp_name,
            &[relative.as_str()],
        )
        .unwrap(),
        support::LeaseDecision::keep(support::KeepReason::RecoveryReferenced)
    );
    assert!(nested.join(&temp_name).is_file());

    // The directory scan must compare in the same relative form: with the
    // prefix it keeps the candidate, and with the bare name it does not even
    // reach the protected branch.
    let kept = support::sweep(&nested, "sub/", &[relative.as_str()]).unwrap();
    assert_eq!(
        kept,
        vec![(
            temp_name.clone(),
            support::LeaseDecision::keep(support::KeepReason::RecoveryReferenced)
        )]
    );
    assert!(nested.join(&temp_name).is_file());
    let wrong_form = support::sweep(&nested, "", &[relative.as_str()]).unwrap();
    assert_eq!(
        wrong_form,
        vec![(
            temp_name.clone(),
            support::LeaseDecision::keep(support::KeepReason::TempWithoutLease)
        )]
    );
}

#[test]
fn sweep_finds_the_lease_that_names_the_temp() {
    let scratch = support::Scratch::create().unwrap();
    let root = scratch.path();

    // Two leases in one directory: only the one whose content names the temp is
    // the owner. A cleaner cannot derive the id from the temp name, so it must
    // enumerate leases and read them.
    let temp_name = format!(".ziyuan-tmp-{}-{}-1", std::process::id(), 7);
    let owner = support::new_lease_id();
    let other = support::new_lease_id();
    std::fs::write(root.join(&temp_name), support::COMPLETE_NEW).unwrap();
    std::fs::write(root.join("keep.txt"), b"keep").unwrap();
    std::fs::write(
        root.join(support::lease_file_name(&other)),
        support::lease_line("keep.txt", &other),
    )
    .unwrap();
    std::fs::write(
        root.join(support::lease_file_name(&owner)),
        support::lease_line(&temp_name, &owner),
    )
    .unwrap();

    assert_eq!(
        support::sweep(root, "", &[]).unwrap(),
        vec![(
            temp_name.clone(),
            support::LeaseDecision::remove(support::RemoveReason::WriterDeadLeaseVerified)
        )]
    );
    assert!(!root.join(&temp_name).exists());
    // Only the owner's lease is consumed; the unrelated lease and its file stay.
    assert!(!root.join(support::lease_file_name(&owner)).exists());
    assert!(root.join(support::lease_file_name(&other)).is_file());
    assert_eq!(std::fs::read(root.join("keep.txt")).unwrap(), b"keep");
}

#[test]
fn sweep_keeps_temp_claimed_by_two_leases() {
    let scratch = support::Scratch::create().unwrap();
    let root = scratch.path();

    // Two unlocked leases both name the same temp. The protocol has no rule for
    // which writer owns it, so the candidate must be retained, not guessed at.
    let temp_name = format!(".ziyuan-tmp-{}-{}-2", std::process::id(), 9);
    std::fs::write(root.join(&temp_name), support::COMPLETE_NEW).unwrap();
    for _ in 0..2 {
        let id = support::new_lease_id();
        std::fs::write(
            root.join(support::lease_file_name(&id)),
            support::lease_line(&temp_name, &id),
        )
        .unwrap();
    }

    assert_eq!(
        support::sweep(root, "", &[]).unwrap(),
        vec![(
            temp_name.clone(),
            support::LeaseDecision::keep(support::KeepReason::TempAmbiguous)
        )]
    );
    assert_eq!(
        std::fs::read(root.join(&temp_name)).unwrap(),
        support::COMPLETE_NEW
    );
    assert_eq!(dir_names(root).len(), 3);
}

#[test]
fn sweep_refuses_a_directory_with_an_unreadable_lease() {
    let scratch = support::Scratch::create().unwrap();
    let root = scratch.path();

    // A corrupt lease might also name this candidate, so one valid claim plus
    // one unreadable lease is not enough to delete anything here.
    let temp_name = format!(".ziyuan-tmp-{}-{}-4", std::process::id(), 13);
    std::fs::write(root.join(&temp_name), support::COMPLETE_NEW).unwrap();
    let owner = support::new_lease_id();
    std::fs::write(
        root.join(support::lease_file_name(&owner)),
        support::lease_line(&temp_name, &owner),
    )
    .unwrap();
    let broken = support::new_lease_id();
    std::fs::write(root.join(support::lease_file_name(&broken)), b"garbage").unwrap();

    assert_eq!(
        support::sweep(root, "", &[]).unwrap(),
        vec![(
            temp_name.clone(),
            support::LeaseDecision::keep(support::KeepReason::LeaseUnparsable)
        )]
    );
    assert_eq!(
        std::fs::read(root.join(&temp_name)).unwrap(),
        support::COMPLETE_NEW
    );
    // Removing the unreadable lease unblocks the same directory.
    std::fs::remove_file(root.join(support::lease_file_name(&broken))).unwrap();
    assert_eq!(
        support::sweep(root, "", &[]).unwrap(),
        vec![(
            temp_name.clone(),
            support::LeaseDecision::remove(support::RemoveReason::WriterDeadLeaseVerified)
        )]
    );
    assert!(!root.join(&temp_name).exists());
}

#[test]
fn sweep_checks_candidate_type_before_looking_for_a_lease() {
    let scratch = support::Scratch::create().unwrap();
    let root = scratch.path();

    // A directory named like a temp file is not a temp file, even in a directory
    // with no leases at all. Reporting TempWithoutLease here would be a weaker
    // answer than the truth, and the type check must not depend on a lease.
    let name = format!(".ziyuan-tmp-{}-{}-5", std::process::id(), 17);
    std::fs::create_dir(root.join(&name)).unwrap();
    assert_eq!(
        support::sweep(root, "", &[]).unwrap(),
        vec![(
            name.clone(),
            support::LeaseDecision::keep(support::KeepReason::TempNotRegular)
        )]
    );
    assert!(root.join(&name).is_dir());
}

#[test]
fn lease_that_is_not_a_file_is_never_followed() {
    let scratch = support::Scratch::create().unwrap();
    let root = scratch.path();

    // A directory (or, on a host that allows it, a link) sitting at the lease
    // path must not be opened, locked or treated as evidence of anything.
    let temp_name = format!(".ziyuan-tmp-{}-{}-3", std::process::id(), 11);
    let id = support::new_lease_id();
    std::fs::write(root.join(&temp_name), support::COMPLETE_NEW).unwrap();
    std::fs::create_dir(root.join(support::lease_file_name(&id))).unwrap();

    assert_eq!(
        support::sweep(root, "", &[]).unwrap(),
        vec![(
            temp_name.clone(),
            support::LeaseDecision::keep(support::KeepReason::LeaseNotRegular)
        )]
    );
    assert_eq!(
        support::reclaim_orphan_lease(root, &id).unwrap(),
        support::LeaseDecision::keep(support::KeepReason::LeaseNotRegular)
    );
    assert_eq!(
        std::fs::read(root.join(&temp_name)).unwrap(),
        support::COMPLETE_NEW
    );
}

#[test]
fn writer_aborts_when_its_lease_was_reclaimed_before_the_lock() {
    // Between "content synced" and "lock taken" the lease is valid, unlocked and
    // points at a temp file that does not exist yet, so the orphan-lease path may
    // reclaim it even though the writer is alive. What happens next decides
    // whether that is harmless.
    let setup = |case: &str| -> (support::ChildGuard, TcpStream, std::path::PathBuf, String) {
        let mut child = support::ChildGuard::spawn(case).unwrap();
        let stream = child
            .ready("lease-unlocked", support::DEFAULT_TIMEOUT)
            .unwrap();
        let root = child.root().to_path_buf();
        let id = child.id().to_owned();
        assert_eq!(
            std::fs::read_to_string(root.join(support::lease_file_name(&id))).unwrap(),
            support::lease_line("temp", &id)
        );
        assert!(!root.join("temp").exists());
        (child, stream, root, id)
    };

    // Without the check the writer proceeds and leaves a temp file that no lease
    // claims, so no cleaner can ever prove it stale. This is the defect model.
    {
        let (mut child, mut stream, root, id) = setup("lease-stolen-before-lock-unchecked");
        assert_eq!(
            support::reclaim_orphan_lease(&root, &id).unwrap(),
            support::LeaseDecision::remove(support::RemoveReason::OrphanLease)
        );
        assert!(!root.join(support::lease_file_name(&id)).exists());
        support::send_message(
            &mut stream,
            "CONTINUE",
            Instant::now() + support::DEFAULT_TIMEOUT,
            "parent CONTINUE",
        )
        .unwrap();
        let message = support::read_message(
            &mut stream,
            Instant::now() + support::DEFAULT_TIMEOUT,
            "parent reclaimed lease result",
        )
        .unwrap();
        assert!(message.starts_with("LEASE_PROCEED"), "{message}");
        // The point of the defect model: locking still succeeded on a lease that
        // no longer exists, so the lock result alone proves nothing.
        assert!(message.contains("locked=true"), "{message}");
        assert!(message.contains("gone=true"), "{message}");
        eprintln!("native reclaimed-lease window (unchecked): {message}");
        child.finish(support::DEFAULT_TIMEOUT).unwrap();
        assert!(root.join("temp").exists());
        // The leftover is unrecoverable by design: no lease names it.
        assert_eq!(
            support::recover_temp(&root, &id, "temp").unwrap(),
            support::LeaseDecision::keep(support::KeepReason::TempWithoutLease)
        );
    }

    // With the check the writer aborts before creating anything, so the same
    // reclaim leaves an empty directory.
    {
        let (mut child, mut stream, root, id) = setup("lease-stolen-before-lock");
        assert_eq!(
            support::reclaim_orphan_lease(&root, &id).unwrap(),
            support::LeaseDecision::remove(support::RemoveReason::OrphanLease)
        );
        support::send_message(
            &mut stream,
            "CONTINUE",
            Instant::now() + support::DEFAULT_TIMEOUT,
            "parent CONTINUE",
        )
        .unwrap();
        let message = support::read_message(
            &mut stream,
            Instant::now() + support::DEFAULT_TIMEOUT,
            "parent reclaimed lease result",
        )
        .unwrap();
        assert!(message.starts_with("LEASE_ABORTED"), "{message}");
        assert!(message.contains("locked=true"), "{message}");
        assert!(message.contains("gone=true"), "{message}");
        eprintln!("native reclaimed-lease window (checked): {message}");
        child.finish(support::DEFAULT_TIMEOUT).unwrap();
        assert!(!root.join("temp").exists(), "no temp may be created");
        assert!(dir_names(&root).is_empty(), "{:?}", dir_names(&root));
    }
}

#[test]
fn second_creator_conflicts_instead_of_overwriting() {
    let mut child = support::ChildGuard::spawn("lease-conflict").unwrap();
    let mut stream = child
        .ready("lease-conflict", support::DEFAULT_TIMEOUT)
        .unwrap();
    let root = child.root().to_path_buf();
    let id = child.id().to_owned();
    let lease = root.join(support::lease_file_name(&id));
    let original = support::lease_line("temp", &id);
    std::fs::write(&lease, &original).unwrap();

    // create_new is the conflict primitive, observed directly as well.
    let conflict = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&lease)
        .unwrap_err();
    assert_eq!(conflict.kind(), io::ErrorKind::AlreadyExists);

    support::send_message(
        &mut stream,
        "GO",
        Instant::now() + support::DEFAULT_TIMEOUT,
        "parent conflict GO",
    )
    .unwrap();
    let message = support::read_message(
        &mut stream,
        Instant::now() + support::DEFAULT_TIMEOUT,
        "parent conflict result",
    )
    .unwrap();
    assert_eq!(message, "LEASE_CONFLICT");
    child.finish(support::DEFAULT_TIMEOUT).unwrap();
    assert_eq!(std::fs::read_to_string(&lease).unwrap(), original);
    assert_eq!(dir_names(&root), vec![support::lease_file_name(&id)]);
}

#[test]
fn lease_lock_wait_has_deadline() {
    let started = Instant::now();
    let mut child = support::ChildGuard::spawn("lease-wait").unwrap();
    let mut stream = child.ready("lease-wait", support::DEFAULT_TIMEOUT).unwrap();
    let root = child.root().to_path_buf();
    let id = child.id().to_owned();
    let lease = root.join(support::lease_file_name(&id));

    // The parent occupies the lease lock for the whole child wait.
    let held = match support::probe_lock(support::create_lease_file(&root, &id, "temp").unwrap())
        .unwrap()
    {
        support::ProbeLock::Acquired(file) => file,
        support::ProbeLock::Busy => panic!("fresh lease lock unexpectedly busy"),
    };
    support::send_message(
        &mut stream,
        "GO",
        Instant::now() + support::DEFAULT_TIMEOUT,
        "parent wait GO",
    )
    .unwrap();
    let message = support::read_message(
        &mut stream,
        Instant::now() + support::DEFAULT_TIMEOUT,
        "parent wait result",
    )
    .unwrap();
    let millis = message
        .strip_prefix("WAIT_TIMEOUT ")
        .expect("writer must report a deadline timeout, not acquisition")
        .parse::<u128>()
        .unwrap();
    assert!(
        (200..2000).contains(&millis),
        "lock wait must end in the hundreds-of-milliseconds range, not {millis}ms"
    );
    eprintln!("native lease lock wait: WAIT_TIMEOUT after {millis} ms");
    // Both sides can still finish; no orphaned child or retained scratch.
    continue_and_finish(&mut child, &mut stream);
    assert!(started.elapsed() < support::DEFAULT_TIMEOUT);
    // The timeout was caused by the parent's own lock, not by a broken probe:
    // releasing it makes the very same lease acquirable again.
    drop(held);
    let released = support::probe_lock(support::open_probe(&lease).unwrap()).unwrap();
    assert!(
        matches!(released, support::ProbeLock::Acquired(_)),
        "lease must be acquirable once the occupying handle closes: {released:?}"
    );
    drop(released);
    assert_eq!(dir_names(&root), vec![support::lease_file_name(&id)]);
}

#[test]
fn two_cleaners_never_double_delete() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let mut child = support::ChildGuard::spawn("handshake").unwrap();
    let mut stream = child.ready("connected", support::DEFAULT_TIMEOUT).unwrap();
    let root = child.root().to_path_buf();
    let id = child.id().to_owned();
    let lease = root.join(support::lease_file_name(&id));
    std::fs::write(&lease, support::lease_line("temp", &id)).unwrap();
    std::fs::write(root.join("temp"), support::COMPLETE_NEW).unwrap();

    // Deterministic interleaving, not a timing race: cleaner A is held inside
    // verification until B has had its chance. If the lease lock were released
    // before deletion, B would also reach verification; while it is held, B can
    // only observe Busy.
    let arrived = Arc::new(AtomicUsize::new(0));
    let released = Arc::new(AtomicBool::new(false));
    {
        let arrived = Arc::clone(&arrived);
        let released = Arc::clone(&released);
        support::install_delete_hook(
            &root,
            Arc::new(move || {
                arrived.fetch_add(1, Ordering::SeqCst);
                let deadline = Instant::now() + support::DEFAULT_TIMEOUT;
                while !released.load(Ordering::SeqCst) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }),
        );
    }

    let wait_for = |target: usize, limit: Duration| {
        let deadline = Instant::now() + limit;
        while arrived.load(Ordering::SeqCst) < target && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        arrived.load(Ordering::SeqCst)
    };

    let outcomes = std::thread::scope(|scope| {
        let first = {
            let root = root.clone();
            let id = id.clone();
            scope.spawn(move || support::recover_temp(&root, &id, "temp"))
        };
        // A must be inside verification before B starts, so B cannot win the
        // lock simply by arriving first.
        assert_eq!(
            wait_for(1, support::DEFAULT_TIMEOUT),
            1,
            "first cleaner never reached verification"
        );
        let second = {
            let root = root.clone();
            let id = id.clone();
            scope.spawn(move || support::recover_temp(&root, &id, "temp"))
        };
        // B gets a full window to reach verification before A is released.
        let observed = wait_for(2, Duration::from_secs(1));
        released.store(true, Ordering::SeqCst);
        let results = vec![first.join().unwrap(), second.join().unwrap()];
        (results, observed)
    });
    support::clear_delete_hook();

    let (results, observed) = outcomes;
    let decisions: Vec<_> = results
        .into_iter()
        .map(|result| result.expect("cleaners must not error on a live lease"))
        .collect();
    assert_eq!(
        observed, 1,
        "only the lock holder may reach verification; a second arrival proves the lease lock was released before deletion"
    );
    let removals = decisions
        .iter()
        .filter(|decision| matches!(decision, support::LeaseDecision::Remove { .. }))
        .count();
    assert_eq!(removals, 1, "exactly one cleaner may remove: {decisions:?}");
    assert_eq!(
        decisions
            .iter()
            .filter(|decision| {
                matches!(
                    decision,
                    support::LeaseDecision::Keep {
                        reason: support::KeepReason::LeaseBusy
                    }
                )
            })
            .count(),
        1,
        "the losing cleaner must report a busy lease, not a silent skip: {decisions:?}"
    );
    eprintln!("native double cleaner: {decisions:?}");
    assert!(!root.join("temp").exists());
    assert!(!lease.exists());
    assert!(dir_names(&root).is_empty(), "directory must be clean");

    support::send_message(
        &mut stream,
        "CONTINUE",
        Instant::now() + support::DEFAULT_TIMEOUT,
        "parent double cleaner CONTINUE",
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
