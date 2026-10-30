use super::*;
use crate::tests::{TOKEN, VIEWER, call};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use ziyuan_app::subscriptions::{ObservedWork, Settings};

#[derive(Default)]
struct Fixture {
    calls: AtomicUsize,
    called: tokio::sync::Notify,
    chapters: AtomicU64,
    error: Mutex<Option<SourceError>>,
}
impl WorkSource for Fixture {
    fn fetch(&self, codes: &[String]) -> Result<Vec<ObservedWork>, SourceError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.called.notify_one();
        if let Some(error) = self.error.lock().unwrap().clone() {
            return Err(error);
        }
        Ok(codes
            .iter()
            .map(|code| ObservedWork {
                code: code.clone(),
                title: "<img src=x onerror=alert(1)>".into(),
                author: "\"<script>作者</script>".into(),
                chapter_count: self.chapters.load(Ordering::SeqCst),
                last_published: 1,
                work_updated: 1,
            })
            .collect())
    }
}
fn fixture(name: &str) -> (Service, Arc<Fixture>, Arc<AtomicU64>) {
    let root = std::env::temp_dir().join(format!("ziyuan-web-sub-{name}-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    let source = Arc::new(Fixture::default());
    source.chapters.store(1, Ordering::SeqCst);
    let clock = Arc::new(AtomicU64::new(1000));
    let c = clock.clone();
    (
        Service {
            root,
            source: source.clone(),
            clock: Arc::new(move || c.load(Ordering::SeqCst)),
        },
        source,
        clock,
    )
}

#[tokio::test(start_paused = true)]
async fn scheduler_ticks_without_browser_and_stops_after_drop() {
    let (service, source, clock) = fixture("timer-tick");
    let s = Subscriptions::new(&service.root);
    s.add("n1234ab", 1000).unwrap();
    s.set_settings(Settings {
        enabled: true,
        interval_seconds: 900,
        queue_new_chapters: false,
    })
    .unwrap();
    let scheduler = crate::timer::Scheduler::start(crate::app_state(config(&service), service));
    tokio::task::yield_now().await;
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    source.called.notified().await;
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    drop(scheduler);
    tokio::task::yield_now().await;
    clock.store(5000, Ordering::SeqCst);
    tokio::time::advance(std::time::Duration::from_secs(3600)).await;
    tokio::task::yield_now().await;
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn embedded_router_does_not_start_scheduler_even_if_enabled() {
    let (service, source, _) = fixture("router-no-timer");
    let s = Subscriptions::new(&service.root);
    s.add("n1234ab", 1000).unwrap();
    s.set_settings(Settings {
        enabled: true,
        interval_seconds: 900,
        queue_new_chapters: false,
    })
    .unwrap();
    let _router = crate::router_with_subscriptions(config(&service), service);
    tokio::time::advance(std::time::Duration::from_secs(3600)).await;
    tokio::task::yield_now().await;
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
}
fn config(service: &Service) -> crate::Config {
    crate::Config {
        root: service.root.clone(),
        token: TOKEN.into(),
        viewer_token: Some(VIEWER.into()),
        backend: Arc::new(ziyuan_runtime::DemoBackend),
        limits: ziyuan_runtime::Limits {
            max_concurrent: 2,
            timeout: std::time::Duration::from_secs(10),
        },
        github: None,
    }
}
async fn start(service: Service) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = crate::router_with_subscriptions(config(&service), service);
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (base, task)
}
#[tokio::test(flavor = "multi_thread")]
async fn subscription_workflow_baseline_unread_errors_and_no_secret_fields() {
    let (service, source, clock) = fixture("workflow");
    let (base, server) = start(service.clone()).await;
    let t = Some(TOKEN);
    for path in ["/subscriptions", "/subscriptions/updates"] {
        assert_eq!(call(&base, path, None, None).await.0, 401);
        assert_eq!(call(&base, path, Some(VIEWER), None).await.0, 200);
    }
    for path in [
        "/subscriptions",
        "/subscriptions/remove",
        "/subscriptions/check",
        "/subscriptions/read",
        "/subscriptions/settings",
    ] {
        assert_eq!(call(&base, path, None, Some(json!({}))).await.0, 401);
        assert_eq!(
            call(&base, path, Some(VIEWER), Some(json!({}))).await.0,
            403
        );
    }
    assert_eq!(
        call(&base, "/subscriptions", Some(VIEWER), None).await.0,
        200
    );
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    assert!(!service.root.exists());
    let (_, defaults) = call(&base, "/subscriptions", t, None).await;
    assert_eq!(
        defaults["settings"],
        json!({"enabled":false,"interval_seconds":1800,"queue_new_chapters":false})
    );
    call(&base, "/subscriptions", t, Some(json!({"input":"n1234ab"}))).await;
    let (status, baseline) = call(&base, "/subscriptions/check", t, Some(json!({}))).await;
    assert_eq!(status, 200);
    assert_eq!(baseline["courier"]["status"], "disabled");
    assert_eq!(baseline["notices"], 0);
    assert_eq!(baseline["checked"], 1);
    source.chapters.store(2, Ordering::SeqCst);
    clock.store(1060, Ordering::SeqCst);
    let (_, changed) = call(&base, "/subscriptions/check", t, Some(json!({}))).await;
    assert_eq!(changed["notices"], 1);
    let (_, updates) = call(&base, "/subscriptions/updates", t, None).await;
    let notice = &updates["updates"][0];
    assert_eq!(
        notice["observation"]["title"],
        "<img src=x onerror=alert(1)>"
    );
    assert!(notice.get("url").is_none());
    let id = notice["id"].as_str().unwrap();
    assert_eq!(
        call(&base, "/subscriptions/read", t, Some(json!({"id":id})))
            .await
            .1["found"],
        true
    );
    assert_eq!(
        call(&base, "/subscriptions/updates", t, None).await.1["updates"],
        json!([])
    );
    assert_eq!(
        call(&base, "/subscriptions/updates?all=true", t, None)
            .await
            .1["updates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    *source.error.lock().unwrap() = Some(SourceError::Unavailable);
    clock.store(1120, Ordering::SeqCst);
    let (_, failed) = call(&base, "/subscriptions/check", t, Some(json!({}))).await;
    assert_eq!(failed["failed"], 1);
    let (_, list) = call(&base, "/subscriptions", t, None).await;
    assert_eq!(list["items"][0]["chapter_count"], 2);
    assert!(
        list["items"][0]["last_error"]
            .as_str()
            .unwrap()
            .contains("无法连接")
    );
    let serialized = list.to_string();
    for private in ["lease", "token", "baseline_revision", "web-token"] {
        assert!(!serialized.contains(private));
    }
    server.abort();
}
#[tokio::test]
async fn scheduled_poll_respects_off_due_backoff_and_disable() {
    let (service, source, clock) = fixture("schedule");
    let s = Subscriptions::new(&service.root);
    s.add("n1234ab", 1000).unwrap();
    assert_eq!(
        service.check(CheckMode::Scheduled).await.unwrap().status,
        ziyuan_app::subscriptions::CheckStatus::Disabled
    );
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    s.set_settings(Settings {
        enabled: true,
        interval_seconds: 900,
        queue_new_chapters: false,
    })
    .unwrap();
    service.check(CheckMode::Scheduled).await.unwrap();
    clock.store(1899, Ordering::SeqCst);
    service.check(CheckMode::Scheduled).await.unwrap();
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    clock.store(1900, Ordering::SeqCst);
    *source.error.lock().unwrap() = Some(SourceError::Timeout);
    assert_eq!(service.check(CheckMode::Scheduled).await.unwrap().failed, 1);
    assert_eq!(s.list().unwrap()[0].next_check, 3700);
    clock.store(3699, Ordering::SeqCst);
    service.check(CheckMode::Scheduled).await.unwrap();
    assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    s.set_settings(Settings {
        enabled: false,
        interval_seconds: 900,
        queue_new_chapters: false,
    })
    .unwrap();
    clock.store(5000, Ordering::SeqCst);
    service.check(CheckMode::Scheduled).await.unwrap();
    assert_eq!(source.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn slow_subscription_source_does_not_block_runtime_or_session() {
    struct Slow {
        entered: tokio::sync::Notify,
        release: std::sync::Barrier,
    }
    impl WorkSource for Slow {
        fn fetch(&self, _: &[String]) -> Result<Vec<ObservedWork>, SourceError> {
            self.entered.notify_one();
            self.release.wait();
            Err(SourceError::Timeout)
        }
    }
    let (mut service, _, _) = fixture("slow");
    let slow = Arc::new(Slow {
        entered: tokio::sync::Notify::new(),
        release: std::sync::Barrier::new(2),
    });
    service.source = slow.clone();
    ziyuan_app::Session::found(ziyuan_storage::EventLog::new(&service.root)).unwrap();
    Subscriptions::new(&service.root)
        .add("n1234ab", 1000)
        .unwrap();
    let (base, server) = start(service).await;
    let b = base.clone();
    let checking = tokio::spawn(async move {
        call(&b, "/subscriptions/check", Some(TOKEN), Some(json!({}))).await
    });
    slow.entered.notified().await;
    let responsive = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        assert_eq!(call(&base, "/state", Some(TOKEN), None).await.0, 200);
        assert_eq!(
            call(&base, "/subscriptions", Some(TOKEN), None).await.0,
            200
        );
    })
    .await;
    // Always release even when the responsiveness assertion fails.
    tokio::task::spawn_blocking(move || slow.release.wait())
        .await
        .unwrap();
    responsive.unwrap();
    assert_eq!(checking.await.unwrap().1["failed"], 1);
    server.abort();
}
#[tokio::test]
async fn dropping_serve_owns_and_stops_scheduler() {
    let (service, source, _) = fixture("serve-drop");
    let weak = Arc::downgrade(&source);
    drop(source);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = config(&service);
    let task = tokio::spawn(crate::serve_with_subscriptions(listener, config, service));
    tokio::task::yield_now().await;
    task.abort();
    let _ = task.await;
    tokio::task::yield_now().await;
    assert!(
        weak.upgrade().is_none(),
        "serve cancellation must not leave the source owned by a timer"
    );
}

fn queue_on(service: &Service) {
    Subscriptions::new(&service.root)
        .update_settings(|s| s.queue_new_chapters = true)
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_round_trip_queue_flag_and_manual_check_reports_courier() {
    let (service, source, clock) = fixture("courier-manual");
    ziyuan_app::Session::found(ziyuan_storage::EventLog::new(&service.root)).unwrap();
    let (base, server) = start(service.clone()).await;
    let t = Some(TOKEN);
    let body = json!({"enabled":false,"interval_seconds":1800,"queue_new_chapters":true});
    assert_eq!(
        call(
            &base,
            "/subscriptions/settings",
            Some(VIEWER),
            Some(body.clone())
        )
        .await
        .0,
        403
    );
    assert!(
        !Subscriptions::new(&service.root)
            .settings()
            .unwrap()
            .queue_new_chapters
    );
    for bad in [
        json!({"enabled":false,"interval_seconds":1800,"queue_new_chapters":"yes"}),
        json!({"queue_new_chapters":true}),
    ] {
        assert_eq!(
            call(&base, "/subscriptions/settings", t, Some(bad)).await.0,
            400
        );
    }
    let (status, saved) = call(&base, "/subscriptions/settings", t, Some(body)).await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["settings"]["queue_new_chapters"], true);
    // 不带此字段（旧页面）时保留原值。
    let (_, kept) = call(
        &base,
        "/subscriptions/settings",
        t,
        Some(json!({"enabled":true,"interval_seconds":900})),
    )
    .await;
    assert_eq!(
        kept["settings"],
        json!({"enabled":true,"interval_seconds":900,"queue_new_chapters":true})
    );
    let (_, list) = call(&base, "/subscriptions", Some(VIEWER), None).await;
    assert_eq!(list["settings"]["queue_new_chapters"], true);

    call(&base, "/subscriptions", t, Some(json!({"input":"n1234ab"}))).await;
    let (_, baseline) = call(&base, "/subscriptions/check", t, Some(json!({}))).await;
    assert_eq!(baseline["courier"]["status"], "done", "{baseline}");
    assert_eq!(baseline["courier"]["queued"], json!([]));
    source.chapters.store(3, Ordering::SeqCst);
    clock.store(1060, Ordering::SeqCst);
    let (status, changed) = call(&base, "/subscriptions/check", t, Some(json!({}))).await;
    assert_eq!(status, 200);
    assert_eq!(changed["notices"], 1);
    let courier = &changed["courier"];
    assert_eq!(courier["status"], "done");
    assert_eq!(courier["queued"][0]["station"], "syosetu:n1234ab#3");
    let task = courier["queued"][0]["task"].as_u64().unwrap();
    let (_, state) = call(&base, "/state", t, None).await;
    assert!(state.to_string().contains("syosetu:n1234ab#3"), "#{task}");
    clock.store(1120, Ordering::SeqCst);
    let (_, again) = call(&base, "/subscriptions/check", t, Some(json!({}))).await;
    assert_eq!(again["courier"]["already_queued"], 1);
    assert_eq!(again["courier"]["queued"], json!([]));
    let (_, updates) = call(&base, "/subscriptions/updates", t, None).await;
    assert_eq!(updates["updates"][0]["read"], false);
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn not_founded_court_keeps_check_successful() {
    let (service, source, clock) = fixture("courier-unfounded");
    let s = Subscriptions::new(&service.root);
    s.add("n1234ab", 1000).unwrap();
    queue_on(&service);
    let (base, server) = start(service.clone()).await;
    let t = Some(TOKEN);
    call(&base, "/subscriptions/check", t, Some(json!({}))).await;
    source.chapters.store(2, Ordering::SeqCst);
    clock.store(1060, Ordering::SeqCst);
    let (status, checked) = call(&base, "/subscriptions/check", t, Some(json!({}))).await;
    assert_eq!(status, 200, "{checked}");
    assert_eq!(checked["notices"], 1);
    assert_eq!(checked["courier"]["status"], "not_founded");
    assert!(!service.root.join(".ziyuan/court.log").exists());
    assert_eq!(s.updates(true).unwrap().len(), 1);
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduled_tick_queues_through_cached_session_and_emits_change() {
    let (service, source, clock) = fixture("courier-timer");
    ziyuan_app::Session::found(ziyuan_storage::EventLog::new(&service.root)).unwrap();
    let s = Subscriptions::new(&service.root);
    s.add("n1234ab", 1000).unwrap();
    s.update_settings(|s| {
        s.enabled = true;
        s.interval_seconds = 900;
        s.queue_new_chapters = true;
    })
    .unwrap();
    let app = crate::app_state(config(&service), service.clone());
    let mut events = app.0.events.subscribe();
    let (summary, courier) = scheduled_tick(&app).await.unwrap();
    assert_eq!(summary.checked, 1);
    assert!(courier.queued.is_empty());
    assert!(
        events.try_recv().is_err(),
        "nothing queued, nothing to refresh"
    );
    // 未到期：不联网，也不打开朝廷。
    let (summary, courier) = scheduled_tick(&app).await.unwrap();
    assert_eq!(summary.status, CheckStatus::NotDue);
    assert_eq!(courier.status, ziyuan_app::FollowCourierStatus::Disabled);
    source.chapters.store(2, Ordering::SeqCst);
    clock.store(1900, Ordering::SeqCst);
    let (summary, courier) = scheduled_tick(&app).await.unwrap();
    assert_eq!(summary.notices, 1);
    assert_eq!(courier.queued.len(), 1, "{courier:?}");
    assert_eq!(courier.queued[0].station, "syosetu:n1234ab#2");
    assert_eq!(events.try_recv().unwrap().0, "changed");
    let cached = app.0.cache.lock().unwrap();
    let session = cached.as_ref().expect("会话放回缓存");
    assert!(
        session
            .court()
            .tasks()
            .any(|t| t.courier.as_deref() == Some("syosetu:n1234ab#2"))
    );
}
