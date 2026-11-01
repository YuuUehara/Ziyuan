//! Real loopback HTTP execution: selected references survive persistence and are
//! checked again after scheduler queueing, without widening the writer's tools.
use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use ziyuan_app::{
    Session,
    text_project::{NewProject, Project, Provenance, TextProjects},
    translation::{ReferencePreview, ReferenceRequest, ReferenceSelection, ReferenceSubmission},
};
use ziyuan_core::{Capability, CourtEvent, Department, FileChange, Seal, TaskId, TranslationKind};
use ziyuan_governance::{Court, TaskStatus};
use ziyuan_llm::{OpenAiCompatible, Retry};
use ziyuan_runtime::{AgentBackend, CancelToken, Limits, Scheduler};
use ziyuan_storage::{EventLog, EventStore};

const B1: &str = "c0001-b0001";
const B3: &str = "c0003-b0001";
const SOURCES: [&str; 3] = [
    "ALPHA_SOURCE_UNSELECTED",
    "BETA_SOURCE_SELECTED",
    "GAMMA_SOURCE_UNSELECTED",
];
const TARGETS: [&str; 3] = [
    "ALPHA_TARGET_UNSELECTED",
    "BETA_TARGET_SELECTED",
    "GAMMA_TARGET_UNSELECTED",
];

struct Fixture {
    root: PathBuf,
    project: Project,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "ziyuan-reference-http-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Session::found(EventLog::new(&root)).unwrap();
        let texts = TextProjects::new(&root);
        let project = texts
            .create(NewProject {
                title: "HTTP references".into(),
                source_language: "en".into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap();
        let files = [
            SOURCES.join("\n\n"),
            "Neighbor context without reference prose.".into(),
            "Actual translation target.".into(),
        ];
        let paths: Vec<_> = files
            .iter()
            .enumerate()
            .map(|(i, body)| {
                let path = root.join(format!("chapter-{i}.txt"));
                fs::write(&path, body).unwrap();
                path
            })
            .collect();
        let project = texts.import(&project.id, &paths).unwrap();
        let fixture = Self { root, project };
        fixture.draft(B1, true);
        fixture
    }
    fn session(&self) -> Session<EventLog> {
        Session::open(EventLog::new(&self.root)).unwrap()
    }
    fn draft_path(&self) -> PathBuf {
        self.root
            .join(format!("translations/{}/{B1}/zh.json", self.project.id))
    }
    // Only the worker response is a fixture. Assignment, proposal and completion
    // are validated by replay, then real app apply/finalize establishes provenance.
    fn draft(&self, block: &str, finalize: bool) {
        let mut session = self.session();
        let task = session
            .issue_translation(&self.root, &self.project.id, block)
            .unwrap()
            .id;
        let worker = session
            .court()
            .route(&session.court().task(task).unwrap().edict)
            .unwrap()
            .worker;
        let mut candidate = self
            .project
            .chapters
            .iter()
            .flat_map(|c| &c.blocks)
            .find(|b| b.block_id == block)
            .unwrap()
            .clone();
        for (i, paragraph) in candidate.paragraphs.iter_mut().enumerate() {
            paragraph.text = if block == B1 {
                TARGETS[i].into()
            } else {
                "Existing target draft.".into()
            };
        }
        let events = vec![
            CourtEvent::TaskAssigned {
                task,
                worker,
                model: None,
            },
            CourtEvent::ChangesProposed {
                task,
                changes: vec![FileChange {
                    path: format!("translations/{}/{block}/zh.json", self.project.id),
                    before: None,
                    after: String::from_utf8(candidate.serialized().unwrap()).unwrap(),
                }],
            },
            CourtEvent::TaskCompleted {
                task,
                worker,
                summary: "fixture done".into(),
            },
        ];
        let mut log = EventLog::new(&self.root);
        let history = log.load().unwrap();
        let mut court = Court::replay(&history).unwrap();
        for event in &events {
            court.apply(event).unwrap();
        }
        log.append(history.len(), &events).unwrap();
        assert!(
            session
                .apply_changes(task, &self.root, vec![])
                .unwrap()
                .is_empty()
        );
        if finalize {
            session
                .finalize_translation(&self.root, &self.project.id, block)
                .unwrap();
        }
    }
    fn selected(&self, kind: TranslationKind) -> (ReferencePreview, ReferenceSubmission) {
        let session = self.session();
        let pairs = session.memory_pairs(&self.root, &self.project.id).unwrap();
        assert_eq!(pairs.pairs.len(), 3);
        let beta = pairs
            .pairs
            .iter()
            .find(|p| p.source.contains(SOURCES[1]))
            .unwrap();
        let request = ReferenceRequest {
            selections: vec![ReferenceSelection {
                identity: beta.identity.clone(),
                digests: beta.digests.clone(),
                origin: beta.origin.clone(),
            }],
            authorized_projects: Default::default(),
        };
        let preview = session
            .preview_references(&self.root, &self.project.id, B3, kind, &request)
            .unwrap();
        let submission = ReferenceSubmission {
            request,
            fingerprint: preview.fingerprint.clone(),
            submission: "loopback-submission".into(),
        };
        (preview, submission)
    }
    fn issue(&self, kind: TranslationKind, submission: &ReferenceSubmission) -> TaskId {
        self.session()
            .issue_with_references(&self.root, &self.project.id, B3, kind, submission)
            .unwrap()
            .id
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

// A finite, nonblocking listener makes zero-request assertions safe. Each accepted
// stream also has timeouts, and Drop always stops and joins the server thread.
struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(callback: impl FnOnce() + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (captured, stopped) = (requests.clone(), stop.clone());
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut callback = Some(callback);
            while !stopped.load(Ordering::SeqCst) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = read_request(&mut stream);
                        captured.lock().unwrap().push(request);
                        if let Some(callback) = callback.take() {
                            callback();
                        }
                        let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"结论：离线执行测试。\\n依据：未调用工具。\\n未办之事：未修改译稿。\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
                        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("loopback accept: {e}"),
                }
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }
    fn scheduler(&self, fixture: &Fixture) -> Scheduler {
        let model =
            OpenAiCompatible::new(&self.url, None, "loopback-reference-test").with_retry(Retry {
                attempts: 0,
                base: Duration::ZERO,
                max: Duration::ZERO,
            });
        Scheduler::new(
            Arc::new(AgentBackend::new(Box::new(model), fixture.root.clone(), 2)),
            Limits {
                max_concurrent: 1,
                timeout: Duration::from_secs(8),
            },
        )
    }
    fn captured(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("loopback server");
        }
    }
}
fn read_request(stream: &mut TcpStream) -> Value {
    // Accepted sockets can inherit the listener's nonblocking mode on other platforms.
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    let (header_end, length) = loop {
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "HTTP closed before headers");
        bytes.extend_from_slice(&chunk[..n]);
        assert!(bytes.len() < 2_000_000);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let header = std::str::from_utf8(&bytes[..end]).unwrap();
            assert!(
                header.starts_with("POST /v1/chat/completions HTTP/1.1"),
                "{header}"
            );
            let length = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .expect("content length");
            break (end + 4, length);
        }
    };
    while bytes.len() < header_end + length {
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "HTTP closed before body");
        bytes.extend_from_slice(&chunk[..n]);
    }
    serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
}

