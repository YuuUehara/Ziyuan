//! Public draft behavior, with actual files and an independently launched reader/writer.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
};
use ziyuan_app::{
    text_project::{NewProject, Project, Provenance, TextProjects},
    translation::{
        DraftParagraph, DraftSave, DraftSubmit, HumanSubmission, ReviewCandidate, ReviewFormat,
        TargetMark, TargetMarkKind, TranslationError, Translations,
    },
};
use ziyuan_storage::{EventLog, JsonFileError, LockScope, Workspace};

const BLOCK: &str = "c0001-b0001";
struct Fixture {
    root: PathBuf,
    project: Project,
}
impl Fixture {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "ziyuan-drafts-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        // macOS temp_dir() can include a system symlink such as /var.
        let root = root.canonicalize().unwrap();
        let texts = TextProjects::new(&root);
        let project = texts
            .create(NewProject {
                title: "灯港".into(),
                source_language: "ja".into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap();
        // Minimal N0 excerpt, kept self-contained when this crate is packaged.
        let path = root.join("chapter.txt");
        fs::write(&path, "港では、名を明かさない女が灯台を見ていた。\n\n船長の蓮は言った。「今夜は出ない。」\n\n薬師の蓮は、船長とは別人だった。").unwrap();
        let project = texts.import(&project.id, &[path]).unwrap();
        Self { root, project }
    }
    fn service(&self) -> Translations {
        Translations::new(&self.root)
    }
    fn request(&self) -> DraftSave {
        let view = self.service().human_draft(&self.project.id, BLOCK).unwrap();
        DraftSave {
            request_id: "save-17".into(),
            expected_revision: view.draft.map(|d| d.revision),
            basis: view.basis,
            paragraphs: vec![
                DraftParagraph {
                    id: "c0001-p0001".into(),
                    text: "港口，一个不肯透露姓名的女人望着灯塔。".into(),
                    marks: Vec::new(),
                },
                DraftParagraph {
                    id: "c0001-p0002".into(),
                    text: String::new(),
                    marks: Vec::new(),
                },
            ],
        }
    }
    fn marked_request(&self) -> DraftSave {
        let mut request = self.request();
        request.paragraphs[0].text = "甲月舟乙".into();
        // Deliberately not range-sorted. Ruby's base is the two middle characters.
        request.paragraphs[0].marks = vec![
            TargetMark {
                start: 3,
                end: 9,
                kind: TargetMarkKind::Ruby {
                    reading: "ねがい".into(),
                },
            },
            TargetMark {
                start: 3,
                end: 9,
                kind: TargetMarkKind::EmphasisDots {},
            },
            TargetMark {
                start: 9,
                end: 12,
                kind: TargetMarkKind::Emphasis {},
            },
            TargetMark {
                start: 0,
                end: 3,
                kind: TargetMarkKind::Strong {},
            },
        ];
        request.paragraphs[1].text = "船长莲".into();
        request.paragraphs.push(DraftParagraph {
            id: "c0001-p0003".into(),
            text: "药师莲".into(),
            marks: Vec::new(),
        });
        request
    }
    fn path(&self) -> PathBuf {
        self.root
            .join(".ziyuan/text-projects")
            .join(&self.project.id)
            .join("human-drafts")
            .join(format!("{BLOCK}.json"))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn bound_literary_candidate_survives_lost_drafts_without_becoming_approved() {
    use ziyuan_core::{HumanActor, TranslationSource};
    let f = Fixture::new();
    let request = f.marked_request();
    let saved = f
        .service()
        .save_human_draft(&f.project.id, BLOCK, &request)
        .unwrap();
    let submission = f
        .service()
        .submit_human_draft(
            &f.project.id,
            BLOCK,
            &DraftSubmit {
                request_id: "literary-submission".into(),
                expected_revision: saved.revision,
            },
        )
        .unwrap();
    let binding = submission
        .prepare_binding(
            "bind-1".into(),
            HumanActor::Local,
            TranslationSource::None {},
        )
        .unwrap();
    let mut forged = submission.clone();
    forged.candidate_hash = "0".repeat(64);
    assert!(
        forged
            .prepare_binding(
                "bind-1".into(),
                HumanActor::Local,
                TranslationSource::None {}
            )
            .is_err()
    );
    assert_eq!(
        binding.request.candidate.as_bytes(),
        submission.candidate().unwrap().serialized().unwrap()
    );

    let session = ziyuan_app::Session::found(EventLog::new(&f.root)).unwrap();
    let workspace = Workspace::open(&f.root).unwrap();
    let mut guard = workspace.lock(LockScope::Workspace).unwrap();
    guard.upgrade_write_protocol().unwrap();
    TextProjects::new(&f.root)
        .upgrade_project_protocol(&f.project.id, &guard)
        .unwrap();
    drop(session);
    assert!(f.service().publish_human_binding(&guard, &binding).unwrap());
    drop(guard);
    fs::remove_file(f.path()).unwrap();
    let guard = workspace.lock(LockScope::Workspace).unwrap();
    let before = fs::read(f.root.join(".ziyuan/court.log")).unwrap();
    assert!(!f.service().publish_human_binding(&guard, &binding).unwrap());
    assert_eq!(fs::read(f.root.join(".ziyuan/court.log")).unwrap(), before);
    drop(guard);
    let reopened = ziyuan_app::Session::open(EventLog::new(&f.root)).unwrap();
    let frozen = reopened
        .court()
        .human_submission_binding(&f.project.id, BLOCK, "bind-1")
        .unwrap();
    assert_eq!(frozen, &binding);
    let candidate = ReviewCandidate::from_binding(frozen).unwrap();
    assert_eq!(
        candidate.paragraphs, request.paragraphs,
        "ruby, dots, bold and italic survive in their original order"
    );
    assert_eq!(
        candidate.serialized().unwrap(),
        binding.request.candidate.as_bytes()
    );
    assert!(
        reopened
            .court()
            .translation_signoff(&f.project.id, BLOCK)
            .is_none()
    );
    assert_eq!(
        reopened
            .court()
            .translation_tasks(&f.project.id, BLOCK)
            .count(),
        0
    );
    assert!(
        f.service()
            .read_translation(&f.project.id, BLOCK)
            .unwrap()
            .is_none()
    );
    assert_eq!(Workspace::open(&f.root).unwrap().generation().unwrap(), 0);
    assert!(!f.path().exists());

    let reseal = |b: &mut ziyuan_core::HumanSubmissionBinding| {
        b.request.candidate_hash = ziyuan_storage::content_sha256(b.request.candidate.as_bytes());
        b.request_hash = ziyuan_governance::human_binding_request_hash(&b.request);
    };
    for change in 0..4 {
        let mut invalid = binding.clone();
        match change {
            0 => {
                invalid.request.candidate.pop();
            } // Missing canonical final LF.
            1 => {
                invalid.request.candidate = invalid
                    .request
                    .candidate
                    .replace("\"schema_version\": 2", "\"schema_version\": 99")
            }
            2 => invalid.request.candidate = invalid.request.candidate.replace("ねがい", " "),
            _ => invalid.request.block = "c0002-b0001".into(),
        }
        reseal(&mut invalid); // Even self-consistent hashes cannot legalize bad material.
        assert!(ReviewCandidate::from_binding(&invalid).is_err());
    }
    let mut reading_changed = binding.clone();
    reading_changed.request.candidate = reading_changed
        .request
        .candidate
        .replace("ねがい", "つきふね");
    reseal(&mut reading_changed);
    assert_ne!(
        reading_changed.request.candidate_hash,
        binding.request.candidate_hash
    );
    assert_ne!(reading_changed.request_hash, binding.request_hash);
    assert!(ReviewCandidate::from_binding(&reading_changed).is_ok());
}

#[test]
fn material_writers_wait_for_admission_lock_and_refuse_interrupted_writes() {
    use std::{sync::mpsc, time::Duration};
    let f = Fixture::new();
    let t = f.service();
    t.set_zhupi(&f.project.id, BLOCK, "c0001-p0001", "原批")
        .unwrap();
    t.ensure_context_draft(&f.project.id).unwrap();
    let workspace = Workspace::open(&f.root).unwrap();
    let guard = workspace.lock(LockScope::Workspace).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let workers: Vec<_> = (0..3)
        .map(|op| {
            let root = f.root.clone();
            let project = f.project.id.clone();
            let started = started_tx.clone();
            let done = done_tx.clone();
            std::thread::spawn(move || {
                let t = Translations::new(root);
                started.send(()).unwrap();
                let result = match op {
                    0 => t.set_zhupi(&project, BLOCK, "c0001-p0002", "新批"),
                    1 => t.clear_zhupi(&project, BLOCK, "c0001-p0001"),
                    _ => t.approve_context(&project),
                };
                done.send(result).unwrap();
            })
        })
        .collect();
    for _ in 0..3 {
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    let blocked = done_rx.recv_timeout(Duration::from_millis(150));
    drop(guard);
    for worker in workers {
        worker.join().unwrap();
    }
    assert!(
        matches!(blocked, Err(mpsc::RecvTimeoutError::Timeout)),
        "material cannot change while admission holds the lock"
    );
    for _ in 0..3 {
        assert!(
            done_rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .is_ok()
        );
    }
    assert_eq!(t.zhupi_version(&f.project.id).unwrap(), 3);
    assert_eq!(t.context_version(&f.project.id).unwrap(), 1);
    let before = t.zhupi(&f.project.id, BLOCK).unwrap();
    let mut guard = workspace.lock(LockScope::Workspace).unwrap();
    guard.begin().unwrap();
    drop(guard);
    assert!(
        t.set_zhupi(&f.project.id, BLOCK, "c0001-p0002", "不该写入")
            .is_err()
    );
    assert!(t.clear_zhupi(&f.project.id, BLOCK, "c0001-p0002").is_err());
    assert!(
        t.approve_context(&f.project.id).is_err(),
        "even an identical snapshot must honor recovery blocking"
    );
    assert_eq!(t.zhupi(&f.project.id, BLOCK).unwrap(), before);
    assert_eq!(t.context_version(&f.project.id).unwrap(), 1);
    workspace
        .lock(LockScope::Workspace)
        .unwrap()
        .finish()
        .unwrap();
}

#[test]
fn project_barrier_preserves_marked_drafts_basis_and_frozen_submission_retries() {
    let f = Fixture::new();
    let request = f.marked_request();
    let receipt = f
        .service()
        .save_human_draft(&f.project.id, BLOCK, &request)
        .unwrap();
    let submit = DraftSubmit {
        request_id: "review-before-project-upgrade".into(),
        expected_revision: receipt.revision.clone(),
    };
    let submission = f
        .service()
        .submit_human_draft(&f.project.id, BLOCK, &submit)
        .unwrap();
    let bytes = fs::read(f.path()).unwrap();
    let view = f.service().human_draft(&f.project.id, BLOCK).unwrap();
    let workspace = Workspace::open(&f.root).unwrap();
    let mut guard = workspace.lock(LockScope::Workspace).unwrap();
    guard.upgrade_write_protocol().unwrap();
    f.service()
        .texts()
        .upgrade_project_protocol(&f.project.id, &guard)
        .unwrap();
    drop(guard);
    assert_eq!(f.service().human_draft(&f.project.id, BLOCK).unwrap(), view);
    assert_eq!(
        f.service()
            .save_human_draft(&f.project.id, BLOCK, &request)
            .unwrap(),
        receipt
    );
    assert_eq!(
        f.service()
            .submit_human_draft(&f.project.id, BLOCK, &submit)
            .unwrap(),
        submission
    );
    assert_eq!(fs::read(f.path()).unwrap(), bytes);
    let mut next = request;
    next.request_id = "after-project-upgrade".into();
    next.expected_revision = Some(receipt.revision);
    next.paragraphs[0].marks[0].kind = TargetMarkKind::Ruby {
        reading: "新读音".into(),
    };
    f.service()
        .save_human_draft(&f.project.id, BLOCK, &next)
        .unwrap();
    assert_eq!(
        f.service()
            .human_draft(&f.project.id, BLOCK)
            .unwrap()
            .draft
            .unwrap()
            .paragraphs,
        next.paragraphs
    );
    assert_eq!(workspace.generation().unwrap(), 0);
    assert!(workspace.pending_record().unwrap().is_none());
    assert!(
        f.service()
            .read_translation(&f.project.id, BLOCK)
            .unwrap()
            .is_none()
    );
    assert!(!f.root.join(".ziyuan/court.log").exists());
}

#[test]
fn drafts_are_incomplete_private_and_retries_do_not_roll_back_newer_text() {
    let f = Fixture::new();
    let source_before = fs::read(
        f.root
            .join(".ziyuan/text-projects")
            .join(&f.project.id)
            .join("project.json"),
    )
    .unwrap();
    let request = f.request();
    let first = f
        .service()
        .save_human_draft(&f.project.id, BLOCK, &request)
        .unwrap();
    let loaded = f
        .service()
        .human_draft(&f.project.id, BLOCK)
        .unwrap()
        .draft
        .unwrap();
    assert_eq!(loaded.paragraphs, request.paragraphs);
    assert_eq!(loaded.revision, first.revision);
    // Saving a partial draft must not create an approved translation or a court.
    assert_eq!(
        f.service().read_translation(&f.project.id, BLOCK).unwrap(),
        None
    );
    assert!(!f.root.join(".ziyuan/court.log").exists());
    assert_eq!(Workspace::open(&f.root).unwrap().generation().unwrap(), 0);
    assert_eq!(
        fs::read(
            f.root
                .join(".ziyuan/text-projects")
                .join(&f.project.id)
                .join("project.json")
        )
        .unwrap(),
        source_before
    );

    let mut second = f.request();
    second.request_id = "save-18".into();
    second.paragraphs[0].text = "第二稿：女人望着灯塔。".into();
    let newer = f
        .service()
        .save_human_draft(&f.project.id, BLOCK, &second)
        .unwrap();
    let bytes = fs::read(f.path()).unwrap();
    assert_eq!(
        f.service()
            .save_human_draft(&f.project.id, BLOCK, &request)
            .unwrap(),
        first
    );
    assert_eq!(
        fs::read(f.path()).unwrap(),
        bytes,
        "retry must not publish or roll back"
    );
    assert_eq!(
        f.service()
            .human_draft(&f.project.id, BLOCK)
            .unwrap()
            .draft
            .unwrap()
            .revision,
        newer.revision
    );
    let mut stale = request.clone();
    stale.request_id = "other-window".into();
    assert!(
        matches!(f.service().save_human_draft(&f.project.id, BLOCK, &stale), Err(TranslationError::DraftConflict { current: Some(r) }) if r == newer.revision)
    );
    let mut reused = request;
    reused.paragraphs[0].text = "same ID, other text".into();
    assert!(matches!(
        f.service().save_human_draft(&f.project.id, BLOCK, &reused),
        Err(TranslationError::DraftRequestConflict)
    ));
    assert_eq!(fs::read(f.path()).unwrap(), bytes);
    let output = f.service().block_dir(&f.project.id, BLOCK).join("zh.json");
    fs::create_dir_all(output.parent().unwrap()).unwrap();
    let existing = br#"{"schema_version":1,"block_id":"c0001-b0001","paragraphs":[]}"#;
    fs::write(&output, existing).unwrap();
    let mut empty = f.request();
    empty.request_id = "explicit-clear".into();
    empty.paragraphs.clear();
    f.service()
        .save_human_draft(&f.project.id, BLOCK, &empty)
        .unwrap();
    assert!(
        f.service()
            .human_draft(&f.project.id, BLOCK)
            .unwrap()
            .draft
            .unwrap()
            .paragraphs
            .is_empty()
    );
    assert_eq!(
        fs::read(output).unwrap(),
        existing,
        "drafts must not touch or copy disk translations"
    );
}

#[test]
fn concurrent_windows_have_one_winner_and_a_typed_conflict() {
    let f = Fixture::new();
    let request = f.request();
    let barrier = Arc::new(Barrier::new(2));
    let threads = (0..2)
        .map(|n| {
            let (root, project, barrier, mut request) = (
                f.root.clone(),
                f.project.id.clone(),
                barrier.clone(),
                request.clone(),
            );
            std::thread::spawn(move || {
                request.request_id = format!("window-{n}");
                request.paragraphs[0].text = format!("window {n}");
                barrier.wait();
                Translations::new(root).save_human_draft(&project, BLOCK, &request)
            })
        })
        .collect::<Vec<_>>();
    let results = threads
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(TranslationError::DraftConflict { .. })))
            .count(),
        1
    );
    let winner = results.iter().position(|r| r.is_ok()).unwrap();
    assert_eq!(
        f.service()
            .human_draft(&f.project.id, BLOCK)
            .unwrap()
            .draft
            .unwrap()
            .paragraphs[0]
            .text,
        format!("window {winner}")
    );
}

#[test]
fn process_restart_preserves_marked_text_and_original_retry_receipt() {
    let f = Fixture::new();
    let request = f.marked_request();
    let receipt = f
        .service()
        .save_human_draft(&f.project.id, BLOCK, &request)
        .unwrap();
    fs::write(
        f.root.join("request.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    fs::write(
        f.root.join("receipt.json"),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
    let before = fs::read(f.path()).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "draft_process_worker", "--nocapture"])
        .env("ZIYUAN_DRAFT_TEST_ROOT", &f.root)
        .env("ZIYUAN_DRAFT_TEST_PROJECT", &f.project.id)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(f.path()).unwrap(), before);
}

