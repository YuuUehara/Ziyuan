//! Independent HTTP fixtures: no dependency on text_tests' private helpers.
use crate::tests::{TOKEN, VIEWER};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use ziyuan_app::text_project::{NewProject, Project, Provenance, TextProjects};
use ziyuan_runtime::{DemoBackend, Limits};

struct Fixture {
    root: PathBuf,
    project: Project,
}
impl Fixture {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "ziyuan-web-materials-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let texts = TextProjects::new(&root);
        let project = texts
            .create(NewProject {
                title: "自创测试".into(),
                source_language: "ja".into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap();
        let source = root.join("source.txt");
        std::fs::write(&source, "先生来了。\n\n第二段。\n").unwrap();
        texts.import(&project.id, &[source]).unwrap();
        Self {
            project: texts.show(&project.id).unwrap(),
            root,
        }
    }
    fn route(&self) -> String {
        format!("/api/text/{}/materials", self.project.id)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
struct Server {
    base: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn start(root: &Path, token: &str) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = crate::router(crate::Config {
        root: root.to_owned(),
        token: token.into(),
        viewer_token: Some(VIEWER.into()),
        backend: Arc::new(DemoBackend),
        limits: Limits {
            max_concurrent: 2,
            timeout: std::time::Duration::from_secs(10),
        },
        github: None,
    });
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Server { base, task }
}
async fn call(
    server: &Server,
    route: &str,
    token: Option<&str>,
    body: Option<String>,
) -> (u16, Value) {
    let url = format!("{}{route}", server.base);
    let token = token.map(str::to_owned);
    tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let auth = token.map(|t| format!("Bearer {t}"));
        let mut response = if let Some(body) = body {
            let mut req = agent.post(&url).header("Content-Type", "application/json");
            if let Some(auth) = &auth {
                req = req.header("Authorization", auth);
            }
            req.send(body.as_bytes()).unwrap()
        } else {
            let mut req = agent.get(&url);
            if let Some(auth) = &auth {
                req = req.header("Authorization", auth);
            }
            req.call().unwrap()
        };
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    })
    .await
    .unwrap()
}
fn material() -> Value {
    json!({"format":2,"terms":[],"persons":[]})
}
fn request(id: &Value) -> Value {
    json!({"workspace_id":id,"material":material()})
}
async fn identity(s: &Server, f: &Fixture) -> Value {
    let (code, body) = call(s, &f.route(), Some(VIEWER), None).await;
    assert_eq!(code, 200, "{body}");
    assert!(body["draft"].is_null());
    assert!(body["workspace_id"].as_str().is_some_and(|s| s.len() == 64));
    body["workspace_id"].clone()
}
fn tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(base).unwrap().to_owned();
            if path.is_dir() {
                out.insert(rel, None);
                visit(base, &path, out);
            } else {
                out.insert(rel, Some(std::fs::read(path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn exact_routes_auth_and_readonly_preview() {
    let f = Fixture::new();
    let s = start(&f.root, TOKEN).await;
    let before = tree(&f.root);
    for suffix in ["", "/draft", "/validate", "/preview"] {
        let body = (!suffix.is_empty()).then(|| "{}".into());
        assert_eq!(call(&s, &(f.route() + suffix), None, body).await.0, 401);
    }
    let id = identity(&s, &f).await;
    let mut payload = request(&id);
    let (code, result) = call(
        &s,
        &(f.route() + "/validate"),
        Some(VIEWER),
        Some(payload.to_string()),
    )
    .await;
    assert_eq!(code, 200, "{result}");
    assert_eq!(result["valid"], true);
    assert_eq!(result["workspace_id"], id);
    payload["block"] = json!(f.project.chapters[0].blocks[0].block_id);
    payload["associations"] = json!([]);
    let (code, result) = call(
        &s,
        &(f.route() + "/preview"),
        Some(VIEWER),
        Some(payload.to_string()),
    )
    .await;
    assert_eq!(code, 200, "{result}");
    assert_eq!(result["workspace_id"], id);
    assert!(result["binding"]["associations_hash"].is_string());
    assert!(result["report"].is_object());
    assert_eq!(
        call(&s, &(f.route() + "/draft"), Some(VIEWER), Some("{}".into()))
            .await
            .0,
        403
    );
    for suffix in [
        "/validate/",
        "/preview/",
        "/validate-extra",
        "/preview/extra",
        "/draft/",
    ] {
        let (code, _) = call(
            &s,
            &(f.route() + suffix),
            Some(VIEWER),
            Some(payload.to_string()),
        )
        .await;
        assert!([403, 404, 405].contains(&code), "{suffix}: {code}");
    }
    assert_eq!(tree(&f.root), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_identity_is_stable_and_rejects_old_null_revision_requests() {
    let a = Fixture::new();
    let b = Fixture::new();
    // Clone the exact project, so project identity cannot substitute workspace identity.
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let p = entry.unwrap().path();
            let dest = to.join(p.file_name().unwrap());
            if p.is_dir() {
                copy(&p, &dest);
            } else {
                std::fs::copy(p, dest).unwrap();
            }
        }
    }
    copy(&a.root, &b.root);
    let sa = start(&a.root, TOKEN).await;
    let sb = start(&b.root, TOKEN).await;
    let id = identity(&sa, &a).await;
    let restarted = start(&a.root.join("."), TOKEN).await;
    assert_eq!(identity(&restarted, &a).await, id);
    let rotated = start(&a.root, VIEWER).await;
    assert_ne!(identity(&rotated, &a).await, id);
    let (code, other) = call(&sb, &a.route(), Some(VIEWER), None).await;
    assert_eq!(code, 200);
    assert_ne!(other["workspace_id"], id);
    assert!(other["draft"].is_null());
    let before = tree(&b.root);
    for suffix in ["/draft", "/validate", "/preview"] {
        let mut payload = request(&id);
        if suffix == "/draft" {
            payload["request_id"] = json!("old-null");
            payload["expected_revision"] = Value::Null;
        }
        if suffix == "/preview" {
            payload["block"] = json!(a.project.chapters[0].blocks[0].block_id);
            payload["associations"] = json!([]);
        }
        let (code, body) = call(
            &sb,
            &(a.route() + suffix),
            Some(TOKEN),
            Some(payload.to_string()),
        )
        .await;
        assert_eq!(code, 409, "{body}");
        assert_eq!(body["workspace_id"], other["workspace_id"]);
    }
    assert_eq!(tree(&b.root), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn strict_raw_requests_reject_duplicate_unknown_types_and_depth_without_writes() {
    let f = Fixture::new();
    let s = start(&f.root, TOKEN).await;
    let id = identity(&s, &f).await;
    let before = tree(&f.root);
    for suffix in ["/draft", "/validate", "/preview"] {
        assert_eq!(
            call(&s, &(f.route() + suffix), Some(TOKEN), Some("{}".into()))
                .await
                .0,
            400
        );
    }
    let base = format!(r#""workspace_id":{},"material":{}"#, id, material());
    let preview = format!(
        r#"{base},"block":{},"associations""#,
        json!(f.project.chapters[0].blocks[0].block_id)
    );
    let paragraph = &f.project.chapters[0].paragraphs[0].id;
    let association = json!({"paragraph":paragraph,"persons":[],"targets":[]});
    let cases = [
        format!(r#"{{{base},"workspace_id":{id}}}"#),
        format!(
            r#"{{"workspace_id":{id},"material":{{"format":2,"terms":[],"terms":[],"persons":[]}}}}"#
        ),
        format!(r#"{{{base},"project":{{}}}}"#),
        format!(r#"{{"workspace_id":{id},"material":[]}}"#),
        format!(
            r#"{{{base},"extra":{}0{}}}"#,
            "[".repeat(33),
            "]".repeat(33)
        ),
    ];
    for raw in cases {
        let (code, error) = call(&s, &(f.route() + "/validate"), Some(TOKEN), Some(raw)).await;
        assert_eq!(code, 400, "{error}");
        assert!(error["error"].as_str().unwrap().contains("资料"));
    }
    for speaker in [
        json!({"kind":"narration","extra":true}),
        json!({"kind":"invented"}),
        json!({"kind":"person","person_id":2}),
    ] {
        let mut assoc = association.clone();
        assoc["speaker"] = speaker;
        assert_eq!(
            call(
                &s,
                &(f.route() + "/preview"),
                Some(TOKEN),
                Some(format!(r#"{{{preview}:[{assoc}]}}"#))
            )
            .await
            .0,
            400
        );
    }
    let raw = format!(
        r#"{{{preview}:[{{"paragraph":{p},"persons":[],"targets":[],"speaker":{{"kind":"unknown","kind":"narration"}}}}]}}"#,
        p = json!(paragraph)
    );
    assert_eq!(
        call(&s, &(f.route() + "/preview"), Some(TOKEN), Some(raw))
            .await
            .0,
        400
    );
    for speaker in [
        None,
        Some(Value::Null),
        Some(json!({"kind":"unknown"})),
        Some(json!({"kind":"narration"})),
    ] {
        let mut assoc = association.clone();
        if let Some(speaker) = speaker {
            assoc["speaker"] = speaker;
        }
        let (code, body) = call(
            &s,
            &(f.route() + "/preview"),
            Some(VIEWER),
            Some(format!(r#"{{{preview}:[{assoc}]}}"#)),
        )
        .await;
        assert_eq!(code, 200, "{body}");
    }
    assert_eq!(tree(&f.root), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn save_roundtrip_conflicts_and_bad_input() {
    let f = Fixture::new();
    let s = start(&f.root, TOKEN).await;
    let id = identity(&s, &f).await;
    let mut payload = request(&id);
    payload["request_id"] = json!("save-1");
    payload["expected_revision"] = Value::Null;
    let (code, receipt) = call(
        &s,
        &(f.route() + "/draft"),
        Some(TOKEN),
        Some(payload.to_string()),
    )
    .await;
    assert_eq!(code, 200, "{receipt}");
    assert_eq!(receipt["workspace_id"], id);
    assert!(receipt["revision"].is_string());
    let (code, view) = call(&s, &f.route(), Some(VIEWER), None).await;
    assert_eq!(code, 200);
    assert_eq!(view["draft"]["material"], material());
    payload["request_id"] = json!("save-2");
    assert_eq!(
        call(
            &s,
            &(f.route() + "/draft"),
            Some(TOKEN),
            Some(payload.to_string())
        )
        .await
        .0,
        409
    );
    payload["expected_revision"] = receipt["revision"].clone();
    payload["material"]["format"] = json!(3);
    assert_eq!(
        call(
            &s,
            &(f.route() + "/draft"),
            Some(TOKEN),
            Some(payload.to_string())
        )
        .await
        .0,
        400
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_root_identity_is_cached_without_breaking_login_or_page() {
    let f = Fixture::new();
    let absent = f.root.join("not-yet-created");
    let s = start(&absent, TOKEN).await;
    std::fs::create_dir_all(&absent).unwrap();
    assert_eq!(call(&s, "/", None, None).await.0, 200);
    assert_eq!(
        call(&s, "/login", None, Some(json!({"token":TOKEN}).to_string()))
            .await
            .0,
        200
    );
    let (code, error) = call(&s, &f.route(), Some(TOKEN), None).await;
    assert_eq!(code, 500);
    assert!(error["error"].as_str().unwrap().contains("工作区"));
    assert!(
        !error
            .to_string()
            .contains(&absent.to_string_lossy().to_string())
    );
    assert!(std::fs::read_dir(absent).unwrap().next().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn posts_require_json_content_type_without_writes() {
    let f = Fixture::new();
    let s = start(&f.root, TOKEN).await;
    let id = identity(&s, &f).await;
    let before = tree(&f.root);
    for suffix in ["/draft", "/validate", "/preview"] {
        let url = format!("{}{}{suffix}", s.base, f.route());
        let body = request(&id).to_string();
        let code = tokio::task::spawn_blocking(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .into();
            agent
                .post(&url)
                .header("Content-Type", "text/plain")
                .header("Authorization", &format!("Bearer {TOKEN}"))
                .send(body.as_bytes())
                .unwrap()
                .status()
                .as_u16()
        })
        .await
        .unwrap();
        assert_eq!(code, 415, "{suffix}");
    }
    assert_eq!(tree(&f.root), before);
}

#[test]
fn workspace_identity_is_keyed_and_uses_lossless_native_path_bytes() {
    use super::identity::derive;
    let a = derive(Path::new("/w/a"), "k");
    assert_eq!(a.len(), 64);
    assert_eq!(a, derive(Path::new("/w/a"), "k"));
    assert_ne!(a, derive(Path::new("/w/b"), "k"));
    assert_ne!(a, derive(Path::new("/w/a"), "k2"));
    #[cfg(windows)]
    let (raw, lossy) = {
        use std::os::windows::ffi::OsStringExt;
        (
            std::ffi::OsString::from_wide(&[0x61, 0xD800]),
            std::ffi::OsString::from_wide(&[0x61, 0xFFFD]),
        )
    };
    #[cfg(unix)]
    let (raw, lossy) = {
        use std::os::unix::ffi::OsStringExt;
        (
            std::ffi::OsString::from_vec(b"a\xff".to_vec()),
            std::ffi::OsString::from("a\u{FFFD}"),
        )
    };
    assert_ne!(derive(Path::new(&raw), "k"), derive(Path::new(&lossy), "k"));
}
