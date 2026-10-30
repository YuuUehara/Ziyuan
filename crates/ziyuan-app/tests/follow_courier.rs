//! 作品关注 → 兵部驿站：新章节提醒入队为诏令（只入队，不执行）。
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use ziyuan_app::subscriptions::*;
use ziyuan_app::{FollowCourierStatus, Session, follow_courier, follow_station};
use ziyuan_core::{AgentId, Department, GovernanceMode, Office, Seal};
use ziyuan_storage::EventLog;

struct Temp(PathBuf);
impl Temp {
    fn new(name: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ziyuan-follow-courier-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn root(&self) -> &Path {
        &self.0
    }
    fn found(&self) {
        Session::found(EventLog::new(&self.0)).unwrap();
    }
    fn session(&self) -> Session<EventLog> {
        Session::open(EventLog::new(&self.0)).unwrap()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Source(Mutex<Vec<ObservedWork>>);
impl Source {
    fn new(works: Vec<ObservedWork>) -> Self {
        Self(Mutex::new(works))
    }
    fn set(&self, works: Vec<ObservedWork>) {
        *self.0.lock().unwrap() = works;
    }
}
impl WorkSource for Source {
    fn fetch(&self, codes: &[String]) -> Result<Vec<ObservedWork>, SourceError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|w| codes.contains(&w.code))
            .cloned()
            .collect())
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

/// 关注 `codes`，记基线后各增 2 章，产生 NewChapters 提醒。
fn new_chapters(root: &Path, codes: &[&str]) -> Subscriptions {
    let s = Subscriptions::new(root);
    for code in codes {
        s.add(code, 0).unwrap();
    }
    let src = Source::new(codes.iter().map(|c| work(c, 10)).collect());
    s.check(&src, 100, CheckMode::Manual).unwrap();
    src.set(codes.iter().map(|c| work(c, 12)).collect());
    assert_eq!(
        s.check(&src, 200, CheckMode::Manual).unwrap().notices,
        codes.len()
    );
    s
}

fn queue_on(s: &Subscriptions) {
    s.update_settings(|settings| settings.queue_new_chapters = true)
        .unwrap();
}

#[test]
fn new_chapters_queue_one_edict_and_repeats_are_deduplicated() {
    let t = Temp::new("queue");
    t.found();
    let s = new_chapters(t.root(), &["n1234ab"]);
    queue_on(&s);
    let report = follow_courier(t.root());
    assert_eq!(report.status, FollowCourierStatus::Done, "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.queued.len(), 1);
    let queued = &report.queued[0];
    assert_eq!(queued.station, "syosetu:n1234ab#12");
    assert_eq!(follow_station("n1234ab", 12), queued.station);
    let notice = &s.updates(false).unwrap()[0];
    assert_eq!(queued.notice, notice.id);
    assert!(!notice.read, "入队不改提醒的已读状态");

    let session = t.session();
    let tasks: Vec<_> = session.court().tasks().collect();
    assert_eq!(tasks.len(), 1);
    let task = tasks[0];
    assert_eq!(task.edict.id.0, queued.task);
    assert_eq!(task.courier.as_deref(), Some("syosetu:n1234ab#12"));
    assert_eq!(task.edict.department, Department::Works);
    assert!(!task.urgent && !task.approved);
    for expected in [
        "《作品n1234ab》",
        "n1234ab",
        "新增 2 章",
        "共 12 章",
        "作者甲",
        "https://ncode.syosetu.com/n1234ab/",
        "不下载正文",
    ] {
        assert!(task.edict.text.contains(expected), "{}", task.edict.text);
    }

    // 再扫一遍：同一站名不重复入队。
    let again = follow_courier(t.root());
    assert_eq!(again.status, FollowCourierStatus::Done);
    assert!(again.queued.is_empty());
    assert_eq!(again.already_queued, 1);

    // 同一作品同一章数的另一条提醒（如取消关注后重新关注再次观察到）也只算已入队。
    let state = s.state_path();
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    let mut copy = value["data"]["notices"][0].clone();
    copy["id"] = "0123456789abcdef0123456789abcdef".into();
    value["data"]["notices"].as_array_mut().unwrap().push(copy);
    std::fs::write(&state, value.to_string()).unwrap();
    assert_eq!(s.updates(true).unwrap().len(), 2);
    let dup = follow_courier(t.root());
    assert!(dup.queued.is_empty() && dup.errors.is_empty(), "{dup:?}");
    assert_eq!(dup.already_queued, 2);
    assert_eq!(t.session().court().tasks().count(), 1);
}

#[test]
fn duplicate_unread_notices_in_one_scan_queue_once() {
    let t = Temp::new("dup-scan");
    t.found();
    let s = new_chapters(t.root(), &["n1234ab"]);
    queue_on(&s);
    let state = s.state_path();
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    let mut copy = value["data"]["notices"][0].clone();
    copy["id"] = "0123456789abcdef0123456789abcdef".into();
    value["data"]["notices"].as_array_mut().unwrap().push(copy);
    std::fs::write(&state, value.to_string()).unwrap();
    let report = follow_courier(t.root());
    assert_eq!((report.queued.len(), report.already_queued), (1, 1));
    assert_eq!(t.session().court().tasks().count(), 1);
}

#[test]
fn read_and_information_changed_notices_are_not_queued() {
    let t = Temp::new("info");
    t.found();
    let s = Subscriptions::new(t.root());
    s.add("n1234ab", 0).unwrap();
    s.add("n5678cd", 0).unwrap();
    let src = Source::new(vec![work("n1234ab", 10), work("n5678cd", 10)]);
    s.check(&src, 100, CheckMode::Manual).unwrap();
    let mut renamed = work("n1234ab", 10);
    renamed.title = "改名".into();
    src.set(vec![renamed, work("n5678cd", 11)]);
    s.check(&src, 200, CheckMode::Manual).unwrap();
    let notices = s.updates(true).unwrap();
    assert_eq!(notices.len(), 2);
    let read = notices.iter().find(|n| n.code == "n5678cd").unwrap();
    s.read(&read.id).unwrap();
    queue_on(&s);
    let report = follow_courier(t.root());
    assert_eq!(report.status, FollowCourierStatus::Done);
    assert!(report.queued.is_empty() && report.errors.is_empty());
    assert_eq!(report.already_queued, 0);
    assert_eq!(t.session().court().tasks().count(), 0);
}

#[test]
fn disabled_and_not_founded_do_not_open_or_create_a_court() {
    let t = Temp::new("off");
    let s = new_chapters(t.root(), &["n1234ab"]);
    let log = t.root().join(".ziyuan/court.log");
    assert_eq!(
        follow_courier(t.root()).status,
        FollowCourierStatus::Disabled
    );
    assert!(!log.exists());
    queue_on(&s);
    let report = follow_courier(t.root());
    assert_eq!(report.status, FollowCourierStatus::NotFounded);
    assert!(report.queued.is_empty());
    assert!(!log.exists());
    // 关注本身不受影响，建朝后下次扫描补入队。
    assert_eq!(s.updates(true).unwrap().len(), 1);
    t.found();
    assert_eq!(follow_courier(t.root()).queued.len(), 1);
}

#[test]
fn routing_failure_is_reported_per_notice_and_heals_on_next_scan() {
    let t = Temp::new("vacant");
    t.found();
    let s = new_chapters(t.root(), &["n1234ab", "n5678cd"]);
    queue_on(&s);
    let mut session = t.session();
    session.reform(GovernanceMode::Cabinet).unwrap();
    drop(session);
    let report = follow_courier(t.root());
    assert_eq!(report.status, FollowCourierStatus::Done);
    assert!(report.queued.is_empty());
    assert_eq!(report.errors.len(), 2, "{report:?}");
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.station == "syosetu:n5678cd#12")
    );
    assert!(report.errors.iter().all(|e| !e.message.is_empty()));
    assert_eq!(s.updates(true).unwrap().len(), 2);

    let mut session = t.session();
    session.dismiss(AgentId(6)).unwrap();
    session
        .appoint(AgentId(6), Office::FirstGrandSecretary)
        .unwrap();
    drop(session);
    let healed = follow_courier(t.root());
    assert_eq!(healed.queued.len(), 2, "{healed:?}");
    assert!(healed.errors.is_empty());
}

#[test]
fn session_intake_reports_into_existing_session() {
    let t = Temp::new("session");
    t.found();
    let s = new_chapters(t.root(), &["n1234ab"]);
    let mut session = t.session();
    // 会话层的入队不看设置（设置由调用者判断）。
    let report = session.follow_intake(&s).unwrap();
    assert_eq!(report.queued.len(), 1);
    assert_eq!(session.court().tasks().count(), 1);
}

#[test]
fn concurrent_sessions_queue_one_edict_per_station() {
    let t = Temp::new("race");
    t.found();
    let s = new_chapters(t.root(), &["n1234ab"]);
    // 两个会话（如 Web 手动检查与定时检查）都在入队前打开，都看不到该站名的诏令。
    let mut first = t.session();
    let mut second = t.session();
    assert_eq!(second.court().tasks().count(), 0);
    let a = first.follow_intake(&s).unwrap();
    let b = second.follow_intake(&s).unwrap();
    assert_eq!((a.queued.len(), a.already_queued), (1, 0), "{a:?}");
    assert!(b.queued.is_empty() && b.errors.is_empty(), "{b:?}");
    assert_eq!(b.already_queued, 1);
    assert_eq!(t.session().court().tasks().count(), 1);
}

#[test]
fn post_unique_rechecks_station_after_storage_conflict() {
    let t = Temp::new("post-unique");
    t.found();
    let mut first = t.session();
    let mut stale = t.session();
    let station = "syosetu:n1234ab#12";
    let posted = first
        .post_unique(
            Department::Works,
            "甲".into(),
            Seal::default(),
            station,
            false,
        )
        .unwrap();
    assert!(posted.is_some());
    // `stale` 未重载：首次提交基于旧状态，写入冲突后重载，重新检查站名而不再入队。
    assert_eq!(stale.court().tasks().count(), 0);
    let again = stale
        .post_unique(
            Department::Works,
            "乙".into(),
            Seal::default(),
            station,
            false,
        )
        .unwrap();
    assert!(again.is_none());
    assert_eq!(stale.court().tasks().count(), 1);
    assert_eq!(t.session().court().tasks().count(), 1);
}
#[test]
fn old_state_without_queue_setting_loads_as_off() {
    let t = Temp::new("old");
    let state = t.root().join(".ziyuan/subscriptions/state.json");
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    std::fs::write(
        &state,
        r#"{"schema_version":1,"revision":3,"data":{"settings":{"enabled":true,"interval_seconds":900},"items":[],"notices":[]}}"#,
    )
    .unwrap();
    let s = Subscriptions::new(t.root());
    let settings = s.settings().unwrap();
    assert!(settings.enabled && !settings.queue_new_chapters);
    assert_eq!(settings.interval_seconds, 900);
    let updated = s
        .update_settings(|settings| settings.queue_new_chapters = true)
        .unwrap();
    assert!(updated.enabled && updated.queue_new_chapters);
    assert_eq!(s.settings().unwrap(), updated);
    assert!(
        s.update_settings(|settings| settings.interval_seconds = 10)
            .is_err()
    );
    assert_eq!(s.settings().unwrap(), updated);
}