#[test]
fn draft_process_worker() {
    let Some(root) = std::env::var_os("ZIYUAN_DRAFT_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let project = std::env::var("ZIYUAN_DRAFT_TEST_PROJECT").unwrap();
    let request: DraftSave =
        serde_json::from_slice(&fs::read(root.join("request.json")).unwrap()).unwrap();
    let expected = serde_json::from_slice::<ziyuan_app::translation::DraftReceipt>(
        &fs::read(root.join("receipt.json")).unwrap(),
    )
    .unwrap();
    let service = Translations::new(&root);
    assert_eq!(
        service
            .human_draft(&project, BLOCK)
            .unwrap()
            .draft
            .unwrap()
            .paragraphs,
        request.paragraphs
    );
    assert_eq!(
        service.save_human_draft(&project, BLOCK, &request).unwrap(),
        expected
    );
    if root.join("submission.json").exists() {
        let submission: HumanSubmission =
            serde_json::from_slice(&fs::read(root.join("submission.json")).unwrap()).unwrap();
        assert_eq!(
            service.human_submissions(&project, BLOCK).unwrap(),
            vec![submission.clone()]
        );
        assert_eq!(
            service
                .submit_human_draft(
                    &project,
                    BLOCK,
                    &DraftSubmit {
                        request_id: submission.request_id.clone(),
                        expected_revision: submission.draft.revision.clone(),
                    }
                )
                .unwrap(),
            submission
        );
    }
}

#[test]
fn rejects_bad_ids_and_bases_but_keeps_recoverable_text() {
    let f = Fixture::new();
    let request = f.request();
    for ids in [
        vec!["foreign"],
        vec!["c0001-p0001", "c0001-p0001"],
        vec!["c0001-p0002", "c0001-p0001"],
    ] {
        let mut invalid = request.clone();
        invalid.paragraphs = ids
            .into_iter()
            .map(|id| DraftParagraph {
                id: id.into(),
                text: "x".into(),
                marks: Vec::new(),
            })
            .collect();
        assert!(
            f.service()
                .save_human_draft(&f.project.id, BLOCK, &invalid)
                .is_err()
        );
        assert!(!f.path().exists());
    }
    assert!(
        f.service()
            .save_human_draft(&f.project.id, "../escape", &request)
            .is_err()
    );
    let receipt = f
        .service()
        .save_human_draft(&f.project.id, BLOCK, &request)
        .unwrap();
    let mut next = f.request();
    next.request_id = "changed-basis".into();
    f.service()
        .set_zhupi(&f.project.id, BLOCK, "c0001-p0001", "核对称谓")
        .unwrap();
    assert!(matches!(
        f.service().save_human_draft(&f.project.id, BLOCK, &next),
        Err(TranslationError::DraftBasisChanged)
    ));
    let view = f.service().human_draft(&f.project.id, BLOCK).unwrap();
    assert_ne!(view.basis, view.draft.as_ref().unwrap().basis);
    assert_eq!(view.draft.unwrap().paragraphs, request.paragraphs);
    // A previously successful request remains a receipt lookup, not a new stale write.
    assert_eq!(
        f.service()
            .save_human_draft(&f.project.id, BLOCK, &request)
            .unwrap(),
        receipt
    );
}

#[test]
fn corrupt_future_and_failed_publications_never_reset_the_previous_draft() {
    let f = Fixture::new();
    let request = f.request();
    f.service()
        .save_human_draft(&f.project.id, BLOCK, &request)
        .unwrap();
    let good = fs::read(f.path()).unwrap();
    for (bytes, newer) in [
        (b"{broken".as_slice(), false),
        (
            b"{\"schema_version\":99,\"revision\":1,\"data\":{}}".as_slice(),
            true,
        ),
    ] {
        fs::write(f.path(), bytes).unwrap();
        let result = f.service().human_draft(&f.project.id, BLOCK);
        assert!(
            matches!(
                result,
                Err(TranslationError::Storage(JsonFileError::Newer { .. }))
            ) == newer
        );
        assert!(result.is_err());
        assert!(
            f.service()
                .save_human_draft(&f.project.id, BLOCK, &request)
                .is_err()
        );
        assert_eq!(fs::read(f.path()).unwrap(), bytes);
    }
    fs::write(f.path(), &good).unwrap();
    let mut changed: serde_json::Value = serde_json::from_slice(&good).unwrap();
    changed["data"]["draft"]["paragraphs"][0]["text"] = "unrecorded edit".into();
    fs::write(f.path(), serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(matches!(
        f.service().human_draft(&f.project.id, BLOCK),
        Err(TranslationError::Storage(JsonFileError::Corrupt(_)))
    ));
    fs::write(f.path(), &good).unwrap();
    let mut next = f.request();
    next.request_id = "at-byte-limit".into();
    next.paragraphs[0].text = "字".repeat(256 * 1024 / 3) + "x";
    let at_limit = f
        .service()
        .save_human_draft(&f.project.id, BLOCK, &next)
        .unwrap();
    let good = fs::read(f.path()).unwrap();
    next.expected_revision = Some(at_limit.revision);
    next.request_id = "too-large".into();
    next.paragraphs[0].text.push('y');
    assert!(
        f.service()
            .save_human_draft(&f.project.id, BLOCK, &next)
            .is_err()
    );
    assert_eq!(fs::read(f.path()).unwrap(), good);
    // A real pending managed write blocks draft publication, without erasing text.
    let workspace = Workspace::open(&f.root).unwrap();
    let mut guard = workspace.lock(LockScope::Workspace).unwrap();
    guard.begin().unwrap();
    drop(guard);
    next.paragraphs[0].text = "short valid request".into();
    assert!(
        f.service()
            .save_human_draft(&f.project.id, BLOCK, &next)
            .is_err()
    );
    assert_eq!(fs::read(f.path()).unwrap(), good);
}

#[test]
fn submissions_survive_new_drafts_and_restart_without_becoming_approved() {
    for marked in [false, true] {
        let f = Fixture::new();
        let _session = ziyuan_app::Session::found(ziyuan_storage::EventLog::new(&f.root)).unwrap();
        let events_before = fs::read(f.root.join(".ziyuan/court.log")).unwrap();
        let mut request = f.request();
        request.paragraphs[1].text = "船长莲说：今晚不出海。".into();
        request.paragraphs.push(DraftParagraph {
            id: "c0001-p0003".into(),
            text: "药师莲和船长并非同一人。".into(),
            marks: Vec::new(),
        });
        if marked {
            request = f.marked_request();
        }
        let receipt = f
            .service()
            .save_human_draft(&f.project.id, BLOCK, &request)
            .unwrap();
        let submit = DraftSubmit {
            request_id: "review-3".into(),
            expected_revision: receipt.revision,
        };
        let submission = f
            .service()
            .submit_human_draft(&f.project.id, BLOCK, &submit)
            .unwrap();
        assert_eq!(submission.draft.paragraphs, request.paragraphs);
        let candidate = submission.candidate().unwrap().serialized().unwrap();
        assert_eq!(
            TextProjects::new(&f.root)
                .check(&f.project.id, BLOCK, &candidate)
                .unwrap()
                .valid,
            !marked,
            "marked v2 must remain outside the legacy model candidate protocol"
        );
        let hash: String = ring::digest::digest(&ring::digest::SHA256, &candidate)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(submission.candidate_hash, hash);
        let mut next = f.request();
        next.request_id = "after-submit".into();
        next.paragraphs[0].text = "仍在修改中的下一稿".into();
        let next_receipt = f
            .service()
            .save_human_draft(&f.project.id, BLOCK, &next)
            .unwrap();
        // Historical retry is a receipt lookup, even after material changes.
        f.service()
            .set_zhupi(&f.project.id, BLOCK, "c0001-p0001", "待复核称谓")
            .unwrap();
        let before = fs::read(f.path()).unwrap();
        assert_eq!(
            f.service()
                .submit_human_draft(&f.project.id, BLOCK, &submit)
                .unwrap(),
            submission
        );
        assert!(matches!(
            f.service().submit_human_draft(
                &f.project.id,
                BLOCK,
                &DraftSubmit {
                    expected_revision: next_receipt.revision.clone(),
                    ..submit.clone()
                }
            ),
            Err(TranslationError::DraftRequestConflict)
        ));
        for (name, bytes) in [
            ("request.json", serde_json::to_vec(&next).unwrap()),
            ("receipt.json", serde_json::to_vec(&next_receipt).unwrap()),
            ("submission.json", serde_json::to_vec(&submission).unwrap()),
        ] {
            fs::write(f.root.join(name), bytes).unwrap();
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "draft_process_worker", "--nocapture"])
            .env("ZIYUAN_DRAFT_TEST_ROOT", &f.root)
            .env("ZIYUAN_DRAFT_TEST_PROJECT", &f.project.id)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(f.path()).unwrap(), before);
        assert_eq!(Workspace::open(&f.root).unwrap().generation().unwrap(), 0);
        assert!(
            f.service()
                .read_translation(&f.project.id, BLOCK)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            fs::read(f.root.join(".ziyuan/court.log")).unwrap(),
            events_before
        );
    }
}

