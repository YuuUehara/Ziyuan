use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
};
use ziyuan_app::{
    text_project::{NewProject, Provenance, TextProjects},
    translation::Translations,
};
struct Fx {
    root: PathBuf,
    id: String,
}
impl Fx {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "ziyuan-local-safety-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let store = TextProjects::new(&root);
        let p = store
            .create(NewProject {
                title: "自编".into(),
                source_language: "ja".into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap();
        let source = root.join("source.txt");
        fs::write(&source, "一。\n\n二。\n").unwrap();
        store.import(&p.id, &[source]).unwrap();
        Self { root, id: p.id }
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
fn local_memory_and_comments_reject_invalid_project_and_foreign_anchors() {
    let f = Fx::new();
    assert!(f.t().tm_add("../escape", "a", "b").is_err());
    assert!(f.t().tm_list("../escape").is_err());
    assert!(f.t().comment_add("../escape", "b", "", "x").is_err());
    assert!(f.t().comment_add(&f.id, "missing", "", "x").is_err());
    assert!(
        f.t()
            .comment_add(&f.id, "c0001-b0001", "foreign", "x")
            .is_err()
    );
}
#[test]
fn concurrent_memory_and_comments_do_not_lose_updates() {
    let f = Fx::new();
    let barrier = Arc::new(Barrier::new(8));
    let threads = (0..8)
        .map(|n| {
            let root = f.root.clone();
            let id = f.id.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let t = Translations::new(root);
                t.tm_add(&id, &format!("s{n}"), &format!("t{n}")).unwrap();
                t.comment_add(&id, "c0001-b0001", "c0001-p0001", &format!("c{n}"))
                    .unwrap();
            })
        })
        .collect::<Vec<_>>();
    for h in threads {
        h.join().unwrap();
    }
    assert_eq!(f.t().tm_list(&f.id).unwrap().len(), 8);
    assert_eq!(f.t().comment_list(&f.id, None).unwrap().len(), 8);
}
#[test]
fn srt_export_requires_signoff_and_preserves_existing_output() {
    let f = Fx::new();
    let dir = f.t().block_dir(&f.id, "c0001-b0001");
    fs::create_dir_all(&dir).unwrap();
    let mut target = f.t().project(&f.id).unwrap().chapters[0].blocks[0].clone();
    for p in &mut target.paragraphs {
        p.text = "译文".into();
    }
    fs::write(dir.join("zh.json"), target.serialized().unwrap()).unwrap();
    let side = f
        .root
        .join(".ziyuan/text-projects")
        .join(&f.id)
        .join("srt.json");
    fs::write(side,r#"[{"index":1,"start":"00:00:00,000","end":"00:00:01,000","text":"一"},{"index":2,"start":"00:00:01,000","end":"00:00:02,000","text":"二"}]"#).unwrap();
    let output = f.root.join("output.srt");
    fs::write(&output, b"KEEP").unwrap();
    assert!(f.t().export_srt(&f.id, &output).is_err());
    assert_eq!(fs::read(&output).unwrap(), b"KEEP");
    assert!(
        f.t()
            .export_srt(&f.id, &f.root.join("unapproved.srt"))
            .is_err()
    );
}

#[test]
fn legacy_plain_json_stays_readable_and_future_versions_are_not_overwritten() {
    let f = Fx::new();
    let dir = f.root.join(".ziyuan/text-projects").join(&f.id);
    let tm = dir.join("tm.json");
    fs::write(
        &tm,
        r#"{"entries":[{"id":"tm-1","source":"一","target":"甲"}]}"#,
    )
    .unwrap();
    assert_eq!(f.t().tm_list(&f.id).unwrap()[0].target, "甲");
    f.t().tm_add(&f.id, "二", "乙").unwrap();
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&tm).unwrap()).unwrap();
    assert!(value["entries"].is_array());
    assert!(value.get("schema_version").is_none());
    let future = br#"{"version":99,"entries":[]}"#;
    fs::write(&tm, future).unwrap();
    assert!(f.t().tm_add(&f.id, "x", "y").is_err());
    assert_eq!(fs::read(&tm).unwrap(), future);
}

#[test]
fn malformed_srt_is_refused_without_importing_any_source() {
    let f = Fx::new();
    let texts = TextProjects::new(&f.root);
    let p = texts
        .create(NewProject {
            title: "坏字幕".into(),
            source_language: "ja".into(),
            target_language: "zh-CN".into(),
            provenance: Provenance::default(),
        })
        .unwrap();
    let path = f.root.join("bad.srt");
    for bad in [
        "1\n00:99:00,000 --> 00:00:01,000\nx\n",
        "1\n00:00:02,000 --> 00:00:01,000\nx\n",
        "junk\n00:00:00,000 --> 00:00:01,000\nx\n",
    ] {
        fs::write(&path, bad).unwrap();
        assert!(f.t().import_srt(&p.id, &path).is_err());
        assert!(texts.show(&p.id).unwrap().chapters.is_empty());
    }
}

#[test]
fn memory_build_does_not_collect_unapproved_disk_translations() {
    let f = Fx::new();
    let directory = f.t().block_dir(&f.id, "c0001-b0001");
    fs::create_dir_all(&directory).unwrap();
    let mut target = f.t().project(&f.id).unwrap().chapters[0].blocks[0].clone();
    for p in &mut target.paragraphs {
        p.text = "未批准译文".into();
    }
    fs::write(directory.join("zh.json"), target.serialized().unwrap()).unwrap();
    assert_eq!(f.t().tm_build(&f.id).unwrap(), 0);
    drop(ziyuan_app::Session::found(ziyuan_storage::EventLog::new(&f.root)).unwrap());
    assert_eq!(f.t().tm_build(&f.id).unwrap(), 0);
    assert!(f.t().tm_list(&f.id).unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn local_memory_and_comments_refuse_symbolic_link_files() {
    use std::os::unix::fs::symlink;
    let f = Fx::new();
    let external = f.root.join("outside.json");
    fs::write(&external, b"KEEP").unwrap();
    for name in ["tm.json", "comments.json"] {
        let link = f.root.join(".ziyuan/text-projects").join(&f.id).join(name);
        symlink(&external, &link).unwrap();
        if name == "tm.json" {
            assert!(f.t().tm_add(&f.id, "x", "y").is_err());
        } else {
            assert!(
                f.t()
                    .comment_add(&f.id, "c0001-b0001", "c0001-p0001", "x")
                    .is_err()
            );
        }
        assert_eq!(fs::read(&external).unwrap(), b"KEEP");
    }
}
