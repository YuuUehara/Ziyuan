use super::support::{Fixture, PROJECT_ENV, changed, is_lower_hex, tree, unfinished};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use ziyuan_app::text_project::{NewProject, Provenance, TextProjects};
use ziyuan_app::translation::{
    Glossary, MaterialDraft, MaterialError, MaterialIssueCode, MaterialPathSegment,
    MaterialReceipt, MaterialSave, Translations, VersionState, validate_material,
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

/// 类别、范围、可用起点、读音、证据与称谓规则都经 CSV／TBX 往返后保持不变。
#[test]
fn termbase_full_fields_roundtrip_csv_and_tbx() {
    for tbx in [false, true] {
        let f = Fixture::new();
        let t = f.service();
        let seeded = super::support::material(super::support::sample());
        t.save_material_draft(
            &f.project,
            &MaterialSave {
                request_id: "seeded".into(),
                expected_revision: None,
                material: seeded.clone(),
            },
        )
        .unwrap();
        let exported = if tbx {
            t.export_termbase_tbx(&f.project).unwrap()
        } else {
            t.export_termbase_csv(&f.project).unwrap()
        };
        if !tbx {
            let header = exported.lines().next().unwrap();
            for column in [
                "category",
                "scope",
                "available_from",
                "reading",
                "evidence",
                "target",
                "speaker",
                "term",
                "owner",
                "terms",
                "voice",
                "examples",
            ] {
                assert!(header.split(',').any(|c| c == column), "缺列 {column}");
            }
            assert!(exported.contains("title"), "{exported}");
            assert!(exported.contains("range:z:pz..a:pa"), "{exported}");
            assert!(exported.contains("よみ"), "{exported}");
            assert!(exported.contains("person:p_1"), "{exported}");
        } else {
            for descrip in [
                "category",
                "scope",
                "available_from",
                "reading",
                "evidence",
                "address_rules",
            ] {
                assert!(
                    exported.contains(&format!("type=\"{descrip}\"")),
                    "缺 {descrip}"
                );
            }
        }
        let receipt = if tbx {
            t.import_termbase_tbx(&f.project, exported.as_bytes())
                .unwrap()
        } else {
            t.import_termbase_csv(&f.project, exported.as_bytes())
                .unwrap()
        };
        assert!(!receipt.revision.is_empty());
        let after = super::support::current(&f).material;
        assert_eq!(
            after,
            seeded,
            "{} 往返不一致",
            if tbx { "TBX" } else { "CSV" }
        );
        let exported_again = if tbx {
            t.export_termbase_tbx(&f.project).unwrap()
        } else {
            t.export_termbase_csv(&f.project).unwrap()
        };
        assert_eq!(exported, exported_again);
    }
}

#[test]
fn version_snapshots_compare_and_prune_keep_manual_by_default() {
    let f = Fixture::new();
    let t = f.service();
    let project = t.project(&f.project).unwrap();
    let block = project.chapters[0].blocks[0].block_id.clone();
    let prompt = project.chapters[0].blocks[0].clone();
    let dir = f.root.join("translations").join(&f.project).join(&block);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("zh.json"), prompt.serialized().unwrap()).unwrap();

    let first = t.snapshot_translation(&f.project, &block, "初稿").unwrap();
    assert_eq!(first.state, VersionState::Manual);
    assert_eq!(
        t.list_translation_versions(&f.project, &block)
            .unwrap()
            .len(),
        1
    );

    let mut changed = prompt.clone();
    changed.paragraphs[0].text = "改过的第一段".into();
    fs::write(dir.join("zh.json"), changed.serialized().unwrap()).unwrap();
    let diff = t
        .compare_version(&f.project, &block, &first.id, false)
        .unwrap();
    assert_eq!(diff.len(), 1);
    assert_eq!(diff[0].kind, "changed");

    // 没有人工草稿时，与草稿比较必须报错，不拿现行译稿冒充草稿。
    assert!(
        t.compare_version(&f.project, &block, &first.id, true)
            .is_err()
    );

    for label in ["乙", "丙", "丁"] {
        t.snapshot_translation(&f.project, &block, label).unwrap();
    }
    // 手工快照默认一律保留；显式包含人工后才按份数预览。
    let kept = t
        .prune_versions(&f.project, &block, 1, false, true)
        .unwrap();
    assert!(kept.candidates.is_empty(), "{kept:?}");
    let preview = t
        .prune_versions(&f.project, &block, 1, true, false)
        .unwrap();
    assert_eq!(preview.candidates.len(), 3);
    assert!(preview.deleted.is_empty());
    let applied = t.prune_versions(&f.project, &block, 1, true, true).unwrap();
    assert_eq!(applied.deleted.len(), 3);
    assert_eq!(
        t.list_translation_versions(&f.project, &block)
            .unwrap()
            .len(),
        1
    );
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
    let comment = t
        .comment_add(&f.project, "c0001-b0001", "c0001-p0001", "这里再柔和些")
        .unwrap();
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
    t.save_material_draft(
        &f.project,
        &MaterialSave {
            request_id: "tbx-save".into(),
            expected_revision: None,
            material,
        },
    )
    .unwrap();
    let tbx = t.export_termbase_tbx(&f.project).unwrap();
    assert!(tbx.contains("<termEntry id=\"t-1\">"));
    assert!(tbx.contains("<termEntry id=\"p_1\">"));
    let receipt = t.import_termbase_tbx(&f.project, tbx.as_bytes()).unwrap();
    assert!(!receipt.revision.is_empty());
}