#[test]
fn incomplete_stale_and_conflicting_submissions_do_not_publish() {
    let f = Fixture::new();
    let mut request = f.request();
    assert!(
        f.service()
            .submit_human_draft(
                &f.project.id,
                BLOCK,
                &DraftSubmit {
                    request_id: "no-draft".into(),
                    expected_revision: "missing".into(),
                }
            )
            .is_err()
    );
    // Missing paragraph and complete-but-whitespace must both fail the existing gate.
    for paragraphs in [
        vec![request.paragraphs[0].clone()],
        vec![
            request.paragraphs[0].clone(),
            DraftParagraph {
                id: "c0001-p0002".into(),
                text: " \n\t".into(),
                marks: Vec::new(),
            },
            DraftParagraph {
                id: "c0001-p0003".into(),
                text: "药师是另一人".into(),
                marks: Vec::new(),
            },
        ],
    ] {
        request.paragraphs = paragraphs;
        request.request_id.push('x');
        let receipt = f
            .service()
            .save_human_draft(&f.project.id, BLOCK, &request)
            .unwrap();
        let before = fs::read(f.path()).unwrap();
        assert!(
            f.service()
                .submit_human_draft(
                    &f.project.id,
                    BLOCK,
                    &DraftSubmit {
                        request_id: "incomplete".into(),
                        expected_revision: receipt.revision.clone(),
                    }
                )
                .is_err()
        );
        assert!(
            f.service()
                .human_submissions(&f.project.id, BLOCK)
                .unwrap()
                .is_empty()
        );
        assert_eq!(fs::read(f.path()).unwrap(), before);
        request.expected_revision = Some(receipt.revision);
    }
    request.request_id = "complete".into();
    request.paragraphs[1].text = "船长说不出海".into();
    let receipt = f
        .service()
        .save_human_draft(&f.project.id, BLOCK, &request)
        .unwrap();
    let before = fs::read(f.path()).unwrap();
    assert!(matches!(
        f.service().submit_human_draft(
            &f.project.id,
            BLOCK,
            &DraftSubmit {
                request_id: "old-window".into(),
                expected_revision: request.expected_revision.unwrap(),
            }
        ),
        Err(TranslationError::DraftConflict { .. })
    ));
    f.service()
        .set_zhupi(&f.project.id, BLOCK, "c0001-p0001", "材料变化")
        .unwrap();
    let submit = DraftSubmit {
        request_id: "stale".into(),
        expected_revision: receipt.revision,
    };
    assert!(matches!(
        f.service()
            .submit_human_draft(&f.project.id, BLOCK, &submit),
        Err(TranslationError::DraftBasisChanged)
    ));
    let workspace = Workspace::open(&f.root).unwrap();
    let mut guard = workspace.lock(LockScope::Workspace).unwrap();
    guard.begin().unwrap();
    drop(guard);
    assert!(
        f.service()
            .submit_human_draft(&f.project.id, BLOCK, &submit)
            .is_err()
    );
    assert_eq!(fs::read(f.path()).unwrap(), before);
}

