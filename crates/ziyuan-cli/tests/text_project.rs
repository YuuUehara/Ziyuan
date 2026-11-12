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

fn assert_import_matches_preview(project: &Value, preview: &Value) {
    let imported = project["chapters"].as_array().unwrap();
    let prepared = preview["chapters"].as_array().unwrap();
    assert_eq!(imported.len(), prepared.len());
    for (actual, expected) in imported.iter().zip(prepared) {
        for field in [
            "id",
            "source_name",
            "sha256",
            "byte_len",
            "paragraphs",
            "blocks",
            "parsing",
        ] {
            assert_eq!(actual[field], expected[field], "chapter field {field}");
        }
        let actual_path = Path::new(actual["original_path"].as_str().unwrap());
        let preview_path = Path::new(expected["original_path"].as_str().unwrap());
        assert!(
            actual_path.ends_with(preview_path),
            "{actual_path:?} does not end with {preview_path:?}"
        );
    }
}

/// Include directories as well as bytes: a read-only preview must not create caches or locks.
fn tree_snapshot(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    fn walk(root: &Path, dir: &Path, entries: &mut Vec<(PathBuf, Option<Vec<u8>>)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_owned();
            if path.is_dir() {
                entries.push((relative, None));
                walk(root, &path, entries);
            } else {
                entries.push((relative, Some(std::fs::read(path).unwrap())));
            }
        }
    }
    let mut entries = Vec::new();
    walk(root, root, &mut entries);
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

