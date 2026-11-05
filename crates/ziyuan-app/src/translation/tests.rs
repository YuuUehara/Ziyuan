use super::*;
use crate::text_project::{BlockParagraph, NewProject, PromptBlock, Provenance};
use crate::{AppError, Session};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use ziyuan_core::{
    Capability, CourtEvent, Department, FileChange, TaskId, TranslationKind, TranslationLink,
};
use ziyuan_governance::{ChangeState, TaskStatus};
use ziyuan_storage::EventLog;

const B1: &str = "c0001-b0001";
const B2: &str = "c0002-b0001";
const B3: &str = "c0003-b0001";

struct Fx {
    root: PathBuf,
    project: String,
}

impl Fx {
    fn new(name: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        // 与其他测试共用同一个临时目录时，别的测试进程留下的事件簿与受管写入记录会让本测试
        // 打不开或写不进去：用一个只属于本测试的目录名。
        let root = std::env::temp_dir().join(format!(
            "ziyuan-translation-{name}-{}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Session::found(EventLog::new(&root)).unwrap();
        let texts = TextProjects::new(&root);
        let project = texts
            .create(NewProject {
                title: "作品".into(),
                source_language: "ja".into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap();
        let c1: String = (1..=6).map(|i| format!("第{i}段の文。\n\n")).collect();
        let files = [
            ("c1.txt", c1.as_str()),
            ("c2.txt", "二章一。\n\n二章二。\n"),
            ("c3.txt", "三章。\n"),
        ];
        let mut paths = Vec::new();
        for (name, body) in files {
            let path = root.join(name);
            fs::write(&path, body).unwrap();
            paths.push(path);
        }
        texts.import(&project.id, &paths).unwrap();
        Self {
            root,
            project: project.id,
        }
    }
    fn session(&self) -> Session<EventLog> {
        Session::open(EventLog::new(&self.root)).unwrap()
    }
    fn t(&self) -> Translations {
        Translations::new(&self.root)
    }
    fn block(&self, block: &str) -> PromptBlock {
        let p = self.t().project(&self.project).unwrap();
        find_block(&p, block).unwrap().2.clone()
    }
    /// 候选稿：每段译作「译：原文」，再经 `edit` 调整段落。
    fn candidate(&self, block: &str, edit: impl FnOnce(&mut Vec<BlockParagraph>)) -> String {
        let mut b = self.block(block);
        for p in &mut b.paragraphs {
            p.text = format!("译：{}", p.text);
        }
        edit(&mut b.paragraphs);
        String::from_utf8(b.serialized().unwrap()).unwrap()
    }
    fn zh_path(&self, block: &str) -> PathBuf {
        self.root.join(output_rel(&self.project, block))
    }
    fn zh_change(&self, block: &str, after: String) -> FileChange {
        FileChange {
            path: output_rel(&self.project, block),
            before: fs::read_to_string(self.zh_path(block)).ok(),
            after,
        }
    }
    fn status(&self, s: &Session<EventLog>, block: &str) -> BlockStatus {
        s.translation_status(&self.root, &self.project)
            .unwrap()
            .into_iter()
            .find(|b| b.block == block)
            .unwrap()
    }
    fn write_glossary(&self, body: &str) {
        self.t().ensure_context_draft(&self.project).unwrap();
        fs::write(
            self.root
                .join(context_dir_rel(&self.project))
                .join(GLOSSARY_FILE),
            body,
        )
        .unwrap();
    }
    fn write_summary(&self, chapter: &str, body: &str) {
        self.t().ensure_context_draft(&self.project).unwrap();
        fs::write(
            self.root
                .join(context_dir_rel(&self.project))
                .join(format!("summaries/{chapter}.md")),
            body,
        )
        .unwrap();
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// 模拟一次执行：指派、呈报 `files`（可空）、完成。
fn complete(s: &mut Session<EventLog>, task: TaskId, files: Vec<FileChange>, summary: &str) {
    let worker = s
        .court()
        .route(&s.court().task(task).unwrap().edict)
        .unwrap()
        .worker;
    s.commit(|_| {
        let mut events = vec![CourtEvent::TaskAssigned {
            task,
            worker,
            model: None,
        }];
        if !files.is_empty() {
            events.push(CourtEvent::ChangesProposed {
                task,
                changes: files.clone(),
            });
        }
        events.push(CourtEvent::TaskCompleted {
            task,
            worker,
            summary: summary.into(),
        });
        Ok(events)
    })
    .unwrap();
}

/// 下初译并完成，呈报一份完整候选稿（不准奏）。
fn translated(fx: &Fx, s: &mut Session<EventLog>, block: &str) -> TaskId {
    let edict = s.issue_translation(&fx.root, &fx.project, block).unwrap();
    let change = fx.zh_change(block, fx.candidate(block, |_| {}));
    complete(s, edict.id, vec![change], "译毕");
    edict.id
}

fn apply_error<T: std::fmt::Debug>(result: std::result::Result<T, AppError>) -> String {
    match result {
        Err(AppError::Apply(msg)) => msg,
        other => panic!("应被门控拒绝：{other:?}"),
    }
}

#[test]
fn human_binding_admission_resolves_approval_order_and_preserves_receipt_identity() {
    use ziyuan_core::{HumanActor, TranslationSource};
    use ziyuan_storage::{EventStore, LockScope, Workspace};
    let fx = Fx::new("human-admission");
    let t = fx.t();
    let mut s = fx.session();
    assert_eq!(
        resolve_translation_source(s.history().unwrap(), &fx.project, B1)
            .unwrap()
            .source,
        TranslationSource::None {}
    );
    let first = translated(&fx, &mut s, B1);
    s.apply_changes(first, &fx.root, vec![]).unwrap();
    let disk = fs::read(fx.zh_path(B1)).unwrap();
    // Two legal model revisions whose application order differs from task order.
    // Only proposals change, so all three approvals have identical translation bytes.
    let link = s.court().task(first).unwrap().translation.clone().unwrap();
    let seal = s.court().task(first).unwrap().edict.seal.clone();
    let mut revisions = Vec::new();
    for n in 0..2 {
        let task = s
            .issue(Department::Works, format!("补建议{n}"), seal.clone())
            .unwrap()
            .id;
        let mut revision = link.clone();
        revision.kind = TranslationKind::Revise;
        s.commit(|court| {
            Ok(vec![
                court.propose_translation_link(task, revision.clone())?,
            ])
        })
        .unwrap();
        complete(
            &mut s,
            task,
            vec![FileChange {
                path: format!("{}note-{n}.md", proposals_rel(&fx.project, B1)),
                before: None,
                after: format!("称谓建议{n}"),
            }],
            "补建议",
        );
        revisions.push(task);
    }
    s.apply_changes(revisions[1], &fx.root, vec![]).unwrap();
    let before_source = resolve_translation_source(s.history().unwrap(), &fx.project, B1).unwrap();
    let draft = DraftSave {
        request_id: "save".into(),
        expected_revision: None,
        basis: t.human_draft(&fx.project, B1).unwrap().basis,
        paragraphs: fx
            .block(B1)
            .paragraphs
            .into_iter()
            .map(|p| DraftParagraph {
                id: p.id,
                text: format!("人工：{}", p.text),
                marks: vec![],
            })
            .collect(),
    };
    let saved = t.save_human_draft(&fx.project, B1, &draft).unwrap();
    let submission = t
        .submit_human_draft(
            &fx.project,
            B1,
            &DraftSubmit {
                request_id: "submit".into(),
                expected_revision: saved.revision,
            },
        )
        .unwrap();
    let old = submission
        .prepare_binding("bind-old".into(), HumanActor::Local, before_source.source)
        .unwrap();
    s.apply_changes(revisions[0], &fx.root, vec![]).unwrap();
    assert_eq!(fs::read(fx.zh_path(B1)).unwrap(), disk);
    let history = s.history().unwrap();
    let resolved = resolve_translation_source(history, &fx.project, B1).unwrap();
    let index = history
        .iter()
        .rposition(|e| matches!(e, CourtEvent::ChangesApplied {task, ..} if *task == revisions[0]))
        .unwrap();
    assert_eq!(
        resolved.source,
        TranslationSource::Model {
            task: revisions[0],
            changes_version: crate::changes_version(
                &s.court()
                    .task(revisions[0])
                    .unwrap()
                    .changes
                    .as_ref()
                    .unwrap()
                    .files
            ),
            event: ziyuan_storage::event_reference(index as u64, &history[index]),
        }
    );
    assert_eq!(resolved.translation_hash, Some(sha256_hex(&disk)));
    let binding = submission
        .prepare_binding("bind".into(), HumanActor::Local, resolved.source.clone())
        .unwrap();
    let workspace = Workspace::open(&fx.root).unwrap();
    let mut guard = workspace.lock(LockScope::Workspace).unwrap();
    assert!(
        t.admit_human_binding(&guard, &binding).is_err(),
        "workspace barrier is required"
    );
    guard.upgrade_write_protocol().unwrap();
    assert!(
        t.admit_human_binding(&guard, &binding).is_err(),
        "project barrier is required too"
    );
    TextProjects::new(&fx.root)
        .upgrade_project_protocol(&fx.project, &guard)
        .unwrap();
    assert!(
        t.admit_human_binding(&guard, &old).is_err(),
        "same bytes cannot conceal another approval"
    );
    for mutation in 0..4 {
        let mut previous = resolved.source.clone();
        if let TranslationSource::Model {
            task,
            changes_version,
            event,
        } = &mut previous
        {
            match mutation {
                0 => event.index += 1,
                1 => event.hash = "0".repeat(64),
                2 => *task = first,
                _ => *changes_version = format!("sha256:{}", "0".repeat(64)),
            }
        }
        let bad = submission
            .prepare_binding(format!("bad-{mutation}"), HumanActor::Local, previous)
            .unwrap();
        assert!(t.admit_human_binding(&guard, &bad).is_err());
    }
    let foreign_root = fx.root.join("other-workspace");
    fs::create_dir(&foreign_root).unwrap();
    let foreign = Workspace::open(&foreign_root).unwrap();
    let foreign_guard = foreign.lock(LockScope::Workspace).unwrap();
    assert!(t.admit_human_binding(&foreign_guard, &binding).is_err());
    drop(foreign_guard);
    let log_bytes = fs::read(fx.root.join(".ziyuan/court.log")).unwrap();
    let generation = workspace.generation().unwrap();
    assert!(t.admit_human_binding(&guard, &binding).unwrap().is_some());
    assert_eq!(
        log_bytes,
        fs::read(fx.root.join(".ziyuan/court.log")).unwrap()
    );
    assert_eq!(fs::read(fx.zh_path(B1)).unwrap(), disk);
    assert_eq!(workspace.generation().unwrap(), generation);
    // An unrelated writer can append without the workspace lock. Publication
    // must read a fresh prefix rather than reuse the earlier admission/session.
    let mut log = EventLog::new(&fx.root);
    log.append(s.event_count(), &[ziyuan_core::CourtEvent::Clock(1)])
        .unwrap();
    let stale = submission
        .prepare_binding("stale".into(), HumanActor::Local, resolved.source.clone())
        .unwrap();
    assert!(t.admit_human_binding(&guard, &stale).unwrap().is_some());
    assert!(t.publish_human_binding(&guard, &binding).unwrap());
    let published = EventLog::new(&fx.root).load().unwrap();
    assert_eq!(
        published
            .iter()
            .filter(|event| matches!(event, ziyuan_core::CourtEvent::HumanSubmissionBound { .. }))
            .count(),
        1
    );
    assert_eq!(fs::read(fx.zh_path(B1)).unwrap(), disk);
    assert_eq!(workspace.generation().unwrap(), generation);
    drop(guard);
    t.set_zhupi(&fx.project, B1, "c0001-p0001", "后改材料")
        .unwrap();
    let guard = workspace.lock(LockScope::Workspace).unwrap();
    assert!(matches!(
        t.publish_human_binding(&guard, &stale),
        Err(TranslationError::DraftBasisChanged)
    ));
    assert_eq!(EventLog::new(&fx.root).load().unwrap(), published);
    drop(guard);
    fs::remove_file(fx.root.join(format!(
        ".ziyuan/text-projects/{}/human-drafts/{B1}.json",
        fx.project
    )))
    .unwrap();
    let mut guard = workspace.lock(LockScope::Workspace).unwrap();
    assert!(
        t.admit_human_binding(&guard, &binding).unwrap().is_none(),
        "durable receipt survives changed basis and lost drafts"
    );
    assert!(!t.publish_human_binding(&guard, &binding).unwrap());
    assert_eq!(EventLog::new(&fx.root).load().unwrap(), published);
    let mut web = binding.clone();
    web.request.actor = HumanActor::Web;
    web.request_hash = ziyuan_governance::human_binding_request_hash(&web.request);
    assert!(
        t.publish_human_binding(&guard, &web).is_err(),
        "authorization precedes receipt lookup"
    );
    assert!(
        t.publish_web_human_binding(&guard, &binding).is_err(),
        "the Web entry also rejects a mismatched channel before receipt lookup"
    );
    let mut conflict = binding.clone();
    conflict.request.submission_id.push('x');
    conflict.request_hash = ziyuan_governance::human_binding_request_hash(&conflict.request);
    assert!(matches!(
        t.publish_human_binding(&guard, &conflict),
        Err(TranslationError::DraftRequestConflict)
    ));
    guard.begin().unwrap();
    assert!(
        t.publish_human_binding(&guard, &binding).is_err(),
        "even receipt lookup refuses interrupted generations"
    );
    guard.finish().unwrap();
    assert_eq!(workspace.generation().unwrap(), generation + 2);
    assert_eq!(fs::read(fx.zh_path(B1)).unwrap(), disk);
    assert_eq!(EventLog::new(&fx.root).load().unwrap(), published);
}

#[test]
fn export_writes_materials_and_never_touches_zh_or_proposals() {
    let fx = Fx::new("export");
    let t = fx.t();
    let exported = t.export(&fx.project, B2).unwrap();
    let dir = t.block_dir(&fx.project, B2);
    let source = fx.block(B2).serialized().unwrap();
    assert_eq!(fs::read(dir.join("source.json")).unwrap(), source);
    assert_eq!(exported.source_hash, sha256_hex(&source));
    let p = t.project(&fx.project).unwrap();
    assert_eq!(block_source_hash(&p, B2).unwrap(), exported.source_hash);
    assert_eq!(exported.context_version, 0);
    // 原文、上下文、衔接段，以及（可为空的）朱批：定稿核对的就是这份逐段条目。
    assert_eq!(
        exported.files,
        [
            "source.json",
            "context.json",
            "neighbors.json",
            "zhupi.json"
        ]
        .map(|name| format!("translations/{}/{B2}/{name}", fx.project))
    );
    let zhupi: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("zhupi.json")).unwrap()).unwrap();
    assert_eq!(zhupi["items"], serde_json::json!([]));
    let context: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("context.json")).unwrap()).unwrap();
    assert_eq!(context["version"], 0);
    assert_eq!(context["glossary"], serde_json::json!([]));
    let neighbors: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("neighbors.json")).unwrap()).unwrap();
    assert_eq!(neighbors["previous"][0]["id"], "c0001-p0006");
    assert_eq!(neighbors["next"][0]["id"], "c0003-p0001");
    let first = t.export(&fx.project, B1).unwrap();
    let n1: serde_json::Value = serde_json::from_slice(
        &fs::read(t.block_dir(&fx.project, B1).join("neighbors.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(n1["previous"], serde_json::json!([]));
    assert_eq!(first.block, B1);

    fs::write(dir.join("zh.json"), "人手所改").unwrap();
    fs::create_dir_all(dir.join("proposals")).unwrap();
    fs::write(dir.join("proposals/terms.md"), "建议").unwrap();
    t.export(&fx.project, B2).unwrap();
    assert_eq!(fs::read_to_string(dir.join("zh.json")).unwrap(), "人手所改");
    assert_eq!(
        fs::read_to_string(dir.join("proposals/terms.md")).unwrap(),
        "建议"
    );
    assert!(
        t.export(&fx.project, "c0009-b0001").is_err(),
        "不属本项目的块"
    );
}

#[test]
fn context_snapshots_are_versioned_validated_and_verified() {
    let fx = Fx::new("context");
    let t = fx.t();
    assert_eq!(t.context_version(&fx.project).unwrap(), 0);
    assert!(t.current_context(&fx.project).unwrap().is_none());
    t.ensure_context_draft(&fx.project).unwrap();
    assert_eq!(t.approve_context(&fx.project).unwrap(), 1);
    assert_eq!(
        t.approve_context(&fx.project).unwrap(),
        1,
        "草稿未变不建新版"
    );
    fx.write_glossary(r#"[{"source":"勇者","allowed":["勇者"],"note":"统一"}]"#);
    assert_eq!(t.approve_context(&fx.project).unwrap(), 2);
    assert_eq!(t.context_versions(&fx.project).unwrap(), [1, 2]);
    assert!(
        t.context_snapshot(&fx.project, 1)
            .unwrap()
            .glossary
            .is_empty()
    );
    assert_eq!(
        t.current_context(&fx.project).unwrap().unwrap().glossary[0].allowed,
        ["勇者"]
    );

    for bad in [
        "不是 JSON",
        r#"{"source":"甲"}"#,
        r#"[{"source":"甲","allowed":[]}]"#,
        r#"[{"source":"","allowed":["乙"]}]"#,
        r#"[{"source":"甲","allowed":["乙"],"extra":1}]"#,
    ] {
        fx.write_glossary(bad);
        assert!(t.approve_context(&fx.project).is_err(), "{bad}");
    }
    fs::write(
        fx.root
            .join(context_dir_rel(&fx.project))
            .join(GLOSSARY_FILE),
        [0xff, 0xfe],
    )
    .unwrap();
    assert!(t.approve_context(&fx.project).is_err(), "非 UTF-8");
    assert_eq!(t.context_versions(&fx.project).unwrap(), [1, 2]);

    fx.write_glossary("[]");
    fx.write_summary("c0009", "无此章");
    assert!(t.approve_context(&fx.project).is_err(), "未知章的摘要");
    fs::remove_file(
        fx.root
            .join(context_dir_rel(&fx.project))
            .join("summaries/c0009.md"),
    )
    .unwrap();
    fx.write_summary("c0001", "前情：勇者出发。");
    assert_eq!(t.approve_context(&fx.project).unwrap(), 3);
    let current = t.current_context(&fx.project).unwrap().unwrap();
    assert_eq!(current.summaries["c0001"], "前情：勇者出发。");

    // 第二章可见第一章摘要，第一章不见自己的。
    t.export(&fx.project, B2).unwrap();
    let ctx: serde_json::Value = serde_json::from_slice(
        &fs::read(t.block_dir(&fx.project, B2).join("context.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(ctx["version"], 3);
    assert_eq!(ctx["summaries"]["c0001"], "前情：勇者出发。");
    t.export(&fx.project, B1).unwrap();
    let ctx: serde_json::Value = serde_json::from_slice(
        &fs::read(t.block_dir(&fx.project, B1).join("context.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(ctx["summaries"], serde_json::json!({}));

    // 快照被改动即拒用。
    let snap = t
        .store_dir(&fx.project)
        .unwrap()
        .join("context/v3/style.md");
    fs::write(&snap, "篡改").unwrap();
    assert!(t.context_snapshot(&fx.project, 3).is_err());
    assert!(t.context_snapshot(&fx.project, 2).is_ok());
}

#[test]
fn issue_translation_commits_edict_and_link_together() {
    let fx = Fx::new("issue");
    let mut s = fx.session();
    let edict = s.issue_translation(&fx.root, &fx.project, B1).unwrap();
    assert_eq!(edict.department, Department::Works);
    assert_eq!(
        edict.seal.capabilities,
        BTreeSet::from([Capability::ReadFile, Capability::WriteFile])
    );
    assert_eq!(
        edict.seal.scope,
        [format!("translations/{}/{B1}", fx.project)]
    );
    assert!(edict.text.contains("zh.json") && edict.text.contains("proposals/"));
    let history = s.history().unwrap();
    let tail = &history[history.len() - 2..];
    assert!(matches!(&tail[0], CourtEvent::EdictIssued(e) if e.id == edict.id));
    let p = fx.t().project(&fx.project).unwrap();
    assert_eq!(
        tail[1],
        CourtEvent::TranslationLinked {
            task: edict.id,
            project: fx.project.clone(),
            block: B1.into(),
            kind: TranslationKind::Translate,
            source_hash: block_source_hash(&p, B1).unwrap(),
            context_version: 0,
            output_path: output_rel(&fx.project, B1),
            zhupi_hash: fx.t().block_zhupi_hash(&p, B1).unwrap(),
        }
    );
    assert!(
        fx.t()
            .block_dir(&fx.project, B1)
            .join("source.json")
            .is_file()
    );
    let count = s.event_count();
    assert!(
        s.issue_translation(&fx.root, &fx.project, B1).is_err(),
        "执行中不重复下诏"
    );
    assert_eq!(s.event_count(), count);
    // 重开后关联仍在。
    let reopened = fx.session();
    assert_eq!(
        reopened
            .court()
            .task(edict.id)
            .unwrap()
            .translation
            .as_ref()
            .unwrap()
            .kind,
        TranslationKind::Translate
    );
}

#[test]
fn gate_accepts_a_valid_translation_and_proposals() {
    let fx = Fx::new("gate-ok");
    let mut s = fx.session();
    let edict = s.issue_translation(&fx.root, &fx.project, B1).unwrap();
    let after = fx.candidate(B1, |_| {});
    let proposal = FileChange {
        path: format!("{}terms.md", proposals_rel(&fx.project, B1)),
        before: None,
        after: "勇者 → 勇者\n".into(),
    };
    complete(
        &mut s,
        edict.id,
        vec![fx.zh_change(B1, after.clone()), proposal],
        "译毕",
    );
    s.apply_changes(edict.id, &fx.root, vec![]).unwrap();
    assert_eq!(fs::read_to_string(fx.zh_path(B1)).unwrap(), after);
    assert_eq!(
        s.court()
            .task(edict.id)
            .unwrap()
            .changes
            .as_ref()
            .unwrap()
            .state,
        ChangeState::Applied
    );
    assert_eq!(
        fx.status(&s, B1).state,
        BlockState::Applied {
            task: edict.id.0,
            next: NextStep::Review
        }
    );
}

#[test]
fn gate_rejects_bad_structure_paths_and_stale_versions() {
    let fx = Fx::new("gate-bad");
    let mut s = fx.session();
    type Files = Box<dyn Fn(&Fx) -> Vec<FileChange>>;
    let cases: Vec<(&str, Files, &str)> = vec![
        (
            B1,
            Box::new(|fx| {
                vec![fx.zh_change(
                    B1,
                    fx.candidate(B1, |ps| {
                        ps.remove(2);
                    }),
                )]
            }),
            "缺少",
        ),
        (
            B2,
            Box::new(|fx| vec![fx.zh_change(B2, fx.candidate(B2, |ps| ps.swap(0, 1)))]),
            "顺序",
        ),
        (
            B3,
            Box::new(|fx| {
                vec![
                    fx.zh_change(B3, fx.candidate(B3, |_| {})),
                    FileChange {
                        path: format!("translations/{}/{B3}/source.json", fx.project),
                        before: fs::read_to_string(
                            fx.t().block_dir(&fx.project, B3).join("source.json"),
                        )
                        .ok(),
                        after: "{}".into(),
                    },
                    FileChange {
                        path: "src/evil.rs".into(),
                        before: None,
                        after: "x".into(),
                    },
                ]
            }),
            "只可写",
        ),
    ];
    for (block, files, expect) in cases {
        let edict = s.issue_translation(&fx.root, &fx.project, block).unwrap();
        complete(&mut s, edict.id, files(&fx), "译毕");
        let count = s.event_count();
        let msg = apply_error(s.apply_changes(edict.id, &fx.root, vec![]));
        assert!(msg.contains(expect), "{block}: {msg}");
        assert!(!fx.zh_path(block).exists(), "{block}: 一处也不写");
        assert!(!fx.root.join("src/evil.rs").exists());
        assert_eq!(s.event_count(), count, "不留记录");
        assert_eq!(
            s.court()
                .task(edict.id)
                .unwrap()
                .changes
                .as_ref()
                .unwrap()
                .state,
            ChangeState::Proposed
        );
        assert_eq!(
            fx.status(&s, block).state,
            BlockState::AwaitingApproval { task: edict.id.0 }
        );
        s.discard_changes(edict.id).unwrap();
    }
}

#[test]
fn gate_rejects_changed_source_hash() {
    let fx = Fx::new("gate-source");
    let mut s = fx.session();
    fx.t().export(&fx.project, B1).unwrap();
    let link = TranslationLink {
        project: fx.project.clone(),
        block: B1.into(),
        kind: TranslationKind::Translate,
        source_hash: "0".repeat(64),
        context_version: 0,
        output_path: output_rel(&fx.project, B1),
        zhupi_hash: "0".repeat(64),
    };
    let seal = ziyuan_core::Seal {
        capabilities: [Capability::ReadFile, Capability::WriteFile].into(),
        scope: vec![block_dir_rel(&fx.project, B1)],
        mcp: Default::default(),
    };
    let committed = s
        .commit(|court| {
            let edict = court.new_edict(Department::Works, "译".into(), seal.clone())?;
            let issued = CourtEvent::EdictIssued(Box::new(edict.clone()));
            let mut next = court.clone();
            next.apply(&issued)?;
            Ok(vec![
                issued,
                next.propose_translation_link(edict.id, link.clone())?,
            ])
        })
        .unwrap();
    let CourtEvent::EdictIssued(edict) = &committed[0] else {
        panic!()
    };
    complete(
        &mut s,
        edict.id,
        vec![fx.zh_change(B1, fx.candidate(B1, |_| {}))],
        "译毕",
    );
    let msg = apply_error(s.apply_changes(edict.id, &fx.root, vec![]));
    assert!(msg.contains("原文已与下诏时不同"), "{msg}");
    assert!(!fx.zh_path(B1).exists());
}

#[test]
fn gate_rejects_a_stale_context_version() {
    let fx = Fx::new("gate-context");
    let mut s = fx.session();
    let edict = s.issue_translation(&fx.root, &fx.project, B1).unwrap();
    fx.t().ensure_context_draft(&fx.project).unwrap();
    assert_eq!(fx.t().approve_context(&fx.project).unwrap(), 1);
    complete(
        &mut s,
        edict.id,
        vec![fx.zh_change(B1, fx.candidate(B1, |_| {}))],
        "译毕",
    );
    let msg = apply_error(s.apply_changes(edict.id, &fx.root, vec![]));
    assert!(msg.contains("版本失效") && msg.contains("v1"), "{msg}");
    assert!(!fx.zh_path(B1).exists());
}

#[test]
fn gate_checks_bytes_after_partial_hunk_selection() {
    let fx = Fx::new("gate-hunks");
    let mut s = fx.session();
    let edict = s.issue_translation(&fx.root, &fx.project, B1).unwrap();
    // 磁盘上的旧稿缺第一段、末段待改；新稿补上第一段并改末段：两段相隔甚远，各成一段 hunk。
    let before = fx.candidate(B1, |ps| {
        ps.remove(0);
        ps.last_mut().unwrap().text = "旧".into();
    });
    fs::write(fx.zh_path(B1), &before).unwrap();
    let change = fx.zh_change(B1, fx.candidate(B1, |_| {}));
    assert_eq!(change.hunks().len(), 2);
    complete(&mut s, edict.id, vec![change.clone()], "译毕");
    let msg = apply_error(s.apply_changes(edict.id, &fx.root, vec![1]));
    assert!(msg.contains("c0001-p0001") && msg.contains("缺少"), "{msg}");
    assert_eq!(fs::read_to_string(fx.zh_path(B1)).unwrap(), before);
    s.apply_changes(edict.id, &fx.root, vec![]).unwrap();
    assert_eq!(fs::read_to_string(fx.zh_path(B1)).unwrap(), change.after);
}

#[test]
fn review_requires_an_applied_translation_and_is_read_only() {
    let fx = Fx::new("review");
    let mut s = fx.session();
    assert!(
        s.issue_review(&fx.root, &fx.project, B1).is_err(),
        "尚无初译"
    );
    let task = translated(&fx, &mut s, B1);
    assert!(s.issue_review(&fx.root, &fx.project, B1).is_err(), "待准奏");
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    let count = s.event_count();
    let edicts = s.issue_review(&fx.root, &fx.project, B1).unwrap();
    assert_eq!(edicts.len(), 2);
    let written = s.event_count() - count;
    assert!(written == 4 || written == 5, "两件诏令与两条关联同一提交");
    let kinds: Vec<_> = edicts
        .iter()
        .map(|e| {
            assert_eq!(e.department, Department::Rites);
            assert_eq!(e.seal.capabilities, BTreeSet::from([Capability::ReadFile]));
            assert_eq!(e.seal.scope, [block_dir_rel(&fx.project, B1)]);
            let link = s.court().task(e.id).unwrap().translation.clone().unwrap();
            assert!(link.output_path.is_empty());
            link.kind
        })
        .collect();
    assert_eq!(
        kinds,
        [
            TranslationKind::ReviewFidelity,
            TranslationKind::ReviewStyle
        ]
    );
    assert!(
        matches!(fx.status(&s, B1).state, BlockState::Running { ref tasks } if tasks.len() == 2)
    );
    assert!(
        s.issue_review(&fx.root, &fx.project, B1).is_err(),
        "审校未结不重复"
    );
}

#[test]
fn revise_exports_zhupi_and_reviews_then_awaits_yulan() {
    let fx = Fx::new("revise");
    let mut s = fx.session();
    let task = translated(&fx, &mut s, B1);
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    fx.t()
        .set_zhupi(&fx.project, B1, "c0001-p0002", "改得口语些")
        .unwrap();
    let reviews = s.issue_review(&fx.root, &fx.project, B1).unwrap();
    complete(
        &mut s,
        reviews[0].id,
        vec![],
        "忠实：c0001-p0001 漏译「の」",
    );
    complete(&mut s, reviews[1].id, vec![], "文风：c0001-p0003 生硬");
    assert_eq!(
        fx.status(&s, B1).state,
        BlockState::Applied {
            task: task.0,
            next: NextStep::Revise
        }
    );
    let revise = s.issue_revise(&fx.root, &fx.project, B1).unwrap();
    assert_eq!(revise.department, Department::Works);
    assert!(revise.text.contains("朱批优先"));
    let dir = fx.t().block_dir(&fx.project, B1);
    let zhupi = fs::read_to_string(dir.join("zhupi.json")).unwrap();
    assert!(
        zhupi.contains("改得口语些") && zhupi.contains("c0001-p0002"),
        "{zhupi}"
    );
    let reviews_json = fs::read_to_string(dir.join("reviews.json")).unwrap();
    assert!(reviews_json.contains("漏译") && reviews_json.contains("生硬"));
    let link = s
        .court()
        .task(revise.id)
        .unwrap()
        .translation
        .clone()
        .unwrap();
    assert_eq!(link.kind, TranslationKind::Revise);
    assert_eq!(link.output_path, output_rel(&fx.project, B1));

    let after = fx.candidate(B1, |ps| ps[1].text = "改后".into());
    complete(
        &mut s,
        revise.id,
        vec![fx.zh_change(B1, after)],
        "修订毕；未采纳：无",
    );
    s.apply_changes(revise.id, &fx.root, vec![]).unwrap();
    assert_eq!(
        fx.status(&s, B1).state,
        BlockState::AwaitingYulan { task: revise.id.0 }
    );
}

#[test]
fn zhupi_is_versioned_and_checks_paragraphs() {
    let fx = Fx::new("zhupi");
    let t = fx.t();
    assert_eq!(t.zhupi_version(&fx.project).unwrap(), 0);
    assert_eq!(
        t.set_zhupi(&fx.project, B1, "c0001-p0001", "甲").unwrap(),
        1
    );
    assert_eq!(
        t.set_zhupi(&fx.project, B1, "c0001-p0001", "甲").unwrap(),
        1
    );
    assert_eq!(
        t.set_zhupi(&fx.project, B1, "c0001-p0002", "乙").unwrap(),
        2
    );
    assert_eq!(
        t.set_zhupi(&fx.project, B1, "c0001-p0001", "丙").unwrap(),
        3
    );
    let z = t.zhupi(&fx.project, B1).unwrap();
    assert_eq!(z.version, 3);
    assert_eq!(z.items["c0001-p0001"].text, "丙");
    assert_eq!(t.clear_zhupi(&fx.project, B1, "c0001-p0001").unwrap(), 4);
    assert_eq!(t.clear_zhupi(&fx.project, B1, "c0001-p0001").unwrap(), 4);
    assert_eq!(t.zhupi(&fx.project, B1).unwrap().items.len(), 1);
    assert!(t.zhupi(&fx.project, B2).unwrap().items.is_empty());
    assert!(t.set_zhupi(&fx.project, B1, "c0002-p0001", "别块").is_err());
    assert!(t.set_zhupi(&fx.project, B1, "c0001-p0099", "无").is_err());
    assert!(t.set_zhupi(&fx.project, B1, "c0001-p0001", "  ").is_err());
    assert!(
        t.set_zhupi(&fx.project, "c0009-b0001", "c0009-p0001", "无")
            .is_err()
    );
    assert_eq!(t.zhupi_version(&fx.project).unwrap(), 4);
}

#[test]
fn a_failed_revise_does_not_stall_the_block() {
    let fx = Fx::new("failed-revise");
    let mut s = fx.session();
    let task = translated(&fx, &mut s, B1);
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    let revise = s.issue_revise(&fx.root, &fx.project, B1).unwrap();
    // 呈报改动后才失败：改动无处准奏（治理层只准已完成的诏令），不得因此卡住块。
    let worker = s
        .court()
        .route(&s.court().task(revise.id).unwrap().edict)
        .unwrap()
        .worker;
    let half = fx.candidate(B1, |ps| ps[0].text = "半成品".into());
    s.commit(|_| {
        Ok(vec![
            CourtEvent::TaskAssigned {
                task: revise.id,
                worker,
                model: None,
            },
            CourtEvent::ChangesProposed {
                task: revise.id,
                changes: vec![fx.zh_change(B1, half.clone())],
            },
            CourtEvent::TaskFailed {
                task: revise.id,
                reason: "模型不可用".into(),
            },
        ])
    })
    .unwrap();
    assert_ne!(fs::read_to_string(fx.zh_path(B1)).unwrap(), half);
    let status = fx.status(&s, B1);
    assert_eq!(
        status.state,
        BlockState::Applied {
            task: task.0,
            next: NextStep::Review
        }
    );
    assert!(status.note.unwrap().contains("未成"));
    // 修订未成：可再下修订、下审校，也可御览定稿。
    let again = s.issue_revise(&fx.root, &fx.project, B1).unwrap();
    s.cancel(again.id).unwrap();
    let reviews = s.issue_review(&fx.root, &fx.project, B1).unwrap();
    for edict in &reviews {
        complete(&mut s, edict.id, vec![], "无可指摘");
    }
    let hash = s.finalize_translation(&fx.root, &fx.project, B1).unwrap();
    assert_eq!(
        fx.status(&s, B1).state,
        BlockState::Finalized {
            task: task.0,
            translation_hash: hash
        }
    );
}

#[test]
fn a_revise_that_leaves_zh_json_alone_keeps_the_translation() {
    let fx = Fx::new("revise-no-change");
    let mut s = fx.session();
    let task = translated(&fx, &mut s, B1);
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    let kept = fs::read_to_string(fx.zh_path(B1)).unwrap();
    let revise = s.issue_revise(&fx.root, &fx.project, B1).unwrap();
    // 只补建议、不改译稿。
    complete(
        &mut s,
        revise.id,
        vec![FileChange {
            path: format!("{}terms.json", proposals_rel(&fx.project, B1)),
            before: None,
            after: "{}\n".into(),
        }],
        "译稿无需改",
    );
    s.apply_changes(revise.id, &fx.root, vec![]).unwrap();
    assert_eq!(fs::read_to_string(fx.zh_path(B1)).unwrap(), kept);
    assert_eq!(
        fx.status(&s, B1).state,
        BlockState::AwaitingYulan { task: revise.id.0 }
    );
    let hash = s.finalize_translation(&fx.root, &fx.project, B1).unwrap();
    assert_eq!(hash, sha256_hex(kept.as_bytes()));
}

#[test]
fn revise_exports_only_reviews_after_the_current_translation() {
    let fx = Fx::new("revise-rounds");
    let mut s = fx.session();
    let task = translated(&fx, &mut s, B1);
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    let reviews = s.issue_review(&fx.root, &fx.project, B1).unwrap();
    complete(&mut s, reviews[0].id, vec![], "忠实：c0001-p0002 漏译");
    complete(&mut s, reviews[1].id, vec![], "文风：c0001-p0003 生硬");
    let revise = s.issue_revise(&fx.root, &fx.project, B1).unwrap();
    let reviews_json =
        || fs::read_to_string(fx.t().block_dir(&fx.project, B1).join("reviews.json")).unwrap();
    assert!(reviews_json().contains("漏译"));
    complete(
        &mut s,
        revise.id,
        vec![fx.zh_change(B1, fx.candidate(B1, |ps| ps[1].text = "改后".into()))],
        "修订毕",
    );
    s.apply_changes(revise.id, &fx.root, vec![]).unwrap();
    // 上一轮的审校意见不再交给下一轮修订。
    let next = s.issue_revise(&fx.root, &fx.project, B1).unwrap();
    assert!(
        !reviews_json().contains("漏译") && !reviews_json().contains("生硬"),
        "{}",
        reviews_json()
    );
    s.cancel(next.id).unwrap();
}

#[test]
fn exporting_for_review_drops_the_previous_review_replies() {
    let fx = Fx::new("revise-cleanup");
    let mut s = fx.session();
    let task = translated(&fx, &mut s, B1);
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    let revise = s.issue_revise(&fx.root, &fx.project, B1).unwrap();
    let dir = fx.t().block_dir(&fx.project, B1);
    assert!(dir.join("zhupi.json").is_file() && dir.join("reviews.json").is_file());
    // 修订未结：换下审校时不再带上一轮的审校回奏；朱批仍随材料导出。
    s.cancel(revise.id).unwrap();
    s.issue_review(&fx.root, &fx.project, B1).unwrap();
    assert!(dir.join("zhupi.json").is_file() && !dir.join("reviews.json").exists());
}

#[test]
fn zhupi_written_before_the_first_translation_reaches_the_model() {
    let fx = Fx::new("zhupi-early");
    let mut s = fx.session();
    fx.t()
        .set_zhupi(&fx.project, B1, "c0001-p0002", "此处用敬语")
        .unwrap();
    let edict = s.issue_translation(&fx.root, &fx.project, B1).unwrap();
    assert!(edict.text.contains("zhupi.json"));
    let dir = fx.t().block_dir(&fx.project, B1);
    let zhupi = fs::read_to_string(dir.join("zhupi.json")).unwrap();
    assert!(
        zhupi.contains("此处用敬语") && zhupi.contains("c0001-p0002"),
        "{zhupi}"
    );
    // 下诏后添的朱批不在模型所见之内，御览会拒签。
    complete(
        &mut s,
        edict.id,
        vec![fx.zh_change(B1, fx.candidate(B1, |_| {}))],
        "译毕",
    );
    s.apply_changes(edict.id, &fx.root, vec![]).unwrap();
    fx.t()
        .set_zhupi(&fx.project, B1, "c0001-p0003", "再短些")
        .unwrap();
    let err = s
        .finalize_translation(&fx.root, &fx.project, B1)
        .unwrap_err();
    assert!(err.to_string().contains("朱批"), "{err}");
}

#[test]
fn review_refuses_a_hand_edited_translation() {
    let fx = Fx::new("review-edited");
    let mut s = fx.session();
    let task = translated(&fx, &mut s, B1);
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    fs::write(
        fx.zh_path(B1),
        fx.candidate(B1, |ps| ps[0].text = "人改".into()),
    )
    .unwrap();
    let err = s.issue_review(&fx.root, &fx.project, B1).unwrap_err();
    assert!(err.to_string().contains("不同"), "{err}");
}

#[test]
fn finalize_refuses_while_a_revise_awaits_approval() {
    let fx = Fx::new("finalize-pending");
    let mut s = fx.session();
    let task = translated(&fx, &mut s, B1);
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    let revise = s.issue_revise(&fx.root, &fx.project, B1).unwrap();
    complete(
        &mut s,
        revise.id,
        vec![fx.zh_change(B1, fx.candidate(B1, |ps| ps[0].text = "改后".into()))],
        "修订毕",
    );
    let err = s
        .finalize_translation(&fx.root, &fx.project, B1)
        .unwrap_err();
    assert!(err.to_string().contains("待准奏"), "{err}");
}

#[test]
fn a_finalized_block_takes_no_new_writer_until_reopened() {
    let fx = Fx::new("finalized-locked");
    let mut s = fx.session();
    let task = translated(&fx, &mut s, B1);
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    s.finalize_translation(&fx.root, &fx.project, B1).unwrap();
    for err in [
        s.issue_revise(&fx.root, &fx.project, B1).unwrap_err(),
        s.issue_translation(&fx.root, &fx.project, B1).unwrap_err(),
    ] {
        assert!(err.to_string().contains("退回"), "{err}");
    }
    s.reopen_translation(&fx.root, &fx.project, B1, "再润色")
        .unwrap();
    let revise = s.issue_revise(&fx.root, &fx.project, B1).unwrap();
    s.cancel(revise.id).unwrap();
}
#[test]
fn finalize_requires_the_applied_translation_and_reopen_returns_it() {
    let fx = Fx::new("finalize");
    let mut s = fx.session();
    assert!(s.finalize_translation(&fx.root, &fx.project, B1).is_err());
    // 朱批须在下诏前写好，才会被初译/修订采纳。
    fx.t()
        .set_zhupi(&fx.project, B1, "c0001-p0001", "好")
        .unwrap();
    let task = translated(&fx, &mut s, B1);
    assert!(
        s.finalize_translation(&fx.root, &fx.project, B1).is_err(),
        "未准奏"
    );
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    let good = fs::read_to_string(fx.zh_path(B1)).unwrap();
    fs::write(
        fx.zh_path(B1),
        fx.candidate(B1, |ps| ps[0].text = "人改".into()),
    )
    .unwrap();
    let err = s
        .finalize_translation(&fx.root, &fx.project, B1)
        .unwrap_err();
    assert!(err.to_string().contains("不同"), "{err}");
    fs::write(fx.zh_path(B1), &good).unwrap();
    // 下诏后才添朱批：御览时拒签，须先修订。
    fx.t()
        .set_zhupi(&fx.project, B1, "c0001-p0002", "再口语些")
        .unwrap();
    let err = s
        .finalize_translation(&fx.root, &fx.project, B1)
        .unwrap_err();
    assert!(err.to_string().contains("朱批"), "{err}");
    fx.t().clear_zhupi(&fx.project, B1, "c0001-p0002").unwrap();
    let hash = s.finalize_translation(&fx.root, &fx.project, B1).unwrap();
    assert_eq!(hash, sha256_hex(good.as_bytes()));
    assert_eq!(
        s.court().translation_signoff(&fx.project, B1),
        Some(&ziyuan_governance::Signoff::Finalized {
            translation_hash: hash.clone(),
            context_version: 0,
            zhupi_version: 3,
        })
    );
    assert_eq!(
        fx.status(&s, B1).state,
        BlockState::Finalized {
            task: task.0,
            translation_hash: hash
        }
    );
    // 定稿后人手改稿：定稿失效。
    fs::write(
        fx.zh_path(B1),
        fx.candidate(B1, |ps| ps[0].text = "人改".into()),
    )
    .unwrap();
    assert_eq!(
        fx.status(&s, B1).state,
        BlockState::Stale {
            task: task.0,
            reasons: vec![StaleReason::TranslationEdited]
        }
    );
    fs::write(fx.zh_path(B1), &good).unwrap();

    assert!(
        s.reopen_translation(&fx.root, &fx.project, B1, " ")
            .is_err()
    );
    s.reopen_translation(&fx.root, &fx.project, B1, "第二段太硬")
        .unwrap();
    let status = fx.status(&s, B1);
    assert_eq!(
        status.state,
        BlockState::Applied {
            task: task.0,
            next: NextStep::Revise
        }
    );
    assert!(status.note.unwrap().contains("第二段太硬"));
}

#[test]
fn status_reports_each_state() {
    let fx = Fx::new("status");
    let mut s = fx.session();
    let all = s.translation_status(&fx.root, &fx.project).unwrap();
    assert_eq!(
        all.iter().map(|b| b.block.as_str()).collect::<Vec<_>>(),
        [B1, B2, B3]
    );
    assert!(all.iter().all(|b| b.state == BlockState::NotStarted));

    let running = s.issue_translation(&fx.root, &fx.project, B1).unwrap();
    let awaiting = translated(&fx, &mut s, B2);
    fs::create_dir_all(fx.t().block_dir(&fx.project, B3)).unwrap();
    fs::write(fx.zh_path(B3), "手写").unwrap();
    assert_eq!(
        fx.status(&s, B1).state,
        BlockState::Running {
            tasks: vec![running.id.0]
        }
    );
    assert_eq!(fx.status(&s, B1).tasks, [running.id.0]);
    assert_eq!(
        fx.status(&s, B2).state,
        BlockState::AwaitingApproval { task: awaiting.0 }
    );
    assert!(matches!(
        fx.status(&s, B3).state,
        BlockState::Uncertain { .. }
    ));

    s.cancel(running.id).unwrap();
    assert_eq!(
        fx.status(&s, B1).state,
        BlockState::Cancelled { task: running.id.0 }
    );
    s.discard_changes(awaiting).unwrap();
    assert!(matches!(
        fx.status(&s, B2).state,
        BlockState::Failed { ref reason, .. } if reason.contains("驳回")
    ));
    let failing = s.issue_translation(&fx.root, &fx.project, B2).unwrap();
    s.commit(|_| {
        Ok(vec![CourtEvent::TaskFailed {
            task: failing.id,
            reason: "模型不可用".into(),
        }])
    })
    .unwrap();
    assert_eq!(
        fx.status(&s, B2).state,
        BlockState::Failed {
            task: failing.id.0,
            reason: "模型不可用".into()
        }
    );
}

#[test]
fn status_marks_stale_after_context_summary_and_disk_changes() {
    let fx = Fx::new("stale");
    let mut s = fx.session();
    fx.write_summary("c0001", "前情甲");
    assert_eq!(fx.t().approve_context(&fx.project).unwrap(), 1);
    for block in [B1, B2] {
        let task = translated(&fx, &mut s, block);
        s.apply_changes(task, &fx.root, vec![]).unwrap();
    }
    assert!(matches!(
        fx.status(&s, B2).state,
        BlockState::Applied { .. }
    ));

    fx.write_summary("c0001", "前情乙");
    assert_eq!(fx.t().approve_context(&fx.project).unwrap(), 2);
    let BlockState::Stale { reasons, .. } = fx.status(&s, B2).state else {
        panic!("应失效")
    };
    assert_eq!(
        reasons,
        [
            StaleReason::ContextChanged {
                used: 1,
                current: 2
            },
            StaleReason::SummaryChanged {
                chapters: vec!["c0001".into()]
            }
        ]
    );
    let BlockState::Stale { reasons, .. } = fx.status(&s, B1).state else {
        panic!("应失效")
    };
    assert_eq!(
        reasons,
        [StaleReason::ContextChanged {
            used: 1,
            current: 2
        }]
    );

    // 磁盘上的译稿被改或删。
    let fx = Fx::new("stale-disk");
    let mut s = fx.session();
    let task = translated(&fx, &mut s, B1);
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    fs::write(fx.zh_path(B1), "人改").unwrap();
    assert_eq!(
        fx.status(&s, B1).state,
        BlockState::Stale {
            task: task.0,
            reasons: vec![StaleReason::TranslationEdited]
        }
    );
    fs::remove_file(fx.zh_path(B1)).unwrap();
    assert_eq!(
        fx.status(&s, B1).state,
        BlockState::Stale {
            task: task.0,
            reasons: vec![StaleReason::TranslationMissing]
        }
    );
}

#[test]
fn resume_skips_running_and_awaiting_and_never_duplicates() {
    let fx = Fx::new("resume");
    let mut s = fx.session();
    let running = s.issue_translation(&fx.root, &fx.project, B1).unwrap();
    let awaiting = translated(&fx, &mut s, B2);
    let outcome = s
        .resume_translation(&fx.root, &fx.project, ResumeOptions::default())
        .unwrap();
    assert_eq!(outcome.errors, []);
    assert_eq!(outcome.issued.len(), 1);
    assert_eq!(outcome.issued[0].0, B3);
    let again = s
        .resume_translation(&fx.root, &fx.project, ResumeOptions::default())
        .unwrap();
    assert_eq!(again, ResumeOutcome::default(), "不重复下诏");
    assert_eq!(s.court().translation_tasks(&fx.project, B1).count(), 1);
    assert_eq!(s.court().translation_tasks(&fx.project, B2).count(), 1);

    s.cancel(TaskId(outcome.issued[0].1)).unwrap();
    let skipped = s
        .resume_translation(&fx.root, &fx.project, ResumeOptions::default())
        .unwrap();
    assert!(skipped.issued.is_empty(), "撤销的须明示才重做");
    let retried = s
        .resume_translation(
            &fx.root,
            &fx.project,
            ResumeOptions {
                include_failed: true,
                include_stale: false,
            },
        )
        .unwrap();
    assert_eq!(
        retried
            .issued
            .iter()
            .map(|(b, _)| b.as_str())
            .collect::<Vec<_>>(),
        [B3]
    );
    assert!(s.court().task(running.id).unwrap().status.is_open());
    assert!(matches!(
        s.court().task(awaiting).unwrap().status,
        TaskStatus::Completed { .. }
    ));
}

#[test]
fn resume_plan_only_takes_flagged_states() {
    let status = |block: &str, state| BlockStatus {
        block: block.into(),
        chapter: "c0001".into(),
        state,
        tasks: vec![],
        note: None,
    };
    let all = [
        status("a", BlockState::NotStarted),
        status("b", BlockState::Running { tasks: vec![1] }),
        status("c", BlockState::AwaitingApproval { task: 2 }),
        status(
            "d",
            BlockState::Failed {
                task: 3,
                reason: "x".into(),
            },
        ),
        status("e", BlockState::Cancelled { task: 4 }),
        status(
            "f",
            BlockState::Stale {
                task: 5,
                reasons: vec![StaleReason::SourceChanged],
            },
        ),
        status("g", BlockState::Uncertain { reason: "?".into() }),
        status("h", BlockState::AwaitingYulan { task: 6 }),
        status(
            "i",
            BlockState::Finalized {
                task: 7,
                translation_hash: "h".into(),
            },
        ),
    ];
    assert_eq!(resume_plan(&all, ResumeOptions::default()), ["a"]);
    assert_eq!(
        resume_plan(
            &all,
            ResumeOptions {
                include_failed: true,
                include_stale: true
            }
        ),
        ["a", "d", "e", "f"]
    );
    assert_eq!(
        resume_plan(
            &all,
            ResumeOptions {
                include_failed: false,
                include_stale: true
            }
        ),
        ["a", "f"]
    );
}

/// 块的「撤销」与诏令状态同名：Web、CLI 的诏令都说「已撤销」。
#[test]
fn cancelled_block_label_matches_task_status() {
    assert_eq!(BlockState::Cancelled { task: 1 }.label(), "已撤销");
}

#[test]
fn finished_export_requires_effective_signoffs_and_preserves_project_order() {
    let fx = Fx::new("finished-export");
    let mut s = fx.session();
    let options = ExportOptions {
        chapters: vec!["c0003".into(), "c0001".into()],
        ..Default::default()
    };
    let err = s
        .export_finished(&fx.root, &fx.project, &options)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(B1) && err.contains(B3) && !err.contains(B2),
        "{err}"
    );
    for block in [B1, B3] {
        let task = translated(&fx, &mut s, block);
        s.apply_changes(task, &fx.root, vec![]).unwrap();
        // Approved is not finalized.
        assert!(s.export_finished(&fx.root, &fx.project, &options).is_err());
        s.finalize_translation(&fx.root, &fx.project, block)
            .unwrap();
    }
    let events = s.history().unwrap().to_vec();
    let first = s.export_finished(&fx.root, &fx.project, &options).unwrap();
    assert_eq!(
        first
            .files
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["c0001.txt", "c0003.txt"]
    );
    assert_eq!(
        String::from_utf8(first.files[1].bytes.clone()).unwrap(),
        "c3.txt\n\n译：三章。\n\n"
    );
    assert_eq!(first.manifest["chapters"][0]["blocks"][0]["task"], 1);
    assert_eq!(s.history().unwrap(), events);
    let merged = s
        .export_finished(
            &fx.root,
            &fx.project,
            &ExportOptions {
                layout: ExportLayout::Book,
                ..options.clone()
            },
        )
        .unwrap();
    assert_eq!(merged.files[0].name, "book.txt");
    assert_eq!(
        merged.files[0].bytes,
        [
            first.files[0].bytes.as_slice(),
            b"\n",
            first.files[1].bytes.as_slice()
        ]
        .concat()
    );
    let hash = first.manifest["files"][0]["sha256"].as_str().unwrap();
    assert_eq!(hash, sha256_hex(&first.files[0].bytes));

    // A still-finalized event cannot override a later annotation, context or disk edit.
    fx.t()
        .set_zhupi(&fx.project, B1, "c0001-p0001", "新要求")
        .unwrap();
    let err = s
        .export_finished(&fx.root, &fx.project, &options)
        .unwrap_err()
        .to_string();
    assert!(err.contains("朱批"), "{err}");
    fx.t().clear_zhupi(&fx.project, B1, "c0001-p0001").unwrap();
    assert!(s.export_finished(&fx.root, &fx.project, &options).is_ok());
    let good = fs::read(fx.zh_path(B1)).unwrap();
    fs::write(
        fx.zh_path(B1),
        fx.candidate(B1, |ps| ps[0].text = "改稿".into()),
    )
    .unwrap();
    assert!(
        s.export_finished(&fx.root, &fx.project, &options)
            .unwrap_err()
            .to_string()
            .contains("版本失效")
    );
    fs::write(fx.zh_path(B1), good).unwrap();
    s.reopen_translation(&fx.root, &fx.project, B3, "重审")
        .unwrap();
    assert!(s.export_finished(&fx.root, &fx.project, &options).is_err());
    fx.t().ensure_context_draft(&fx.project).unwrap();
    fx.t().approve_context(&fx.project).unwrap();
    assert!(
        s.export_finished(
            &fx.root,
            &fx.project,
            &ExportOptions {
                chapters: vec!["c0001".into()],
                ..Default::default()
            }
        )
        .unwrap_err()
        .to_string()
        .contains("版本失效")
    );
}

#[test]
fn finished_markdown_reinserts_approved_text_without_activating_markup() {
    use crate::text_project::{ImportOptions, ImportSource, ParseOptions};
    let mut fx = Fx::new("markdown-export");
    let t = fx.t();
    let project = t
        .texts()
        .create(t.project(&fx.project).unwrap().metadata)
        .unwrap();
    fx.project = project.id;
    let source = "\u{feff}序文。\r\n\r\n## 二章\r\n\r\n｜紫垣《しえん》の文。\r\n\r\n```js\r\nalert('not translated');\r\n```\r\n\r\n---\r\n\r\n### 小節\r\n\r\n- 項目一\r\n- 項目二\r\n\r\n> 引用\r\n\r\n## 一章\r\n\r\n終 `tail` の文。\r\n";
    let sources = [ImportSource::Text {
        name: "ordered.md".into(),
        text: source.into(),
    }];
    let opts = ImportOptions {
        parsing: ParseOptions {
            ruby: true,
            heading_level: Some(2),
        },
        ..Default::default()
    };
    let preview = t
        .texts()
        .preview_import(&fx.project, &sources, &opts)
        .unwrap();
    let project = t
        .texts()
        .import_prepared(&fx.project, &sources, &opts, &preview.fingerprint)
        .unwrap();
    let mut s = fx.session();
    let attack = "![画](https://invalid.example) <script>alert(1)</script> &copy;\n    # 不是代码";
    for c in &project.chapters {
        for b in &c.blocks {
            let e = s
                .issue_translation(&fx.root, &fx.project, &b.block_id)
                .unwrap();
            let mut candidate = b.clone();
            for p in &mut candidate.paragraphs {
                p.text = attack.into();
                p.inlines.clear();
            }
            let change = fx.zh_change(
                &b.block_id,
                String::from_utf8(candidate.serialized().unwrap()).unwrap(),
            );
            complete(&mut s, e.id, vec![change], "完成");
            s.apply_changes(e.id, &fx.root, vec![]).unwrap();
            s.finalize_translation(&fx.root, &fx.project, &b.block_id)
                .unwrap();
        }
    }
    let originals = t
        .export_originals(&fx.project, &["c0003".into(), "c0002".into()])
        .unwrap();
    assert_eq!(originals.files.len(), 1);
    assert_eq!(originals.files[0].bytes, source.as_bytes());
    assert_eq!(originals.manifest["chapters"][0]["chapter"], "c0002");
    let exported = s
        .export_finished(
            &fx.root,
            &fx.project,
            &ExportOptions {
                chapters: vec!["c0002".into(), "c0001".into()],
                format: ExportFormat::Markdown,
                ..Default::default()
            },
        )
        .unwrap();
    let md = String::from_utf8(exported.files[1].bytes.clone()).unwrap();
    assert!(md.starts_with("# 二章\n\n"), "{md}");
    assert!(md.contains("```js\nalert('not translated');\n```"), "{md}");
    assert!(md.contains("\n---\n"), "{md}");
    assert!(md.contains("### "), "{md}");
    assert!(!md.contains("しえん") && !md.contains("<ruby>"), "{md}");
    let mut code_blocks = 0;
    for e in pulldown_cmark::Parser::new(&md) {
        use pulldown_cmark::{Event, Tag};
        match e {
            Event::Start(Tag::Link { .. } | Tag::Image { .. })
            | Event::Html(_)
            | Event::InlineHtml(_) => panic!("activated translated markup: {e:?}\n{md}"),
            Event::Start(Tag::CodeBlock(_)) => code_blocks += 1,
            _ => {}
        }
    }
    assert_eq!(code_blocks, 1, "{md}");
    let txt = s
        .export_finished(&fx.root, &fx.project, &ExportOptions::default())
        .unwrap();
    assert!(
        !String::from_utf8(txt.files[1].bytes.clone())
            .unwrap()
            .contains("not translated")
    );
    assert!(txt.warnings.iter().any(|w| w.contains("TXT")));
    let err = s
        .export_finished(
            &fx.root,
            &fx.project,
            &ExportOptions {
                format: ExportFormat::Markdown,
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(err.to_string().contains("行内代码"), "{err}");
}

#[test]
fn finished_export_uses_only_approved_target_annotations() {
    use crate::text_project::Inline;
    let fx = Fx::new("target-ruby");
    let mut s = fx.session();
    let task = s.issue_translation(&fx.root, &fx.project, B3).unwrap();
    let candidate = fx.candidate(B3, |ps| {
        ps[0].text = "甲<乙（保留）".into();
        ps[0].inlines = vec![
            Inline::Ruby {
                base: "甲<乙".into(),
                reading: "a&b[link](x)".into(),
                byte_start: 0,
                byte_end: 0,
            },
            Inline::Text {
                text: "（保留）".into(),
            },
        ];
    });
    complete(&mut s, task.id, vec![fx.zh_change(B3, candidate)], "注音");
    s.apply_changes(task.id, &fx.root, vec![]).unwrap();
    s.finalize_translation(&fx.root, &fx.project, B3).unwrap();
    let opts = ExportOptions {
        chapters: vec!["c0003".into()],
        ..Default::default()
    };
    let txt = s.export_finished(&fx.root, &fx.project, &opts).unwrap();
    assert_eq!(
        String::from_utf8(txt.files[0].bytes.clone()).unwrap(),
        "c3.txt\n\n｜甲<乙《a&b[link](x)》（保留）\n\n"
    );
    let md = s
        .export_finished(
            &fx.root,
            &fx.project,
            &ExportOptions {
                format: ExportFormat::Markdown,
                ..opts
            },
        )
        .unwrap();
    assert!(
        String::from_utf8(md.files[0].bytes.clone())
            .unwrap()
            .contains("<ruby>甲&lt;乙<rt>a&amp;b&#91;link&#93;&#40;x&#41;</rt></ruby>（保留）")
    );
}

#[test]
fn finished_export_refuses_a_change_during_collection() {
    use std::cell::Cell;
    use ziyuan_storage::{EventStore, StorageError};
    struct ChangingStore {
        events: Vec<CourtEvent>,
        reads: Cell<usize>,
        path: PathBuf,
    }
    impl EventStore for ChangingStore {
        fn load(&self) -> std::result::Result<Vec<CourtEvent>, StorageError> {
            self.reads.set(self.reads.get() + 1);
            if self.reads.get() == 2 {
                fs::write(&self.path, b"changed during export").unwrap();
            }
            Ok(self.events.clone())
        }
        fn append(&mut self, _: usize, _: &[CourtEvent]) -> std::result::Result<(), StorageError> {
            panic!("export must not write events")
        }
    }
    let fx = Fx::new("export-snapshot-race");
    let mut s = fx.session();
    let task = translated(&fx, &mut s, B3);
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    s.finalize_translation(&fx.root, &fx.project, B3).unwrap();
    let history = s.history().unwrap().to_vec();
    let store = ChangingStore {
        events: history.clone(),
        reads: Cell::new(0),
        path: fx.zh_path(B3),
    };
    let mut changing = Session::replayed(store, s.court().clone(), history);
    let error = changing
        .export_finished(
            &fx.root,
            &fx.project,
            &ExportOptions {
                chapters: vec!["c0003".into()],
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("版本失效"), "{error}");
}

fn reference_fixture(fx: &Fx, s: &mut Session<EventLog>) -> ReferenceRequest {
    for block in [B1, B2] {
        let task = translated(fx, s, block);
        s.apply_changes(task, &fx.root, vec![]).unwrap();
        s.finalize_translation(&fx.root, &fx.project, block)
            .unwrap();
    }
    ReferenceRequest {
        selections: s
            .memory_pairs(&fx.root, &fx.project)
            .unwrap()
            .pairs
            .into_iter()
            .map(|p| ReferenceSelection {
                identity: p.identity,
                digests: p.digests,
                origin: p.origin,
            })
            .collect(),
        authorized_projects: BTreeSet::new(),
    }
}

#[test]
fn selected_references_preview_is_stable_strict_and_bounded() {
    let fx = Fx::new("reference-preview");
    let mut s = fx.session();
    let all = reference_fixture(&fx, &mut s);
    assert_eq!(all.selections.len(), 8);
    let mut request = all.clone();
    request.selections = vec![all.selections[1].clone()];
    let events = s.event_count();
    let draft = fs::read(fx.zh_path(B1)).unwrap();
    let preview = s
        .preview_references(
            &fx.root,
            &fx.project,
            B3,
            TranslationKind::Translate,
            &request,
        )
        .unwrap();
    assert_eq!(
        preview,
        s.preview_references(
            &fx.root,
            &fx.project,
            B3,
            TranslationKind::Translate,
            &request
        )
        .unwrap()
    );
    assert_eq!(preview.references.len(), 1);
    assert!(preview.prompt.contains("第2段の文。"));
    assert!(!preview.prompt.contains("第1段の文。"));
    assert!(!preview.prompt.contains("第3段の文。"));
    assert_eq!(preview.bytes, preview.prompt.len());
    assert_eq!(s.event_count(), events);
    assert_eq!(fs::read(fx.zh_path(B1)).unwrap(), draft);
    assert!(!fx.zh_path(B3).exists());
    assert!(
        s.preview_references(&fx.root, &fx.project, B3, TranslationKind::Translate, &all)
            .is_ok()
    );
    let mut nine = all.clone();
    nine.selections.push(all.selections[0].clone());
    assert!(
        s.preview_references(&fx.root, &fx.project, B3, TranslationKind::Translate, &nine)
            .is_err()
    );
    let mut duplicate = request.clone();
    duplicate.selections.push(request.selections[0].clone());
    assert!(
        s.preview_references(
            &fx.root,
            &fx.project,
            B3,
            TranslationKind::Translate,
            &duplicate
        )
        .is_err()
    );
    let mut forged = request.clone();
    forged.selections[0].digests.block_target_sha256 = "a".repeat(64);
    assert!(
        s.preview_references(
            &fx.root,
            &fx.project,
            B3,
            TranslationKind::Translate,
            &forged
        )
        .is_err()
    );
    let mut unauthorized = request.clone();
    unauthorized.authorized_projects.insert("a".repeat(32));
    assert!(
        s.preview_references(
            &fx.root,
            &fx.project,
            B3,
            TranslationKind::Translate,
            &unauthorized
        )
        .is_err()
    );
    assert!(
        s.preview_references(
            &fx.root,
            &fx.project,
            B3,
            TranslationKind::ReviewStyle,
            &request
        )
        .is_err()
    );
    let mut supplied_prose = serde_json::to_value(&request).unwrap();
    supplied_prose["selections"][0]["translation"] = "客户端伪造的定稿".into();
    assert!(serde_json::from_value::<ReferenceRequest>(supplied_prose).is_err());

    // Authorization is a concrete project set, not inherited from an index.
    for language in ["ja", "en"] {
        let target = fx
            .t()
            .texts()
            .create(NewProject {
                title: "跨项目目标".into(),
                source_language: language.into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap();
        fx.t()
            .texts()
            .import(&target.id, &[fx.root.join("c3.txt")])
            .unwrap();
        assert!(
            s.preview_references(
                &fx.root,
                &target.id,
                B1,
                TranslationKind::Translate,
                &request
            )
            .is_err()
        );
        let mut authorized = request.clone();
        authorized.authorized_projects = BTreeSet::from([fx.project.clone(), target.id.clone()]);
        let result = s.preview_references(
            &fx.root,
            &target.id,
            B1,
            TranslationKind::Translate,
            &authorized,
        );
        assert_eq!(result.is_ok(), language == "ja");
    }
}

#[test]
fn selected_references_concurrent_retries_keep_one_task_and_exact_snapshot() {
    let fx = Fx::new("reference-retry");
    let mut s = fx.session();
    let mut request = reference_fixture(&fx, &mut s);
    request.selections = vec![request.selections[1].clone()];
    let preview = s
        .preview_references(
            &fx.root,
            &fx.project,
            B3,
            TranslationKind::Translate,
            &request,
        )
        .unwrap();
    let submission = ReferenceSubmission {
        request,
        fingerprint: preview.fingerprint.clone(),
        submission: "retry-1".into(),
    };
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let ids: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let barrier = barrier.clone();
                let submission = &submission;
                let fx = &fx;
                scope.spawn(move || {
                    let mut session = fx.session();
                    barrier.wait();
                    session
                        .issue_with_references(
                            &fx.root,
                            &fx.project,
                            B3,
                            TranslationKind::Translate,
                            submission,
                        )
                        .unwrap()
                        .id
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(ids[0], ids[1]);
    s.reload().unwrap();
    assert_eq!(
        s.court().tasks().filter(|t| t.references.is_some()).count(),
        1
    );
    let view = s.reference_task(&fx.root, ids[0]).unwrap();
    assert!(view.current);
    assert_eq!(view.snapshot, preview);
    let mut conflict = submission.clone();
    conflict.fingerprint = "0".repeat(64);
    let before = s.event_count();
    assert!(
        s.issue_with_references(
            &fx.root,
            &fx.project,
            B3,
            TranslationKind::Translate,
            &conflict
        )
        .is_err()
    );
    assert_eq!(s.event_count(), before);
    s.reopen_translation(&fx.root, &fx.project, B1, "纠正译法")
        .unwrap();
    let view = s.reference_task(&fx.root, ids[0]).unwrap();
    assert!(!view.current);
    assert_eq!(view.snapshot, preview);
    let before = s.event_count();
    let retried = fx
        .session()
        .issue_with_references(
            &fx.root,
            &fx.project,
            B3,
            TranslationKind::Translate,
            &submission,
        )
        .unwrap();
    assert_eq!(retried.id, ids[0]);
    s.reload().unwrap();
    assert_eq!(s.event_count(), before);
    let mut fresh_identity = submission.clone();
    fresh_identity.submission = "retry-new".into();
    assert!(
        s.issue_with_references(
            &fx.root,
            &fx.project,
            B3,
            TranslationKind::Translate,
            &fresh_identity
        )
        .is_err()
    );
}

#[test]
fn selected_references_reject_stale_confirmation_and_count_full_utf8_material() {
    let fx = Fx::new("reference-bytes");
    let mut s = fx.session();
    let mut request = reference_fixture(&fx, &mut s);
    request.selections = vec![request.selections[1].clone()];
    let preview = s
        .preview_references(
            &fx.root,
            &fx.project,
            B3,
            TranslationKind::Translate,
            &request,
        )
        .unwrap();
    let submission = ReferenceSubmission {
        request: request.clone(),
        fingerprint: preview.fingerprint,
        submission: "stale".into(),
    };
    fx.t()
        .set_zhupi(&fx.project, B3, "c0003-p0001", "保留人物口气")
        .unwrap();
    let events = s.event_count();
    assert!(
        s.issue_with_references(
            &fx.root,
            &fx.project,
            B3,
            TranslationKind::Translate,
            &submission
        )
        .is_err()
    );
    assert_eq!(s.event_count(), events);

    // Tune a real finalized paragraph, not a synthetic prompt builder. Each
    // revision goes through public apply/finalize and yields new selection data.
    let mut size = 100;
    let mut reached_boundary = false;
    for _ in 0..4 {
        s.reopen_translation(&fx.root, &fx.project, B1, "字节边界夹具")
            .unwrap();
        let task = s.issue_revise(&fx.root, &fx.project, B1).unwrap().id;
        complete(
            &mut s,
            task,
            vec![fx.zh_change(
                B1,
                fx.candidate(B1, |ps| ps[1].text = "界".to_owned() + &"x".repeat(size)),
            )],
            "修订夹具",
        );
        s.apply_changes(task, &fx.root, vec![]).unwrap();
        s.finalize_translation(&fx.root, &fx.project, B1).unwrap();
        let pair = s
            .memory_pairs(&fx.root, &fx.project)
            .unwrap()
            .pairs
            .into_iter()
            .find(|p| p.identity.paragraph == "c0001-p0002")
            .unwrap();
        request.selections = vec![ReferenceSelection {
            identity: pair.identity,
            digests: pair.digests,
            origin: pair.origin,
        }];
        let preview = s
            .preview_references(
                &fx.root,
                &fx.project,
                B3,
                TranslationKind::Translate,
                &request,
            )
            .unwrap();
        if preview.bytes == MAX_REFERENCE_BYTES {
            reached_boundary = true;
            // One extra UTF-8 byte (ASCII here) must fail, not be truncated.
            s.reopen_translation(&fx.root, &fx.project, B1, "超界夹具")
                .unwrap();
            let task = s.issue_revise(&fx.root, &fx.project, B1).unwrap().id;
            complete(
                &mut s,
                task,
                vec![fx.zh_change(
                    B1,
                    fx.candidate(B1, |ps| {
                        ps[1].text = "界".to_owned() + &"x".repeat(size + 1)
                    }),
                )],
                "超界",
            );
            s.apply_changes(task, &fx.root, vec![]).unwrap();
            s.finalize_translation(&fx.root, &fx.project, B1).unwrap();
            let pair = s
                .memory_pairs(&fx.root, &fx.project)
                .unwrap()
                .pairs
                .into_iter()
                .find(|p| p.identity.paragraph == "c0001-p0002")
                .unwrap();
            request.selections = vec![ReferenceSelection {
                identity: pair.identity,
                digests: pair.digests,
                origin: pair.origin,
            }];
            let error = s
                .preview_references(
                    &fx.root,
                    &fx.project,
                    B3,
                    TranslationKind::Translate,
                    &request,
                )
                .unwrap_err();
            assert!(error.to_string().contains("8192"), "{error}");
            break;
        }
        size += MAX_REFERENCE_BYTES - preview.bytes;
    }
    assert!(
        reached_boundary,
        "must exercise the exact public byte boundary"
    );
}

fn verification_checks(category: SemanticCategory, resolution: Resolution) -> Vec<SemanticCheck> {
    [
        SemanticCategory::SubjectSpeaker,
        SemanticCategory::AgencyNegation,
        SemanticCategory::Quantity,
        SemanticCategory::Timeline,
        SemanticCategory::NamesRelations,
        SemanticCategory::HumorAdditions,
    ]
    .into_iter()
    .map(|c| SemanticCheck {
        category: c,
        disposition: if c == category {
            Disposition::Accepted
        } else {
            Disposition::NotApplicable
        },
        resolution: if c == category {
            resolution
        } else {
            Resolution::NotApplicable
        },
        note: if c == category {
            "已对照原文核查此项，修订解决情况另记"
        } else {
            "本次问题与此项无关，未据此判断整段质量"
        }
        .into(),
        advice: Vec::new(),
        evidence: Vec::new(),
    })
    .collect()
}

#[test]
fn verification_preserves_opinions_zhupi_and_revision_differences_without_a_signoff_gate() {
    let fx = Fx::new("verification-loop");
    let t = fx.t();
    let mut s = fx.session();
    let writer = translated(&fx, &mut s, B1);
    s.apply_changes(writer, &fx.root, vec![]).unwrap();
    let reviews = s.issue_review(&fx.root, &fx.project, B1).unwrap();
    complete(
        &mut s,
        reviews[0].id,
        vec![],
        "第1段请核对主语，勿把清的动作归给主人公。",
    );
    complete(&mut s, reviews[1].id, vec![], "保留叙述者的自嘲。");
    let paragraph = fx.block(B1).paragraphs[0].id.clone();
    let evidence = t
        .add_evidence(
            &fx.project,
            B1,
            &EvidenceInput {
                paragraph: paragraph.clone(),
                quote: "第1段".into(),
                source: "本地词义核对手册".into(),
                locator: "主语条目 §2".into(),
                explanation: "核对动作的实际发出者".into(),
                context: "本段叙述视角，不推及其余章节".into(),
                status: EvidenceStatus::Tentative,
            },
        )
        .unwrap();
    let initial = t.verification(Some(s.court()), &fx.project, B1).unwrap();
    assert!(
        initial
            .paragraphs
            .iter()
            .all(|p| !p.operator_reviewed && !p.opinions_handled && p.latest.is_none())
    );
    assert_eq!(initial.writer_task, Some(writer.0));
    assert_eq!(initial.reviews[0].writer_task, Some(writer.0));
    let mut checks = verification_checks(SemanticCategory::SubjectSpeaker, Resolution::Unresolved);
    checks[0].advice.push(AdviceLink {
        task: reviews[0].id.0,
        quote: "第1段请核对主语".into(),
    });
    checks[0].evidence.push(evidence.id);
    let request = VerificationInput {
        paragraph: paragraph.clone(),
        fingerprint: initial.fingerprint,
        checks,
    };
    let first = t
        .record_verification(Some(s.court()), &fx.project, B1, &request)
        .unwrap();
    let pending = t.verification(Some(s.court()), &fx.project, B1).unwrap();
    assert!(pending.paragraphs[0].opinions_handled);
    assert!(!pending.paragraphs[0].operator_reviewed);
    assert_eq!(pending.paragraphs[0].unresolved, 1);
    assert!(pending.records[0].current);
    // Existing finalization deliberately does NOT depend on semantic checklist completion.
    s.finalize_translation(&fx.root, &fx.project, B1).unwrap();
    s.reopen_translation(&fx.root, &fx.project, B1, "主语仍须修订")
        .unwrap();
    t.set_zhupi(&fx.project, B1, &paragraph, "核对动作的施受，保留否定")
        .unwrap();
    let old = t.verification(Some(s.court()), &fx.project, B1).unwrap();
    assert!(!old.records[0].current);
    assert_eq!(old.records[0].record, first);
    assert!(
        t.record_verification(Some(s.court()), &fx.project, B1, &request)
            .is_err()
    );
    let revision = s.issue_revise(&fx.root, &fx.project, B1).unwrap();
    let corrected = fx.candidate(B1, |ps| ps[0].text = "清的动作，不是主人公的。".into());
    complete(
        &mut s,
        revision.id,
        vec![fx.zh_change(B1, corrected)],
        "修订完成不是操作者验收",
    );
    s.apply_changes(revision.id, &fx.root, vec![]).unwrap();
    let fresh = t.verification(Some(s.court()), &fx.project, B1).unwrap();
    assert!(fresh.paragraphs.iter().all(|p| !p.operator_reviewed));
    let mut checks = request.checks.clone();
    checks[0].resolution = Resolution::Resolved;
    checks[0].note = "已对照实际修订前后文字核实主语；不是仅据模型声称".into();
    let second = t
        .record_verification(
            Some(s.court()),
            &fx.project,
            B1,
            &VerificationInput {
                paragraph,
                fingerprint: fresh.fingerprint,
                checks,
            },
        )
        .unwrap();
    assert_eq!(second.previous, Some(first.id));
    assert_eq!(second.snapshot.writer_task, Some(revision.id.0));
    assert_eq!(second.zhupi.as_deref(), Some("核对动作的施受，保留否定"));
    let after_restart = fx
        .t()
        .verification(Some(fx.session().court()), &fx.project, B1)
        .unwrap();
    assert!(after_restart.paragraphs[0].operator_reviewed);
    assert!(!after_restart.records[0].current);
    assert!(after_restart.records[1].current);
    assert_eq!(
        after_restart.records[1].change,
        Some(TranslationChange {
            before: "译：第1段の文。".into(),
            after: "清的动作，不是主人公的。".into(),
        })
    );
    // Neither model-written "completed" nor this paragraph's checklist implies batch-wide review.
    assert!(
        after_restart.paragraphs[1..]
            .iter()
            .all(|p| !p.operator_reviewed)
    );
    t.ensure_context_draft(&fx.project).unwrap();
    t.approve_context(&fx.project).unwrap();
    assert!(
        !t.verification(Some(s.court()), &fx.project, B1)
            .unwrap()
            .records[1]
            .current
    );
}

#[test]
fn verification_semantic_samples_keep_pending_and_rejected_evidence_until_explicit_recheck() {
    let mut fx = Fx::new("semantic-samples");
    // Minimal original examples: expectations are independent bilingual judgments, not machine rules.
    let samples = [
        (
            "一円札を銀貨に換えた。",
            "把银币换成一圆纸钞。",
            "把一圆纸钞换成银币。",
            SemanticCategory::AgencyNegation,
        ),
        (
            "清はおれを可愛がらなくてもよかった。",
            "我不必疼清。",
            "清本不必疼我。",
            SemanticCategory::SubjectSpeaker,
        ),
        (
            "合計は五倍だ。",
            "总数增加了五倍。",
            "总数是原来的五倍。",
            SemanticCategory::Quantity,
        ),
        (
            "狸が校長だ。",
            "狐狸是校长。",
            "狸是校长。",
            SemanticCategory::NamesRelations,
        ),
        (
            "教頭は数学主任ではない。",
            "教头就是数学主任。",
            "教头并不是数学主任。",
            SemanticCategory::AgencyNegation,
        ),
        (
            "うらなりという意味をおれは知らない。",
            "我知道蔫瓜是藤梢晚结的南瓜。",
            "我不知道蔫瓜是什么意思。",
            SemanticCategory::HumorAdditions,
        ),
    ];
    let source = fx.root.join("semantic.txt");
    fs::write(
        &source,
        samples.iter().map(|s| s.0).collect::<Vec<_>>().join("\n\n"),
    )
    .unwrap();
    let p = fx
        .t()
        .texts()
        .create(NewProject {
            title: "最小语义反例".into(),
            source_language: "ja".into(),
            target_language: "zh-CN".into(),
            provenance: Provenance::default(),
        })
        .unwrap();
    fx.t().texts().import(&p.id, &[source]).unwrap();
    fx.project = p.id;
    let t = fx.t();
    t.export(&fx.project, B1).unwrap();
    let mut candidate = fx.block(B1);
    for (p, sample) in candidate.paragraphs.iter_mut().zip(samples) {
        p.text = sample.1.into();
    }
    fs::write(fx.zh_path(B1), candidate.serialized().unwrap()).unwrap();
    let initial = t.verification(None, &fx.project, B1).unwrap();
    assert!(initial.structure.as_ref().unwrap().valid);
    assert!(initial.paragraphs.iter().all(|p| !p.operator_reviewed));
    for (p, sample) in candidate.paragraphs.iter().zip(samples) {
        let view = t.verification(None, &fx.project, B1).unwrap();
        let mut checks = verification_checks(sample.3, Resolution::Unresolved);
        // A rejected model suggestion remains recorded; rejection itself is not resolution.
        if sample.3 == SemanticCategory::HumorAdditions {
            checks.last_mut().unwrap().disposition = Disposition::Rejected;
            checks.last_mut().unwrap().resolution = Resolution::Unchecked;
        }
        t.record_verification(
            None,
            &fx.project,
            B1,
            &VerificationInput {
                paragraph: p.id.clone(),
                fingerprint: view.fingerprint,
                checks,
            },
        )
        .unwrap();
    }
    for (p, sample) in candidate.paragraphs.iter_mut().zip(samples) {
        p.text = sample.2.into();
    }
    fs::write(fx.zh_path(B1), candidate.serialized().unwrap()).unwrap();
    let changed = t.verification(None, &fx.project, B1).unwrap();
    assert!(changed.records.iter().all(|r| !r.current));
    assert!(changed.paragraphs.iter().all(|p| !p.operator_reviewed));
    for (p, sample) in candidate.paragraphs.iter().zip(samples) {
        let view = t.verification(None, &fx.project, B1).unwrap();
        t.record_verification(
            None,
            &fx.project,
            B1,
            &VerificationInput {
                paragraph: p.id.clone(),
                fingerprint: view.fingerprint,
                checks: verification_checks(sample.3, Resolution::Resolved),
            },
        )
        .unwrap();
    }
    let checked = t.verification(None, &fx.project, B1).unwrap();
    assert!(checked.paragraphs.iter().all(|p| p.operator_reviewed));
    for (view, sample) in checked.records[6..].iter().zip(samples) {
        assert_eq!(view.change.as_ref().unwrap().before, sample.1);
        assert_eq!(view.change.as_ref().unwrap().after, sample.2);
        assert!(view.current);
    }
    assert_eq!(
        checked.records[5].record.checks[5].disposition,
        Disposition::Rejected
    );
}

#[test]
fn verification_rejects_forged_links_and_stale_forms_and_limits_glossary_hints_to_literals() {
    let fx = Fx::new("verification-contract");
    let t = fx.t();
    let p = fx.block(B1).paragraphs[0].id.clone();
    let mut request = VerificationInput {
        paragraph: p.clone(),
        fingerprint: String::new(),
        checks: verification_checks(SemanticCategory::Quantity, Resolution::Resolved),
    };
    request.fingerprint = t.verification(None, &fx.project, B1).unwrap().fingerprint;
    assert!(
        t.record_verification(None, &fx.project, B1, &request)
            .is_err()
    );
    fx.write_glossary(r#"[{"source":"第1段","allowed":["第一译法","另一允许译法"]},{"source":"第2段","allowed":["限定译法"],"scope":"只有绰号语境","note":"不机械替换"}]"#);
    t.approve_context(&fx.project).unwrap();
    t.export(&fx.project, B1).unwrap();
    fs::write(
        fx.zh_path(B1),
        fx.candidate(B1, |ps| ps[0].text = "另一允许译法".into()),
    )
    .unwrap();
    let view = t.verification(None, &fx.project, B1).unwrap();
    assert_eq!(
        view.hints.len(),
        1,
        "第二个 allowed 也被接受；有 scope 只提示人工核对"
    );
    assert_eq!(view.hints[0].source, "第2段");
    assert_eq!(view.hints[0].scope.as_deref(), Some("只有绰号语境"));
    let mut evidence = EvidenceInput {
        paragraph: p.clone(),
        quote: "第1段".into(),
        source: "本地书目".into(),
        locator: "页2".into(),
        explanation: "查词不等于适用".into(),
        context: "仅此段".into(),
        status: EvidenceStatus::Confirmed,
    };
    let saved = t.add_evidence(&fx.project, B1, &evidence).unwrap();
    evidence.quote = "不在原文".into();
    assert!(t.add_evidence(&fx.project, B1, &evidence).is_err());
    evidence.quote = "第1段".into();
    evidence.source = "界".repeat(2001);
    assert!(t.add_evidence(&fx.project, B1, &evidence).is_err());
    // Adding evidence invalidates an already-open form instead of losing a concurrent change.
    request.fingerprint = view.fingerprint;
    assert!(
        t.record_verification(None, &fx.project, B1, &request)
            .is_err()
    );
    request.fingerprint = t.verification(None, &fx.project, B1).unwrap().fingerprint;
    request.checks[0].advice.push(AdviceLink {
        task: 987,
        quote: "伪造回奏".into(),
    });
    assert!(
        t.record_verification(None, &fx.project, B1, &request)
            .is_err()
    );
    request.checks[0].advice.clear();
    request.checks[0].evidence.push(saved.id);
    request.paragraph = fx.block(B1).paragraphs[1].id.clone();
    assert!(
        t.record_verification(None, &fx.project, B1, &request)
            .is_err()
    );
    request.paragraph = p;
    let record = t
        .record_verification(None, &fx.project, B1, &request)
        .unwrap();
    assert!(
        t.record_verification(None, &fx.project, B1, &request)
            .is_err()
    );
    let mut unknown = serde_json::to_value(&request).unwrap();
    unknown["operator_reviewed"] = true.into();
    assert!(serde_json::from_value::<VerificationInput>(unknown).is_err());
    assert_eq!(
        t.verification(None, &fx.project, B1).unwrap().records[0]
            .record
            .id,
        record.id
    );
    // Evidence and review records never enter the existing export path automatically.
    t.export(&fx.project, B1).unwrap();
    for name in [
        "source.json",
        "context.json",
        "zhupi.json",
        "neighbors.json",
    ] {
        assert!(
            !fs::read_to_string(t.block_dir(&fx.project, B1).join(name))
                .unwrap()
                .contains("查词不等于适用")
        );
    }
}

#[test]
fn human_approval_event_resolves_as_current_source() {
    use ziyuan_core::{
        DraftBasis, EventReference, GovernanceMode, HumanActor, HumanApproval,
        HumanApprovalRequest, HumanApprovalTarget, HumanBindingRequest, HumanSubmissionBinding,
        TranslationSource,
    };
    let project = "0123456789abcdef0123456789abcdef";
    let previous = TranslationSource::None {};
    let binding_request = HumanBindingRequest {
        project: project.into(),
        block: B1.into(),
        request_id: "bind-1".into(),
        actor: HumanActor::Local,
        submission_id: "submit-1".into(),
        revision: "d".repeat(32),
        basis: DraftBasis {
            project_hash: "a".repeat(64),
            source_hash: "b".repeat(64),
            context_version: 1,
            zhupi_hash: "c".repeat(64),
            translation_hash: None,
        },
        candidate: "{}\n".into(),
        candidate_hash: sha256_hex(b"{}\n"),
        previous: previous.clone(),
    };
    let binding = HumanSubmissionBinding {
        request_hash: ziyuan_governance::human_binding_request_hash(&binding_request),
        request: binding_request,
    };
    let approval_request = HumanApprovalRequest {
        project: project.into(),
        block: B1.into(),
        request_id: "approve-1".into(),
        actor: HumanActor::Local,
        submission_id: "bind-1".into(),
        binding: EventReference {
            index: 1,
            hash: "e".repeat(64),
        },
        previous,
        target: HumanApprovalTarget {
            format: "zh-json/1".into(),
            path: format!("translations/{project}/{B1}/zh.json"),
            before: None,
            after: "f".repeat(64),
        },
    };
    let approval = HumanApproval {
        request_hash: ziyuan_governance::human_approval_request_hash(&approval_request),
        request: approval_request,
        operation_id: "1".repeat(32),
    };
    let history = vec![
        CourtEvent::Founded(GovernanceMode::Chancellor),
        CourtEvent::HumanSubmissionBound {
            binding: Box::new(binding),
        },
        CourtEvent::HumanTranslationApproved {
            approval: Box::new(approval),
        },
    ];
    let resolved = resolve_translation_source(&history, project, B1).unwrap();
    assert_eq!(
        resolved.source,
        TranslationSource::Human {
            operation_id: "1".repeat(32),
            event: ziyuan_storage::event_reference(2, &history[2]),
        }
    );
    assert_eq!(resolved.translation_hash, Some("f".repeat(64)));
}

#[test]
fn human_finalization_appends_fact_without_touching_translation() {
    use ziyuan_core::{HumanActor, HumanApproval, HumanApprovalRequest, HumanApprovalTarget};
    use ziyuan_storage::{LockScope, Workspace};
    let fx = Fx::new("human-finalize");
    let t = fx.t();
    let mut s = fx.session();
    let task = translated(&fx, &mut s, B1);
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    let before = fs::read(fx.zh_path(B1)).unwrap();

    let draft = DraftSave {
        request_id: "save".into(),
        expected_revision: None,
        basis: t.human_draft(&fx.project, B1).unwrap().basis,
        paragraphs: fx
            .block(B1)
            .paragraphs
            .into_iter()
            .map(|p| DraftParagraph {
                id: p.id,
                text: format!("人工：{}", p.text),
                marks: vec![],
            })
            .collect(),
    };
    let saved = t.save_human_draft(&fx.project, B1, &draft).unwrap();
    let submission = t
        .submit_human_draft(
            &fx.project,
            B1,
            &DraftSubmit {
                request_id: "submit".into(),
                expected_revision: saved.revision,
            },
        )
        .unwrap();
    let current = resolve_translation_source(s.history().unwrap(), &fx.project, B1).unwrap();
    let binding = submission
        .prepare_binding("bind".into(), HumanActor::Local, current.source.clone())
        .unwrap();
    let candidate = binding.request.candidate.clone();
    let after = sha256_hex(candidate.as_bytes());

    let workspace = Workspace::open(&fx.root).unwrap();
    let mut guard = workspace.lock(LockScope::Workspace).unwrap();
    guard.upgrade_write_protocol().unwrap();
    t.texts()
        .upgrade_project_protocol(&fx.project, &guard)
        .unwrap();
    assert!(t.publish_human_binding(&guard, &binding).unwrap());
    drop(guard);
    // 绑定由另一会话写入；重新打开后再提议批准，避免用旧的内存朝廷。
    let mut s = fx.session();

    let approval_request = HumanApprovalRequest {
        project: fx.project.clone(),
        block: B1.into(),
        request_id: "approve-1".into(),
        actor: HumanActor::Local,
        submission_id: binding.request.request_id.clone(),
        binding: ziyuan_core::EventReference {
            index: 0,
            hash: "e".repeat(64),
        },
        previous: current.source,
        target: HumanApprovalTarget {
            format: "zh-json/1".into(),
            path: output_rel(&fx.project, B1),
            before: Some(sha256_hex(&before)),
            after: after.clone(),
        },
    };
    let approval = HumanApproval {
        request_hash: ziyuan_governance::human_approval_request_hash(&approval_request),
        request: approval_request,
        operation_id: "1".repeat(32),
    };
    s.commit(|court| {
        Ok(vec![
            court.propose_human_translation_approved(approval.clone())?,
        ])
    })
    .unwrap_or_else(|error| panic!("commit: {error:?}"));

    fs::write(fx.zh_path(B1), candidate.as_bytes()).unwrap();
    let guard = workspace.lock(LockScope::Workspace).unwrap();
    assert!(
        t.finalize_human_translation(&guard, &fx.project, B1)
            .unwrap()
    );
    drop(guard);

    assert_eq!(fs::read(fx.zh_path(B1)).unwrap(), candidate.as_bytes());
    let reopened = fx.session();
    let court = ziyuan_governance::Court::replay(reopened.history().unwrap()).unwrap();
    assert!(matches!(
        court.translation_signoff(&fx.project, B1),
        Some(ziyuan_governance::Signoff::HumanFinalized { .. })
    ));
    let report = reopened.memory_pairs(&fx.root, &fx.project).unwrap();
    assert!(
        !report.pairs.is_empty(),
        "人工定稿应进入记忆；skipped: {:?}",
        report.skipped
    );
    assert!(
        report.pairs[0].origin.human.is_some(),
        "记忆来源须标出人工定稿"
    );
}

#[test]
fn marked_markdown_renders_ruby_and_styles() {
    use super::finished::marked_text;
    let para = DraftParagraph {
        id: "p1".into(),
        text: "夜が明ける。".into(),
        marks: vec![
            TargetMark {
                start: 0,
                end: 3,
                kind: TargetMarkKind::Ruby {
                    reading: "よ".into(),
                },
            },
            TargetMark {
                start: 6,
                end: 9,
                kind: TargetMarkKind::Strong {},
            },
        ],
    };
    let out = marked_text(&para).unwrap();
    assert!(out.contains("<ruby>夜<rt>よ</rt></ruby>"), "{out}");
    assert!(out.contains("<strong>明</strong>"), "{out}");
    assert!(out.ends_with('。'), "{out}");

    let dotted = DraftParagraph {
        id: "p1".into(),
        text: "夜".into(),
        marks: vec![TargetMark {
            start: 0,
            end: 3,
            kind: TargetMarkKind::EmphasisDots {},
        }],
    };
    assert!(marked_text(&dotted).is_err());
}

#[test]
fn peek_human_draft_does_not_create_directories() {
    let fx = Fx::new("peek-missing-draft");
    let dir = fx
        .root
        .join(".ziyuan/text-projects")
        .join(&fx.project)
        .join("human-drafts");
    assert!(!dir.exists());
    let view = fx.t().peek_human_draft(&fx.project, B1).unwrap();
    assert!(view.draft.is_none());
    assert!(!dir.exists(), "只读读取不得创建空目录");
}

#[test]
fn peek_human_draft_rejects_non_directory_ancestors_and_invalid_blocks() {
    let fx = Fx::new("peek-draft-paths");
    let dir = fx
        .root
        .join(".ziyuan/text-projects")
        .join(&fx.project)
        .join("human-drafts");
    assert!(fx.t().peek_human_draft(&fx.project, "../outside").is_err());
    assert!(!dir.exists());
    fs::write(&dir, "not a directory").unwrap();
    assert!(fx.t().peek_human_draft(&fx.project, B1).is_err());
    assert_eq!(fs::read_to_string(&dir).unwrap(), "not a directory");
}

#[cfg(unix)]
#[test]
fn peek_human_draft_rejects_symlink_ancestors_and_files() {
    use std::os::unix::fs::symlink;
    let fx = Fx::new("peek-draft-symlink");
    let dir = fx
        .root
        .join(".ziyuan/text-projects")
        .join(&fx.project)
        .join("human-drafts");
    let outside = fx.root.join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, &dir).unwrap();
    assert!(fx.t().peek_human_draft(&fx.project, B1).is_err());
    fs::remove_file(&dir).unwrap();
    fs::create_dir(&dir).unwrap();
    for path in [
        dir.join(format!("{B1}.json")),
        dir.join(format!("{B1}.json.lock")),
    ] {
        symlink(outside.join("missing"), &path).unwrap();
        assert!(fx.t().peek_human_draft(&fx.project, B1).is_err());
        fs::remove_file(path).unwrap();
    }
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn peek_human_draft_reads_without_writing() {
    let fx = Fx::new("peek-draft");
    let t = fx.t();
    let draft = DraftSave {
        request_id: "save".into(),
        expected_revision: None,
        basis: t.human_draft(&fx.project, B1).unwrap().basis,
        paragraphs: fx
            .block(B1)
            .paragraphs
            .into_iter()
            .map(|p| DraftParagraph {
                id: p.id,
                text: format!("人工：{}", p.text),
                marks: vec![],
            })
            .collect(),
    };
    let saved = t.save_human_draft(&fx.project, B1, &draft).unwrap();
    let before = fs::read(fx.root.join(".ziyuan/write.generation")).ok();
    let view = t.peek_human_draft(&fx.project, B1).unwrap();
    assert_eq!(view.draft.unwrap().revision, saved.revision);
    assert_eq!(
        fs::read(fx.root.join(".ziyuan/write.generation")).ok(),
        before
    );
}

/// 人工一轮：保存草稿、送审、准奏（准奏即落盘），可选人工定稿；全走 CLI 所用的公开接口。
fn human_round(fx: &Fx, block: &str, paragraphs: Vec<DraftParagraph>, finalize: bool) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let t = fx.t();
    let view = t.human_draft(&fx.project, block).unwrap();
    let saved = t
        .save_human_draft(
            &fx.project,
            block,
            &DraftSave {
                request_id: format!("save-{}", N.fetch_add(1, Ordering::Relaxed)),
                expected_revision: view.draft.map(|d| d.revision),
                basis: view.basis,
                paragraphs,
            },
        )
        .unwrap();
    let id = human_submit_request_id(&saved.revision);
    let binding = t
        .prepare_human_submission(&fx.project, block, &id, &id, &saved.revision)
        .unwrap();
    let workspace = ziyuan_storage::Workspace::open(&fx.root).unwrap();
    let mut guard = workspace
        .lock(ziyuan_storage::LockScope::Workspace)
        .unwrap();
    guard.upgrade_write_protocol().unwrap();
    t.texts()
        .upgrade_project_protocol(&fx.project, &guard)
        .unwrap();
    assert!(t.publish_human_binding(&guard, &binding).unwrap());
    assert!(
        t.approve_human_submission(&mut guard, &fx.project, block)
            .unwrap()
    );
    if finalize {
        assert!(
            t.finalize_human_translation(&guard, &fx.project, block)
                .unwrap()
        );
    }
}

fn human_finalize(fx: &Fx, block: &str) {
    let workspace = ziyuan_storage::Workspace::open(&fx.root).unwrap();
    let guard = workspace
        .lock(ziyuan_storage::LockScope::Workspace)
        .unwrap();
    assert!(
        fx.t()
            .finalize_human_translation(&guard, &fx.project, block)
            .unwrap()
    );
}

/// 每段译作「人工：原文」，不带标记。
fn human_paragraphs(fx: &Fx, block: &str) -> Vec<DraftParagraph> {
    fx.block(block)
        .paragraphs
        .into_iter()
        .map(|p| DraftParagraph {
            id: p.id,
            text: format!("人工：{}", p.text),
            marks: vec![],
        })
        .collect()
}

#[test]
fn finished_export_accepts_human_finalized_blocks_alongside_model_ones() {
    let fx = Fx::new("human-export");
    let mut s = fx.session();
    let task = translated(&fx, &mut s, B1);
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    s.finalize_translation(&fx.root, &fx.project, B1).unwrap();
    human_round(&fx, B3, human_paragraphs(&fx, B3), true);
    let mut marked = human_paragraphs(&fx, B2);
    assert_eq!(marked[0].text, "人工：二章一。");
    assert_eq!(marked[1].text, "人工：二章二。");
    marked[0].marks = vec![TargetMark {
        start: 0,
        end: 6,
        kind: TargetMarkKind::Ruby {
            reading: "じんこう".into(),
        },
    }];
    marked[1].marks = vec![
        TargetMark {
            start: 9,
            end: 18,
            kind: TargetMarkKind::Strong {},
        },
        TargetMark {
            start: 0,
            end: 6,
            kind: TargetMarkKind::Emphasis {},
        },
    ];
    human_round(&fx, B2, marked, true);

    let mut s = fx.session();
    for block in [B2, B3] {
        assert!(
            matches!(
                fx.status(&s, block).state,
                BlockState::HumanFinalized { .. }
            ),
            "{block}: {:?}",
            fx.status(&s, block)
        );
    }
    let markdown = ExportOptions {
        format: ExportFormat::Markdown,
        ..Default::default()
    };
    let md = s.export_finished(&fx.root, &fx.project, &markdown).unwrap();
    assert_eq!(
        md.files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        ["c0001.md", "c0002.md", "c0003.md"]
    );
    assert_eq!(
        String::from_utf8(md.files[1].bytes.clone()).unwrap(),
        "# c2\\.txt\n\n<ruby>人工<rt>じんこう</rt></ruby>：二章一。\n\n<em>人工</em>：<strong>二章二</strong>。\n\n"
    );
    assert_eq!(
        String::from_utf8(md.files[2].bytes.clone()).unwrap(),
        "# c3\\.txt\n\n人工：三章。\n\n"
    );
    let chapters = &md.manifest["chapters"];
    assert_eq!(chapters[0]["blocks"][0]["task"], task.0);
    assert_eq!(chapters[1]["blocks"][0]["source"], "human");
    assert_eq!(chapters[2]["blocks"][0]["source"], "human");
    assert_eq!(
        chapters[2]["blocks"][0]["translation_sha256"],
        sha256_hex(&fs::read(fx.zh_path(B3)).unwrap())
    );

    // TXT cannot carry the marks: the whole batch is refused and names only the marked block.
    let err = s
        .export_finished(&fx.root, &fx.project, &ExportOptions::default())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("整批未导出")
            && err.contains(B2)
            && err.contains("--format md")
            && !err.contains(B1)
            && !err.contains(B3),
        "{err}"
    );
    // A plain human finalization exports to TXT in the same shape as a model one.
    let txt = s
        .export_finished(
            &fx.root,
            &fx.project,
            &ExportOptions {
                chapters: vec!["c0001".into(), "c0003".into()],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        String::from_utf8(txt.files[1].bytes.clone()).unwrap(),
        "c3.txt\n\n人工：三章。\n\n"
    );

    // Disk drift after human finalization is not exportable.
    let good = fs::read(fx.zh_path(B3)).unwrap();
    fs::write(fx.zh_path(B3), fx.candidate(B3, |_| {})).unwrap();
    let err = s
        .export_finished(&fx.root, &fx.project, &markdown)
        .unwrap_err()
        .to_string();
    assert!(err.contains(B3) && err.contains("不确定"), "{err}");
    fs::write(fx.zh_path(B3), good).unwrap();

    // Emphasis dots have no lossless Markdown form either.
    let mut dotted = human_paragraphs(&fx, B2);
    dotted[0].marks = vec![TargetMark {
        start: 0,
        end: 6,
        kind: TargetMarkKind::EmphasisDots {},
    }];
    human_round(&fx, B2, dotted, true);
    let mut s = fx.session();
    let err = s
        .export_finished(&fx.root, &fx.project, &markdown)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("整批未导出") && err.contains(B2) && err.contains("着重点"),
        "{err}"
    );
}

#[test]
fn status_follows_the_latest_approval_source_in_event_order() {
    let fx = Fx::new("source-order");
    // Model, then human.
    let mut s = fx.session();
    let task = translated(&fx, &mut s, B1);
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    assert!(matches!(
        fx.status(&s, B1).state,
        BlockState::Applied { .. }
    ));
    human_round(&fx, B1, human_paragraphs(&fx, B1), false);
    let s = fx.session();
    assert!(
        matches!(fx.status(&s, B1).state, BlockState::HumanApproved { .. }),
        "{:?}",
        fx.status(&s, B1)
    );
    human_finalize(&fx, B1);
    let s = fx.session();
    let status = fx.status(&s, B1);
    assert!(
        matches!(status.state, BlockState::HumanFinalized { .. }),
        "{status:?}"
    );
    assert!(
        status
            .note
            .as_deref()
            .is_some_and(|n| n.contains("ziyuan text export-finished")),
        "{status:?}"
    );

    // Human, then model: the human signoff no longer applies.
    human_round(&fx, B3, human_paragraphs(&fx, B3), true);
    let mut s = fx.session();
    assert!(matches!(
        fx.status(&s, B3).state,
        BlockState::HumanFinalized { .. }
    ));
    let task = translated(&fx, &mut s, B3);
    assert_eq!(
        fx.status(&s, B3).state,
        BlockState::AwaitingApproval { task: task.0 }
    );
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    assert_eq!(
        fx.status(&s, B3).state,
        BlockState::Applied {
            task: task.0,
            next: NextStep::Review
        },
        "{:?}",
        fx.status(&s, B3)
    );
    s.finalize_translation(&fx.root, &fx.project, B3).unwrap();
    assert!(matches!(
        fx.status(&s, B3).state,
        BlockState::Finalized { task: t, .. } if t == task.0
    ));
}

#[test]
fn model_signoffs_give_way_to_a_current_human_approval() {
    let fx = Fx::new("human-over-model");
    let mut s = fx.session();
    let task = translated(&fx, &mut s, B1);
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    s.finalize_translation(&fx.root, &fx.project, B1).unwrap();
    let task = translated(&fx, &mut s, B3);
    s.apply_changes(task, &fx.root, vec![]).unwrap();
    s.finalize_translation(&fx.root, &fx.project, B3).unwrap();
    s.reopen_translation(&fx.root, &fx.project, B3, "重审")
        .unwrap();

    // Model finalized, then human: the human round finalizes over it.
    human_round(&fx, B1, human_paragraphs(&fx, B1), true);
    // Model reopened, then human: likewise.
    human_round(&fx, B3, human_paragraphs(&fx, B3), true);
    let mut s = fx.session();
    for block in [B1, B3] {
        assert!(
            matches!(
                fx.status(&s, block).state,
                BlockState::HumanFinalized { .. }
            ),
            "{block}: {:?}",
            fx.status(&s, block)
        );
    }
    // Idempotent for this approval's own finalization.
    let workspace = ziyuan_storage::Workspace::open(&fx.root).unwrap();
    let guard = workspace
        .lock(ziyuan_storage::LockScope::Workspace)
        .unwrap();
    assert!(
        !fx.t()
            .finalize_human_translation(&guard, &fx.project, B1)
            .unwrap()
    );
    drop(guard);
    let txt = s
        .export_finished(
            &fx.root,
            &fx.project,
            &ExportOptions {
                chapters: vec!["c0001".into(), "c0003".into()],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        String::from_utf8(txt.files[0].bytes.clone()).unwrap(),
        "c1.txt\n\n人工：第1段の文。\n\n人工：第2段の文。\n\n人工：第3段の文。\n\n人工：第4段の文。\n\n人工：第5段の文。\n\n人工：第6段の文。\n\n"
    );
    assert_eq!(
        String::from_utf8(txt.files[1].bytes.clone()).unwrap(),
        "c3.txt\n\n人工：三章。\n\n"
    );
    assert_eq!(txt.manifest["chapters"][0]["blocks"][0]["source"], "human");
}

#[test]
fn human_rounds_go_stale_when_zhupi_or_context_change() {
    let fx = Fx::new("human-stale");
    human_round(&fx, B1, human_paragraphs(&fx, B1), true);
    let mut s = fx.session();
    let c1 = ExportOptions {
        chapters: vec!["c0001".into()],
        ..Default::default()
    };
    assert!(s.export_finished(&fx.root, &fx.project, &c1).is_ok());

    // Zhupi added after finalization.
    fx.t()
        .set_zhupi(&fx.project, B1, "c0001-p0001", "新要求")
        .unwrap();
    let status = fx.status(&s, B1);
    let BlockState::HumanStale { reasons, .. } = &status.state else {
        panic!("应失效：{status:?}")
    };
    assert_eq!(reasons, &[StaleReason::ZhupiChanged]);
    assert_eq!(status.state.label(), "版本失效");
    assert!(
        status
            .note
            .as_deref()
            .is_some_and(|n| n.contains("须重新定稿") && n.contains("ziyuan text human submit")),
        "{status:?}"
    );
    let err = s
        .export_finished(&fx.root, &fx.project, &c1)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(B1) && err.contains("版本失效") && err.contains("朱批已变"),
        "{err}"
    );
    fx.t().clear_zhupi(&fx.project, B1, "c0001-p0001").unwrap();
    assert!(matches!(
        fx.status(&s, B1).state,
        BlockState::HumanFinalized { .. }
    ));

    // A newly approved context after finalization.
    fx.write_summary("c0001", "前情甲");
    assert_eq!(fx.t().approve_context(&fx.project).unwrap(), 1);
    assert_eq!(
        fx.status(&s, B1).state,
        BlockState::HumanStale {
            operation_id: match fx.status(&s, B1).state {
                BlockState::HumanStale { operation_id, .. } => operation_id,
                other => panic!("应失效：{other:?}"),
            },
            reasons: vec![StaleReason::ContextChanged {
                used: 0,
                current: 1
            }],
        }
    );
    assert!(s.export_finished(&fx.root, &fx.project, &c1).is_err());

    // An approval whose submission basis no longer holds cannot be finalized.
    human_round(&fx, B3, human_paragraphs(&fx, B3), false);
    let s = fx.session();
    assert!(matches!(
        fx.status(&s, B3).state,
        BlockState::HumanApproved { .. }
    ));
    fx.t()
        .set_zhupi(&fx.project, B3, "c0003-p0001", "再斟酌")
        .unwrap();
    let status = fx.status(&s, B3);
    assert!(
        matches!(&status.state, BlockState::HumanStale { reasons, .. } if reasons == &[StaleReason::ZhupiChanged]),
        "{status:?}"
    );
    assert!(
        status
            .note
            .as_deref()
            .is_some_and(|n| n.contains("不能再定稿") && n.contains("ziyuan text human submit")),
        "{status:?}"
    );
    let log = fs::read(fx.root.join(".ziyuan/court.log")).unwrap();
    let workspace = ziyuan_storage::Workspace::open(&fx.root).unwrap();
    let guard = workspace
        .lock(ziyuan_storage::LockScope::Workspace)
        .unwrap();
    let err = fx
        .t()
        .finalize_human_translation(&guard, &fx.project, B3)
        .unwrap_err()
        .to_string();
    drop(guard);
    assert!(
        err.contains("送审后材料有变") && err.contains("朱批已变") && err.contains("human submit"),
        "{err}"
    );
    assert_eq!(fs::read(fx.root.join(".ziyuan/court.log")).unwrap(), log);
    fx.t().clear_zhupi(&fx.project, B3, "c0003-p0001").unwrap();
    human_finalize(&fx, B3);
    let s = fx.session();
    assert!(matches!(
        fx.status(&s, B3).state,
        BlockState::HumanFinalized { .. }
    ));
}