#[test]
fn legacy_read_is_non_mutating_and_new_format_blocks_old_readers_and_corrupt_snapshots() {
    let f = Fixture::new();
    let mut request = f.request();
    request.paragraphs[1].text = "船长莲拒绝出航".into();
    request.paragraphs.push(DraftParagraph {
        id: "c0001-p0003".into(),
        text: "药师莲是另一个人".into(),
        marks: Vec::new(),
    });
    let receipt = f
        .service()
        .save_human_draft(&f.project.id, BLOCK, &request)
        .unwrap();
    // Exact legacy shapes: empty marks must not alter old receipt hashes.
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&fs::read(f.path()).unwrap()).unwrap();
    for schema in [1, 2, 3] {
        legacy["schema_version"] = schema.into();
        if schema == 1 {
            legacy["data"]
                .as_object_mut()
                .unwrap()
                .remove("submissions");
        } else {
            legacy["data"]["submissions"] = serde_json::json!({});
        }
        let original = serde_json::to_vec(&legacy).unwrap();
        fs::write(f.path(), &original).unwrap();
        assert!(
            f.service()
                .human_submissions(&f.project.id, BLOCK)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            f.service()
                .save_human_draft(&f.project.id, BLOCK, &request)
                .unwrap(),
            receipt
        );
        assert_eq!(fs::read(f.path()).unwrap(), original);
    }
    let submit = DraftSubmit {
        request_id: "first-v2".into(),
        expected_revision: receipt.revision,
    };
    let submission = f
        .service()
        .submit_human_draft(&f.project.id, BLOCK, &submit)
        .unwrap();
    let good = fs::read(f.path()).unwrap();
    for supported in [1, 2, 3] {
        let old_reader = ziyuan_storage::JsonFile::<serde_json::Value>::new(
            f.path(),
            supported,
            8 * 1024 * 1024,
        );
        assert!(matches!(
            old_reader.load(),
            Err(JsonFileError::Newer { found: 4, .. })
        ));
        let result = old_reader
            .update::<(), JsonFileError>(|_| panic!("old writer must never reach publication"));
        assert!(matches!(result, Err(JsonFileError::Newer { .. })));
    }
    assert_eq!(fs::read(f.path()).unwrap(), good);
    let mut corrupt: serde_json::Value = serde_json::from_slice(&good).unwrap();
    corrupt["data"]["submissions"]["first-v2"]["draft"]["paragraphs"][0]["text"] =
        "undocumented change".into();
    let corrupt = serde_json::to_vec(&corrupt).unwrap();
    fs::write(f.path(), &corrupt).unwrap();
    assert!(matches!(
        f.service().human_submissions(&f.project.id, BLOCK),
        Err(TranslationError::Storage(JsonFileError::Corrupt(_)))
    ));
    assert!(
        f.service()
            .submit_human_draft(&f.project.id, BLOCK, &submit)
            .is_err()
    );
    assert!(
        f.service()
            .save_human_draft(&f.project.id, BLOCK, &request)
            .is_err()
    );
    assert_eq!(fs::read(f.path()).unwrap(), corrupt);
    fs::write(f.path(), &good).unwrap();
    // Failed oversized publication retains both snapshot and original retry receipt.
    request = f.request();
    request.request_id = "large-complete".into();
    request.paragraphs = submission.draft.paragraphs.clone();
    request.paragraphs[0].text = "x"
        .repeat(256 * 1024 - request.paragraphs[1].text.len() - request.paragraphs[2].text.len());
    let large = f
        .service()
        .save_human_draft(&f.project.id, BLOCK, &request)
        .unwrap();
    let mut failures = 0;
    for n in 0..40 {
        let before = fs::read(f.path()).unwrap();
        match f.service().submit_human_draft(
            &f.project.id,
            BLOCK,
            &DraftSubmit {
                request_id: format!("large-{n}"),
                expected_revision: large.revision.clone(),
            },
        ) {
            Ok(_) => {}
            Err(TranslationError::Storage(JsonFileError::TooLarge { .. })) => {
                assert_eq!(fs::read(f.path()).unwrap(), before);
                failures += 1;
                break;
            }
            other => panic!("unexpected submit result: {other:?}"),
        }
    }
    assert_eq!(failures, 1);
    assert_eq!(
        f.service()
            .submit_human_draft(&f.project.id, BLOCK, &submit)
            .unwrap(),
        submission
    );
}

#[test]
fn marked_submissions_are_lossless_and_old_plain_submissions_remain_immutable() {
    let f = Fixture::new();
    let mut marked = f.marked_request();
    let mut plain = marked.clone();
    plain.request_id = "plain-before-markup".into();
    plain.paragraphs[0].marks.clear();
    let first = f
        .service()
        .save_human_draft(&f.project.id, BLOCK, &plain)
        .unwrap();
    let submit = DraftSubmit {
        request_id: "plain-review".into(),
        expected_revision: first.revision.clone(),
    };
    let snapshot = f
        .service()
        .submit_human_draft(&f.project.id, BLOCK, &submit)
        .unwrap();
    // Independently fixed legacy bytes, not a second call to the new serializer.
    let legacy = r#"{
  "schema_version": 1,
  "block_id": "c0001-b0001",
  "paragraphs": [
    {
      "id": "c0001-p0001",
      "text": "甲月舟乙"
    },
    {
      "id": "c0001-p0002",
      "text": "船长莲"
    },
    {
      "id": "c0001-p0003",
      "text": "药师莲"
    }
  ]
}
"#;
    assert_eq!(
        snapshot.candidate().unwrap().serialized().unwrap(),
        legacy.as_bytes()
    );
    marked.expected_revision = Some(first.revision);
    let receipt = f
        .service()
        .save_human_draft(&f.project.id, BLOCK, &marked)
        .unwrap();
    assert_ne!(receipt.content_hash, first.content_hash);
    let draft = f
        .service()
        .human_draft(&f.project.id, BLOCK)
        .unwrap()
        .draft
        .unwrap();
    assert_eq!(draft.paragraphs, marked.paragraphs);
    assert_eq!(&draft.paragraphs[0].text[3..9], "月舟");
    let rich = f
        .service()
        .submit_human_draft(
            &f.project.id,
            BLOCK,
            &DraftSubmit {
                request_id: "rich-review".into(),
                expected_revision: receipt.revision.clone(),
            },
        )
        .unwrap();
    let candidate = rich.candidate().unwrap();
    assert_eq!(candidate.schema_version, 2);
    assert_eq!(candidate.paragraphs, marked.paragraphs);
    let bytes = candidate.serialized().unwrap();
    assert_eq!(bytes.last(), Some(&b'\n'));
    assert_eq!(
        serde_json::from_slice::<ReviewCandidate>(&bytes).unwrap(),
        candidate
    );
    assert_ne!(rich.candidate_hash, snapshot.candidate_hash);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["paragraphs"][0]["marks"],
        serde_json::json!([
            {"start":3,"end":9,"kind":{"type":"ruby","reading":"ねがい"}},
            {"start":3,"end":9,"kind":{"type":"emphasis_dots"}},
            {"start":9,"end":12,"kind":{"type":"emphasis"}},
            {"start":0,"end":3,"kind":{"type":"strong"}}
        ])
    );
    let before = fs::read(f.path()).unwrap();
    assert_eq!(
        f.service()
            .submit_human_draft(&f.project.id, BLOCK, &submit)
            .unwrap(),
        snapshot
    );
    assert_eq!(
        f.service().human_submissions(&f.project.id, BLOCK).unwrap(),
        vec![snapshot, rich.clone()]
    );
    let mut changed = marked.clone();
    changed.paragraphs[0].marks[0].kind = TargetMarkKind::Ruby {
        reading: "別の意味".into(),
    };
    assert!(matches!(
        f.service().save_human_draft(&f.project.id, BLOCK, &changed),
        Err(TranslationError::DraftRequestConflict)
    ));
    assert_eq!(
        f.service()
            .save_human_draft(&f.project.id, BLOCK, &marked)
            .unwrap(),
        receipt
    );
    assert_eq!(fs::read(f.path()).unwrap(), before);
    // Only ruby changes; base text and other marks are identical.
    changed.request_id = "changed-reading".into();
    changed.expected_revision = Some(receipt.revision);
    let updated = f
        .service()
        .save_human_draft(&f.project.id, BLOCK, &changed)
        .unwrap();
    let new_rich = f
        .service()
        .submit_human_draft(
            &f.project.id,
            BLOCK,
            &DraftSubmit {
                request_id: "changed-review".into(),
                expected_revision: updated.revision,
            },
        )
        .unwrap();
    assert_ne!(new_rich.candidate_hash, rich.candidate_hash);
    assert_eq!(
        f.service()
            .submit_human_draft(
                &f.project.id,
                BLOCK,
                &DraftSubmit {
                    request_id: rich.request_id.clone(),
                    expected_revision: rich.draft.revision.clone(),
                }
            )
            .unwrap(),
        rich
    );
    assert!(
        f.service()
            .read_translation(&f.project.id, BLOCK)
            .unwrap()
            .is_none()
    );
    assert_eq!(Workspace::open(&f.root).unwrap().generation().unwrap(), 0);
}

