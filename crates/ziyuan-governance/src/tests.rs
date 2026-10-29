use super::*;
use ziyuan_core::{
    Agent, AgentId, Capability, CensorVerdict, CourtEvent, DagNode, Department, Edict, Evidence,
    GovernanceMode, Office, Seal, TaskId, TokenUsage, Verdict,
};
#[test]
fn reform_changes_route() {
    let mut court = Court::replay(&founding_events(1)).unwrap();
    let edict = court
        .new_edict(Department::Works, "检查源码".into(), Seal::default())
        .unwrap();
    assert!(
        court
            .route(&edict)
            .unwrap()
            .path
            .iter()
            .any(|p| p.contains("丞相"))
    );
    court
        .apply(&court.propose_reform(GovernanceMode::DirectRule).unwrap())
        .unwrap();
    assert!(
        !court
            .route(&edict)
            .unwrap()
            .path
            .iter()
            .any(|p| p.contains("丞相"))
    );
    court
        .apply(&court.propose_reform(GovernanceMode::Cabinet).unwrap())
        .unwrap();
    assert!(court.route(&edict).is_err());
    court
        .apply(&court.propose_dismissal(AgentId(6)).unwrap())
        .unwrap();
    court
        .apply(
            &court
                .propose_appointment(AgentId(6), Office::FirstGrandSecretary)
                .unwrap(),
        )
        .unwrap();
    assert!(
        court
            .route(&edict)
            .unwrap()
            .path
            .iter()
            .any(|p| p.contains("首辅"))
    );
}
#[test]
fn deputies_act_for_vacant_heads_and_ranks_follow_the_mode() {
    let mut court = Court::replay(&founding_events(1)).unwrap();
    let works = |court: &Court| {
        let edict = court
            .new_edict(Department::Works, "查".into(), Seal::default())
            .unwrap();
        court.route(&edict).map(|r| r.path)
    };
    let apply = |court: &mut Court,
                 propose: &dyn Fn(&Court) -> Result<CourtEvent, GovernanceError>| {
        let event = propose(court).unwrap();
        court.apply(&event).unwrap()
    };
    // 丞相制：右丞相（#1）罢免后，左丞相署理；尚书（#2）罢免后，侍郎署理。
    apply(&mut court, &|c| c.propose_dismissal(AgentId(1)));
    assert_eq!(
        works(&court).unwrap_err(),
        GovernanceError::Vacant(Office::Chancellor)
    );
    apply(&mut court, &|c| {
        c.propose_appointment(AgentId(1), Office::ViceChancellor)
    });
    apply(&mut court, &|c| c.propose_dismissal(AgentId(2)));
    assert_eq!(
        works(&court).unwrap_err(),
        GovernanceError::Vacant(Office::Minister(Department::Works))
    );
    apply(&mut court, &|c| {
        c.propose_appointment(AgentId(2), Office::ViceMinister(Department::Works))
    });
    let path = works(&court).unwrap();
    let names: Vec<String> = court.agents().map(|a| a.name.clone()).collect();
    let name = |id: usize| names[id - 1].clone();
    assert_eq!(
        path,
        [
            "皇帝".to_owned(),
            format!("中书左丞相·{}（署理）", name(1)),
            format!("工部侍郎·{}（署理）", name(2)),
            format!("工部郎中·{}", name(4)),
        ]
    );
    let minister = Office::Minister(Department::Works);
    assert_eq!(minister.rank(court.mode()), "正三品");

    // 废相亲政：中书省官职随之撤销，六部官职保留并升为明制品级；内阁官职不可任命。
    apply(&mut court, &|c| {
        c.propose_reform(GovernanceMode::DirectRule)
    });
    assert_eq!(court.office_of(AgentId(1)), None);
    assert_eq!(
        court.office_of(AgentId(2)),
        Some(Office::ViceMinister(Department::Works))
    );
    assert_eq!(minister.rank(court.mode()), "正二品");
    assert_eq!(works(&court).unwrap().len(), 3);
    assert_eq!(
        court.propose_appointment(AgentId(1), Office::SecondGrandSecretary),
        Err(GovernanceError::OfficeNotInMode(
            Office::SecondGrandSecretary
        ))
    );
    // 内阁制：次辅在首辅空缺时署理。
    apply(&mut court, &|c| c.propose_reform(GovernanceMode::Cabinet));
    apply(&mut court, &|c| {
        c.propose_appointment(AgentId(1), Office::SecondGrandSecretary)
    });
    assert_eq!(
        works(&court).unwrap()[1],
        format!("内阁次辅·{}（署理）", name(1))
    );
}
#[test]
fn abolition_purges_chancellors_and_cabinet_edicts_need_approval() {
    let mut court = Court::replay(&founding_events(1)).unwrap();
    let apply = |events: Vec<CourtEvent>, court: &mut Court| {
        for e in events {
            court.apply(&e).unwrap();
        }
    };
    // 革去右丞相 #1 并废相；被革者不能再任官，也不入荐举（一键朱元璋见本测试末尾）。
    apply(
        vec![
            court.propose_purge(AgentId(1)).unwrap(),
            court.propose_reform(GovernanceMode::DirectRule).unwrap(),
        ],
        &mut court,
    );
    assert!(court.is_purged(AgentId(1)) && court.office_of(AgentId(1)).is_none());
    assert!(!court.is_purged(AgentId(2)));
    assert_eq!(
        court.propose_abolition(),
        Err(GovernanceError::NoChancellery)
    );
    apply(
        vec![court.propose_reform(GovernanceMode::Cabinet).unwrap()],
        &mut court,
    );
    assert_eq!(
        court.propose_appointment(AgentId(1), Office::FirstGrandSecretary),
        Err(GovernanceError::Purged(AgentId(1)))
    );
    assert!(
        court
            .recommend(Office::FirstGrandSecretary)
            .unwrap()
            .iter()
            .all(|c| c.agent.id != AgentId(1))
    );
    assert_eq!(
        court.propose_purge(AgentId(1)),
        Err(GovernanceError::Purged(AgentId(1)))
    );

    // 内阁制：未批红的诏令不可执行；批红一次后可执行，不能重复批红。
    apply(
        vec![court.propose_dismissal(AgentId(6)).unwrap()],
        &mut court,
    );
    apply(
        vec![
            court
                .propose_appointment(AgentId(6), Office::FirstGrandSecretary)
                .unwrap(),
        ],
        &mut court,
    );
    let edict = court
        .new_edict(Department::Works, "查".into(), Seal::default())
        .unwrap();
    let id = edict.id;
    apply(vec![CourtEvent::EdictIssued(Box::new(edict))], &mut court);
    assert!(court.runnable_tasks().is_empty());
    assert_eq!(court.open_tasks(), [id]);
    // 票拟只能拟派本部在任官员；拟派尚书后，派工改给尚书（路线注明票拟），不再按旧例给郎中。
    let none = TokenUsage::default();
    assert_eq!(
        court.propose_draft(id, AgentId(3), String::new(), None, none),
        Err(GovernanceError::NotEligible {
            worker: AgentId(3),
            department: Department::Works
        })
    );
    assert_eq!(
        court.route(&court.task(id).unwrap().edict).unwrap().worker,
        AgentId(4)
    );
    let used = TokenUsage {
        input: 100,
        output: 10,
    };
    apply(
        vec![
            court
                .propose_draft(id, AgentId(2), "尚书亲办".into(), Some("pro".into()), used)
                .unwrap(),
        ],
        &mut court,
    );
    let route = court.route_task(court.task(id).unwrap()).unwrap();
    assert_eq!(route.worker, AgentId(2));
    assert!(
        route.path.last().unwrap().ends_with("（票拟）"),
        "{:?}",
        route.path
    );
    let task = court.task(id).unwrap();
    assert_eq!(
        (task.usage, task.draft.as_ref().unwrap().usage),
        (used, used)
    );
    apply(vec![court.propose_approval(id).unwrap()], &mut court);
    assert_eq!(court.runnable_tasks(), [id]);
    assert_eq!(
        court.propose_approval(id),
        Err(GovernanceError::AlreadyApproved(id))
    );
    // 其他制度不批红，诏令直接可执行。
    apply(
        vec![court.propose_reform(GovernanceMode::DirectRule).unwrap()],
        &mut court,
    );
    assert_eq!(
        court.propose_approval(id),
        Err(GovernanceError::ApprovalNotNeeded)
    );

    // 一键朱元璋：复设丞相后一键株连——在任官员（含六部）全部革职，白身 #11 幸免。
    apply(
        vec![
            court.propose_reform(GovernanceMode::Chancellor).unwrap(),
            CourtEvent::AgentRegistered(Agent {
                id: AgentId(11),
                name: "白身".into(),
                speciality: Department::Works,
            }),
        ],
        &mut court,
    );
    apply(
        vec![
            court
                .propose_appointment(AgentId(6), Office::Chancellor)
                .unwrap(),
        ],
        &mut court,
    );
    let holders: Vec<_> = court
        .agents()
        .filter(|a| court.office_of(a.id).is_some())
        .map(|a| a.id)
        .collect();
    assert!(holders.len() >= 3, "{holders:?}");
    apply(court.propose_abolition().unwrap(), &mut court);
    assert_eq!(court.mode(), GovernanceMode::DirectRule);
    assert!(holders.iter().all(|&a| court.is_purged(a)));
    assert!(court.agents().all(|a| court.office_of(a.id).is_none()));
    assert!(!court.is_purged(AgentId(11)));
}
#[test]
fn censorate_reviews_completed_edicts_and_feeds_the_service_record() {
    let mut court = Court::replay(&founding_events(1)).unwrap();
    let mut apply = |events: Vec<CourtEvent>| {
        for e in events {
            court.apply(&e).unwrap();
        }
    };
    let edict = Edict {
        id: TaskId(1),
        department: Department::Works,
        text: "查".into(),
        seal: Seal::default(),
    };
    apply(vec![
        CourtEvent::EdictIssued(Box::new(edict)),
        CourtEvent::TaskAssigned {
            task: TaskId(1),
            worker: AgentId(4),
            model: None,
        },
    ]);
    let report = |censor: u64, verdict, evidence| CourtEvent::CensorReported {
        task: TaskId(1),
        censor: AgentId(censor),
        verdict,
        note: "核".into(),
        evidence,
        model: None,
        usage: TokenUsage::default(),
    };
    // 未完成不复核；完成后，非御史不能复核，经手者任御史也须回避。
    assert_eq!(
        court.propose_censure_check(TaskId(1), AgentId(4)),
        Err(GovernanceError::NotCompleted(TaskId(1)))
    );
    for e in [
        CourtEvent::TaskCompleted {
            task: TaskId(1),
            worker: AgentId(4),
            summary: "src/lib.rs:3".into(),
        },
        CourtEvent::OfficialDismissed { agent: AgentId(4) },
        CourtEvent::OfficialAppointed {
            agent: AgentId(4),
            office: Office::Censor,
        },
    ] {
        court.apply(&e).unwrap();
    }
    assert_eq!(
        court.propose_censure_check(TaskId(1), AgentId(5)),
        Err(GovernanceError::NotCensor(AgentId(5)))
    );
    assert_eq!(
        court.apply(&report(4, CensorVerdict::Endorsed, Evidence::default())),
        Err(GovernanceError::SelfReview(AgentId(4)))
    );
    // 御史不随改制撤销；改由吏部郎中 #5 任御史后背书，证据属实。
    court
        .apply(&CourtEvent::InstitutionReformed(GovernanceMode::Cabinet))
        .unwrap();
    assert_eq!(court.office_of(AgentId(4)), Some(Office::Censor));
    for e in [
        CourtEvent::OfficialDismissed { agent: AgentId(4) },
        CourtEvent::OfficialDismissed { agent: AgentId(5) },
        CourtEvent::OfficialAppointed {
            agent: AgentId(5),
            office: Office::Censor,
        },
    ] {
        court.apply(&e).unwrap();
    }
    let sound = Evidence {
        cited: 1,
        missing: 0,
        tests: None,
    };
    court
        .apply(&report(5, CensorVerdict::Endorsed, sound))
        .unwrap();
    assert_eq!(
        court.apply(&report(5, CensorVerdict::Impeached, sound)),
        Err(GovernanceError::AlreadyCensured(TaskId(1)))
    );
    let record = court.service_record(AgentId(4));
    assert_eq!(
        (
            record.endorsed,
            record.impeached,
            record.verified,
            record.unreviewed
        ),
        (1, 0, 1, 1)
    );
    assert_eq!(record.merit(), 2);
    // 证据：引用有一处查无实据，或测试失败，都不算有据可查。
    let flawed = Evidence {
        cited: 2,
        missing: 1,
        tests: Some(true),
    };
    let failing = Evidence {
        cited: 0,
        missing: 0,
        tests: Some(false),
    };
    assert!(!flawed.verified() && !failing.verified() && !Evidence::default().verified());
}