#[test]
fn srt_import_creates_project_paragraphs() {
    let root = std::env::temp_dir().join(format!("ziyuan-srt-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let texts = TextProjects::new(&root);
    let project = texts
        .create(NewProject {
            title: "字幕".into(),
            source_language: "ja".into(),
            target_language: "zh-CN".into(),
            provenance: Provenance::default(),
        })
        .unwrap();
    let srt = root.join("a.srt");
    fs::write(&srt, "1\n00:00:01,000 --> 00:00:02,000\n夜が明ける。\n\n2\n00:00:03,000 --> 00:00:04,000\n朝だ。\n").unwrap();
    let cues = Translations::new(&root)
        .import_srt(&project.id, &srt)
        .unwrap();
    assert_eq!(cues, 2);
    let loaded = Translations::new(&root).project(&project.id).unwrap();
    assert_eq!(loaded.chapters[0].paragraphs.len(), 2);
    let _ = fs::remove_dir_all(&root);
}

fn zip_bytes(files: &[(&str, &str)]) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        for (name, content) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf.into_inner()
}

#[test]
fn docx_and_epub_import_plain_text() {
    let root = std::env::temp_dir().join(format!("ziyuan-formats-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let texts = TextProjects::new(&root);
    let make = |title: &str| {
        texts
            .create(NewProject {
                title: title.into(),
                source_language: "ja".into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap()
    };
    let docx = root.join("a.docx");
    fs::write(&docx, zip_bytes(&[("word/document.xml", r#"<w:document><w:body><w:p><w:r><w:t>夜が明ける。</w:t></w:r></w:p><w:p><w:r><w:t>朝だ。</w:t></w:r></w:p></w:body></w:document>"#)])).unwrap();
    let docx_project = make("DOCX");
    Translations::new(&root)
        .import_docx(&docx_project.id, &docx)
        .unwrap();
    let loaded = Translations::new(&root).project(&docx_project.id).unwrap();
    assert_eq!(loaded.chapters[0].paragraphs.len(), 2);

    let epub = root.join("a.epub");
    fs::write(&epub, zip_bytes(&[
        ("META-INF/container.xml", r#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#),
        ("OEBPS/content.opf", r#"<package><manifest><item id="c1" href="c1.xhtml"/></manifest><spine><itemref idref="c1"/></spine></package>"#),
        ("OEBPS/c1.xhtml", r#"<html><body><p>夜が明ける。</p><p>朝だ。</p></body></html>"#),
    ])).unwrap();
    let epub_project = make("EPUB");
    Translations::new(&root)
        .import_epub(&epub_project.id, &epub)
        .unwrap();
    let loaded = Translations::new(&root).project(&epub_project.id).unwrap();
    assert_eq!(loaded.chapters[0].paragraphs.len(), 2);
    let _ = fs::remove_dir_all(&root);
}

/// 回填保留粗体、斜体、表格与图片，并只替换文字节点。
#[test]
fn docx_and_epub_write_back_preserve_markup() {
    fn write_zh(t: &Translations, project: &str, texts: &[&str]) {
        let p = t.project(project).unwrap();
        let block = &p.chapters[0].blocks[0];
        let paragraphs: Vec<_> = block
            .paragraphs
            .iter()
            .zip(texts)
            .map(|(source, text)| json!({"id": source.id, "text": text}))
            .collect();
        let dir = t
            .root()
            .join("translations")
            .join(project)
            .join(&block.block_id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("zh.json"),
            serde_json::to_vec_pretty(&json!({
                "schema_version": 1,
                "block_id": block.block_id,
                "paragraphs": paragraphs,
            }))
            .unwrap(),
        )
        .unwrap();
    }
    fn entry(bytes: &[u8], name: &str) -> String {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut file = archive.by_name(name).unwrap();
        let mut out = String::new();
        std::io::Read::read_to_string(&mut file, &mut out).unwrap();
        out
    }

    let root = std::env::temp_dir().join(format!("ziyuan-writeback-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let texts = TextProjects::new(&root);
    let make = |title: &str| {
        texts
            .create(NewProject {
                title: title.into(),
                source_language: "ja".into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap()
    };
    let t = Translations::new(&root);

    let docx = root.join("in.docx");
    let docx_xml = r#"<w:document><w:body><w:p><w:r><w:rPr><w:b/></w:rPr><w:t>夜が明ける。</w:t></w:r><w:r><w:rPr><w:i/></w:rPr><w:t>朝だ。</w:t></w:r></w:p><w:p><w:r><w:drawing/></w:r><w:r><w:t>終</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>表</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>"#;
    fs::write(&docx, zip_bytes(&[("word/document.xml", docx_xml)])).unwrap();
    let docx_project = make("DOCX 回填");
    t.import_docx(&docx_project.id, &docx).unwrap();
    write_zh(&t, &docx_project.id, &["天亮。", "结束。", "表格。"]);
    let docx_out = root.join("out.docx");
    assert_eq!(t.export_docx(&docx_project.id, &docx_out).unwrap(), 3);
    let out = fs::read(&docx_out).unwrap();
    let xml = entry(&out, "word/document.xml");
    for marker in [
        "<w:b/>",
        "<w:i/>",
        "<w:drawing/>",
        "<w:tbl>",
        "天亮",
        "结束",
        "表格",
    ] {
        assert!(xml.contains(marker), "DOCX 回填缺 {marker}：{xml}");
    }

    let epub = root.join("in.epub");
    fs::write(
        &epub,
        zip_bytes(&[
            ("mimetype", "application/epub+zip"),
            ("META-INF/container.xml", r#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#),
            ("OEBPS/content.opf", r#"<package><manifest><item id="c1" href="c1.xhtml"/></manifest><spine><itemref idref="c1"/></spine></package>"#),
            ("OEBPS/c1.xhtml", r#"<html><body><p>夜<b>明</b><img src="x.png"/></p><table><tr><td>表</td></tr></table></body></html>"#),
        ]),
    )
    .unwrap();
    let epub_project = make("EPUB 回填");
    t.import_epub(&epub_project.id, &epub).unwrap();
    write_zh(&t, &epub_project.id, &["天亮", "表格"]);
    let epub_out = root.join("out.epub");
    assert_eq!(t.export_epub(&epub_project.id, &epub_out).unwrap(), 2);
    let out = fs::read(&epub_out).unwrap();
    {
        let mut archive = zip::ZipArchive::new(Cursor::new(out.as_slice())).unwrap();
        assert_eq!(archive.by_index(0).unwrap().name().unwrap(), "mimetype");
    }
    let xhtml = entry(&out, "OEBPS/c1.xhtml");
    for marker in ["<b>", "<img", "<table>", "<td>", "表格"] {
        assert!(xhtml.contains(marker), "EPUB 回填缺 {marker}：{xhtml}");
    }
    // 粗体范围仍在：原 `夜<b>明</b>` 的两个文字节点各得一部分译文。
    assert!(
        xhtml.contains("天<b>亮</b>"),
        "EPUB 回填未保留粗体范围：{xhtml}"
    );
    // 已有输出文件时拒绝覆盖。
    assert!(t.export_epub(&epub_project.id, &epub_out).is_err());
    let _ = fs::remove_dir_all(&root);
}
