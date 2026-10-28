//! 稳定性约定（docs/stability.md）的契约测试：以命令行黑盒方式核对 1.0 起冻结的接口。
//! 这里列出的只能增加，不能删改；新增字段、子命令或环境变量无须改动本文件。

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_ziyuan");

fn ziyuan(dir: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(BIN);
    command.args(args).current_dir(dir);
    for (key, _) in std::env::vars() {
        if key.starts_with("ZIYUAN_") {
            command.env_remove(key);
        }
    }
    command.output().expect("能运行 ziyuan")
}

fn ok(dir: &Path, args: &[&str]) -> Output {
    let out = ziyuan(dir, args);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn json(dir: &Path, command: &str) -> Value {
    serde_json::from_slice(&ok(dir, &[command, "--json"]).stdout)
        .unwrap_or_else(|e| panic!("{command} --json 不是 JSON：{e}"))
}

/// `value`（对象，或数组的第一项）须含 `keys` 中的全部键。
fn has_keys(what: &str, value: &Value, keys: &[&str]) {
    let object = match value {
        Value::Array(items) => items.first().unwrap_or_else(|| panic!("{what} 为空")),
        other => other,
    };
    let object = object
        .as_object()
        .unwrap_or_else(|| panic!("{what} 不是对象"));
    let missing: Vec<_> = keys.iter().filter(|k| !object.contains_key(**k)).collect();
    assert!(missing.is_empty(), "{what} 缺少冻结的字段 {missing:?}");
}

fn temp_court(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ziyuan-contract-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    ok(&dir, &["init"]);
    dir
}

const TASK_KEYS: &[&str] = &[
    "id",
    "text",
    "department",
    "status",
    "worker",
    "seal",
    "usage",
    "review",
    "censure",
    "changes",
    "parent",
    "split",
    "urgent",
    "courier",
    "draft",
    "needs_approval",
    "suspicions",
    "template",
    "debates",
    "budget",
    "model",
    "tool_calls",
    "tool_failures",
];
const OFFICIAL_KEYS: &[&str] = &[
    "id",
    "name",
    "courtesy",
    "art",
    "speciality",
    "office",
    "model",
    "ceiling",
    "record",
    "purged",
    "demotion",
    "persona",
];

#[test]
fn json_outputs_keep_their_frozen_fields() {
    let dir = temp_court("json");
    ok(&dir, &["edict", "--queue", "检查 unsafe"]);
    ok(&dir, &["run"]);
    ok(&dir, &["review", "1", "accept"]);

    let court = json(&dir, "court");
    has_keys(
        "court",
        &court,
        &[
            "mode",
            "modes",
            "departments",
            "offices",
            "officials",
            "tasks",
            "task_total",
            "open_total",
            "retreat",
            "treasury",
            "usage",
            "vacancies",
            "templates",
        ],
    );
    has_keys("court.mode", &court["mode"], &["code", "label"]);
    has_keys(
        "court.departments",
        &court["departments"],
        &["code", "label", "route"],
    );
    has_keys(
        "court.offices",
        &court["offices"],
        &["code", "label", "rank", "department"],
    );
    has_keys("court.officials", &court["officials"], OFFICIAL_KEYS);
    has_keys("court.tasks", &court["tasks"], TASK_KEYS);
    has_keys("officials", &json(&dir, "officials"), OFFICIAL_KEYS);
    has_keys("tasks", &json(&dir, "tasks"), TASK_KEYS);
    has_keys(
        "history",
        &json(&dir, "history"),
        &["seq", "kind", "text", "time"],
    );
    has_keys(
        "records",
        &json(&dir, "records"),
        &[
            "task",
            "department",
            "edict",
            "worker",
            "handler",
            "summary",
            "verdict",
        ],
    );
    has_keys(
        "migrate",
        &json(&dir, "migrate"),
        &[
            "format",
            "version",
            "events",
            "readable",
            "readable_since",
            "first_needing",
            "problem",
            "torn_tail",
            "huidian",
        ],
    );
    for command in ["flags", "mcp"] {
        assert!(json(&dir, command).is_array(), "{command} --json 应为数组");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn exit_codes_distinguish_usage_errors_from_runtime_errors() {
    let dir = temp_court("exit");
    assert_eq!(ziyuan(&dir, &["court"]).status.code(), Some(0));
    assert_eq!(ziyuan(&dir, &["no-such-command"]).status.code(), Some(2));
    assert_eq!(ziyuan(&dir, &["court", "extra"]).status.code(), Some(2));
    assert_eq!(
        ziyuan(&dir, &["review", "99", "accept"]).status.code(),
        Some(1)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn version_and_help_list_the_frozen_commands_and_environment() {
    let dir = std::env::temp_dir();
    let version = String::from_utf8(ok(&dir, &["--version"]).stdout).unwrap();
    assert_eq!(
        version.trim(),
        format!("ziyuan {}", env!("CARGO_PKG_VERSION"))
    );
    let help = String::from_utf8(ok(&dir, &["help"]).stdout).unwrap();
    for command in [
        "init",
        "court",
        "officials",
        "recruit",
        "rename",
        "appoint",
        "dismiss",
        "purge",
        "reform",
        "edict",
        "template",
        "run",
        "tasks",
        "diff",
        "apply",
        "discard",
        "review",
        "cancel",
        "approve",
        "draft",
        "redraft",
        "split",
        "debate",
        "censor",
        "recheck",
        "acquit",
        "recommend",
        "examine",
        "kaocheng",
        "nominate",
        "cane",
        "pardon",
        "retreat",
        "abolish",
        "budget",
        "model",
        "ceiling",
        "exam",
        "crown",
        "post",
        "urgent",
        "github",
        "changelog",
        "history",
        "records",
        "migrate",
        "flags",
        "mcp",
        "serve",
        "tui",
        "login",
        "logout",
        "completions",
        "version",
        "help",
    ] {
        assert!(
            help.contains(&format!("ziyuan {command}")),
            "help 未列出 {command}"
        );
    }
    for var in [
        "ZIYUAN_LLM_MODEL",
        "ZIYUAN_LLM_BASE_URL",
        "ZIYUAN_LLM_API_KEY",
        "ZIYUAN_LLM_PROVIDER",
        "ZIYUAN_LLM_PRICE",
        "ZIYUAN_LLM_RETRIES",
        "ZIYUAN_MAX_STEPS",
        "ZIYUAN_TEST_COMMAND",
        "ZIYUAN_TZ",
        "ZIYUAN_GITHUB_REPO",
        "ZIYUAN_GITHUB_TOKEN",
        "ZIYUAN_CONFIG_DIR",
    ] {
        assert!(help.contains(var), "help 未列出 {var}");
    }
}