#[test]
fn caning_needs_repeated_impeachments_and_lifts_after_redemption_or_pardon() {
    let mut court = Court::replay(&founding_events(1)).unwrap();
    // #6 改任左都御史；#4（工部郎中）承办的诏令由他复核。
    for e in [
        CourtEvent::OfficialDismissed { agent: AgentId(6) },
        CourtEvent::OfficialAppointed {
            agent: AgentId(6),
            office: Office::Censor,
        },
    ] {
        court.apply(&e).unwrap();
    }
    let mut next = 0;
    let mut finish = |court: &mut Court, impeach: bool| {
        next += 1;
        let task = TaskId(next);
        let verdict = if impeach {
            CourtEvent::CensorReported {
                task,
                censor: AgentId(6),
                verdict: CensorVerdict::Impeached,
                note: "不实".into(),
                evidence: Evidence::default(),
                model: None,
                usage: TokenUsage::default(),
            }
        } else {
            CourtEvent::TaskReviewed {
                task,
                verdict: Verdict::Accepted,
                note: String::new(),
            }
        };
        for e in [
            CourtEvent::EdictIssued(Box::new(Edict {
                id: task,
                department: Department::Works,
                text: "办".into(),
                seal: Seal::default(),
            })),
            CourtEvent::TaskAssigned {
                task,
                worker: AgentId(4),
                model: None,
            },
            CourtEvent::TaskCompleted {
                task,
                worker: AgentId(4),
                summary: "毕".into(),
            },
            verdict,
        ] {
            court.apply(&e).unwrap();
        }
    };
    let clerk = AgentId(4);
    let cane = |court: &mut Court| court.apply(&CourtEvent::OfficialCaned { agent: clerk });
    // 弹劾不足两次不能杖；未任官职者无级可降。
    finish(&mut court, true);
    assert!(
        matches!(cane(&mut court), Err(GovernanceError::Court(why)) if why.contains("遭弹劾 1 次"))
    );
    finish(&mut court, true);
    assert!(court.propose_caning(AgentId(2)).is_err());
    cane(&mut court).unwrap();
    assert_eq!(
        (court.demotion(clerk), court.redemption_left(clerk)),
        (1, Some(3))
    );
    // 降级留用期间收回写权；同一批弹劾不能再杖。
    let works = Edict {
        id: TaskId(99),
        department: Department::Works,
        text: "改".into(),
        seal: Seal {
            capabilities: [
                Capability::ReadFile,
                Capability::WriteFile,
                Capability::RunTests,
            ]
            .into(),
            scope: vec![".".into()],
            mcp: Default::default(),
        },
    };
    let seal = court.effective_seal(clerk, &works).unwrap();
    assert_eq!(seal.capabilities, [Capability::ReadFile].into());
    assert!(cane(&mut court).is_err());
    // 未立功又遭两次弹劾，再杖则再降一级，立功的件数重新算起。
    finish(&mut court, false);
    finish(&mut court, false);
    assert_eq!(court.redemption_left(clerk), Some(1));
    finish(&mut court, true);
    finish(&mut court, true);
    cane(&mut court).unwrap();
    assert_eq!(
        (court.demotion(clerk), court.redemption_left(clerk)),
        (2, Some(3))
    );
    // 宽宥即复；再宽宥报错。
    court
        .apply(&CourtEvent::OfficialPardoned { agent: clerk })
        .unwrap();
    assert_eq!(court.demotion(clerk), 0);
    assert_eq!(court.effective_seal(clerk, &works).unwrap(), works.seal);
    assert!(court.propose_pardon(clerk).is_err());
    // 宽宥后再杖从一级起；获采纳三件即戴罪立功，自动复职。
    finish(&mut court, true);
    finish(&mut court, true);
    cane(&mut court).unwrap();
    assert_eq!(court.demotion(clerk), 1);
    for _ in 0..3 {
        finish(&mut court, false);
    }
    assert_eq!(
        (court.demotion(clerk), court.redemption_left(clerk)),
        (0, None)
    );
    assert!(court.propose_pardon(clerk).is_err(), "已立功，无须宽宥");
    assert_eq!(court.service_record(clerk).caned, 3);
}

