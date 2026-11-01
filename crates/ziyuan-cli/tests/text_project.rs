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
            vec!["p", "--format", "html"],
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
