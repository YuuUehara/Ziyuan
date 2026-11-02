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
    translation::{DraftParagraph, DraftSave, TranslationError, Translations},
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
                },
                DraftParagraph {
                    id: "c0001-p0002".into(),
                    text: String::new(),
                },
            ],
        }
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
fn process_restart_preserves_text_and_original_retry_receipt() {
    let f = Fixture::new();
    let request = f.request();
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
