//! Deterministic, unpaid real-adapter request capture for repeated material reads.
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};
use ziyuan_llm::{OpenAiCompatible, Retry};
use ziyuan_runtime::{
    BoxError, ExecutionHooks,
    agent_loop::{self, Message, Tool, ToolSpec},
};

const MARK: &str = "SOURCE_RUBY_｜猫《ねこ》_REFERENCE_SELECTED_ZHUPI_";
fn material() -> String {
    MARK.repeat(160)
}
struct ReadFile(AtomicUsize);
impl Tool for ReadFile {
    fn spec(&self) -> ToolSpec {
        spec("read_file")
    }
    fn execute(&self, args: &str) -> Result<String, BoxError> {
        match serde_json::from_str::<Value>(args)?["path"]
            .as_str()
            .unwrap()
        {
            "a" if self.0.fetch_add(1, Ordering::SeqCst) == 1 => {
                Ok("CHANGED_REREAD_DISTINCT".repeat(200))
            }
            "error" => Err("UNIQUE_READ_ERROR".into()),
            _ => Ok(material()),
        }
    }
}
struct Other(&'static str);
impl Tool for Other {
    fn spec(&self) -> ToolSpec {
        spec(self.0)
    }
    fn execute(&self, _: &str) -> Result<String, BoxError> {
        Ok(material())
    }
}
fn spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: "fixture".into(),
        input_schema_json:
            r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#
                .into(),
    }
}
// Total HTTP request bytes, exact JSON body bytes, parsed request.
type CapturedRequest = (usize, usize, Value);
struct Server {
    url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (captured, stopped) = (requests.clone(), stop.clone());
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                let request = read_request(&mut stream);
                let step = captured.lock().unwrap().len();
                captured.lock().unwrap().push(request);
                let calls: Vec<(&str, &str)> = match step {
                    0 => vec![("read_file", "a"), ("read_file", "b")],
                    1..=3 => vec![("read_file", "a")],
                    4 => vec![
                        ("read_file", "a"),
                        ("search_code", "a"),
                        ("write_file", "a"),
                        ("read_file", "error"),
                    ],
                    _ => vec![],
                };
                let delta = if calls.is_empty() {
                    json!({"content":"done"})
                } else {
                    json!({"tool_calls": calls.iter().enumerate().map(|(i, (name,path))| json!({"index":i,"id":format!("call-{step}-{i}"),"type":"function","function":{"name":name,"arguments":json!({"path":path}).to_string()}})).collect::<Vec<_>>()})
                };
                let body = format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    json!({"choices":[{"index":0,"delta":delta,"finish_reason":if calls.is_empty(){"stop"}else{"tool_calls"}}]})
                );
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn read_request(stream: &mut TcpStream) -> CapturedRequest {
    // Accepted sockets can inherit the listener's nonblocking mode on other platforms.
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buf = [0; 4096];
    let (end, len) = loop {
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buf[..n]);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let header = std::str::from_utf8(&bytes[..end]).unwrap();
            let len = header
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            break (end + 4, len);
        }
    };
    while bytes.len() < end + len {
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buf[..n]);
    }
    (
        end + len,
        len,
        serde_json::from_slice(&bytes[end..end + len]).unwrap(),
    )
}
fn capture(
    context: u64,
    hooks: &dyn ExecutionHooks,
) -> (Result<String, agent_loop::LoopError>, Vec<CapturedRequest>) {
    let server = Server::new();
    let model = OpenAiCompatible::new(&server.url, None, "material-fixture").with_retry(Retry {
        attempts: 0,
        base: Duration::ZERO,
        max: Duration::ZERO,
    });
    let result = agent_loop::run_with_context(
        &model,
        &[
            Box::new(ReadFile(AtomicUsize::new(0))),
            Box::new(Other("search_code")),
            Box::new(Other("write_file")),
        ],
        vec![
            Message::System("APPROVED_CONTEXT_KEEP".into()),
            Message::User("NEEDED_REVIEW_ADVICE_KEEP".into()),
        ],
        8,
        "fixed-material-boundary",
        hooks,
        context,
    );
    let requests = server.requests.lock().unwrap().clone();
    (result, requests)
}
#[test]
fn repeated_material_wire_capture() {
    let (result, requests) = capture(u64::MAX, &());
    assert_eq!(result.unwrap(), "done");
    assert_eq!(requests.len(), 6);
    for (i, (wire, body, request)) in requests.iter().enumerate() {
        let content: String = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["content"].as_str())
            .collect();
        println!(
            "request={i} http_bytes={wire} json_bytes={body} material_occurrences={}",
            content.matches(MARK).count()
        );
        assert!(content.contains("APPROVED_CONTEXT_KEEP"));
        assert!(content.contains("NEEDED_REVIEW_ADVICE_KEEP"));
        assert_eq!(
            content.matches(MARK).count(),
            [0, 320, 320, 320, 320, 640][i]
        );
        assert_eq!(
            content.matches("CHANGED_REREAD_DISTINCT").count(),
            if i >= 2 { 200 } else { 0 }
        );
        assert_pairing(request);
    }
    let last = &requests.last().unwrap().2;
    for id in ["call-0-1", "call-4-0", "call-4-1", "call-4-2"] {
        assert!(
            output(last, id).contains(&material()),
            "distinct argument or non-read output lost: {id}"
        );
    }
    assert!(output(last, "call-4-3").contains("UNIQUE_READ_ERROR"));
    for (i, (_, _, request)) in requests.iter().enumerate().skip(3) {
        let latest = format!("call-{}-0", i - 1);
        assert!(output(request, &latest).contains(&material()));
        assert!(output(request, "call-0-0").contains(&latest));
        assert!(!output(request, "call-0-0").contains(MARK));
        // The pointer remains within exactly the same untrusted-data boundary.
        assert_eq!(
            output(request, "call-0-0").lines().next(),
            output(request, &latest).lines().next()
        );
        assert_eq!(
            output(request, "call-0-0").lines().last(),
            output(request, &latest).lines().last()
        );
    }
}
fn output<'a>(request: &'a Value, id: &str) -> &'a str {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool" && m["tool_call_id"] == id)
        .unwrap()["content"]
        .as_str()
        .unwrap()
}
fn assert_pairing(request: &Value) {
    let messages = request["messages"].as_array().unwrap();
    let calls: Vec<_> = messages
        .iter()
        .flat_map(|m| m["tool_calls"].as_array().into_iter().flatten())
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    let outputs: Vec<_> = messages
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["tool_call_id"].as_str().unwrap())
        .collect();
    assert_eq!(calls, outputs);
}
#[test]
fn abridging_never_dangles_a_compaction_pointer() {
    let (result, requests) = capture(0, &());
    assert_eq!(result.unwrap(), "done");
    for (_, _, request) in &requests {
        assert_pairing(request);
        let messages = request["messages"].as_array().unwrap();
        for (i, message) in messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m["role"] == "tool")
        {
            let text = message["content"].as_str().unwrap();
            // A pointer must identify a later output that still contains the
            // entire material, not another pointer or an abridged head.
            for later in messages[i + 1..].iter().filter(|m| m["role"] == "tool") {
                if text.contains(later["tool_call_id"].as_str().unwrap()) {
                    assert!(later["content"].as_str().unwrap().contains(&material()));
                }
            }
        }
    }
    let last = &requests.last().unwrap().2;
    assert!(output(last, "call-4-0").contains(&material()));
    assert!(output(last, "call-4-1").contains(&material()));
    assert!(output(last, "call-4-2").contains(&material()));
    // Earlier pointers are regenerated from history: after their target ages
    // out of the recent four, the existing abridging policy applies normally.
    assert!(!output(last, "call-0-0").contains("call-3-0"));
}
#[test]
fn insufficient_budget_sends_no_http() {
    struct EmptyBudget;
    impl ExecutionHooks for EmptyBudget {
        fn token_allowance(&self) -> Option<u64> {
            Some(1)
        }
    }
    let (result, requests) = capture(u64::MAX, &EmptyBudget);
    assert!(matches!(result, Err(agent_loop::LoopError::Budget { .. })));
    assert!(requests.is_empty());
}
