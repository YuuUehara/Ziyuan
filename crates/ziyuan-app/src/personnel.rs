//! 吏部与人事：招募、任免、廷推、廷杖、改制、修道与罢相。

use std::collections::BTreeMap;
use ziyuan_core::{
    Agent, AgentId, BudgetScope, CourtEvent, Department, GovernanceMode, Nomination, Office,
    Persona, Seal, TokenUsage,
};
use ziyuan_governance::names::NameGenerator;
use ziyuan_governance::{Caning, Court, GovernanceError};
use ziyuan_runtime::{BoxError, WorkerBackend};
use ziyuan_storage::EventStore;

use crate::cabinet::precedent;
use crate::reports::describe_record;
use crate::session::random_seed;
use crate::{AppError, Session, truncate};

/// 修道或回朝时的史官记述。
pub(crate) fn retreat_note(court: &Court, on: bool) -> String {
    if !on {
        let proxied = court.tasks().filter(|t| t.proxied).count();
        return format!(
            "户部主事海瑞抬棺上《治安疏》：「嘉靖者，言家家皆净而无财用也。」陛下怒而复悟，回朝亲政。\
             修道期间司礼监代批 {proxied} 件，考成簿与事件簿俱在，可一一追查。"
        );
    }
    let regent = match court.mode() {
        GovernanceMode::Cabinet => match court.coordinator() {
            Some((agent, office)) => format!(
                "{}{}票拟、司礼监批红，天下大事尽归内阁",
                office.label(),
                agent.name
            ),
            None => "内阁无人，司礼监独掌批红".to_owned(),
        },
        GovernanceMode::Chancellor => match court.coordinator() {
            Some((agent, office)) => {
                format!("{}{}总揽庶务，司礼监代批", office.label(), agent.name)
            }
            None => "中书省无人，司礼监独掌批红".to_owned(),
        },
        GovernanceMode::DirectRule => {
            "废了丞相又不上朝，只好由司礼监代批——太祖若知，当从孝陵里爬出来".to_owned()
        }
    };
    format!(
        "嘉靖二十一年壬寅宫变后，世宗移居西苑，斋醮修玄，二十余年不视朝；万历帝更三十年不上朝。\
         陛下今亦修道去也：{regent}。批红与评定由司礼监代批，一一记入事件簿；\
         唯工部改动仍须陛下回朝亲自准奏，方得落盘。"
    )
}

/// 一键朱元璋时的史官记述。
pub const ABOLITION_NOTE: &str = "洪武十三年正月，丞相胡惟庸以谋反伏诛，株连者三万余人。即日罢中书省，升六部，天子亲裁庶务。——一键朱元璋：丞相，从此不再是问题。";

/// 史官对一次改制的记述（埋了些历史梗）。
pub fn reform_note(from: GovernanceMode, to: GovernanceMode) -> &'static str {
    use GovernanceMode::*;
    match (from, to) {
        (Chancellor, DirectRule) => {
            "洪武十三年，罢中书省，析其权归六部。太祖垂训：「以后嗣君毋得议置丞相，臣下敢以此请者，置之重典。」"
        }
        (_, Cabinet) => {
            "建文四年六月，燕军渡江，金川门开。「侄儿，四叔来看你了。」新君命解缙、杨士奇等入直文渊阁，参预机务——内阁自此始。今后诏令须经内阁票拟、皇帝批红，方可施行。"
        }
        (Cabinet, Chancellor) => {
            "正统十四年，土木堡之变，天子北狩。瓦剌铁骑南下叩关，中书省竟随之复设——大元，杀回来了。"
        }
        (Cabinet, DirectRule) => {
            "崇祯：「朕非亡国之君，诸臣皆亡国之臣。」十七年间换了五十位大学士，这回索性一个不要，事必躬亲。"
        }
        (DirectRule, Chancellor) => {
            "《皇明祖训》：「以后子孙做皇帝时，并不许立丞相。臣下敢有奏请设立者，文武群臣即时劾奏，将犯人凌迟，全家处死。」太祖的棺材板，快压不住了。"
        }
        _ => "",
    }
}

/// 任命的事件：被廷推者现任他职的，先罢现职再任命（擢升），一并提交；其余照常须未任官职。
pub(crate) fn appointment_events(
    court: &Court,
    agent: AgentId,
    office: Office,
) -> Result<Vec<CourtEvent>, GovernanceError> {
    let nominated = court
        .nomination_tally(office)
        .iter()
        .any(|(id, _)| *id == agent);
    match court.office_of(agent) {
        Some(current) if nominated && current != office => {
            let dismissed = court.propose_dismissal(agent)?;
            let mut next = court.clone();
            next.apply(&dismissed)?;
            Ok(vec![dismissed, next.propose_appointment(agent, office)?])
        }
        _ => Ok(vec![court.propose_appointment(agent, office)?]),
    }
}