#[test]
fn marked_candidate_format_checks_are_explicit_and_refuse_invalid_material() {
    let f = Fixture::new();
    let mut request = f.marked_request();
    // Literal HTML and delimiters remain data in both base and literary reading.
    request.paragraphs[1].text = "<script>unsafe()</script>｜字《音》".into();
    request.paragraphs[1].marks = vec![TargetMark {
        start: 0,
        end: 8,
        kind: TargetMarkKind::Ruby {
            reading: " <&《》> ".into(),
        },
    }];
    let receipt = f
        .service()
        .save_human_draft(&f.project.id, BLOCK, &request)
        .unwrap();
    let submission = f
        .service()
        .submit_human_draft(
            &f.project.id,
            BLOCK,
            &DraftSubmit {
                request_id: "formats".into(),
                expected_revision: receipt.revision,
            },
        )
        .unwrap();
    let candidate = submission.candidate().unwrap();
    let before = fs::read(f.path()).unwrap();
    let supported = candidate.check_format(ReviewFormat::ReviewJson).unwrap();
    assert!(supported.lossless);
    assert!(supported.unsupported_marks.is_empty());
    for format in [ReviewFormat::LegacyPromptJson, ReviewFormat::PlainText] {
        let report = candidate.check_format(format).unwrap();
        assert!(!report.lossless);
        let locations: Vec<_> = report
            .unsupported_marks
            .iter()
            .map(|m| (m.paragraph_id.as_str(), m.mark_index))
            .collect();
        assert_eq!(
            locations,
            vec![
                ("c0001-p0001", 0),
                ("c0001-p0001", 1),
                ("c0001-p0001", 2),
                ("c0001-p0001", 3),
                ("c0001-p0002", 0),
            ]
        );
    }
    let decoded: ReviewCandidate =
        serde_json::from_slice(&candidate.serialized().unwrap()).unwrap();
    assert_eq!(decoded.paragraphs, request.paragraphs);
    let mut plain = candidate.clone();
    for p in &mut plain.paragraphs {
        p.marks.clear();
    }
    plain.schema_version = 1;
    for format in [
        ReviewFormat::ReviewJson,
        ReviewFormat::LegacyPromptJson,
        ReviewFormat::PlainText,
    ] {
        assert!(plain.check_format(format).unwrap().lossless);
    }
    for version in [0, 1, 3, u32::MAX] {
        let mut invalid = candidate.clone();
        invalid.schema_version = version;
        assert!(invalid.serialized().is_err());
        assert!(invalid.check_format(ReviewFormat::ReviewJson).is_err());
    }
    let mut invalid = candidate.clone();
    invalid.paragraphs[0].marks[0].start = 4; // Splits 月 in UTF-8.
    assert!(invalid.serialized().is_err());
    assert!(invalid.check_format(ReviewFormat::ReviewJson).is_err());
    let mut unknown = serde_json::to_value(&candidate).unwrap();
    unknown["paragraphs"][0]["marks"][0]["kind"]["position"] = "above".into();
    assert!(serde_json::from_value::<ReviewCandidate>(unknown).is_err());
    assert_eq!(fs::read(f.path()).unwrap(), before);
    // A corrupted candidate digest cannot be hidden by intact draft receipts.
    let mut corrupt: serde_json::Value = serde_json::from_slice(&before).unwrap();
    corrupt["data"]["submissions"]["formats"]["candidate_hash"] = "0".repeat(64).into();
    let corrupt = serde_json::to_vec(&corrupt).unwrap();
    fs::write(f.path(), &corrupt).unwrap();
    assert!(matches!(
        f.service().human_submissions(&f.project.id, BLOCK),
        Err(TranslationError::Storage(JsonFileError::Corrupt(_)))
    ));
    assert!(
        f.service()
            .save_human_draft(&f.project.id, BLOCK, &request)
            .is_err()
    );
    assert_eq!(fs::read(f.path()).unwrap(), corrupt);
}

#[test]
fn incomplete_marked_submissions_retain_drafts_and_do_not_publish() {
    let f = Fixture::new();
    let complete = f.marked_request();
    for n in 0..4 {
        let mut request = complete.clone();
        request.request_id = format!("unfinished-{n}");
        request.expected_revision = f
            .service()
            .human_draft(&f.project.id, BLOCK)
            .unwrap()
            .draft
            .map(|d| d.revision);
        match n {
            0 | 1 => {
                request.paragraphs[0].marks[0].kind = TargetMarkKind::Ruby {
                    reading: if n == 0 { "" } else { " \t　" }.into(),
                }
            }
            2 => {
                request.paragraphs.pop();
            }
            _ => request.paragraphs[2].text = "　 \t".into(),
        }
        let receipt = f
            .service()
            .save_human_draft(&f.project.id, BLOCK, &request)
            .unwrap();
        let before = fs::read(f.path()).unwrap();
        assert!(
            f.service()
                .submit_human_draft(
                    &f.project.id,
                    BLOCK,
                    &DraftSubmit {
                        request_id: "never-published".into(),
                        expected_revision: receipt.revision,
                    }
                )
                .is_err()
        );
        let draft = f
            .service()
            .human_draft(&f.project.id, BLOCK)
            .unwrap()
            .draft
            .unwrap();
        assert_eq!(draft.paragraphs, request.paragraphs);
        if n < 2 {
            let incomplete = ReviewCandidate {
                schema_version: 2,
                block_id: BLOCK.into(),
                paragraphs: draft.paragraphs,
            };
            assert!(incomplete.serialized().is_err());
            assert!(incomplete.check_format(ReviewFormat::ReviewJson).is_err());
        }
        assert!(
            f.service()
                .human_submissions(&f.project.id, BLOCK)
                .unwrap()
                .is_empty()
        );
        assert_eq!(fs::read(f.path()).unwrap(), before);
    }
}

#[test]
fn mark_only_concurrent_edits_conflict_instead_of_overwriting_each_other() {
    let f = Fixture::new();
    let mut request = f.marked_request();
    let first = f
        .service()
        .save_human_draft(&f.project.id, BLOCK, &request)
        .unwrap();
    request.expected_revision = Some(first.revision);
    let barrier = Arc::new(Barrier::new(2));
    let requests: Vec<_> = (0..2)
        .map(|n| {
            let mut next = request.clone();
            next.request_id = format!("marks-window-{n}");
            if n == 0 {
                next.paragraphs[0].marks[0].kind = TargetMarkKind::Ruby {
                    reading: "つきぶね".into(),
                };
            } else {
                next.paragraphs[0].marks.remove(1);
            }
            next
        })
        .collect();
    let threads: Vec<_> = requests
        .iter()
        .cloned()
        .map(|next| {
            let root = f.root.clone();
            let project = f.project.id.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                Translations::new(root).save_human_draft(&project, BLOCK, &next)
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(TranslationError::DraftConflict { .. })))
            .count(),
        1
    );
    let winner = results.iter().position(|r| r.is_ok()).unwrap();
    assert_ne!(
        results[winner].as_ref().unwrap().content_hash,
        first.content_hash
    );
    assert_eq!(
        f.service()
            .human_draft(&f.project.id, BLOCK)
            .unwrap()
            .draft
            .unwrap()
            .paragraphs,
        requests[winner].paragraphs
    );
    let before = fs::read(f.path()).unwrap();
    assert!(matches!(
        f.service().save_human_draft(&f.project.id, BLOCK, &request),
        Err(TranslationError::DraftRequestConflict)
    ));
    assert_eq!(fs::read(f.path()).unwrap(), before);
}

