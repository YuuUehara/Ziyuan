//! `ziyuan follow` 的黑盒测试：不建立朝廷、不读模型配置，也不连接真实的 syosetu API。
//! 需要检查结果时，先用 `ziyuan-app` 的服务和假数据源写好本地状态，再交给命令行读取。

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};
use ziyuan_app::subscriptions::{CheckMode, ObservedWork, SourceError, Subscriptions, WorkSource};

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
            "ziyuan-follow-cli-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).expect("建立唯一临时目录");
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
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

fn json_of(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("不是 JSON：{e}\n{}", String::from_utf8_lossy(&out.stdout)))
}

fn ok_json(dir: &Path, args: &[&str]) -> Value {
    json_of(&ok(dir, args))
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn no_court(dir: &Path) {
    assert!(
        !dir.join(".ziyuan/court.log").exists(),
        "关注命令不应建立朝廷事件簿"
    );
    assert!(!dir.join(".ziyuan/models.json").exists());
}

struct Fake(Result<Vec<ObservedWork>, SourceError>);

impl WorkSource for Fake {
    fn fetch(&self, _: &[String]) -> Result<Vec<ObservedWork>, SourceError> {
        self.0.clone()
    }
}

fn work(code: &str, chapters: u64) -> ObservedWork {
    ObservedWork {
        code: code.into(),
        title: format!("作品{code}"),
        author: "作者甲".into(),
        chapter_count: chapters,
        last_published: 1_700_000_000 + chapters,
        work_updated: 1_700_000_000 + chapters,
    }
}

#[test]
fn add_list_remove_offline() {
    let dir = TempDir::new("lifecycle");
    let root = dir.path();
    let out = ok(
        root,
        &["follow", "add", "https://ncode.syosetu.com/N1234AB/"],
    );
    let text = stdout(&out);
    assert!(
        text.contains("n1234ab") && text.contains("follow check"),
        "{text}"
    );

    let again = ok_json(root, &["follow", "add", "n1234ab", "--json"]);
    assert_eq!(again["added"], false);
    assert_eq!(again["subscription"]["code"], "n1234ab");
    let dup = ok(root, &["follow", "add", "N1234AB"]);
    assert!(stdout(&dup).contains("已在关注列表"), "{}", stdout(&dup));

    let list = ok_json(root, &["--json", "follow", "list"]);
    let items = list["subscriptions"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["code"], "n1234ab");
    assert_eq!(items[0]["url"], "https://ncode.syosetu.com/n1234ab/");
    assert_eq!(items[0]["work"], Value::Null);
    assert_eq!(items[0]["id"], again["subscription"]["id"]);
    assert_eq!(list["unread"], 0);
    let shown = stdout(&ok(root, &["follow", "list"]));
    assert!(
        shown.contains("n1234ab") && shown.contains("尚未检查"),
        "{shown}"
    );

    let removed = ok_json(root, &["follow", "remove", "n1234ab", "--json"]);
    assert_eq!(removed["found"], true);
    assert_eq!(
        ok_json(root, &["follow", "list", "--json"])["subscriptions"],
        serde_json::json!([])
    );
    assert!(stdout(&ok(root, &["follow", "list"])).contains("尚未关注"));

    // 取消未关注的作品：给出明确的 found=false，退出码 1。
    let missing = ziyuan(root, &["follow", "remove", "n1234ab", "--json"]);
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(json_of(&missing)["found"], false);
    let missing = ziyuan(root, &["follow", "remove", "n1234ab"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("未关注"), "{}", stderr(&missing));
    no_court(root);
}

#[test]
fn updates_read_and_errors_are_reported_in_chinese() {
    let dir = TempDir::new("updates");
    let root = dir.path();
    let service = Subscriptions::new(root);
    let t = now() - 1000;
    service.add("n1234ab", t).unwrap();
    service.add("n5678cd", t).unwrap();
    let first = Fake(Ok(vec![work("n1234ab", 10), work("n5678cd", 3)]));
    service.check(&first, t + 100, CheckMode::Manual).unwrap();
    let second = Fake(Ok(vec![work("n1234ab", 12)]));
    let summary = service.check(&second, t + 200, CheckMode::Manual).unwrap();
    assert_eq!((summary.notices, summary.failed), (1, 1));

    let list = ok_json(root, &["follow", "list", "--json"]);
    assert_eq!(list["unread"], 1);
    let broken = list["subscriptions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["code"] == "n5678cd")
        .unwrap()
        .clone();
    assert_eq!(broken["last_error"], "not_found");
    assert!(
        broken["last_error_message"]
            .as_str()
            .unwrap()
            .contains("未找到")
    );
    assert_eq!(broken["work"]["chapter_count"], 3);
    let shown = stdout(&ok(root, &["follow", "list"]));
    assert!(
        shown.contains("未找到作品") && !shown.contains("NotFound"),
        "{shown}"
    );
    assert!(
        shown.contains("作品n1234ab") && shown.contains("12"),
        "{shown}"
    );

    let updates = ok_json(root, &["follow", "updates", "--json"]);
    let notices = updates["notices"].as_array().unwrap();
    assert_eq!(notices.len(), 1);
    let notice = &notices[0];
    assert_eq!(notice["code"], "n1234ab");
    assert_eq!(notice["kind"], "new_chapters");
    assert_eq!(notice["observed_increase"], 2);
    assert_eq!(notice["work"]["chapter_count"], 12);
    assert_eq!(notice["read"], false);
    let id = notice["id"].as_str().unwrap().to_owned();
    let text = stdout(&ok(root, &["follow", "updates"]));
    assert!(text.contains(&id) && text.contains("2 章"), "{text}");

    let read = ok_json(root, &["follow", "read", &id, "--json"]);
    assert_eq!(read["found"], true);
    assert_eq!(
        ok_json(root, &["follow", "updates", "--json"])["notices"],
        serde_json::json!([])
    );
    assert!(stdout(&ok(root, &["follow", "updates"])).contains("没有未读"));
    let all = ok_json(root, &["follow", "updates", "--all", "--json"]);
    assert_eq!(all["notices"][0]["id"], id.as_str());
    assert_eq!(all["notices"][0]["read"], true);
    assert!(stdout(&ok(root, &["follow", "updates", "--all"])).contains(&id));
    // 再读一次已读的提醒仍算找到。
    ok(root, &["follow", "read", &id]);

    let missing = "0123456789abcdef0123456789abcdef";
    let out = ziyuan(root, &["follow", "read", missing, "--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(json_of(&out)["found"], false);
    let out = ziyuan(root, &["follow", "read", missing]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("没有这条提醒"), "{}", stderr(&out));
    no_court(root);
}

#[test]
fn check_without_network_reports_empty_and_cooldown() {
    let dir = TempDir::new("check");
    let root = dir.path();
    let out = ok(root, &["follow", "check"]);
    assert!(stdout(&out).contains("关注列表为空"), "{}", stdout(&out));
    let report = ok_json(root, &["follow", "check", "--json"]);
    assert_eq!(report["status"], "empty");
    assert_eq!(report["checked"], 0);

    // 刚检查过的作品处于 60 秒冷却中：命令行不会联网。
    let service = Subscriptions::new(root);
    service.add("n1234ab", now()).unwrap();
    service
        .check(
            &Fake(Ok(vec![work("n1234ab", 5)])),
            now(),
            CheckMode::Manual,
        )
        .unwrap();
    let report = ok_json(root, &["follow", "check", "--json"]);
    assert_eq!(report["status"], "not_due");
    assert_eq!(report["skipped"], 1);
    assert_eq!(report["cooldown"], serde_json::json!(["n1234ab"]));
    assert_eq!(report["failed"], 0);
    let out = ok(root, &["follow", "check"]);
    let text = stdout(&out);
    assert!(text.contains("冷却") && text.contains("60 秒"), "{text}");
    assert!(!text.contains("无变化"), "{text}");
    no_court(root);
}

#[test]
fn usage_errors_exit_2() {
    let dir = TempDir::new("usage");
    let root = dir.path();
    let id = "0123456789abcdef0123456789abcdef";
    let cases: &[&[&str]] = &[
        &["follow", "frobnicate"],
        &["follow", "add"],
        &["follow", "add", "n123ab"],
        &["follow", "add", "https://ncode.syosetu.com/n1234ab/1/"],
        &["follow", "add", "https://evil.test/n1234ab/"],
        &["follow", "add", "n1234ab", "n5678cd"],
        &["follow", "add", "--all"],
        &["follow", "list", "extra"],
        &["follow", "remove"],
        &["follow", "remove", "bad"],
        &["follow", "check", "--force"],
        &["follow", "check", "https://example.invalid/api"],
        &["follow", "updates", "--all", "--all"],
        &["follow", "updates", "--unread"],
        &["follow", "read"],
        &["follow", "read", "xyz"],
        &["follow", "read", id, id],
        &["--json", "follow", "add"],
    ];
    for args in cases {
        let out = ziyuan(root, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    assert!(!root.join(".ziyuan").exists());
}

#[test]
fn corrupt_or_newer_state_is_a_runtime_error() {
    let dir = TempDir::new("corrupt");
    let root = dir.path();
    let state = root.join(".ziyuan/subscriptions/state.json");
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    for body in [
        "{".to_owned(),
        r#"{"schema_version":99,"revision":1,"data":{}}"#.to_owned(),
    ] {
        std::fs::write(&state, &body).unwrap();
        for args in [
            &["follow", "list"][..],
            &["follow", "list", "--json"],
            &["follow", "add", "n1234ab"],
            &["follow", "remove", "n1234ab"],
            &["follow", "updates"],
            &["follow", "read", "0123456789abcdef0123456789abcdef"],
            &["follow", "check", "--json"],
        ] {
            let out = ziyuan(root, args);
            assert_eq!(out.status.code(), Some(1), "{body} {args:?}");
            assert!(out.stdout.is_empty(), "{body} {args:?}");
            assert!(stderr(&out).contains("状态文件"), "{}", stderr(&out));
        }
        assert_eq!(std::fs::read_to_string(&state).unwrap(), body);
    }
    no_court(root);
}

#[test]
fn help_is_plain_and_offline() {
    let dir = TempDir::new("help");
    let root = dir.path();
    for args in [
        &["follow"][..],
        &["follow", "help"],
        &["follow", "--help"],
        &["follow", "add", "--help"],
        &["--json", "follow", "help"],
    ] {
        let out = ok(root, args);
        let text = stdout(&out);
        assert!(
            text.contains("ziyuan follow add") && text.contains("--all"),
            "{args:?}: {text}"
        );
    }
    let help = stdout(&ok(root, &["help"]));
    assert!(help.contains("ziyuan follow add"), "{help}");
    let bash = stdout(&ok(root, &["completions", "bash"]));
    assert!(bash.contains("follow") && bash.contains("updates"));
    let fish = stdout(&ok(root, &["completions", "fish"]));
    assert!(fish.contains("follow"));
    assert!(!root.join(".ziyuan").exists());
}

#[test]
fn queue_setting_is_offline_and_check_reports_courier_intake() {
    let dir = TempDir::new("queue");
    let root = dir.path();
    let service = Subscriptions::new(root);
    service
        .set_settings(ziyuan_app::subscriptions::Settings {
            enabled: true,
            interval_seconds: 900,
            queue_new_chapters: false,
        })
        .unwrap();
    let shown = ok_json(root, &["follow", "queue", "--json"]);
    assert_eq!(shown["queue_new_chapters"], false);
    assert!(stdout(&ok(root, &["follow", "queue"])).contains("关闭"));

    // 未开启：照常检查，不打开朝廷。
    let t = now() - 1000;
    service.add("n1234ab", t).unwrap();
    service
        .check(&Fake(Ok(vec![work("n1234ab", 10)])), t, CheckMode::Manual)
        .unwrap();
    service
        .check(
            &Fake(Ok(vec![work("n1234ab", 12)])),
            now(),
            CheckMode::Manual,
        )
        .unwrap();
    let report = ok_json(root, &["follow", "check", "--json"]);
    assert_eq!(report["status"], "not_due");
    assert_eq!(report["courier"]["status"], "disabled");
    assert!(!stdout(&ok(root, &["follow", "check"])).contains("驿站"));
    no_court(root);

    let on = ok_json(root, &["follow", "queue", "on", "--json"]);
    assert_eq!(on["queue_new_chapters"], true);
    let settings = service.settings().unwrap();
    assert!(settings.queue_new_chapters && settings.enabled);
    assert_eq!(settings.interval_seconds, 900);
    assert!(stdout(&ok(root, &["follow", "queue"])).contains("开启"));

    // 朝廷尚未建立：检查仍成功，提醒保留，不建朝。
    let report = ok_json(root, &["follow", "check", "--json"]);
    assert_eq!(report["courier"]["status"], "not_founded");
    assert_eq!(report["courier"]["queued"], serde_json::json!([]));
    let text = stdout(&ok(root, &["follow", "check"]));
    assert!(
        text.contains("朝廷尚未建立") && text.contains("ziyuan init"),
        "{text}"
    );
    no_court(root);

    ok(root, &["init"]);
    let report = ok_json(root, &["follow", "check", "--json"]);
    assert_eq!(report["status"], "not_due");
    let courier = &report["courier"];
    assert_eq!(courier["status"], "done", "{courier}");
    assert_eq!(courier["queued"][0]["station"], "syosetu:n1234ab#12");
    assert!(courier["queued"][0]["task"].as_u64().is_some());
    assert_eq!(
        courier["queued"][0]["notice"],
        service.updates(true).unwrap()[0].id.as_str()
    );
    assert_eq!(courier["already_queued"], 0);
    assert_eq!(courier["errors"], serde_json::json!([]));
    let task = courier["queued"][0]["task"].as_u64().unwrap();
    let text = stdout(&ok(root, &["follow", "check"]));
    assert!(text.contains("已入队过 1 件"), "{text}");
    let report = ok_json(root, &["follow", "check", "--json"]);
    assert_eq!(report["courier"]["already_queued"], 1);
    assert_eq!(report["courier"]["queued"], serde_json::json!([]));
    let tasks = ok_json(root, &["tasks", "--json"]);
    assert!(
        tasks.to_string().contains("syosetu:n1234ab#12"),
        "{tasks} #{task}"
    );
    // 入队不把提醒标为已读。
    assert_eq!(service.updates(true).unwrap().len(), 1);

    let off = ok_json(root, &["follow", "queue", "off", "--json"]);
    assert_eq!(off["queue_new_chapters"], false);
    assert!(service.settings().unwrap().enabled);
}

#[test]
fn queue_usage_errors_exit_2() {
    let dir = TempDir::new("queue-usage");
    let root = dir.path();
    for args in [
        &["follow", "queue", "maybe"][..],
        &["follow", "queue", "on", "off"],
        &["follow", "queue", "--all"],
    ] {
        let out = ziyuan(root, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    assert!(!root.join(".ziyuan").exists());
    let help = stdout(&ok(root, &["follow", "help"]));
    assert!(help.contains("ziyuan follow queue"), "{help}");
}
