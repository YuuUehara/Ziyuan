//! 科举：会试、殿试与状元。

use crate::{Court, GovernanceError};
use serde::{Deserialize, Serialize};
use ziyuan_core::{AgentId, CourtEvent, Speech, TokenUsage};

/// 一次科举应试（见 `CourtEvent::ExamTaken`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExamRecord {
    pub agent: AgentId,
    pub exam: String,
    pub score: u8,
    pub answer: String,
    pub model: Option<String>,
    pub usage: TokenUsage,
}
impl ExamRecord {
    /// 及格线。
    pub const PASS: u8 = 60;
    pub fn passed(&self) -> bool {
        self.score >= Self::PASS
    }
}

/// 一次殿试：皇帝的题目、各官员的答卷与钦点的状元。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PalaceExam {
    pub question: String,
    pub answers: Vec<Speech>,
    pub zhuangyuan: Option<AgentId>,
}

impl Court {
    /// 应试者须在册、未被革职。
    pub fn propose_exam(&self, agent: AgentId) -> Result<(), GovernanceError> {
        if !self.agents.contains_key(&agent) {
            return Err(GovernanceError::UnknownAgent(agent));
        }
        if self.purged.contains(&agent) {
            return Err(GovernanceError::Purged(agent));
        }
        Ok(())
    }

    /// 殿试：题目非空，至少一份答卷，答卷者在任所记官职且不重复。
    pub fn propose_palace_exam(
        &self,
        question: &str,
        answers: &[Speech],
    ) -> Result<(), GovernanceError> {
        if question.trim().is_empty() {
            return Err(GovernanceError::InvalidExam("殿试须有题目".into()));
        }
        if answers.is_empty() {
            return Err(GovernanceError::InvalidExam("无人作答".into()));
        }
        for (i, answer) in answers.iter().enumerate() {
            if self.appointments.get(&answer.speaker) != Some(&answer.office) {
                return Err(GovernanceError::NotAppointed(answer.speaker));
            }
            if answers[..i].iter().any(|a| a.speaker == answer.speaker) {
                let why = format!("官员 #{} 重复作答", answer.speaker.0);
                return Err(GovernanceError::InvalidExam(why));
            }
        }
        Ok(())
    }

    /// 钦点状元：须在最近一次殿试的答卷者中，且该次尚未点过。
    pub fn propose_zhuangyuan(&self, agent: AgentId) -> Result<CourtEvent, GovernanceError> {
        let last = self
            .palace_exams
            .last()
            .ok_or_else(|| GovernanceError::InvalidExam("尚未举行殿试".into()))?;
        if last.zhuangyuan.is_some() {
            return Err(GovernanceError::InvalidExam("本科状元已点".into()));
        }
        if !last.answers.iter().any(|a| a.speaker == agent) {
            let why = format!("官员 #{} 未参加本科殿试", agent.0);
            return Err(GovernanceError::InvalidExam(why));
        }
        Ok(CourtEvent::ZhuangyuanNamed { agent })
    }

    pub fn exams(&self) -> &[ExamRecord] {
        &self.exams
    }

    pub fn palace_exams(&self) -> &[PalaceExam] {
        &self.palace_exams
    }
}