#[test]
fn target_markup_rejects_bad_boundaries_overlaps_and_unknown_shapes_without_writing() {
    use serde_json::json;
    let f = Fixture::new();
    for (case, (text, marks, valid)) in [
        ("甲乙丙", json!([{"start":0,"end":3,"kind":{"type":"strong"}}, {"start":3,"end":9,"kind":{"type":"emphasis"}}]), true),
        ("e\u{301}", json!([{"start":0,"end":3,"kind":{"type":"emphasis_dots"}}]), true),
        ("👩‍👧", json!([{"start":0,"end":11,"kind":{"type":"ruby","reading":"おやこ"}}]), true),
        ("甲乙丙", json!([{"start":1,"end":3,"kind":{"type":"strong"}}]), false),
        ("甲乙丙", json!([{"start":3,"end":3,"kind":{"type":"strong"}}]), false),
        ("甲乙丙", json!([{"start":6,"end":3,"kind":{"type":"strong"}}]), false),
        ("甲乙丙", json!([{"start":0,"end":99,"kind":{"type":"strong"}}]), false),
        ("e\u{301}", json!([{"start":0,"end":1,"kind":{"type":"strong"}}]), false),
        ("👩‍👧", json!([{"start":0,"end":4,"kind":{"type":"strong"}}]), false),
        ("甲\n乙", json!([{"start":0,"end":7,"kind":{"type":"strong"}}]), false),
        ("甲乙丙", json!([{"start":0,"end":3,"kind":{"type":"ruby","reading":"か\nな"}}]), false),
        ("甲乙丙", json!([{"start":0,"end":6,"kind":{"type":"strong"}}, {"start":3,"end":9,"kind":{"type":"emphasis"}}]), false),
        ("甲乙丙", json!([{"start":0,"end":9,"kind":{"type":"ruby","reading":"甲乙丙"}}, {"start":3,"end":6,"kind":{"type":"emphasis_dots"}}]), false),
        ("甲乙丙", json!([{"start":0,"end":3,"kind":{"type":"ruby","reading":"a"}}, {"start":0,"end":3,"kind":{"type":"strong"}}, {"start":0,"end":3,"kind":{"type":"ruby","reading":"b"}}]), false),
        ("甲乙丙", json!([{"start":0,"end":3,"kind":{"type":"underline"}}]), false),
        ("甲乙丙", json!([{"start":0,"end":3,"kind":{"type":"strong","reading":"unexpected"}}]), false),
        ("甲乙丙", json!([{"start":0,"end":3,"byte_start":25,"kind":{"type":"strong"}}]), false),
        ("甲乙丙", json!([{"start":0,"end":3,"kind":{"type":"ruby","reading":"a","children":[]}}]), false),
    ].into_iter().enumerate() {
        let before = fs::read(f.path()).ok();
        let mut input = serde_json::to_value(f.request()).unwrap();
        input["request_id"] = format!("case-{case}").into();
        input["paragraphs"][0]["text"] = text.into();
        input["paragraphs"][0]["marks"] = marks;
        let result = serde_json::from_value::<DraftSave>(input).map_err(|e| e.to_string())
            .and_then(|r| f.service().save_human_draft(&f.project.id, BLOCK, &r).map_err(|e| e.to_string()));
        assert_eq!(result.is_ok(), valid, "case={case}, text={text:?}: {result:?}");
        if !valid { assert_eq!(fs::read(f.path()).ok(), before); }
    }
}

#[test]
fn markup_limits_include_readings_and_all_paragraph_marks_and_pending_writes() {
    let f = Fixture::new();
    let mut request = f.marked_request();
    // 12 + 9 + 9 bytes of base text; reading alone is below the bound on both sides.
    request.paragraphs[0].marks[0].kind = TargetMarkKind::Ruby {
        reading: "x".repeat(256 * 1024 - 30),
    };
    let first = f
        .service()
        .save_human_draft(&f.project.id, BLOCK, &request)
        .unwrap();
    let before = fs::read(f.path()).unwrap();
    request.expected_revision = Some(first.revision);
    request.request_id = "reading-over-limit".into();
    if let TargetMarkKind::Ruby { reading } = &mut request.paragraphs[0].marks[0].kind {
        reading.push('字');
    }
    assert!(
        f.service()
            .save_human_draft(&f.project.id, BLOCK, &request)
            .is_err()
    );
    assert_eq!(fs::read(f.path()).unwrap(), before);
    request.request_id = "nodes-at-limit".into();
    request.paragraphs[0].text = "x".repeat(2048);
    request.paragraphs[1].text = "y".repeat(2049);
    for paragraph in &mut request.paragraphs[..2] {
        paragraph.marks = (0..2048)
            .map(|start| TargetMark {
                start,
                end: start + 1,
                kind: TargetMarkKind::EmphasisDots {},
            })
            .collect();
    }
    let at_limit = f
        .service()
        .save_human_draft(&f.project.id, BLOCK, &request)
        .unwrap();
    let before = fs::read(f.path()).unwrap();
    request.expected_revision = Some(at_limit.revision);
    request.request_id = "nodes-over-limit".into();
    request.paragraphs[1].marks.push(TargetMark {
        start: 2048,
        end: 2049,
        kind: TargetMarkKind::Strong {},
    });
    assert!(
        f.service()
            .save_human_draft(&f.project.id, BLOCK, &request)
            .is_err()
    );
    assert_eq!(fs::read(f.path()).unwrap(), before);
    request.paragraphs[1].marks.pop();
    let workspace = Workspace::open(&f.root).unwrap();
    let mut guard = workspace.lock(LockScope::Workspace).unwrap();
    guard.begin().unwrap();
    drop(guard);
    assert!(
        f.service()
            .save_human_draft(&f.project.id, BLOCK, &request)
            .is_err()
    );
    assert_eq!(fs::read(f.path()).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn static_symlinks_do_not_redirect_draft_writes() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let request = f.request();
    let sentinel = f.root.join("sentinel");
    fs::write(&sentinel, "untouched").unwrap();
    for path in [
        f.path(),
        f.path().with_file_name(format!("{BLOCK}.json.lock")),
    ] {
        symlink(&sentinel, &path).unwrap();
        assert!(
            f.service()
                .save_human_draft(&f.project.id, BLOCK, &request)
                .is_err()
        );
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "untouched");
        fs::remove_file(path).unwrap();
    }
}

#[test]
fn approved_targets_keep_plain_bytes_and_refuse_unknown_formats() {
    use ziyuan_app::text_project::{BlockParagraph, PromptBlock};
    use ziyuan_app::translation::{
        ApprovedTarget, DraftParagraph, ReviewCandidate, TARGET_FORMAT_MARKED, TARGET_FORMAT_PLAIN,
        TargetMark, TargetMarkKind, read_approved_target,
    };
    let block = "c0001-b0001";
    let plain = ReviewCandidate {
        schema_version: 1,
        block_id: block.into(),
        paragraphs: vec![DraftParagraph {
            id: "p1".into(),
            text: "夜".into(),
            marks: Vec::new(),
        }],
    };
    let target = plain.approved_target().unwrap();
    assert_eq!(target.format(), TARGET_FORMAT_PLAIN);
    let legacy = PromptBlock {
        schema_version: 1,
        block_id: block.into(),
        paragraphs: vec![BlockParagraph {
            id: "p1".into(),
            text: "夜".into(),
            inlines: Vec::new(),
        }],
    }
    .serialized()
    .unwrap();
    let bytes = target.bytes().unwrap();
    assert_eq!(bytes, legacy, "plain targets must keep old zh.json bytes");
    assert_eq!(read_approved_target(&bytes, block).unwrap(), target);
    assert!(read_approved_target(&bytes, "c0002-b0001").is_err());

    let marked = ReviewCandidate {
        schema_version: 2,
        block_id: block.into(),
        paragraphs: vec![DraftParagraph {
            id: "p1".into(),
            text: "夜が明ける".into(),
            marks: vec![TargetMark {
                start: 0,
                end: 3,
                kind: TargetMarkKind::Ruby {
                    reading: "よ".into(),
                },
            }],
        }],
    };
    let target = marked.approved_target().unwrap();
    assert_eq!(target.format(), TARGET_FORMAT_MARKED);
    let bytes = target.bytes().unwrap();
    assert_eq!(bytes, marked.serialized().unwrap());
    match read_approved_target(&bytes, block).unwrap() {
        ApprovedTarget::Marked(read) => assert_eq!(read, marked),
        ApprovedTarget::Plain(_) => panic!("marked target must not be downgraded"),
    }

    let mixed = serde_json::json!({
        "schema_version": 1,
        "block_id": block,
        "paragraphs": [{"id": "p1", "text": "夜", "marks": []}],
    })
    .to_string();
    assert!(
        read_approved_target(mixed.as_bytes(), block).is_err(),
        "v1 must not carry marks, even empty ones"
    );

    let empty = serde_json::json!({
        "schema_version": 2,
        "block_id": block,
        "paragraphs": [{"id": "p1", "text": "夜"}],
    })
    .to_string();
    assert!(
        read_approved_target(empty.as_bytes(), block).is_err(),
        "v2 without marks is not a canonical target"
    );

    let future = serde_json::json!({
        "schema_version": 3,
        "block_id": block,
        "paragraphs": [],
    })
    .to_string();
    let error = read_approved_target(future.as_bytes(), block)
        .unwrap_err()
        .to_string();
    assert!(error.contains("第 3 版"), "{error}");

    let unknown = serde_json::json!({
        "schema_version": 2,
        "block_id": block,
        "paragraphs": [{"id": "p1", "text": "夜", "marks": [], "extra": 1}],
    })
    .to_string();
    assert!(read_approved_target(unknown.as_bytes(), block).is_err());
}

#[test]
fn markdown_capability_reports_only_losslessly_expressible_marks() {
    use ziyuan_app::translation::{
        DraftParagraph, ReviewCandidate, ReviewFormat, TargetMark, TargetMarkKind,
    };
    let block = "c0001-b0001";
    let ruby = ReviewCandidate {
        schema_version: 2,
        block_id: block.into(),
        paragraphs: vec![DraftParagraph {
            id: "p1".into(),
            text: "夜が明ける".into(),
            marks: vec![TargetMark {
                start: 0,
                end: 3,
                kind: TargetMarkKind::Ruby {
                    reading: "よ".into(),
                },
            }],
        }],
    };
    assert!(
        ruby.check_format(ReviewFormat::MarkdownHtml)
            .unwrap()
            .lossless
    );
    assert!(!ruby.check_format(ReviewFormat::PlainText).unwrap().lossless);
    assert!(
        ruby.check_format(ReviewFormat::ReviewJson)
            .unwrap()
            .lossless
    );

    let dotted = ReviewCandidate {
        schema_version: 2,
        block_id: block.into(),
        paragraphs: vec![DraftParagraph {
            id: "p1".into(),
            text: "夜".into(),
            marks: vec![TargetMark {
                start: 0,
                end: 3,
                kind: TargetMarkKind::EmphasisDots {},
            }],
        }],
    };
    let check = dotted.check_format(ReviewFormat::MarkdownHtml).unwrap();
    assert!(!check.lossless);
    assert_eq!(check.unsupported_marks.len(), 1);

    let stacked = ReviewCandidate {
        schema_version: 2,
        block_id: block.into(),
        paragraphs: vec![DraftParagraph {
            id: "p1".into(),
            text: "夜".into(),
            marks: vec![
                TargetMark {
                    start: 0,
                    end: 3,
                    kind: TargetMarkKind::Ruby {
                        reading: "よ".into(),
                    },
                },
                TargetMark {
                    start: 0,
                    end: 3,
                    kind: TargetMarkKind::Strong {},
                },
            ],
        }],
    };
    assert!(
        !stacked
            .check_format(ReviewFormat::MarkdownHtml)
            .unwrap()
            .lossless
    );
}

