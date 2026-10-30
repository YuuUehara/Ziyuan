//! 御笔札记：不需要朝廷即可增删改查，并入上下文草稿不重复。
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use ziyuan_app::notes::*;
use ziyuan_app::text_project::{NewProject, Provenance, TextProjects};
use ziyuan_app::translation::{GlossaryEntry, Translations, context_dir_rel, parse_glossary};

struct Fx(PathBuf);
impl Fx {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "ziyuan-notes-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn project(&self) -> String {
        let texts = TextProjects::new(&self.0);
        let p = texts
            .create(NewProject {
                title: "作品".into(),
                source_language: "ja".into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap();
        let src = self.0.join("a.txt");
        fs::write(&src, "勇者は旅立った。\n\n魔王城へ。\n").unwrap();
        texts.import(&p.id, &[src]).unwrap();
        p.id
    }
}
impl Drop for Fx {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn word(source: &str, zh: &str) -> NewNote {
    NewNote {
        kind: NoteKind::Word,
        source: source.into(),
        zh: zh.into(),
        remark: String::new(),
        project: None,
        block: None,
        paragraph: None,
    }
}

#[test]
fn notes_crud_and_search_without_a_court() {
    let fx = Fx::new();
    let notes = Notes::new(&fx.0);
    assert!(notes.list(&NoteFilter::default()).unwrap().is_empty());
    let a = notes.add(word("勇者", "勇者")).unwrap();
    assert_eq!(a.id.len(), 32);
    assert!(a.id.bytes().all(|b| b.is_ascii_hexdigit()));
    let b = notes
        .add(NewNote {
            kind: NoteKind::Passage,
            source: "魔王城へ。\n二行目。".into(),
            zh: "前往魔王城。\n第二行。".into(),
            remark: "语气要硬".into(),
            ..word("", "")
        })
        .unwrap();
    assert!(!fx.0.join(".ziyuan/court.log").exists(), "不建朝廷");
    assert!(fx.0.join(".ziyuan/notes.json").is_file());

    let search = |s: &str| {
        notes
            .list(&NoteFilter {
                search: Some(s.into()),
                ..Default::default()
            })
            .unwrap()
            .into_iter()
            .map(|n| n.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(search("魔王"), std::slice::from_ref(&b.id));
    assert_eq!(search("语气"), std::slice::from_ref(&b.id));
    assert_eq!(search("勇"), std::slice::from_ref(&a.id));
    let words = notes
        .list(&NoteFilter {
            kind: Some(NoteKind::Word),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(words.len(), 1);

    let edited = notes
        .edit(
            &a.id,
            NoteEdit {
                zh: Some("勇士".into()),
                remark: Some("第一卷起".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(edited.zh, "勇士");
    assert_eq!(edited.source, "勇者");
    assert_eq!(notes.get(&a.id).unwrap(), edited);
    assert!(
        notes
            .edit(
                &a.id,
                NoteEdit {
                    zh: Some(" ".into()),
                    ..Default::default()
                }
            )
            .is_err()
    );
    assert_eq!(notes.get(&a.id).unwrap().zh, "勇士", "改坏了不保存");

    assert_eq!(notes.remove(&b.id).unwrap().id, b.id);
    assert!(notes.get(&b.id).is_err());
    assert!(notes.remove(&b.id).is_err());
    assert_eq!(notes.list(&NoteFilter::default()).unwrap().len(), 1);
}

#[test]
fn notes_validate_text_and_links() {
    let fx = Fx::new();
    let notes = Notes::new(&fx.0);
    assert!(notes.add(word(" ", "甲")).is_err());
    assert!(notes.add(word("甲", "")).is_err());
    assert!(notes.add(word("甲\n乙", "丙")).is_err(), "词语须单行");
    assert!(notes.add(word(&"长".repeat(201), "丙")).is_err());
    let project = fx.project();
    let linked = |block: Option<&str>, paragraph: Option<&str>, project: Option<&str>| NewNote {
        project: project.map(Into::into),
        block: block.map(Into::into),
        paragraph: paragraph.map(Into::into),
        ..word("勇者", "勇者")
    };
    let p = Some(project.as_str());
    let ok = notes
        .add(linked(Some("c0001-b0001"), Some("c0001-p0002"), p))
        .unwrap();
    assert_eq!(ok.paragraph.as_deref(), Some("c0001-p0002"));
    assert!(
        notes
            .add(linked(None, None, Some(&"0".repeat(32))))
            .is_err(),
        "无此项目"
    );
    assert!(notes.add(linked(Some("c0009-b0001"), None, p)).is_err());
    assert!(
        notes
            .add(linked(Some("c0001-b0001"), Some("c0001-p0099"), p))
            .is_err()
    );
    assert!(notes.add(linked(None, Some("c0001-p0001"), p)).is_err());
    assert!(notes.add(linked(Some("c0001-b0001"), None, None)).is_err());
    assert_eq!(
        notes
            .list(&NoteFilter {
                project: Some(project.clone()),
                ..Default::default()
            })
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn promote_merges_into_drafts_once() {
    let fx = Fx::new();
    let project = fx.project();
    let notes = Notes::new(&fx.0);
    let term = notes
        .add(NewNote {
            remark: "全书统一".into(),
            ..word("魔王", "魔王")
        })
        .unwrap();
    let first = notes.promote(&term.id, &project).unwrap();
    assert!(first.added);
    assert_eq!(
        first.path,
        format!("{}/glossary.json", context_dir_rel(&project))
    );
    let again = notes.promote(&term.id, &project).unwrap();
    assert!(!again.added, "重复并入不重复写");
    let glossary = fs::read(fx.0.join(&first.path)).unwrap();
    assert_eq!(
        parse_glossary(&glossary).unwrap(),
        [GlossaryEntry {
            source: "魔王".into(),
            allowed: vec!["魔王".into()],
            scope: None,
            note: Some("全书统一".into()),
        }]
    );
    // 另一条同原文同译法的札记也不重复写。
    let twin = notes.add(word("魔王", "魔王")).unwrap();
    assert!(!notes.promote(&twin.id, &project).unwrap().added);

    let passage = notes
        .add(NewNote {
            kind: NoteKind::Passage,
            source: "勇者は旅立った。".into(),
            zh: "勇者踏上了旅途。".into(),
            remark: String::new(),
            ..word("", "")
        })
        .unwrap();
    let style = notes.promote(&passage.id, &project).unwrap();
    assert!(style.added);
    let body = fs::read_to_string(fx.0.join(&style.path)).unwrap();
    assert!(body.contains("## 范例"));
    assert!(body.contains("> 勇者踏上了旅途。"));
    assert!(!notes.promote(&passage.id, &project).unwrap().added);
    assert_eq!(fs::read_to_string(fx.0.join(&style.path)).unwrap(), body);
    assert_eq!(body.matches("## 范例").count(), 1);

    // 并入的草稿照常可批准。
    assert_eq!(
        Translations::new(&fx.0).approve_context(&project).unwrap(),
        1
    );
    assert!(
        notes.promote(&term.id, &"0".repeat(32)).is_err(),
        "无此项目"
    );
}
