//! Public accounting regressions using replay-validated historical events.
use ziyuan_app::translation::translation_usage;
use ziyuan_core::{
    AgentId, BudgetScope, Capability, CourtEvent, Department, GovernanceMode, Price, Seal, TaskId,
    TokenUsage, TranslationKind, TranslationLink,
};
use ziyuan_governance::{Court, founding_events};

const PROJECT: &str = "0123456789abcdef0123456789abcdef";
const BLOCK: &str = "c0001-b0001";
struct Ledger {
    court: Court,
    events: Vec<CourtEvent>,
}
impl Ledger {
    fn new() -> Self {
        let events = founding_events(47);
        Self {
            court: Court::replay(&events).unwrap(),
            events,
        }
    }
    fn append(&mut self, event: CourtEvent) {
        self.court.apply(&event).unwrap();
        self.events.push(event);
    }
    fn issue(&mut self, kind: TranslationKind, project: &str) -> TaskId {
        let path = format!("translations/{project}/{BLOCK}/");
        let mut capabilities = [Capability::ReadFile]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if kind.writes() {
            capabilities.insert(Capability::WriteFile);
        }
        let edict = self
            .court
            .new_edict(
                Department::Works,
                "translate fixture".into(),
                Seal {
                    capabilities,
                    scope: vec![path.clone()],
                    mcp: Default::default(),
                },
            )
            .unwrap();
        let id = edict.id;
        self.append(CourtEvent::EdictIssued(Box::new(edict)));
        let event = self
            .court
            .propose_translation_link(
                id,
                TranslationLink {
                    project: project.into(),
                    block: BLOCK.into(),
                    kind,
                    source_hash: "a".repeat(64),
                    context_version: 0,
                    output_path: if kind.writes() {
                        format!("{path}zh.json")
                    } else {
                        String::new()
                    },
                    zhupi_hash: if kind.writes() {
                        "b".repeat(64)
                    } else {
                        String::new()
                    },
                },
            )
            .unwrap();
        self.append(event);
        id
    }
    fn assign(&mut self, task: TaskId, model: Option<&str>) -> AgentId {
        let worker = self.court.draft_candidates(Department::Works)[0].0.id;
        self.append(CourtEvent::TaskAssigned {
            task,
            worker,
            model: model.map(str::to_owned),
        });
        worker
    }
    fn usage(&mut self, task: TaskId, input: u64, output: u64) {
        self.append(CourtEvent::ModelUsage {
            task,
            usage: TokenUsage { input, output },
            trace: Some("shared-retry-trace".into()),
        });
    }
}

fn price(model: &str) -> Option<(Price, &'static str)> {
    let (input_per_million, output_per_million, currency) = match model {
        "a" => (2.0, 11.0, "CNY"),
        "b" => (7.0, 3.0, "USD"),
        _ => return None,
    };
    Some((
        Price {
            input_per_million,
            output_per_million,
            currency: currency.into(),
        },
        "fixture rates",
    ))
}

