//! 回放：把事件逐条施于朝廷状态（唯一改变状态的地方），以及开国事件。

use crate::cabinet::{Draft, Split};
use crate::censorate::{Censure, Suspicion, SuspicionReview};
use crate::edicts::{ChangeState, Changes, check_criteria};
use crate::exam::{ExamRecord, PalaceExam};
use crate::offices::validate_edict_seal;
use crate::templates::TemplateUse;
use crate::{Court, GovernanceError, Task, TaskStatus};
use std::collections::{BTreeMap, BTreeSet};
use ziyuan_core::{
    Agent, AgentId, BudgetScope, CourtEvent, Department, Edict, GovernanceMode, Office, TokenUsage,
};

/// 最小开局（丞相制）：只有工部与吏部有人，其余四部留空待扩展。官员姓名由 `seed` 随机生成。
pub fn founding_events(seed: u64) -> Vec<CourtEvent> {
    use CourtEvent::*;
    use Department::*;
    let mut generator = crate::names::NameGenerator::new(seed);
    let mut taken: Vec<String> = Vec::new();
    let recruits = [Personnel, Works, Personnel, Works, Personnel, Personnel]
        .into_iter()
        .zip(1..)
        .map(|(speciality, id)| {
            let name = generator.name(|n| taken.iter().any(|t| t == n));
            taken.push(name.clone());
            (id, name, speciality)
        })
        .collect::<Vec<_>>();
    let mut events = vec![Founded(GovernanceMode::Chancellor)];
    for (id, name, speciality) in recruits {
        events.push(AgentRegistered(Agent {
            id: AgentId(id),
            name,
            speciality,
        }));
    }
    events.extend([
        OfficialAppointed {
            agent: AgentId(1),
            office: Office::Chancellor,
        },
        OfficialAppointed {
            agent: AgentId(2),
            office: Office::Minister(Works),
        },
        OfficialAppointed {
            agent: AgentId(3),
            office: Office::Minister(Personnel),
        },
        OfficialAppointed {
            agent: AgentId(4),
            office: Office::Clerk(Works),
        },
        OfficialAppointed {
            agent: AgentId(5),
            office: Office::Clerk(Personnel),
        },
        OfficialAppointed {
            agent: AgentId(6),
            office: Office::Clerk(Personnel),
        },
    ]);
    events
}

impl Court {
    pub fn replay(events: &[CourtEvent]) -> Result<Self, GovernanceError> {
        let mode = match events.first() {
            Some(CourtEvent::Founded(mode)) => *mode,
            _ => return Err(GovernanceError::NotFounded),
        };
        let mut court = Self {
            mode,
            agents: BTreeMap::new(),
            appointments: BTreeMap::new(),
            purged: BTreeSet::new(),
            retreat: false,
            office_models: BTreeMap::new(),
            office_ceilings: BTreeMap::new(),
            court_budget: None,
            office_budgets: BTreeMap::new(),
            next_task_id: 1,
            next_agent_id: 1,
            tasks: BTreeMap::new(),
            exams: Vec::new(),
            palace_exams: Vec::new(),
            templates: BTreeMap::new(),
            nominations: BTreeMap::new(),
            nomination_usage: TokenUsage::default(),
            nomination_votes: BTreeMap::new(),
            canings: BTreeMap::new(),
            courtesy_names: BTreeMap::new(),
            art_names: BTreeMap::new(),
        };
        for event in events {
            court.apply(event)?;
        }
        Ok(court)
    }