#[test]
fn prepared_import_options_are_usage_errors_before_io() {
    let dir = TempDir::new("preview-usage");
    let root = dir.path();
    let cases: &[&[&str]] = &[
        &["preview"],
        &["preview", "id"],
        &["preview", "id", "a.md", "--ruby", "--ruby"],
        &[
            "preview",
            "id",
            "a.md",
            "--heading-level",
            "1",
            "--heading-level",
            "2",
        ],
        &["preview", "id", "a.md", "--heading-level", "3"],
        &["preview", "id", "a.md", "--heading-level"],
        &["preview", "id", "a.md", "--chapter-title"],
        &["preview", "id", "a.md", "--chapter-title", "  "],
        &["preview", "id", "a.md", "--chapter-title", "--ruby"],
        &["preview", "id", "a.md", "--unknown"],
        &["preview", "id", "a.md", "--confirm", "abc"],
        &["import", "id", "a.txt", "--ruby"],
        &["import", "id", "a.txt", "--heading-level", "1"],
        &["import", "id", "a.txt", "--chapter-title", "章"],
        &["import", "id", "a.txt", "--confirm"],
        &["import", "id", "a.txt", "--confirm", ""],
        &["import", "id", "a.txt", "--confirm", "a", "--confirm", "b"],
    ];
    for case in cases {
        let mut args = vec!["text"];
        args.extend_from_slice(case);
        let out = ziyuan(root, &args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(tree_snapshot(root).is_empty());
}

#[test]
fn preview_is_read_only_and_confirmed_markdown_preserves_order_and_titles() {
    let dir = TempDir::new("preview-md");
    let root = dir.path();
    let id = init_json(root, "章序", "ja");
    dir.write("z.MD", "# 第一章\n\n｜漢字《かんじ》。\n");
    dir.write("a.MarkDown", "# 第二章\n\n次の朝。\n");
    let options = [
        "z.MD",
        "a.MarkDown",
        "--ruby",
        "--heading-level",
        "1",
        "--chapter-title",
        "先の章",
        "--chapter-title",
        "後の章",
    ];
    let mut args = vec!["text", "preview", &id];
    args.extend_from_slice(&options);
    let before = tree_snapshot(root);
    let out = ok(root, &args);
    let text = stdout(&out);
    for expected in [
        "先の章",
        "後の章",
        "原文字节范围",
        "段",
        "提示块",
        "预览指纹",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    args.push("--json");
    let preview = ok_json(root, &args);
    assert_eq!(tree_snapshot(root), before);
    assert_eq!(preview["project"], id);
    assert_eq!(preview["options"]["parsing"]["ruby"], true);
    assert_eq!(preview["options"]["parsing"]["heading_level"], 1);
    assert_eq!(preview["chapters"][0]["source_name"], "z.MD");
    assert_eq!(preview["chapters"][1]["source_name"], "a.MarkDown");
    assert_eq!(preview["chapters"][0]["parsing"]["title"], "先の章");
    assert_eq!(preview["chapters"][1]["parsing"]["title"], "後の章");
    assert_eq!(preview["chapters"][0]["parsing"]["source_file"], 1);
    assert_eq!(preview["chapters"][1]["parsing"]["source_file"], 2);
    let fingerprint = preview["fingerprint"].as_str().unwrap();
    assert!(!fingerprint.is_empty());
    let mut import = vec!["text", "import", &id];
    import.extend_from_slice(&options);
    import.extend_from_slice(&["--confirm", fingerprint, "--json"]);
    let project = ok_json(root, &import);
    assert_import_matches_preview(&project, &preview);
    no_court(root);
}

#[test]
fn stale_preview_fails_without_import_and_fresh_preview_can_confirm() {
    let dir = TempDir::new("preview-stale");
    let root = dir.path();
    let id = init_json(root, "变化", "ja");
    dir.write("chapter.md", "# 章\n\n元の本文。\n");
    let preview = ok_json(root, &["text", "preview", &id, "chapter.md", "--json"]);
    dir.write("chapter.md", "# 章\n\n変わった本文。\n");
    let out = ziyuan(
        root,
        &[
            "text",
            "import",
            &id,
            "chapter.md",
            "--confirm",
            preview["fingerprint"].as_str().unwrap(),
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let project = ok_json(root, &["text", "show", &id, "--json"]);
    assert!(project["chapters"].as_array().unwrap().is_empty());
    let fresh = ok_json(root, &["text", "preview", &id, "chapter.md", "--json"]);
    assert_ne!(preview["fingerprint"], fresh["fingerprint"]);
    ok(
        root,
        &[
            "text",
            "import",
            &id,
            "chapter.md",
            "--confirm",
            fresh["fingerprint"].as_str().unwrap(),
        ],
    );
    no_court(root);
}

#[test]
fn plain_txt_keeps_legacy_bom_until_explicit_preview_confirmation() {
    let dir = TempDir::new("preview-bom");
    let root = dir.path();
    let original = "\u{feff}本文。\r\n";
    let path = dir.write("chapter.txt", original);
    let legacy_id = init_json(root, "旧解析", "ja");
    let legacy = ok_json(
        root,
        &["text", "import", &legacy_id, "chapter.txt", "--json"],
    );
    assert_eq!(
        legacy["chapters"][0]["paragraphs"][0]["text"],
        "\u{feff}本文。"
    );
    assert!(legacy["chapters"][0]["parsing"].is_null());
    let id = init_json(root, "新解析", "ja");
    let preview = ok_json(root, &["text", "preview", &id, "chapter.txt", "--json"]);
    assert_eq!(preview["chapters"][0]["paragraphs"][0]["text"], "本文。");
    let project = ok_json(
        root,
        &[
            "text",
            "import",
            &id,
            "chapter.txt",
            "--confirm",
            preview["fingerprint"].as_str().unwrap(),
            "--json",
        ],
    );
    assert_import_matches_preview(&project, &preview);
    assert_eq!(std::fs::read(path).unwrap(), original.as_bytes());
}

#[test]
fn preview_io_and_title_count_errors_use_runtime_exit_code() {
    let dir = TempDir::new("preview-errors");
    let root = dir.path();
    let id = init_json(root, "错误", "ja");
    assert_eq!(code(root, &["text", "preview", &id, "missing.md"]), Some(1));
    dir.write("chapter.md", "# 一\n\n本文。\n");
    assert_eq!(
        code(
            root,
            &[
                "text",
                "preview",
                &id,
                "chapter.md",
                "--chapter-title",
                "一",
                "--chapter-title",
                "二"
            ]
        ),
        Some(1)
    );
    assert_eq!(
        code(root, &["text", "preview", "missing-project", "chapter.md"]),
        Some(1)
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
            "text human submit",
            "text human approve",
            "text human finalize",
        ] {
            assert!(help.contains(needle), "{args:?} 缺少 {needle}：{help}");
        }
    }
    let help = stdout(&ok(root, &["help"]));
    assert!(help.contains("ziyuan text init"), "{help}");
    assert!(help.contains("ziyuan text human"), "{help}");
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
        vec!["text", "memory", "search", "p"],
        vec!["text", "memory", "search", "p", "--query", ""],
        vec![
            "text", "memory", "search", "p", "--query", "x", "--limit", "0",
        ],
        vec![
            "text", "memory", "search", "p", "--query", "x", "--limit", "51",
        ],
        vec!["text", "memory", "build", "p", "--all"],
        vec!["text", "memory", "status", "p", "--project", "q"],
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
            "human",
            "memory",
            "query",
        ] {
            assert!(c.contains(word), "{shell}: {word}");
        }
    }
}

#[test]
fn memory_cli_is_explicit_and_rebuilds_only_derived_data() {
    let dir = TempDir::new("memory");
    let root = dir.path();
    let project = init_json(root, "记忆库测试", "ja");
    dir.write("chapter.txt", "夜が明けた。\n\n風が止んだ。\n");
    ok(root, &["text", "import", &project, "chapter.txt"]);
    ok(root, &["init"]);
    let status = ok_json(root, &["text", "memory", "status", &project, "--json"]);
    assert_eq!(status["built"], false);
    assert_eq!(status["records"], 0);
    let disabled = ziyuan(
        root,
        &["text", "memory", "search", &project, "--query", "夜"],
    );
    assert_eq!(disabled.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&disabled.stderr).contains("ZIYUAN_MEMORY_PROVIDER"));
    assert!(!root.join(".ziyuan/memory/index.json").exists());

    let local = |args: &[&str]| {
        let mut command = Command::new(BIN);
        command.current_dir(root).args(args);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("ZIYUAN_") {
                command.env_remove(key);
            }
        }
        let result = command
            .env("ZIYUAN_MEMORY_PROVIDER", "local")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice::<Value>(&result.stdout).unwrap()
    };
    let before = std::fs::read(root.join(".ziyuan/court.log")).unwrap();
    let built = local(&["text", "memory", "build", &project, "--json"]);
    assert_eq!(built["records"], 0, "未定稿不能收录");
    assert_eq!(built["skipped"].as_array().unwrap().len(), 1);
    let hits = local(&[
        "text", "memory", "search", &project, "--query", "夜", "--json",
    ]);
    assert!(hits["hits"].as_array().unwrap().is_empty());
    std::fs::remove_file(root.join(".ziyuan/memory/index.json")).unwrap();
    assert_eq!(
        local(&["text", "memory", "build", &project, "--json"])["records"],
        0
    );
    assert_eq!(
        std::fs::read(root.join(".ziyuan/court.log")).unwrap(),
        before
    );
    assert_eq!(
        std::fs::read_to_string(root.join("chapter.txt")).unwrap(),
        "夜が明けた。\n\n風が止んだ。\n"
    );
}