fn assert_writer_tools(request: &Value) {
    let mut names: Vec<_> = request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    // Runtime's existing ReadFile capability also grants list_files. The
    // finalized fixture supplies records, so search_records already exists.
    // Require the identical, exact set for selected and no-reference controls.
    assert_eq!(
        names,
        ["list_files", "read_file", "search_records", "write_file"]
    );
}
fn assert_failed(fixture: &Fixture, task: TaskId) {
    let session = fixture.session();
    assert!(
        matches!(
            &session.court().task(task).unwrap().status,
            TaskStatus::Failed { reason, .. } if !reason.is_empty()
        ),
        "{:?}",
        session.court().task(task).unwrap().status
    );
    // Do not couple this to localized error wording: concurrent completion of
    // the first job can also trip the reference reader's watermark guard.
    let view = session.reference_task(&fixture.root, task).unwrap();
    assert!(!view.current);
    assert!(view.reason.is_some());
    assert!(!session.history().unwrap().iter().any(|event| matches!(
        event,
        CourtEvent::ModelUsage { task: id, .. } | CourtEvent::ToolInvoked { task: id, .. } if *id == task
    )));
    assert!(
        session
            .history()
            .unwrap()
            .iter()
            .any(|e| matches!(e, CourtEvent::TaskFailed { task: id, .. } if *id == task))
    );
}