#[test]
fn mixed_attempts_and_governance_are_priced_from_original_events_not_latest_model() {
    let mut f = Ledger::new();
    f.append(CourtEvent::InstitutionReformed(GovernanceMode::Cabinet));
    let task = f.issue(TranslationKind::Translate, PROJECT);
    let worker = f.court.draft_candidates(Department::Works)[0].0.id;
    for (model, input, output) in [("a", 100, 9), ("b", 31, 7)] {
        let event = f
            .court
            .propose_draft(
                task,
                worker,
                "draft".into(),
                Some(model.into()),
                TokenUsage { input, output },
            )
            .unwrap();
        f.append(event);
    }
    f.append(f.court.propose_approval(task).unwrap());
    f.append(CourtEvent::BudgetSet {
        scope: BudgetScope::Task(task),
        tokens: Some(10000),
    });
    f.assign(task, Some("a"));
    f.usage(task, 1300, 73);
    // An interrupted task can be reassigned to a different model.
    f.assign(task, Some("b"));
    f.usage(task, 200, 41);
    f.append(CourtEvent::ToolInvoked {
        task,
        tool: "read_file".into(),
        args_json: "{}".into(),
        ok: false,
    });
    f.append(CourtEvent::TaskFailed {
        task,
        reason: "fixture timeout".into(),
    });
    let report = translation_usage(&f.court, &f.events, PROJECT, price);
    let row = &report.tasks[0];
    assert_eq!(row.attempts, 2);
    assert_eq!(row.budget, Some(10000));
    assert_eq!(row.status, "failed");
    assert!(row.usage_reconciled);
    assert_eq!(
        row.recorded_execution_usage,
        Some(TokenUsage {
            input: 1500,
            output: 114
        })
    );
    assert_eq!(
        report.totals.recorded_usage,
        Some(TokenUsage {
            input: 1631,
            output: 130
        })
    );
    // Independent arithmetic: CNY (1400×2 + 82×11)/1M; USD (231×7 + 48×3)/1M.
    assert!((report.totals.estimates["CNY"] - 0.003702).abs() < 1e-12);
    assert!((report.totals.estimates["USD"] - 0.001761).abs() < 1e-12);
    assert_eq!(report.totals.estimates.len(), 2);
    assert_eq!(report.totals.usage_events, 2); // Same trace is not deduplicated.
    assert_eq!(report.totals.tool_calls, 1);
    assert_eq!(report.totals.tool_failures, 1);
    assert_eq!(row.charges.len(), 4);
    assert!(
        row.charges
            .iter()
            .all(|c| c.rate.as_ref().unwrap().source == "fixture rates")
    );
    assert_eq!(report.stages.len(), 4);
    assert_eq!(
        report.stages[0].totals.recorded_usage,
        report.totals.recorded_usage
    );
    assert_eq!(
        f.court.task(task).unwrap().usage,
        report.totals.recorded_usage.unwrap()
    );
}

#[test]
fn missing_usage_is_unknown_but_reported_zero_is_known_and_unknown_rates_are_not_free() {
    let mut f = Ledger::new();
    let missing = f.issue(TranslationKind::ReviewFidelity, PROJECT);
    let worker = f.assign(missing, Some("a"));
    f.append(CourtEvent::TaskCompleted {
        task: missing,
        worker,
        summary: "no reported usage".into(),
    });
    let zero = f.issue(TranslationKind::ReviewStyle, PROJECT);
    f.assign(zero, Some("a"));
    f.usage(zero, 0, 0);
    let unknown = f.issue(TranslationKind::Revise, PROJECT);
    f.assign(unknown, Some("unknown-price"));
    f.usage(unknown, 71, 13);
    let legacy = f.issue(TranslationKind::Translate, PROJECT);
    f.assign(legacy, None);
    f.usage(legacy, 19, 2);
    let other = f.issue(
        TranslationKind::Translate,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    f.assign(other, Some("a"));
    f.usage(other, 1_000_000, 500_000);
    let report = translation_usage(&f.court, &f.events, PROJECT, price);
    assert_eq!(report.tasks.len(), 4);
    assert!(report.tasks.iter().all(|t| t.usage_reconciled));
    assert!(report.tasks[0].recorded_execution_usage.is_none());
    assert!(report.tasks[0].totals.recorded_usage.is_none());
    assert!(report.tasks[0].totals.estimates.is_empty());
    assert_eq!(
        report.tasks[1].recorded_execution_usage,
        Some(TokenUsage::default())
    );
    assert_eq!(report.tasks[1].totals.estimates["CNY"], 0.0);
    assert_eq!(
        report.totals.unpriced_usage,
        TokenUsage {
            input: 90,
            output: 15
        }
    );
    assert_eq!(report.totals.tasks_without_execution_usage, 1);
    assert_eq!(report.totals.usage_events, 3);
    assert_eq!(report.stages[1].totals.recorded_usage, None);
    assert_eq!(
        report.stages[2].totals.recorded_usage,
        Some(TokenUsage::default())
    );
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(
        json["tasks"][0]["recorded_execution_usage"],
        serde_json::Value::Null
    );
    assert_eq!(
        json["tasks"][2]["charges"][0]["estimated_cost"],
        serde_json::Value::Null
    );
    assert_eq!(
        json["tasks"][3]["charges"][0]["model"],
        serde_json::Value::Null
    );
    assert!(report.notes.join(" ").contains("HTTP 请求总数未知"));
}