#[test]
fn split_edicts_run_subtasks_first_then_the_minister_summarizes() {
    let mut court = Court::replay(&founding_events(1)).unwrap();
    let apply = |court: &mut Court, e: CourtEvent| court.apply(&e).unwrap();
    apply(
        &mut court,
        CourtEvent::AgentRegistered(Agent {
            id: AgentId(11),
            name: "新工".into(),
            speciality: Department::Works,
        }),
    );
    apply(
        &mut court,
        CourtEvent::OfficialAppointed {
            agent: AgentId(11),
            office: Office::Clerk(Department::Works),
        },
    );
    let edict = court
        .new_edict(Department::Works, "查全库".into(), Seal::default())
        .unwrap();
    apply(&mut court, CourtEvent::EdictIssued(Box::new(edict)));
    let none = TokenUsage::default();
    let split = |court: &Court, task: u64, plan: &[(u64, &str)]| {
        let plan = plan
            .iter()
            .map(|(w, t)| (AgentId(*w), t.to_string()))
            .collect();
        court.propose_split(TaskId(task), plan, "分办".into(), None, none)
    };
    // 只分一件、分给他部官员（#3 吏部尚书）都不行。
    assert!(matches!(
        split(&court, 1, &[(4, "甲")]),
        Err(GovernanceError::InvalidSplit(_))
    ));
    assert!(matches!(
        split(&court, 1, &[(4, "甲"), (3, "乙")]),
        Err(GovernanceError::NotEligible { .. })
    ));
    let event = split(&court, 1, &[(4, "查 storage"), (11, "查 runtime")]).unwrap();
    apply(&mut court, event);
    // 子诏令 #2、#3 先执行，原诏令等子诏令结束；子诏令不再分办，原诏令不能再分。
    assert_eq!(court.runnable_tasks(), [TaskId(2), TaskId(3)]);
    assert_eq!(court.task(TaskId(3)).unwrap().edict.seal, Seal::default());
    for task in [1, 2] {
        assert_eq!(
            split(&court, task, &[(4, "甲"), (11, "乙")]),
            Err(GovernanceError::CannotSplit(TaskId(task)))
        );
    }
    let route = court.route_task(court.task(TaskId(3)).unwrap()).unwrap();
    assert_eq!(route.worker, AgentId(11));
    assert!(
        route.path.last().unwrap().ends_with("（分办）"),
        "{:?}",
        route.path
    );

    apply(
        &mut court,
        CourtEvent::TaskAssigned {
            task: TaskId(2),
            worker: AgentId(4),
            model: None,
        },
    );
    apply(
        &mut court,
        CourtEvent::TaskCompleted {
            task: TaskId(2),
            worker: AgentId(4),
            summary: "无 unsafe".into(),
        },
    );
    assert_eq!(court.runnable_tasks(), [TaskId(3)]);
    apply(&mut court, CourtEvent::TaskCancelled { task: TaskId(3) });
    // 子诏令全部结束（撤销也算），原诏令派给工部尚书 #2 汇总。
    assert_eq!(court.runnable_tasks(), [TaskId(1)]);
    let route = court.route_task(court.task(TaskId(1)).unwrap()).unwrap();
    assert_eq!(route.worker, AgentId(2));
    assert!(
        route.path.last().unwrap().ends_with("（汇总）"),
        "{:?}",
        route.path
    );

    // 内阁制下子诏令随原诏令批红。
    apply(
        &mut court,
        CourtEvent::InstitutionReformed(GovernanceMode::Cabinet),
    );
    apply(
        &mut court,
        CourtEvent::OfficialDismissed { agent: AgentId(6) },
    );
    apply(
        &mut court,
        CourtEvent::OfficialAppointed {
            agent: AgentId(6),
            office: Office::FirstGrandSecretary,
        },
    );
    let edict = court
        .new_edict(Department::Works, "再查".into(), Seal::default())
        .unwrap();
    let id = edict.id;
    apply(&mut court, CourtEvent::EdictIssued(Box::new(edict)));
    let event = split(&court, id.0, &[(4, "甲"), (11, "乙")]).unwrap();
    apply(&mut court, event);
    // （改制后 #1 本身也须批红。）
    assert!(court.runnable_tasks().is_empty());
    apply(&mut court, CourtEvent::EdictApproved { task: id });
    assert_eq!(court.runnable_tasks(), [TaskId(id.0 + 1), TaskId(id.0 + 2)]);
}