/// 任命 `agent` 为 `office` 时的史官记：此官职有廷推才有。
pub fn appointment_note(court: &Court, agent: AgentId, office: Office) -> Option<String> {
    court.nomination(office)?;
    let tally = court.nomination_tally(office);
    let name = &court.agent(agent)?.name;
    let total: u32 = tally.iter().map(|(_, n)| n).sum();
    let promoted = match court.office_of(agent) {
        Some(current) if current != office && tally.iter().any(|(id, _)| *id == agent) => {
            format!("由{}擢升。", current.label())
        }
        _ => String::new(),
    };
    Some(
        promoted
            + &match tally.iter().position(|(id, _)| *id == agent) {
                Some(0) => format!(
                    "从廷推：{name}得 {} 票（共 {total} 票），居廷推之首，众望所归。",
                    tally[0].1
                ),
                Some(rank) => format!(
                    "廷推第 {}（{} 票），陛下择而用之，不取票多者。",
                    rank + 1,
                    tally[rank].1
                ),
                None => format!(
                    "中旨特简：{name}不在廷推之列，言官交章论列，然任命出自圣裁。\n\
             ——成化年间，宪宗屡以中旨授官，不经吏部，号「传奉官」，朝野非之。"
                ),
            },
    )
}

/// 降级留用的说明（未受杖或已复职为 `None`）。
pub fn demotion_note(court: &Court, agent: AgentId) -> Option<String> {
    let levels = court.demotion(agent);
    (levels > 0).then(|| {
        let left = court.redemption_left(agent).unwrap_or(0);
        format!("降 {levels} 级留用，再获采纳 {left} 件即复")
    })
}

impl<S: EventStore> Session<S> {
    /// 招募新官员（不授官职），返回其 ID。`name` 为 `None` 时随机取一个未被占用的姓名。
    pub fn recruit(
        &mut self,
        name: Option<String>,
        speciality: Department,
    ) -> Result<AgentId, AppError> {
        let seed = random_seed();
        let committed = self.commit(|court| {
            let name = name.clone().unwrap_or_else(|| {
                NameGenerator::new(seed).name(|n| court.agents().any(|a| a.name == n))
            });
            Ok(vec![court.propose_recruitment(name, speciality)?])
        })?;
        match committed.first() {
            Some(CourtEvent::AgentRegistered(agent)) => Ok(agent.id),
            _ => unreachable!("propose_recruitment 只产生 AgentRegistered"),
        }
    }

    /// 任命；此官职有廷推时返回史官记（从廷推、择而用之或中旨特简）。
    pub fn appoint(&mut self, agent: AgentId, office: Office) -> Result<Option<String>, AppError> {
        self.reload()?;
        // 任命后廷推即作罢，须先写好史官记。
        let note = appointment_note(&self.court, agent, office);
        self.commit(|court| appointment_events(court, agent, office))?;
        Ok(note)
    }

