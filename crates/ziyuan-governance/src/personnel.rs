//! 吏部与人事：招募、任免、廷推、廷杖、改制、修道与罢相。

use crate::edicts::department_of;
use crate::{Court, GovernanceError, ServiceRecord};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use ziyuan_core::{
    Agent, AgentId, CourtEvent, Department, GovernanceMode, Nomination, Office, TaskId,
};

/// 一位官员的廷杖记录。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caning {
    /// 受杖次数（历来累计）。
    pub times: u32,
    /// 现降几级；宽宥或戴罪立功后为 0。
    pub demoted: u32,
    /// 最近一次受杖时的弹劾数与获采纳数，用来计「再遭弹劾」与「戴罪立功」。
    pub impeached_at: u32,
    pub accepted_at: u32,
}

impl Caning {
    /// 受杖后再获采纳这么多件回奏，即戴罪立功、复其品级与玉玺。
    pub const REDEMPTION: u32 = 3;
    /// 受杖前须（自上次受杖以来）遭弹劾这么多次。
    pub const IMPEACHMENTS: u32 = 2;
}

/// 廷推的选票：九卿（及其官职）与候选人。
pub type NominationBallot<'a> = (Vec<(&'a Agent, Office)>, Vec<Candidate<'a>>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate<'a> {
    pub agent: &'a Agent,
    /// 现任官职；荐举不会自动罢免或调任。
    pub current: Option<Office>,
    pub record: ServiceRecord,
}

/// 派工时如何选承办者。
#[derive(Clone, Copy)]
pub(crate) enum Pick {
    /// 旧例：任务量均衡派给郎中。
    Default,
    /// 指定官员（票拟或分办），路线上附注标记。
    Named(AgentId, &'static str),
    /// 尚书（或署理的侍郎）亲办：汇总分办的回奏。
    Head,
}

impl Court {
    /// 皇帝是否在修道怠政。
    pub fn in_retreat(&self) -> bool {
        self.retreat
    }

    pub fn propose_retreat(&self, on: bool) -> Result<CourtEvent, GovernanceError> {
        if self.retreat == on {
            return Err(GovernanceError::RetreatUnchanged(on));
        }
        Ok(CourtEvent::EmperorRetreat(on))
    }

    /// 司礼监代批红：`tasks` 中（`None` 为全部）待批红的诏令。只在修道期间有事可做。
    pub fn proxy_approvals(&self, tasks: Option<&[TaskId]>) -> Vec<CourtEvent> {
        if !self.retreat {
            return Vec::new();
        }
        self.tasks
            .values()
            .filter(|t| tasks.is_none_or(|ids| ids.contains(&t.edict.id)))
            .filter(|t| self.needs_approval(t))
            // 有未平反的密报（如驿传内容可疑），司礼监不敢代批红。
            .filter(|t| t.standing_suspicions().next().is_none())
            .map(|t| CourtEvent::EdictApproved { task: t.edict.id })
            .collect()
    }

    pub fn is_purged(&self, agent: AgentId) -> bool {
        self.purged.contains(&agent)
    }

    /// 革职：除去官职并削去仕籍，此后不得再任官。皇帝可革任何在籍官员。
    pub fn propose_purge(&self, agent: AgentId) -> Result<CourtEvent, GovernanceError> {
        if !self.agents.contains_key(&agent) {
            return Err(GovernanceError::UnknownAgent(agent));
        }
        if self.purged.contains(&agent) {
            return Err(GovernanceError::Purged(agent));
        }
        Ok(CourtEvent::OfficialPurged { agent })
    }

    /// 参与廷推的九卿：六部尚书、左都御史与丞相或首辅（在任者）。须有吏部尚书主持。
    pub fn nine_ministers(&self) -> Result<Vec<(&Agent, Office)>, GovernanceError> {
        let personnel = Office::Minister(Department::Personnel);
        if self.appointed_as(personnel).is_none() {
            return Err(GovernanceError::Court(
                "廷推须由吏部会同九卿，吏部尚书空缺，不能廷推".into(),
            ));
        }
        let mut offices: Vec<Office> = Department::ALL
            .iter()
            .map(|&d| Office::Minister(d))
            .collect();
        offices.extend([
            Office::Censor,
            Office::Chancellor,
            Office::FirstGrandSecretary,
        ]);
        Ok(offices
            .into_iter()
            .filter_map(|o| self.appointed_as(o).map(|a| (a, o)))
            .collect())
    }

    /// 廷推 `office` 的候选人：可任此职（专长、制度符合，未革职）且现不任此职，按吏部考绩排序。
    pub fn nomination_candidates(
        &self,
        office: Office,
    ) -> Result<Vec<Candidate<'_>>, GovernanceError> {
        let candidates = self.recommend(office)?;
        Ok(candidates
            .into_iter()
            .filter(|c| c.current != Some(office))
            .collect())
    }