/// Approval in one managed transaction: helpers drive the public API exactly as the CLI does.
mod approval {
    use super::*;
    use ziyuan_app::translation::{
        TARGET_FORMAT_MARKED, TARGET_FORMAT_PLAIN, VersionState, human_approve_request_id,
        human_submit_request_id, read_approved_target,
    };
    use ziyuan_core::{HumanApproval, HumanSubmissionBinding};
    use ziyuan_storage::{EventStore, WriteGuard};

    fn founded(f: &Fixture) {
        if EventLog::new(&f.root).load().unwrap().is_empty() {
            drop(ziyuan_app::Session::found(EventLog::new(&f.root)).unwrap());
        }
    }

    fn guard(f: &Fixture) -> WriteGuard {
        let workspace = Workspace::open(&f.root).unwrap();
        let mut guard = workspace.lock(LockScope::Workspace).unwrap();
        guard.upgrade_write_protocol().unwrap();
        TextProjects::new(&f.root)
            .upgrade_project_protocol(&f.project.id, &guard)
            .unwrap();
        guard
    }

    fn plain_request(f: &Fixture) -> DraftSave {
        let mut request = f.marked_request();
        for paragraph in &mut request.paragraphs {
            paragraph.marks.clear();
        }
        request
    }

    #[test]
    fn signed_off_srt_export_is_exact_and_refuses_overwrite_and_foreign_ids() {
        let f = Fixture::new();
        let revision = save(&f, plain_request(&f), "srt-final");
        submit(&f, &revision);
        approve(&f).unwrap();
        finalize(&f).unwrap();
        let cues=f.project.chapters[0].paragraphs.iter().enumerate().map(|(i,p)|serde_json::json!({"index":i+1,"start":format!("00:00:{:02},000",i*2),"end":format!("00:00:{:02},000",i*2+1),"text":p.text})).collect::<Vec<_>>();
        fs::write(
            f.root
                .join(".ziyuan/text-projects")
                .join(&f.project.id)
                .join("srt.json"),
            serde_json::to_vec(&cues).unwrap(),
        )
        .unwrap();
        assert_eq!(f.service().tm_build(&f.project.id).unwrap(), 3);
        let output = f.root.join("finished.srt");
        assert_eq!(f.service().export_srt(&f.project.id, &output).unwrap(), 3);
        let bytes = fs::read(&output).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains("00:00:02,000 --> 00:00:03,000"));
        assert!(text.contains("船长莲"));
        assert!(f.service().export_srt(&f.project.id, &output).is_err());
        assert_eq!(fs::read(&output).unwrap(), bytes);
        let path = f.service().block_dir(&f.project.id, BLOCK).join("zh.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["paragraphs"][0]["id"] = serde_json::json!("foreign");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(
            f.service()
                .export_srt(&f.project.id, &f.root.join("bad.srt"))
                .is_err()
        );
        assert!(!f.root.join("bad.srt").exists());
    }

    #[test]
    fn finished_html_keeps_ruby_emphasis_dots_and_overlapping_styles_without_active_content() {
        let f = Fixture::new();
        let mut request = f.marked_request();
        request.paragraphs[0].marks.push(TargetMark {
            start: 3,
            end: 9,
            kind: TargetMarkKind::Strong {},
        });
        let revision = save(&f, request, "html-export");
        submit(&f, &revision);
        approve(&f).unwrap();
        finalize(&f).unwrap();
        let mut session = ziyuan_app::Session::open(EventLog::new(&f.root)).unwrap();
        let options = ziyuan_app::translation::ExportOptions {
            format: ziyuan_app::translation::ExportFormat::Html,
            ..Default::default()
        };
        let bundle = session
            .export_finished(&f.root, &f.project.id, &options)
            .unwrap();
        let text = String::from_utf8(bundle.files[0].bytes.clone()).unwrap();
        assert!(bundle.files[0].name.ends_with(".html"));
        assert!(text.contains("<ruby>"));
        assert!(text.contains("<strong>"));
        assert!(text.contains("text-emphasis"));
        assert!(!text.contains("<script"));
        assert!(
            f.service()
                .delivery_preflight(&f.project.id, &options)
                .unwrap()
                .export_ready
        );
    }

    fn save(f: &Fixture, mut request: DraftSave, id: &str) -> String {
        let view = f.service().human_draft(&f.project.id, BLOCK).unwrap();
        request.request_id = id.into();
        request.expected_revision = view.draft.map(|d| d.revision);
        request.basis = view.basis;
        f.service()
            .save_human_draft(&f.project.id, BLOCK, &request)
            .unwrap()
            .revision
    }

    fn prepare(f: &Fixture, revision: &str) -> HumanSubmissionBinding {
        founded(f);
        let id = human_submit_request_id(revision);
        f.service()
            .prepare_human_submission(&f.project.id, BLOCK, &id, &id, revision)
            .unwrap()
    }

    fn submit(f: &Fixture, revision: &str) -> HumanSubmissionBinding {
        let binding = prepare(f, revision);
        let guard = guard(f);
        assert!(f.service().publish_human_binding(&guard, &binding).unwrap());
        binding
    }

    /// Publish a binding the admission path would refuse, to reach the approval's own checks.
    fn force_bind(f: &Fixture, binding: HumanSubmissionBinding) {
        let mut log = EventLog::new(&f.root);
        let history = log.load().unwrap();
        let court = ziyuan_governance::Court::replay(&history).unwrap();
        let event = court.propose_human_submission_binding(binding).unwrap();
        log.append_if_unchanged(&history, &[event]).unwrap();
    }

    fn reseal(binding: &mut HumanSubmissionBinding) {
        binding.request.candidate_hash =
            ziyuan_storage::content_sha256(binding.request.candidate.as_bytes());
        binding.request_hash = ziyuan_governance::human_binding_request_hash(&binding.request);
    }

    fn approve(f: &Fixture) -> Result<bool, TranslationError> {
        let mut guard = guard(f);
        f.service()
            .approve_human_submission(&mut guard, &f.project.id, BLOCK)
    }

    fn finalize(f: &Fixture) -> Result<bool, TranslationError> {
        let guard = guard(f);
        f.service()
            .finalize_human_translation(&guard, &f.project.id, BLOCK)
    }

    #[test]
    fn full_backup_preserves_marked_drafts_approvals_and_export_gate_after_migration() {
        let f = Fixture::new();
        let revision = save(&f, f.marked_request(), "backup-marked");
        submit(&f, &revision);
        approve(&f).unwrap();
        finalize(&f).unwrap();
        let bundle = f.service().backup_workspace().unwrap();
        let destination = f.root.join("migrated");
        ziyuan_app::translation::restore_backup(&bundle.bytes, &destination).unwrap();
        let restored = Translations::new(&destination);
        assert_eq!(
            restored.human_draft(&f.project.id, BLOCK).unwrap().draft,
            f.service().human_draft(&f.project.id, BLOCK).unwrap().draft
        );
        let mut session = ziyuan_app::Session::open(EventLog::new(&destination)).unwrap();
        assert!(matches!(
            session
                .translation_status(&destination, &f.project.id)
                .unwrap()[0]
                .state,
            ziyuan_app::translation::BlockState::HumanFinalized { .. }
        ));
        assert!(
            session
                .export_finished(&destination, &f.project.id, &Default::default())
                .is_err(),
            "不支持的文学标记仍须拒绝导出，不能因迁机丢失标记而放行"
        );
    }

    #[test]
    fn source_revision_keeps_approved_translation_but_invalidates_its_signoff() {
        let f = Fixture::new();
        let revision = save(&f, plain_request(&f), "source-revision");
        submit(&f, &revision);
        approve(&f).unwrap();
        finalize(&f).unwrap();
        let before = target(&f).unwrap();
        let original = f.project.chapters[0]
            .paragraphs
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let input = ziyuan_app::translation::SourceUpdate {
            chapter: Some("c0001".into()),
            name: "revision.txt".into(),
            text: original.replace("灯台", "港町"),
            package_base64: None,
        };
        let preview = f
            .service()
            .preview_source_update(&f.project.id, &input)
            .unwrap();
        f.service()
            .apply_source_update(&f.project.id, &input, &preview.fingerprint)
            .unwrap();
        assert_eq!(target(&f).unwrap(), before);
        let session = ziyuan_app::Session::open(EventLog::new(&f.root)).unwrap();
        assert!(matches!(
            session.translation_status(&f.root, &f.project.id).unwrap()[0].state,
            ziyuan_app::translation::BlockState::HumanStale { .. }
        ));
    }

    #[test]
    fn delivery_preflight_separates_signoff_from_open_and_stale_issues() {
        let f = Fixture::new();
        let revision = save(&f, plain_request(&f), "delivery");
        submit(&f, &revision);
        approve(&f).unwrap();
        finalize(&f).unwrap();
        let report = f
            .service()
            .delivery_preflight(&f.project.id, &Default::default())
            .unwrap();
        assert!(report.export_ready, "{:?}", report.blockers);
        let list = f.service().issues(&f.project.id).unwrap();
        let input = ziyuan_app::translation::IssueInput {
            paragraph: "c0001-p0001".into(),
            related: vec!["c0001-p0002".into()],
            rule: "称呼".into(),
            body: "自编待查".into(),
        };
        f.service()
            .add_issue(&f.project.id, &input, &list.fingerprint)
            .unwrap();
        let report = f
            .service()
            .delivery_preflight(&f.project.id, &Default::default())
            .unwrap();
        assert!(report.export_ready);
        assert_eq!(report.open_issues, 1);
        assert!(!report.warnings.is_empty());
    }

    fn target(f: &Fixture) -> Option<Vec<u8>> {
        f.service().read_translation(&f.project.id, BLOCK).unwrap()
    }

    fn generation(f: &Fixture) -> u64 {
        Workspace::open(&f.root).unwrap().generation().unwrap()
    }

    /// Byte snapshot of everything an approval may touch.
    fn evidence(f: &Fixture) -> Vec<Option<Vec<u8>>> {
        [
            f.root.join(".ziyuan/court.log"),
            f.root.join(".ziyuan/write.generation"),
            f.root.join(".ziyuan/recovery.json"),
            f.root
                .join("translations")
                .join(&f.project.id)
                .join(BLOCK)
                .join("zh.json"),
        ]
        .iter()
        .map(|path| fs::read(path).ok())
        .collect()
    }

    fn approval(f: &Fixture, binding: &HumanSubmissionBinding) -> HumanApproval {
        let session = ziyuan_app::Session::open(EventLog::new(&f.root)).unwrap();
        session
            .court()
            .human_approval(
                &f.project.id,
                BLOCK,
                &human_approve_request_id(&binding.request.request_id),
            )
            .unwrap()
            .clone()
    }

    #[test]
    fn auto_snapshots_record_draft_approval_and_finalization() {
        let f = Fixture::new();
        let revision = save(&f, plain_request(&f), "save-snapshot");
        let versions = f
            .service()
            .list_translation_versions(&f.project.id, BLOCK)
            .unwrap();
        assert!(versions.iter().any(|v| v.state == VersionState::Draft));

        submit(&f, &revision);
        assert!(approve(&f).unwrap());
        assert!(finalize(&f).unwrap());

        let versions = f
            .service()
            .list_translation_versions(&f.project.id, BLOCK)
            .unwrap();
        assert!(versions.iter().any(|v| v.state == VersionState::Draft));
        assert!(versions.iter().any(|v| v.state == VersionState::Approved));
        assert!(versions.iter().any(|v| v.state == VersionState::Finalized));
        // 草稿与准奏译文相同：自动快照可逐段比较且没有差异。
        let draft = versions
            .iter()
            .find(|v| v.state == VersionState::Draft)
            .unwrap();
        let changes = f
            .service()
            .compare_version(&f.project.id, BLOCK, &draft.id, true)
            .unwrap();
        assert!(changes.is_empty(), "{changes:?}");
    }

    #[test]
    fn approval_lands_plain_and_marked_targets_and_new_rounds_finalize_again() {
        let f = Fixture::new();
        // Round 1: plain text lands as the first format, byte-compatible with legacy zh.json.
        let revision = save(&f, plain_request(&f), "save-a");
        let binding = submit(&f, &revision);
        assert_eq!(binding.request.request_id, format!("submit-r{revision}"));
        let generation_before = generation(&f);
        assert!(approve(&f).unwrap());
        let bytes = target(&f).expect("approval writes zh.json");
        let expected = ReviewCandidate::from_binding(&binding)
            .unwrap()
            .approved_target()
            .unwrap();
        assert_eq!(bytes, expected.bytes().unwrap());
        assert_eq!(
            read_approved_target(&bytes, BLOCK).unwrap().format(),
            TARGET_FORMAT_PLAIN
        );
        assert_eq!(generation(&f), generation_before + 2);
        assert!(!f.root.join(".ziyuan/recovery.json").exists());
        let first = approval(&f, &binding);
        assert_eq!(first.request.target.before, None);
        assert_eq!(
            first.request.target.after,
            ziyuan_storage::content_sha256(&bytes)
        );
        let settled = evidence(&f);
        assert!(!approve(&f).unwrap(), "same round is idempotent");
        assert_eq!(evidence(&f), settled, "idempotent approval writes nothing");
        assert!(finalize(&f).unwrap());
        assert!(!finalize(&f).unwrap());

        // Round 2: a marked draft under a new revision replaces the target and finalizes anew.
        let revision = save(&f, f.marked_request(), "save-b");
        let binding = submit(&f, &revision);
        assert!(approve(&f).unwrap());
        let marked = target(&f).unwrap();
        assert_eq!(marked, binding.request.candidate.as_bytes());
        assert_eq!(
            read_approved_target(&marked, BLOCK).unwrap().format(),
            TARGET_FORMAT_MARKED
        );
        let second = approval(&f, &binding);
        assert_eq!(
            second.request.target.before,
            Some(ziyuan_storage::content_sha256(&bytes))
        );
        assert!(
            finalize(&f).unwrap(),
            "an earlier round's signoff gives way"
        );
        assert!(!finalize(&f).unwrap());

        // Round 3: same bytes again; zero targets, zh.json untouched, still a new approval.
        let revision = save(&f, f.marked_request(), "save-c");
        let binding = submit(&f, &revision);
        let generation_before = generation(&f);
        assert!(approve(&f).unwrap());
        assert_eq!(target(&f).unwrap(), marked);
        assert_eq!(generation(&f), generation_before + 2);
        let third = approval(&f, &binding);
        assert_eq!(
            third.request.target.before.as_deref(),
            Some(third.request.target.after.as_str())
        );
        assert!(finalize(&f).unwrap());
    }

    #[test]
    fn unmarked_second_version_candidate_lands_as_the_first_format() {
        let f = Fixture::new();
        let revision = save(&f, plain_request(&f), "save-a");
        let plain = prepare(&f, &revision);
        let mut candidate = ReviewCandidate::from_binding(&plain).unwrap();
        candidate.schema_version = 2;
        let mut binding = plain.clone();
        binding.request.candidate = String::from_utf8(candidate.serialized().unwrap()).unwrap();
        reseal(&mut binding);
        assert_ne!(binding.request.candidate, plain.request.candidate);
        force_bind(&f, binding.clone());
        assert!(approve(&f).unwrap());
        let bytes = target(&f).unwrap();
        let legacy = ReviewCandidate::from_binding(&plain)
            .unwrap()
            .approved_target()
            .unwrap()
            .bytes()
            .unwrap();
        assert_eq!(bytes, legacy, "an unmarked v2 candidate keeps legacy bytes");
        assert_eq!(
            approval(&f, &binding).request.target.format,
            TARGET_FORMAT_PLAIN
        );
        assert!(finalize(&f).unwrap());
    }

    #[test]
    fn approval_refuses_changed_disk_or_material_without_writing() {
        let f = Fixture::new();
        let revision = save(&f, plain_request(&f), "save-a");
        submit(&f, &revision);
        let dir = f.root.join("translations").join(&f.project.id).join(BLOCK);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("zh.json"), b"outside edit\n").unwrap();
        let before = evidence(&f);
        let error = approve(&f).unwrap_err().to_string();
        assert!(error.contains("已被改动"), "{error}");
        assert!(error.contains("ziyuan text human submit"), "{error}");
        assert_eq!(evidence(&f), before);
        fs::remove_file(dir.join("zh.json")).unwrap();

        f.service()
            .set_zhupi(&f.project.id, BLOCK, "c0001-p0001", "朱批")
            .unwrap();
        let before = evidence(&f);
        let error = approve(&f).unwrap_err().to_string();
        assert!(error.contains("朱批已变"), "{error}");
        assert_eq!(evidence(&f), before);
    }

    #[test]
    fn approval_refuses_a_binding_whose_predecessor_was_replaced() {
        let f = Fixture::new();
        let revision = save(&f, plain_request(&f), "save-a");
        submit(&f, &revision);
        assert!(approve(&f).unwrap());
        // A binding that still names the old source after another approval landed.
        let revision = save(&f, f.marked_request(), "save-b");
        let mut binding = prepare(&f, &revision);
        assert_ne!(
            binding.request.previous,
            ziyuan_core::TranslationSource::None {}
        );
        binding.request.previous = ziyuan_core::TranslationSource::None {};
        reseal(&mut binding);
        force_bind(&f, binding);
        let before = evidence(&f);
        let error = approve(&f).unwrap_err().to_string();
        assert!(error.contains("当前来源已变"), "{error}");
        assert_eq!(evidence(&f), before);
    }

    #[test]
    fn approval_refuses_an_unfinished_log_line_before_reserving() {
        let f = Fixture::new();
        let revision = save(&f, plain_request(&f), "save-a");
        submit(&f, &revision);
        let log = f.root.join(".ziyuan/court.log");
        let mut bytes = fs::read(&log).unwrap();
        bytes.extend_from_slice(b"v1\tCLOCK");
        fs::write(&log, &bytes).unwrap();
        let before = evidence(&f);
        assert!(approve(&f).is_err());
        assert_eq!(evidence(&f), before);
    }
}
