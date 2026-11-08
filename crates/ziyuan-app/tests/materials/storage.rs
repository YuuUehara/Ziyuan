use super::support::{Fixture, PROJECT_ENV, changed, is_lower_hex, tree, unfinished};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use ziyuan_app::translation::{
    Glossary, MaterialDraft, MaterialError, MaterialIssueCode, MaterialPathSegment,
    MaterialReceipt, MaterialSave, Translations, validate_material,
};
use ziyuan_storage::{JsonFileError, LockScope, Workspace};

const ROOT_ENV: &str = "ZIYUAN_TEST_MATERIAL_ROOT";

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

#[test]
fn prepare_material_production_writes_v2_and_approve_succeeds() {
    let f = Fixture::new();
    let t = f.service();
    let material = serde_json::from_value(json!({
        "format": 2,
        "terms": [{
            "term_id": "t-1", "source": "先生", "category": "common",
            "allowed": ["先生"], "scope": {"kind": "project"},
            "evidence": [], "status": "tentative"
        }],
        "persons": []
    }))
    .unwrap();
    t.save_material_draft(
        &f.project,
        &MaterialSave {
            request_id: "prepare-1".into(),
            expected_revision: None,
            material,
        },
    )
    .unwrap();
    t.ensure_context_draft(&f.project).unwrap();
    let workspace = Workspace::open(&f.root).unwrap();
    let mut guard = workspace.lock(LockScope::Workspace).unwrap();
    guard.upgrade_write_protocol().unwrap();
    t.texts()
        .upgrade_project_protocol(&f.project, &guard)
        .unwrap();
    let bytes = t.prepare_material_production(&guard, &f.project).unwrap();
    assert!(bytes > 0);
    let path = f
        .root
        .join("translations")
        .join(&f.project)
        .join("context/glossary.json");
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"format\": 2"), "{text}");
    assert!(text.contains("\"term_id\": \"t-1\""), "{text}");
    drop(guard);
    assert_eq!(t.approve_context(&f.project).unwrap(), 1);
    match t.context_snapshot(&f.project, 1).unwrap().glossary {
        Glossary::Structured(data) => assert_eq!(data.terms[0].term_id, "t-1"),
        Glossary::Legacy(_) => panic!("应为第二版资料快照"),
    }
}

#[test]
fn material_diff_reports_added_changed_and_removed() {
    let f = Fixture::new();
    let t = f.service();
    let material_a = serde_json::from_value(json!({
        "format": 2,
        "terms": [{
            "term_id": "t-1", "source": "夜", "category": "common",
            "allowed": ["夜晚"], "scope": {"kind": "project"},
            "evidence": [], "status": "confirmed"
        }],
        "persons": []
    }))
    .unwrap();
    t.save_material_draft(
        &f.project,
        &MaterialSave {
            request_id: "diff-a".into(),
            expected_revision: None,
            material: material_a,
        },
    )
    .unwrap();
    t.ensure_context_draft(&f.project).unwrap();
    let workspace = Workspace::open(&f.root).unwrap();
    let mut guard = workspace.lock(LockScope::Workspace).unwrap();
    guard.upgrade_write_protocol().unwrap();
    t.texts()
        .upgrade_project_protocol(&f.project, &guard)
        .unwrap();
    t.prepare_material_production(&guard, &f.project).unwrap();
    drop(guard);
    assert_eq!(t.approve_context(&f.project).unwrap(), 1);

    let material_b = serde_json::from_value(json!({
        "format": 2,
        "terms": [
            {
                "term_id": "t-1", "source": "夜", "category": "common",
                "allowed": ["夜"], "scope": {"kind": "project"},
                "evidence": [], "status": "confirmed"
            },
            {
                "term_id": "t-2", "source": "明", "category": "common",
                "allowed": ["明亮"], "scope": {"kind": "project"},
                "evidence": [], "status": "tentative"
            }
        ],
        "persons": []
    }))
    .unwrap();
    t.save_material_draft(
        &f.project,
        &MaterialSave {
            request_id: "diff-b".into(),
            expected_revision: Some(t.material_draft(&f.project).unwrap().unwrap().revision),
            material: material_b,
        },
    )
    .unwrap();
    let diff = t.material_diff(&f.project).unwrap();
    assert_eq!(diff.approved_version, Some(1));
    assert!(!diff.legacy_production);
    assert_eq!(diff.terms.len(), 2);
    let changed = diff.terms.iter().find(|c| c.id == "t-1").unwrap();
    assert_eq!(changed.kind, "changed");
    assert!(changed.fields.contains(&"allowed".to_string()));
    let added = diff.terms.iter().find(|c| c.id == "t-2").unwrap();
    assert_eq!(added.kind, "added");
}

