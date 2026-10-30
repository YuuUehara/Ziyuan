use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ziyuan_app::{ScheduledSubtask, Session};
use ziyuan_core::{AgentId, Capability, Department, FileChange, Seal};
use ziyuan_governance::{ChangeState, TaskStatus};
use ziyuan_runtime::{
    BoxError, CancelToken, ExecutionHooks, Job, Limits, Scheduler, WorkerBackend,
};
use ziyuan_storage::MemoryEventLog;

fn plan(value: serde_json::Value) -> Vec<ScheduledSubtask> {
    serde_json::from_value(value).unwrap()
}

fn two() -> Vec<ScheduledSubtask> {
    plan(serde_json::json!([
        {"worker":4,"text":"first","idempotency_key":"first"},
        {"worker":4,"text":"second","dependencies":[1],"idempotency_key":"second"}
    ]))
}

fn seal(writes: bool) -> Seal {
    Seal {
        capabilities: if writes {
            [Capability::ReadFile, Capability::WriteFile].into()
        } else {
            [Capability::ReadFile].into()
        },
        scope: vec![".".into()],
        mcp: Default::default(),
    }
}

#[test]
fn scheduled_contract_defaults_and_strict_json() {
    let value = serde_json::json!({"worker":4,"text":"read"});
    let sub: ScheduledSubtask = serde_json::from_value(value).unwrap();
    assert_eq!(sub.worker, AgentId(4));
    assert!(sub.dependencies.is_empty());
    assert!(!sub.approval_required);
    assert_eq!(sub.attempts, None);
    assert_eq!(sub.deadline, None);
    assert_eq!(sub.budget, None);
    assert_eq!(sub.artifact, None);
    assert_eq!(sub.idempotency_key, None);
    assert_eq!(
        serde_json::from_str::<ScheduledSubtask>(&serde_json::to_string(&sub).unwrap()).unwrap(),
        sub
    );
    for invalid in [
        serde_json::json!({"worker":4,"text":"x","dependences":[1]}),
        serde_json::json!({"worker":4}),
        serde_json::json!({"worker":4,"text":"x","dependencies":[-1]}),
        serde_json::json!({"worker":4,"text":"x","attempts":4294967296u64}),
    ] {
        assert!(serde_json::from_value::<ScheduledSubtask>(invalid).is_err());
    }
}

#[test]
fn scheduled_retries_reload_and_conflicts_are_atomic() {
    let mut store = MemoryEventLog::default();
    let mut s = Session::found(&mut store).unwrap();
    // IDs differ from plan indices.
    s.issue(Department::Works, "other".into(), seal(false))
        .unwrap();
    let parent = s
        .issue(Department::Works, "parent".into(), seal(false))
        .unwrap()
        .id;
    let mut p = two();
    p[1].attempts = Some(3);
    p[1].deadline = Some(u64::MAX);
    p[1].budget = Some(99);
    p[1].artifact = Some("answer.txt".into());
    p[1].approval_required = true;
    let ids = s.split_scheduled(parent, p.clone()).unwrap();
    assert_eq!(
        s.court()
            .task(ids[1])
            .unwrap()
            .dag
            .as_ref()
            .unwrap()
            .dependencies,
        [ids[0]]
    );
    let len = s.history().unwrap().len();
    assert_eq!(s.split_scheduled(parent, p.clone()).unwrap(), ids);
    assert_eq!(s.history().unwrap().len(), len);
    drop(s);
    let mut s = Session::open(&mut store).unwrap();
    assert_eq!(s.split_scheduled(parent, p.clone()).unwrap(), ids);
    for field in 0..9 {
        let mut changed = p.clone();
        match field {
            0 => changed[0].text.push('!'),
            1 => changed[1].dependencies.clear(),
            2 => changed[1].attempts = Some(4),
            3 => changed[1].deadline = Some(42),
            4 => changed[1].budget = Some(100),
            5 => changed[1].artifact = Some("other".into()),
            6 => changed[1].approval_required = false,
            7 => changed[1].idempotency_key = Some("new-key".into()),
            _ => changed[0].worker = AgentId(2),
        }
        assert!(
            s.split_scheduled(parent, changed)
                .unwrap_err()
                .to_string()
                .contains("不一致")
        );
        assert_eq!(s.history().unwrap().len(), len);
    }
    // Keys are scoped to the parent, not global identifiers.
    let other = s
        .issue(Department::Works, "new".into(), seal(false))
        .unwrap()
        .id;
    assert!(s.split_scheduled(other, p).is_ok());
}

