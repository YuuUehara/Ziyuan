use super::support::{material, sample};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use ziyuan_app::text_project::{NewProject, Provenance, TextProjects};
use ziyuan_app::translation::{
    MaterialDraft, MaterialError, MaterialIssueCode, MaterialPathSegment, MaterialReceipt,
    MaterialSave, StructuredGlossary, Translations, validate_material,
};
use ziyuan_storage::JsonFileError;

const ROOT_ENV: &str = "ZIYUAN_TEST_MATERIAL_ROOT";
const PROJECT_ENV: &str = "ZIYUAN_TEST_MATERIAL_PROJECT";

struct Fixture {
    root: PathBuf,
    project: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "ziyuan-material-storage-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let texts = TextProjects::new(&root);
        let project = texts
            .create(NewProject {
                title: "自创资料样本".into(),
                source_language: "ja".into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap();
        let source = root.join("source.txt");
        fs::write(&source, "先生来了。\n\n第二段。\n").unwrap();
        texts.import(&project.id, &[source]).unwrap();
        Self {
            root,
            project: project.id,
        }
    }

    fn service(&self) -> Translations {
        Translations::new(&self.root)
    }

    fn draft_path(&self) -> PathBuf {
        Path::new(".ziyuan/text-projects")
            .join(&self.project)
            .join("material-draft.json")
    }
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(base).unwrap().to_owned();
            if path.is_dir() {
                out.insert(relative, None);
                visit(base, &path, out);
            } else {
                out.insert(relative, Some(fs::read(path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

fn changed(
    before: &BTreeMap<PathBuf, Option<Vec<u8>>>,
    after: &BTreeMap<PathBuf, Option<Vec<u8>>>,
) -> BTreeSet<PathBuf> {
    before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect()
}

fn unfinished() -> StructuredGlossary {
    let mut value = sample();
    value["persons"][0]["name"] = json!("");
    material(value)
}

fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[test]
fn missing_draft_read_does_not_create_files() {
    let f = Fixture::new();
    let before = tree(&f.root);
    assert_eq!(f.service().material_draft(&f.project).unwrap(), None);
    assert!(
        tree(&f.root) == before,
        "读取不存在的资料草稿不得建目录、锁或工作区写协议"
    );
    assert!(f.service().material_draft("missing-project").is_err());
    assert!(tree(&f.root) == before);
}

#[test]
fn material_reopen_child() {
    let Some(root) = std::env::var_os(ROOT_ENV) else {
        return;
    };
    let project = std::env::var(PROJECT_ENV).unwrap();
    let draft = Translations::new(PathBuf::from(root))
        .material_draft(&project)
        .unwrap();
    println!("MATERIAL_DRAFT:{}", serde_json::to_string(&draft).unwrap());
}

#[test]
fn unfinished_draft_survives_a_fresh_process() {
    let f = Fixture::new();
    let data = unfinished();
    let project = f.service().project(&f.project).unwrap();
    let report = validate_material(&project, &data).unwrap();
    assert!(!report.valid, "保存资格不依赖校验状态");
    let receipt = f
        .service()
        .save_material_draft(
            &f.project,
            &MaterialSave {
                request_id: "first-save".into(),
                expected_revision: None,
                material: data.clone(),
            },
        )
        .unwrap();
    assert!(is_lower_hex(&receipt.revision, 32), "{receipt:?}");
    assert_eq!(receipt.content_hash, report.content_hash);
    let stored: Value =
        serde_json::from_slice(&fs::read(f.root.join(f.draft_path())).unwrap()).unwrap();
    assert_eq!(stored["schema_version"], json!(1));
    assert_eq!(stored["data"]["draft"]["revision"], json!(receipt.revision));
    assert_eq!(
        stored["data"]["requests"]["first-save"]["receipt"],
        serde_json::to_value(&receipt).unwrap()
    );

    let before = tree(&f.root);
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "storage::material_reopen_child", "--nocapture"])
        .env(ROOT_ENV, &f.root)
        .env(PROJECT_ENV, &f.project)
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let stdout = String::from_utf8(child.stdout).unwrap();
    let line = stdout
        .lines()
        .find_map(|line| line.strip_prefix("MATERIAL_DRAFT:"))
        .unwrap_or_else(|| panic!("子进程未输出草稿：{stdout}"));
    let reopened: Option<MaterialDraft> = serde_json::from_str(line).unwrap();
    assert_eq!(
        reopened,
        Some(MaterialDraft {
            project: f.project.clone(),
            revision: receipt.revision.clone(),
            material: data,
        })
    );
    assert!(tree(&f.root) == before, "新进程读取不得改动磁盘");
}

#[test]
fn save_does_not_touch_context_or_events() {
    let f = Fixture::new();
    let store = f.root.join(".ziyuan/text-projects").join(&f.project);
    let context = f.root.join("translations").join(&f.project).join("context");
    fs::create_dir_all(context.join("summaries")).unwrap();
    fs::write(context.join("glossary.json"), b"{\"legacy\":true}").unwrap();
    fs::create_dir_all(store.join("context/v1")).unwrap();
    fs::write(store.join("context/v1/glossary.json"), b"snapshot").unwrap();
    fs::write(store.join("verification.json"), b"verification").unwrap();
    fs::write(f.root.join(".ziyuan/court.log"), b"").unwrap();
    let exports = f.root.join("translations").join(&f.project).join("exports");
    fs::create_dir_all(&exports).unwrap();
    fs::write(exports.join("book.txt"), b"export").unwrap();
    fs::create_dir_all(f.root.join("translations/empty-dir")).unwrap();

    let before = tree(&f.root);
    f.service()
        .save_material_draft(
            &f.project,
            &MaterialSave {
                request_id: "isolated".into(),
                expected_revision: None,
                material: unfinished(),
            },
        )
        .unwrap();
    let after = tree(&f.root);
    let changes = changed(&before, &after);
    let draft = f.draft_path();
    let mut lock = draft.clone().into_os_string();
    lock.push(".lock");
    let allowed: BTreeSet<PathBuf> = [
        draft.clone(),
        PathBuf::from(lock),
        Path::new(".ziyuan").join("write.lock"),
        Path::new(".ziyuan").join("write-protocol"),
    ]
    .into_iter()
    .collect();
    assert!(changes.contains(&draft), "{changes:?}");
    assert!(changes.is_subset(&allowed), "{changes:?}");
    assert!(before.keys().all(|path| after.contains_key(path)));
}

fn save(
    f: &Fixture,
    request_id: &str,
    expected_revision: Option<String>,
) -> Result<MaterialReceipt, MaterialError> {
    f.service().save_material_draft(
        &f.project,
        &MaterialSave {
            request_id: request_id.into(),
            expected_revision,
            material: unfinished(),
        },
    )
}

const INVALID_REQUEST_IDS: &[&str] = &[
    "", "资料", "a/b", "..", "a\\b", "a b", "a\u{0}b", "a\nb", "a.b",
];

fn assert_request_id_issue(result: Result<MaterialReceipt, MaterialError>) {
    let MaterialError::Issue(issue) = result.unwrap_err() else {
        panic!("无效请求身份应报可定位问题，而非写后损坏")
    };
    assert_eq!(
        issue.path,
        vec![MaterialPathSegment::Field("request_id".into())]
    );
    assert_eq!(issue.code, MaterialIssueCode::Id);
}

#[test]
fn invalid_request_ids_are_rejected_before_publication() {
    let f = Fixture::new();
    let long = "a".repeat(129);
    let before = tree(&f.root);
    for id in INVALID_REQUEST_IDS.iter().copied().chain([long.as_str()]) {
        assert_request_id_issue(save(&f, id, None));
        assert!(tree(&f.root) == before, "无效请求身份 {id:?} 不得改动盘面");
    }
    assert_eq!(f.service().material_draft(&f.project).unwrap(), None);

    let mut revision = None;
    for id in ["a", "A-z_9", &"b".repeat(128)] {
        revision = Some(save(&f, id, revision).unwrap().revision);
    }
    let saved = tree(&f.root);
    for id in INVALID_REQUEST_IDS.iter().copied().chain([long.as_str()]) {
        assert_request_id_issue(save(&f, id, revision.clone()));
        assert!(
            tree(&f.root) == saved,
            "已有草稿时无效请求身份 {id:?} 不得改动盘面"
        );
    }
    assert_eq!(
        f.service()
            .material_draft(&f.project)
            .unwrap()
            .unwrap()
            .revision,
        revision.unwrap()
    );
}

#[test]
fn stored_invalid_request_id_stays_corrupt_without_reset() {
    let f = Fixture::new();
    let receipt = save(&f, "valid", None).unwrap();
    let path = f.root.join(f.draft_path());
    let mut stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let requests = stored["data"]["requests"].as_object_mut().unwrap();
    let saved = requests.remove("valid").unwrap();
    requests.insert("bad id".into(), saved);
    fs::write(&path, serde_json::to_vec_pretty(&stored).unwrap()).unwrap();
    let before = tree(&f.root);
    assert!(matches!(
        f.service().material_draft(&f.project).unwrap_err(),
        MaterialError::Storage(JsonFileError::Corrupt(_))
    ));
    assert!(matches!(
        save(&f, "next", Some(receipt.revision)).unwrap_err(),
        MaterialError::Storage(JsonFileError::Corrupt(_))
    ));
    assert!(tree(&f.root) == before, "损坏的旧回执不得被清除或覆盖");
}

fn variant(tag: &str) -> StructuredGlossary {
    let mut value = sample();
    value["persons"][0]["name"] = json!("");
    value["terms"][0]["note"] = json!(format!("变体{tag}"));
    material(value)
}

fn save_with(
    f: &Fixture,
    request_id: &str,
    expected_revision: Option<String>,
    material: StructuredGlossary,
) -> Result<MaterialReceipt, MaterialError> {
    f.service().save_material_draft(
        &f.project,
        &MaterialSave {
            request_id: request_id.into(),
            expected_revision,
            material,
        },
    )
}

fn draft_bytes(f: &Fixture) -> Vec<u8> {
    fs::read(f.root.join(f.draft_path())).unwrap()
}

fn receipt_count(f: &Fixture) -> usize {
    let stored: Value = serde_json::from_slice(&draft_bytes(f)).unwrap();
    stored["data"]["requests"].as_object().unwrap().len()
}

fn current(f: &Fixture) -> MaterialDraft {
    f.service().material_draft(&f.project).unwrap().unwrap()
}

fn assert_conflict(result: Result<MaterialReceipt, MaterialError>, expected: &str) {
    match result {
        Err(MaterialError::Conflict { current }) => assert_eq!(current.as_deref(), Some(expected)),
        other => panic!("应报修订冲突并携带现行修订，实际：{other:?}"),
    }
}

#[test]
fn create_race_has_one_winner() {
    const RACERS: usize = 8;
    let f = Fixture::new();
    let barrier = std::sync::Barrier::new(RACERS);
    let results: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..RACERS)
            .map(|i| {
                let (f, barrier) = (&f, &barrier);
                scope.spawn(move || {
                    let service = f.service();
                    let request = MaterialSave {
                        request_id: format!("racer-{i}"),
                        expected_revision: None,
                        material: variant(&i.to_string()),
                    };
                    barrier.wait();
                    service.save_material_draft(&f.project, &request)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let winners: Vec<_> = results
        .iter()
        .enumerate()
        .filter_map(|(i, r)| r.as_ref().ok().map(|receipt| (i, receipt.clone())))
        .collect();
    assert_eq!(winners.len(), 1, "{results:?}");
    let (winner, receipt) = winners.into_iter().next().unwrap();
    for (i, result) in results.into_iter().enumerate() {
        if i != winner {
            assert_conflict(result, &receipt.revision);
        }
    }
    let draft = current(&f);
    assert_eq!(draft.revision, receipt.revision);
    assert_eq!(draft.material, variant(&winner.to_string()));
    assert_eq!(receipt_count(&f), 1, "失败者不得留下回执");
}

#[test]
fn stale_revision_keeps_local_content() {
    let f = Fixture::new();
    let first = save_with(&f, "first", None, variant("A")).unwrap();
    let second = save_with(&f, "second", Some(first.revision.clone()), variant("B")).unwrap();
    let bytes = draft_bytes(&f);
    let before = tree(&f.root);
    let local = variant("本地");
    assert_conflict(
        save_with(&f, "stale", Some(first.revision.clone()), local.clone()),
        &second.revision,
    );
    assert_conflict(save_with(&f, "missing", None, local), &second.revision);
    assert_eq!(draft_bytes(&f), bytes, "过期修订不得覆盖较新草稿");
    assert!(tree(&f.root) == before);
    let draft = current(&f);
    assert_eq!(draft.revision, second.revision);
    assert_eq!(draft.material, variant("B"));
    assert_eq!(receipt_count(&f), 2);
}

#[test]
fn same_id_different_payload_conflicts() {
    let f = Fixture::new();
    let first = save_with(&f, "same", None, variant("A")).unwrap();
    let bytes = draft_bytes(&f);
    assert!(matches!(
        save_with(&f, "same", None, variant("B")),
        Err(MaterialError::RequestConflict)
    ));
    assert!(
        matches!(
            save_with(&f, "same", Some(first.revision.clone()), variant("A")),
            Err(MaterialError::RequestConflict)
        ),
        "预期修订也属于请求内容"
    );
    assert_eq!(draft_bytes(&f), bytes);
    assert_eq!(receipt_count(&f), 1);
    assert_eq!(save_with(&f, "same", None, variant("A")).unwrap(), first);
    assert_eq!(draft_bytes(&f), bytes);
}

const RETRY_ROOT_ENV: &str = "ZIYUAN_TEST_MATERIAL_RETRY_ROOT";

#[test]
fn material_retry_child() {
    let Some(root) = std::env::var_os(RETRY_ROOT_ENV) else {
        return;
    };
    let project = std::env::var(PROJECT_ENV).unwrap();
    let receipt = Translations::new(PathBuf::from(root))
        .save_material_draft(
            &project,
            &MaterialSave {
                request_id: "restart".into(),
                expected_revision: None,
                material: variant("A"),
            },
        )
        .unwrap();
    println!(
        "MATERIAL_RECEIPT:{}",
        serde_json::to_string(&receipt).unwrap()
    );
}

#[test]
fn retry_after_restart_returns_same_receipt_without_write() {
    let f = Fixture::new();
    let receipt = save_with(&f, "restart", None, variant("A")).unwrap();
    let bytes = draft_bytes(&f);
    let before = tree(&f.root);
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "storage::material_retry_child", "--nocapture"])
        .env(RETRY_ROOT_ENV, &f.root)
        .env(PROJECT_ENV, &f.project)
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let stdout = String::from_utf8(child.stdout).unwrap();
    let line = stdout
        .lines()
        .find_map(|line| line.strip_prefix("MATERIAL_RECEIPT:"))
        .unwrap_or_else(|| panic!("子进程未输出回执：{stdout}"));
    let retried: MaterialReceipt = serde_json::from_str(line).unwrap();
    assert_eq!(retried, receipt);
    assert_eq!(draft_bytes(&f), bytes, "重启后重试不得重写草稿");
    assert!(tree(&f.root) == before, "重启后重试不得改动磁盘");
    assert_eq!(current(&f).revision, receipt.revision);
    assert_eq!(receipt_count(&f), 1);
}

#[test]
fn old_retry_never_rolls_back_newer_save() {
    let f = Fixture::new();
    let old = save_with(&f, "old", None, variant("A")).unwrap();
    let newer = save_with(&f, "newer", Some(old.revision.clone()), variant("B")).unwrap();
    let bytes = draft_bytes(&f);
    let before = tree(&f.root);
    assert_eq!(save_with(&f, "old", None, variant("A")).unwrap(), old);
    assert_eq!(draft_bytes(&f), bytes, "旧请求重试不得写盘");
    assert!(tree(&f.root) == before);
    let draft = current(&f);
    assert_eq!(draft.revision, newer.revision);
    assert_eq!(draft.material, variant("B"));
    assert_eq!(receipt_count(&f), 2);
}

#[test]
fn a_b_a_has_three_distinct_revisions() {
    let f = Fixture::new();
    let a1 = save_with(&f, "a1", None, variant("A")).unwrap();
    let b = save_with(&f, "b", Some(a1.revision.clone()), variant("B")).unwrap();
    let a2 = save_with(&f, "a2", Some(b.revision.clone()), variant("A")).unwrap();
    let revisions: BTreeSet<_> = [&a1.revision, &b.revision, &a2.revision]
        .into_iter()
        .collect();
    assert_eq!(revisions.len(), 3, "修订不得由内容摘要推出");
    assert_eq!(a1.content_hash, a2.content_hash);
    assert_ne!(a1.content_hash, b.content_hash);
    let bytes = draft_bytes(&f);
    assert_conflict(
        save_with(&f, "stale-a", Some(a1.revision.clone()), variant("C")),
        &a2.revision,
    );
    assert_eq!(save_with(&f, "a1", None, variant("A")).unwrap(), a1);
    assert_eq!(draft_bytes(&f), bytes, "ABA 后的旧修订与旧重试都不得写盘");
    assert_eq!(current(&f).revision, a2.revision);
    assert_eq!(receipt_count(&f), 3);
}
