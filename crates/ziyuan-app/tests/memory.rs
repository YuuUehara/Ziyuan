//! Public-API regressions: caches cannot turn reopened or edited work into references.
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use ziyuan_app::Session;
use ziyuan_app::text_project::{NewProject, Provenance, TextProjects};
use ziyuan_app::translation::{BlockState, Translations, output_rel};
use ziyuan_core::{CourtEvent, FileChange};
use ziyuan_memory::{
    Embedding, EmbeddingProvider, LocalProvider, MemoryError, MemoryIndex, ModelSignature,
};
use ziyuan_storage::{EventLog, EventStore, LockScope, Workspace};

const BLOCK: &str = "c0001-b0001";
struct Fixture {
    root: PathBuf,
    project: String,
}
impl Fixture {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "ziyuan-memory-app-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Session::found(EventLog::new(&root)).unwrap();
        let texts = TextProjects::new(&root);
        let p = texts
            .create(NewProject {
                title: "Memory".into(),
                source_language: "ja".into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap();
        let source = root.join("source.txt");
        fs::write(&source, "夜が明けた。\n\n猫が来た。\n").unwrap();
        texts.import(&p.id, &[source]).unwrap();
        Self {
            root,
            project: p.id,
        }
    }
    fn session(&self) -> Session<EventLog> {
        Session::open(EventLog::new(&self.root)).unwrap()
    }
    fn projects(&self) -> BTreeSet<String> {
        BTreeSet::from([self.project.clone()])
    }
    fn zh(&self) -> PathBuf {
        self.root.join(output_rel(&self.project, BLOCK))
    }
    fn applied(&self) -> Session<EventLog> {
        let mut session = self.session();
        let edict = session
            .issue_translation(&self.root, &self.project, BLOCK)
            .unwrap();
        let worker = session.court().route(&edict).unwrap().worker;
        let mut prompt = Translations::new(&self.root)
            .project(&self.project)
            .unwrap()
            .chapters[0]
            .blocks[0]
            .clone();
        prompt.paragraphs[0].text = "天亮了。".into();
        prompt.paragraphs[1].text = "猫来了。".into();
        let events = [
            CourtEvent::TaskAssigned {
                task: edict.id,
                worker,
                model: None,
            },
            CourtEvent::ChangesProposed {
                task: edict.id,
                changes: vec![FileChange {
                    path: output_rel(&self.project, BLOCK),
                    before: None,
                    after: String::from_utf8(prompt.serialized().unwrap()).unwrap(),
                }],
            },
            CourtEvent::TaskCompleted {
                task: edict.id,
                worker,
                summary: "done".into(),
            },
        ];
        EventLog::new(&self.root)
            .append(session.event_count(), &events)
            .unwrap();
        let mut session = self.session();
        session.apply_changes(edict.id, &self.root, vec![]).unwrap();
        session
    }
    fn finalized(&self) -> Session<EventLog> {
        let mut s = self.applied();
        s.finalize_translation(&self.root, &self.project, BLOCK)
            .unwrap();
        s
    }
    fn search(&self, s: &Session<EventLog>) -> Vec<ziyuan_app::memory::MemoryHit> {
        s.memory_search(
            &self.root,
            &BTreeSet::new(),
            &self.project,
            "夜が明けた。",
            5,
            &LocalProvider::new(),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn finalized_pairs_build_search_and_reopened_invalidation() {
    let f = Fixture::new();
    let mut s = f.finalized();
    let report = s.memory_pairs(&f.root, &f.project).unwrap();
    assert_eq!(report.pairs.len(), 2);
    assert_eq!(report.pairs[0].source, "夜が明けた。");
    assert_eq!(report.pairs[0].target, "天亮了。");
    assert_eq!(report.pairs[1].target, "猫来了。");
    let before = s.memory_watermark(&f.root).unwrap();
    let build = s
        .memory_build(&f.root, &f.projects(), &LocalProvider::new())
        .unwrap();
    assert_eq!(build.records, 2);
    assert_eq!(build.watermark.events, s.event_count());
    assert_eq!(s.memory_watermark(&f.root).unwrap(), before);
    assert_eq!(f.search(&s).len(), 2);
    assert!(!s.memory_status(&f.root).unwrap().stale);
    s.reopen_translation(&f.root, &f.project, BLOCK, "revise")
        .unwrap();
    assert!(
        s.memory_pairs(&f.root, &f.project)
            .unwrap()
            .pairs
            .is_empty()
    );
    assert!(f.search(&s).is_empty());
    assert!(s.memory_status(&f.root).unwrap().stale);
    assert!(f.search(&f.session()).is_empty());
}

#[test]
fn applied_unstarted_and_manual_edits_are_not_eligible() {
    let f = Fixture::new();
    assert!(
        f.session()
            .memory_pairs(&f.root, &f.project)
            .unwrap()
            .pairs
            .is_empty()
    );
    let mut s = f.applied();
    assert!(matches!(
        s.translation_status(&f.root, &f.project).unwrap()[0].state,
        BlockState::Applied { .. }
    ));
    assert!(
        s.memory_pairs(&f.root, &f.project)
            .unwrap()
            .pairs
            .is_empty()
    );
    s.finalize_translation(&f.root, &f.project, BLOCK).unwrap();
    s.memory_build(&f.root, &f.projects(), &LocalProvider::new())
        .unwrap();
    let good = fs::read(f.zh()).unwrap();
    fs::write(f.zh(), b"manual edit").unwrap();
    assert!(
        s.memory_pairs(&f.root, &f.project)
            .unwrap()
            .pairs
            .is_empty()
    );
    assert!(f.search(&s).is_empty());
    fs::write(f.zh(), good).unwrap();
    assert_eq!(f.search(&s).len(), 2);
}

#[test]
fn corrupt_cache_errors_and_lossless_rebuild() {
    let f = Fixture::new();
    let s = f.finalized();
    let original = fs::read(f.zh()).unwrap();
    let history = s.memory_watermark(&f.root).unwrap();
    s.memory_build(&f.root, &f.projects(), &LocalProvider::new())
        .unwrap();
    let path = ziyuan_memory::index::index_path(&f.root);
    let good_cache = fs::read(&path).unwrap();
    fs::write(&path, b"corrupt").unwrap();
    assert!(s.memory_status(&f.root).is_err());
    assert!(
        s.memory_search(
            &f.root,
            &f.projects(),
            &f.project,
            "夜",
            5,
            &LocalProvider::new()
        )
        .is_err()
    );
    s.memory_build(&f.root, &f.projects(), &LocalProvider::new())
        .unwrap();
    assert_eq!(fs::read(&path).unwrap(), good_cache);
    fs::remove_file(&path).unwrap();
    s.memory_build(&f.root, &f.projects(), &LocalProvider::new())
        .unwrap();
    assert_eq!(fs::read(&path).unwrap(), good_cache);
    assert_eq!(fs::read(f.zh()).unwrap(), original);
    assert_eq!(s.memory_watermark(&f.root).unwrap(), history);
}

#[test]
fn old_sessions_and_pending_generations_are_rejected() {
    let f = Fixture::new();
    let old = f.session();
    let s = f.finalized();
    assert!(old.memory_watermark(&f.root).is_err());
    assert!(old.memory_pairs(&f.root, &f.project).is_err());
    assert!(
        old.memory_build(&f.root, &f.projects(), &LocalProvider::new())
            .is_err()
    );
    let workspace = Workspace::open(&f.root).unwrap();
    let mut guard = workspace.lock(LockScope::Workspace).unwrap();
    guard.begin().unwrap();
    drop(guard);
    assert!(s.memory_watermark(&f.root).is_err());
    assert!(
        s.memory_build(&f.root, &f.projects(), &LocalProvider::new())
            .is_err()
    );
}

struct MutatingProvider<'a> {
    local: LocalProvider,
    action: Box<dyn Fn() + 'a>,
}
impl EmbeddingProvider for MutatingProvider<'_> {
    fn signature(&self) -> &ModelSignature {
        self.local.signature()
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Embedding>, MemoryError> {
        (self.action)();
        self.local.embed(texts)
    }
}

#[test]
fn manual_edit_during_embedding_aborts_without_replacing_cache() {
    let f = Fixture::new();
    let s = f.finalized();
    s.memory_build(&f.root, &f.projects(), &LocalProvider::new())
        .unwrap();
    let path = ziyuan_memory::index::index_path(&f.root);
    let original_cache = fs::read(&path).unwrap();
    let provider = MutatingProvider {
        local: LocalProvider::new(),
        action: Box::new(|| {
            fs::write(f.zh(), "edited while embedding").unwrap();
        }),
    };
    assert!(s.memory_build(&f.root, &f.projects(), &provider).is_err());
    assert_eq!(fs::read(path).unwrap(), original_cache);
}

#[test]
fn workspace_lock_spans_embedding_without_generation_changes() {
    let f = Fixture::new();
    let s = f.finalized();
    let before = s.memory_watermark(&f.root).unwrap();
    let provider = MutatingProvider {
        local: LocalProvider::new(),
        action: Box::new(|| {
            let workspace = Workspace::open(&f.root)
                .unwrap()
                .with_lock_timeout(std::time::Duration::from_millis(5));
            assert!(workspace.lock(LockScope::Workspace).is_err());
        }),
    };
    s.memory_build(&f.root, &f.projects(), &provider).unwrap();
    assert_eq!(s.memory_watermark(&f.root).unwrap(), before);
}

#[test]
fn empty_scope_is_one_project_and_missing_current_authorization_is_rejected() {
    let f = Fixture::new();
    let s = f.finalized();
    s.memory_build(&f.root, &f.projects(), &LocalProvider::new())
        .unwrap();
    assert_eq!(f.search(&s).len(), 2);
    assert!(
        s.memory_search(
            &f.root,
            &BTreeSet::from(["other".into()]),
            &f.project,
            "夜",
            5,
            &LocalProvider::new()
        )
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn symlink_cache_directory_is_rejected_for_read_and_build() {
    let f = Fixture::new();
    let s = f.finalized();
    let elsewhere = f.root.join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, f.root.join(".ziyuan/memory")).unwrap();
    assert!(
        s.memory_build(&f.root, &f.projects(), &LocalProvider::new())
            .is_err()
    );
    assert!(s.memory_status(&f.root).is_err());
    assert!(
        s.memory_search(
            &f.root,
            &f.projects(),
            &f.project,
            "夜",
            5,
            &LocalProvider::new()
        )
        .is_err()
    );
    assert!(!elsewhere.join("index.json").exists());
    assert!(MemoryIndex::load(&f.root).unwrap().is_none());
}

#[test]
fn malformed_finalized_blocks_are_rejected_whole_with_version_and_block_diagnostics() {
    let cases = [
        (
            "missing",
            serde_json::json!({"schema_version":1,"block_id":BLOCK,"paragraphs":[{"id":"c0001-p0001","text":"甲"}]}),
        ),
        (
            "order",
            serde_json::json!({"schema_version":1,"block_id":BLOCK,"paragraphs":[{"id":"c0001-p0002","text":"乙"},{"id":"c0001-p0001","text":"甲"}]}),
        ),
        (
            "duplicate",
            serde_json::json!({"schema_version":1,"block_id":BLOCK,"paragraphs":[{"id":"c0001-p0001","text":"甲"},{"id":"c0001-p0001","text":"乙"}]}),
        ),
        (
            "unknown",
            serde_json::json!({"schema_version":1,"block_id":BLOCK,"paragraphs":[{"id":"unknown","text":"甲"}]}),
        ),
        (
            "empty",
            serde_json::json!({"schema_version":1,"block_id":BLOCK,"paragraphs":[{"id":"c0001-p0001","text":" "},{"id":"c0001-p0002","text":"乙"}]}),
        ),
        (
            "block",
            serde_json::json!({"schema_version":1,"block_id":"wrong","paragraphs":[]}),
        ),
        (
            "wide_version",
            serde_json::json!({"schema_version":4294967297_u64,"block_id":BLOCK,"paragraphs":[]}),
        ),
        (
            "top_unknown",
            serde_json::json!({"schema_version":1,"block_id":BLOCK,"extra":"ignored?","paragraphs":[]}),
        ),
        (
            "para_unknown",
            serde_json::json!({"schema_version":1,"block_id":BLOCK,"paragraphs":[{"id":"c0001-p0001","text":"甲","extra":true},{"id":"c0001-p0002","text":"乙"}]}),
        ),
    ];
    for (name, value) in cases {
        let f = Fixture::new();
        let s = f.applied();
        let bytes = serde_json::to_vec(&value).unwrap();
        fs::write(f.zh(), &bytes).unwrap();
        // Simulate historical malformed input: replayed finalization is authoritative
        // for eligibility, but does not waive the separate structural pairing gate.
        EventLog::new(&f.root)
            .append(
                s.event_count(),
                &[CourtEvent::TranslationFinalized {
                    project: f.project.clone(),
                    block: BLOCK.into(),
                    translation_hash: ziyuan_memory::sha256_hex(&bytes),
                    context_version: 0,
                    zhupi_version: 0,
                }],
            )
            .unwrap();
        let s = f.session();
        assert!(
            matches!(
                s.translation_status(&f.root, &f.project).unwrap()[0].state,
                BlockState::Finalized { .. }
            ),
            "{name}"
        );
        let report = s.memory_pairs(&f.root, &f.project).unwrap();
        assert!(report.pairs.is_empty(), "{name}");
        assert_eq!(report.skipped.len(), 1, "{name}");
        assert_eq!(report.skipped[0].block, BLOCK);
        if name == "wide_version" {
            assert_eq!(
                report.skipped[0].reason,
                "unsupported_schema_version 4294967297"
            );
        }
    }
}

#[test]
fn changed_approved_context_invalidates_finalized_cache() {
    let f = Fixture::new();
    let s = f.finalized();
    s.memory_build(&f.root, &f.projects(), &LocalProvider::new())
        .unwrap();
    let t = Translations::new(&f.root);
    t.ensure_context_draft(&f.project).unwrap();
    t.approve_context(&f.project).unwrap();
    assert!(matches!(
        s.translation_status(&f.root, &f.project).unwrap()[0].state,
        BlockState::Stale { .. }
    ));
    assert!(
        s.memory_pairs(&f.root, &f.project)
            .unwrap()
            .pairs
            .is_empty()
    );
    assert!(f.search(&s).is_empty());
}

#[test]
fn manual_edit_during_query_never_returns_the_old_reference() {
    let f = Fixture::new();
    let s = f.finalized();
    s.memory_build(&f.root, &f.projects(), &LocalProvider::new())
        .unwrap();
    let provider = MutatingProvider {
        local: LocalProvider::new(),
        action: Box::new(|| {
            fs::write(f.zh(), "edited during query").unwrap();
        }),
    };
    assert!(
        s.memory_search(&f.root, &f.projects(), &f.project, "夜", 5, &provider)
            .is_err()
    );
}

#[test]
fn explicit_cross_project_scope_still_excludes_other_languages() {
    let mut f = Fixture::new();
    f.finalized();
    let first = f.project.clone();
    let texts = TextProjects::new(&f.root);
    let mut projects = f.projects();
    for language in ["ja", "en"] {
        let p = texts
            .create(NewProject {
                title: format!("Additional {language}"),
                source_language: language.into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap();
        texts.import(&p.id, &[f.root.join("source.txt")]).unwrap();
        f.project = p.id;
        f.finalized();
        projects.insert(f.project.clone());
    }
    f.project = first;
    let s = f.session();
    s.memory_build(&f.root, &projects, &LocalProvider::new())
        .unwrap();
    assert_eq!(f.search(&s).len(), 2);
    let hits = s
        .memory_search(
            &f.root,
            &projects,
            &f.project,
            "夜",
            10,
            &LocalProvider::new(),
        )
        .unwrap();
    assert_eq!(hits.len(), 4);
    assert!(hits.iter().all(|hit| hit.record.source_language == "ja"));
    assert!(
        hits.iter()
            .any(|hit| hit.record.identity.project != f.project)
    );
}

#[test]
fn unterminated_history_bytes_are_not_an_unchanged_watermark() {
    use std::io::Write;
    let f = Fixture::new();
    let s = f.finalized();
    s.memory_build(&f.root, &f.projects(), &LocalProvider::new())
        .unwrap();
    let path = ziyuan_memory::index::index_path(&f.root);
    let original = fs::read(&path).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(f.root.join(".ziyuan/court.log"))
        .unwrap()
        .write_all(b"{unfinished")
        .unwrap();
    assert!(s.memory_watermark(&f.root).is_err());
    assert!(
        s.memory_build(&f.root, &f.projects(), &LocalProvider::new())
            .is_err()
    );
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn snapshot_backed_session_rejects_rewritten_history_before_its_first_history_read() {
    let f = Fixture::new();
    let s = f.finalized();
    let mut log = EventLog::new(&f.root);
    let mut events = log.load().unwrap();
    let snapshot = serde_json::json!({"clock": null, "court": s.court()});
    assert!(
        log.save_snapshot(
            &format!("v{}", env!("CARGO_PKG_VERSION")),
            events.len(),
            &|out| { serde_json::to_writer(out, &snapshot).map_err(std::io::Error::from) }
        )
        .unwrap()
    );
    assert!(f.session().memory_watermark(&f.root).is_ok());
    let from_snapshot = f.session();
    let event = events
        .iter_mut()
        .rev()
        .find(|e| matches!(e, CourtEvent::TranslationFinalized { .. }))
        .unwrap();
    *event = CourtEvent::TranslationReopened {
        project: f.project.clone(),
        block: BLOCK.into(),
        reason: "history replaced".into(),
    };
    let bytes = events
        .iter()
        .map(ziyuan_storage::encode_event_line)
        .collect::<String>();
    fs::write(f.root.join(".ziyuan/court.log"), bytes).unwrap();
    assert_eq!(from_snapshot.event_count(), events.len());
    assert!(from_snapshot.memory_watermark(&f.root).is_err());
    assert!(
        from_snapshot
            .memory_build(&f.root, &f.projects(), &LocalProvider::new())
            .is_err()
    );
}
