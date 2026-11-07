//! 第二版生产拒绝边界与第一版逐字节兼容。
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use ziyuan_app::notes::{NewNote, NoteKind, Notes};
use ziyuan_app::text_project::{NewProject, Project, Provenance, TextProjects};
use ziyuan_app::translation::{Translations, context_dir_rel};

const V2: &str = "{\"format\":2,\"terms\":[],\"persons\":[]}\n";
const LEGACY: &str = "[ { \"source\": \"勇者\", \"allowed\": [\"勇士\"], \"scope\": \"第二章以后；先人工核对\", \"note\": \"统一\" } ]\n";

struct Fx {
    root: PathBuf,
    project: Project,
}

impl Fx {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "ziyuan-context-v2-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let texts = TextProjects::new(&root);
        let p = texts
            .create(NewProject {
                title: "格式对照".into(),
                source_language: "ja".into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap();
        let source = root.join("a.txt");
        fs::write(&source, "勇者は旅立った。\n\n城へ。\n").unwrap();
        let project = texts.import(&p.id, &[source]).unwrap();
        Self { root, project }
    }
    fn t(&self) -> Translations {
        Translations::new(&self.root)
    }
    fn draft(&self) -> PathBuf {
        self.root.join(context_dir_rel(&self.project.id))
    }
    fn write_glossary_only(&self, body: &str) {
        fs::create_dir_all(self.draft()).unwrap();
        fs::write(self.draft().join("glossary.json"), body).unwrap();
    }
    fn note(&self, kind: NoteKind) -> String {
        Notes::new(&self.root)
            .add(NewNote {
                kind,
                source: "勇者".into(),
                zh: "勇者大人".into(),
                remark: String::new(),
                project: None,
                block: None,
                paragraph: None,
            })
            .unwrap()
            .id
    }
    fn block(&self) -> &str {
        &self.project.chapters[0].blocks[0].block_id
    }
    fn snapshot_dir(&self) -> PathBuf {
        self.root
            .join(".ziyuan/text-projects")
            .join(&self.project.id)
            .join("context/v1")
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// 目录也纳入对照，避免只比较已有文件而漏掉空目录。
#[allow(dead_code)]
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

#[allow(dead_code)]
fn refused<T: std::fmt::Debug>(result: Result<T, impl std::fmt::Display>) {
    let error = result.expect_err("第二版不得进入生产消费");
    assert!(
        error.to_string().contains("人物／术语资料尚未开放"),
        "{error}"
    );
}

#[test]
fn legacy_context_bytes_scope_hints_and_note_promotion_stay_unchanged() {
    let f = Fx::new();
    let t = f.t();
    f.write_glossary_only(LEGACY);
    t.ensure_context_draft(&f.project.id).unwrap();
    assert_eq!(t.approve_context(&f.project.id).unwrap(), 1);
    assert_eq!(t.approve_context(&f.project.id).unwrap(), 1);
    assert_eq!(
        fs::read(f.snapshot_dir().join("glossary.json")).unwrap(),
        LEGACY.as_bytes()
    );
    let snapshot = t.context_snapshot(&f.project.id, 1).unwrap();
    match &snapshot.glossary {
        ziyuan_app::translation::Glossary::Legacy(entries) => {
            assert_eq!(entries[0].scope.as_deref(), Some("第二章以后；先人工核对"))
        }
        ziyuan_app::translation::Glossary::Structured(_) => panic!("应为旧数组资料"),
    }
    t.export(&f.project.id, f.block()).unwrap();
    let dir = t.block_dir(&f.project.id, f.block());
    let exported = fs::read(dir.join("context.json")).unwrap();
    assert_eq!(
        String::from_utf8(exported.clone()).unwrap(),
        "{\n  \"glossary\": [\n    {\n      \"allowed\": [\n        \"勇士\"\n      ],\n      \"note\": \"统一\",\n      \"scope\": \"第二章以后；先人工核对\",\n      \"source\": \"勇者\"\n    }\n  ],\n  \"note\": \"已批准的上下文快照（只读）：术语表 glossary 的 allowed 为允许译法；summaries 为此前各章的前情摘要。\",\n  \"style\": \"# 文体\\n\\n（在此写明译文的语气、人称、用字等要求。）\\n\",\n  \"summaries\": {},\n  \"version\": 1\n}\n"
    );
    let mut target = f.project.chapters[0].blocks[0].clone();
    for p in &mut target.paragraphs {
        p.text = "译文没有允许译法。".into();
    }
    fs::write(dir.join("zh.json"), target.serialized().unwrap()).unwrap();
    let hints = t
        .verification(None, &f.project.id, f.block())
        .unwrap()
        .hints;
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].source, "勇者");
    assert_eq!(hints[0].allowed, ["勇士"]);
    assert_eq!(hints[0].scope.as_deref(), Some("第二章以后；先人工核对"));
    assert_eq!(
        hints[0].message,
        "原文命中词条而译文未见允许译法；作用范围为自由文本，先人工核对是否适用"
    );
    let id = f.note(NoteKind::Word);
    assert!(
        Notes::new(&f.root)
            .promote(&id, &f.project.id)
            .unwrap()
            .added
    );
    assert_eq!(
        fs::read_to_string(f.draft().join("glossary.json")).unwrap(),
        "[\n  {\n    \"source\": \"勇者\",\n    \"allowed\": [\n      \"勇士\"\n    ],\n    \"scope\": \"第二章以后；先人工核对\",\n    \"note\": \"统一\"\n  },\n  {\n    \"source\": \"勇者\",\n    \"allowed\": [\n      \"勇者大人\"\n    ]\n  }\n]\n"
    );
    assert!(
        !Notes::new(&f.root)
            .promote(&id, &f.project.id)
            .unwrap()
            .added
    );
    assert_eq!(fs::read(dir.join("context.json")).unwrap(), exported);
}

#[test]
fn structured_draft_prepares_and_approves_v2_snapshot() {
    for ensure in [false, true] {
        let f = Fx::new();
        f.write_glossary_only(V2);
        let t = f.t();
        if ensure {
            t.ensure_context_draft(&f.project.id).unwrap();
        }
        assert_eq!(t.approve_context(&f.project.id).unwrap(), 1);
        assert_eq!(
            fs::read(f.snapshot_dir().join("glossary.json")).unwrap(),
            V2.as_bytes()
        );
        assert!(matches!(
            t.context_snapshot(&f.project.id, 1).unwrap().glossary,
            ziyuan_app::translation::Glossary::Structured(_)
        ));
    }
}

#[test]
fn structured_draft_still_refuses_note_promotion_without_changing_glossary() {
    for kind in [NoteKind::Word, NoteKind::Passage] {
        let f = Fx::new();
        f.write_glossary_only(V2);
        let id = f.note(kind);
        let result = Notes::new(&f.root).promote(&id, &f.project.id);
        if kind == NoteKind::Word {
            let error = result.expect_err("第二版资料仍不支持术语札记并入");
            assert!(
                error.to_string().contains("第二版资料暂不支持札记并入"),
                "{error}"
            );
        } else {
            assert!(result.unwrap().added, "文体札记仍可并入");
        }
        assert_eq!(
            fs::read(f.draft().join("glossary.json")).unwrap(),
            V2.as_bytes()
        );
    }
}

#[test]
fn structured_snapshot_is_read_exported_verified_and_approvable() {
    let f = Fx::new();
    let t = f.t();
    t.ensure_context_draft(&f.project.id).unwrap();
    t.approve_context(&f.project.id).unwrap();
    fs::write(f.snapshot_dir().join("glossary.json"), V2).unwrap();
    let path = f.snapshot_dir().join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let digest = ring::digest::digest(&ring::digest::SHA256, V2.as_bytes());
    let hash: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    for entry in manifest["files"].as_array_mut().unwrap() {
        if entry["path"] == "glossary.json" {
            entry["sha256"] = serde_json::json!(hash);
            entry["bytes"] = serde_json::json!(V2.len());
        }
    }
    fs::write(path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    assert!(matches!(
        t.context_snapshot(&f.project.id, 1).unwrap().glossary,
        ziyuan_app::translation::Glossary::Structured(_)
    ));
    t.export(&f.project.id, f.block()).unwrap();
    t.verification(None, &f.project.id, f.block()).unwrap();
    assert!(t.approve_context(&f.project.id).unwrap() >= 2);
}
