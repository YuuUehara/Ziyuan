use super::support::{material, sample};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use ziyuan_app::text_project::{NewProject, Provenance, TextProjects};
use ziyuan_app::translation::{
    MaterialDraft, MaterialSave, StructuredGlossary, Translations, validate_material,
};

const ROOT_ENV: &str = "ZIYUAN_TEST_MATERIAL_ROOT";
const PROJECT_ENV: &str = "ZIYUAN_TEST_MATERIAL_PROJECT";

struct Fixture {
    root: PathBuf,
    project: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new() -> Self {
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

    fn service(&self) -> Translations {
        Translations::new(&self.root)
    }

    fn draft_path(&self) -> PathBuf {
        Path::new(".ziyuan/text-projects")
            .join(&self.project)
            .join("material-draft.json")
    }
}

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

fn changed(
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

fn unfinished() -> StructuredGlossary {
    let mut value = sample();
    value["persons"][0]["name"] = json!("");
    material(value)
}

fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

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