#[test]
fn termbase_csv_roundtrip_terms_and_persons() {
    let f = Fixture::new();
    let t = f.service();
    let material = serde_json::from_value(json!({
        "format": 2,
        "terms": [{
            "term_id": "t-1", "source": "夜", "category": "common",
            "allowed": ["夜晚", "夜里"], "scope": {"kind": "project"},
            "evidence": [], "status": "confirmed"
        }],
        "persons": [{
            "person_id": "p_1", "name": "夜", "aliases": ["夜晚"], "terms": [],
            "address_rules": [], "voice": {"description": "低"}, "evidence": [],
            "status": "tentative"
        }]
    }))
    .unwrap();
    t.save_material_draft(
        &f.project,
        &MaterialSave {
            request_id: "termbase-save".into(),
            expected_revision: None,
            material,
        },
    )
    .unwrap();
    let csv = t.export_termbase_csv(&f.project).unwrap();
    assert!(csv.starts_with("kind,id,source,name,allowed,aliases,status,note"));
    assert!(csv.contains("term,t-1,夜,,夜晚;夜里,,confirmed,"), "{csv}");
    assert!(csv.contains("person,p_1,,夜,,夜晚,tentative,"), "{csv}");
    let receipt = t.import_termbase_csv(&f.project, csv.as_bytes()).unwrap();
    assert!(!receipt.revision.is_empty());
    let csv2 = t.export_termbase_csv(&f.project).unwrap();
    assert_eq!(csv, csv2);
}

#[test]
fn tm_comments_and_tbx_roundtrip() {
    let f = Fixture::new();
    let t = f.service();
    let entry = t.tm_add(&f.project, "夜", "夜晚").unwrap();
    assert_eq!(t.tm_list(&f.project).unwrap().len(), 1);
    let updated = t.tm_update(&f.project, &entry.id, "夜", "夜里").unwrap();
    assert_eq!(updated.target, "夜里");
    let adopted = t.tm_adopt(&f.project, &entry.id).unwrap();
    assert_eq!(adopted.adopted, 1);
    let comment = t.comment_add(&f.project, "c0001-b0001", "c0001-p0001", "这里再柔和些").unwrap();
    assert_eq!(t.comment_list(&f.project, None).unwrap().len(), 1);
    let resolved = t.comment_resolve(&f.project, &comment.id, true).unwrap();
    assert!(resolved.resolved);
    t.tm_delete(&f.project, &entry.id).unwrap();
    assert!(t.tm_list(&f.project).unwrap().is_empty());

    let material = serde_json::from_value(json!({
        "format": 2,
        "terms": [{"term_id":"t-1","source":"夜","category":"common","allowed":["夜晚"],"scope":{"kind":"project"},"evidence":[],"status":"confirmed"}],
        "persons": [{"person_id":"p_1","name":"夜","aliases":[],"terms":[],"address_rules":[],"voice":{"description":"低"},"evidence":[],"status":"tentative"}]
    })).unwrap();
    t.save_material_draft(&f.project, &MaterialSave { request_id: "tbx-save".into(), expected_revision: None, material }).unwrap();
    let tbx = t.export_termbase_tbx(&f.project).unwrap();
    assert!(tbx.contains("<termEntry id=\"t-1\">"));
    assert!(tbx.contains("<termEntry id=\"p_1\">"));
    let receipt = t.import_termbase_tbx(&f.project, tbx.as_bytes()).unwrap();
    assert!(!receipt.revision.is_empty());
}
