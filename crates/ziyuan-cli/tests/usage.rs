//! Public CLI accounting, backed by real projects and replay-validated fixture events.
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};
use ziyuan_app::{
    Session,
    text_project::{NewProject, Project, Provenance, TextProjects},
};
use ziyuan_core::{BudgetScope, CourtEvent, FileChange, TaskId, TokenUsage};
use ziyuan_governance::Court;
use ziyuan_storage::{EventLog, EventStore};

const BLOCK: &str = "c0001-b0001";
struct Fixture {
    root: PathBuf,
    project: Project,
    other: Project,
}
impl Fixture {
    fn new(founded: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "ziyuan-cli-usage-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        if founded {
            Session::found(EventLog::new(&root)).unwrap();
        }
        let texts = TextProjects::new(&root);
        let mut projects = Vec::new();
        for title in ["usage target", "unrelated project"] {
            let project = texts
                .create(NewProject {
                    title: title.into(),
                    source_language: "en".into(),
                    target_language: "zh-CN".into(),
                    provenance: Provenance::default(),
                })
                .unwrap();
            let path = root.join(format!("{}.txt", project.id));
            fs::write(&path, "A legal original paragraph.").unwrap();
            projects.push(texts.import(&project.id, &[path]).unwrap());
        }
        Self {
            root,
            project: projects.remove(0),
            other: projects.remove(0),
        }
    }
    fn session(&self) -> Session<EventLog> {
        Session::open(EventLog::new(&self.root)).unwrap()
    }
    fn append(&self, events: Vec<CourtEvent>) {
        let mut log = EventLog::new(&self.root);
        let history = log.load().unwrap();
        let mut court = Court::replay(&history).unwrap();
        for event in &events {
            court.apply(event).unwrap();
        }
        log.append(history.len(), &events).unwrap();
    }
    fn complete(&self, task: TaskId, model: &str, usage: TokenUsage, changes: Vec<FileChange>) {
        let session = self.session();
        let worker = session
            .court()
            .route(&session.court().task(task).unwrap().edict)
            .unwrap()
            .worker;
        let mut events = vec![
            CourtEvent::TaskAssigned {
                task,
                worker,
                model: Some(model.into()),
            },
            CourtEvent::ToolInvoked {
                task,
                tool: "read_file".into(),
                args_json: "{}".into(),
                ok: false,
            },
            CourtEvent::ToolInvoked {
                task,
                tool: "read_file".into(),
                args_json: "{}".into(),
                ok: true,
            },
            CourtEvent::ModelUsage {
                task,
                usage,
                trace: None,
            },
        ];
        if !changes.is_empty() {
            events.push(CourtEvent::ChangesProposed { task, changes });
        }
        events.push(CourtEvent::TaskCompleted {
            task,
            worker,
            summary: "fixture completion, no model request".into(),
        });
        self.append(events);
    }
    fn populate(&self) -> (TaskId, TaskId) {
        let writer = self
            .session()
            .issue_translation(&self.root, &self.project.id, BLOCK)
            .unwrap()
            .id;
        let mut draft = self.project.chapters[0].blocks[0].clone();
        draft.paragraphs[0].text = "合法译稿。".into();
        self.complete(
            writer,
            "deepseek-v4-flash",
            TokenUsage {
                input: 1000,
                output: 200,
            },
            vec![FileChange {
                path: format!("translations/{}/{BLOCK}/zh.json", self.project.id),
                before: None,
                after: String::from_utf8(draft.serialized().unwrap()).unwrap(),
            }],
        );
        assert!(
            self.session()
                .apply_changes(writer, &self.root, vec![])
                .unwrap()
                .is_empty()
        );
        let reviews = self
            .session()
            .issue_review(&self.root, &self.project.id, BLOCK)
            .unwrap();
        self.complete(
            reviews[0].id,
            "other-priced-model",
            TokenUsage {
                input: 100,
                output: 20,
            },
            vec![],
        );
        self.complete(
            reviews[1].id,
            "deepseek-v4-pro",
            TokenUsage {
                input: 200,
                output: 30,
            },
            vec![],
        );
        let failed = self
            .session()
            .issue_revise(&self.root, &self.project.id, BLOCK)
            .unwrap()
            .id;
        let session = self.session();
        let worker = session
            .court()
            .route(&session.court().task(failed).unwrap().edict)
            .unwrap()
            .worker;
        self.append(vec![
            CourtEvent::BudgetSet {
                scope: BudgetScope::Task(failed),
                tokens: Some(12345),
            },
            CourtEvent::TaskAssigned {
                task: failed,
                worker,
                model: None,
            },
            CourtEvent::TaskFailed {
                task: failed,
                reason: "fixture failed without provider usage".into(),
            },
        ]);
        let foreign = self
            .session()
            .issue_translation(&self.root, &self.other.id, BLOCK)
            .unwrap()
            .id;
        self.complete(
            foreign,
            "deepseek-v4-flash",
            TokenUsage {
                input: 9000000,
                output: 1000000,
            },
            vec![],
        );
        // Session::open may refresh derived huidian; prewarm before read-only snapshots.
        drop(self.session());
        (writer, failed)
    }
    fn run(&self, args: &[&str], price: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ziyuan"));
        command.current_dir(&self.root).args(args);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("ZIYUAN_") {
                command.env_remove(key);
            }
        }
        if let Some(price) = price {
            command.env("ZIYUAN_LLM_PRICE", price);
        }
        command.output().unwrap()
    }
    fn json(&self, project: &str, price: Option<&str>) -> Value {
        let output = self.run(&["text", "usage", project, "--json"], price);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().into(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

#[test]
fn usage_is_project_scoped_four_stage_read_only_and_missing_is_unknown() {
    let f = Fixture::new(true);
    let (_, failed) = f.populate();
    let before = snapshot(&f.root);
    let view = f.json(&f.project.id, None);
    assert_eq!(view["project"], f.project.id);
    let kinds: Vec<_> = view["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        ["translate", "review_fidelity", "review_style", "revise"]
    );
    assert_eq!(view["tasks"].as_array().unwrap().len(), 4);
    assert_eq!(view["totals"]["tasks"], 4);
    assert_eq!(view["totals"]["recorded_usage"]["input"], 1300);
    assert_eq!(view["totals"]["recorded_usage"]["output"], 250);
    assert_eq!(view["totals"]["tool_calls"], 6);
    assert_eq!(view["totals"]["tool_failures"], 3);
    assert_eq!(view["totals"]["usage_events"], 3);
    assert_eq!(view["totals"]["tasks_without_execution_usage"], 1);
    assert_eq!(view["totals"]["unpriced_usage"]["input"], 100);
    let task = view["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task"] == failed.0)
        .unwrap();
    assert_eq!(task["status"], "failed");
    assert_eq!(task["attempts"], 1);
    assert_eq!(task["budget"], 12345);
    assert!(task["recorded_execution_usage"].is_null());
    assert!(task["totals"]["recorded_usage"].is_null());
    let other = f.json(&f.other.id, None);
    assert_eq!(other["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(other["totals"]["recorded_usage"]["input"], 9000000);
    let human = f.run(&["text", "usage", &f.project.id], None);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    for phrase in [
        "已记录承办用量=未知",
        "本件预算=12345",
        "工具调用=",
        "工具失败=",
        "用量记录=",
        "未定价 tokens",
        "deepseek-v4-flash",
        "费率",
        "来源",
        "非账单",
        "未报告用量视为未知，不按零计算",
        "历史 HTTP 请求总数未知",
        "各币种不合并",
        "历史精确耗时未知",
        "未减少词义核对、朱批复核、审校或人工分批验收",
    ] {
        assert!(human.contains(phrase), "missing {phrase}: {human}");
    }
    assert_eq!(
        snapshot(&f.root),
        before,
        "usage modified workspace bytes or paths"
    );
}

#[test]
fn price_provenance_and_currencies_are_not_combined() {
    let f = Fixture::new(true);
    f.populate();
    let view = f.json(&f.project.id, Some("3,9,USD"));
    let estimates = view["totals"]["estimates"].as_object().unwrap();
    assert_eq!(estimates.len(), 2);
    assert!(estimates.contains_key("USD"));
    assert!(estimates.contains_key("元"));
    assert_eq!(view["totals"]["unpriced_usage"]["input"], 0);
    let charges: Vec<_> = view["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|t| t["charges"].as_array().unwrap())
        .collect();
    let builtin = charges
        .iter()
        .find(|c| c["model"] == "deepseek-v4-flash")
        .unwrap();
    assert!(
        builtin["rate"]["source"]
            .as_str()
            .unwrap()
            .contains("DeepSeek")
    );
    assert_eq!(builtin["rate"]["currency"], "元");
    let env = charges
        .iter()
        .find(|c| c["model"] == "other-priced-model")
        .unwrap();
    assert!(
        env["rate"]["source"]
            .as_str()
            .unwrap()
            .contains("ZIYUAN_LLM_PRICE")
    );
    assert_eq!(env["rate"]["currency"], "USD");
    assert_eq!(env["rate"]["input_per_million"], 3.0);
    let human = f.run(&["text", "usage", &f.project.id], Some("3,9,USD"));
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("USD"));
    assert!(human.contains("元"));
    assert!(human.contains("ZIYUAN_LLM_PRICE"));
}

#[test]
fn usage_rejects_arguments_checks_project_and_does_not_found_court() {
    let f = Fixture::new(false);
    let before = snapshot(&f.root);
    for args in [
        vec!["text", "usage"],
        vec!["text", "usage", &f.project.id, "extra"],
        vec!["text", "usage", "--project", &f.project.id],
        vec!["text", "usage", ""],
        vec!["text", "usage", "-x"],
        vec!["text", "usage", &f.project.id, "--write"],
    ] {
        let output = f.run(&args, None);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
    let missing = f.run(&["text", "usage", "nonexistent", "--json"], None);
    assert_eq!(missing.status.code(), Some(1));
    let offline = f.run(&["text", "usage", &f.project.id, "--json"], None);
    assert_eq!(offline.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&offline.stderr).contains("ziyuan init"));
    assert_eq!(snapshot(&f.root), before);
    // Once founded, an empty project reports no recorded tokens, not a fabricated zero.
    Session::found(EventLog::new(&f.root)).unwrap();
    drop(f.session());
    let empty = f.json(&f.project.id, None);
    assert_eq!(empty["stages"].as_array().unwrap().len(), 4);
    assert!(empty["totals"]["recorded_usage"].is_null());
    assert!(empty["tasks"].as_array().unwrap().is_empty());
}
