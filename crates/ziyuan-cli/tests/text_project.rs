//! 独立离线文本项目命令的黑盒测试：不建立朝廷、不调用模型或网络。

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_ziyuan");

/// 本测试自建的临时目录；只删除自己建立的这一个。
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ziyuan-text-cli-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).expect("建立唯一临时目录");
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn write(&self, name: &str, body: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ziyuan(dir: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(BIN);
    command.args(args).current_dir(dir);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("ZIYUAN_") {
            command.env_remove(key);
        }
    }
    command.output().expect("能运行 ziyuan")
}

fn code(dir: &Path, args: &[&str]) -> Option<i32> {
    ziyuan(dir, args).status.code()
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

fn ok_json(dir: &Path, args: &[&str]) -> Value {
    let out = ok(dir, args);
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{args:?} 不是 JSON：{e}\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn init_args<'a>(title: &'a str, language: &'a str) -> Vec<&'a str> {
    vec![
        "text",
        "init",
        "--title",
        title,
        "--language",
        language,
        "--source",
        "https://example.invalid/work",
        "--rights",
        "用户自述：仅供个人学习",
    ]
}

fn init_json(dir: &Path, title: &str, language: &str) -> String {
    let mut args = init_args(title, language);
    args.push("--json");
    let project = ok_json(dir, &args);
    project["id"].as_str().expect("id").to_owned()
}

fn no_court(dir: &Path) {
    assert!(
        !dir.join(".ziyuan/court.log").exists(),
        "文本命令不应建立朝廷事件簿"
    );
}

/// 用存好的提示块造一份同结构的候选译稿。
fn candidate_from(block: &Value) -> Value {
    let paragraphs: Vec<Value> = block["paragraphs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| json!({ "id": p["id"], "text": format!("译：{}", p["text"].as_str().unwrap()) }))
        .collect();
    json!({
        "schema_version": 1,
        "block_id": block["block_id"],
        "paragraphs": paragraphs,
    })
}

#[test]
fn japanese_project_init_import_show_check_round_trip() {
    let dir = TempDir::new("ja");
    let root = dir.path();
    let out = ok(root, &init_args("物語", "ja"));
    let text = stdout(&out);
    let id = text
        .split_whitespace()
        .find(|w| w.len() == 32 && w.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_else(|| panic!("init 应打印项目 ID：{text}"))
        .to_owned();

    let original = "第一段です。\n\n第二段です。\n";
    let source = dir.write("ch1.txt", original);
    let out = ok(root, &["text", "import", &id, source.to_str().unwrap()]);
    assert!(stdout(&out).contains("c0001"), "{}", stdout(&out));
    assert_eq!(std::fs::read_to_string(&source).unwrap(), original);

    let out = ok(root, &["text", "show", &id]);
    let shown = stdout(&out);
    assert!(shown.contains("物語") && shown.contains("ja") && shown.contains("zh-CN"));
    assert!(shown.contains("c0001"), "{shown}");

    let project = ok_json(root, &["text", "show", &id, "--json"]);
    assert_eq!(project["id"], id.as_str());
    assert_eq!(project["metadata"]["source_language"], "ja");
    assert_eq!(project["metadata"]["target_language"], "zh-CN");
    let block = &project["chapters"][0]["blocks"][0];
    let candidate = dir.write(
        "candidate.json",
        &serde_json::to_string_pretty(&candidate_from(block)).unwrap(),
    );
    let before = std::fs::read(&candidate).unwrap();
    let out = ok(
        root,
        &[
            "text",
            "check",
            &id,
            "--candidate",
            candidate.to_str().unwrap(),
        ],
    );
    assert!(!stdout(&out).trim().is_empty());
    let report = ok_json(
        root,
        &[
            "text",
            "check",
            &id,
            "--candidate",
            candidate.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(report["valid"], true);
    assert_eq!(report["block_id"], block["block_id"]);
    assert_eq!(report["issues"], json!([]));
    assert_eq!(std::fs::read(&candidate).unwrap(), before);
    assert_eq!(std::fs::read_to_string(&source).unwrap(), original);
    no_court(root);
}

#[test]
fn english_project_with_optional_provenance_and_ordered_files() {
    let dir = TempDir::new("en");
    let root = dir.path();
    let mut args = init_args("Tale", "en");
    args.extend([
        "--author",
        "Someone",
        "--version",
        "rev-2",
        "--date",
        "2026-10-30",
        "--scope",
        "chapters 1-2",
        "--json",
    ]);
    let project = ok_json(root, &args);
    let id = project["id"].as_str().unwrap().to_owned();
    assert_eq!(project["metadata"]["provenance"]["author"], "Someone");
    assert_eq!(project["metadata"]["provenance"]["version"], "rev-2");
    assert_eq!(project["metadata"]["provenance"]["date"], "2026-10-30");
    assert_eq!(project["metadata"]["provenance"]["scope"], "chapters 1-2");
    assert_eq!(project["chapters"], json!([]));

    let second = dir.write("b-second.txt", "Second chapter.\n");
    let first = dir.write("a-first.txt", "First chapter.\n\nMore.\n");
    // 按命令行给出的顺序导入，而不是按文件名排序。
    let imported = ok_json(
        root,
        &[
            "text",
            "import",
            &id,
            second.to_str().unwrap(),
            first.to_str().unwrap(),
            "--json",
        ],
    );
    let chapters = imported["chapters"].as_array().unwrap();
    assert_eq!(chapters.len(), 2);
    assert_eq!(chapters[0]["id"], "c0001");
    assert_eq!(chapters[0]["source_name"], "b-second.txt");
    assert_eq!(chapters[1]["source_name"], "a-first.txt");

    // 只能导入一次。
    let again = ziyuan(root, &["text", "import", &id, first.to_str().unwrap()]);
    assert_eq!(again.status.code(), Some(1));
    let again = ziyuan(
        root,
        &["text", "import", &id, first.to_str().unwrap(), "--json"],
    );
    assert_eq!(again.status.code(), Some(1));
    assert!(again.stdout.is_empty());
    let shown = ok_json(root, &["--json", "text", "show", &id]);
    assert_eq!(shown["chapters"].as_array().unwrap().len(), 2);
    assert_eq!(
        std::fs::read_to_string(&first).unwrap(),
        "First chapter.\n\nMore.\n"
    );
    no_court(root);
}

#[test]
fn invalid_candidate_prints_report_and_exits_one() {
    let dir = TempDir::new("candidate");
    let root = dir.path();
    let id = init_json(root, "Tale", "en");
    let source = dir.write("one.txt", "Alpha.\n\nBeta.\n\nGamma.\n");
    ok(root, &["text", "import", &id, source.to_str().unwrap()]);
    let project = ok_json(root, &["text", "show", &id, "--json"]);
    let block = &project["chapters"][0]["blocks"][0];
    let block_id = block["block_id"].as_str().unwrap().to_owned();

    let mut candidate = candidate_from(block);
    candidate["paragraphs"].as_array_mut().unwrap().remove(1);
    let missing = dir.write("missing.json", &candidate.to_string());
    let out = ziyuan(
        root,
        &[
            "text",
            "check",
            &id,
            "--candidate",
            missing.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&out.stdout).expect("无效候选也输出报告");
    assert_eq!(report["valid"], false);
    assert_eq!(report["issues"][0]["kind"], "missing");
    let out = ziyuan(
        root,
        &[
            "text",
            "check",
            &id,
            "--candidate",
            missing.to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).contains("missing") || stdout(&out).contains("缺少"));

    // 不是 JSON：无法推断提示块时为运行错误，不输出报告。
    let garbage = dir.write("garbage.json", "not json");
    let out = ziyuan(
        root,
        &[
            "text",
            "check",
            &id,
            "--candidate",
            garbage.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    // 显式给出提示块时，同一份坏候选得到 invalid_json 报告。
    let out = ziyuan(
        root,
        &[
            "text",
            "check",
            &id,
            "--candidate",
            garbage.to_str().unwrap(),
            "--block",
            &block_id,
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["issues"][0]["kind"], "invalid_json");

    // 候选指向不存在的提示块、候选文件不存在：运行错误。
    let mut unknown = candidate_from(block);
    unknown["block_id"] = json!("c9999-b0001");
    let unknown = dir.write("unknown.json", &unknown.to_string());
    assert_eq!(
        code(
            root,
            &[
                "text",
                "check",
                &id,
                "--candidate",
                unknown.to_str().unwrap()
            ]
        ),
        Some(1)
    );
    let absent = root.join("absent.json");
    assert_eq!(
        code(
            root,
            &[
                "text",
                "check",
                &id,
                "--candidate",
                absent.to_str().unwrap()
            ]
        ),
        Some(1)
    );
    no_court(root);
}

#[test]
fn usage_errors_exit_two_and_runtime_errors_exit_one() {
    let dir = TempDir::new("errors");
    let root = dir.path();
    let usage: &[&[&str]] = &[
        &["text", "frobnicate"],
        &["text", "init"],
        &["text", "init", "--title", "T", "--language", "ja"],
        &[
            "text",
            "init",
            "--title",
            "T",
            "--language",
            "fr",
            "--source",
            "s",
            "--rights",
            "r",
        ],
        &[
            "text",
            "init",
            "--title",
            "T",
            "--language",
            "ja",
            "--source",
            "s",
            "--rights",
            "r",
            "--bogus",
            "x",
        ],
        &["text", "init", "--title"],
        &["text", "import"],
        &["text", "import", "0123456789abcdef0123456789abcdef"],
        &["text", "show"],
        &["text", "show", "a", "b"],
        &["text", "check", "0123456789abcdef0123456789abcdef"],
        &[
            "text",
            "check",
            "0123456789abcdef0123456789abcdef",
            "--candidate",
        ],
        &["text", "init", "--language", "fr", "--json"],
    ];
    for args in usage {
        assert_eq!(code(root, args), Some(2), "{args:?}");
    }
    let mut lang = init_args("T", "zh");
    lang.push("--json");
    assert_eq!(code(root, &lang), Some(2));

    let missing = "0123456789abcdef0123456789abcdef";
    let txt = dir.write("x.txt", "Body.\n");
    let runtime: Vec<Vec<&str>> = vec![
        vec!["text", "show", missing],
        vec!["text", "show", "not-an-id"],
        vec!["text", "show", missing, "--json"],
        vec!["text", "import", missing, txt.to_str().unwrap()],
        vec![
            "text",
            "check",
            missing,
            "--candidate",
            txt.to_str().unwrap(),
        ],
    ];
    for args in &runtime {
        let out = ziyuan(root, args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }

    // 坏输入：非 TXT、空文本、非 UTF-8。
    let id = init_json(root, "T", "ja");
    let not_txt = dir.write("x.md", "Body.\n");
    let blank = dir.write("blank.txt", "\n  \n");
    let binary = root.join("bin.txt");
    std::fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
    for bad in [&not_txt, &blank, &binary] {
        assert_eq!(
            code(root, &["text", "import", &id, bad.to_str().unwrap()]),
            Some(1),
            "{bad:?}"
        );
    }
    assert_eq!(
        ok_json(root, &["text", "show", &id, "--json"])["chapters"],
        json!([])
    );
    no_court(root);
}

#[test]
fn text_help_shows_usage_without_a_project() {
    let dir = TempDir::new("help");
    let root = dir.path();
    for args in [
        &["text"][..],
        &["text", "help"],
        &["text", "--help"],
        &["text", "check", "--help"],
    ] {
        let out = ok(root, args);
        let help = stdout(&out);
        for needle in [
            "text init",
            "text import",
            "text show",
            "text check",
            "--candidate",
        ] {
            assert!(help.contains(needle), "{args:?} 缺少 {needle}：{help}");
        }
    }
    let help = stdout(&ok(root, &["help"]));
    assert!(help.contains("ziyuan text init"), "{help}");
    let bash = stdout(&ok(root, &["completions", "bash"]));
    assert!(bash.contains("text"));
    assert!(!root.join(".ziyuan").exists());
}

#[test]
fn notes_and_context_are_offline_and_json_round_trip() {
    let dir = TempDir::new("notes");
    let root = dir.path();
    let id = init_json(root, "Tale", "en");
    let note = ok_json(
        root,
        &[
            "note",
            "add",
            "--source",
            "moon",
            "--zh",
            "月亮",
            "--remark",
            "认可",
            "--project",
            &id,
            "--json",
        ],
    );
    let n = note["id"].as_str().unwrap();
    assert_eq!(note["kind"], "word");
    assert!(note["created"].is_u64());
    let listed = ok_json(
        root,
        &[
            "note",
            "list",
            "--project",
            &id,
            "--search",
            "MOON",
            "--kind",
            "word",
            "--json",
        ],
    );
    assert_eq!(listed, json!([note]));
    assert_eq!(ok_json(root, &["note", "show", n, "--json"]), note);
    assert_eq!(
        ok_json(root, &["note", "edit", n, "--remark", "", "--json"])["remark"],
        ""
    );
    let promoted = ok_json(root, &["note", "promote", n, "--project", &id, "--json"]);
    assert_eq!(promoted["added"], true);
    assert_eq!(promoted["requires_context_approval"], true);
    assert_eq!(
        ok_json(root, &["note", "promote", n, "--project", &id, "--json"])["added"],
        false
    );
    assert_eq!(
        ok_json(root, &["text", "context", "approve", &id, "--json"]),
        json!({"project":id,"version":1})
    );
    assert_eq!(ok_json(root, &["note", "remove", n, "--json"])["id"], n);
    assert_eq!(ok_json(root, &["note", "list", "--json"]), json!([]));
    no_court(root);
}

#[test]
fn translation_status_zhupi_and_queued_issuance() {
    let dir = TempDir::new("translation");
    let root = dir.path();
    let id = init_json(root, "Tale", "en");
    let source = dir.write("one.txt", "Alpha.\n\nBeta.\n");
    let p = ok_json(
        root,
        &["text", "import", &id, source.to_str().unwrap(), "--json"],
    );
    let b = p["chapters"][0]["blocks"][0]["block_id"].as_str().unwrap();
    let para = p["chapters"][0]["blocks"][0]["paragraphs"][0]["id"]
        .as_str()
        .unwrap();
    ok(root, &["text", "zhupi", &id, b, para, "平实一些"]);
    let z = ok_json(root, &["text", "zhupi", &id, b, para, "--show", "--json"]);
    assert_eq!(z["items"][para]["text"], "平实一些");
    assert_eq!(
        ok_json(root, &["text", "zhupi", &id, b, para, "--clear", "--json"])["version"],
        2
    );
    no_court(root);
    ok(root, &["init"]);
    let s = ok_json(root, &["text", "status", &id, "--json"]);
    assert_eq!(s[0]["state"]["state"], "not_started");
    let issued = ok_json(
        root,
        &["text", "translate", &id, b, "--dept", "works", "--json"],
    );
    assert!(issued["task"].is_u64());
    assert_eq!(
        ok_json(root, &["text", "status", &id, "--json"])[0]["state"]["state"],
        "running"
    );
    assert_eq!(
        ok_json(root, &["text", "resume", &id, "--yes", "--json"])["issued"],
        json!([])
    );
    for action in ["translate", "review", "revise", "finalize"] {
        assert_eq!(code(root, &["text", action, &id, b]), Some(1));
    }
}

#[test]
fn new_cli_usage_and_completions() {
    let dir = TempDir::new("new-usage");
    let root = dir.path();
    for args in [
        vec!["note", "add"],
        vec!["note", "add", "--source", "x", "--zh", "y", "--kind", "bad"],
        vec!["note", "list", "--search"],
        vec!["note", "show"],
        vec!["note", "edit", "id"],
        vec!["note", "promote", "id"],
        vec!["note", "remove", "id", "extra"],
        vec!["note", "list", "--kind", "word", "--kind", "passage"],
        vec!["text", "translate", "p"],
        vec!["text", "translate", "p", "b", "--dept", "rites"],
        vec!["text", "review", "p", "b", "extra"],
        vec!["text", "reopen", "p", "b"],
        vec!["text", "reopen", "p", "b", "--reason", ""],
        vec!["text", "resume", "p", "--yes", "--yes"],
        vec!["text", "context", "approve"],
        vec!["text", "zhupi", "p", "b", "para", "--clear", "text"],
    ] {
        assert_eq!(code(root, &args), Some(2), "{args:?}");
    }
    assert!(stdout(&ok(root, &["note", "--help"])).contains("text context approve"));
    for shell in ["bash", "zsh", "fish"] {
        let c = stdout(&ok(root, &["completions", shell]));
        for word in [
            "note",
            "promote",
            "translate",
            "zhupi",
            "finalize",
            "resume",
            // 补全词取自 help.rs 的子命令清单：从任一清单里漏掉 `recovery` 都会在这里失败。
            "recovery",
        ] {
            assert!(c.contains(word), "{shell}: {word}");
        }
    }
}
