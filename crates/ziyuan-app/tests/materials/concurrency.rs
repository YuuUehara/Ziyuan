use super::support::{
    Fixture, PROJECT_ENV, assert_conflict, current, draft_bytes, receipt_count, save_with, tree,
    variant,
};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;
use ziyuan_app::translation::{MaterialError, MaterialReceipt, MaterialSave, Translations};

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
        .args([
            "--exact",
            "concurrency::material_retry_child",
            "--nocapture",
        ])
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