/// Read the short, regular-file USTAR entries emitted by the export archive writer.
fn export_tar_entries(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut entries = Vec::new();
    let mut offset = 0;
    while offset + 512 <= bytes.len() {
        let header = &bytes[offset..offset + 512];
        if header.iter().all(|byte| *byte == 0) {
            break;
        }
        let name_end = header[..100].iter().position(|b| *b == 0).unwrap_or(100);
        let name = std::str::from_utf8(&header[..name_end]).unwrap();
        let size = std::str::from_utf8(&header[124..136])
            .unwrap()
            .trim_matches(['\0', ' ']);
        let size = usize::from_str_radix(size, 8).unwrap();
        assert!(matches!(header[156], 0 | b'0'), "regular files only");
        offset += 512;
        entries.push((name.to_owned(), bytes[offset..offset + size].to_vec()));
        offset += size.div_ceil(512) * 512;
    }
    entries
}

#[test]
fn export_originals_preview_archive_order_and_no_overwrite() {
    let dir = TempDir::new("export-originals");
    let root = dir.path();
    let project = init_json(root, "原件", "ja");
    let first = "\u{feff}第一章。\r\n\r\n原文。\r\n";
    let second = "第二章。\n";
    dir.write("first.txt", first);
    dir.write("second.txt", second);
    let imported = ok_json(
        root,
        &[
            "text",
            "import",
            &project,
            "first.txt",
            "second.txt",
            "--json",
        ],
    );
    let before = tree_snapshot(root);
    let preview = ok_json(root, &["text", "export-originals", &project, "--json"]);
    assert_eq!(tree_snapshot(root), before, "JSON preview must not write");
    let text = ok(root, &["text", "export-originals", &project]);
    assert!(stdout(&text).contains("只读"));
    assert_eq!(tree_snapshot(root), before, "text preview must not write");
    let files = preview["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["size"], first.len());
    assert!(files.iter().all(|file| file.get("bytes").is_none()));
    assert!(preview["manifest"].is_object());
    let chapters = imported["chapters"].as_array().unwrap();
    let selected = ok_json(
        root,
        &[
            "text",
            "export-originals",
            &project,
            "--chapter",
            chapters[1]["id"].as_str().unwrap(),
            "--chapter",
            chapters[0]["id"].as_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(
        selected["files"], preview["files"],
        "selection uses project order"
    );
    let written = ok_json(
        root,
        &[
            "text",
            "export-originals",
            &project,
            "--output",
            "originals.tar",
            "--json",
        ],
    );
    assert_eq!(written["output"], "originals.tar");
    let archive = std::fs::read(root.join("originals.tar")).unwrap();
    let entries = export_tar_entries(&archive);
    for (index, expected) in [first, second].iter().enumerate() {
        let name = files[index]["name"].as_str().unwrap();
        let entry = entries.iter().find(|entry| entry.0 == name).unwrap();
        assert_eq!(entry.1, expected.as_bytes());
    }
    let manifest = entries
        .iter()
        .find(|entry| entry.0 == "manifest.json")
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&manifest.1).unwrap(),
        preview["manifest"]
    );
    assert_eq!(entries.len(), 3);
    let before_failure = tree_snapshot(root);
    for destination in [
        "originals.tar",
        "first.txt",
        "missing/export.tar",
        ".ziyuan/forbidden.tar",
    ] {
        assert_eq!(
            code(
                root,
                &[
                    "text",
                    "export-originals",
                    &project,
                    "--output",
                    destination
                ]
            ),
            Some(1)
        );
        assert_eq!(
            tree_snapshot(root),
            before_failure,
            "failed destination {destination}"
        );
    }
    // A fresh output invocation must recheck source integrity, not trust the preview.
    let original = imported["chapters"][0]["original_path"].as_str().unwrap();
    let project_dir = root.join(".ziyuan/text-projects").join(&project);
    std::fs::write(project_dir.join(original), "tampered").unwrap();
    assert_eq!(
        code(
            root,
            &[
                "text",
                "export-originals",
                &project,
                "--output",
                "stale.tar"
            ]
        ),
        Some(1)
    );
    assert!(!root.join("stale.tar").exists());
    no_court(root);
}

#[test]
fn export_usage_is_strict_and_finished_requires_court() {
    let dir = TempDir::new("export-usage");
    let root = dir.path();
    for action in ["export-originals", "export-finished"] {
        let before = tree_snapshot(root);
        for suffix in [
            vec![],
            vec![""],
            vec!["p", "--unknown", "x"],
            vec!["p", "--chapter"],
            vec!["p", "--chapter", " "],
            vec!["p", "--output", ""],
            vec!["p", "--output", "a", "--output", "b"],
            vec!["p", "extra"],
            vec!["p", "--format", "pdf"],
            vec!["p", "--layout", "wrong"],
            vec!["p", "--format", "txt", "--format", "md"],
            vec!["p", "--layout", "chapters", "--layout", "book"],
        ] {
            let mut args = vec!["text", action];
            args.extend(suffix);
            assert_eq!(code(root, &args), Some(2), "{args:?}");
        }
        assert_eq!(tree_snapshot(root), before);
    }
    for flag in ["--format", "--layout"] {
        let value = if flag == "--format" {
            "txt"
        } else {
            "chapters"
        };
        assert_eq!(
            code(root, &["text", "export-originals", "p", flag, value]),
            Some(2)
        );
    }
    let project = init_json(root, "未定稿", "ja");
    dir.write("chapter.txt", "原文。\n");
    let imported = ok_json(root, &["text", "import", &project, "chapter.txt", "--json"]);
    let chapter = imported["chapters"][0]["id"].as_str().unwrap();
    let before = tree_snapshot(root);
    assert_eq!(
        code(
            root,
            &[
                "text",
                "export-originals",
                &project,
                "--chapter",
                chapter,
                "--chapter",
                chapter
            ]
        ),
        Some(1)
    );
    for suffix in [
        vec![],
        vec!["--json"],
        vec![
            "--format",
            "md",
            "--layout",
            "book",
            "--output",
            "finished.tar",
        ],
    ] {
        let mut args = vec!["text", "export-finished", &project];
        args.extend(suffix);
        assert_eq!(code(root, &args), Some(1));
    }
    assert_eq!(tree_snapshot(root), before);
    no_court(root);
}

#[test]
fn selected_reference_flags_are_usage_errors_before_file_io() {
    let dir = TempDir::new("reference-usage");
    for args in [
        vec!["references", "preview", "p", "b"],
        vec![
            "references",
            "preview",
            "p",
            "b",
            "--kind",
            "review",
            "--selection",
            "missing.json",
        ],
        vec![
            "references",
            "preview",
            "p",
            "b",
            "--kind",
            "translate",
            "--kind",
            "revise",
            "--selection",
            "missing.json",
        ],
        vec![
            "references",
            "preview",
            "p",
            "b",
            "--kind",
            "translate",
            "--selection",
            "missing.json",
            "--unknown",
            "x",
        ],
        vec!["references", "show"],
        vec!["references", "show", "no-task"],
        vec!["references", "show", "1", "--kind", "translate"],
        vec!["translate", "p", "b", "--references", "missing.json"],
        vec![
            "revise",
            "p",
            "b",
            "--confirm",
            "fingerprint",
            "--submission",
            "id",
        ],
        vec![
            "translate",
            "p",
            "b",
            "--references",
            "missing.json",
            "--confirm",
            "fp",
            "--submission",
            "bad/id",
        ],
        vec![
            "revise",
            "p",
            "b",
            "--references",
            "missing.json",
            "--confirm",
            "fp",
            "--submission",
            "非ASCII",
        ],
        vec![
            "translate",
            "p",
            "b",
            "--references",
            "missing.json",
            "--confirm",
            "fp",
            "--submission",
            "id",
            "--submission",
            "id",
        ],
    ] {
        let mut command = vec!["text"];
        command.extend(args);
        assert_eq!(code(dir.path(), &command), Some(2), "{command:?}");
    }
    for submission in ["".to_owned(), "a".repeat(129)] {
        assert_eq!(
            code(
                dir.path(),
                &[
                    "text",
                    "translate",
                    "p",
                    "b",
                    "--references",
                    "missing.json",
                    "--confirm",
                    "fp",
                    "--submission",
                    &submission
                ]
            ),
            Some(2)
        );
    }
    assert!(stdout(&ok(dir.path(), &["text", "help"])).contains("authorized_projects"));
    no_court(dir.path());
}

#[test]
fn selected_reference_preview_and_persisted_submission_are_offline() {
    use ziyuan_app::Session;
    use ziyuan_core::{CourtEvent, FileChange, TaskId, TranslationKind};
    use ziyuan_storage::{EventLog, EventStore};

    let dir = TempDir::new("reference-roundtrip");
    let root = dir.path();
    let project = init_json(root, "Reference target", "en");
    dir.write("reference.txt", "Alpha reference.\n");
    dir.write("chapter.txt", "Alpha.\n\nBeta.\n");
    let imported = ok_json(
        root,
        &[
            "text",
            "import",
            &project,
            "reference.txt",
            "chapter.txt",
            "--json",
        ],
    );
    let block = imported["chapters"][1]["blocks"][0]["block_id"]
        .as_str()
        .unwrap();
    ok(root, &["init"]);
    let reference_block = imported["chapters"][0]["blocks"][0]["block_id"]
        .as_str()
        .unwrap();
    let issued = ok_json(
        root,
        &["text", "translate", &project, reference_block, "--json"],
    );
    let task = TaskId(issued["task"].as_u64().unwrap());
    let mut candidate = imported["chapters"][0]["blocks"][0].clone();
    candidate["paragraphs"][0]["text"] = json!("阿尔法参考译文。");
    // Fixture only the worker response, using the same legal assignment/proposal/completion
    // lifecycle as app translation fixtures. CLI apply + finalize establish the real source.
    let session = Session::open(EventLog::new(root)).unwrap();
    let worker = session
        .court()
        .route(&session.court().task(task).unwrap().edict)
        .unwrap()
        .worker;
    let events = vec![
        CourtEvent::TaskAssigned {
            task,
            worker,
            model: None,
        },
        CourtEvent::ChangesProposed {
            task,
            changes: vec![FileChange {
                path: format!("translations/{project}/{reference_block}/zh.json"),
                before: None,
                after: serde_json::to_string_pretty(&candidate).unwrap(),
            }],
        },
        CourtEvent::TaskCompleted {
            task,
            worker,
            summary: "Fixture translation complete".into(),
        },
    ];
    let mut log = EventLog::new(root);
    let history = log.load().unwrap();
    let mut court = ziyuan_governance::Court::replay(&history).unwrap();
    for event in &events {
        court.apply(event).unwrap();
    }
    log.append(history.len(), &events).unwrap();
    drop(session);
    let task_id = task.0.to_string();
    ok(root, &["apply", &task_id]);
    ok(root, &["text", "finalize", &project, reference_block]);

    let local_memory = |args: &[&str]| {
        let mut command = Command::new(BIN);
        command.current_dir(root).args(args);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("ZIYUAN_") {
                command.env_remove(key);
            }
        }
        let out = command
            .env("ZIYUAN_MEMORY_PROVIDER", "local")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()
    };
    local_memory(&["text", "memory", "build", &project, "--json"]);
    let hits = local_memory(&[
        "text",
        "memory",
        "search",
        &project,
        "--query",
        "Alpha reference.",
        "--json",
    ]);
    let hit = &hits["hits"][0];
    assert_eq!(hit["selection"]["identity"], hit["identity"]);
    assert_eq!(hit["selection"]["origin"], hit["origin"]);
    assert!(hit["selection"]["digests"].is_object());
    for field in [
        "source",
        "target",
        "score",
        "keyword_score",
        "cosine_score",
        "keyword_rank",
        "vector_rank",
    ] {
        assert!(hit.get(field).is_some(), "legacy hit field {field}");
    }
    dir.write(
        "selection.json",
        &serde_json::to_string(&json!({"selections":[hit["selection"]]})).unwrap(),
    );
    let before = tree_snapshot(root);
    let preview_args = [
        "text",
        "references",
        "preview",
        &project,
        block,
        "--kind",
        "translate",
        "--selection",
        "selection.json",
        "--json",
    ];
    let preview = ok_json(root, &preview_args);
    assert_eq!(preview["project"], project);
    assert_eq!(preview["block"], block);
    assert_eq!(
        preview["kind"],
        serde_json::to_value(TranslationKind::Translate).unwrap()
    );
    assert_eq!(
        preview["bytes"].as_u64().unwrap(),
        preview["prompt"].as_str().unwrap().len() as u64
    );
    assert_eq!(
        tree_snapshot(root),
        before,
        "preview must not export or enqueue"
    );
    let human = ok(root, &preview_args[..preview_args.len() - 1]);
    assert!(stdout(&human).contains(preview["prompt"].as_str().unwrap()));
    assert!(stdout(&human).contains("外部模型"));
    assert!(stdout(&human).contains("仅入队"));
    assert_eq!(tree_snapshot(root), before);

    for body in [
        "arbitrary reference prose",
        r#"{"selections":[]}"#,
        r#"{"selections":[],"prompt":"injected"}"#,
        r#"{"selections":[],"selections":[]}"#,
    ] {
        dir.write("bad.json", body);
        assert_eq!(
            code(
                root,
                &[
                    "text",
                    "references",
                    "preview",
                    &project,
                    block,
                    "--kind",
                    "translate",
                    "--selection",
                    "bad.json"
                ]
            ),
            Some(1)
        );
    }
    assert_eq!(
        code(
            root,
            &[
                "text",
                "references",
                "preview",
                &project,
                block,
                "--kind",
                "translate",
                "--selection",
                "missing.json"
            ]
        ),
        Some(1)
    );
    let fingerprint = preview["fingerprint"].as_str().unwrap();
    let issue_args = [
        "text",
        "translate",
        &project,
        block,
        "--references",
        "selection.json",
        "--confirm",
        fingerprint,
        "--submission",
        "CLI-retry_1",
        "--json",
    ];
    let issued = ok_json(root, &issue_args);
    let task = issued["task"].as_u64().unwrap().to_string();
    let after_issue = tree_snapshot(root);
    // Separate CLI processes reopen Session from disk; retries must not enqueue twice.
    assert_eq!(ok_json(root, &issue_args), issued);
    assert_eq!(tree_snapshot(root), after_issue);
    let shown = ok_json(root, &["text", "references", "show", &task, "--json"]);
    assert_eq!(shown["snapshot"], preview);
    assert_eq!(shown["current"], true);
    ok(
        root,
        &[
            "text",
            "zhupi",
            &project,
            block,
            imported["chapters"][1]["blocks"][0]["paragraphs"][0]["id"]
                .as_str()
                .unwrap(),
            "Changed note",
        ],
    );
    let stale = ok_json(root, &["text", "references", "show", &task, "--json"]);
    assert_eq!(
        stale["snapshot"], preview,
        "show must retain the exact issued snapshot"
    );
    assert_eq!(stale["current"], false);
    assert!(stale["reason"].is_string());
    let human = stdout(&ok(root, &["text", "references", "show", &task]));
    assert!(human.contains("stale"));
    assert!(human.contains(preview["prompt"].as_str().unwrap()));
}

