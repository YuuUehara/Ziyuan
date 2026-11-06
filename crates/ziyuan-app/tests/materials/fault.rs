use super::support::{
    Fixture, changed, current, draft_bytes, save_with, tree, unfinished, variant,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use ziyuan_app::translation::{MaterialError, MaterialReceipt};
use ziyuan_storage::JsonFileError;

const MAX_ENVELOPE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Damage {
    Corrupt,
    Newer,
    TooLarge,
}

fn damage(result: Result<impl std::fmt::Debug, MaterialError>) -> Damage {
    match result {
        Err(MaterialError::Storage(JsonFileError::Corrupt(_))) => Damage::Corrupt,
        Err(MaterialError::Storage(JsonFileError::Newer {
            found: 2,
            supported: 1,
        })) => Damage::Newer,
        Err(MaterialError::Storage(JsonFileError::TooLarge { limit }))
            if limit == MAX_ENVELOPE_BYTES as u64 =>
        {
            Damage::TooLarge
        }
        other => panic!("损坏资料草稿须报明确存储类别，不得当空草稿：{other:?}"),
    }
}

#[test]
fn damaged_drafts_are_errors_and_never_reset() {
    let f = Fixture::new();
    fs::create_dir_all(f.root.join("translations/empty-dir")).unwrap();
    let seed = save_with(&f, "seed", None, unfinished()).unwrap();
    let file = f.root.join(f.draft_path());
    let original = draft_bytes(&f);
    let good: Value = serde_json::from_slice(&original).unwrap();
    let edit = |change: &dyn Fn(&mut Value)| {
        let mut value = good.clone();
        change(&mut value);
        serde_json::to_vec_pretty(&value).unwrap()
    };
    let other = if f.project == "0".repeat(32) {
        "1".repeat(32)
    } else {
        "0".repeat(32)
    };
    let mut oversize = original.clone();
    oversize.resize(MAX_ENVELOPE_BYTES + 1, b' ');
    let cases: Vec<(&str, Vec<u8>, Damage)> = vec![
        ("empty", vec![], Damage::Corrupt),
        ("not json", b"{not json".to_vec(), Damage::Corrupt),
        (
            "truncated",
            original[..original.len() / 2].to_vec(),
            Damage::Corrupt,
        ),
        (
            "schema zero",
            edit(&|v| v["schema_version"] = json!(0)),
            Damage::Corrupt,
        ),
        (
            "newer schema",
            edit(&|v| v["schema_version"] = json!(2)),
            Damage::Newer,
        ),
        (
            "data type",
            edit(&|v| v["data"] = json!([])),
            Damage::Corrupt,
        ),
        (
            "unknown field",
            edit(&|v| v["data"]["extra"] = json!(true)),
            Damage::Corrupt,
        ),
        (
            "other project",
            edit(&|v| v["data"]["draft"]["project"] = json!(other)),
            Damage::Corrupt,
        ),
        (
            "missing project",
            edit(&|v| {
                v["data"]["draft"]
                    .as_object_mut()
                    .unwrap()
                    .remove("project");
            }),
            Damage::Corrupt,
        ),
        (
            "revision shape",
            edit(&|v| v["data"]["draft"]["revision"] = json!("abc")),
            Damage::Corrupt,
        ),
        (
            "missing requests",
            edit(&|v| {
                v["data"].as_object_mut().unwrap().remove("requests");
            }),
            Damage::Corrupt,
        ),
        (
            "empty requests",
            edit(&|v| v["data"]["requests"] = json!({})),
            Damage::Corrupt,
        ),
        (
            "missing receipt",
            edit(&|v| {
                v["data"]["requests"]["seed"]
                    .as_object_mut()
                    .unwrap()
                    .remove("receipt");
            }),
            Damage::Corrupt,
        ),
        (
            "receipt content mismatch",
            edit(&|v| {
                v["data"]["requests"]["seed"]["receipt"]["content_hash"] = json!("0".repeat(64))
            }),
            Damage::Corrupt,
        ),
        (
            "receipt revision mismatch",
            edit(&|v| v["data"]["requests"]["seed"]["receipt"]["revision"] = json!("0".repeat(32))),
            Damage::Corrupt,
        ),
        (
            "request hash shape",
            edit(&|v| v["data"]["requests"]["seed"]["hash"] = json!("xyz")),
            Damage::Corrupt,
        ),
        (
            "material format",
            edit(&|v| v["data"]["draft"]["material"]["format"] = json!(3)),
            Damage::Corrupt,
        ),
        (
            "material over limit",
            edit(&|v| {
                v["data"]["draft"]["material"]["terms"][0]["source"] = json!("字".repeat(201))
            }),
            Damage::Corrupt,
        ),
        (
            "material type",
            edit(&|v| v["data"]["draft"]["material"]["terms"] = json!({})),
            Damage::Corrupt,
        ),
        ("oversize", oversize, Damage::TooLarge),
    ];
    for (name, bytes, expected) in cases {
        fs::write(&file, &bytes).unwrap();
        let before = tree(&f.root);
        assert_eq!(
            damage(f.service().material_draft(&f.project)),
            expected,
            "{name}"
        );
        assert_eq!(
            damage(save_with(
                &f,
                "next",
                Some(seed.revision.clone()),
                variant("B")
            )),
            expected,
            "{name}"
        );
        assert_eq!(
            damage(save_with(&f, "seed", None, unfinished())),
            expected,
            "{name}"
        );
        assert_eq!(damage(save_with(&f, "fresh", None, unfinished())), expected);
        assert!(tree(&f.root) == before, "{name}：损坏文件不得被重置或覆盖");
    }
    fs::write(&file, &original).unwrap();
    assert_eq!(current(&f).revision, seed.revision);
}

#[test]
fn duplicate_receipt_keys_are_corrupt_not_last_wins() {
    let f = Fixture::new();
    let seed = save_with(&f, "seed", None, unfinished()).unwrap();
    let file = f.root.join(f.draft_path());
    let text = String::from_utf8(draft_bytes(&f)).unwrap();
    let earlier = json!({"hash": "1".repeat(64), "receipt": {
        "revision": "2".repeat(32), "content_hash": "3".repeat(64)}});
    let marker = "\"requests\": {";
    assert_eq!(text.matches(marker).count(), 1);
    let raw = text.replacen(marker, &format!("{marker}\"seed\": {earlier},"), 1);
    fs::write(&file, &raw).unwrap();
    let before = tree(&f.root);
    assert_eq!(
        damage(f.service().material_draft(&f.project)),
        Damage::Corrupt
    );
    assert_eq!(
        damage(save_with(&f, "seed", None, unfinished())),
        Damage::Corrupt
    );
    assert_eq!(
        damage(save_with(&f, "next", Some(seed.revision), variant("B"))),
        Damage::Corrupt
    );
    assert!(tree(&f.root) == before, "重复回执键不得被重置或覆盖");
    assert_eq!(fs::read_to_string(&file).unwrap(), raw);
}

fn root_text(f: &Fixture) -> String {
    f.root.display().to_string()
}

#[test]
fn missing_or_traversing_projects_are_rejected_without_disk_changes() {
    let f = Fixture::new();
    save_with(&f, "seed", None, unfinished()).unwrap();
    let missing = if f.project == "0".repeat(32) {
        "1".repeat(32)
    } else {
        "0".repeat(32)
    };
    let traversal = format!("{}/..", f.project);
    let upper = f.project.to_uppercase();
    let before = tree(&f.root);
    for id in [
        missing.as_str(),
        "",
        ".",
        "..",
        "../x",
        "a/b",
        "a\\b",
        "../../.ziyuan",
        "C:\\Windows",
        "/etc",
        traversal.as_str(),
        upper.as_str(),
    ] {
        let read = f.service().material_draft(id).unwrap_err();
        assert!(
            matches!(read, MaterialError::Project(_)),
            "{id:?}: {read:?}"
        );
        assert!(!read.to_string().contains(&root_text(&f)), "{read}");
        let saved = f
            .service()
            .save_material_draft(
                id,
                &ziyuan_app::translation::MaterialSave {
                    request_id: "x".into(),
                    expected_revision: None,
                    material: unfinished(),
                },
            )
            .unwrap_err();
        assert!(
            matches!(saved, MaterialError::Project(_)),
            "{id:?}: {saved:?}"
        );
        assert!(tree(&f.root) == before, "{id:?} 不得改动盘面");
    }
}

fn assert_path_refusal(f: &Fixture, result: Result<impl std::fmt::Debug, MaterialError>) {
    let error = result.unwrap_err();
    let message = error.to_string();
    assert!(
        matches!(error, MaterialError::Path(_)),
        "路径拒绝须有明确类别：{error:?}"
    );
    assert!(message.contains("拒绝"), "{message}");
    assert!(
        !message.contains(&root_text(f)),
        "不得泄漏根路径：{message}"
    );
}

fn path_refusals_leave_disk_unchanged(f: &Fixture, seed: Option<&MaterialReceipt>) {
    let before = tree(&f.root);
    assert_path_refusal(f, f.service().material_draft(&f.project));
    assert_path_refusal(
        f,
        save_with(f, "next", seed.map(|s| s.revision.clone()), variant("B")),
    );
    assert_path_refusal(f, save_with(f, "fresh", None, variant("C")));
    assert!(tree(&f.root) == before, "路径拒绝不得改动盘面");
}

#[test]
fn non_regular_target_and_lock_are_path_refusals() {
    let f = Fixture::new();
    fs::create_dir_all(f.root.join(f.draft_path())).unwrap();
    path_refusals_leave_disk_unchanged(&f, None);

    let f = Fixture::new();
    let seed = save_with(&f, "seed", None, unfinished()).unwrap();
    let lock = f.root.join(f.lock_path());
    fs::remove_file(&lock).unwrap();
    fs::create_dir(&lock).unwrap();
    path_refusals_leave_disk_unchanged(&f, Some(&seed));
}

#[cfg(unix)]
fn link(original: &Path, link: &Path, _dir: bool) -> bool {
    std::os::unix::fs::symlink(original, link).unwrap();
    true
}

/// Windows needs Developer Mode or the symlink privilege; without it the case is
/// reported as skipped instead of being guessed from another failure.
#[cfg(windows)]
fn link(original: &Path, link: &Path, dir: bool) -> bool {
    let made = if dir {
        std::os::windows::fs::symlink_dir(original, link)
    } else {
        std::os::windows::fs::symlink_file(original, link)
    };
    match made {
        Ok(()) => true,
        Err(error) => {
            println!("SKIP 符号链接用例：本机无创建符号链接权限（{error}）");
            false
        }
    }
}

#[test]
fn symlinked_target_lock_and_parents_are_refused() {
    for kind in ["target", "lock"] {
        let f = Fixture::new();
        let seed = save_with(&f, "seed", None, unfinished()).unwrap();
        let outside = f.root.join("outside.json");
        fs::write(&outside, draft_bytes(&f)).unwrap();
        let path = f.root.join(if kind == "target" {
            f.draft_path()
        } else {
            f.lock_path()
        });
        fs::remove_file(&path).unwrap();
        if !link(&outside, &path, false) {
            return;
        }
        let outside_bytes = fs::read(&outside).unwrap();
        path_refusals_leave_disk_unchanged(&f, Some(&seed));
        assert_eq!(fs::read(&outside).unwrap(), outside_bytes, "{kind}");
    }

    let f = Fixture::new();
    let seed = save_with(&f, "seed", None, unfinished()).unwrap();
    let parent = f.root.join(".ziyuan/text-projects");
    let real = f.root.join("real-text-projects");
    fs::rename(&parent, &real).unwrap();
    if !link(&real, &parent, true) {
        return;
    }
    let before = tree(&f.root);
    for result in [
        f.service().material_draft(&f.project).map(|_| ()),
        save_with(&f, "next", Some(seed.revision.clone()), variant("B")).map(|_| ()),
    ] {
        let error = result.unwrap_err();
        assert!(
            matches!(error, MaterialError::Path(_) | MaterialError::Project(_)),
            "{error:?}"
        );
        assert!(!error.to_string().contains(&root_text(&f)), "{error}");
    }
    assert!(tree(&f.root) == before, "父目录符号链接不得被跟随写入");
}

fn allowed_publication_changes(f: &Fixture) -> BTreeSet<PathBuf> {
    [
        f.draft_path(),
        f.lock_path(),
        Path::new(".ziyuan").join("write.lock"),
        Path::new(".ziyuan").join("write-protocol"),
    ]
    .into_iter()
    .collect()
}

fn assert_published_together(f: &Fixture, request: &str, receipt: &MaterialReceipt) {
    let stored: Value = serde_json::from_slice(&draft_bytes(f)).unwrap();
    assert_eq!(stored["data"]["draft"]["revision"], json!(receipt.revision));
    assert_eq!(
        stored["data"]["requests"][request]["receipt"],
        serde_json::to_value(receipt).unwrap(),
        "正文与回执须在同一次发布中"
    );
}

#[test]
fn orphaned_temp_file_is_neither_consumed_nor_cleaned() {
    let f = Fixture::new();
    fs::create_dir_all(f.root.join("translations/empty-dir")).unwrap();
    let seed = save_with(&f, "seed", None, unfinished()).unwrap();
    let old = draft_bytes(&f);
    // Only the leftover a crashed writer can leave; real exits before and after the rename
    // are covered in `ziyuan-storage` (`process_exit_around_rename_leaves_old_or_new_whole`).
    let stray = f
        .root
        .join(f.draft_path())
        .with_file_name(format!(".material-draft.json.{}.0.0.tmp", u32::MAX));
    fs::write(&stray, &old[..old.len() / 2]).unwrap();
    let before = tree(&f.root);
    assert_eq!(current(&f).revision, seed.revision);
    assert_eq!(draft_bytes(&f), old);
    assert!(tree(&f.root) == before, "读取不得清理或改动中断残留");

    let next = save_with(&f, "next", Some(seed.revision.clone()), variant("B")).unwrap();
    let after = tree(&f.root);
    let changes = changed(&before, &after);
    assert!(changes.contains(&f.draft_path()), "{changes:?}");
    assert!(
        changes.is_subset(&allowed_publication_changes(&f)),
        "{changes:?}"
    );
    assert!(after.contains_key(Path::new("translations/empty-dir")));
    assert_published_together(&f, "next", &next);
    let stored: Value = serde_json::from_slice(&draft_bytes(&f)).unwrap();
    assert_eq!(
        stored["data"]["requests"]["seed"]["receipt"],
        serde_json::to_value(&seed).unwrap(),
        "旧回执须保留"
    );
    assert_eq!(current(&f).material, variant("B"));
}

#[test]
fn lost_response_after_publication_is_confirmed_by_same_request() {
    let f = Fixture::new();
    fs::create_dir_all(f.root.join("translations/empty-dir")).unwrap();
    let before = tree(&f.root);
    let lost = save_with(&f, "lost", None, variant("A")).unwrap();
    let published = tree(&f.root);
    let changes = changed(&before, &published);
    assert!(
        changes.is_subset(&allowed_publication_changes(&f)),
        "{changes:?}"
    );
    assert_published_together(&f, "lost", &lost);

    assert_eq!(save_with(&f, "lost", None, variant("A")).unwrap(), lost);
    assert!(tree(&f.root) == published, "确认重试不得再次发布");
    assert!(matches!(
        save_with(&f, "lost", None, variant("B")),
        Err(MaterialError::RequestConflict)
    ));
    assert!(tree(&f.root) == published);
    assert!(published.contains_key(Path::new("translations/empty-dir")));
}

#[test]
fn lock_timeout_keeps_old_draft_without_false_success() {
    let f = Fixture::new();
    let seed = save_with(&f, "seed", None, unfinished()).unwrap();
    // Windows refuses reads of a locked file, so snapshot before locking.
    let before = tree(&f.root);
    let holder = File::options()
        .write(true)
        .open(f.root.join(f.lock_path()))
        .unwrap();
    holder.lock().unwrap();
    let result = save_with(&f, "next", Some(seed.revision.clone()), variant("B"));
    assert!(
        matches!(
            result,
            Err(MaterialError::Storage(JsonFileError::LockTimeout { .. }))
        ),
        "{result:?}"
    );
    drop(holder);
    assert!(tree(&f.root) == before, "锁超时不得发布");
    assert_eq!(current(&f).revision, seed.revision);
    assert_eq!(
        save_with(&f, "next", Some(seed.revision), variant("B"))
            .unwrap()
            .content_hash
            .len(),
        64
    );
}