#[test]
fn scheduled_invalid_plans_leave_no_partial_writes() {
    let mut store = MemoryEventLog::default();
    let mut s = Session::found(&mut store).unwrap();
    let parent = s
        .issue(Department::Works, "parent".into(), seal(false))
        .unwrap()
        .id;
    let mut invalid = vec![vec![], vec![two()[0].clone()], vec![two()[0].clone(); 9]];
    for deps in [vec![0], vec![3], vec![1], vec![2, 2]] {
        let mut p = two();
        p[0].dependencies = deps;
        invalid.push(p);
    }
    let mut cycle = two();
    cycle[0].dependencies = vec![2];
    invalid.push(cycle);
    for field in 0..6 {
        let mut p = two();
        match field {
            0 => p[0].attempts = Some(0),
            1 => p[0].budget = Some(0),
            2 => p[0].idempotency_key = Some("  ".into()),
            3 => p[1].idempotency_key = p[0].idempotency_key.clone(),
            4 => p[0].worker = AgentId(9999),
            _ => p[0].text = " ".into(),
        }
        invalid.push(p);
    }
    let len = s.history().unwrap().len();
    for p in invalid {
        assert!(s.split_scheduled(parent, p).is_err());
        assert_eq!(s.history().unwrap().len(), len);
        assert_eq!(s.court().tasks().count(), 1);
    }
    assert!(s.split_scheduled(parent, two()).is_ok());
}

#[test]
fn scheduled_backwards_edges_and_write_serialization() {
    for writes in [false, true] {
        let mut store = MemoryEventLog::default();
        let mut s = Session::found(&mut store).unwrap();
        let parent = s
            .issue(Department::Works, "parent".into(), seal(writes))
            .unwrap()
            .id;
        let p = plan(serde_json::json!([
            {"worker":4,"text":"last","dependencies":[3]},
            {"worker":4,"text":"independent"},
            {"worker":4,"text":"first"}
        ]));
        let ids = s.split_scheduled(parent, p.clone()).unwrap();
        assert_eq!(
            s.court()
                .task(ids[0])
                .unwrap()
                .dag
                .as_ref()
                .unwrap()
                .dependencies,
            [ids[2]]
        );
        if writes {
            assert_eq!(
                s.court()
                    .task(ids[2])
                    .unwrap()
                    .dag
                    .as_ref()
                    .unwrap()
                    .dependencies,
                [ids[1]]
            );
            assert_eq!(s.court().runnable_tasks(), [ids[1]]);
        } else {
            assert!(
                s.court()
                    .task(ids[2])
                    .unwrap()
                    .dag
                    .as_ref()
                    .unwrap()
                    .dependencies
                    .is_empty()
            );
            assert_eq!(s.court().runnable_tasks(), [ids[1], ids[2]]);
        }
        let len = s.history().unwrap().len();
        assert_eq!(s.split_scheduled(parent, p).unwrap(), ids);
        assert_eq!(s.history().unwrap().len(), len);
    }
}

#[tokio::test]
async fn scheduled_no_change_dependencies_complete_and_finished_retries_are_noops() {
    for writes in [false, true] {
        let mut store = MemoryEventLog::default();
        let mut s = Session::found(&mut store).unwrap();
        let parent = s
            .issue(Department::Works, "parent".into(), seal(writes))
            .unwrap()
            .id;
        let p = two();
        let ids = s.split_scheduled(parent, p.clone()).unwrap();
        let scheduler = Scheduler::new(
            Arc::new(ziyuan_runtime::DemoBackend),
            Limits {
                max_concurrent: 4,
                timeout: Duration::from_secs(5),
            },
        );
        s.run(&[ids[0]], &scheduler, &CancelToken::new(), |_| {})
            .await
            .unwrap();
        let len = s.history().unwrap().len();
        assert_eq!(s.split_scheduled(parent, p.clone()).unwrap(), ids);
        assert_eq!(s.history().unwrap().len(), len);
        let mut changed = p.clone();
        changed[1].text = "edit after upstream completion".into();
        assert!(s.split_scheduled(parent, changed).is_err());
        assert_eq!(s.history().unwrap().len(), len);
        assert!(s.court().runnable_tasks().contains(&ids[1]));
        s.run_open(&scheduler, &CancelToken::new(), |_| {})
            .await
            .unwrap();
        assert!(std::iter::once(&parent).chain(&ids).all(|id| matches!(
            s.court().task(*id).unwrap().status,
            TaskStatus::Completed { .. }
        )));
        drop(s);
        let mut s = Session::open(&mut store).unwrap();
        let len = s.history().unwrap().len();
        assert_eq!(s.split_scheduled(parent, p).unwrap(), ids);
        assert_eq!(s.history().unwrap().len(), len);
    }
}