#[test]
fn human_draft_save_then_peek_round_trips() {
    let dir = TempDir::new("human-draft");
    let root = dir.path();
    let id = init_json(root, "草稿项目", "ja");
    dir.write("a.md", "# 章一\n\n夜が明ける。\n");
    let preview = ok_json(
        root,
        &[
            "text",
            "preview",
            &id,
            "a.md",
            "--chapter-title",
            "章一",
            "--json",
        ],
    );
    let fingerprint = preview["fingerprint"].as_str().unwrap();
    let project = ok_json(
        root,
        &[
            "text",
            "import",
            &id,
            "a.md",
            "--chapter-title",
            "章一",
            "--confirm",
            fingerprint,
            "--json",
        ],
    );
    let block = project["chapters"][0]["blocks"][0]["block_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let paragraph = project["chapters"][0]["blocks"][0]["paragraphs"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let view = ok_json(root, &["text", "human", "show", &id, &block, "--json"]);
    let save = json!({
        "request_id": "save-1",
        "expected_revision": null,
        "basis": view["basis"],
        "paragraphs": [{"id": paragraph, "text": "人工译文"}],
    });
    dir.write("draft.json", &save.to_string());
    let out = ok(
        root,
        &[
            "text",
            "human",
            "save",
            &id,
            &block,
            "--file",
            "draft.json",
            "--json",
        ],
    );
    let receipt: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(receipt["revision"].as_str().is_some());
    let view = ok_json(root, &["text", "human", "show", &id, &block, "--json"]);
    assert_eq!(view["draft"]["paragraphs"][0]["text"], "人工译文");
    no_court(root);
}

#[test]
fn human_submit_freezes_candidate_and_is_idempotent() {
    let dir = TempDir::new("human-submit");
    let root = dir.path();
    ok(root, &["init"]);
    let id = init_json(root, "送审项目", "ja");
    dir.write("a.md", "# 章一\n\n夜が明ける。\n");
    let preview = ok_json(
        root,
        &[
            "text",
            "preview",
            &id,
            "a.md",
            "--chapter-title",
            "章一",
            "--json",
        ],
    );
    let fingerprint = preview["fingerprint"].as_str().unwrap();
    let project = ok_json(
        root,
        &[
            "text",
            "import",
            &id,
            "a.md",
            "--chapter-title",
            "章一",
            "--confirm",
            fingerprint,
            "--json",
        ],
    );
    let block = project["chapters"][0]["blocks"][0]["block_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut seen = std::collections::BTreeSet::new();
    let mut paragraphs: Vec<Value> = Vec::new();
    for block in project["chapters"][0]["blocks"].as_array().unwrap() {
        for p in block["paragraphs"].as_array().unwrap() {
            let id = p["id"].as_str().unwrap().to_owned();
            if seen.insert(id.clone()) {
                paragraphs.push(json!({"id": id, "text": "人工译文"}));
            }
        }
    }
    paragraphs.sort_by_key(|p| p["id"].as_str().unwrap_or_default().to_owned());
    let view = ok_json(root, &["text", "human", "show", &id, &block, "--json"]);
    let save = json!({
        "request_id": "save-1",
        "expected_revision": null,
        "basis": view["basis"],
        "paragraphs": paragraphs,
    });
    dir.write("draft.json", &save.to_string());
    ok(
        root,
        &["text", "human", "save", &id, &block, "--file", "draft.json"],
    );
    let first = ok_json(root, &["text", "human", "submit", &id, &block, "--json"]);
    assert_eq!(first["published"], true);
    assert!(root.join(".ziyuan/court.log").exists());
    let second = ok_json(root, &["text", "human", "submit", &id, &block, "--json"]);
    assert_eq!(second["published"], false);
    let zh = root
        .join("translations")
        .join(&id)
        .join(&block)
        .join("zh.json");
    assert!(!zh.exists(), "送审不写译稿");
    let approved = stdout(&ok(root, &["text", "human", "approve", &id, &block]));
    assert!(approved.contains("已准奏并落盘"), "{approved}");
    assert!(
        approved.contains(&format!("下一步：ziyuan text human finalize {id} {block}")),
        "{approved}"
    );
    assert!(!approved.contains("已批准"), "{approved}");
    let written = std::fs::read(&zh).expect("准奏即落盘");
    let target: Value = serde_json::from_slice(&written).unwrap();
    assert_eq!(target["schema_version"], 1, "无标记候选按第一版落盘");
    let again = ok_json(root, &["text", "human", "approve", &id, &block, "--json"]);
    assert_eq!(again["approved"], false);
    let again = stdout(&ok(root, &["text", "human", "approve", &id, &block]));
    assert!(again.contains("ziyuan text human finalize"), "{again}");
    assert!(!root.join(".ziyuan/recovery.json").exists());
    let log = std::fs::read(root.join(".ziyuan/court.log")).unwrap();
    let applied = ok_json(root, &["text", "human", "apply", &id, &block, "--json"]);
    assert_eq!(applied["applied"], false);
    assert_eq!(applied["already_written"], true);
    let applied = stdout(&ok(root, &["text", "human", "apply", &id, &block]));
    assert!(applied.contains("准奏时已落盘"), "{applied}");
    assert!(
        applied.contains(&format!("下一步：ziyuan text human finalize {id} {block}")),
        "{applied}"
    );
    assert_eq!(std::fs::read(&zh).unwrap(), written, "apply 只读");
    assert_eq!(
        std::fs::read(root.join(".ziyuan/court.log")).unwrap(),
        log,
        "apply 只读"
    );
    let finalized = stdout(&ok(root, &["text", "human", "finalize", &id, &block]));
    assert!(finalized.contains("已定稿"), "{finalized}");
    assert!(
        finalized.contains(&format!("下一步：ziyuan text export-finished {id}")),
        "{finalized}"
    );
    let again_final = ok_json(root, &["text", "human", "finalize", &id, &block, "--json"]);
    assert_eq!(again_final["finalized"], false);
    // The suggested next step really exports a human finalization.
    let exported = ok_json(root, &["text", "export-finished", &id, "--json"]);
    assert_eq!(
        exported["manifest"]["chapters"][0]["blocks"][0]["source"],
        "human"
    );

    // 第二轮：带注音的新修订号，按第二版落盘并可再定稿。
    let view = ok_json(root, &["text", "human", "show", &id, &block, "--json"]);
    let mut marked = paragraphs.clone();
    marked[0] = json!({
        "id": marked[0]["id"],
        "text": "人工译文",
        "marks": [{"start": 0, "end": 6, "kind": {"type": "ruby", "reading": "じんこう"}}],
    });
    let save = json!({
        "request_id": "save-2",
        "expected_revision": view["draft"]["revision"],
        "basis": view["basis"],
        "paragraphs": marked,
    });
    dir.write("draft.json", &save.to_string());
    let saved = stdout(&ok(
        root,
        &["text", "human", "save", &id, &block, "--file", "draft.json"],
    ));
    assert!(saved.contains("修订号"), "{saved}");
    assert!(!saved.contains("revision"), "{saved}");
    let shown = stdout(&ok(root, &["text", "human", "show", &id, &block]));
    assert!(shown.contains("修订号："), "{shown}");
    ok(root, &["text", "human", "submit", &id, &block]);
    ok(root, &["text", "human", "approve", &id, &block]);
    let target: Value = serde_json::from_slice(&std::fs::read(&zh).unwrap()).unwrap();
    assert_eq!(target["schema_version"], 2, "带标记候选按第二版落盘");
    let status = stdout(&ok(root, &["text", "status", &id]));
    assert!(
        status.contains("已人工准奏"),
        "新一轮准奏不被旧定稿遮住：{status}"
    );
    assert!(status.contains("可人工定稿"), "{status}");
    let finalized = ok_json(root, &["text", "human", "finalize", &id, &block, "--json"]);
    assert_eq!(finalized["finalized"], true);
    let status = stdout(&ok(root, &["text", "status", &id]));
    assert!(status.contains("已人工定稿"), "{status}");
    // TXT would drop the ruby: the batch is refused and no archive appears.
    let out = ziyuan(
        root,
        &["text", "export-finished", &id, "--output", "plain.tar"],
    );
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("整批未导出") && err.contains("--format md"),
        "{err}"
    );
    assert!(!root.join("plain.tar").exists(), "拒绝时不得留下归档");
    ok(
        root,
        &[
            "text",
            "export-finished",
            &id,
            "--format",
            "md",
            "--output",
            "marked.tar",
        ],
    );
    assert!(root.join("marked.tar").exists());

    // 外部改动 zh.json 后 apply 只报不符，不改盘。
    std::fs::write(&zh, b"outside\n").unwrap();
    let out = ziyuan(root, &["text", "human", "apply", &id, &block]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("不符"));
    assert_eq!(std::fs::read(&zh).unwrap(), b"outside\n");
}
