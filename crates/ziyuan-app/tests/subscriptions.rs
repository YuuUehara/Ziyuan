use std::sync::{Arc, Barrier, Mutex};
use ziyuan_app::subscriptions::*;

struct Temp(std::path::PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ziyuan-subscriptions-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn service(&self) -> Subscriptions {
        Subscriptions::new(&self.0)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Source(Mutex<Result<Vec<ObservedWork>, SourceError>>);
impl Source {
    fn new() -> Self {
        Self(Mutex::new(Ok(vec![work()])))
    }
    fn set(&self, value: Result<Vec<ObservedWork>, SourceError>) {
        *self.0.lock().unwrap() = value;
    }
}
impl WorkSource for Source {
    fn fetch(&self, _: &[String]) -> Result<Vec<ObservedWork>, SourceError> {
        self.0.lock().unwrap().clone()
    }
}
fn work() -> ObservedWork {
    ObservedWork {
        code: "n1234ab".into(),
        title: "title".into(),
        author: "author".into(),
        chapter_count: 10,
        last_published: 1_700_000_000,
        work_updated: 1_700_000_000,
    }
}
fn check(s: &Subscriptions, source: &impl WorkSource, now: u64) -> CheckSummary {
    s.check(source, now, CheckMode::Manual).unwrap()
}

#[test]
fn normalize_and_offline_lifecycle() {
    for input in [
        "N1234AB",
        "https://ncode.syosetu.com/N1234AB/",
        "n1234abcdef",
    ] {
        assert!(normalize_code(input).is_ok(), "{input}");
    }
    for input in [
        "n123ab",
        "n12345ab",
        "n1234",
        "n1234abcdefg",
        "https://evil.test/n1234ab/",
        "http://ncode.syosetu.com/n1234ab/",
        "https://a@ncode.syosetu.com/n1234ab/",
        "https://ncode.syosetu.com/n1234ab/?a",
        "https://ncode.syosetu.com/n1234ab/#a",
        "https://ncode.syosetu.com/n1234ab/2/",
        "https://ncode.syosetu.com:443/n1234ab/",
    ] {
        assert!(normalize_code(input).is_err(), "{input}");
    }
    let t = Temp::new();
    let s = t.service();
    assert_eq!(s.settings().unwrap(), Settings::default());
    let a = s.add("N1234AB", 0).unwrap();
    assert_eq!(s.add("n1234ab", 10).unwrap().id, a.id);
    assert_eq!(s.list().unwrap().len(), 1);
    assert!(s.remove("n1234ab").unwrap());
    assert!(!s.remove("n1234ab").unwrap());
    assert_ne!(s.add("n1234ab", 20).unwrap().id, a.id);
    assert_eq!(t.service().list().unwrap().len(), 1);
}

#[test]
fn baseline_changes_unread_and_restart() {
    let t = Temp::new();
    let s = t.service();
    let src = Source::new();
    s.add("n1234ab", 0).unwrap();
    assert_eq!(check(&s, &src, 100).checked, 1);
    assert!(s.updates(false).unwrap().is_empty());
    let mut w = work();
    w.chapter_count = 12;
    w.last_published += 100;
    w.work_updated += 100;
    src.set(Ok(vec![w.clone()]));
    assert_eq!(check(&s, &src, 160).notices, 1);
    let notices = s.updates(true).unwrap();
    assert_eq!(
        notices[0].kind,
        NoticeKind::NewChapters {
            observed_increase: 2
        }
    );
    let id = notices[0].id.clone();
    assert_eq!(s.add("n1234ab", 200).unwrap().id, s.list().unwrap()[0].id);
    assert_eq!(check(&s, &src, 220).notices, 0);
    assert_eq!(t.service().updates(true).unwrap()[0].id, id);
    w.title = "new title".into();
    src.set(Ok(vec![w]));
    assert_eq!(check(&s, &src, 280).notices, 1);
    assert_eq!(s.updates(true).unwrap().len(), 2);
    assert!(s.read(&id).unwrap());
    assert!(!s.read("missing").unwrap());
    assert_eq!(s.updates(true).unwrap().len(), 1);
    assert_eq!(s.updates(false).unwrap().len(), 2);
}

#[test]
fn suspicious_observations_and_errors_preserve_good_baseline_and_unread() {
    let t = Temp::new();
    let s = t.service();
    let src = Source::new();
    s.add("n1234ab", 0).unwrap();
    check(&s, &src, 100);
    let mut w = work();
    w.chapter_count += 1;
    src.set(Ok(vec![w.clone()]));
    check(&s, &src, 160);
    for (i, anomaly) in [
        ObservedWork {
            chapter_count: 9,
            ..w.clone()
        },
        ObservedWork {
            last_published: 1,
            ..w.clone()
        },
        ObservedWork {
            work_updated: 1,
            ..w.clone()
        },
    ]
    .into_iter()
    .enumerate()
    {
        src.set(Ok(vec![anomaly]));
        assert_eq!(check(&s, &src, 220 + i as u64 * 60).failed, 1);
        assert_eq!(s.list().unwrap()[0].baseline.as_ref().unwrap(), &w);
    }
    for (i, error) in [
        SourceError::NotFound,
        SourceError::RateLimited,
        SourceError::Timeout,
        SourceError::Malformed,
    ]
    .into_iter()
    .enumerate()
    {
        src.set(Err(error.clone()));
        assert_eq!(check(&s, &src, 400 + i as u64 * 60).failed, 1);
        assert_eq!(s.list().unwrap()[0].last_error, Some(error));
    }
    assert_eq!(s.updates(true).unwrap().len(), 1);
}

#[test]
fn scheduler_cooldown_backoff_partial_and_settings() {
    let t = Temp::new();
    let s = t.service();
    let src = Source::new();
    s.add("n1234ab", 0).unwrap();
    s.add("n9999a", 0).unwrap();
    assert_eq!(
        s.check(&src, 100, CheckMode::Scheduled).unwrap().status,
        CheckStatus::Disabled
    );
    assert!(
        s.set_settings(Settings {
            enabled: true,
            interval_seconds: 899,
            queue_new_chapters: false,
        })
        .is_err()
    );
    s.set_settings(Settings {
        enabled: true,
        interval_seconds: 900,
        queue_new_chapters: false,
    })
    .unwrap();
    let result = check(&s, &src, 100);
    assert_eq!((result.checked, result.failed), (2, 1));
    assert_eq!(check(&s, &src, 159).skipped, 2);
    assert_eq!(s.check(&src, 999, CheckMode::Scheduled).unwrap().checked, 0);
    assert_eq!(
        s.check(&src, 1000, CheckMode::Scheduled).unwrap().checked,
        1
    );
    let missing = s
        .list()
        .unwrap()
        .into_iter()
        .find(|i| i.code == "n9999a")
        .unwrap();
    assert_eq!(missing.next_check, 1900);
    src.set(Err(SourceError::Timeout));
    check(&s, &src, 2000);
    let missing = s
        .list()
        .unwrap()
        .into_iter()
        .find(|i| i.code == "n9999a")
        .unwrap();
    assert_eq!(missing.next_check, 5600);
    check(&s, &src, 6000);
    let missing = s
        .list()
        .unwrap()
        .into_iter()
        .find(|i| i.code == "n9999a")
        .unwrap();
    assert_eq!(missing.next_check, 13200);
}

#[test]
fn network_does_not_lock_state_competing_claim_and_remove_readd_discards_response() {
    struct Blocking {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    }
    impl WorkSource for Blocking {
        fn fetch(&self, _: &[String]) -> Result<Vec<ObservedWork>, SourceError> {
            self.entered.wait();
            self.release.wait();
            Ok(vec![work()])
        }
    }
    let t = Temp::new();
    let s = t.service();
    s.add("n1234ab", 0).unwrap();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let other = t.service();
    let source = Blocking {
        entered: entered.clone(),
        release: release.clone(),
    };
    let handle = std::thread::spawn(move || check(&other, &source, 100));
    entered.wait();
    assert_eq!(check(&s, &Source::new(), 160).checked, 0);
    assert_eq!(s.list().unwrap().len(), 1);
    s.remove("n1234ab").unwrap();
    s.add("n1234ab", 160).unwrap();
    release.wait();
    assert_eq!(handle.join().unwrap().discarded, 1);
    assert!(s.list().unwrap()[0].baseline.is_none());
}

#[test]
fn expired_lease_can_be_reclaimed_and_old_response_is_discarded() {
    struct Reentrant<'a>(&'a Subscriptions);
    impl WorkSource for Reentrant<'_> {
        fn fetch(&self, _: &[String]) -> Result<Vec<ObservedWork>, SourceError> {
            assert_eq!(check(self.0, &Source::new(), 191).checked, 1);
            Ok(vec![ObservedWork {
                title: "stale".into(),
                ..work()
            }])
        }
    }
    let t = Temp::new();
    let s = t.service();
    s.add("n1234ab", 0).unwrap();
    assert_eq!(check(&s, &Reentrant(&s), 100).discarded, 1);
    assert_eq!(
        s.list().unwrap()[0].baseline.as_ref().unwrap().title,
        "title"
    );
}