#[test]
fn dag_dependencies_gate_scheduling_and_upstream_failures_block() {
    let mut court = Court::replay(&founding_events(1)).unwrap();
    let apply = |court: &mut Court, e: CourtEvent| court.apply(&e).unwrap();
    apply(
        &mut court,
        CourtEvent::AgentRegistered(Agent {
            id: AgentId(11),
            name: "新工".into(),
            speciality: Department::Works,
        }),
    );
    apply(
        &mut court,
        CourtEvent::OfficialAppointed {
            agent: AgentId(11),
            office: Office::Clerk(Department::Works),
        },
    );
    let edict = court
        .new_edict(Department::Works, "查全库".into(), Seal::default())
        .unwrap();
    apply(&mut court, CourtEvent::EdictIssued(Box::new(edict)));
    let node = |task: u64, deps: &[u64]| DagNode {
        task: TaskId(task),
        dependencies: deps.iter().map(|d| TaskId(*d)).collect(),
        attempts: None,
        deadline: None,
        budget: None,
        artifact: None,
        approval_required: false,
        idempotency_key: None,
    };
    let split_dag = |nodes: Vec<DagNode>| {
        court.propose_split_dag(
            TaskId(1),
            vec![
                (AgentId(4), "甲".into()),
                (AgentId(2), "乙".into()),
                (AgentId(11), "丙".into()),
            ],
            "分办".into(),
            None,
            TokenUsage::default(),
            nodes,
        )
    };
    // 节点不齐、点名他件、依赖自身、依赖圈外、成环、零次数、零度支：皆拒。
    for (why, nodes) in [
        ("节点不齐", vec![node(2, &[]), node(3, &[])]),
        ("点名他件", vec![node(2, &[]), node(3, &[]), node(9, &[])]),
        ("依赖自身", vec![node(2, &[]), node(3, &[3]), node(4, &[])]),
        ("依赖圈外", vec![node(2, &[]), node(3, &[9]), node(4, &[])]),
        ("依赖成环", vec![node(2, &[3]), node(3, &[2]), node(4, &[])]),
        (
            "次数为零",
            vec![
                DagNode {
                    attempts: Some(0),
                    ..node(2, &[])
                },
                node(3, &[]),
                node(4, &[]),
            ],
        ),
        (
            "度支为零",
            vec![
                DagNode {
                    budget: Some(0),
                    ..node(2, &[])
                },
                node(3, &[]),
                node(4, &[]),
            ],
        ),
    ] {
        assert!(
            matches!(split_dag(nodes), Err(GovernanceError::InvalidDag(_))),
            "{why}应被拒"
        );
    }
    // 依例排程：#3 待 #2，#4 待 #2 与 #3；#2 另有度支。
    let events = split_dag(vec![
        DagNode {
            budget: Some(100),
            ..node(2, &[])
        },
        node(3, &[2]),
        node(4, &[2, 3]),
    ])
    .unwrap();
    assert_eq!(events.len(), 2);
    for event in &events {
        apply(&mut court, event.clone());
    }
    assert_eq!(court.task(TaskId(2)).unwrap().budget, Some(100));
    assert_eq!(court.runnable_tasks(), [TaskId(2)]);
    assert_eq!(
        court.blocked_by(court.task(TaskId(3)).unwrap()),
        Some(TaskId(2))
    );
    // #4 依赖 #2 与 #3：最先未成的是 #2。
    assert_eq!(
        court.blocked_by(court.task(TaskId(4)).unwrap()),
        Some(TaskId(2))
    );

    apply(
        &mut court,
        CourtEvent::TaskAssigned {
            task: TaskId(2),
            worker: AgentId(4),
            model: None,
        },
    );
    apply(
        &mut court,
        CourtEvent::TaskCompleted {
            task: TaskId(2),
            worker: AgentId(4),
            summary: "甲成".into(),
        },
    );
    assert_eq!(court.runnable_tasks(), [TaskId(3)]);

    // 上游失败：下游受阻，不再调度；原诏令亦不得汇总。
    apply(
        &mut court,
        CourtEvent::TaskAssigned {
            task: TaskId(3),
            worker: AgentId(2),
            model: None,
        },
    );
    apply(
        &mut court,
        CourtEvent::TaskFailed {
            task: TaskId(3),
            reason: "模型不可用".into(),
        },
    );
    assert!(court.runnable_tasks().is_empty());
    assert_eq!(
        court.blocked_by(court.task(TaskId(4)).unwrap()),
        Some(TaskId(3))
    );
    // 指派计次：#3 曾指派一次。
    assert_eq!(court.task(TaskId(3)).unwrap().attempts, 1);

    // 撤销受阻的子诏令后，原诏令交尚书汇总。
    apply(&mut court, CourtEvent::TaskCancelled { task: TaskId(4) });
    assert_eq!(court.runnable_tasks(), [TaskId(1)]);

    // 未分办无从排程；一件子诏令只排一次。
    let edict = court
        .new_edict(Department::Works, "再查".into(), Seal::default())
        .unwrap();
    let fresh = edict.id;
    apply(&mut court, CourtEvent::EdictIssued(Box::new(edict)));
    assert!(matches!(
        court.apply(&CourtEvent::EdictDag {
            task: fresh,
            nodes: vec![node(fresh.0 + 1, &[])]
        }),
        Err(GovernanceError::InvalidDag(_))
    ));
    assert!(matches!(
        court.apply(&CourtEvent::EdictDag {
            task: TaskId(1),
            nodes: vec![node(2, &[]), node(3, &[]), node(4, &[])]
        }),
        Err(GovernanceError::InvalidDag(_))
    ));
}

