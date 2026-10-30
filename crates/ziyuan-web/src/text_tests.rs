//! 翰林院 · 译书与御笔札记的 API 测试：空状态、项目与块材料、朱批、御览、札记与观政权限。

use super::*;
use crate::tests::{TOKEN, VIEWER, call};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use ziyuan_app::Session;
use ziyuan_app::text_project::{NewProject, Project, Provenance, TextProjects};
use ziyuan_app::translation::{block_dir_rel, block_source_hash, context_dir_rel, output_rel};
use ziyuan_core::{
    AgentId, Capability, CourtEvent, Department, FileChange, Seal, TaskId, TranslationKind,
    TranslationLink,
};
use ziyuan_governance::{Court, founding_events};
use ziyuan_runtime::{DemoBackend, Limits};
use ziyuan_storage::{EventLog, EventStore};

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
    let css = crate::INDEX_HTML
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
    for expected in [
        "已御览定稿；要重译或修订请先退回重修",
        "finalized: [\"translate\", \"revise\"]",
        "running: [\"translate\", \"review\", \"revise\", \"finalize\"]",
        "awaiting_approval: [\"translate\", \"review\", \"revise\", \"finalize\"]",
        "awaiting_approval: \"awaiting\"",
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
