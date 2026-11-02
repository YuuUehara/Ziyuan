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
use ziyuan_storage::{JsonFileError, LockScope, Workspace};

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
