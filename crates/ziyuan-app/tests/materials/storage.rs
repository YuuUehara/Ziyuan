use super::support::{Fixture, PROJECT_ENV, changed, is_lower_hex, tree, unfinished};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use ziyuan_app::translation::{
    MaterialDraft, MaterialError, MaterialIssueCode, MaterialPathSegment, MaterialReceipt,
    MaterialSave, Translations, validate_material,
};
use ziyuan_storage::JsonFileError;

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
