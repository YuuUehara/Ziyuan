//! 稳定性约定（docs/stability.md）的契约测试：以命令行黑盒方式核对 1.0 起冻结的接口。
//! 这里列出的只能增加，不能删改；新增字段、子命令或环境变量无须改动本文件。

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use ziyuan_core::{AgentId, Capability, CourtEvent, Department, Seal, TokenUsage};
use ziyuan_governance::Court;
use ziyuan_storage::{EventLog, EventStore};

const BIN: &str = env!("CARGO_BIN_EXE_ziyuan");

fn ziyuan(dir: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(BIN);
    command.args(args).current_dir(dir);
    // vars_os：环境中可能有非 UTF-8 的变量，vars() 遇到会 panic。
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("ZIYUAN_") {
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
fn run_explains_when_only_blocked_subtasks_remain() {
    let dir = temp_court("dag-blocked-run");
    let mut log = EventLog::new(&dir);
    let existing = log.load().unwrap();
    let mut court = Court::replay(&existing).unwrap();
    let edict = court
        .new_edict(
            Department::Works,
            "改全库".into(),
            Seal {
                capabilities: [Capability::ReadFile, Capability::WriteFile].into(),
                scope: vec![".".into()],
                mcp: Default::default(),
            },
        )
        .unwrap();
    let parent = edict.id;
    let mut events = vec![CourtEvent::EdictIssued(Box::new(edict))];
    court.apply(&events[0]).unwrap();
    let split = court
        .propose_split_serial_writes(
            parent,
            vec![(AgentId(4), "改甲".into()), (AgentId(4), "改乙".into())],
            "御定分办".into(),
            None,
            TokenUsage::default(),
        )
        .unwrap();
    for event in split {
        court.apply(&event).unwrap();
        events.push(event);
    }
    let subtask = court.task(parent).unwrap().split.as_ref().unwrap().subtasks[0];
    let failure = CourtEvent::TaskFailed {
        task: subtask,
        reason: "测试上游失败".into(),
    };
    court.apply(&failure).unwrap();
    events.push(failure);
    log.append(existing.len(), &events).unwrap();

    let output = ok(&dir, &["run"]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("受阻"), "{stdout}");
    assert!(stdout.contains("ziyuan tasks"), "{stdout}");
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
        "ZIYUAN_CONTEXT_TOKENS",
        "ZIYUAN_TEST_COMMAND",
        "ZIYUAN_TZ",
        "ZIYUAN_GITHUB_REPO",
        "ZIYUAN_GITHUB_TOKEN",
        "ZIYUAN_CONFIG_DIR",
        "ZIYUAN_SANDBOX",
    ] {
        assert!(help.contains(var), "help 未列出 {var}");
    }
}

#[test]
fn invalid_context_threshold_does_not_start_or_change_tasks() {
    let dir = temp_court("context");
    ok(&dir, &["edict", "--queue", "检查上下文"]);
    let before = EventLog::new(&dir).load().unwrap();
    for value in ["", "0", "-1", "1.5", "abc", "18446744073709551616"] {
        let mut command = Command::new(BIN);
        command.current_dir(&dir).arg("run");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("ZIYUAN_") {
                command.env_remove(key);
            }
        }
        let out = command
            .env("ZIYUAN_LLM_MODEL", "context-test")
            .env("ZIYUAN_LLM_BASE_URL", "http://127.0.0.1:9/v1")
            .env("ZIYUAN_CONTEXT_TOKENS", value)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{value:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("ZIYUAN_CONTEXT_TOKENS 须为正整数"));
        assert_eq!(EventLog::new(&dir).load().unwrap(), before);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// 造一个“写入者已死”的工作目录：临时文件加一份认领它的租约。
fn stale_temp_workspace(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("ziyuan-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let temp = dir.join(".ziyuan-tmp-4242-1-1");
    std::fs::write(&temp, b"partial").unwrap();
    std::fs::write(
        dir.join(".ziyuan-lease-0123456789abcdef0123456789abcdef"),
        "ziyuan-lease v1 temp=.ziyuan-tmp-4242-1-1 id=0123456789abcdef0123456789abcdef\n",
    )
    .unwrap();
    (dir, temp)
}

#[test]
fn text_recovery_temps_previews_then_deletes() {
    let (dir, temp) = stale_temp_workspace("recovery-temps");
    let preview = ziyuan(&dir, &["text", "recovery", "temps", "--json"]);
    // 预览不是失败：设计写明“仅保留活跃或未知项不算执行失败”，可清理数写在报告里。
    assert_eq!(preview.status.code(), Some(0), "{:?}", preview.stdout);
    let report: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(report["mode"], "preview");
    assert_eq!(report["scope"], ".");
    assert_eq!(report["temps"]["removable"], 1);
    assert_eq!(report["temps"]["removed"], 0);
    // 报告里只出现相对路径，不泄露绝对路径。
    assert_eq!(report["items"][0]["path"], ".ziyuan-tmp-4242-1-1");
    assert!(
        !String::from_utf8_lossy(&preview.stdout).contains(r"\\?\"),
        "报告不得包含绝对路径"
    );
    assert!(temp.is_file(), "预览不得删除");

    let applied = ziyuan(&dir, &["text", "recovery", "temps", "--yes", "--json"]);
    assert_eq!(applied.status.code(), Some(0), "{:?}", applied.stdout);
    let report: Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(report["mode"], "apply");
    assert_eq!(report["temps"]["removed"], 1);
    assert!(!temp.exists(), "确认后应删除");
    assert!(
        !dir.join(".ziyuan-lease-0123456789abcdef0123456789abcdef")
            .exists()
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn text_recovery_temps_keeps_a_temp_without_a_lease() {
    // 旧版遗留或 PID 复用：名字合法但没有租约，没有归属证据，一律保留。
    let dir = std::env::temp_dir().join(format!("ziyuan-cli-temps-reuse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let orphan = dir.join(".ziyuan-tmp-4242-1-1");
    std::fs::write(&orphan, b"partial").unwrap();
    let out = ziyuan(&dir, &["text", "recovery", "temps", "--yes", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{:?}", out.stdout);
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["temps"]["kept"], 1);
    assert_eq!(report["temps"]["removed"], 0);
    assert_eq!(report["items"][0]["reason"], "TempWithoutLease");
    assert!(orphan.is_file(), "无归属证据的文件绝不删除");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn text_recovery_temps_is_clean_on_an_empty_workspace() {
    let dir = std::env::temp_dir().join(format!("ziyuan-cli-temps-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = ziyuan(&dir, &["text", "recovery", "temps", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["temps"]["candidates"], 0);
    assert_eq!(report["leases"]["candidates"], 0);
    assert_eq!(report["removed_total"], 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn text_recovery_temps_reports_capped_without_failing() {
    // 33 层目录只是建 33 次，代价可忽略；深度上限是 32，所以第 32 层会被记进 `capped`。
    let dir = std::env::temp_dir().join(format!("ziyuan-cli-temps-capped-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut deep = dir.clone();
    for _ in 0..33 {
        deep = deep.join("d");
        std::fs::create_dir_all(&deep).unwrap();
    }
    // 工作区根算第 0 层，上限 32：第 32 层那个目录本身不进入，报的相对路径就是 32 段。
    let expected = vec!["d"; 32].join("/");
    let out = ziyuan(&dir, &["text", "recovery", "temps", "--json"]);
    // 深度上限不是 I/O 失败：它只说明没走到那一层，本轮仍然算成功。
    assert_eq!(out.status.code(), Some(0), "{:?}", out.stdout);
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    let capped = report["capped"].as_array().unwrap();
    assert!(
        capped.iter().any(|entry| entry == expected.as_str()),
        "{expected} 未出现在 capped 中：{capped:?}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
