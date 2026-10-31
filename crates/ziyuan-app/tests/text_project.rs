use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use ziyuan_app::text_project::*;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "ziyuan-text-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn service(&self) -> TextProjects {
        TextProjects::new(&self.0)
    }
    fn file(&self, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let p = self.0.join(name);
        fs::write(&p, bytes).unwrap();
        p
    }
    fn create(&self, language: &str) -> Project {
        self.service()
            .create(NewProject {
                title: "作品".into(),
                source_language: language.into(),
                target_language: "zh-CN".into(),
                provenance: Provenance {
                    author: "作者".into(),
                    version: "初版".into(),
                    source: "手元".into(),
                    date: "2026-10-30".into(),
                    rights: "user statement".into(),
                    scope: "private translation".into(),
                },
            })
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn preserves_raw_order_layout_unicode_and_duplicate_paragraphs() {
    let f = Fixture::new();
    let p = f.create("ja");
    let a = f.file("a.txt", "\r\n　\r\n猫。\r\n次の行。\r\n \t\r\n猫。\r\n\r\n");
    let b = f.file("b.txt", "English first.\n\nSecond.");
    let p = f.service().import(&p.id, &[a.clone(), b.clone()]).unwrap();
    assert_eq!(p.chapters.len(), 2);
    assert_eq!(p.chapters[0].paragraphs[0].text, "猫。\n次の行。");
    assert_eq!(p.chapters[0].paragraphs[1].text, "猫。");
    assert_eq!(p.chapters[0].paragraphs[0].id, "c0001-p0001");
    assert_eq!(p.chapters[0].paragraphs[1].id, "c0001-p0002");
    assert_eq!(p.chapters[1].paragraphs[0].id, "c0002-p0001");
    for (c, path) in p.chapters.iter().zip([a, b]) {
        let raw = fs::read(path).unwrap();
        assert_eq!(
            fs::read(
                f.service()
                    .project_dir(&p.id)
                    .unwrap()
                    .join(&c.original_path)
            )
            .unwrap(),
            raw
        );
        for paragraph in &c.paragraphs {
            let slice = &raw[paragraph.byte_start..paragraph.byte_end];
            assert_eq!(
                String::from_utf8(slice.to_vec())
                    .unwrap()
                    .replace("\r\n", "\n"),
                paragraph.text
            );
        }
        for block in &c.blocks {
            assert!(block.serialized().unwrap().len() <= DEFAULT_BLOCK_BYTES);
        }
    }
    assert_eq!(f.service().show(&p.id).unwrap(), p);
    assert!(f.service().import(&p.id, &[]).is_err());
    assert!(
        f.service()
            .import(&p.id, &[f.file("again.txt", "same")])
            .is_err()
    );
}

#[test]
fn rejects_invalid_registration_ids_and_sources_without_partial_visibility() {
    let f = Fixture::new();
    let mut spec = NewProject {
        title: "x".into(),
        source_language: "fr".into(),
        target_language: "zh-CN".into(),
        provenance: Provenance::default(),
    };
    assert!(f.service().create(spec.clone()).is_err());
    spec.source_language = "en".into();
    spec.target_language = "ja".into();
    assert!(f.service().create(spec).is_err());
    for id in ["../x", "/x", "x\\y", ".", ""] {
        assert!(f.service().show(id).is_err());
    }
    let p = f.create("en");
    let good = f.file("good.txt", "First\n\nSecond");
    for bad in [
        f.file("empty.txt", ""),
        f.file("white.txt", " \r\n　\t\n"),
        f.file("invalid.txt", [0xff]),
        f.0.clone(),
        f.file("wrong.md", "text"),
    ] {
        assert!(f.service().import(&p.id, &[good.clone(), bad]).is_err());
        assert!(f.service().show(&p.id).unwrap().chapters.is_empty());
        assert_eq!(fs::read(&good).unwrap(), b"First\n\nSecond");
    }
    assert_eq!(
        f.service().import(&p.id, &[good]).unwrap().chapters.len(),
        1
    );
}

#[test]
fn serialized_exact_boundary_and_rechunking_preserve_paragraph_ids() {
    let paragraphs = split_paragraphs("c0001", "猫\r\n\r\n猫\r\n\r\n三".as_bytes()).unwrap();
    let blocks = chunk_paragraphs("c0001", &paragraphs, DEFAULT_BLOCK_BYTES).unwrap();
    let exact = blocks[0].serialized().unwrap().len();
    assert_eq!(
        chunk_paragraphs("c0001", &paragraphs, exact).unwrap().len(),
        1
    );
    let small = chunk_paragraphs("c0001", &paragraphs, exact - 1).unwrap();
    assert!(small.len() > 1);
    assert_eq!(
        small
            .iter()
            .flat_map(|b| b.paragraphs.iter().map(|p| &p.id))
            .collect::<Vec<_>>(),
        paragraphs.iter().map(|p| &p.id).collect::<Vec<_>>()
    );
    assert!(small.iter().all(|b| b.serialized().unwrap().len() < exact));
    let huge = split_paragraphs("c0002", "界".repeat(DEFAULT_BLOCK_BYTES).as_bytes()).unwrap();
    let error = chunk_paragraphs("c0002", &huge, DEFAULT_BLOCK_BYTES)
        .unwrap_err()
        .to_string();
    assert!(error.contains("c0002-p0001"), "{error}");
    assert!(chunk_paragraphs("c0001", &paragraphs, 65536).is_err());
}

#[test]
fn preserves_non_crlf_carriage_return_and_indentation() {
    let p = split_paragraphs("c0001", b"  First\r\ncontinued\r\n\r\nLast\r").unwrap();
    assert_eq!(p[0].text, "  First\ncontinued");
    assert_eq!(p[1].text, "Last\r");
}

#[test]
fn failed_oversized_import_preserves_manifest_and_can_retry() {
    let f = Fixture::new();
    let p = f.create("en");
    let path = f.service().project_dir(&p.id).unwrap().join("project.json");
    let before = fs::read(&path).unwrap();
    let good = f.file("good.txt", "First");
    let huge = f.file("huge.txt", "界".repeat(DEFAULT_BLOCK_BYTES));
    assert!(f.service().import(&p.id, &[good.clone(), huge]).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(f.service().show(&p.id).unwrap().chapters.is_empty());
    assert_eq!(
        f.service().import(&p.id, &[good]).unwrap().chapters.len(),
        1
    );
}

#[cfg(windows)]
#[test]
fn windows_symlink_sources_rejected_when_supported() {
    let f = Fixture::new();
    let p = f.create("en");
    let a = f.file("a.txt", "One");
    let link = f.0.join("link.txt");
    match std::os::windows::fs::symlink_file(a, &link) {
        Ok(()) => assert!(f.service().import(&p.id, &[link]).is_err()),
        Err(e)
            if e.kind() == std::io::ErrorKind::PermissionDenied
                || e.raw_os_error() == Some(1314) => {}
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn concurrent_import_has_one_winner_and_preserves_original() {
    let f = Fixture::new();
    let p = f.create("en");
    let a = f.file("a.txt", "One\n\nTwo");
    let results = std::thread::scope(|s| {
        let run = || f.service().import(&p.id, std::slice::from_ref(&a));
        let x = s.spawn(run);
        let y = s.spawn(run);
        [x.join().unwrap(), y.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(f.service().show(&p.id).unwrap().chapters.len(), 1);
    assert_eq!(fs::read(a).unwrap(), b"One\n\nTwo");
}

#[cfg(unix)]
#[test]
fn symlink_sources_rejected() {
    let f = Fixture::new();
    let p = f.create("en");
    let a = f.file("a.txt", "One");
    let link = f.0.join("link.txt");
    std::os::unix::fs::symlink(a, &link).unwrap();
    assert!(f.service().import(&p.id, &[link]).is_err());
}

fn imported(f: &Fixture) -> Project {
    let p = f.create("en");
    f.service()
        .import(
            &p.id,
            &[
                f.file("first.txt", "One\n\nTwo\n\nThree\n\nFour"),
                f.file("second.txt", "五"),
            ],
        )
        .unwrap()
}
fn candidate(block: &str, paragraphs: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(
        &serde_json::json!({"schema_version":1,"block_id":block,"paragraphs":paragraphs}),
    )
    .unwrap()
}

#[test]
fn valid_candidate_is_structural_only_and_read_only() {
    let f = Fixture::new();
    let p = imported(&f);
    let block = &p.chapters[0].blocks[0];
    let bytes = candidate(
        &block.block_id,
        serde_json::json!([
            {"id":"c0001-p0001","text":"随意的中文也不代表质量合格"},
            {"id":"c0001-p0002","text":"二"},
            {"id":"c0001-p0003","text":"三"},
            {"id":"c0001-p0004","text":"四"}
        ]),
    );
    let path = f.service().project_dir(&p.id).unwrap().join("project.json");
    let before = fs::read(&path).unwrap();
    let report = f.service().check(&p.id, &block.block_id, &bytes).unwrap();
    assert!(report.valid);
    assert!(report.issues.is_empty());
    assert_eq!(report.chapter_id, "c0001");
    assert_eq!(report.expected_ids.len(), 4);
    assert_eq!(fs::read(path).unwrap(), before);
    assert_eq!(f.service().show(&p.id).unwrap(), p);
    serde_json::to_value(report).unwrap();
    assert!(f.service().check(&p.id, "c9999-b0001", &bytes).is_err());
}

#[test]
fn candidate_collects_missing_duplicate_order_unknown_and_empty_together() {
    let f = Fixture::new();
    let p = imported(&f);
    let bytes = candidate(
        "c0001-b0001",
        serde_json::json!([
            {"id":"c0001-p0002","text":"二"},
            {"id":"c0001-p0001","text":"一"},
            {"id":"c0001-p0002","text":"重复"},
            {"id":"c0002-p0001","text":"别章"},
            {"id":"c0001-p0004","text":"　 \r\n"}
        ]),
    );
    let a = f.service().check(&p.id, "c0001-b0001", &bytes).unwrap();
    let b = f.service().check(&p.id, "c0001-b0001", &bytes).unwrap();
    assert_eq!(a, b);
    assert!(!a.valid);
    for (kind, id) in [
        (IssueKind::Missing, "c0001-p0003"),
        (IssueKind::Duplicate, "c0001-p0002"),
        (IssueKind::OutOfOrder, "c0001-p0001"),
        (IssueKind::Unknown, "c0002-p0001"),
        (IssueKind::EmptyText, "c0001-p0004"),
    ] {
        assert!(
            a.issues
                .iter()
                .any(|i| i.kind == kind && i.paragraph_id.as_deref() == Some(id)),
            "{a:?}"
        );
    }
    assert!(
        a.issues
            .iter()
            .all(|i| i.chapter_id == "c0001" && i.block_id == "c0001-b0001")
    );
}

#[test]
fn candidate_rejects_bad_json_versions_wrong_block_and_missing_schema_fields() {
    let f = Fixture::new();
    let p = imported(&f);
    for (bytes, kind) in [
        (b"not JSON".to_vec(), IssueKind::InvalidJson),
        (
            br#"{"block_id":"c0001-b0001","paragraphs":[]}"#.to_vec(),
            IssueKind::InvalidJson,
        ),
        (
            br#"{"schema_version":2,"block_id":"c0001-b0001","paragraphs":[]}"#.to_vec(),
            IssueKind::NewerVersion,
        ),
        (
            br#"{"schema_version":0,"block_id":"c0001-b0001","paragraphs":[]}"#.to_vec(),
            IssueKind::UnsupportedVersion,
        ),
        (
            candidate("c0002-b0001", serde_json::json!([])),
            IssueKind::WrongBlock,
        ),
    ] {
        let report = f.service().check(&p.id, "c0001-b0001", &bytes).unwrap();
        assert!(!report.valid);
        assert!(report.issues.iter().any(|i| i.kind == kind), "{report:?}");
    }
}

#[test]
fn original_tamper_and_newer_manifest_refuse_show_and_candidate_check() {
    let f = Fixture::new();
    let p = imported(&f);
    let dir = f.service().project_dir(&p.id).unwrap();
    let original = dir.join(&p.chapters[0].original_path);
    let bytes = fs::read(&original).unwrap();
    fs::write(&original, b"changed").unwrap();
    assert!(matches!(
        f.service().show(&p.id),
        Err(TextProjectError::Stale(_))
    ));
    assert!(f.service().check(&p.id, "c0001-b0001", b"{}").is_err());
    fs::write(original, bytes).unwrap();
    let manifest = dir.join("project.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    value["schema_version"] = 2.into();
    fs::write(manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(
        f.service()
            .show(&p.id)
            .unwrap_err()
            .to_string()
            .contains("升级")
    );
    assert!(f.service().check(&p.id, "c0001-b0001", b"{}").is_err());
}

#[test]
fn rejects_text_too_large_for_one_project_before_staging() {
    let f = Fixture::new();
    let p = f.create("ja");
    let body = (0..400)
        .map(|i| format!("第{i}段。"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let src = f.file("long.txt", body);
    let service = TextProjects::new(&f.0).with_manifest_limit(16 * 1024);
    let err = service.import(&p.id, &[src]).unwrap_err().to_string();
    assert!(err.contains("拆成多个项目"), "{err}");
    let dir = service.project_dir(&p.id).unwrap();
    let staged = fs::read_dir(&dir)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".import-")
        })
        .count();
    assert_eq!(staged, 0);
    assert!(service.show(&p.id).unwrap().chapters.is_empty());
}

// ---- Cross-crate drift guard: `ziyuan-memory` pairing vs `TextProjects::check` ----
//
// `ziyuan_memory::pair_paragraphs` claims to be equivalent to `check`. A hand-copied fixture
// cannot hold that claim (it drifts silently), so this compares the two implementations on the
// same bytes, element by element: same kind, same paragraph ID, same 1-based position, same order.

/// A problem reduced to its comparable parts: kind, paragraph ID, 1-based position.
type ProblemKey = (MemoryShape, Option<String>, Option<usize>);

/// The kind of problem `ziyuan-memory` reports, without its payload.
///
/// `NotVisibleToMemory` marks app-side kinds the memory path cannot see at all: it decodes the
/// candidate itself, so malformed JSON stops it before any structural check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemoryShape {
    NewerVersion,
    UnsupportedVersion,
    WrongBlock,
    Missing,
    Duplicate,
    OutOfOrder,
    Unknown,
    EmptyText,
    NotVisibleToMemory,
}

/// Maps an app-side `IssueKind` to the memory-side shape it must correspond to.
///
/// Exhaustive on purpose: adding an `IssueKind` variant becomes a compile error here instead of
/// silently passing, which is the whole point of this check.
fn memory_shape(kind: IssueKind) -> MemoryShape {
    match kind {
        IssueKind::InvalidJson => MemoryShape::NotVisibleToMemory,
        IssueKind::NewerVersion => MemoryShape::NewerVersion,
        IssueKind::UnsupportedVersion => MemoryShape::UnsupportedVersion,
        IssueKind::WrongBlock => MemoryShape::WrongBlock,
        IssueKind::Missing => MemoryShape::Missing,
        IssueKind::Duplicate => MemoryShape::Duplicate,
        IssueKind::OutOfOrder => MemoryShape::OutOfOrder,
        IssueKind::Unknown => MemoryShape::Unknown,
        IssueKind::EmptyText => MemoryShape::EmptyText,
    }
}

/// Reduces a memory-side problem to the same comparable parts. Also exhaustive, so a new
/// `PairingProblem` variant cannot slip past this test either.
fn memory_problem(problem: &ziyuan_memory::PairingProblem) -> ProblemKey {
    use ziyuan_memory::PairingProblem as P;
    match problem {
        P::NewerVersion { .. } => (MemoryShape::NewerVersion, None, None),
        P::UnsupportedVersion { .. } => (MemoryShape::UnsupportedVersion, None, None),
        P::WrongBlock { .. } => (MemoryShape::WrongBlock, None, None),
        P::Missing { id } => (MemoryShape::Missing, Some(id.clone()), None),
        P::Duplicate { id, position } => {
            (MemoryShape::Duplicate, Some(id.clone()), Some(*position))
        }
        P::OutOfOrder { id, position } => {
            (MemoryShape::OutOfOrder, Some(id.clone()), Some(*position))
        }
        P::Unknown { id, position } => (MemoryShape::Unknown, Some(id.clone()), Some(*position)),
        P::EmptyText { id, position } => {
            (MemoryShape::EmptyText, Some(id.clone()), Some(*position))
        }
    }
}

fn memory_view(block: &PromptBlock) -> ziyuan_memory::PromptBlockView {
    ziyuan_memory::PromptBlockView {
        schema_version: block.schema_version,
        block_id: block.block_id.clone(),
        paragraphs: block
            .paragraphs
            .iter()
            .map(|p| ziyuan_memory::ParaPairInput {
                id: p.id.clone(),
                text: p.text.clone(),
            })
            .collect(),
    }
}

/// Runs the memory path over the same bytes; `None` means it cannot even decode them.
fn memory_problems(expected: &PromptBlock, bytes: &[u8]) -> Option<Vec<ProblemKey>> {
    let candidate: PromptBlock = serde_json::from_slice(bytes).ok()?;
    Some(
        match ziyuan_memory::pair_paragraphs(&memory_view(expected), &memory_view(&candidate)) {
            Ok(_) => Vec::new(),
            Err(problems) => problems.iter().map(memory_problem).collect(),
        },
    )
}

#[test]
fn memory_pairing_and_check_agree_on_kind_id_and_position() {
    let f = Fixture::new();
    let p = imported(&f);
    let block = p.chapters[0].blocks[0].clone();
    let id = &block.block_id;
    let cases: [(&str, Vec<u8>); 11] = [
        (
            "well_formed",
            candidate(
                id,
                serde_json::json!([
                    {"id":"c0001-p0001","text":"一"},
                    {"id":"c0001-p0002","text":"二"},
                    {"id":"c0001-p0003","text":"三"},
                    {"id":"c0001-p0004","text":"四"}
                ]),
            ),
        ),
        (
            "missing",
            candidate(
                id,
                serde_json::json!([
                    {"id":"c0001-p0001","text":"一"},
                    {"id":"c0001-p0002","text":"二"}
                ]),
            ),
        ),
        ("no_paragraphs", candidate(id, serde_json::json!([]))),
        (
            "duplicated",
            candidate(
                id,
                serde_json::json!([
                    {"id":"c0001-p0001","text":"一"},
                    {"id":"c0001-p0001","text":"甲"},
                    {"id":"c0001-p0002","text":"二"},
                    {"id":"c0001-p0003","text":"三"},
                    {"id":"c0001-p0004","text":"四"}
                ]),
            ),
        ),
        (
            "reordered",
            candidate(
                id,
                serde_json::json!([
                    {"id":"c0001-p0002","text":"二"},
                    {"id":"c0001-p0001","text":"一"},
                    {"id":"c0001-p0003","text":"三"},
                    {"id":"c0001-p0004","text":"四"}
                ]),
            ),
        ),
        (
            "unknown_id",
            candidate(
                id,
                serde_json::json!([
                    {"id":"c0001-p0001","text":"一"},
                    {"id":"c0001-p0002","text":"二"},
                    {"id":"c0001-p0003","text":"三"},
                    {"id":"c0001-p0004","text":"四"},
                    {"id":"c0002-p0001","text":"别章"}
                ]),
            ),
        ),
        (
            "empty_text",
            candidate(
                id,
                serde_json::json!([
                    {"id":"c0001-p0001","text":"一"},
                    {"id":"c0001-p0002","text":"二"},
                    {"id":"c0001-p0003","text":"三"},
                    {"id":"c0001-p0004","text":"　 \r\n"}
                ]),
            ),
        ),
        (
            "wrong_block",
            candidate(
                "c0002-b0001",
                serde_json::json!([{"id":"c0001-p0001","text":"一"}]),
            ),
        ),
        (
            "everything_at_once",
            candidate(
                id,
                serde_json::json!([
                    {"id":"c0001-p0002","text":"二"},
                    {"id":"c0001-p0001","text":"一"},
                    {"id":"c0001-p0002","text":"重复"},
                    {"id":"c0002-p0001","text":"别章"},
                    {"id":"c0001-p0004","text":"　 \r\n"}
                ]),
            ),
        ),
        (
            "version_2",
            br#"{"schema_version":2,"block_id":"c0001-b0001","paragraphs":[]}"#.to_vec(),
        ),
        (
            "version_0",
            br#"{"schema_version":0,"block_id":"c0001-b0001","paragraphs":[]}"#.to_vec(),
        ),
    ];
    for (label, bytes) in cases {
        let report = f.service().check(&p.id, id, &bytes).unwrap();
        let mut app: Vec<ProblemKey> = Vec::new();
        let mut app_version: Option<MemoryShape> = None;
        let mut unreadable = false;
        for issue in &report.issues {
            match memory_shape(issue.kind) {
                MemoryShape::NotVisibleToMemory => {
                    assert_eq!(issue.kind, IssueKind::InvalidJson, "{label}: {report:?}");
                    unreadable = true;
                }
                shape @ (MemoryShape::NewerVersion | MemoryShape::UnsupportedVersion) => {
                    app_version = Some(shape);
                }
                shape => app.push((shape, issue.paragraph_id.clone(), issue.position)),
            }
        }
        assert_eq!(
            report.valid,
            app.is_empty() && app_version.is_none() && !unreadable,
            "{label}: {report:?}"
        );
        let memory = memory_problems(&block, &bytes);
        if unreadable {
            // The memory path decodes before it looks at structure, so it must see nothing.
            assert!(
                memory.is_none(),
                "{label}: 记忆库不该解得出 check 读不了的字节"
            );
            continue;
        }
        let memory = memory.unwrap_or_else(|| panic!("{label}: 记忆库解不出 check 读得了的字节"));
        if let Some(shape) = app_version {
            // Both sides stop at the version and report nothing else.
            assert_eq!(memory, vec![(shape, None, None)], "{label}");
            continue;
        }
        assert_eq!(memory, app, "{label}");
    }
}