    pub fn apply(&mut self, event: &CourtEvent) -> Result<(), GovernanceError> {
        match event {
            CourtEvent::Clock(_) => {}
            CourtEvent::Founded(mode) => self.mode = *mode,
            CourtEvent::AgentRegistered(agent) => {
                if agent.name.trim().is_empty() {
                    return Err(GovernanceError::EmptyName);
                }
                if self.agents.contains_key(&agent.id) {
                    return Err(GovernanceError::DuplicateAgent(agent.id));
                }
                self.next_agent_id = self.next_agent_id.max(agent.id.0 + 1);
                self.agents.insert(agent.id, agent.clone());
            }
            CourtEvent::AgentRenamed {
                agent,
                name,
                courtesy,
                art,
            } => {
                self.propose_rename(*agent, name.clone(), courtesy.clone(), art.clone())?;
                if let (Some(name), Some(a)) = (name, self.agents.get_mut(agent)) {
                    a.name = name.clone();
                }
                if let Some(c) = courtesy {
                    self.courtesy_names.insert(*agent, c.clone());
                }
                if let Some(h) = art {
                    self.art_names.insert(*agent, h.clone());
                }
            }
            CourtEvent::OfficialAppointed { agent, office } => {
                self.propose_appointment(*agent, *office)?;
                self.appointments.insert(*agent, *office);
                // 官缺已补，此前的廷推作罢。
                self.nominations.remove(office.code().as_str());
            }
            CourtEvent::CourtNominated { office, votes } => {
                self.propose_nomination(*office, votes.clone())?;
                for vote in votes {
                    self.nomination_usage += vote.usage;
                    *self.nomination_votes.entry(vote.candidate).or_default() += 1;
                }
                self.nominations.insert(office.code(), votes.clone());
            }
            CourtEvent::OfficialCaned { agent } => {
                self.propose_caning(*agent)?;
                let record = self.service_record(*agent);
                // 戴罪未立功又受杖的，再降一级。
                let demoted = self.demotion(*agent) + 1;
                let entry = self.canings.entry(*agent).or_default();
                entry.times += 1;
                entry.demoted = demoted;
                entry.impeached_at = record.impeached;
                entry.accepted_at = record.accepted;
            }
            CourtEvent::OfficialPardoned { agent } => {
                self.propose_pardon(*agent)?;
                self.canings.get_mut(agent).expect("已确认受杖").demoted = 0;
            }
            CourtEvent::OfficialDismissed { agent } => {
                self.propose_dismissal(*agent)?;
                self.appointments.remove(agent);
            }
            CourtEvent::OfficialPurged { agent } => {
                self.propose_purge(*agent)?;
                self.appointments.remove(agent);
                self.purged.insert(*agent);
            }
            CourtEvent::EdictDrafted {
                task,
                worker,
                note,
                model,
                usage,
            } => {
                self.propose_draft(*task, *worker, note.clone(), model.clone(), *usage)?;
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                entry.usage += *usage;
                let (mut total, mut model) = (*usage, model.clone());
                if let Some(previous) = &entry.draft {
                    total += previous.usage;
                    model = model.or_else(|| previous.model.clone());
                }
                entry.draft = Some(Draft {
                    worker: *worker,
                    note: note.clone(),
                    model,
                    usage: total,
                });
            }
            CourtEvent::EdictApproved { task } => {
                self.propose_approval(*task)?;
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                entry.approved = true;
                entry.proxied |= self.retreat;
            }
            CourtEvent::EmperorRetreat(on) => {
                self.propose_retreat(*on)?;
                self.retreat = *on;
            }
            CourtEvent::InstitutionReformed(mode) => {
                self.propose_reform(*mode)?;
                // 改制时撤销旧制度协调机构的官职；六部官员保留（品级随制度变化）。
                self.appointments
                    .retain(|_, office| office.mode().is_none());
                self.mode = *mode;
            }
            CourtEvent::EdictIssued(edict) => {
                if edict.text.trim().is_empty() {
                    return Err(GovernanceError::EmptyEdict);
                }
                validate_edict_seal(edict.department, &edict.seal)?;
                if self.tasks.contains_key(&edict.id) {
                    return Err(GovernanceError::DuplicateEdict(edict.id));
                }
                self.next_task_id = self.next_task_id.max(edict.id.0 + 1);
                self.tasks.insert(
                    edict.id,
                    Task {
                        edict: (**edict).clone(),
                        status: TaskStatus::Pending,
                        tool_calls: 0,
                        tool_failures: 0,
                        usage: TokenUsage::default(),
                        review: None,
                        approved: false,
                        model: None,
                        draft: None,
                        changes: None,
                        censure: None,
                        parent: None,
                        assignee: None,
                        split: None,
                        proxied: false,
                        debates: Vec::new(),
                        budget: None,
                        suspicions: Vec::new(),
                        courier: None,
                        replied: None,
                        pull_request: None,
                        urgent: false,
                        template: None,
                    },
                );
            }
            CourtEvent::TaskAssigned {
                task,
                worker,
                model,
            } => {
                self.open_task(*task)?;
                if !self.agents.contains_key(worker) {
                    return Err(GovernanceError::UnknownAgent(*worker));
                }
                self.set_status(*task, TaskStatus::Assigned(*worker));
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                entry.model = model.clone();
                // 重新指派（如上次运行中断）：上次暂存的改动作废。
                entry.changes = None;
            }
            CourtEvent::OfficeModelSet { office, model } => {
                self.propose_office_model(*office, model.clone())?;
                match model {
                    Some(m) => self.office_models.insert(office.code(), m.clone()),
                    None => self.office_models.remove(&office.code()),
                };
            }
            CourtEvent::BudgetSet { scope, tokens } => {
                self.propose_budget(*scope, *tokens)?;
                match scope {
                    BudgetScope::Court => self.court_budget = *tokens,
                    BudgetScope::Office(office) => match tokens {
                        Some(t) => {
                            self.office_budgets.insert(office.code(), *t);
                        }
                        None => {
                            self.office_budgets.remove(&office.code());
                        }
                    },
                    BudgetScope::Task(task) => {
                        self.tasks.get_mut(task).expect("已确认存在").budget = *tokens;
                    }
                }
            }
            CourtEvent::OfficeCeilingSet { office, ceiling } => {
                self.propose_office_ceiling(*office, ceiling.clone())?;
                match ceiling {
                    Some(c) => self.office_ceilings.insert(office.code(), c.clone()),
                    None => self.office_ceilings.remove(&office.code()),
                };
            }
            CourtEvent::ToolInvoked { task, ok, .. } => {
                if !matches!(self.open_task(*task)?, TaskStatus::Assigned(_)) {
                    return Err(GovernanceError::TaskNotAssigned(*task));
                }
                let entry = self.tasks.get_mut(task).expect("open_task 已确认存在");
                entry.tool_calls += 1;
                entry.tool_failures += u32::from(!ok);
            }
            CourtEvent::TaskCompleted {
                task,
                worker,
                summary,
            } => {
                // v0.1 的日志没有 TaskAssigned，直接从 Pending 完成。
                match self.open_task(*task)? {
                    TaskStatus::Assigned(assigned) if assigned != worker => {
                        return Err(GovernanceError::WrongWorker {
                            task: *task,
                            worker: *worker,
                        });
                    }
                    _ => {}
                }
                if !self.agents.contains_key(worker) {
                    return Err(GovernanceError::UnknownAgent(*worker));
                }
                self.set_status(
                    *task,
                    TaskStatus::Completed {
                        worker: *worker,
                        summary: summary.clone(),
                    },
                );
            }
            CourtEvent::ChangesProposed { task, changes } => {
                if !matches!(self.open_task(*task)?, TaskStatus::Assigned(_)) {
                    return Err(GovernanceError::TaskNotAssigned(*task));
                }
                self.tasks.get_mut(task).expect("已确认存在").changes = Some(Changes {
                    files: changes.clone(),
                    state: ChangeState::Proposed,
                    skipped: Vec::new(),
                });
            }
            CourtEvent::EdictSplit {
                task,
                subtasks,
                note,
                model,
                usage,
            } => {
                let plan: Vec<_> = subtasks
                    .iter()
                    .map(|s| (s.worker, s.text.clone()))
                    .collect();
                self.check_split(*task, &plan)?;
                for (i, sub) in subtasks.iter().enumerate() {
                    if self.tasks.contains_key(&sub.id)
                        || subtasks[..i].iter().any(|s| s.id == sub.id)
                    {
                        return Err(GovernanceError::DuplicateEdict(sub.id));
                    }
                }
                let parent = self.tasks.get(task).expect("已确认存在").clone();
                for sub in subtasks {
                    self.next_task_id = self.next_task_id.max(sub.id.0 + 1);
                    self.tasks.insert(
                        sub.id,
                        Task {
                            edict: Edict {
                                id: sub.id,
                                text: sub.text.clone(),
                                ..parent.edict.clone()
                            },
                            status: TaskStatus::Pending,
                            tool_calls: 0,
                            tool_failures: 0,
                            usage: TokenUsage::default(),
                            review: None,
                            approved: false,
                            model: None,
                            draft: None,
                            changes: None,
                            censure: None,
                            parent: Some(*task),
                            courier: None,
                            replied: None,
                            pull_request: None,
                            urgent: parent.urgent,
                            template: None,
                            assignee: Some(sub.worker),
                            split: None,
                            proxied: false,
                            debates: Vec::new(),
                            budget: None,
                            suspicions: Vec::new(),
                        },
                    );
                }
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                entry.usage += *usage;
                entry.split = Some(Split {
                    subtasks: subtasks.iter().map(|s| s.id).collect(),
                    note: note.clone(),
                    model: model.clone(),
                    usage: *usage,
                });
            }
            CourtEvent::ExamTaken {
                agent,
                exam,
                score,
                answer,
                model,
                usage,
            } => {
                self.propose_exam(*agent)?;
                if *score > 100 {
                    return Err(GovernanceError::InvalidExam("分数须为 0–100".into()));
                }
                self.exams.push(ExamRecord {
                    agent: *agent,
                    exam: exam.clone(),
                    score: *score,
                    answer: answer.clone(),
                    model: model.clone(),
                    usage: *usage,
                });
            }
            CourtEvent::PalaceExamHeld { question, answers } => {
                self.propose_palace_exam(question, answers)?;
                self.palace_exams.push(PalaceExam {
                    question: question.clone(),
                    answers: answers.clone(),
                    zhuangyuan: None,
                });
            }
            CourtEvent::TemplateSaved { name, template } => {
                self.propose_template(name.clone(), (**template).clone())?;
                self.templates.insert(name.clone(), (**template).clone());
            }
            CourtEvent::TemplateRemoved { name } => {
                self.propose_template_removal(name.clone())?;
                self.templates.remove(name);
            }
            CourtEvent::TemplateApplied {
                task,
                name,
                criteria,
            } => {
                self.check_template_use(*task)?;
                check_criteria(criteria)?;
                self.tasks.get_mut(task).expect("已确认存在").template = Some(TemplateUse {
                    name: name.clone(),
                    criteria: criteria.clone(),
                });
            }
            CourtEvent::CourierDelivered { task, station } => {
                self.propose_courier(*task, station.clone())?;
                self.tasks.get_mut(task).expect("已确认存在").courier = Some(station.clone());
            }
            CourtEvent::CourierReplied {
                task,
                pull_request,
                url,
            } => {
                self.propose_courier_reply(*task, *pull_request, url)?;
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                if *pull_request {
                    entry.pull_request = Some(url.clone());
                } else {
                    entry.replied = Some(url.clone());
                }
            }
            CourtEvent::EdictExpedited { task } => {
                self.propose_expedite(*task)?;
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                entry.urgent = true;
                // 已分办的，未结的子诏令一并加急。
                let subtasks = entry
                    .split
                    .as_ref()
                    .map(|s| s.subtasks.clone())
                    .unwrap_or_default();
                for id in subtasks {
                    if let Some(sub) = self.tasks.get_mut(&id).filter(|t| t.status.is_open()) {
                        sub.urgent = true;
                    }
                }
            }
            CourtEvent::ZhuangyuanNamed { agent } => {
                self.propose_zhuangyuan(*agent)?;
                let last = self.palace_exams.last_mut().expect("已确认有殿试");
                last.zhuangyuan = Some(*agent);
            }
            CourtEvent::CourtDebated { task, speeches } => {
                self.propose_debate(*task, speeches)?;
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                for speech in speeches {
                    entry.usage += speech.usage;
                }
                entry.debates.push(speeches.clone());
            }
            CourtEvent::CensorReported {
                task,
                censor,
                verdict,
                note,
                evidence,
                model,
                usage,
            } => {
                self.propose_censure_check(*task, *censor)?;
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                entry.usage += *usage;
                entry.censure = Some(Censure {
                    censor: *censor,
                    verdict: *verdict,
                    note: note.clone(),
                    evidence: *evidence,
                    model: model.clone(),
                    usage: *usage,
                });
            }
            CourtEvent::ChangesApplied { task, skipped } => {
                self.propose_application(*task, skipped.clone())?;
                let changes = self.tasks.get_mut(task).and_then(|t| t.changes.as_mut());
                let changes = changes.expect("已确认有待决改动");
                changes.state = ChangeState::Applied;
                changes.skipped = skipped.clone();
            }
            CourtEvent::ChangesDiscarded { task } => {
                self.propose_changes_decision(*task, false)?;
                let changes = self.tasks.get_mut(task).and_then(|t| t.changes.as_mut());
                changes.expect("已确认有待决改动").state = ChangeState::Discarded;
            }
            CourtEvent::TaskFailed { task, reason } => {
                let worker = match self.open_task(*task)? {
                    TaskStatus::Assigned(worker) => Some(*worker),
                    _ => None,
                };
                self.set_status(
                    *task,
                    TaskStatus::Failed {
                        worker,
                        reason: reason.clone(),
                    },
                );
            }
            CourtEvent::SuspicionRaised {
                task,
                source,
                threat,
                excerpt,
            } => {
                let entry = self
                    .tasks
                    .get_mut(task)
                    .ok_or(GovernanceError::UnknownEdict(*task))?;
                // 驿传的检出在入队时记录，其余须在执行中或之后。
                if entry.status == TaskStatus::Pending && source != Suspicion::COURIER {
                    return Err(GovernanceError::TaskNotAssigned(*task));
                }
                entry.suspicions.push(Suspicion {
                    source: source.clone(),
                    threat: *threat,
                    excerpt: excerpt.clone(),
                    review: None,
                    acquitted: false,
                });
            }
            CourtEvent::SuspicionReviewed {
                task,
                index,
                reviewer,
                upheld,
                note,
                model,
                usage,
            } => {
                self.propose_recheck(*task, *index, *reviewer)?;
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                entry.usage += *usage;
                entry.suspicions[*index as usize].review = Some(SuspicionReview {
                    reviewer: *reviewer,
                    upheld: *upheld,
                    note: note.clone(),
                    model: model.clone(),
                    usage: *usage,
                });
            }
            CourtEvent::SuspicionAcquitted { task, index } => {
                self.propose_acquittal(*task, *index)?;
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                entry.suspicions[*index as usize].acquitted = true;
            }
            CourtEvent::ModelUsage { task, usage, .. } => {
                let entry = self
                    .tasks
                    .get_mut(task)
                    .ok_or(GovernanceError::UnknownEdict(*task))?;
                if entry.status == TaskStatus::Pending {
                    return Err(GovernanceError::TaskNotAssigned(*task));
                }
                entry.usage += *usage;
            }
            CourtEvent::TaskReviewed {
                task,
                verdict,
                note,
            } => {
                self.propose_review(*task, *verdict, note.clone())?;
                let entry = self.tasks.get_mut(task).expect("propose_review 已确认存在");
                entry.review = Some((*verdict, note.clone()));
                entry.proxied |= self.retreat;
            }
            CourtEvent::TaskCancelled { task } => {
                self.propose_cancellation(*task)?;
                self.set_status(*task, TaskStatus::Cancelled);
            }
        }
        Ok(())
    }
}
