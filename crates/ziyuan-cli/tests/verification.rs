//! Offline black-box regression tests: all evidence/checklist operations use the public CLI.
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const BLOCK: &str = "c0001-b0001";
struct Fixture {
    root: PathBuf,
    project: String,
    paragraph: String,
    source: Value,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "ziyuan-cli-verification-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let mut f = Self {
            root,
            project: String::new(),
            paragraph: String::new(),
            source: Value::Null,
        };
        let project = f.ok(&[
            "text",
            "init",
            "--title",
            "Checks",
            "--language",
            "en",
            "--source",
            "self",
            "--rights",
            "self",
        ]);
        f.project = project["id"].as_str().unwrap().into();
        fs::write(
            f.root.join("source.txt"),
            "Alice did not give Bob two coins.",
        )
        .unwrap();
        let project = f.ok(&["text", "import", &f.project, "source.txt"]);
        f.source = project["chapters"][0]["blocks"][0].clone();
        f.paragraph = f.source["paragraphs"][0]["id"].as_str().unwrap().into();
        f
    }
    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ziyuan"));
        command.current_dir(&self.root).args(args).arg("--json");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("ZIYUAN_") {
                command.env_remove(key);
            }
        }
        command.output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn fail(&self, args: &[&str]) {
        let out = self.run(args);
        assert!(!out.status.success(), "unexpected success: {args:?}");
    }
    fn write(&self, name: &str, value: &Value) {
        fs::write(self.root.join(name), serde_json::to_vec(value).unwrap()).unwrap();
    }
    fn show(&self) -> Value {
        self.ok(&["text", "verification", "show", &self.project, BLOCK])
    }
    fn evidence(&self) -> Value {
        json!({"paragraph": self.paragraph, "quote":"two coins", "source":"https://example.invalid/dictionary", "locator":"p. 4", "explanation":"quantity", "context":"Alice's gift", "status":"tentative"})
    }
    fn add(&self) -> Value {
        self.ok(&[
            "text",
            "evidence",
            "add",
            &self.project,
            BLOCK,
            "--file",
            "evidence.json",
        ])
    }
    fn request(&self, fingerprint: &Value, resolution: &str) -> Value {
        let checks: Vec<_> = ["subject_speaker", "agency_negation", "quantity", "timeline", "names_relations", "humor_additions"].iter().map(|category| json!({"category":category,"disposition":"accepted","resolution":resolution,"note":"operator checked", "advice":[],"evidence":[]})).collect();
        json!({"paragraph":self.paragraph,"fingerprint":fingerprint,"checks":checks})
    }
    fn record(&self) -> Value {
        self.ok(&[
            "text",
            "verification",
            "record",
            &self.project,
            BLOCK,
            "--file",
            "checks.json",
        ])
    }
    fn draft(&self, text: &str) {
        let path = self
            .root
            .join(format!("translations/{}/{BLOCK}/zh.json", self.project));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut draft = self.source.clone();
        draft["paragraphs"][0]["text"] = json!(text);
        fs::write(path, serde_json::to_vec(&draft).unwrap()).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn strict_public_command_shapes_and_human_labels() {
    let f = Fixture::new();
    for args in [
        vec!["text", "evidence", "add", &f.project, BLOCK],
        vec!["text", "verification", "show", &f.project, BLOCK, "extra"],
        vec![
            "text",
            "verification",
            "record",
            &f.project,
            BLOCK,
            "--file",
        ],
        vec!["text", "evidence", "remove", &f.project, BLOCK],
        vec!["text", "verification", "show", "--file", BLOCK],
    ] {
        let out = f.run(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
    let out = Command::new(env!("CARGO_BIN_EXE_ziyuan"))
        .current_dir(&f.root)
        .args(["text", "verification", "show", &f.project, BLOCK])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    for label in [
        "结构有效性",
        "关联意见已处理",
        "人工复核",
        "有效御览定稿",
        "text status",
    ] {
        assert!(text.contains(label), "missing {label}: {text}");
    }
    let help = f.run(&["text", "help"]);
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    for command in [
        "text evidence add",
        "text verification show",
        "text verification record",
        "逐段勘校",
        "言行归属 subject_speaker",
        "事理正反 agency_negation",
        "数目分寸 quantity",
        "前后次第 timeline",
        "称谓名分 names_relations",
        "谐趣留白 humor_additions",
        "清替我换钱",
        "并非主任",
    ] {
        assert!(help.contains(command));
    }
}

#[test]
fn evidence_roundtrip_and_strict_requests() {
    let f = Fixture::new();
    let evidence = f.evidence();
    f.write("evidence.json", &evidence);
    let first = f.add();
    let second = f.add();
    assert_ne!(first["id"], second["id"]);
    assert_eq!(first["details"], evidence);
    let view = f.show();
    assert_eq!(view["evidence"], json!([first, second]));
    assert!(!view["fingerprint"].as_str().unwrap().is_empty());
    assert_eq!(view["translation"], Value::Null);
    for kind in ["missing", "forged", "unknown"] {
        let mut bad = evidence.clone();
        match kind {
            "missing" => {
                bad.as_object_mut().unwrap().remove("paragraph");
            }
            "forged" => bad["paragraph"] = json!("c9999-p9999"),
            _ => bad["surprise"] = json!(true),
        }
        f.write("evidence.json", &bad);
        f.fail(&[
            "text",
            "evidence",
            "add",
            &f.project,
            BLOCK,
            "--file",
            "evidence.json",
        ]);
    }
    assert_eq!(f.show()["evidence"].as_array().unwrap().len(), 2);
    f.write("checks.json", &f.request(&view["fingerprint"], "resolved"));
    f.fail(&[
        "text",
        "verification",
        "record",
        &f.project,
        BLOCK,
        "--file",
        "checks.json",
    ]);
    assert!(f.show()["records"].as_array().unwrap().is_empty());
    // Sparse oversized file verifies the bounded reader before any JSON parse/write.
    fs::File::create(f.root.join("evidence.json"))
        .unwrap()
        .set_len(16 * 1024 * 1024 + 1)
        .unwrap();
    let out = f.run(&[
        "text",
        "evidence",
        "add",
        &f.project,
        BLOCK,
        "--file",
        "evidence.json",
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("16 MiB"));
}

#[test]
fn fingerprints_and_immutable_stale_resolved_history() {
    let f = Fixture::new();
    f.draft("爱丽丝没有给鲍勃两枚硬币。");
    let preview = f.show();
    assert_eq!(preview["structure"]["valid"], true);
    assert_eq!(preview["fingerprint"], f.show()["fingerprint"]);
    let request = f.request(&preview["fingerprint"], "unresolved");
    for kind in ["missing", "forged", "unknown", "nested"] {
        let mut bad = request.clone();
        match kind {
            "missing" => {
                bad.as_object_mut().unwrap().remove("paragraph");
            }
            "forged" => bad["paragraph"] = json!("c9999-p9999"),
            "nested" => bad["checks"][0]["surprise"] = json!(true),
            _ => bad["surprise"] = json!(true),
        }
        f.write("checks.json", &bad);
        f.fail(&[
            "text",
            "verification",
            "record",
            &f.project,
            BLOCK,
            "--file",
            "checks.json",
        ]);
    }
    f.write("checks.json", &request);
    let first = f.record();
    assert_eq!(first["checks"][0]["disposition"], "accepted");
    assert_eq!(first["checks"][0]["resolution"], "unresolved");
    assert_eq!(f.show()["records"][0]["current"], true);
    f.draft("爱丽丝没给鲍勃两枚钱币。");
    let changed = f.show();
    assert_ne!(changed["fingerprint"], preview["fingerprint"]);
    assert_eq!(changed["records"][0]["current"], false);
    assert!(changed["records"][0]["reason"].is_string());
    // Change links compare immutable successive records, not a record to live text.
    assert!(changed["records"][0]["change"].is_null());
    f.fail(&[
        "text",
        "verification",
        "record",
        &f.project,
        BLOCK,
        "--file",
        "checks.json",
    ]);
    f.ok(&[
        "text",
        "zhupi",
        &f.project,
        BLOCK,
        &f.paragraph,
        "核对否定和数量",
    ]);
    let annotated = f.show();
    assert_ne!(annotated["fingerprint"], changed["fingerprint"]);
    f.write(
        "checks.json",
        &f.request(&annotated["fingerprint"], "resolved"),
    );
    let second = f.record();
    assert_eq!(second["previous"], first["id"]);
    let history = f.show();
    assert_eq!(history["records"].as_array().unwrap().len(), 2);
    assert_eq!(history["records"][0]["record"], first);
    assert_eq!(history["records"][0]["current"], false);
    assert_eq!(history["records"][1]["current"], true);
    assert_eq!(history["records"][1]["record"], second);
    assert_eq!(
        history["records"][1]["change"]["before"],
        first["translation"]
    );
    assert_eq!(
        history["records"][1]["change"]["after"],
        second["translation"]
    );
    assert_eq!(history["paragraphs"][0]["unresolved"], 0);
    f.ok(&[
        "text",
        "zhupi",
        &f.project,
        BLOCK,
        &f.paragraph,
        "另核说话者",
    ]);
    let reannotated = f.show();
    assert_ne!(reannotated["fingerprint"], history["fingerprint"]);
    assert_eq!(reannotated["records"][1]["current"], false);
    assert_eq!(reannotated["records"][1]["record"], second);
    assert_eq!(reannotated["paragraphs"][0]["operator_reviewed"], false);
    // A structurally broken on-disk draft cannot receive a new operator record.
    let path = f
        .root
        .join(format!("translations/{}/{BLOCK}/zh.json", f.project));
    let mut broken = f.source.clone();
    broken["paragraphs"] = json!([]);
    fs::write(path, serde_json::to_vec(&broken).unwrap()).unwrap();
    let invalid = f.show();
    assert_eq!(invalid["structure"]["valid"], false);
    f.write(
        "checks.json",
        &f.request(&invalid["fingerprint"], "resolved"),
    );
    f.fail(&[
        "text",
        "verification",
        "record",
        &f.project,
        BLOCK,
        "--file",
        "checks.json",
    ]);
}