    /// 廷推的投票者与候选人；官职未出缺、无吏部尚书、无候选人时报错。
    pub fn nomination_ballot(
        &self,
        office: Office,
    ) -> Result<NominationBallot<'_>, GovernanceError> {
        let bad = |why: String| Err(GovernanceError::Court(format!("廷推不成：{why}")));
        if office.is_unique() && self.appointed_as(office).is_some() {
            return bad(format!("{}未出缺", office.label()));
        }
        let ministers = self.nine_ministers()?;
        let candidates = self.nomination_candidates(office)?;
        if candidates.is_empty() {
            return bad(format!("无人可任{}", office.label()));
        }
        Ok((ministers, candidates))
    }

    /// 廷推：唯一的官职须出缺；每位投票者须是九卿之一、各投一次，所推须是候选人且不是自己。
    pub fn propose_nomination(
        &self,
        office: Office,
        votes: Vec<Nomination>,
    ) -> Result<CourtEvent, GovernanceError> {
        let bad = |why: String| Err(GovernanceError::Court(format!("廷推不成：{why}")));
        let (ministers, candidates) = self.nomination_ballot(office)?;
        if votes.is_empty() {
            return bad("无人推举".into());
        }
        let mut seen = BTreeSet::new();
        for vote in &votes {
            if !ministers.iter().any(|(a, _)| a.id == vote.voter) {
                return bad(format!("#{} 不在九卿之列", vote.voter.0));
            }
            if !seen.insert(vote.voter) {
                return bad(format!("#{} 重复推举", vote.voter.0));
            }
            if vote.candidate == vote.voter
                || !candidates.iter().any(|c| c.agent.id == vote.candidate)
            {
                return bad(format!("#{} 不可任{}", vote.candidate.0, office.label()));
            }
        }
        Ok(CourtEvent::CourtNominated { office, votes })
    }

    /// `office` 最近一次（任命之前）的廷推。
    pub fn nomination(&self, office: Office) -> Option<&[Nomination]> {
        self.nominations
            .get(office.code().as_str())
            .map(Vec::as_slice)
    }

    /// 廷推得票，按票数从多到少（同票者先得票的在前）。
    pub fn nomination_tally(&self, office: Office) -> Vec<(AgentId, u32)> {
        let mut tally: Vec<(AgentId, u32)> = Vec::new();
        for vote in self.nomination(office).unwrap_or_default() {
            match tally.iter_mut().find(|(id, _)| *id == vote.candidate) {
                Some((_, n)) => *n += 1,
                None => tally.push((vote.candidate, 1)),
            }
        }
        tally.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        tally
    }

    /// 廷杖：须为在任、未革职的官员，且自上次受杖以来遭都察院弹劾至少 [`Caning::IMPEACHMENTS`] 次。
    pub fn propose_caning(&self, agent: AgentId) -> Result<CourtEvent, GovernanceError> {
        if !self.agents.contains_key(&agent) {
            return Err(GovernanceError::UnknownAgent(agent));
        }
        if self.purged.contains(&agent) {
            return Err(GovernanceError::Purged(agent));
        }
        if !self.appointments.contains_key(&agent) {
            return Err(GovernanceError::Court(
                "廷杖是降级留用之罚，须为在任官员；未任官职者无级可降".into(),
            ));
        }
        let since = self.impeachments_since_caning(agent);
        if since < Caning::IMPEACHMENTS {
            return Err(GovernanceError::Court(format!(
                "廷杖须屡遭弹劾：#{} 自上次受杖以来遭弹劾 {since} 次，不足 {} 次",
                agent.0,
                Caning::IMPEACHMENTS
            )));
        }
        Ok(CourtEvent::OfficialCaned { agent })
    }

    /// 自上次受杖（未受过则自始）以来遭弹劾的次数。
    pub fn impeachments_since_caning(&self, agent: AgentId) -> u32 {
        let at = self.canings.get(&agent).map_or(0, |c| c.impeached_at);
        self.service_record(agent).impeached.saturating_sub(at)
    }

    /// 宽宥：须正在降级留用。
    pub fn propose_pardon(&self, agent: AgentId) -> Result<CourtEvent, GovernanceError> {
        if self.demotion(agent) == 0 {
            return Err(GovernanceError::Court(format!(
                "#{} 并未降级留用，无须宽宥",
                agent.0
            )));
        }
        Ok(CourtEvent::OfficialPardoned { agent })
    }

    /// 廷杖记录。
    pub fn caning(&self, agent: AgentId) -> Option<Caning> {
        self.canings.get(&agent).copied()
    }

    /// 现降几级留用；未受杖、已宽宥或已戴罪立功的为 0。
    pub fn demotion(&self, agent: AgentId) -> u32 {
        match self.canings.get(&agent) {
            Some(c) if !self.redeemed(agent) => c.demoted,
            _ => 0,
        }
    }

    /// 受杖后已获采纳的件数，与戴罪立功尚差几件（不在降级中为 `None`）。
    pub fn redemption_left(&self, agent: AgentId) -> Option<u32> {
        let c = self.canings.get(&agent).filter(|c| c.demoted > 0)?;
        let done = self
            .service_record(agent)
            .accepted
            .saturating_sub(c.accepted_at);
        Caning::REDEMPTION
            .checked_sub(done)
            .filter(|&left| left > 0)
    }

    pub(crate) fn redeemed(&self, agent: AgentId) -> bool {
        self.canings.get(&agent).is_none_or(|c| c.demoted == 0)
            || self.redemption_left(agent).is_none()
    }

    /// 一键朱元璋：罢中书省，株连百官——丞相与全部在任官员革职削籍，改为废相亲政。
    /// 未任官职者（白身）不受牵连。
    pub fn propose_abolition(&self) -> Result<Vec<CourtEvent>, GovernanceError> {
        if self.mode != GovernanceMode::Chancellor {
            return Err(GovernanceError::NoChancellery);
        }
        let mut events: Vec<_> = self
            .appointments
            .iter()
            .map(|(&agent, _)| CourtEvent::OfficialPurged { agent })
            .collect();
        events.push(CourtEvent::InstitutionReformed(GovernanceMode::DirectRule));
        Ok(events)
    }

    /// 招募新官员，登记身份但不自动授职。
    pub fn propose_recruitment(
        &self,
        name: String,
        speciality: Department,
    ) -> Result<CourtEvent, GovernanceError> {
        if name.trim().is_empty() {
            return Err(GovernanceError::EmptyName);
        }
        Ok(CourtEvent::AgentRegistered(Agent {
            id: AgentId(self.next_agent_id),
            name,
            speciality,
        }))
    }

    /// 改名、改字或改号（`None` 的一项不变）。姓名至多 12 字，字、号至多 6 字，都须单行、
    /// 不含空白；至少要改一项，且与现在的不同。革职的官员也可改（史书照旧记其事）。
    pub fn propose_rename(
        &self,
        agent: AgentId,
        name: Option<String>,
        courtesy: Option<String>,
        art: Option<String>,
    ) -> Result<CourtEvent, GovernanceError> {
        let current = self
            .agents
            .get(&agent)
            .ok_or(GovernanceError::UnknownAgent(agent))?;
        let check = |value: &Option<String>, what: &str, limit: usize, now: String| {
            let Some(value) = value else { return Ok(false) };
            if value.is_empty() || value.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(GovernanceError::InvalidName(format!(
                    "{what}不能为空，也不能含空白"
                )));
            }
            if value.chars().count() > limit {
                return Err(GovernanceError::InvalidName(format!(
                    "{what}至多 {limit} 个字"
                )));
            }
            Ok(*value != now)
        };
        let changed = [
            check(&name, "姓名", 12, current.name.clone())?,
            check(&courtesy, "字", 6, self.courtesy_name(agent))?,
            check(&art, "号", 6, self.art_name(agent))?,
        ];
        if !changed.contains(&true) {
            return Err(GovernanceError::InvalidName(format!(
                "{}的姓名字号并无改易",
                current.name
            )));
        }
        Ok(CourtEvent::AgentRenamed {
            agent,
            name,
            courtesy,
            art,
        })
    }

    /// 皇帝的任命：当前 MVP 只有 CLI 皇帝可以调用此入口。
    pub fn propose_appointment(
        &self,
        agent: AgentId,
        office: Office,
    ) -> Result<CourtEvent, GovernanceError> {
        let person = self
            .agents
            .get(&agent)
            .ok_or(GovernanceError::UnknownAgent(agent))?;
        if self.purged.contains(&agent) {
            return Err(GovernanceError::Purged(agent));
        }
        if self.appointments.contains_key(&agent) {
            return Err(GovernanceError::AlreadyAppointed(agent));
        }
        if !office.exists_in(self.mode) {
            return Err(GovernanceError::OfficeNotInMode(office));
        }
        if let Some(d) = department_of(office)
            && d != person.speciality
        {
            return Err(GovernanceError::SpecialityMismatch {
                name: person.name.clone(),
                department: d,
            });
        }
        if office.is_unique() && self.appointments.values().any(|o| *o == office) {
            return Err(GovernanceError::OfficeOccupied(office));
        }
        Ok(CourtEvent::OfficialAppointed { agent, office })
    }

    pub fn propose_dismissal(&self, agent: AgentId) -> Result<CourtEvent, GovernanceError> {
        if !self.appointments.contains_key(&agent) {
            return Err(GovernanceError::NotAppointed(agent));
        }
        Ok(CourtEvent::OfficialDismissed { agent })
    }

    pub fn propose_reform(&self, mode: GovernanceMode) -> Result<CourtEvent, GovernanceError> {
        if self.mode == mode {
            return Err(GovernanceError::SameMode(mode));
        }
        Ok(CourtEvent::InstitutionReformed(mode))
    }
}