    /// 廷推：吏部会同九卿为出缺的 `office` 各推一人（经 `WorkerBackend::complete`，以各自的模型与人格）。
    /// 模型不可用、度支不足或所答不合规矩的，按吏部考绩代推并注明。只是建议，不任命。
    pub fn nominate(
        &mut self,
        office: Office,
        backend: &dyn WorkerBackend,
        mut on_vote: impl FnMut(&Agent, &Nomination),
    ) -> Result<Vec<Nomination>, AppError> {
        self.reload()?;
        let court = &self.court;
        let (ministers, candidates) = court.nomination_ballot(office)?;
        let records = court.records();
        let mut votes = Vec::new();
        let mut spent = 0;
        for (voter, voter_office) in &ministers {
            let eligible: Vec<_> = candidates
                .iter()
                .filter(|c| c.agent.id != voter.id)
                .collect();
            let Some(fallback) = eligible.first() else {
                continue;
            };
            let roster: String = eligible
                .iter()
                .take(12)
                .map(|c| {
                    let current = c
                        .current
                        .map_or("未任官职".to_owned(), |o| format!("现任{}", o.label()));
                    // 翰林院实录中此人最近经办的一件，供九卿参考前例。
                    let recent = records
                        .iter()
                        .rev()
                        .find(|r| r.worker == c.agent.id)
                        .map(|r| format!("；近办 {}", precedent(r)))
                        .unwrap_or_default();
                    format!(
                        "#{} {}（专长{}，{current}）：{}{recent}\n",
                        c.agent.id.0,
                        c.agent.name,
                        c.agent.speciality.label(),
                        describe_record(&c.record)
                    )
                })
                .collect();
            let system = format!(
                "你是紫垣朝廷{}{}，奉旨与九卿廷推{}。从候选人中推举一人，不得推举自己。\n\
                 回复第一行只写所推官员的 ID（数字），第二行起用不超过 80 字说明理由。\
                 廷推只是建议，任命由皇帝决定。\n{}",
                voter_office.label(),
                voter.name,
                office.label(),
                Persona::of(voter.id).prompt()
            );
            let user = format!("候选人（按吏部考绩排序）：\n{roster}");
            let model = court
                .model_of(voter.id)
                .or(backend.model_name())
                .map(str::to_owned);
            let answer = court
                .check_treasury(spent)
                .map_err(BoxError::from)
                .and_then(|()| backend.complete(model.as_deref(), &system, &user));
            let vote = match answer {
                Ok((reply, usage)) => {
                    spent += usage.total();
                    let mut lines = reply.trim().lines();
                    let first = lines.next().unwrap_or_default();
                    let digits: String = first.chars().filter(char::is_ascii_digit).collect();
                    let picked = digits
                        .parse()
                        .ok()
                        .map(AgentId)
                        .filter(|id| eligible.iter().any(|c| c.agent.id == *id));
                    let reason = truncate(lines.collect::<Vec<_>>().join(" ").trim(), 200);
                    match picked {
                        Some(candidate) => Nomination {
                            voter: voter.id,
                            candidate,
                            note: reason,
                            model,
                            usage,
                        },
                        None => Nomination {
                            voter: voter.id,
                            candidate: fallback.agent.id,
                            note: format!(
                                "所推不在候选之列（「{}」），按吏部考绩代推",
                                truncate(first, 40)
                            ),
                            model,
                            usage,
                        },
                    }
                }
                Err(e) => Nomination {
                    voter: voter.id,
                    candidate: fallback.agent.id,
                    note: format!("未能以模型推举（{e}），按吏部考绩代推"),
                    model: None,
                    usage: TokenUsage::default(),
                },
            };
            on_vote(voter, &vote);
            votes.push(vote);
        }
        self.commit(|court| Ok(vec![court.propose_nomination(office, votes.clone())?]))?;
        Ok(votes)
    }

    /// 枚卜：从 `office` 最近一次廷推的被推者中（可即任命者）随机取一人任命，返回其人与史官记。
    pub fn draw_lots(&mut self, office: Office) -> Result<(AgentId, String), AppError> {
        self.reload()?;
        let court = &self.court;
        let pool: Vec<AgentId> = court
            .nomination_tally(office)
            .into_iter()
            .map(|(id, _)| id)
            .filter(|&id| appointment_events(court, id, office).is_ok())
            .collect();
        if pool.is_empty() {
            let why = format!("{}尚无廷推，或被推者都不能任命", office.label());
            return Err(GovernanceError::Court(why).into());
        }
        let agent = pool[(random_seed() % pool.len() as u64) as usize];
        let name = court.agent(agent).expect("在册").name.clone();
        self.commit(|court| appointment_events(court, agent, office))?;
        Ok((
            agent,
            format!(
                "枚卜：金瓯覆名，焚香拜天，以箸夹之，得{name}为{}（{} 人中取一）。\n\
                 ——崇祯元年，帝疑廷推阁臣结党，纳诸臣之名于金瓯，焚香肃拜而枚卜之；然十七年间易相五十，终无救于亡。",
                office.label(),
                pool.len()
            ),
        ))
    }

    /// 廷杖：屡遭弹劾的在任官员降一级留用、收回写权，返回史官记。
    pub fn cane(&mut self, agent: AgentId) -> Result<String, AppError> {
        self.commit(|court| Ok(vec![court.propose_caning(agent)?]))?;
        let court = &self.court;
        let name = &court.agent(agent).expect("在册").name;
        let office = court.office_of(agent).expect("在任方可受杖");
        let levels = court.demotion(agent);
        let history = if office == Office::Censor {
            "——言官受杖，直声动天下：正德十四年百官谏南巡，跪阙下五日，杖于午门者百余人。"
        } else {
            "——嘉靖三年大礼议，群臣伏阙左顺门哭谏，廷杖百八十余人，死者十余人。廷杖之辱，明一代之弊政。"
        };
        Ok(format!(
            "廷杖：{}{name}屡遭弹劾，于午门外受杖，降 {levels} 级留用，收回工部动土之权（write_file、run_tests）；\
             再有 {} 件回奏获采纳，即戴罪立功、复其品级与玉玺。\n{history}",
            office.label(),
            Caning::REDEMPTION
        ))
    }

    /// 宽宥受杖的官员，返回史官记。
    pub fn pardon(&mut self, agent: AgentId) -> Result<String, AppError> {
        self.commit(|court| Ok(vec![court.propose_pardon(agent)?]))?;
        let name = &self.court.agent(agent).expect("在册").name;
        Ok(format!(
            "宽宥：{name}复其品级与玉玺。圣恩浩荡，臣当戴罪图报。"
        ))
    }

