use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use ziyuan_app::text_project::{NewProject, Project, Provenance, TextProjects};
use ziyuan_app::translation::{
    MaterialDraft, MaterialError, MaterialReceipt, MaterialSave, StructuredGlossary, Translations,
};

pub const PROJECT_ENV: &str = "ZIYUAN_TEST_MATERIAL_PROJECT";

pub fn sample() -> Value {
    json!({
        "format":2,
        "terms":[{
            "term_id":"t-1","source":"先生","category":"title","allowed":["先生","老师"],
            "note":"备注\n制表\t","scope":{"kind":"range","from":{"chapter":"z","paragraph":"pz"},
            "to":{"chapter":"a","paragraph":"pa"}},"available_from":{"chapter":"z"},
            "evidence":[evidence()],"status":"confirmed"
        }],
        "persons":[{
            "person_id":"p_1","available_from":{"chapter":"z","paragraph":"pz"},
            "name":"同名","reading":"よみ","aliases":["别名","同名"],
            "terms":["t-1"],"address_rules":[{
                "target":"p_2","speaker":{"kind":"person","person_id":"p_1"},
                "scope":{"kind":"project"},"term":"t-1","note":"备注","evidence":[evidence()]
            }],"voice":{"description":"庄重","examples":["先生来了","第二例"]},
            "evidence":[evidence()],"status":"tentative"
        },{
            "person_id":"p_2","name":"同名","aliases":[],"terms":[],
            "address_rules":[],"voice":{"description":"轻快"},"evidence":[],"status":"confirmed"
        }]
    })
}

fn evidence() -> Value {
    json!({"paragraph":"pz","quote":"先生","explanation":"称呼","stance":"project_fact","status":"confirmed"})
}

pub fn material(value: Value) -> StructuredGlossary {
    serde_json::from_value(value).unwrap()
}

pub fn project() -> Project {
    serde_json::from_value(json!({
        "id":"test","metadata":{"title":"书","source_language":"ja","target_language":"zh",
        "provenance":{"author":"","version":"","source":"","date":"","rights":"","scope":""}},
        "block_bytes":1000,"chapters":[
            {"id":"z","source_name":"","original_path":"","sha256":"","byte_len":0,
            "paragraphs":[
                {"id":"pz","byte_start":0,"byte_end":15,"text":"先生来了。"},
                {"id":"pb","byte_start":15,"byte_end":30,"text":"第二段。"}
            ],"blocks":[]},
            {"id":"a","source_name":"","original_path":"","sha256":"","byte_len":0,
            "paragraphs":[{"id":"pa","byte_start":0,"byte_end":9,"text":"末段。"}],"blocks":[]}
        ]
    }))
    .unwrap()
}

/// A real workspace with one imported text project; removed on drop.
pub struct Fixture {
    pub root: PathBuf,
    pub project: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    pub fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "ziyuan-material-storage-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let texts = TextProjects::new(&root);
        let project = texts
            .create(NewProject {
                title: "自创资料样本".into(),
                source_language: "ja".into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap();
        let source = root.join("source.txt");
        fs::write(&source, "先生来了。\n\n第二段。\n").unwrap();
        texts.import(&project.id, &[source]).unwrap();
        Self {
            root,
            project: project.id,
        }
    }

    pub fn service(&self) -> Translations {
        Translations::new(&self.root)
    }

    /// Relative to `root`.
    pub fn draft_path(&self) -> PathBuf {
        Path::new(".ziyuan/text-projects")
            .join(&self.project)
            .join("material-draft.json")
    }

    /// Relative to `root`.
    pub fn lock_path(&self) -> PathBuf {
        let mut lock = self.draft_path().into_os_string();
        lock.push(".lock");
        PathBuf::from(lock)
    }
}

/// Every directory (as `None`, so empty directories count) and file below `root`.
pub fn tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(base).unwrap().to_owned();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.file_type().is_symlink() {
                out.insert(relative, Some(b"<symlink>".to_vec()));
            } else if meta.is_dir() {
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

pub fn changed(
    before: &BTreeMap<PathBuf, Option<Vec<u8>>>,
    after: &BTreeMap<PathBuf, Option<Vec<u8>>>,
) -> BTreeSet<PathBuf> {
    before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect()
}

pub fn unfinished() -> StructuredGlossary {
    let mut value = sample();
    value["persons"][0]["name"] = json!("");
    material(value)
}

pub fn variant(tag: &str) -> StructuredGlossary {
    let mut value = sample();
    value["persons"][0]["name"] = json!("");
    value["terms"][0]["note"] = json!(format!("变体{tag}"));
    material(value)
}

pub fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn save_with(
    f: &Fixture,
    request_id: &str,
    expected_revision: Option<String>,
    material: StructuredGlossary,
) -> Result<MaterialReceipt, MaterialError> {
    f.service().save_material_draft(
        &f.project,
        &MaterialSave {
            request_id: request_id.into(),
            expected_revision,
            material,
        },
    )
}

pub fn draft_bytes(f: &Fixture) -> Vec<u8> {
    fs::read(f.root.join(f.draft_path())).unwrap()
}

pub fn receipt_count(f: &Fixture) -> usize {
    let stored: Value = serde_json::from_slice(&draft_bytes(f)).unwrap();
    stored["data"]["requests"].as_object().unwrap().len()
}

pub fn current(f: &Fixture) -> MaterialDraft {
    f.service().material_draft(&f.project).unwrap().unwrap()
}

pub fn assert_conflict(result: Result<MaterialReceipt, MaterialError>, expected: &str) {
    match result {
        Err(MaterialError::Conflict { current }) => assert_eq!(current.as_deref(), Some(expected)),
        other => panic!("应报修订冲突并携带现行修订，实际：{other:?}"),
    }
}
