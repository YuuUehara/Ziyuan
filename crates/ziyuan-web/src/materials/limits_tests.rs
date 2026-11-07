//! Raw HTTP boundary regressions and independent production-state checks.
use super::*;
use ziyuan_app::translation::Translations;
use ziyuan_runtime::{BoxError, ExecutionHooks, Job, WorkerBackend};
use ziyuan_storage::{JsonFileError, RecoveryError, WorkspaceError};

const LIMIT: usize = 2 * 1024 * 1024;
const EMPTY: &str = r#"{"format":2,"terms":[],"persons":[]}"#;

fn raw(id: &Value, material: &str, suffix: &str, f: &Fixture) -> String {
    let extra = match suffix {
        "/draft" => r#","request_id":"boundary","expected_revision":null"#.to_owned(),
        "/preview" => format!(
            r#","block":"{}","associations":[]"#,
            f.project.chapters[0].blocks[0].block_id
        ),
        _ => String::new(),
    };
    format!(r#"{{"workspace_id":{id},"material":{material}{extra}}}"#)
}

fn safe_error(body: &Value, id: &Value, secrets: &[&str]) {
    assert!(body.is_object(), "not a structured error: {body}");
    assert_eq!(&body["workspace_id"], id);
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|s| s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))),
        "{body}"
    );
    assert!(
        body.get("report").is_none(),
        "error reused a successful report"
    );
    let text = body.to_string();
    for secret in secrets {
        assert!(!text.contains(secret), "client error disclosed a secret");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn raw_boundary_accepts_exactly_two_mib_and_returns_chinese_json_above_it() {
    let f = Fixture::new();
    let s = start(&f.root, TOKEN).await;
    let id = identity(&s, &f).await;
    for suffix in ["/validate", "/preview", "/draft"] {
        let mut body = raw(&id, EMPTY, suffix, &f);
        body.push_str(&" ".repeat(LIMIT - body.len()));
        assert_eq!(body.len(), LIMIT);
        let (code, result) = call(&s, &(f.route() + suffix), Some(TOKEN), Some(body.clone())).await;
        assert_eq!(code, 200, "{suffix}: {result}");
        let before = tree(&f.root);
        body.push(' ');
        let (code, error) = call(&s, &(f.route() + suffix), Some(TOKEN), Some(body)).await;
        assert_eq!(code, 413, "{suffix}: {error}");
        safe_error(&error, &id, &[TOKEN, f.root.to_str().unwrap()]);
        assert!(error["error"].as_str().unwrap().contains("2 MiB"));
        assert_eq!(tree(&f.root), before);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn raw_malformed_requests_never_echo_input_or_reuse_preview_report() {
    let f = Fixture::new();
    let s = start(&f.root, TOKEN).await;
    let id = identity(&s, &f).await;
    let before = tree(&f.root);
    for suffix in ["/validate", "/preview", "/draft"] {
        // Establish a real successful preview before trying each malformed body.
        if suffix == "/preview" {
            let (code, good) = call(
                &s,
                &(f.route() + suffix),
                Some(TOKEN),
                Some(raw(&id, EMPTY, suffix, &f)),
            )
            .await;
            assert_eq!(code, 200, "{good}");
            assert!(good["report"].is_object());
        }
        let valid = raw(&id, EMPTY, suffix, &f);
        let depth33 = format!("{}0{}", "[".repeat(32), "]".repeat(32));
        let cases = [
            valid.replacen(
                r#""workspace_id":"#,
                &format!(r#""workspace_id":"{TOKEN}","workspace_id":"#),
                1,
            ),
            valid.replacen(
                r#""material":"#,
                &format!(r#""{TOKEN}":true,"material":"#),
                1,
            ),
            raw(
                &id,
                &format!(r#"{{"format":"{TOKEN}","terms":[],"persons":[]}}"#),
                suffix,
                &f,
            ),
            raw(&id, r#"{"format":3,"terms":[],"persons":[]}"#, suffix, &f),
            raw(&id, &depth33, suffix, &f),
            raw(&id, "[]", suffix, &f),
            raw(
                &id,
                r#"{"format":2,"terms":[{"term_id":"t","source":"s","category":"common","allowed":["a"],"scope":{"kind":"project","kind":"project"},"evidence":[],"status":"tentative"}],"persons":[]}"#,
                suffix,
                &f,
            ),
        ];
        for body in cases {
            let (code, error) = call(&s, &(f.route() + suffix), Some(TOKEN), Some(body)).await;
            assert_eq!(code, 400, "{suffix}: {error}");
            safe_error(&error, &id, &[TOKEN, f.root.to_str().unwrap()]);
        }
    }
    assert_eq!(tree(&f.root), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn preview_semantic_errors_preserve_structured_issue_without_input_values() {
    let f = Fixture::new();
    let s = start(&f.root, TOKEN).await;
    let id = identity(&s, &f).await;
    let before = tree(&f.root);
    let material = format!(
        r#"{{"format":2,"terms":[{{"term_id":"t","source":"s","category":"common","allowed":["a"],"scope":{{"kind":"range","from":{{"chapter":"{TOKEN}"}},"to":{{"chapter":"{TOKEN}"}}}},"evidence":[],"status":"tentative"}}],"persons":[]}}"#
    );
    let (code, error) = call(
        &s,
        &(f.route() + "/preview"),
        Some(TOKEN),
        Some(raw(&id, &material, "/preview", &f)),
    )
    .await;
    assert_eq!(code, 400, "{error}");
    assert_eq!(error["issue"]["code"], "position");
    assert!(error["issue"]["path"].is_array());
    safe_error(&error, &id, &[TOKEN, f.root.to_str().unwrap()]);
    assert_eq!(tree(&f.root), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn raw_revision_conflict_preserves_current_and_state() {
    let f = Fixture::new();
    let s = start(&f.root, TOKEN).await;
    let id = identity(&s, &f).await;
    let body = raw(&id, EMPTY, "/draft", &f);
    let (code, receipt) = call(&s, &(f.route() + "/draft"), Some(TOKEN), Some(body.clone())).await;
    assert_eq!(code, 200, "{receipt}");
    let before = tree(&f.root);
    let (code, error) = call(
        &s,
        &(f.route() + "/draft"),
        Some(TOKEN),
        Some(body.replace("boundary", "other-request")),
    )
    .await;
    assert_eq!(code, 409);
    assert_eq!(error["current"], receipt["revision"]);
    safe_error(&error, &id, &[TOKEN, f.root.to_str().unwrap()]);
    assert_eq!(tree(&f.root), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn corrupt_newer_and_oversized_drafts_have_safe_http_errors() {
    let f = Fixture::new();
    let s = start(&f.root, TOKEN).await;
    let id = identity(&s, &f).await;
    let path = f
        .root
        .join(".ziyuan/text-projects")
        .join(&f.project.id)
        .join("material-draft.json");
    let bad = format!(r#"{{"schema_version":1,"revision":1,"data":{{"{TOKEN}":true}}}}"#);
    for (bytes, expected) in [
        (bad, 500),
        (
            r#"{"schema_version":999,"revision":1,"data":{}}"#.into(),
            409,
        ),
        (" ".repeat(8 * 1024 * 1024 + 1), 413),
    ] {
        std::fs::write(&path, bytes).unwrap();
        let before = tree(&f.root);
        let (code, error) = call(&s, &f.route(), Some(TOKEN), None).await;
        assert_eq!(code, expected, "{error}");
        safe_error(&error, &id, &[TOKEN, f.root.to_str().unwrap()]);
        assert_eq!(tree(&f.root), before);
    }
}

#[tokio::test]
async fn internal_diagnostics_are_not_client_messages() {
    use ziyuan_app::translation::materials::MaterialError as E;
    let secret = r#"D:\private\workspace request-body bearer-token"#;
    let id = Value::String("workspace".into());
    for (error, status) in [
        (E::Storage(JsonFileError::Corrupt(secret.into())), 500),
        (E::Storage(JsonFileError::Serialize(secret.into())), 500),
        (E::Io(std::io::Error::other(secret)), 500),
        (E::Path(secret.into()), 500),
        (
            E::Workspace(WorkspaceError::Recovery(RecoveryError::Corrupt(
                secret.into(),
            ))),
            500,
        ),
        (E::Storage(JsonFileError::TooLarge { limit: 123 }), 413),
    ] {
        let response = super::super::failure("workspace", error);
        assert_eq!(response.status().as_u16(), status);
        let bytes = axum::body::to_bytes(response.into_body(), LIMIT)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        safe_error(
            &body,
            &id,
            &[secret, "private", "bearer-token", "request-body"],
        );
    }
}

#[derive(Default)]
struct CountingBackend(AtomicUsize);
impl WorkerBackend for CountingBackend {
    fn execute(&self, _: &Job, _: &dyn ExecutionHooks) -> Result<String, BoxError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("materials must not execute".into())
    }
    fn complete(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
    ) -> Result<(String, ziyuan_core::TokenUsage), BoxError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err("materials must not complete".into())
    }
    fn workspace_root(&self) -> Option<&Path> {
        self.0.fetch_add(1, Ordering::SeqCst);
        None
    }
    fn model_name(&self) -> Option<&str> {
        self.0.fetch_add(1, Ordering::SeqCst);
        None
    }
    fn price(&self, _: &str) -> Option<ziyuan_core::Price> {
        self.0.fetch_add(1, Ordering::SeqCst);
        None
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_are_isolated_from_backend_snapshots_events_exports_and_verification() {
    let f = Fixture::new();
    ziyuan_app::Session::found(ziyuan_storage::EventLog::new(&f.root)).unwrap();
    let t = Translations::new(&f.root);
    t.ensure_context_draft(&f.project.id).unwrap();
    t.approve_context(&f.project.id).unwrap();
    let block = &f.project.chapters[0].blocks[0].block_id;
    t.export(&f.project.id, block).unwrap();
    let verification = f
        .root
        .join(".ziyuan/text-projects")
        .join(&f.project.id)
        .join("verification.json");
    std::fs::write(
        &verification,
        r#"{"schema_version":1,"revision":1,"data":{"sequence":0,"evidence":[],"records":[]}}"#,
    )
    .unwrap();
    let verification_before =
        serde_json::to_value(t.verification(None, &f.project.id, block).unwrap()).unwrap();
    let before = tree(&f.root);
    let backend = Arc::new(CountingBackend::default());
    let s = start_with_backend(&f.root, TOKEN, backend.clone()).await;
    let id = identity(&s, &f).await;
    assert_eq!(tree(&f.root), before, "GET changed production state");
    assert_eq!(backend.0.load(Ordering::SeqCst), 0);
    for suffix in ["/validate", "/preview", "/draft"] {
        let (code, result) = call(
            &s,
            &(f.route() + suffix),
            Some(TOKEN),
            Some(raw(&id, EMPTY, suffix, &f)),
        )
        .await;
        assert_eq!(code, 200, "{suffix}: {result}");
        assert_eq!(backend.0.load(Ordering::SeqCst), 0);
        let mut after = tree(&f.root);
        // Only the independent draft and its persistent JsonFile lock may be new.
        let dir = PathBuf::from(".ziyuan/text-projects").join(&f.project.id);
        after.remove(&dir.join("material-draft.json"));
        after.remove(&dir.join("material-draft.json.lock"));
        assert_eq!(after, before, "production state changed after {suffix}");
    }
    let saved = tree(&f.root);
    assert_eq!(call(&s, &f.route(), Some(VIEWER), None).await.0, 200);
    assert_eq!(tree(&f.root), saved, "GET changed the saved workspace");
    assert_eq!(backend.0.load(Ordering::SeqCst), 0);
    assert_eq!(
        serde_json::to_value(t.verification(None, &f.project.id, block).unwrap()).unwrap(),
        verification_before
    );
    assert!(
        f.root
            .join(".ziyuan/text-projects")
            .join(&f.project.id)
            .join("context/v1/manifest.json")
            .is_file()
    );
    assert!(
        f.root
            .join("translations")
            .join(&f.project.id)
            .join(block)
            .join("context.json")
            .is_file()
    );
}