    pub fn dismiss(&mut self, agent: AgentId) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_dismissal(agent)?]))?;
        Ok(())
    }

    /// 改制，返回史官对这次改制的记述（见 [`reform_note`]）。
    pub fn reform(&mut self, mode: GovernanceMode) -> Result<&'static str, AppError> {
        let mut from = self.court.mode();
        self.commit(|court| {
            from = court.mode();
            Ok(vec![court.propose_reform(mode)?])
        })?;
        Ok(reform_note(from, mode))
    }

    /// 革职：除去官职、削去仕籍，永不叙用。
    pub fn purge(&mut self, agent: AgentId) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_purge(agent)?]))?;
        Ok(())
    }

    /// 修道怠政（`true`）或回朝亲政（`false`），返回史官记述。
    ///
    /// 修道期间：执行时待批红的诏令由司礼监代批红（按票拟，未票拟按旧例），完成的回奏由司礼监
    /// 代批「照准」，皆记为代批；皇帝不能亲自批红与评定。唯工部改动仍须皇帝亲自准奏才落盘。
    pub fn retreat(&mut self, on: bool) -> Result<String, AppError> {
        self.commit(|court| Ok(vec![court.propose_retreat(on)?]))?;
        Ok(retreat_note(&self.court, on))
    }

    /// 钦点状元（最近一次殿试的答卷者之一），返回传胪的史官记。
    pub fn crown(&mut self, agent: AgentId) -> Result<String, AppError> {
        self.commit(|court| Ok(vec![court.propose_zhuangyuan(agent)?]))?;
        let court = &self.court;
        let name = &court.agent(agent).expect("答卷者在册").name;
        let rank = match court.office_of(agent) {
            Some(Office::Chancellor | Office::FirstGrandSecretary) => {
                "以宰辅之尊下场应试，礼部翻遍旧档，从无此例。"
            }
            Some(Office::Clerk(_)) => "郎中一跃而魁天下，满朝侧目。",
            _ => "",
        };
        let times = court.service_record(agent).zhuangyuan;
        let again = if times > 1 {
            format!("此已是第 {times} 次大魁天下，连中三元亦不过如此。")
        } else {
            String::new()
        };
        Ok(format!(
            "传胪：一甲第一名{name}，赐进士及第，跨马游街，琼林赐宴。{rank}{again}\n\
             ——洪武四年首开科举，状元吴伯宗；殿试由天子亲策，名次出自圣裁。"
        ))
    }

    /// 为官职指定承办模型；`None` 恢复默认。
    pub fn set_office_model(
        &mut self,
        office: Office,
        model: Option<String>,
    ) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_office_model(office, model.clone())?]))?;
        Ok(())
    }

    /// 户部度支：设定朝廷总额、某官职每件或某件诏令的 token 限额；`None` 取消。
    pub fn set_budget(&mut self, scope: BudgetScope, tokens: Option<u64>) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_budget(scope, tokens)?]))?;
        Ok(())
    }

    /// 为官职设定玉玺上限；`None` 取消上限。
    pub fn set_office_ceiling(
        &mut self,
        office: Office,
        ceiling: Option<Seal>,
    ) -> Result<(), AppError> {
        self.commit(|court| Ok(vec![court.propose_office_ceiling(office, ceiling.clone())?]))?;
        Ok(())
    }

    /// 一键朱元璋：罢中书省，株连百官（全部在任官员革职削籍），改为废相亲政。
    /// 返回株连的文字记述（丞相在前，各附原官职）。
    pub fn abolish_chancellery(&mut self) -> Result<String, AppError> {
        let mut offices = BTreeMap::new();
        let committed = self.commit(|court| {
            offices = court
                .agents()
                .filter_map(|a| Some((a.id, court.office_of(a.id)?)))
                .collect();
            court.propose_abolition()
        })?;
        let mut purged: Vec<_> = committed
            .iter()
            .filter_map(|e| match e {
                CourtEvent::OfficialPurged { agent } => Some((*agent, offices.get(agent)?)),
                _ => None,
            })
            .collect();
        if purged.is_empty() {
            return Ok("朝中本无在任官员。".into());
        }
        purged.sort_by_key(|(_, office)| office.mode() != Some(GovernanceMode::Chancellor));
        let names: Vec<_> = purged
            .iter()
            .map(|(id, office)| {
                let name = self.court.agent(*id).map_or("?", |a| a.name.as_str());
                format!("{}{name}", office.label())
            })
            .collect();
        Ok(format!(
            "株连所及，凡 {} 人：{}，一并革职削籍，永不叙用。六部为之一空，请重新招募任官。",
            names.len(),
            names.join("、")
        ))
    }
}