struct StageIncrement {
    root: PathBuf,
    reads: Arc<Mutex<Vec<String>>>,
}

impl WorkerBackend for StageIncrement {
    fn execute(&self, job: &Job, hooks: &dyn ExecutionHooks) -> Result<String, BoxError> {
        if !job.edict.seal.capabilities.contains(&Capability::WriteFile) {
            return Ok("summary".into());
        }
        let before = std::fs::read_to_string(self.root.join("value.txt"))?;
        self.reads.lock().unwrap().push(before.clone());
        let after = format!("{}\n", before.trim().parse::<u32>()? + 4);
        hooks.tool_invoked(
            "write_file",
            &serde_json::json!({"path":"value.txt","content":after}).to_string(),
            true,
        );
        hooks.changes_proposed(vec![FileChange {
            path: "value.txt".into(),
            before: Some(before),
            after,
        }]);
        Ok("staged".into())
    }
}

#[tokio::test]
async fn scheduled_staged_handoff_requires_apply_and_survives_reload() {
    for (discard, scheduled) in [(false, false), (true, false), (false, true), (true, true)] {
        let root = std::env::temp_dir().join(format!(
            "ziyuan-scheduled-{}-{discard}-{scheduled}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("value.txt"), "7\n").unwrap();
        let mut store = MemoryEventLog::default();
        let mut s = Session::found(&mut store).unwrap();
        let parent = s
            .issue(Department::Works, "increment twice".into(), seal(true))
            .unwrap()
            .id;
        // Exercise the existing automatic write-split public interface too.
        let ids = if scheduled {
            s.split_scheduled(parent, two()).unwrap()
        } else {
            s.split(
                parent,
                vec![(AgentId(4), "first".into()), (AgentId(4), "second".into())],
            )
            .unwrap()
        };
        let reads = Arc::new(Mutex::new(Vec::new()));
        let scheduler = Scheduler::new(
            Arc::new(StageIncrement {
                root: root.clone(),
                reads: reads.clone(),
            }),
            Limits {
                max_concurrent: 4,
                timeout: Duration::from_secs(5),
            },
        );
        s.run_open(&scheduler, &CancelToken::new(), |_| {})
            .await
            .unwrap();
        assert_eq!(
            *reads.lock().unwrap(),
            ["7\n"],
            "downstream must not read unapproved upstream output or stale project data"
        );
        assert!(matches!(
            s.court().task(ids[0]).unwrap().status,
            TaskStatus::Completed { .. }
        ));
        assert_eq!(
            s.court()
                .task(ids[0])
                .unwrap()
                .changes
                .as_ref()
                .unwrap()
                .state,
            ChangeState::Proposed
        );
        assert_eq!(
            std::fs::read_to_string(root.join("value.txt")).unwrap(),
            "7\n"
        );
        assert_eq!(
            s.court().blocked_by(s.court().task(ids[1]).unwrap()),
            Some(ids[0])
        );
        drop(s);
        let mut s = Session::open(&mut store).unwrap();
        s.run(&[ids[1]], &scheduler, &CancelToken::new(), |_| {})
            .await
            .unwrap();
        assert_eq!(*reads.lock().unwrap(), ["7\n"]);
        if discard {
            s.discard_changes(ids[0]).unwrap();
        } else {
            s.apply_changes(ids[0], &root, vec![]).unwrap();
        }
        drop(s);
        let mut s = Session::open(&mut store).unwrap();
        assert_eq!(
            s.court().blocked_by(s.court().task(ids[1]).unwrap()),
            discard.then_some(ids[0])
        );
        s.run_open(&scheduler, &CancelToken::new(), |_| {})
            .await
            .unwrap();
        if discard {
            assert_eq!(*reads.lock().unwrap(), ["7\n"]);
            assert_eq!(s.court().task(ids[1]).unwrap().status, TaskStatus::Pending);
        } else {
            assert_eq!(*reads.lock().unwrap(), ["7\n", "11\n"]);
            assert_eq!(
                s.court()
                    .task(ids[1])
                    .unwrap()
                    .changes
                    .as_ref()
                    .unwrap()
                    .files[0]
                    .before
                    .as_deref(),
                Some("11\n")
            );
            assert_eq!(
                std::fs::read_to_string(root.join("value.txt")).unwrap(),
                "11\n",
                "second proposal is still unapproved"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
