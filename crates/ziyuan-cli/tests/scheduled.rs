use std::path::Path;
use std::process::{Command, Output};
use ziyuan_storage::{EventLog, EventStore};

fn cli(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ziyuan"));
    command.current_dir(root).args(args);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("ZIYUAN_") {
            command.env_remove(key);
        }
    }
    command.output().unwrap()
}

fn ok(root: &Path, args: &[&str]) -> Output {
    let out = cli(root, args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn scheduled_plan_black_box_roundtrip_retry_and_rejection() {
    let root = std::env::temp_dir().join(format!("ziyuan-cli-scheduled-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    ok(&root, &["init"]);
    ok(&root, &["edict", "--queue", "--dept", "works", "parent"]);
    let json = r#"[{"worker":4,"text":"first","dependencies":[2],"idempotency_key":"first"},{"worker":4,"text":"second","attempts":2,"artifact":"report","idempotency_key":"second"}]"#;
    std::fs::write(root.join("plan.json"), json).unwrap();
    let first = ok(&root, &["split", "1", "--plan", "plan.json"]);
    let store = EventLog::new(&root);
    let history = store.load().unwrap();
    let retry = ok(&root, &["split", "1", "--plan", "plan.json"]);
    assert_eq!(first.stdout, retry.stdout);
    assert_eq!(store.load().unwrap(), history);
    let tasks: serde_json::Value =
        serde_json::from_slice(&ok(&root, &["tasks", "--json"]).stdout).unwrap();
    let children = tasks.as_array().unwrap();
    let first = children.iter().find(|t| t["text"] == "first").unwrap();
    let second = children.iter().find(|t| t["text"] == "second").unwrap();
    assert_eq!(first["dag"]["dependencies"][0], second["id"]);
    assert_eq!(second["dag"]["attempts"], 2);
    assert_eq!(second["dag"]["artifact"], "report");
    for bad in [
        json.replace("first", "changed"),
        "{}".into(),
        "[".into(),
        r#"[{"worker":4,"text":"x","unknown":1}]"#.into(),
        r#"[{"worker":4,"text":"x","dependencies":[0]},{"worker":4,"text":"y"}]"#.into(),
    ] {
        std::fs::write(root.join("plan.json"), bad).unwrap();
        assert!(
            !cli(&root, &["split", "1", "--plan", "plan.json"])
                .status
                .success()
        );
        assert_eq!(store.load().unwrap(), history);
    }
    for args in [
        vec!["split", "1", "--plan", "missing.json"],
        vec!["split", "1", "--plan", "plan.json", "4:x"],
        vec!["split", "1", "4:x", "--plan", "plan.json"],
    ] {
        assert!(!cli(&root, &args).status.success());
        assert_eq!(store.load().unwrap(), history);
    }
    std::fs::remove_dir_all(root).unwrap();
}