#[test]
fn write_subtasks_are_serialized_automatically_and_readonly_ones_are_not() {
    let mut court = Court::replay(&founding_events(1)).unwrap();
    let write = Seal {
        capabilities: [Capability::ReadFile, Capability::WriteFile].into(),
        scope: vec![".".into()],
        mcp: Default::default(),
    };
    let edict = court
        .new_edict(Department::Works, "改全库".into(), write)
        .unwrap();
    court
        .apply(&CourtEvent::EdictIssued(Box::new(edict)))
        .unwrap();
    let events = court
        .propose_split_serial_writes(
            TaskId(1),
            vec![(AgentId(4), "改甲".into()), (AgentId(2), "改乙".into())],
            "御定分办".into(),
            None,
            TokenUsage::default(),
        )
        .unwrap();
    assert_eq!(events.len(), 2);
    let CourtEvent::EdictDag { nodes, .. } = &events[1] else {
        unreachable!("第二件是排程");
    };
    assert_eq!(nodes[0].dependencies, Vec::new());
    assert_eq!(nodes[1].dependencies, [TaskId(2)]);
    for event in events {
        court.apply(&event).unwrap();
    }
    // 写子诏令串行：先 #2 后 #3。
    assert_eq!(court.runnable_tasks(), [TaskId(2)]);

    // 只读诏令无冲突，照旧全并行（不记排程事件）。
    let edict = court
        .new_edict(Department::Works, "再查".into(), Seal::default())
        .unwrap();
    let id = edict.id;
    court
        .apply(&CourtEvent::EdictIssued(Box::new(edict)))
        .unwrap();
    let events = court
        .propose_split_serial_writes(
            id,
            vec![(AgentId(4), "查丙".into()), (AgentId(2), "查丁".into())],
            "御定分办".into(),
            None,
            TokenUsage::default(),
        )
        .unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], CourtEvent::EdictSplit { .. }));
    court.apply(&events[0]).unwrap();
    assert_eq!(
        court.runnable_tasks(),
        [TaskId(2), TaskId(id.0 + 1), TaskId(id.0 + 2)]
    );
}

