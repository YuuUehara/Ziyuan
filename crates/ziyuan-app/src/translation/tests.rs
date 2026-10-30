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

fn apply_error(result: std::result::Result<(), AppError>) -> String {
    match result {
        Err(AppError::Apply(msg)) => msg,
        other => panic!("应被门控拒绝：{other:?}"),
    }
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
