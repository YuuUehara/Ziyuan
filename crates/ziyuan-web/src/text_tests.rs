//! 翰林院 · 译书与御笔札记的 API 测试：空状态、项目与块材料、朱批、御览、札记与观政权限。

use super::*;
use crate::tests::{TOKEN, VIEWER, call};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use ziyuan_app::Session;
use ziyuan_app::text_project::{NewProject, Project, Provenance, TextProjects};
use ziyuan_app::translation::{
    ReviewCandidate, block_dir_rel, block_source_hash, context_dir_rel, output_rel,
};
use ziyuan_core::{
    AgentId, Capability, CourtEvent, Department, FileChange, Seal, TaskId, TranslationKind,
    TranslationLink,
};
use ziyuan_governance::{Court, founding_events};
use ziyuan_runtime::{DemoBackend, Limits};
use ziyuan_storage::{EventLog, EventStore};

fn context_tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(base).unwrap().to_owned();
            if path.is_dir() {
                out.insert(relative, None);
                visit(base, &path, out);
            } else {
                out.insert(relative, Some(fs::read(path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn context_v2_http_approval_refuses_before_filling_missing_draft_files() {
    let (base, root) = start("context-v2-approve", false).await;
    let p = project(&root, "第二版拒绝", "勇者。\n");
    let dir = root.join(context_dir_rel(&p.id));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("glossary.json"),
        r#"{"format":2,"terms":[],"persons":[]}"#,
    )
    .unwrap();
    let before = context_tree(&root);
    let route = format!("/text/{}/context/approve", p.id);
    assert_eq!(
        call(&base, &route, Some(VIEWER), Some(json!({}))).await.0,
        403
    );
    let (code, error) = call(&base, &route, Some(TOKEN), Some(json!({}))).await;
    assert_eq!(code, 400, "{error}");
    assert!(
        error.to_string().contains("人物／术语资料尚未开放"),
        "{error}"
    );
    assert_eq!(context_tree(&root), before);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn context_v2_http_snapshot_refusal_keeps_message_and_missing_helpers() {
    for founded in [false, true] {
        let (base, root) = start(&format!("context-v2-snapshot-{founded}"), founded).await;
        let p = project(&root, "手工第二版快照", "勇者。\n");
        let block = &p.chapters[0].blocks[0].block_id;
        let t = ziyuan_app::translation::Translations::new(&root);
        t.ensure_context_draft(&p.id).unwrap();
        t.approve_context(&p.id).unwrap();
        let snapshot = root
            .join(".ziyuan/text-projects")
            .join(&p.id)
            .join("context/v1");
        let glossary = br#"{"format":2,"terms":[],"persons":[]}"#;
        fs::write(snapshot.join("glossary.json"), glossary).unwrap();
        let manifest_file = snapshot.join("manifest.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_file).unwrap()).unwrap();
        for entry in manifest["files"].as_array_mut().unwrap() {
            if entry["path"] == "glossary.json" {
                entry["sha256"] = json!(sha256_hex(glossary));
                entry["bytes"] = json!(glossary.len());
            }
        }
        fs::write(manifest_file, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let draft = root.join(context_dir_rel(&p.id));
        fs::remove_file(draft.join("style.md")).unwrap();
        fs::remove_dir(draft.join("summaries")).unwrap();
        let before = context_tree(&root);
        let (code, error) = call(
            &base,
            &format!("/text/{}/blocks/{block}/verification", p.id),
            Some(VIEWER),
            None,
        )
        .await;
        assert_eq!(code, 400, "{error}");
        assert!(
            error.to_string().contains("人物／术语资料尚未开放"),
            "{error}"
        );
        assert!(context_tree(&root) == before, "读取勘校不得改盘");
        let (code, error) = call(
            &base,
            &format!("/text/{}/context/approve", p.id),
            Some(TOKEN),
            Some(json!({})),
        )
        .await;
        assert_eq!(code, 400, "{error}");
        assert!(
            error.to_string().contains("人物／术语资料尚未开放"),
            "{error}"
        );
        assert!(
            context_tree(&root) == before,
            "拒绝前不得补出草稿文件或目录"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn context_v2_http_viewer_reports_unsupported_exported_glossary_without_writes() {
    let (base, root) = start("context-v2-viewer", false).await;
    let p = project(&root, "观政第二版", "勇者。\n");
    let block = &p.chapters[0].blocks[0].block_id;
    let dir = root.join(block_dir_rel(&p.id, block));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("context.json");
    let route = format!("/text/{}/blocks/{block}", p.id);
    for glossary in [
        json!({"format":2,"terms":[],"persons":[]}),
        json!({"format":3,"terms":[],"persons":[]}),
        json!([{"source":"勇者","allowed":["勇士"],"scope":"自由文本"}]),
    ] {
        let text = json!({"version":1,"glossary":glossary,"style":"","summaries":{}}).to_string();
        fs::write(&path, &text).unwrap();
        let before = context_tree(&root);
        let (code, view) = call(&base, &route, Some(VIEWER), None).await;
        assert_eq!(code, 200, "{view}");
        assert_eq!(context_tree(&root), before);
        assert_eq!(view["context"]["text"], text);
        if glossary.is_array() {
            assert_eq!(view["context"]["value"]["glossary"], glossary);
            assert!(view["context"]["error"].is_null());
        } else {
            assert!(view["context"]["value"].is_null(), "{view}");
            let error = view["context"]["error"].as_str().unwrap();
            if glossary["format"] == 2 {
                assert!(error.contains("人物／术语资料尚未开放"), "{error}");
            } else {
                assert!(error.contains("升级"), "{error}");
            }
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn verification_http_readonly_strict_permissions_stale_and_history() {
    fn files(root: &Path) -> std::collections::BTreeMap<PathBuf, String> {
        let mut result = std::collections::BTreeMap::new();
        for entry in fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                result.extend(files(&path));
            } else {
                result.insert(path.clone(), sha256_hex(&fs::read(path).unwrap()));
            }
        }
        result
    }
    let mut readonly_changes = Vec::new();
    for founded in [false, true] {
        let (base, root) = start(&format!("verification-{founded}"), founded).await;
        let p = project(
            &root,
            "人工语义复核",
            "彼女は私を愛さなくてよい。\n\n五倍になった。\n",
        );
        let block = &p.chapters[0].blocks[0];
        let paragraph = &block.paragraphs[0].id;
        let base_route = format!("/text/{}/blocks/{}", p.id, block.block_id);
        let route = format!("{base_route}/verification");
        let evidence_route = format!("{base_route}/evidence");
        assert_eq!(call(&base, &route, None, None).await.0, 401);
        let before = files(&root);
        let (code, empty) = call(&base, &route, Some(VIEWER), None).await;
        assert_eq!(code, 200, "{empty}");
        let after = files(&root);
        readonly_changes.extend(
            after
                .iter()
                .filter(|(path, hash)| before.get(*path) != Some(*hash))
                .map(|(path, _)| path.clone()),
        );
        readonly_changes.extend(
            before
                .keys()
                .filter(|path| !after.contains_key(*path))
                .cloned(),
        );
        assert_eq!(empty["records"], json!([]));
        assert!(
            empty["paragraphs"]
                .as_array()
                .unwrap()
                .iter()
                .all(|p| p["operator_reviewed"] == false)
        );
        let dir = root.join(block_dir_rel(&p.id, &block.block_id));
        fs::create_dir_all(&dir).unwrap();
        let first_translation = candidate(&p, &block.block_id);
        fs::write(dir.join("zh.json"), &first_translation).unwrap();
        let evidence = json!({"paragraph":paragraph, "quote":"彼女", "source":"https://example.invalid/dictionary", "locator":"見出し：彼女", "explanation":"主体を独立して確認", "context":"この段落の主語", "status":"tentative"});
        assert_eq!(
            call(&base, &evidence_route, Some(VIEWER), Some(evidence.clone()))
                .await
                .0,
            403
        );
        let mut extra = evidence.clone();
        extra["auto_fetch"] = json!(true);
        assert_eq!(
            call(&base, &evidence_route, Some(TOKEN), Some(extra))
                .await
                .0,
            422
        );
        let mut invalid = evidence.clone();
        invalid["status"] = json!("expert_verified");
        assert_eq!(
            call(&base, &evidence_route, Some(TOKEN), Some(invalid))
                .await
                .0,
            422
        );
        let mut invalid = evidence.clone();
        invalid["quote"] = json!("not in source");
        let (code, error) = call(&base, &evidence_route, Some(TOKEN), Some(invalid)).await;
        assert_eq!(code, 400, "{error}");
        assert!(!error.to_string().contains(root.to_str().unwrap()));
        let (code, saved_evidence) =
            call(&base, &evidence_route, Some(TOKEN), Some(evidence.clone())).await;
        assert_eq!(code, 200, "{saved_evidence}");
        assert_eq!(saved_evidence["details"], evidence);
        let (code, view) = call(&base, &route, Some(VIEWER), None).await;
        assert_eq!(code, 200, "{view}");
        let checks: Vec<_> = ["subject_speaker", "agency_negation", "quantity", "timeline", "names_relations", "humor_additions"].into_iter().map(|category| json!({"category":category, "disposition":"pending", "resolution":"unchecked", "note":"", "advice":[], "evidence":[]})).collect();
        let mut input =
            json!({"paragraph":paragraph, "fingerprint":view["fingerprint"], "checks":checks});
        input["checks"][0]["evidence"] = json!([saved_evidence["id"]]);
        assert_eq!(
            call(&base, &route, Some(VIEWER), Some(input.clone()))
                .await
                .0,
            403
        );
        let mut bad = input.clone();
        bad["auto_review"] = json!(true);
        assert_eq!(call(&base, &route, Some(TOKEN), Some(bad)).await.0, 422);
        let mut bad = input.clone();
        bad["checks"][0]["extra"] = json!(true);
        assert_eq!(call(&base, &route, Some(TOKEN), Some(bad)).await.0, 422);
        let mut bad = input.clone();
        bad["checks"][0]["resolution"] = json!("accepted");
        assert_eq!(call(&base, &route, Some(TOKEN), Some(bad)).await.0, 422);
        let mut bad = input.clone();
        bad["checks"][0]["advice"] = json!([{"task":999,"quote":"invented suggestion"}]);
        assert_eq!(call(&base, &route, Some(TOKEN), Some(bad)).await.0, 400);
        let mut bad = input.clone();
        bad["checks"][0]["evidence"] = json!([999]);
        assert_eq!(call(&base, &route, Some(TOKEN), Some(bad)).await.0, 400);
        let (code, first) = call(&base, &route, Some(TOKEN), Some(input.clone())).await;
        assert_eq!(code, 200, "{first}");
        assert_eq!(first["checks"], input["checks"]);
        let (_, recorded) = call(&base, &route, Some(VIEWER), None).await;
        assert_eq!(recorded["records"][0]["current"], true);
        assert_eq!(recorded["paragraphs"][0]["operator_reviewed"], false);
        assert_eq!(recorded["paragraphs"][0]["opinions_handled"], false);
        // Concurrent/replayed writes must not overwrite or silently duplicate history.
        assert_eq!(
            call(&base, &route, Some(TOKEN), Some(input.clone()))
                .await
                .0,
            400
        );
        input["fingerprint"] = recorded["fingerprint"].clone();
        let mut revised: Value = serde_json::from_str(&first_translation).unwrap();
        revised["paragraphs"][0]["text"] = json!("她不必爱我。");
        fs::write(dir.join("zh.json"), serde_json::to_vec(&revised).unwrap()).unwrap();
        assert_eq!(
            call(&base, &route, Some(TOKEN), Some(input.clone()))
                .await
                .0,
            400
        );
        let (_, stale) = call(&base, &route, Some(VIEWER), None).await;
        assert_eq!(stale["records"][0]["record"], first);
        assert_eq!(stale["records"][0]["current"], false);
        assert_eq!(stale["paragraphs"][0]["operator_reviewed"], false);
        input["fingerprint"] = stale["fingerprint"].clone();
        input["checks"][0]["disposition"] = json!("accepted");
        input["checks"][0]["resolution"] = json!("resolved");
        input["checks"][0]["note"] = json!("独立核对原文：她是主语；其余项目未查");
        let (code, second) = call(&base, &route, Some(TOKEN), Some(input)).await;
        assert_eq!(code, 200, "{second}");
        assert_eq!(second["previous"], first["id"]);
        let before = files(&root);
        let (_, history) = call(&base, &route, Some(VIEWER), None).await;
        assert_eq!(files(&root), before);
        assert_eq!(history["records"].as_array().unwrap().len(), 2);
        assert_eq!(history["records"][0]["record"], first);
        assert_eq!(history["records"][1]["record"], second);
        assert_eq!(
            history["records"][1]["change"]["before"],
            first["translation"]
        );
        assert_eq!(history["records"][1]["change"]["after"], "她不必爱我。");
        assert_eq!(history["evidence"][0], saved_evidence);
        assert_eq!(history["paragraphs"][0]["operator_reviewed"], false);
        fs::remove_dir_all(root).unwrap();
    }
    assert!(
        readonly_changes.is_empty(),
        "GET must not create or rewrite files: {readonly_changes:?}"
    );
}

async fn export_get(base: &str, path: &str, token: &str) -> (u16, String, String, Vec<u8>) {
    let url = format!("{base}/api{path}");
    let token = format!("Bearer {token}");
    tokio::task::spawn_blocking(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let mut response = agent
            .get(&url)
            .header("Authorization", &token)
            .call()
            .unwrap();
        let status = response.status().as_u16();
        let get = |key: &str| {
            response
                .headers()
                .get(key)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned()
        };
        let mime = get("content-type");
        let disposition = get("content-disposition");
        (
            status,
            mime,
            disposition,
            response.body_mut().read_to_vec().unwrap(),
        )
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn export_read_only_originals_strict_queries_and_safe_errors() {
    let (base, root) = start("export", false).await;
    let p = project(&root, "不可放进 header\r\n作品名", "第一段。\n");
    let route = format!("/text/{}/export", p.id);
    assert_eq!(call(&base, &route, None, None).await.0, 401);
    let (code, mime, disposition, bytes) =
        export_get(&base, &format!("{route}?kind=original"), VIEWER).await;
    assert_eq!(code, 200, "{}", String::from_utf8_lossy(&bytes));
    assert_eq!(mime, "application/x-tar");
    assert_eq!(
        disposition,
        format!("attachment; filename=\"ziyuan-{}.tar\"", p.id)
    );
    assert_eq!(bytes.len() % 512, 0);
    assert!(bytes.windows(13).any(|w| w == b"manifest.json"));
    let (code, preview) = call(
        &base,
        &format!("{route}?kind=original&preview=true"),
        Some(VIEWER),
        None,
    )
    .await;
    assert_eq!(code, 200, "{preview}");
    assert!(!preview["files"].as_array().unwrap().is_empty());
    for file in preview["files"].as_array().unwrap() {
        assert!(file.get("bytes").is_none());
        assert!(file["length"].is_number());
    }
    assert!(!preview.to_string().contains(root.to_str().unwrap()));
    for query in [
        "kind=nope",
        "format=html",
        "layout=flat",
        "unknown=true",
        "preview=yes",
        "chapters=,",
        "chapters=x,",
        "chapters=%20",
        "chapters=x,,y",
        "kind=original&format=html",
        "kind=original&chapters=missing",
        "kind=original&kind=finished",
    ] {
        let (code, mime, disposition, body) =
            export_get(&base, &format!("{route}?{query}"), VIEWER).await;
        assert_eq!(code, 400, "{query}: {}", String::from_utf8_lossy(&body));
        assert_ne!(mime, "application/x-tar");
        assert!(disposition.is_empty());
        assert!(!String::from_utf8_lossy(&body).contains(root.to_str().unwrap()));
    }
    let duplicate = format!("kind=original&chapters={0},{0}", p.chapters[0].id);
    assert_eq!(
        export_get(&base, &format!("{route}?{duplicate}"), VIEWER)
            .await
            .0,
        400
    );
    let (code, mime, disposition, body) = export_get(&base, &route, VIEWER).await;
    assert_eq!(code, 409);
    assert_ne!(mime, "application/x-tar");
    assert!(disposition.is_empty());
    assert!(!String::from_utf8_lossy(&body).contains(root.to_str().unwrap()));
    Session::found(EventLog::new(&root)).unwrap();
    let (code, mime, disposition, _) = export_get(&base, &route, VIEWER).await;
    assert_eq!(code, 400);
    assert_ne!(mime, "application/x-tar");
    assert!(disposition.is_empty());
    fs::remove_dir_all(root).unwrap();
}

/// 在临时目录启动服务；`found` 为假时不建朝（译书与札记仍应可用）。
async fn start(name: &str, found: bool) -> (String, PathBuf) {
    let root = std::env::temp_dir().join(format!("ziyuan-web-text-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    if found {
        Session::found(EventLog::new(&root)).unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let config = Config {
        root: root.clone(),
        token: TOKEN.into(),
        viewer_token: Some(VIEWER.into()),
        backend: Arc::new(DemoBackend),
        limits: Limits {
            max_concurrent: 2,
            timeout: Duration::from_secs(10),
        },
        github: None,
    };
    tokio::spawn(async move {
        axum::serve(listener, router(config)).await.unwrap();
    });
    (base, root)
}

/// 建一个只含一章一块的文本项目。
fn project(root: &Path, title: &str, body: &str) -> Project {
    let texts = TextProjects::new(root);
    let project = texts
        .create(NewProject {
            title: title.into(),
            source_language: "ja".into(),
            target_language: "zh-CN".into(),
            provenance: Provenance::default(),
        })
        .unwrap();
    let source = root.join("c1.txt");
    fs::write(&source, body).unwrap();
    texts
        .import(&project.id, std::slice::from_ref(&source))
        .unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 一块的候选译稿：每段译作「译：原文」。
fn candidate(project: &Project, block: &str) -> String {
    let prompt = project.chapters[0]
        .blocks
        .iter()
        .find(|b| b.block_id == block)
        .unwrap();
    let mut prompt = prompt.clone();
    for p in &mut prompt.paragraphs {
        p.text = format!("译：{}", p.text);
    }
    String::from_utf8(prompt.serialized().unwrap()).unwrap()
}

/// 初译诏令从下达到完成的各事件（逐条应用到 `court`，返回新事件）。
fn translate_events(
    court: &mut Court,
    project: &Project,
    block: &str,
    candidate: &str,
) -> (Vec<CourtEvent>, TaskId) {
    let seal = Seal {
        capabilities: BTreeSet::from([Capability::ReadFile, Capability::WriteFile]),
        scope: vec![block_dir_rel(&project.id, block)],
        mcp: Default::default(),
    };
    let edict = court
        .new_edict(Department::Works, "翰林院译书 · 初译".into(), seal)
        .unwrap();
    let task = edict.id;
    let worker: AgentId = court.route(&edict).unwrap().worker;
    let mut events = Vec::new();
    let push = |court: &mut Court, event: CourtEvent, events: &mut Vec<CourtEvent>| {
        court.apply(&event).unwrap();
        events.push(event);
    };
    push(court, CourtEvent::EdictIssued(Box::new(edict)), &mut events);
    let link = TranslationLink {
        project: project.id.clone(),
        block: block.into(),
        kind: TranslationKind::Translate,
        source_hash: block_source_hash(project, block).unwrap(),
        context_version: 0,
        output_path: output_rel(&project.id, block),
        zhupi_hash: sha256_hex(b"[]\n"),
    };
    push(
        court,
        court.propose_translation_link(task, link).unwrap(),
        &mut events,
    );
    push(
        court,
        CourtEvent::TaskAssigned {
            task,
            worker,
            model: None,
        },
        &mut events,
    );
    push(
        court,
        CourtEvent::ChangesProposed {
            task,
            changes: vec![FileChange {
                path: output_rel(&project.id, block),
                before: None,
                after: candidate.into(),
            }],
        },
        &mut events,
    );
    push(
        court,
        CourtEvent::TaskCompleted {
            task,
            worker,
            summary: "译毕".into(),
        },
        &mut events,
    );
    (events, task)
}

fn append(root: &Path, events: &[CourtEvent]) {
    let mut log = EventLog::new(root);
    let expected = log.load().unwrap().len();
    log.append(expected, events).unwrap();
}

fn first_paragraph(project: &Project, block: &str) -> String {
    project.chapters[0]
        .blocks
        .iter()
        .find(|b| b.block_id == block)
        .unwrap()
        .paragraphs[0]
        .id
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_state_needs_no_court_and_has_no_files() {
    let (base, root) = start("empty", false).await;
    for path in ["/text", "/notes"] {
        assert_eq!(call(&base, path, None, None).await.0, 401);
        let (status, body) = call(&base, path, Some(VIEWER), None).await;
        assert_eq!(status, 200, "{body}");
    }
    let (status, list) = call(&base, "/text", Some(TOKEN), None).await;
    assert_eq!(status, 200);
    assert_eq!(list["projects"], json!([]));
    let (status, notes) = call(&base, "/notes", Some(TOKEN), None).await;
    assert_eq!((status, notes), (200, json!([])));
    // 未知项目与未知札记都是 400，不是 500。
    assert_eq!(
        call(
            &base,
            &format!("/text/{}", "0".repeat(32)),
            Some(TOKEN),
            None
        )
        .await
        .0,
        400
    );
    assert_eq!(
        call(
            &base,
            "/notes/not-a-note/remove",
            Some(TOKEN),
            Some(json!({}))
        )
        .await
        .0,
        400
    );
    // 只读不建任何文件。
    assert!(!root.join("translations").exists());
    assert!(!root.join(".ziyuan/notes.json").exists());
    assert!(!root.join(".ziyuan/text-projects").exists());
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn project_list_outline_and_block_materials() {
    let (base, root) = start("materials", false).await;
    let project = project(&root, "作品甲", "第一段。\n\n第二段。\n");
    let block = project.chapters[0].blocks[0].block_id.clone();
    let t = Some(TOKEN);

    let (status, list) = call(&base, "/text", t, None).await;
    assert_eq!(status, 200, "{list}");
    assert_eq!(list["projects"].as_array().unwrap().len(), 1);
    let summary = &list["projects"][0];
    assert_eq!(summary["id"], json!(project.id));
    assert_eq!(summary["title"], json!("作品甲"));
    assert_eq!(summary["source_language"], json!("ja"));
    assert_eq!(summary["target_language"], json!("zh-CN"));
    assert_eq!(
        (summary["chapters"].as_u64(), summary["blocks"].as_u64()),
        (Some(1), Some(1))
    );
    assert_eq!(summary["context_version"], json!(0));

    let (status, detail) = call(&base, &format!("/text/{}", project.id), t, None).await;
    assert_eq!(status, 200, "{detail}");
    assert_eq!(detail["project"]["id"], json!(project.id));
    assert_eq!(detail["context"]["version"], json!(0));
    assert_eq!(detail["context"]["versions"], json!([]));
    assert_eq!(detail["founded"], json!(false));
    assert_eq!(detail["statuses"], json!([]));
    assert_eq!(detail["outline"][0]["blocks"][0], json!(block), "{detail}");

    let path = format!("/text/{}/blocks/{block}", project.id);
    let (status, block_json) = call(&base, &path, t, None).await;
    assert_eq!(status, 200, "{block_json}");
    assert_eq!(block_json["founded"], json!(false));
    assert_eq!(block_json["status"], Value::Null);
    assert_eq!(block_json["pending"], Value::Null);
    assert_eq!(block_json["chapter"], json!("c0001"));
    // 还没下诏：原文取项目内的块副本，译稿与检查都为空。
    assert_eq!(block_json["source_origin"], json!("project"));
    assert_eq!(
        block_json["source"]["paragraphs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["text"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        vec!["第一段。", "第二段。"]
    );
    assert_eq!(block_json["translation"], Value::Null);
    assert_eq!(block_json["translation_error"], Value::Null);
    assert_eq!(block_json["check"], Value::Null);
    assert_eq!(block_json["zhupi"], json!({ "version": 0, "items": {} }));
    assert_eq!(block_json["proposals"], json!([]));
    assert_eq!(block_json["context"]["value"], Value::Null);

    // 手写的译稿：结构检查能指出漏段。
    let dir = root.join(block_dir_rel(&project.id, &block));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("zh.json"),
        format!(
            "{{\"schema_version\": 1, \"block_id\": \"{block}\", \"paragraphs\": [{{\"id\": \"{}\", \"text\": \"译一\"}}]}}\n",
            first_paragraph(&project, &block)
        ),
    )
    .unwrap();
    fs::create_dir_all(dir.join("proposals")).unwrap();
    fs::write(dir.join("proposals/new-term.json"), b"{}").unwrap();
    let (status, block_json) = call(&base, &path, t, None).await;
    assert_eq!(status, 200, "{block_json}");
    assert_eq!(
        block_json["translation"]["paragraphs"][0]["text"],
        json!("译一")
    );
    assert_eq!(block_json["check"]["valid"], json!(false));
    assert!(
        block_json["check"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == json!("missing")),
        "{block_json}"
    );
    assert_eq!(block_json["proposals"], json!(["new-term.json"]));

    // 观政令牌：读可以，所有的写（下诏、朱批、定稿、退回、批上下文）一律 403。
    for path in [
        format!("/text/{}/context/approve", project.id),
        format!("/text/{}/blocks/{block}/zhupi", project.id),
        format!("/text/{}/blocks/{block}/finalize", project.id),
        format!("/text/{}/blocks/{block}/reopen", project.id),
        format!("/text/{}/blocks/{block}/issue", project.id),
    ] {
        assert_eq!(call(&base, &path, None, Some(json!({}))).await.0, 401);
        assert_eq!(
            call(&base, &path, Some(VIEWER), Some(json!({}))).await.0,
            403
        );
    }

    // 未知项目、未知块都是 400。
    assert_eq!(
        call(
            &base,
            &format!("/text/{}/blocks/{block}", "f".repeat(32)),
            t,
            None
        )
        .await
        .0,
        400
    );
    assert_eq!(
        call(
            &base,
            &format!("/text/{}/blocks/c0001-b0009", project.id),
            t,
            None
        )
        .await
        .0,
        400
    );
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn zhupi_set_clear_and_viewer_cannot_write() {
    let (base, root) = start("zhupi", false).await;
    let project = project(&root, "作品乙", "甲段。\n\n乙段。\n");
    let block = project.chapters[0].blocks[0].block_id.clone();
    let first = first_paragraph(&project, &block);
    let t = Some(TOKEN);
    let path = format!("/text/{}/blocks/{block}/zhupi", project.id);
    let body = json!({ "paragraph": first, "text": "此处用「主角」" });

    assert_eq!(call(&base, &path, None, Some(body.clone())).await.0, 401);
    assert_eq!(
        call(&base, &path, Some(VIEWER), Some(body.clone())).await.0,
        403
    );
    let (status, set) = call(&base, &path, t, Some(body.clone())).await;
    assert_eq!(status, 200, "{set}");
    assert_eq!(set["version"], json!(1));
    assert_eq!(set["items"][&first]["text"], json!("此处用「主角」"));
    // 同文再写不改版本。
    let (_, again) = call(&base, &path, t, Some(body)).await;
    assert_eq!(again["version"], json!(1));

    let (_, block_json) = call(
        &base,
        &format!("/text/{}/blocks/{block}", project.id),
        t,
        None,
    )
    .await;
    assert_eq!(block_json["zhupi"]["version"], json!(1));
    assert_eq!(
        block_json["zhupi"]["items"][&first]["text"],
        json!("此处用「主角」")
    );

    // 空白即清除。
    let (status, cleared) = call(
        &base,
        &path,
        t,
        Some(json!({ "paragraph": first, "text": "  " })),
    )
    .await;
    assert_eq!(status, 200, "{cleared}");
    assert_eq!(
        (cleared["version"].as_u64(), cleared["items"].clone()),
        (Some(2), json!({}))
    );
    // 不存在的段是 400。
    assert_eq!(
        call(
            &base,
            &path,
            t,
            Some(json!({ "paragraph": "c0001-p9999", "text": "改" }))
        )
        .await
        .0,
        400
    );
    assert_eq!(
        call(&base, &path, t, Some(json!({ "text": "改" }))).await.0,
        400
    );
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn notes_add_list_edit_promote_and_remove() {
    let (base, root) = start("notes", false).await;
    let project = project(&root, "作品丙", "甲段。\n\n乙段。\n");
    let block = project.chapters[0].blocks[0].block_id.clone();
    let first = first_paragraph(&project, &block);
    let t = Some(TOKEN);

    assert_eq!(
        call(
            &base,
            "/notes",
            Some(VIEWER),
            Some(json!({"source":"甲","zh":"乙"}))
        )
        .await
        .0,
        403
    );
    // 缺字段是 400。
    assert_eq!(
        call(&base, "/notes", t, Some(json!({"zh":"乙"}))).await.0,
        400
    );
    let (status, added) = call(
        &base,
        "/notes",
        t,
        Some(json!({
            "kind": "word",
            "source": "勇者",
            "zh": "勇士",
            "remark": "全书统一",
            "project": project.id,
            "block": block,
            "paragraph": first,
        })),
    )
    .await;
    assert_eq!(status, 200, "{added}");
    let id = added["note"]["id"].as_str().unwrap().to_owned();
    assert_eq!(added["note"]["kind"], json!("word"));
    assert_eq!(added["note"]["paragraph"], json!(first));

    let (status, listed) = call(&base, "/notes?kind=word&search=勇", t, None).await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], json!(id));
    assert_eq!(
        call(&base, "/notes?kind=passage", t, None)
            .await
            .1
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(call(&base, "/notes?kind=bogus", t, None).await.0, 400);

    let (status, edited) = call(
        &base,
        &format!("/notes/{id}/edit"),
        t,
        Some(json!({ "zh": "勇者大人", "remark": "" })),
    )
    .await;
    assert_eq!(status, 200, "{edited}");
    assert_eq!(edited["note"]["zh"], json!("勇者大人"));
    assert_eq!(edited["note"]["remark"], json!(""));

    // 并入术语表草稿：须再批准才生效。
    let (status, promoted) = call(
        &base,
        &format!("/notes/{id}/promote"),
        t,
        Some(json!({ "project": project.id })),
    )
    .await;
    assert_eq!(status, 200, "{promoted}");
    assert_eq!(promoted["added"], json!(true));
    assert_eq!(promoted["requires_context_approval"], json!(true));
    assert_eq!(
        promoted["path"],
        json!(format!("translations/{}/context/glossary.json", project.id))
    );
    let glossary = fs::read_to_string(
        root.join(context_dir_rel(&project.id))
            .join("glossary.json"),
    )
    .unwrap();
    assert!(glossary.contains("勇者大人"), "{glossary}");
    // 重复并入不重复写。
    let (_, again) = call(
        &base,
        &format!("/notes/{id}/promote"),
        t,
        Some(json!({ "project": project.id })),
    )
    .await;
    assert_eq!(again["added"], json!(false));

    assert_eq!(
        call(
            &base,
            &format!("/notes/{id}/remove"),
            Some(VIEWER),
            Some(json!({}))
        )
        .await
        .0,
        403
    );
    assert_eq!(
        call(&base, &format!("/notes/{id}/remove"), t, Some(json!({})))
            .await
            .0,
        200
    );
    assert_eq!(
        call(&base, "/notes", t, None)
            .await
            .1
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        call(
            &base,
            &format!("/notes/{id}/edit"),
            t,
            Some(json!({"zh":"x"}))
        )
        .await
        .0,
        400
    );
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread")]
async fn translation_status_pending_finalize_reopen_and_issue() {
    let (base, root) = start("workflow", true).await;
    let project = project(&root, "作品丁", "第一段。\n\n第二段。\n");
    let block = project.chapters[0].blocks[0].block_id.clone();
    let candidate = candidate(&project, &block);
    let mut court = Court::replay(&founding_events(7)).unwrap();
    let (events, task) = translate_events(&mut court, &project, &block, &candidate);
    append(&root, &events);
    fs::create_dir_all(root.join(block_dir_rel(&project.id, &block))).unwrap();
    fs::write(root.join(output_rel(&project.id, &block)), &candidate).unwrap();
    let t = Some(TOKEN);

    // 待准奏：状态、待准修订的最终译稿都对得上。
    let (status, block_json) = call(
        &base,
        &format!("/text/{}/blocks/{block}", project.id),
        t,
        None,
    )
    .await;
    assert_eq!(status, 200, "{block_json}");
    assert_eq!(block_json["founded"], json!(true));
    assert_eq!(
        block_json["status"]["state"]["state"],
        json!("awaiting_approval")
    );
    assert_eq!(block_json["status"]["label"], json!("待准奏"));
    assert_eq!(block_json["pending"]["task"], json!(task.0));
    assert_eq!(block_json["pending"]["kind"], json!("translate"));
    assert_eq!(
        block_json["pending"]["output"]["paragraphs"][1]["text"],
        json!("译：第二段。")
    );
    assert_eq!(block_json["check"]["valid"], json!(true));
    assert_eq!(block_json["translation"]["block_id"], json!(block));
    // 节与诏令的 JSON 都带上译书关联。
    let (_, state) = call(&base, "/state", t, None).await;
    let entry = state["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == json!(task.0))
        .unwrap();
    assert_eq!(entry["translation"]["project"], json!(project.id));
    assert_eq!(entry["translation"]["block"], json!(block));
    assert_eq!(entry["translation"]["kind"], json!("translate"));
    assert_eq!(entry["translation"]["context_version"], json!(0));
    assert_eq!(
        entry["translation"]["output_path"],
        json!(output_rel(&project.id, &block))
    );
    assert!(entry["translation"]["source_hash"].as_str().unwrap().len() == 64);
    // 尚有未结的诏令时不下新诏、不准定稿。
    assert_eq!(
        call(
            &base,
            &format!("/text/{}/blocks/{block}/issue", project.id),
            t,
            Some(json!({ "kind": "translate" }))
        )
        .await
        .0,
        400
    );
    assert_eq!(
        call(
            &base,
            &format!("/text/{}/blocks/{block}/finalize", project.id),
            t,
            Some(json!({}))
        )
        .await
        .0,
        400
    );

    // 准奏后：已准奏（待审校）。
    append(
        &root,
        &[CourtEvent::ChangesApplied {
            task,
            skipped: vec![],
        }],
    );
    let (_, block_json) = call(
        &base,
        &format!("/text/{}/blocks/{block}", project.id),
        t,
        None,
    )
    .await;
    assert_eq!(
        block_json["status"]["state"],
        json!({ "state": "applied", "task": task.0, "next": "review" })
    );
    assert_eq!(block_json["pending"], Value::Null);

    // 御览定稿。
    let (status, finalized) = call(
        &base,
        &format!("/text/{}/blocks/{block}/finalize", project.id),
        t,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, 200, "{finalized}");
    assert_eq!(
        finalized["translation_hash"],
        json!(sha256_hex(candidate.as_bytes()))
    );
    for options in ["format=txt&layout=chapters", "format=markdown&layout=book"] {
        let route = format!("/text/{}/export?{options}", project.id);
        let (code, preview) =
            call(&base, &format!("{route}&preview=true"), Some(VIEWER), None).await;
        assert_eq!(code, 200, "{preview}");
        let snapshot = preview["manifest"]["snapshot_sha256"].as_str().unwrap();
        let (code, wrong) = call(
            &base,
            &format!("{route}&snapshot=wrong"),
            Some(VIEWER),
            None,
        )
        .await;
        assert_eq!(code, 400, "{wrong}");
        assert!(wrong["error"].as_str().unwrap().contains("重新预览"));
        let (code, mime, disposition, bytes) =
            export_get(&base, &format!("{route}&snapshot={snapshot}"), VIEWER).await;
        assert_eq!(code, 200, "{}", String::from_utf8_lossy(&bytes));
        assert_eq!(mime, "application/x-tar");
        assert!(disposition.starts_with("attachment; filename=\"ziyuan-"));
        assert!(bytes.windows(13).any(|w| w == b"manifest.json"));
        assert!(!String::from_utf8_lossy(&bytes).contains(root.to_str().unwrap()));
    }
    let (_, block_json) = call(
        &base,
        &format!("/text/{}/blocks/{block}", project.id),
        t,
        None,
    )
    .await;
    assert_eq!(block_json["status"]["label"], json!("已定稿"));

    // 退回重修：须有理由。
    let reopen = format!("/text/{}/blocks/{block}/reopen", project.id);
    assert_eq!(
        call(&base, &reopen, t, Some(json!({ "reason": "  " })))
            .await
            .0,
        400
    );
    assert_eq!(
        call(
            &base,
            &reopen,
            Some(VIEWER),
            Some(json!({ "reason": "不对" }))
        )
        .await
        .0,
        403
    );
    assert_eq!(
        call(&base, &reopen, t, Some(json!({ "reason": "改第二段" })))
            .await
            .0,
        200
    );
    // A previously successful preview cannot authorize a later download after reopening.
    let (code, mime, disposition, _) =
        export_get(&base, &format!("/text/{}/export", project.id), VIEWER).await;
    assert_eq!(code, 400);
    assert_ne!(mime, "application/x-tar");
    assert!(disposition.is_empty());
    let (_, block_json) = call(
        &base,
        &format!("/text/{}/blocks/{block}", project.id),
        t,
        None,
    )
    .await;
    assert_eq!(block_json["status"]["state"]["state"], json!("applied"));
    assert_eq!(block_json["status"]["state"]["next"], json!("revise"));
    assert!(
        block_json["status"]["note"]
            .as_str()
            .unwrap()
            .contains("改第二段"),
        "{block_json}"
    );

    // 下修订诏令：只入队，块随即「执行中」。
    let (status, issued) = call(
        &base,
        &format!("/text/{}/blocks/{block}/issue", project.id),
        t,
        Some(json!({ "kind": "revise" })),
    )
    .await;
    assert_eq!(status, 200, "{issued}");
    let ids = issued["tasks"].as_array().unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(
        call(
            &base,
            &format!("/text/{}/blocks/{block}/issue", project.id),
            t,
            Some(json!({ "kind": "review" }))
        )
        .await
        .0,
        400
    );
    let (_, block_json) = call(
        &base,
        &format!("/text/{}/blocks/{block}", project.id),
        t,
        None,
    )
    .await;
    assert_eq!(block_json["status"]["label"], json!("执行中"));

    // 批准上下文：旧译稿随即「版本失效」。
    let (status, approved) = call(
        &base,
        &format!("/text/{}/context/approve", project.id),
        t,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, 200, "{approved}");
    assert_eq!(approved["version"], json!(1));
    let (_, detail) = call(&base, &format!("/text/{}", project.id), t, None).await;
    assert_eq!(detail["context"]["version"], json!(1));
    assert_eq!(detail["context"]["versions"], json!([1]));
    let _ = fs::remove_dir_all(&root);
}

/// 页面契约：卡片、三栏、逐段朱批、札记与 #wuying/text/<项目>/<块> 都得在；
/// 写操作只经 POST（观政令牌的 GET 不会被拿来改材料）。
#[test]
fn text_frontend_contract() {
    for expected in [
        "id=\"text-memory\"",
        "id=\"memory-build\"",
        "id=\"memory-query\" type=\"text\" data-view",
        "id=\"memory-search\" type=\"submit\" data-view",
        "id=\"memory-status\"",
        "id=\"memory-result\"",
        "id=\"memory-hits\"",
        "参考不是指令，朱批与术语优先，本地方法不理解文意",
        "request !== memoryRequest || project !== textProjectId",
        "esc(hit.source)",
        "esc(hit.translation)",
        "id=\"text-card\"",
        "翰林院 · 译书",
        "id=\"text-project\"",
        "id=\"text-outline\"",
        "id=\"text-source\"",
        "id=\"text-zh\"",
        "id=\"text-pending\"",
        "id=\"text-materials\"",
        "id=\"text-actions\"",
        "id=\"text-zhupi-para\"",
        "id=\"text-zhupi-input\"",
        "id=\"note-list\"",
        "id=\"note-form\"",
        "id=\"note-search\"",
        "id=\"note-kind\"",
        "id=\"note-submit\"",
        "id=\"note-status\"",
        "id=\"note-result\"",
        "初译",
        "审校",
        "修订",
        "逐段朱批",
        "御览定稿",
        "退回重修",
        "批准上下文",
        "data-text-action=\"${action}\"",
        "data-note-action=\"promote\"",
        "data-goto-task",
        "#wuying/text",
        "textHash()",
        "textDrafts",
        "sessionStorage",
        "textDraftKey",
        "ziyuan text context approve",
        "overflow-wrap: anywhere",
        "repeat(3, minmax(0, 1fr))",
        "body.viewer button:not([data-view])",
        "body.viewer input[id^=\"note-\"]:not([data-view])",
        "id=\"note-search\" data-view",
    ] {
        assert!(crate::INDEX_HTML.contains(expected), "missing {expected}");
    }
    // 三栏在窄屏（390px 落在此区间）竖排；三栏对照与札记分栏都在同一处。
    // Windows 检出可能是 CRLF。
    let html = crate::INDEX_HTML.replace("\r\n", "\n");
    let css = html
        .split("/* ---- 翰林院 · 译书")
        .nth(1)
        .expect("要有译书的样式");
    assert!(css.contains("@media (max-width: 760px)"));
    assert!(css.contains(".text-compare { grid-template-columns: minmax(0, 1fr); }"));
    // 平板先把项目栏移到上方，避免三栏被挤成每行两字；术语与路径也须断行。
    assert!(css.contains(
        "@media (max-width: 1100px) {\n    .text-layout { grid-template-columns: minmax(0, 1fr); }"
    ));
    assert!(
        css.contains(
            ".text-material { font-size: 12.5px; min-width: 0; overflow-wrap: anywhere; }"
        )
    );
    // 译书诏令的玉玺范围含 32 位项目 ID，徽章不能整行不断撑出窄屏。
    assert!(
        crate::INDEX_HTML.contains(".badge.seal { white-space: normal; overflow-wrap: anywhere; }")
    );
    // 按钮与 text status 的下一步一致：定稿后不能直接初译、修订，执行中与待准奏时不再下诏或定稿。
    // 人工三种状态复用既有徽章配色，理由与提示照服务端事实写。
    for expected in [
        "已御览定稿；要重译或修订请先退回重修",
        "finalized: [\"translate\", \"revise\"]",
        "running: [\"translate\", \"review\", \"revise\", \"finalize\"]",
        "awaiting_approval: [\"translate\", \"review\", \"revise\", \"finalize\"]",
        "awaiting_approval: \"awaiting\"",
        "human_approved: \"awaiting\", human_finalized: \"accept\", human_stale: \"failed\"",
        "human_finalized: [\"translate\", \"review\", \"revise\", \"finalize\"]",
        "human_approved: [\"finalize\"]",
        "human_stale: [\"finalize\"]",
        "已人工定稿；要重译或修订请先退回重修",
        "已人工准奏并落盘；请在上方人工编辑器核对后人工御览定稿",
        "import(`/assets/human-editor.mjs",
        "id=\"text-human-editor\"",
        "import(`/assets/materials-editor.mjs",
        "id=\"text-materials-panel\"",
        "读取本地资料",
        "保存草稿",
        "/materials/draft",
        "校验",
        "/materials/validate",
        "复制恢复 JSON",
        "预览当前块",
        "/materials/preview",
        "materials-associations",
        "适用（applicable）",
        "materials-restore",
        "放弃本地，载入服务器",
        "materials-save-state",
        "text-n2b-hints",
        "renderN2bHints",
        "materials-validate-state",
        "人工版本失效：原文、上下文或朱批已变",
        "case \"zhupi_changed\": return \"朱批已变\";",
        "b.status.state.state === \"human_stale\"",
        "ziyuan text init、ziyuan text import",
    ] {
        assert!(crate::INDEX_HTML.contains(expected), "missing {expected}");
    }
    assert!(!crate::INDEX_HTML.contains("ziyuan text new"));
    assert!(!crate::INDEX_HTML.contains("的的"));
    // 写操作都经 textAction/api(路径, 体) 走 POST；没有以查询串改状态的路子。
    for write in [
        "textAction(`${base}/zhupi`, { paragraph, text }",
        "textAction(`${base}/finalize`, {}",
        "textAction(`${base}/reopen`, { reason: reason.trim() }",
        "textAction(`${base}/issue`, { kind: \"translate\" }",
        "api(\"/notes\", body)",
    ] {
        assert!(crate::INDEX_HTML.contains(write), "missing {write}");
    }
    // 卡片排在武英殿的兵部驿站之后，不与作品关注混在一处。
    let wuying = crate::INDEX_HTML
        .split("id=\"hall-wuying\"")
        .nth(1)
        .unwrap()
        .split("id=\"hall-qiju\"")
        .next()
        .unwrap();
    assert!(
        wuying.find("id=\"text-card\"").unwrap()
            < wuying.find("id=\"subscription-panel\"").unwrap()
    );
}

/// 子进程独占提供方配置，避免并行测试修改进程环境。
#[test]
fn memory_api_local_and_disabled() {
    for provider in ["none", "local"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "text_tests::memory_api_child", "--nocapture"])
            .env("ZIYUAN_WEB_MEMORY_TEST", "1")
            .env("ZIYUAN_MEMORY_PROVIDER", provider)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_api_child() {
    if std::env::var("ZIYUAN_WEB_MEMORY_TEST").as_deref() != Ok("1") {
        return;
    }
    let local = std::env::var("ZIYUAN_MEMORY_PROVIDER").as_deref() == Ok("local");
    let (base, root) = start("memory", true).await;
    let a = project(&root, "甲", "共有记忆甲。\n");
    let b = project(&root, "乙", "共有记忆乙。\n");
    let route = format!("/text/{}/memory", a.id);
    assert_eq!(call(&base, &route, None, None).await.0, 401);
    let (code, status) = call(&base, &route, Some(VIEWER), None).await;
    assert_eq!(code, 200, "{status}");
    assert_eq!(status["available"], json!(local));
    assert_eq!(status["records"], json!(0));
    assert_eq!(status["indexed"], json!(false));
    assert_eq!(
        call(
            &base,
            &format!("{route}/build"),
            Some(VIEWER),
            Some(json!({}))
        )
        .await
        .0,
        403
    );
    assert_eq!(
        call(&base, &format!("{route}/search?q="), Some(VIEWER), None)
            .await
            .0,
        400
    );
    assert_eq!(
        call(
            &base,
            &format!("{route}/search?q=test&projects={}", b.id),
            Some(VIEWER),
            None
        )
        .await
        .0,
        400
    );
    if !local {
        for (path, body) in [
            (format!("{route}/build"), Some(json!({}))),
            (format!("{route}/search?q=test"), None),
        ] {
            let (code, error) = call(&base, &path, Some(TOKEN), body).await;
            assert_eq!(code, 400);
            assert!(
                !error
                    .to_string()
                    .contains(&root.to_string_lossy().to_string())
            );
        }
        assert!(!root.join(".ziyuan/memory/index.json").exists());
        fs::remove_dir_all(root).unwrap();
        return;
    }
    let (code, empty) = call(
        &base,
        &format!("{route}/build"),
        Some(TOKEN),
        Some(json!({})),
    )
    .await;
    assert_eq!(code, 200, "{empty}");
    assert_eq!(empty["records"], json!(0));
    fs::write(
        root.join(".ziyuan/memory/index.json"),
        "broken private cache",
    )
    .unwrap();
    let (code, broken) = call(&base, &route, Some(VIEWER), None).await;
    assert_eq!(code, 200, "{broken}");
    assert_eq!(broken["available"], json!(true));
    assert!(broken["problem"].is_string());
    assert!(!broken.to_string().contains("broken private cache"));
    assert_eq!(
        call(
            &base,
            &format!("{route}/build"),
            Some(TOKEN),
            Some(json!({}))
        )
        .await
        .0,
        200
    );
    let mut court = Court::replay(&founding_events(7)).unwrap();
    for p in [&a, &b] {
        let block = &p.chapters[0].blocks[0].block_id;
        let text = candidate(p, block);
        let (events, task) = translate_events(&mut court, p, block, &text);
        append(&root, &events);
        fs::create_dir_all(root.join(block_dir_rel(&p.id, block))).unwrap();
        fs::write(root.join(output_rel(&p.id, block)), text).unwrap();
        let applied = CourtEvent::ChangesApplied {
            task,
            skipped: vec![],
        };
        court.apply(&applied).unwrap();
        append(&root, &[applied]);
        let (code, result) = call(
            &base,
            &format!("/text/{}/blocks/{block}/finalize", p.id),
            Some(TOKEN),
            Some(json!({})),
        )
        .await;
        assert_eq!(code, 200, "{result}");
    }
    // 先建含两个项目的缓存，证明 Web 检索不能借现成缓存跨项目。
    let s = Session::open(EventLog::new(&root)).unwrap();
    s.memory_build(
        &root,
        &BTreeSet::from([a.id.clone(), b.id.clone()]),
        &ziyuan_memory::local::LocalProvider::new(),
    )
    .unwrap();
    let (code, hits) = call(
        &base,
        &format!("{route}/search?q=%E5%85%B1%E6%9C%89"),
        Some(VIEWER),
        None,
    )
    .await;
    assert_eq!(code, 200, "{hits}");
    let hits = hits["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["project"], json!(a.id));
    assert_eq!(hits[0]["source"], json!("共有记忆甲。"));
    assert_eq!(hits[0]["translation"], json!("译：共有记忆甲。"));
    assert!(hits[0]["paragraph"].is_string());
    assert!(hits[0]["block"].is_string());
    let (code, built) = call(
        &base,
        &format!("{route}/build"),
        Some(TOKEN),
        Some(json!({})),
    )
    .await;
    assert_eq!(code, 200, "{built}");
    assert_eq!(built["records"], json!(1));
    let (_, other) = call(&base, &format!("/text/{}/memory", b.id), Some(VIEWER), None).await;
    assert_eq!(other["records"], json!(0));
    // 绕过 Web 会话追加退回事件；后续检索必须刷新，不能继续拿缓存定稿。
    let block = &a.chapters[0].blocks[0].block_id;
    let mut fresh = Session::open(EventLog::new(&root)).unwrap();
    fresh
        .reopen_translation(&root, &a.id, block, "重修")
        .unwrap();
    let (code, result) = call(&base, &format!("{route}/search?q=test"), Some(VIEWER), None).await;
    assert_eq!(code, 200, "{result}");
    assert!(result["hits"].as_array().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn ordered_upload_preview_and_exact_confirmation() {
    let (base, root) = start("upload-preview", false).await;
    let texts = TextProjects::new(&root);
    let p = texts
        .create(NewProject {
            title: "上传".into(),
            source_language: "ja".into(),
            target_language: "zh-CN".into(),
            provenance: Provenance::default(),
        })
        .unwrap();
    let route = format!("/text/{}/import", p.id);
    let preview_route = format!("{route}/preview");
    let mut body = json!({"files": [
        {"name": "second.md", "text": "# 第一章\n\n漢字《かんじ》\n\n<script>alert(1)</script>\n\n# 次章\n\n続き"},
        {"name": "first.txt", "text": "末尾"}
    ], "options": {"parsing": {"ruby": true, "heading_level": 1}, "chapter_titles": []}});
    let (code, _) = call(&base, &preview_route, Some(VIEWER), Some(body.clone())).await;
    assert_eq!(code, 403);
    let (code, preview) = call(&base, &preview_route, Some(TOKEN), Some(body.clone())).await;
    assert_eq!(code, 200, "{preview}");
    assert_eq!(preview["chapters"].as_array().unwrap().len(), 3);
    assert_eq!(preview["chapters"][0]["source_name"], "second.md");
    assert!(
        !preview["chapters"][0]["parsing"]["warnings"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let serialized = preview.to_string();
    assert!(serialized.contains("\"kind\":\"ruby\""));
    assert!(serialized.contains("<script>alert(1)</script>"));
    let (code, unchanged) = call(&base, &format!("/text/{}", p.id), Some(TOKEN), None).await;
    assert_eq!(code, 200);
    assert_eq!(unchanged["project"]["chapters"], 0);
    // The unchanged preview token cannot approve changed source or titles.
    body["fingerprint"] = preview["fingerprint"].clone();
    let mut stale = body.clone();
    stale["files"][0]["text"] = json!("changed");
    let (code, _) = call(&base, &route, Some(TOKEN), Some(stale)).await;
    assert_eq!(code, 400);
    let mut titled = body.clone();
    titled["options"]["chapter_titles"] = json!(["一", "二", "三"]);
    let (code, _) = call(&base, &route, Some(TOKEN), Some(titled.clone())).await;
    assert_eq!(code, 400);
    titled.as_object_mut().unwrap().remove("fingerprint");
    let (code, revised) = call(&base, &preview_route, Some(TOKEN), Some(titled.clone())).await;
    assert_eq!(code, 200, "{revised}");
    titled["fingerprint"] = revised["fingerprint"].clone();
    let (code, _) = call(&base, &route, Some(VIEWER), Some(titled.clone())).await;
    assert_eq!(code, 403);
    let (code, imported) = call(&base, &route, Some(TOKEN), Some(titled.clone())).await;
    assert_eq!(code, 200, "{imported}");
    assert_eq!(imported["chapters"][0]["parsing"]["title"], "一");
    let block = imported["chapters"][0]["blocks"][0]["block_id"]
        .as_str()
        .unwrap();
    let (code, source) = call(
        &base,
        &format!("/text/{}/blocks/{block}", p.id),
        Some(VIEWER),
        None,
    )
    .await;
    assert_eq!(code, 200, "{source}");
    assert!(
        source["source"]
            .to_string()
            .contains("<script>alert(1)</script>")
    );
    assert!(source["source"].to_string().contains("\"kind\":\"ruby\""));
    let (code, _) = call(&base, &route, Some(TOKEN), Some(titled)).await;
    assert_eq!(code, 400);
    let (code, _) = call(
        &base,
        &preview_route,
        Some(TOKEN),
        Some(json!({"files": [{"path": "/etc/passwd"}]})),
    )
    .await;
    assert_eq!(code, 422);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn selected_references_http_permissions_confirmation_replay_and_snapshot() {
    let (base, root) = start("selected-references", true).await;
    let texts = TextProjects::new(&root);
    let p = texts
        .create(NewProject {
            title: "参考测试".into(),
            source_language: "ja".into(),
            target_language: "zh-CN".into(),
            provenance: Provenance::default(),
        })
        .unwrap();
    let first = root.join("reference.txt");
    let second = root.join("target.txt");
    fs::write(&first, "参考原文。\n").unwrap();
    fs::write(&second, "待译原文。\n").unwrap();
    let p = texts.import(&p.id, &[first, second]).unwrap();
    let source = &p.chapters[0].blocks[0].block_id;
    let target = &p.chapters[1].blocks[0].block_id;
    let mut court = Court::replay(&founding_events(7)).unwrap();
    let text = candidate(&p, source);
    let (events, task) = translate_events(&mut court, &p, source, &text);
    append(&root, &events);
    fs::create_dir_all(root.join(block_dir_rel(&p.id, source))).unwrap();
    fs::write(root.join(output_rel(&p.id, source)), text).unwrap();
    append(
        &root,
        &[CourtEvent::ChangesApplied {
            task,
            skipped: vec![],
        }],
    );
    let mut session = Session::open(EventLog::new(&root)).unwrap();
    session.finalize_translation(&root, &p.id, source).unwrap();
    assert_eq!(
        call(
            &base,
            &format!("/text/references/{}", task.0),
            Some(VIEWER),
            None
        )
        .await
        .0,
        404
    );
    let provider = ziyuan_memory::local::LocalProvider::new();
    session
        .memory_build(&root, &BTreeSet::from([p.id.clone()]), &provider)
        .unwrap();
    let hits = session
        .memory_search(&root, &BTreeSet::new(), &p.id, "参考", 8, &provider)
        .unwrap();
    let record = &hits[0].record;
    let selection =
        json!({"identity": record.identity, "digests": record.digests, "origin": record.origin});
    let request = json!({"selections": [selection], "authorized_projects": []});
    let route = format!("/text/{}/blocks/{target}/references", p.id);
    let preview_route = format!("{route}/preview");
    let issue_route = format!("{route}/issue");
    let body = json!({"kind": "translate", "request": request});
    assert_eq!(
        call(&base, &preview_route, None, Some(body.clone()))
            .await
            .0,
        401
    );
    let (code, mut preview) = call(&base, &preview_route, Some(VIEWER), Some(body.clone())).await;
    assert_eq!(code, 200, "{preview}");
    assert!(preview["prompt"].as_str().unwrap().contains("参考原文。"));
    assert_eq!(preview["references"].as_array().unwrap().len(), 1);
    assert!(preview["bytes"].as_u64().unwrap() <= 8192);
    let mut extra = body.clone();
    extra["text"] = json!("不能信任的客户端文本");
    assert_eq!(
        call(&base, &preview_route, Some(TOKEN), Some(extra))
            .await
            .0,
        422
    );
    let mut extra = body.clone();
    extra["request"]["selections"][0]["source"] = json!("伪造");
    assert_eq!(
        call(&base, &preview_route, Some(TOKEN), Some(extra))
            .await
            .0,
        422
    );
    for kind in ["review", "review_style"] {
        let mut wrong = body.clone();
        wrong["kind"] = json!(kind);
        assert_ne!(
            call(&base, &preview_route, Some(VIEWER), Some(wrong))
                .await
                .0,
            200
        );
    }
    let mut cross = body.clone();
    cross["request"]["authorized_projects"] = json!([p.id]);
    assert_eq!(
        call(&base, &preview_route, Some(VIEWER), Some(cross))
            .await
            .0,
        400
    );
    let mut cross = body.clone();
    cross["request"]["selections"][0]["identity"]["project"] = json!("other");
    assert_eq!(
        call(&base, &preview_route, Some(VIEWER), Some(cross))
            .await
            .0,
        400
    );
    let old_confirmation = json!({"kind": "translate", "submission": {
        "request": request, "fingerprint": preview["fingerprint"], "submission": "web-stale-materials"
    }});
    let (code, changed) = call(
        &base,
        &format!("/text/{}/blocks/{target}/zhupi", p.id),
        Some(TOKEN),
        Some(json!({"paragraph": p.chapters[1].blocks[0].paragraphs[0].id, "text": "预览后更新朱批"})),
    ).await;
    assert_eq!(code, 200, "{changed}");
    assert_eq!(
        call(&base, &issue_route, Some(TOKEN), Some(old_confirmation))
            .await
            .0,
        400
    );
    let (code, fresh) = call(&base, &preview_route, Some(VIEWER), Some(body)).await;
    assert_eq!(code, 200, "{fresh}");
    assert_ne!(fresh["fingerprint"], preview["fingerprint"]);
    preview = fresh;
    let submission = json!({"kind": "translate", "submission": {
        "request": request, "fingerprint": preview["fingerprint"], "submission": "web-confirmation-1"
    }});
    assert_eq!(
        call(&base, &issue_route, Some(VIEWER), Some(submission.clone()))
            .await
            .0,
        403
    );
    let mut stale = submission.clone();
    stale["submission"]["fingerprint"] = json!("stale");
    assert_eq!(
        call(&base, &issue_route, Some(TOKEN), Some(stale)).await.0,
        400
    );
    let (code, issued) = call(&base, &issue_route, Some(TOKEN), Some(submission.clone())).await;
    assert_eq!(code, 200, "{issued}");
    let (code, replay) = call(&base, &issue_route, Some(TOKEN), Some(submission.clone())).await;
    assert_eq!(code, 200, "{replay}");
    assert_eq!(replay, issued);
    let task = issued["tasks"][0].as_u64().unwrap();
    let snapshot_route = format!("/text/references/{task}");
    assert_eq!(call(&base, &snapshot_route, None, None).await.0, 401);
    let (code, view) = call(&base, &snapshot_route, Some(VIEWER), None).await;
    assert_eq!(code, 200, "{view}");
    assert_eq!(view["snapshot"], preview);
    assert_eq!(view["current"], json!(true));
    let mut session = Session::open(EventLog::new(&root)).unwrap();
    session
        .reopen_translation(&root, &p.id, source, "版本变动")
        .unwrap();
    let (code, view) = call(&base, &snapshot_route, Some(VIEWER), None).await;
    assert_eq!(code, 200, "{view}");
    assert_eq!(view["snapshot"], preview);
    assert_eq!(view["current"], json!(false));
    assert!(view["reason"].is_string());
    assert_eq!(
        call(&base, &issue_route, Some(TOKEN), Some(submission))
            .await
            .1,
        issued
    );
    fs::remove_dir_all(root).unwrap();
}

// ---- 人工译稿：只读查看、草稿、送审、准奏、定稿 ----

/// 递归列出全部目录及文件摘要，包含空目录以捕获只读入口的 mkdir。
fn all_files(root: &Path) -> std::collections::BTreeMap<PathBuf, String> {
    fn walk(dir: &Path, into: &mut std::collections::BTreeMap<PathBuf, String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                into.insert(path.clone(), "directory".into());
                walk(&path, into);
            } else {
                into.insert(path.clone(), sha256_hex(&fs::read(&path).unwrap()));
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    walk(root, &mut files);
    files
}

/// 多章项目：每个 (文件名, 正文) 成一章一块。
fn project_files(root: &Path, title: &str, files: &[(&str, &str)]) -> Project {
    let texts = TextProjects::new(root);
    let project = texts
        .create(NewProject {
            title: title.into(),
            source_language: "ja".into(),
            target_language: "zh-CN".into(),
            provenance: Provenance::default(),
        })
        .unwrap();
    let mut paths = Vec::new();
    for (name, body) in files {
        let path = root.join(name);
        fs::write(&path, body).unwrap();
        paths.push(path);
    }
    texts.import(&project.id, &paths).unwrap()
}

fn block_ref<'a>(project: &'a Project, block: &str) -> &'a ziyuan_app::text_project::PromptBlock {
    project
        .chapters
        .iter()
        .flat_map(|c| &c.blocks)
        .find(|b| b.block_id == block)
        .unwrap()
}

/// 每段「人工：原文」的草稿请求体（`DraftSave` 的 JSON 形状）。
fn draft_body(
    project: &Project,
    block: &str,
    basis: &Value,
    expected: Option<&str>,
    request_id: &str,
) -> Value {
    let paragraphs: Vec<Value> = block_ref(project, block)
        .paragraphs
        .iter()
        .map(|p| json!({ "id": p.id, "text": format!("人工：{}", p.text) }))
        .collect();
    json!({
        "request_id": request_id,
        "expected_revision": expected,
        "basis": basis.clone(),
        "paragraphs": paragraphs,
    })
}

fn zh_path(root: &Path, project: &Project, block: &str) -> PathBuf {
    root.join(output_rel(&project.id, block))
}

/// 由草稿段落推出准奏该产生的目标：（格式, 字节）。带标记即第二版。
fn expected_target(block: &str, paragraphs: &Value) -> (String, Vec<u8>) {
    let has_marks = paragraphs.as_array().is_some_and(|paragraphs| {
        paragraphs.iter().any(|p| {
            p.get("marks")
                .and_then(Value::as_array)
                .is_some_and(|marks| !marks.is_empty())
        })
    });
    let candidate: ReviewCandidate = serde_json::from_value(json!({
        "schema_version": if has_marks { 2 } else { 1 },
        "block_id": block,
        "paragraphs": paragraphs.clone(),
    }))
    .unwrap();
    let target = candidate.approved_target().unwrap();
    (target.format().to_owned(), target.bytes().unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn human_view_is_read_only_and_reports_no_draft() {
    for founded in [false, true] {
        let (base, root) = start(&format!("human-view-{founded}"), founded).await;
        let p = project(&root, "人工只读", "夜が明ける。\n");
        let block = p.chapters[0].blocks[0].block_id.clone();
        let route = format!("/text/{}/blocks/{}/human", p.id, block);
        assert_eq!(call(&base, &route, None, None).await.0, 401);
        let before = all_files(&root);
        let (code, view) = call(&base, &route, Some(VIEWER), None).await;
        assert_eq!(code, 200, "{view}");
        assert_eq!(
            all_files(&root),
            before,
            "只读 GET 不得改写或新增任何文件（含 lock 与控制目录里的文件）"
        );
        assert_eq!(view["draft"], Value::Null);
        assert_eq!(view["revision"], Value::Null);
        assert!(
            view["message"].as_str().unwrap().contains("尚无草稿"),
            "{view}"
        );
        assert_eq!(view["founded"], json!(founded));
        assert_eq!(view["human"]["approved"], json!(false));
        assert_eq!(view["human"]["finalized"], json!(false));
        assert_eq!(view["human"]["operation_id"], Value::Null);
        assert!(view["basis"]["source_hash"].is_string(), "{view}");
        assert!(!zh_path(&root, &p, &block).exists());
        if founded {
            assert_eq!(view["status"]["state"]["state"], json!("not_started"));
        } else {
            assert_eq!(view["status"], Value::Null);
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn human_draft_save_conflicts_and_mark_roundtrip() {
    let (base, root) = start("human-draft", true).await;
    let p = project(&root, "人工草稿", "夜が明ける。\n");
    let block = p.chapters[0].blocks[0].block_id.clone();
    let view_route = format!("/text/{}/blocks/{}/human", p.id, block);
    let save_route = format!("{view_route}/draft");
    let (code, view) = call(&base, &view_route, Some(TOKEN), None).await;
    assert_eq!(code, 200, "{view}");
    let basis = view["basis"].clone();

    let body = draft_body(&p, &block, &basis, None, "save-1");
    let (code, first) = call(&base, &save_route, Some(TOKEN), Some(body.clone())).await;
    assert_eq!(code, 200, "{first}");
    let revision = first["revision"].as_str().unwrap().to_owned();
    assert!(first["content_hash"].is_string());
    // 同请求同内容：返回原回执，不推进修订号。
    let (code, again) = call(&base, &save_route, Some(TOKEN), Some(body)).await;
    assert_eq!(code, 200, "{again}");
    assert_eq!(again["revision"], json!(revision));

    // 旧修订号：409 且带当前修订号；盘上仍是旧稿。
    let mut stale = draft_body(&p, &block, &basis, Some(&"0".repeat(32)), "save-2");
    let (code, conflict) = call(&base, &save_route, Some(TOKEN), Some(stale.clone())).await;
    assert_eq!(code, 409, "{conflict}");
    assert_eq!(conflict["current"], json!(revision));
    assert!(conflict["error"].is_string(), "{conflict}");
    let (_, after_conflict) = call(&base, &view_route, Some(TOKEN), None).await;
    assert_eq!(after_conflict["revision"], json!(revision));
    assert_eq!(
        after_conflict["draft"]["paragraphs"][0]["text"],
        json!("人工：夜が明ける。")
    );

    // 同请求号异内容：409，说明请求号已被占用。
    stale["request_id"] = json!("save-1");
    stale["expected_revision"] = json!(revision);
    stale["paragraphs"][0]["text"] = json!("改过的译文");
    let (code, taken) = call(&base, &save_route, Some(TOKEN), Some(stale)).await;
    assert_eq!(code, 409, "{taken}");
    assert!(taken["message"].as_str().unwrap().contains("request_id"));

    // 材料基准过期：409 且要求重新读取。
    let mut old_basis = basis.clone();
    old_basis["source_hash"] = json!("f".repeat(64));
    let body = draft_body(&p, &block, &old_basis, Some(&revision), "save-3");
    let (code, changed) = call(&base, &save_route, Some(TOKEN), Some(body)).await;
    assert_eq!(code, 409, "{changed}");
    assert!(
        changed["error"].as_str().unwrap().contains("重新读取"),
        "{changed}"
    );

    // 标记往返：ruby ＋ emphasis 逐字段读回一致。
    let marked = json!({
        "request_id": "save-marks",
        "expected_revision": revision,
        "basis": basis,
        "paragraphs": [{
            "id": block_ref(&p, &block).paragraphs[0].id,
            "text": "夜明け",
            "marks": [
                {"start": 0, "end": 3, "kind": {"type": "ruby", "reading": "よ"}},
                {"start": 3, "end": 6, "kind": {"type": "emphasis"}}
            ]
        }]
    });
    let (code, saved_marks) = call(&base, &save_route, Some(TOKEN), Some(marked.clone())).await;
    assert_eq!(code, 200, "{saved_marks}");
    let (_, view) = call(&base, &view_route, Some(VIEWER), None).await;
    assert_eq!(view["draft"]["paragraphs"], marked["paragraphs"]);
    assert_eq!(view["revision"], saved_marks["revision"]);

    // 非法字素边界（start 切在 UTF-8 中间）：400，旧稿不动。
    let mut broken = marked.clone();
    broken["request_id"] = json!("save-broken");
    broken["expected_revision"] = saved_marks["revision"].clone();
    broken["paragraphs"][0]["marks"][0]["start"] = json!(1);
    let (code, invalid) = call(&base, &save_route, Some(TOKEN), Some(broken)).await;
    assert_eq!(code, 400, "{invalid}");
    let (_, unchanged) = call(&base, &view_route, Some(VIEWER), None).await;
    assert_eq!(unchanged["draft"]["paragraphs"], marked["paragraphs"]);
    assert_eq!(unchanged["revision"], saved_marks["revision"]);
    assert!(!zh_path(&root, &p, &block).exists(), "保存草稿不得写译稿");
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn human_round_lands_plain_and_marked_targets_and_is_idempotent() {
    let (base, root) = start("human-round", true).await;
    let p = project_files(
        &root,
        "人工全流程",
        &[("c1.txt", "夜が明ける。\n"), ("c2.txt", "唄う。\n")],
    );
    let plain = p.chapters[0].blocks[0].block_id.clone();
    let marked = p.chapters[1].blocks[0].block_id.clone();
    for (block, with_marks) in [(&plain, false), (&marked, true)] {
        let view_route = format!("/text/{}/blocks/{block}/human", p.id);
        let save_route = format!("{view_route}/draft");
        let submit_route = format!("{view_route}/submit");
        let approve_route = format!("{view_route}/approve");
        let finalize_route = format!("{view_route}/finalize");
        let (code, view) = call(&base, &view_route, Some(TOKEN), None).await;
        assert_eq!(code, 200, "{view}");
        let mut body = draft_body(&p, block, &view["basis"], None, &format!("save-{block}"));
        if with_marks {
            body["paragraphs"][0]["text"] = json!("唄声");
            body["paragraphs"][0]["marks"] = json!([
                {"start": 0, "end": 3, "kind": {"type": "ruby", "reading": "うた"}},
                {"start": 3, "end": 6, "kind": {"type": "strong"}}
            ]);
        }
        let (code, saved) = call(&base, &save_route, Some(TOKEN), Some(body)).await;
        assert_eq!(code, 200, "{saved}");
        let revision = saved["revision"].as_str().unwrap().to_owned();

        // 送审：同修订号派生同一请求；重复发布不追加。
        let (code, submitted) = call(&base, &submit_route, Some(TOKEN), Some(json!({}))).await;
        assert_eq!(code, 200, "{submitted}");
        assert_eq!(submitted["published"], json!(true), "{submitted}");
        assert_eq!(
            submitted["submit_request_id"],
            json!(format!("submit-r{revision}"))
        );
        let binding = submitted["binding_hash"].clone();
        let (code, again) = call(&base, &submit_route, Some(TOKEN), Some(json!({}))).await;
        assert_eq!(code, 200, "{again}");
        assert_eq!(again["published"], json!(false));
        assert_eq!(again["binding_hash"], binding);
        assert!(again["message"].as_str().unwrap().contains("此前已入簿"));

        // 准奏即落盘：zh.json 字节等于期望目标。
        let (code, approved) = call(&base, &approve_route, Some(TOKEN), Some(json!({}))).await;
        assert_eq!(code, 200, "{approved}");
        assert_eq!(approved["approved"], json!(true), "{approved}");
        let operation = approved["operation_id"].as_str().unwrap().to_owned();
        assert_eq!(operation.len(), 32, "{approved}");
        let (_, draft_view) = call(&base, &view_route, Some(VIEWER), None).await;
        let (format, bytes) = expected_target(block, &draft_view["draft"]["paragraphs"]);
        assert_eq!(
            format,
            if with_marks {
                // 带标记候选按第二版落盘。
                "zh-json/2"
            } else {
                "zh-json/1"
            }
        );
        assert_eq!(approved["target_format"], json!(format), "{approved}");
        assert_eq!(approved["target_hash"], json!(sha256_hex(&bytes)));
        assert_eq!(fs::read(zh_path(&root, &p, block)).unwrap(), bytes);

        // 重复准奏幂等。
        let (code, repeated) = call(&base, &approve_route, Some(TOKEN), Some(json!({}))).await;
        assert_eq!(code, 200, "{repeated}");
        assert_eq!(repeated["approved"], json!(false), "{repeated}");
        assert_eq!(repeated["operation_id"], json!(operation));

        // 定稿及其幂等。
        let (code, finalized) = call(&base, &finalize_route, Some(TOKEN), Some(json!({}))).await;
        assert_eq!(code, 200, "{finalized}");
        assert_eq!(finalized["finalized"], json!(true), "{finalized}");
        let (code, repeated) = call(&base, &finalize_route, Some(TOKEN), Some(json!({}))).await;
        assert_eq!(code, 200, "{repeated}");
        assert_eq!(repeated["finalized"], json!(false), "{repeated}");

        // 状态与人工事实（只读）。
        let (_, view) = call(&base, &view_route, Some(VIEWER), None).await;
        assert_eq!(view["status"]["state"]["state"], json!("human_finalized"));
        assert_eq!(view["human"]["approved"], json!(true));
        assert_eq!(view["human"]["finalized"], json!(true));
        assert_eq!(view["human"]["operation_id"], json!(operation));
        assert_eq!(view["human"]["target_format"], json!(format));
        assert_eq!(view["human"]["target_hash"], json!(sha256_hex(&bytes)));
    }
    // 每块恰有一条送审、一条准奏、一条定稿。
    let history = EventLog::new(&root).load().unwrap();
    for event in &history {
        let actor = match event {
            CourtEvent::HumanSubmissionBound { binding } => Some(binding.request.actor),
            CourtEvent::HumanTranslationApproved { approval } => Some(approval.request.actor),
            _ => None,
        };
        if let Some(actor) = actor {
            assert_eq!(actor, ziyuan_core::HumanActor::Web);
        }
    }
    for block in [&plain, &marked] {
        let bound = history
            .iter()
            .filter(|e| {
                matches!(e, CourtEvent::HumanSubmissionBound { binding }
                    if &binding.request.block == block)
            })
            .count();
        let approved = history
            .iter()
            .filter(|e| {
                matches!(e, CourtEvent::HumanTranslationApproved { approval }
                    if &approval.request.block == block)
            })
            .count();
        let finalized = history
            .iter()
            .filter(|e| {
                matches!(e, CourtEvent::HumanTranslationFinalized { finalization }
                    if &finalization.block == block)
            })
            .count();
        assert_eq!((bound, approved, finalized), (1, 1, 1), "{block}");
    }
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn human_writes_without_court_ask_to_found_first() {
    let (base, root) = start("human-no-court", false).await;
    let p = project(&root, "未开国", "夜が明ける。\n");
    let block = p.chapters[0].blocks[0].block_id.clone();
    let view_route = format!("/text/{}/blocks/{}/human", p.id, block);
    // 草稿不经朝廷，未开国也能保存。
    let (code, view) = call(&base, &view_route, Some(TOKEN), None).await;
    assert_eq!(code, 200, "{view}");
    let body = draft_body(&p, &block, &view["basis"], None, "save-1");
    let (code, saved) = call(
        &base,
        &format!("{view_route}/draft"),
        Some(TOKEN),
        Some(body),
    )
    .await;
    assert_eq!(code, 200, "{saved}");
    for (action, route) in [
        ("submit", format!("{view_route}/submit")),
        ("approve", format!("{view_route}/approve")),
        ("finalize", format!("{view_route}/finalize")),
    ] {
        let (code, error) = call(&base, &route, Some(TOKEN), Some(json!({}))).await;
        assert_eq!(code, 400, "{action}: {error}");
        let message = error["error"].as_str().unwrap();
        assert!(message.contains("先开国"), "{action}: {message}");
    }
    assert!(!root.join(".ziyuan/court.log").exists(), "不得伪造开国");
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn human_writes_report_unreadable_history_without_init_hint() {
    let (base, root) = start("human-bad-court", true).await;
    let p = project(&root, "损坏事件簿", "夜が明ける。\n");
    let block = &p.chapters[0].blocks[0].block_id;
    fs::write(root.join(".ziyuan/court.log"), "broken history\n").unwrap();
    let before = all_files(&root);
    for action in ["submit", "approve", "finalize"] {
        let route = format!("/text/{}/blocks/{block}/human/{action}", p.id);
        let (code, error) = call(&base, &route, Some(TOKEN), Some(json!({}))).await;
        assert_eq!(code, 400, "{action}: {error}");
        let message = error["error"].as_str().unwrap();
        assert!(message.contains("事件簿"), "{action}: {message}");
        assert!(!message.contains("先开国"), "{action}: {message}");
        assert!(!message.contains(root.to_str().unwrap()));
        assert_eq!(all_files(&root), before);
    }
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn human_progress_rejects_malformed_drafts_and_non_directory_ancestors() {
    let (base, root) = start("human-progress-corrupt", true).await;
    let p = project(&root, "草稿损坏", "夜が明ける。\n");
    let block = &p.chapters[0].blocks[0].block_id;
    let dir = root
        .join(".ziyuan/text-projects")
        .join(&p.id)
        .join("human-drafts");
    let route = format!("/text/{}/human/progress", p.id);
    fs::create_dir(&dir).unwrap();
    let path = dir.join(format!("{block}.json"));
    for body in [
        "{}",
        "{\"data\":{\"draft\":{\"paragraphs\":[]}}}",
        "not json",
    ] {
        fs::write(&path, body).unwrap();
        let before = all_files(&root);
        let (code, error) = call(&base, &route, Some(VIEWER), None).await;
        assert_eq!(code, 400, "{body}: {error}");
        assert_eq!(all_files(&root), before);
    }
    fs::remove_file(path).unwrap();
    fs::remove_dir(&dir).unwrap();
    fs::write(&dir, "not a directory").unwrap();
    assert_eq!(call(&base, &route, Some(VIEWER), None).await.0, 400);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn human_progress_counts_chapters_and_next_untranslated() {
    let (base, root) = start("human-progress", true).await;
    let p = project_files(
        &root,
        "人工进度",
        &[("c1.txt", "夜が明ける。\n"), ("c2.txt", "唄う。\n")],
    );
    let first = p.chapters[0].blocks[0].block_id.clone();
    let second = p.chapters[1].blocks[0].block_id.clone();
    let route = format!("/text/{}/human/progress", p.id);
    assert_eq!(call(&base, &route, None, None).await.0, 401);
    let before = all_files(&root);
    let (code, progress) = call(&base, &route, Some(VIEWER), None).await;
    assert_eq!(code, 200, "{progress}");
    assert_eq!(all_files(&root), before, "进度只读");
    assert_eq!(progress["founded"], json!(true));
    assert_eq!(progress["totals"]["total"], json!(2));
    assert_eq!(progress["totals"]["drafted"], json!(0));
    assert_eq!(progress["totals"]["submitted"], json!(0));
    assert_eq!(progress["totals"]["approved"], json!(0));
    assert_eq!(progress["totals"]["finalized"], json!(0));
    assert_eq!(progress["totals"]["not_started"], json!(2));
    assert_eq!(progress["next_untranslated"], json!(first));
    assert_eq!(progress["chapters"].as_array().unwrap().len(), 2);
    assert_eq!(progress["chapters"][0]["chapter"], p.chapters[0].id);
    assert_eq!(progress["chapters"][0]["total"], json!(1));

    // 只保存第一块：草稿计 1，下一块是第二块。
    let (_, view) = call(
        &base,
        &format!("/text/{}/blocks/{first}/human", p.id),
        Some(TOKEN),
        None,
    )
    .await;
    let body = draft_body(&p, &first, &view["basis"], None, "save-first");
    assert_eq!(
        call(
            &base,
            &format!("/text/{}/blocks/{first}/human/draft", p.id),
            Some(TOKEN),
            Some(body)
        )
        .await
        .0,
        200
    );
    let (_, progress) = call(&base, &route, Some(VIEWER), None).await;
    assert_eq!(progress["totals"]["drafted"], json!(1));
    assert_eq!(progress["totals"]["not_started"], json!(1));
    assert_eq!(progress["chapters"][0]["drafted"], json!(1));
    assert_eq!(progress["chapters"][1]["drafted"], json!(0));
    assert_eq!(progress["next_untranslated"], json!(second));

    // 送审并准奏第一块：已送审／已准奏各 1。
    for action in ["submit", "approve"] {
        let (code, body) = call(
            &base,
            &format!("/text/{}/blocks/{first}/human/{action}", p.id),
            Some(TOKEN),
            Some(json!({})),
        )
        .await;
        assert_eq!(code, 200, "{action}: {body}");
    }
    let (_, progress) = call(&base, &route, Some(VIEWER), None).await;
    assert_eq!(progress["totals"]["submitted"], json!(1));
    assert_eq!(progress["totals"]["approved"], json!(1));
    assert_eq!(progress["totals"]["finalized"], json!(0));

    // 两块都有草稿后没有下一块待起草；定稿再计 1。
    let (_, view) = call(
        &base,
        &format!("/text/{}/blocks/{second}/human", p.id),
        Some(TOKEN),
        None,
    )
    .await;
    let body = draft_body(&p, &second, &view["basis"], None, "save-second");
    assert_eq!(
        call(
            &base,
            &format!("/text/{}/blocks/{second}/human/draft", p.id),
            Some(TOKEN),
            Some(body)
        )
        .await
        .0,
        200
    );
    assert_eq!(
        call(
            &base,
            &format!("/text/{}/blocks/{first}/human/finalize", p.id),
            Some(TOKEN),
            Some(json!({}))
        )
        .await
        .0,
        200
    );
    let (_, progress) = call(&base, &route, Some(VIEWER), None).await;
    assert_eq!(progress["totals"]["drafted"], json!(2));
    assert_eq!(progress["totals"]["not_started"], json!(0));
    assert_eq!(progress["totals"]["finalized"], json!(1));
    assert_eq!(progress["next_untranslated"], Value::Null);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn viewer_cannot_post_human_endpoints_and_disk_is_untouched() {
    let (base, root) = start("human-viewer", true).await;
    let p = project(&root, "观政只读", "夜が明ける。\n");
    let block = p.chapters[0].blocks[0].block_id.clone();
    let view_route = format!("/text/{}/blocks/{}/human", p.id, block);
    let (_, view) = call(&base, &view_route, Some(TOKEN), None).await;
    let body = draft_body(&p, &block, &view["basis"], None, "save-1");
    let draft_route = format!("{view_route}/draft");
    assert_eq!(
        call(&base, &draft_route, Some(TOKEN), Some(body.clone()))
            .await
            .0,
        200
    );
    let before = all_files(&root);
    for (route, payload) in [
        (draft_route.clone(), body),
        (format!("{view_route}/submit"), json!({})),
        (format!("{view_route}/approve"), json!({})),
        (format!("{view_route}/finalize"), json!({})),
    ] {
        let (code, error) = call(&base, &route, Some(VIEWER), Some(payload)).await;
        assert_eq!(code, 403, "{route}: {error}");
        assert!(
            error["error"].as_str().unwrap().contains("观政令牌"),
            "{error}"
        );
    }
    assert_eq!(all_files(&root), before, "观政的四个 POST 不得改动盘面");
    // 只读的人工查看与进度仍可看。
    assert_eq!(call(&base, &view_route, Some(VIEWER), None).await.0, 200);
    assert_eq!(
        call(
            &base,
            &format!("/text/{}/human/progress", p.id),
            Some(VIEWER),
            None
        )
        .await
        .0,
        200
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn materials_modules_are_served_as_javascript_without_stale_cache() {
    let (base, _root) = start("materials-assets", false).await;
    for (name, expected) in [
        ("materials-model.mjs", crate::MATERIALS_MODEL),
        ("materials-editor.mjs", crate::MATERIALS_EDITOR),
        ("materials-preview.mjs", crate::MATERIALS_PREVIEW),
    ] {
        let url = format!("{base}/assets/{name}");
        let token = format!("Bearer {TOKEN}");
        let (status, content_type, cache, body) = tokio::task::spawn_blocking(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .into();
            let mut response = agent
                .get(&url)
                .header("Authorization", token)
                .call()
                .expect("GET assets");
            let status = response.status().as_u16();
            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned();
            let cache = response
                .headers()
                .get("cache-control")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned();
            let body = response.body_mut().read_to_vec().expect("read assets");
            (status, content_type, cache, body)
        })
        .await
        .unwrap();
        assert_eq!(status, 200, "{name}");
        assert_eq!(content_type, "text/javascript; charset=utf-8", "{name}");
        assert_eq!(cache, "no-cache", "{name}");
        assert_eq!(body, expected.as_bytes(), "{name}");
    }
}