#[tokio::test]
async fn selected_beta_reaches_real_http_for_translate_and_revise() {
    for kind in [TranslationKind::Translate, TranslationKind::Revise] {
        let fixture = Fixture::new();
        if kind == TranslationKind::Revise {
            fixture.draft(B3, false);
        }
        let (preview, submission) = fixture.selected(kind);
        let task = fixture.issue(kind, &submission);
        let mut session = fixture.session();
        assert_eq!(
            session.court().task(task).unwrap().edict.seal.capabilities,
            [Capability::ReadFile, Capability::WriteFile].into()
        );
        let server = Server::new(|| {});
        session
            .run(
                &[task],
                &server.scheduler(&fixture),
                &CancelToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert!(
            matches!(
                session.court().task(task).unwrap().status,
                TaskStatus::Completed { .. }
            ),
            "{:?}",
            session.court().task(task).unwrap().status
        );
        let requests = server.captured();
        // The real agent loop sends a no-tools-used final answer back once.
        assert_eq!(requests.len(), 2);
        for request in &requests {
            let messages = request["messages"].as_array().unwrap();
            assert_eq!(
                messages
                    .iter()
                    .filter_map(|m| m["content"].as_str())
                    .map(|text| text.matches(&preview.prompt).count())
                    .sum::<usize>(),
                1,
                "the exact confirmed payload must occur once, without rewriting or duplication"
            );
            assert_eq!(
                messages
                    .iter()
                    .filter(|m| m["role"] == "user"
                        && m["content"]
                            .as_str()
                            .is_some_and(|s| s.contains(&preview.prompt)))
                    .count(),
                1
            );
            assert!(preview.prompt.contains(SOURCES[1]) && preview.prompt.contains(TARGETS[1]));
            let wire = serde_json::to_string(request).unwrap();
            for excluded in [SOURCES[0], SOURCES[2], TARGETS[0], TARGETS[2]] {
                assert!(!preview.prompt.contains(excluded));
                assert!(
                    !wire.contains(excluded),
                    "unselected prose leaked: {excluded}"
                );
            }
            assert_writer_tools(request);
        }
    }
}

#[tokio::test]
async fn no_reference_control_uses_same_real_writer_tools() {
    let fixture = Fixture::new();
    let mut session = fixture.session();
    let task = session
        .issue_translation(&fixture.root, &fixture.project.id, B3)
        .unwrap()
        .id;
    assert!(session.court().task(task).unwrap().references.is_none());
    assert_eq!(
        session.court().task(task).unwrap().edict.seal.capabilities,
        [Capability::ReadFile, Capability::WriteFile].into()
    );
    let server = Server::new(|| {});
    session
        .run(
            &[task],
            &server.scheduler(&fixture),
            &CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert!(matches!(
        session.court().task(task).unwrap().status,
        TaskStatus::Completed { .. }
    ));
    let requests = server.captured();
    assert_eq!(requests.len(), 2, "one no-tools-used send-back is expected");
    assert_writer_tools(&requests[0]);
    let wire = serde_json::to_string(&requests).unwrap();
    for prose in SOURCES.into_iter().chain(TARGETS) {
        assert!(!wire.contains(prose));
    }
}

#[tokio::test]
async fn stale_sources_fail_without_http_and_retry_recovers_original_task() {
    for reopen in [false, true] {
        let fixture = Fixture::new();
        let (preview, submission) = fixture.selected(TranslationKind::Translate);
        let task = fixture.issue(TranslationKind::Translate, &submission);
        if reopen {
            fixture
                .session()
                .reopen_translation(&fixture.root, &fixture.project.id, B1, "source withdrawn")
                .unwrap();
        } else {
            let path = fixture.draft_path();
            fs::write(
                &path,
                fs::read_to_string(&path)
                    .unwrap()
                    .replace(TARGETS[1], "Edited selected translation"),
            )
            .unwrap();
        }
        // Simulate a lost issue response: a fresh session must return the old
        // task even though a new preview would reject the now-stale selection.
        let mut session = fixture.session();
        let before = session.event_count();
        assert_eq!(
            session
                .issue_with_references(
                    &fixture.root,
                    &fixture.project.id,
                    B3,
                    TranslationKind::Translate,
                    &submission
                )
                .unwrap()
                .id,
            task
        );
        assert_eq!(session.event_count(), before);
        let view = session.reference_task(&fixture.root, task).unwrap();
        assert!(!view.current);
        assert_eq!(view.snapshot, preview);
        let server = Server::new(|| {});
        session
            .run(
                &[task],
                &server.scheduler(&fixture),
                &CancelToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert!(server.captured().is_empty());
        assert_failed(&fixture, task);
    }
}

#[tokio::test]
async fn queued_reference_is_rechecked_after_worker_permit() {
    let fixture = Fixture::new();
    let (_, submission) = fixture.selected(TranslationKind::Translate);
    let mut session = fixture.session();
    let first = session
        .issue(
            Department::Works,
            "Unrelated first task; reply briefly.".into(),
            Seal {
                capabilities: [Capability::ReadFile].into(),
                scope: vec![".".into()],
                mcp: Default::default(),
            },
        )
        .unwrap()
        .id;
    let bound = fixture.issue(TranslationKind::Translate, &submission);
    let root = fixture.root.clone();
    let path = fixture.draft_path();
    let changed = Arc::new(AtomicBool::new(false));
    let did_change = changed.clone();
    let server = Server::new(move || {
        // Both jobs are assigned before the first HTTP response is released;
        // max_concurrent=1 keeps the second job waiting for its worker permit.
        let queued = Session::open(EventLog::new(&root)).unwrap();
        assert!(matches!(
            queued.court().task(bound).unwrap().status,
            TaskStatus::Assigned(_)
        ));
        fs::write(
            &path,
            fs::read_to_string(&path)
                .unwrap()
                .replace(TARGETS[1], "Changed while queued"),
        )
        .unwrap();
        did_change.store(true, Ordering::SeqCst);
    });
    session
        .run(
            &[first, bound],
            &server.scheduler(&fixture),
            &CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert!(changed.load(Ordering::SeqCst));
    let requests = server.captured();
    assert_eq!(
        requests.len(),
        2,
        "only the first task and its no-tools-used send-back may call HTTP"
    );
    for request in &requests {
        assert!(request["messages"].as_array().unwrap().iter().any(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_str()
                    .is_some_and(|s| s.contains("Unrelated first task; reply briefly."))
        }));
        assert!(!serde_json::to_string(request).unwrap().contains(SOURCES[1]));
    }
    assert!(matches!(
        session.court().task(first).unwrap().status,
        TaskStatus::Completed { .. }
    ));
    assert_failed(&fixture, bound);
}