#[test]
fn batch_is_at_most_fifty_and_save_limit_preserves_old_state() {
    let t = Temp::new();
    let s = t.service();
    for n in 0..51 {
        s.add(&format!("n{n:04}a"), 0).unwrap();
    }
    struct Size;
    impl WorkSource for Size {
        fn fetch(&self, codes: &[String]) -> Result<Vec<ObservedWork>, SourceError> {
            assert_eq!(codes.len(), 50);
            Ok(vec![])
        }
    }
    assert_eq!(check(&s, &Size, 100).checked, 50);
    let before = std::fs::read(s.state_path()).unwrap();
    let bounded = Subscriptions::with_max_bytes(&t.0, before.len() as u64);
    assert!(bounded.add("n9999z", 200).is_err());
    assert_eq!(std::fs::read(s.state_path()).unwrap(), before);
}

#[test]
fn idle_checks_do_not_fetch_or_rewrite_state() {
    struct Never;
    impl WorkSource for Never {
        fn fetch(&self, _: &[String]) -> Result<Vec<ObservedWork>, SourceError> {
            panic!("idle check fetched");
        }
    }
    let t = Temp::new();
    let s = t.service();
    assert_eq!(
        s.check(&Never, 100, CheckMode::Scheduled).unwrap().status,
        CheckStatus::Disabled
    );
    assert!(!s.state_path().exists());
    assert_eq!(check(&s, &Never, 100).status, CheckStatus::Empty);
    s.add("n1234ab", 0).unwrap();
    check(&s, &Source::new(), 100);
    let before = std::fs::read(s.state_path()).unwrap();
    assert_eq!(check(&s, &Never, 120).status, CheckStatus::NotDue);
    assert_eq!(std::fs::read(s.state_path()).unwrap(), before);
}

#[test]
fn one_malformed_work_does_not_stop_valid_results_and_failed_save_keeps_baseline() {
    let t = Temp::new();
    let s = t.service();
    s.add("n1234ab", 0).unwrap();
    s.add("n9999a", 0).unwrap();
    let src = Source::new();
    src.set(Ok(vec![
        work(),
        ObservedWork {
            code: "n9999a".into(),
            chapter_count: 0,
            ..work()
        },
    ]));
    let summary = check(&s, &src, 100);
    assert_eq!((summary.checked, summary.failed), (2, 1));
    assert!(s.list().unwrap()[0].baseline.is_some());
    let before = std::fs::read(s.state_path()).unwrap();
    let bounded = Subscriptions::with_max_bytes(&t.0, before.len() as u64 + 512);
    src.set(Ok(vec![ObservedWork {
        title: "x".repeat(5000),
        ..work()
    }]));
    assert!(bounded.check(&src, 160, CheckMode::Manual).is_err());
    assert_eq!(s.list().unwrap()[0].baseline, Some(work()));
    assert!(s.updates(false).unwrap().is_empty());
}