#[test]
fn dag_approval_required_subtasks_need_their_own_approval() {
    let mut court = Court::replay(&founding_events(1)).unwrap();
    let apply = |court: &mut Court, e: CourtEvent| court.apply(&e).unwrap();
    apply(
        &mut court,
        CourtEvent::InstitutionReformed(GovernanceMode::Cabinet),
    );
    apply(
        &mut court,
        CourtEvent::OfficialDismissed { agent: AgentId(6) },
    );
    apply(
        &mut court,
        CourtEvent::OfficialAppointed {
            agent: AgentId(6),
            office: Office::FirstGrandSecretary,
        },
    );
    let edict = court
        .new_edict(Department::Works, "查全库".into(), Seal::default())
        .unwrap();
    apply(&mut court, CourtEvent::EdictIssued(Box::new(edict)));
    let node = |task: u64, approval_required: bool| DagNode {
        task: TaskId(task),
        dependencies: Vec::new(),
        attempts: None,
        deadline: None,
        budget: None,
        artifact: None,
        approval_required,
        idempotency_key: None,
    };
    let events = court
        .propose_split_dag(
            TaskId(1),
            vec![(AgentId(4), "甲".into()), (AgentId(2), "乙".into())],
            "分办".into(),
            None,
            TokenUsage::default(),
            vec![node(2, false), node(3, true)],
        )
        .unwrap();
    for event in events {
        apply(&mut court, event);
    }
    // 原诏令未批红：全都不动。
    assert!(court.runnable_tasks().is_empty());
    apply(&mut court, CourtEvent::EdictApproved { task: TaskId(1) });
    // #3 须皇帝另行批红。
    assert!(!court.needs_approval(court.task(TaskId(2)).unwrap()));
    assert!(court.needs_approval(court.task(TaskId(3)).unwrap()));
    assert_eq!(court.runnable_tasks(), [TaskId(2)]);
    apply(&mut court, CourtEvent::EdictApproved { task: TaskId(3) });
    assert_eq!(court.runnable_tasks(), [TaskId(2), TaskId(3)]);
}
#[test]
fn cannot_create_two_chancellors() {
    let court = Court::replay(&founding_events(1)).unwrap();
    let mut court = court;
    court
        .apply(&court.propose_dismissal(AgentId(6)).unwrap())
        .unwrap();
    assert!(
        court
            .propose_appointment(AgentId(6), Office::Chancellor)
            .is_err()
    );
}
#[test]
fn cannot_replay_duplicate_appointment() {
    let mut events = founding_events(1);
    events.push(CourtEvent::OfficialAppointed {
        agent: AgentId(2),
        office: Office::Minister(Department::Works),
    });
    assert!(Court::replay(&events).is_err());
}
#[test]
fn replay_applies_same_rules_as_proposals() {
    let mut events = founding_events(1);
    events.push(CourtEvent::OfficialDismissed { agent: AgentId(11) });
    assert_eq!(
        Court::replay(&events).unwrap_err(),
        GovernanceError::NotAppointed(AgentId(11))
    );
    let mut events = founding_events(1);
    events.push(CourtEvent::InstitutionReformed(GovernanceMode::Chancellor));
    assert!(Court::replay(&events).is_err());
}
#[test]
fn libu_counts_only_reviewed_results_and_ranks_by_them() {
    let mut events = founding_events(1);
    events.push(CourtEvent::AgentRegistered(Agent {
        id: AgentId(11),
        name: "新工".into(),
        speciality: Department::Works,
    }));
    let mut next = 1;
    // (承办者, 是否完成, 评定, 失败的工具调用数)
    let mut handle = |worker: u64, ok: bool, verdict: Option<Verdict>, bad_tools: u32| {
        let task = TaskId(next);
        next += 1;
        events.push(CourtEvent::EdictIssued(Box::new(Edict {
            id: task,
            department: Department::Works,
            text: "查".into(),
            seal: Seal::default(),
        })));
        events.push(CourtEvent::TaskAssigned {
            task,
            worker: AgentId(worker),
            model: None,
        });
        events.push(CourtEvent::ModelUsage {
            task,
            usage: TokenUsage {
                input: 100,
                output: 7,
            },
            trace: None,
        });
        for _ in 0..bad_tools {
            events.push(CourtEvent::ToolInvoked {
                task,
                tool: "read_file".into(),
                args_json: "{}".into(),
                ok: false,
            });
        }
        events.push(if ok {
            CourtEvent::TaskCompleted {
                task,
                worker: AgentId(worker),
                summary: "ok".into(),
            }
        } else {
            CourtEvent::TaskFailed {
                task,
                reason: "超时".into(),
            }
        });
        if let Some(verdict) = verdict {
            events.push(CourtEvent::TaskReviewed {
                task,
                verdict,
                note: String::new(),
            });
        }
    };
    // 工部郎中 #4 完成数最多，但多为驳回或未评定；新工（#11）完成少但被采纳。
    handle(4, true, Some(Verdict::Rejected), 0);
    handle(4, true, None, 0);
    handle(4, true, None, 0);
    handle(4, false, None, 2);
    handle(11, true, Some(Verdict::Accepted), 1);
    let court = Court::replay(&events).unwrap();

    assert_eq!(
        court.service_record(AgentId(4)),
        ServiceRecord {
            accepted: 0,
            rejected: 1,
            unreviewed: 2,
            failed: 1,
            tool_calls: 2,
            tool_failures: 2,
            // 4 件（含失败的一件）各 100/7。
            usage: TokenUsage {
                input: 400,
                output: 28,
            },
            ..ServiceRecord::default()
        }
    );
    assert_eq!(court.total_usage().total(), 5 * 107);
    let ranked: Vec<_> = court
        .recommend(Office::Clerk(Department::Works))
        .unwrap()
        .iter()
        .map(|c| c.agent.id.0)
        .collect();
    // 新工居首；工部尚书 #2（无履历）在被驳回过的 #4 之前；吏部官员不入选。
    assert_eq!(ranked, [11, 2, 4]);
    assert!(court.recommend(Office::FirstGrandSecretary).is_err());

    // 只能评定已完成且未评定过的诏令。
    assert_eq!(
        court.propose_review(TaskId(4), Verdict::Accepted, String::new()),
        Err(GovernanceError::NotCompleted(TaskId(4)))
    );
    assert_eq!(
        court.propose_review(TaskId(5), Verdict::Rejected, String::new()),
        Err(GovernanceError::AlreadyReviewed(TaskId(5)))
    );
    assert!(
        court
            .propose_review(TaskId(2), Verdict::Accepted, "好".into())
            .is_ok()
    );
}
#[test]
fn seal_scope_must_stay_inside_project() {
    let court = Court::replay(&founding_events(1)).unwrap();
    let seal = |scope: &[&str]| Seal {
        capabilities: Default::default(),
        scope: scope.iter().map(|s| s.to_string()).collect(),
        mcp: Default::default(),
    };
    let issue = |scope: &[&str]| court.new_edict(Department::Works, "查".into(), seal(scope));
    assert!(issue(&["."]).is_ok());
    assert!(issue(&["./crates/ziyuan-storage", "README.md"]).is_ok());
    for bad in ["/etc", "../secret", "src/../../x", " "] {
        assert_eq!(
            issue(&["src", bad]).unwrap_err(),
            GovernanceError::InvalidScope(bad.into()),
            "{bad}"
        );
    }
    assert!(issue(&[]).is_err());
    // 回放同样拒绝：手写的日志也不能绕过。
    let mut events = founding_events(1);
    events.push(CourtEvent::EdictIssued(Box::new(Edict {
        id: TaskId(1),
        department: Department::Works,
        text: "查".into(),
        seal: seal(&["/"]),
    })));
    assert!(Court::replay(&events).is_err());
}
#[test]
fn task_lifecycle_is_enforced_on_replay() {
    let task = TaskId(1);
    let mut base = founding_events(1);
    base.push(CourtEvent::EdictIssued(Box::new(Edict {
        id: task,
        department: Department::Works,
        text: "查".into(),
        seal: Seal::default(),
    })));
    let with = |extra: &[CourtEvent]| {
        let mut events = base.clone();
        events.extend_from_slice(extra);
        Court::replay(&events)
    };
    let assigned = CourtEvent::TaskAssigned {
        task,
        worker: AgentId(4),
        model: None,
    };
    let tool = CourtEvent::ToolInvoked {
        task,
        tool: "read_file".into(),
        args_json: "{}".into(),
        ok: true,
    };
    let done_by = |worker| CourtEvent::TaskCompleted {
        task,
        worker: AgentId(worker),
        summary: "ok".into(),
    };

    let usage = CourtEvent::ModelUsage {
        task,
        usage: TokenUsage {
            input: 1,
            output: 1,
        },
        trace: None,
    };
    assert_eq!(
        with(std::slice::from_ref(&usage)).unwrap_err(),
        GovernanceError::TaskNotAssigned(task)
    );
    // 结束后仍可补记用量（超时后才返回的调用同样消耗了 token）。
    assert!(with(&[assigned.clone(), CourtEvent::TaskCancelled { task }, usage]).is_ok());
    // v0.1 日志：未指派即完成，仍可回放。
    assert!(with(&[done_by(4)]).is_ok());
    assert!(with(&[assigned.clone(), tool.clone(), done_by(4)]).is_ok());
    assert_eq!(
        with(&[assigned.clone(), done_by(2)]).unwrap_err(),
        GovernanceError::WrongWorker {
            task,
            worker: AgentId(2)
        }
    );
    assert_eq!(
        with(std::slice::from_ref(&tool)).unwrap_err(),
        GovernanceError::TaskNotAssigned(task)
    );
    assert_eq!(
        with(&[assigned, CourtEvent::TaskCancelled { task }, tool]).unwrap_err(),
        GovernanceError::TaskFinished(task)
    );
    let court = with(&[CourtEvent::TaskFailed {
        task,
        reason: "x".into(),
    }])
    .unwrap();
    assert!(court.open_tasks().is_empty());
    assert!(court.propose_cancellation(task).is_err());
}
#[test]
fn recruitment_enables_dynamic_worker_pool() {
    let mut court = Court::replay(&founding_events(1)).unwrap();
    let event = court
        .propose_recruitment("新工".into(), Department::Works)
        .unwrap();
    court.apply(&event).unwrap();
    assert!(court.agents.contains_key(&AgentId(11)));
    court
        .apply(
            &court
                .propose_appointment(AgentId(11), Office::Clerk(Department::Works))
                .unwrap(),
        )
        .unwrap();
    let edict = court
        .new_edict(Department::Works, "测试".into(), Seal::default())
        .unwrap();
    assert_eq!(court.route(&edict).unwrap().worker, AgentId(4));
    let completed = [
        CourtEvent::EdictIssued(Box::new(edict.clone())),
        CourtEvent::TaskCompleted {
            task: edict.id,
            worker: AgentId(4),
            summary: "demo".into(),
        },
    ];
    for e in &completed {
        court.apply(e).unwrap();
    }
    assert_eq!(court.route(&edict).unwrap().worker, AgentId(11));
}
#[test]
fn appoint_validates_department() {
    let mut court = Court::replay(&founding_events(1)).unwrap();
    court
        .apply(&court.propose_dismissal(AgentId(4)).unwrap())
        .unwrap();
    assert!(
        court
            .propose_appointment(AgentId(4), Office::Clerk(Department::Personnel))
            .is_err()
    );
}

