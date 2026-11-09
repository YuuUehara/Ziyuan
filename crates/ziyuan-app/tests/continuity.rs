use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use ziyuan_app::{
    text_project::{NewProject, Project, Provenance, TextProjects},
    translation::Translations,
};

struct Fx {
    root: PathBuf,
    project: Project,
}
impl Fx {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "ziyuan-continuity-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let store = TextProjects::new(&root);
        let p = store
            .create(NewProject {
                title: "自编样章".into(),
                source_language: "ja".into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap();
        let source = root.join("source.txt");
        fs::write(&source, "勇者は城へ。\n\n夜だった。\n").unwrap();
        let project = store.import(&p.id, &[source]).unwrap();
        Self { root, project }
    }
    fn t(&self) -> Translations {
        Translations::new(&self.root)
    }
}
impl Drop for Fx {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn literal_search_finds_untranslated_sources_and_draft_replace_preserves_approval() {
    let f = Fx::new();
    let found = f.t().search_text(&f.project.id, "勇者", "source").unwrap();
    assert_eq!(found.hits.len(), 1);
    assert_eq!(found.hits[0].paragraph, "c0001-p0001");
    let view = f.t().human_draft(&f.project.id, "c0001-b0001").unwrap();
    f.t()
        .save_human_draft(
            &f.project.id,
            "c0001-b0001",
            &ziyuan_app::translation::DraftSave {
                request_id: "replace-test".into(),
                expected_revision: None,
                basis: view.basis,
                paragraphs: vec![ziyuan_app::translation::DraftParagraph {
                    id: "c0001-p0001".into(),
                    text: "勇士到城。".into(),
                    marks: vec![ziyuan_app::translation::TargetMark {
                        start: 9,
                        end: 12,
                        kind: ziyuan_app::translation::TargetMarkKind::Strong {},
                    }],
                }],
            },
        )
        .unwrap();
    let request = ziyuan_app::translation::DraftReplace {
        block: "c0001-b0001".into(),
        paragraph: "c0001-p0001".into(),
        from: "勇士".into(),
        to: "勇敢的人".into(),
        fingerprint: None,
        confirm: false,
    };
    let preview = f.t().replace_draft_text(&f.project.id, &request).unwrap();
    assert_eq!(preview.after, "勇敢的人到城。");
    let mut confirm = request.clone();
    confirm.confirm = true;
    confirm.fingerprint = Some("stale".into());
    assert!(f.t().replace_draft_text(&f.project.id, &confirm).is_err());
    confirm.fingerprint = Some(preview.fingerprint);
    f.t().replace_draft_text(&f.project.id, &confirm).unwrap();
    let draft = f
        .t()
        .peek_human_draft(&f.project.id, "c0001-b0001")
        .unwrap()
        .draft
        .unwrap();
    assert_eq!(draft.paragraphs[0].marks[0].start, 15);
    assert!(
        f.t()
            .read_translation(&f.project.id, "c0001-b0001")
            .unwrap()
            .is_none()
    );
    let overlap = ziyuan_app::translation::DraftReplace {
        from: "城".into(),
        to: "镇".into(),
        ..confirm
    };
    assert!(f.t().replace_draft_text(&f.project.id, &overlap).is_err());
}

#[test]
fn complete_backup_restores_sources_drafts_versions_and_court_without_credentials() {
    let f = Fx::new();
    let _court = ziyuan_app::Session::found(ziyuan_storage::EventLog::new(&f.root)).unwrap();
    let t = f.t();
    t.ensure_context_draft(&f.project.id).unwrap();
    t.approve_context(&f.project.id).unwrap();
    t.export(&f.project.id, "c0001-b0001").unwrap();
    let material = t.block_dir(&f.project.id, "c0001-b0001");
    fs::write(material.join("zh.json"), b"draft bytes").unwrap();
    fs::write(f.root.join(".ziyuan/web-token"), b"SECRET").unwrap();
    let archive = t.backup_workspace().unwrap();
    assert_eq!(
        archive.manifest.projects,
        std::slice::from_ref(&f.project.id)
    );
    assert!(
        !archive
            .manifest
            .files
            .iter()
            .any(|f| f.path.contains("token"))
    );
    let target = f.root.join("restored");
    ziyuan_app::translation::restore_backup(&archive.bytes, &target).unwrap();
    assert_eq!(
        TextProjects::new(&target).show(&f.project.id).unwrap(),
        f.project
    );
    assert_eq!(
        fs::read(
            target
                .join("translations")
                .join(&f.project.id)
                .join("c0001-b0001/zh.json")
        )
        .unwrap(),
        b"draft bytes"
    );
    assert_eq!(
        Translations::new(&target)
            .current_context(&f.project.id)
            .unwrap()
            .unwrap()
            .version,
        1
    );
    ziyuan_app::Session::open(ziyuan_storage::EventLog::new(&target)).unwrap();
    assert!(!target.join(".ziyuan/web-token").exists());
    assert!(ziyuan_app::translation::restore_backup(&archive.bytes, &target).is_err());
}

#[test]
fn backup_rejects_corruption_before_creating_destination() {
    let f = Fx::new();
    let mut archive = f.t().backup_workspace().unwrap();
    let needle = "勇者は城へ。".as_bytes();
    let index = archive
        .bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .unwrap();
    archive.bytes[index] ^= 1;
    let target = f.root.join("corrupt-restore");
    assert!(ziyuan_app::translation::restore_backup(&archive.bytes, &target).is_err());
    assert!(!target.exists());
}

#[test]
fn backup_refuses_to_silently_skip_a_broken_project() {
    let f = Fx::new();
    let broken = f
        .root
        .join(".ziyuan/text-projects/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("project.json"), b"broken").unwrap();
    assert!(f.t().backup_workspace().is_err());
}

#[test]
fn source_revision_refuses_to_break_approved_evidence() {
    let f = Fx::new();
    f.t().ensure_context_draft(&f.project.id).unwrap();
    let path = f
        .root
        .join("translations")
        .join(&f.project.id)
        .join("context/glossary.json");
    fs::write(path, serde_json::json!({"format":2,"terms":[{"term_id":"hero","source":"勇者","category":"common","allowed":["勇士"],"scope":{"kind":"project"},"evidence":[{"paragraph":"c0001-p0001","quote":"城","explanation":"自编样本","stance":"project_fact","status":"confirmed"}],"status":"confirmed"}],"persons":[]}).to_string()).unwrap();
    f.t().approve_context(&f.project.id).unwrap();
    let update = ziyuan_app::translation::SourceUpdate {
        chapter: Some("c0001".into()),
        name: "source.txt".into(),
        text: "勇者は海へ。\n\n夜だった。\n".into(),
    };
    let preview = f.t().preview_source_update(&f.project.id, &update).unwrap();
    assert!(preview.blockers.iter().any(|b| b.contains("证据")));
    assert!(
        f.t()
            .apply_source_update(&f.project.id, &update, &preview.fingerprint)
            .is_err()
    );
    assert_eq!(f.t().project(&f.project.id).unwrap(), f.project);
}

#[test]
fn source_append_and_revision_preserve_previous_translation_and_originals() {
    use ziyuan_app::translation::SourceUpdate;
    let f = Fx::new();
    f.t().export(&f.project.id, "c0001-b0001").unwrap();
    let target = f
        .t()
        .block_dir(&f.project.id, "c0001-b0001")
        .join("zh.json");
    fs::write(&target, b"old translation").unwrap();
    let old_source = f
        .root
        .join(".ziyuan/text-projects")
        .join(&f.project.id)
        .join(&f.project.chapters[0].original_path);
    let append = SourceUpdate {
        chapter: None,
        name: "next.txt".into(),
        text: "次の章。\n".into(),
    };
    let preview = f.t().preview_source_update(&f.project.id, &append).unwrap();
    assert!(preview.blockers.is_empty());
    assert!(
        f.t()
            .apply_source_update(&f.project.id, &append, "stale")
            .is_err()
    );
    f.t()
        .apply_source_update(&f.project.id, &append, &preview.fingerprint)
        .unwrap();
    let after = f.t().project(&f.project.id).unwrap();
    assert_eq!(after.chapters[0], f.project.chapters[0]);
    assert_eq!(after.chapters[1].id, "c0002");
    assert!(
        f.t()
            .apply_source_update(&f.project.id, &append, &preview.fingerprint)
            .is_err()
    );
    let replace = SourceUpdate {
        chapter: Some("c0001".into()),
        name: "source.txt".into(),
        text: "勇者は海へ。\n\n夜だった。\n".into(),
    };
    let preview = f
        .t()
        .preview_source_update(&f.project.id, &replace)
        .unwrap();
    assert_eq!(preview.changed_blocks, ["c0001-b0001"]);
    f.t()
        .apply_source_update(&f.project.id, &replace, &preview.fingerprint)
        .unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"old translation");
    assert!(fs::read_to_string(old_source).unwrap().contains("城"));
    assert!(
        f.t().project(&f.project.id).unwrap().chapters[0].paragraphs[0]
            .text
            .contains("海")
    );
}

#[test]
fn source_revision_rejects_segment_remapping_before_writing() {
    let f = Fx::new();
    let update = ziyuan_app::translation::SourceUpdate {
        chapter: Some("c0001".into()),
        name: "source.txt".into(),
        text: "全く別の一段。\n".into(),
    };
    let preview = f.t().preview_source_update(&f.project.id, &update).unwrap();
    assert!(!preview.blockers.is_empty());
    assert!(
        f.t()
            .apply_source_update(&f.project.id, &update, &preview.fingerprint)
            .is_err()
    );
    assert_eq!(f.t().project(&f.project.id).unwrap(), f.project);
}

#[test]
fn cross_chapter_issue_keeps_history_and_requires_explicit_recheck_after_edit() {
    use ziyuan_app::translation::{IssueInput, IssueReview, IssueState};
    let f = Fx::new();
    let update = ziyuan_app::translation::SourceUpdate {
        chapter: None,
        name: "second.txt".into(),
        text: "勇者は帰った。".into(),
    };
    let preview = f.t().preview_source_update(&f.project.id, &update).unwrap();
    f.t()
        .apply_source_update(&f.project.id, &update, &preview.fingerprint)
        .unwrap();
    let input = IssueInput {
        paragraph: "c0001-p0001".into(),
        related: vec!["c0002-p0001".into()],
        rule: "人物称呼".into(),
        body: "两段称呼是否有意不同？".into(),
    };
    let list = f.t().issues(&f.project.id).unwrap();
    let created = f
        .t()
        .add_issue(&f.project.id, &input, &list.fingerprint)
        .unwrap();
    assert_eq!(
        f.t()
            .add_issue(&f.project.id, &input, &list.fingerprint)
            .unwrap()
            .issue_id,
        created.issue_id
    );
    let view = f.t().issues(&f.project.id).unwrap();
    let review = IssueReview {
        version: created.version,
        fingerprint: view.issues[0].fingerprint.clone(),
        state: IssueState::Intentional,
        reason: "称呼者不同，自编例外".into(),
        recheck: false,
    };
    f.t()
        .review_issue(&f.project.id, created.issue_id, &review)
        .unwrap();
    let file = f
        .t()
        .block_dir(&f.project.id, "c0001-b0001")
        .join("zh.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let mut target = f.project.chapters[0].blocks[0].clone();
    target.paragraphs[0].text = "新的译文。".into();
    fs::write(file, target.serialized().unwrap()).unwrap();
    let stale = f.t().issues(&f.project.id).unwrap();
    assert!(!stale.issues[0].current);
    let mut retry = IssueReview {
        version: stale.issues[0].record.version,
        fingerprint: stale.issues[0].fingerprint.clone(),
        ..review
    };
    assert!(
        f.t()
            .review_issue(&f.project.id, created.issue_id, &retry)
            .is_err()
    );
    retry.recheck = true;
    retry.state = IssueState::Pending;
    f.t()
        .review_issue(&f.project.id, created.issue_id, &retry)
        .unwrap();
    let current = f.t().issues(&f.project.id).unwrap();
    assert!(current.issues[0].current);
    assert_eq!(current.issues[0].history.len(), 3);
}

#[test]
fn continuous_reading_and_delivery_preflight_are_readonly_and_do_not_grant_signoff() {
    let f = Fx::new();
    let reading = f.t().read_chapter(&f.project.id, Some("c0001")).unwrap();
    assert_eq!(reading.paragraphs.len(), 2);
    assert_eq!(reading.paragraphs[0].source, "勇者は城へ。");
    assert!(reading.paragraphs.iter().all(|p| p.translation.is_none()));
    assert!(f.t().read_chapter(&f.project.id, Some("missing")).is_err());
    let report = f
        .t()
        .delivery_preflight(&f.project.id, &Default::default())
        .unwrap();
    assert!(!report.export_ready);
    assert!(!report.blockers.is_empty());
    assert!(!f.root.join(".ziyuan/court.log").exists());
}
