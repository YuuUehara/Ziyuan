use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
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
            "ziyuan-tmx-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let p = TextProjects::new(&root)
            .create(NewProject {
                title: "自编TMX".into(),
                source_language: "ja".into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap();
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
fn document(body: &str) -> Vec<u8> {
    format!(r#"<?xml version="1.0" encoding="utf-8"?><tmx version="1.4"><header srclang="ja"/><body>{body}</body></tmx>"#).into_bytes()
}
fn pair(source: &str, target: &str) -> String {
    format!(
        r#"<tu><tuv xml:lang="ja"><seg>{source}</seg></tuv><tuv xml:lang="zh-CN"><seg>{target}</seg></tuv></tu>"#
    )
}
#[test]
fn tmx_roundtrip_preserves_plain_text_and_merges_atomically() {
    let a = Fx::new();
    a.t().tm_add(&a.id, "夜 & <星>", "夜晚与〈星〉").unwrap();
    let xml = a.t().export_tmx(&a.id).unwrap();
    let b = Fx::new();
    let preview = b.t().preview_tmx(&b.id, &xml).unwrap();
    assert_eq!(preview.entries, 1);
    assert_eq!(
        b.t().import_tmx(&b.id, &xml, &preview.fingerprint).unwrap(),
        1
    );
    assert_eq!(b.t().tm_list(&b.id).unwrap()[0].source, "夜 & <星>");
    let preview = b.t().preview_tmx(&b.id, &xml).unwrap();
    assert_eq!(preview.duplicates, 1);
    assert_eq!(
        b.t().import_tmx(&b.id, &xml, &preview.fingerprint).unwrap(),
        0
    );
    assert!(!b.root.join(".ziyuan/court.log").exists());
}
#[test]
fn tmx_rejects_inline_entities_wrong_languages_and_stale_confirmation() {
    let f = Fx::new();
    for xml in [
        document(&pair("<b>夜</b>", "夜晚")),
        document(&pair("&custom;", "夜晚")),
    ] {
        if xml.windows(6).any(|s| s == b"custom") || xml.windows(3).any(|s| s == b"<b>") {
            assert!(f.t().preview_tmx(&f.id, &xml).is_err());
        }
    }
    let xml = document(&pair("夜", "夜晚"));
    let wrong = String::from_utf8(xml.clone())
        .unwrap()
        .replace("zh-CN", "en");
    assert!(f.t().preview_tmx(&f.id, wrong.as_bytes()).is_err());
    let preview = f.t().preview_tmx(&f.id, &xml).unwrap();
    f.t().tm_add(&f.id, "朝", "早晨").unwrap();
    assert!(f.t().import_tmx(&f.id, &xml, &preview.fingerprint).is_err());
    assert_eq!(f.t().tm_list(&f.id).unwrap().len(), 1);
    let dtd = String::from_utf8(xml).unwrap().replace(
        "<tmx",
        r#"<!DOCTYPE tmx [<!ENTITY secret SYSTEM "file:///secret">]><tmx"#,
    );
    assert!(f.t().preview_tmx(&f.id, dtd.as_bytes()).is_err());
}

#[test]
fn invalid_late_unit_does_not_partially_import_and_metadata_is_explicit() {
    let f = Fx::new();
    let invalid = document(&(pair("夜", "夜晚") + &pair("<ph id=\"1\"/>", "坏数据")));
    assert!(f.t().import_tmx(&f.id, &invalid, "anything").is_err());
    assert!(f.t().tm_list(&f.id).unwrap().is_empty());
    assert!(f.t().export_tmx(&f.id).is_err());
    let xml = document(
        r#"<tu><prop type="domain">文学</prop><tuv xml:lang="JA"><seg><![CDATA[夜 & <星>]]></seg></tuv><tuv xml:lang="zh-cn"><seg>夜晚&#13;与星</seg></tuv></tu>"#,
    );
    let preview = f.t().preview_tmx(&f.id, &xml).unwrap();
    assert!(!preview.warnings.is_empty());
    f.t().import_tmx(&f.id, &xml, &preview.fingerprint).unwrap();
    let entries = f.t().tm_list(&f.id).unwrap();
    assert_eq!(entries[0].source, "夜 & <星>");
    assert_eq!(entries[0].target, "夜晚\r与星");
}

#[test]
fn duplicate_languages_and_oversize_input_are_rejected() {
    let f = Fx::new();
    let xml = document(
        r#"<tu><tuv xml:lang="ja"><seg>甲</seg></tuv><tuv xml:lang="JA"><seg>乙</seg></tuv><tuv xml:lang="zh-CN"><seg>丙</seg></tuv></tu>"#,
    );
    assert!(f.t().preview_tmx(&f.id, &xml).is_err());
    assert!(
        f.t()
            .preview_tmx(&f.id, &vec![b' '; 8 * 1024 * 1024 + 1])
            .is_err()
    );
    assert!(
        f.t()
            .preview_tmx(&f.id, &document(&pair("&#0;", "零")))
            .is_err()
    );
}