#[test]
fn renaming_changes_only_the_given_parts_and_shows_in_routes() {
    let mut court = Court::replay(&founding_events(1)).unwrap();
    let id = court.agents().next().unwrap().id;
    let old = court.agent(id).unwrap().name.clone();
    let (courtesy, art) = (court.courtesy_name(id), court.art_name(id));
    assert_eq!(
        (courtesy.clone(), art.clone()),
        ziyuan_core::default_styles(id)
    );

    // 只改名：字号不变，派工路线随即用新名。
    let event = court
        .propose_rename(id, Some("徐有贞".into()), None, None)
        .unwrap();
    let mut events = founding_events(1);
    events.push(event.clone());
    court.apply(&event).unwrap();
    assert_eq!(court.agent(id).unwrap().name, "徐有贞");
    assert_eq!(court.courtesy_name(id), courtesy);
    let edict = court
        .new_edict(Department::Works, "x".into(), Seal::default())
        .unwrap();
    let route = court.route(&edict).unwrap().path.join(" ");
    assert!(route.contains("徐有贞") && !route.contains(&old), "{route}");

    // 只改号：名与字不变。
    let event = court
        .propose_rename(id, None, None, Some("天全".into()))
        .unwrap();
    events.push(event.clone());
    court.apply(&event).unwrap();
    assert_eq!(
        (
            court.agent(id).unwrap().name.as_str(),
            court.courtesy_name(id),
            court.art_name(id).as_str()
        ),
        ("徐有贞", courtesy.clone(), "天全")
    );
    // 回放得到同样的结果。
    let replayed = Court::replay(&events).unwrap();
    assert_eq!(replayed.agent(id).unwrap().name, "徐有贞");
    assert_eq!(replayed.art_name(id), "天全");

    for (name, courtesy, art) in [
        (None, None, None),                              // 无所更易
        (Some("徐有贞".to_owned()), None, None),         // 与现名相同
        (None, Some("元 玉".to_owned()), None),          // 含空白
        (None, None, Some("一二三四五六七".to_owned())), // 号过长
    ] {
        assert!(court.propose_rename(id, name, courtesy, art).is_err());
    }
    assert!(matches!(
        court.propose_rename(AgentId(999), Some("某".into()), None, None),
        Err(GovernanceError::UnknownAgent(_))
    ));
    // 回放同样拒绝不合规矩的改名。
    assert!(
        court
            .apply(&CourtEvent::AgentRenamed {
                agent: id,
                name: Some(String::new()),
                courtesy: None,
                art: None,
            })
            .is_err()
    );
}
