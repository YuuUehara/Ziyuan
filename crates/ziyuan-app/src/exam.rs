//! 科举：会试与殿试。

use std::path::Path;
use ziyuan_core::{Agent, AgentId, CourtEvent, Office, Persona, Speech};
use ziyuan_governance::GovernanceError;
use ziyuan_runtime::{BoxError, WorkerBackend};
use ziyuan_storage::EventStore;

use crate::{AppError, Session, truncate};

/// 礼部的一道考题：`.ziyuan/exam/<名>.txt`，格式为题目（可带「题：」前缀、可多行），
/// 其后一行「要点：甲；乙；丙」。官员读不到 `.ziyuan/`，因此看不到要点。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExamPaper {
    pub name: String,
    pub question: String,
    pub points: Vec<String>,
}
impl ExamPaper {
    pub fn parse(name: &str, text: &str) -> Result<Self, String> {
        let at = text
            .find("要点：")
            .or_else(|| text.find("要点:"))
            .ok_or("缺少「要点：」一行")?;
        let question = text[..at].trim();
        let question = question
            .strip_prefix("题：")
            .or_else(|| question.strip_prefix("题:"))
            .unwrap_or(question)
            .trim();
        let rest = &text[at..];
        let rest = &rest[rest.find([':', '：']).expect("已找到要点")..];
        let rest = &rest[rest.chars().next().expect("有冒号").len_utf8()..];
        let points: Vec<String> = rest
            .lines()
            .next()
            .unwrap_or("")
            .split(['；', ';', '，', ',', '、'])
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect();
        if question.is_empty() || points.is_empty() {
            return Err("题目与要点都不能为空".into());
        }
        Ok(Self {
            name: name.to_owned(),
            question: question.to_owned(),
            points,
        })
    }

    /// 答案命中要点的比例（0–100，向下取整）；英文不分大小写。
    pub fn grade(&self, answer: &str) -> u8 {
        let answer = answer.to_lowercase();
        let hit = self
            .points
            .iter()
            .filter(|p| answer.contains(&p.to_lowercase()))
            .count();
        (hit * 100 / self.points.len()) as u8
    }
}

/// 读取 `root/.ziyuan/exam/*.txt`，按文件名排序。没有考题或格式不合时报错。
pub fn exam_papers(root: &Path) -> Result<Vec<ExamPaper>, AppError> {
    let dir = root.join(".ziyuan").join("exam");
    let invalid = |why: String| AppError::from(GovernanceError::InvalidExam(why));
    let hint = "礼部尚未出题：在 .ziyuan/exam/ 下放 .txt 考题，先写题目，再写一行「要点：甲；乙」";
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(invalid(hint.into())),
        Err(e) => return Err(invalid(format!("读不到考题：{e}"))),
    };
    let mut paths: Vec<_> = entries
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "txt"))
        .collect();
    paths.sort();
    let mut papers = Vec::new();
    for path in paths {
        let name = path.file_stem().and_then(|n| n.to_str()).unwrap_or("?");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| invalid(format!("读不到考题 {name}：{e}")))?;
        let paper =
            ExamPaper::parse(name, &text).map_err(|e| invalid(format!("考题 {name}：{e}")))?;
        papers.push(paper);
    }
    if papers.is_empty() {
        return Err(invalid(hint.into()));
    }
    Ok(papers)
}

impl<S: EventStore> Session<S> {
    /// 御笔改票：皇帝改定拟派的承办人（仍须批红）。
    /// 礼部开科：`agent` 以其官职所配模型（否则默认模型）逐一作答 `root` 下 `.ziyuan/exam/` 中的考题，
    /// 按要点判分，每道题记一条 `ExamTaken`，并经 `on_result` 报告。不能查阅文件，单轮作答。
    /// 没有考题、模型不可用（含演示后端）时报错；中途出错则已考的照记。阻塞直到全部考完。
    pub fn exam(
        &mut self,
        agent: AgentId,
        root: &Path,
        backend: &dyn WorkerBackend,
        mut on_result: impl FnMut(&ExamPaper, u8),
    ) -> Result<usize, AppError> {
        self.reload()?;
        self.court.propose_exam(agent)?;
        let papers = exam_papers(root)?;
        let person = self.court.agent(agent).expect("已确认在册").clone();
        let office = self
            .court
            .office_of(agent)
            .map_or("未任官职的".to_owned(), |o| o.label());
        let model = self
            .court
            .model_of(agent)
            .or(backend.model_name())
            .map(str::to_owned);
        let system = format!(
            "你是紫垣朝廷{office}{}，赴礼部应试。{}\n\\
             不能查阅任何文件，凭所知作答，不超过 300 字。",
            person.name,
            Persona::of(agent).prompt()
        );
        let mut taken = 0;
        for paper in &papers {
            self.court.check_treasury(0)?;
            let (answer, usage) = match backend.complete(model.as_deref(), &system, &paper.question)
            {
                Ok(reply) => reply,
                Err(e) if taken == 0 => {
                    let why = format!("{}未能应试（{e}）", person.name);
                    return Err(GovernanceError::InvalidExam(why).into());
                }
                Err(_) => break,
            };
            let answer = truncate(answer.trim(), 2000);
            let score = paper.grade(&answer);
            self.commit(|court| {
                court.propose_exam(agent)?;
                Ok(vec![CourtEvent::ExamTaken {
                    agent,
                    exam: paper.name.clone(),
                    score,
                    answer: answer.clone(),
                    model: model.clone(),
                    usage,
                }])
            })?;
            on_result(paper, score);
            taken += 1;
        }
        Ok(taken)
    }

    /// 殿试：皇帝亲自出题，在任官员（至多八位，按 ID）以其官职所配模型各答一卷，经 `on_answer` 报告。
    /// 有人未能作答（模型出错、国库告罄）时停止，已有的答卷照记并返回原因；一卷也没有则报错。
    pub fn palace_exam(
        &mut self,
        question: &str,
        backend: &dyn WorkerBackend,
        mut on_answer: impl FnMut(&Agent, &Speech),
    ) -> Result<Option<String>, AppError> {
        self.reload()?;
        let court = &self.court;
        if question.trim().is_empty() {
            return Err(GovernanceError::InvalidExam("殿试须有题目".into()).into());
        }
        let attendees: Vec<(Agent, Office)> = court
            .agents()
            .filter_map(|a| Some((a.clone(), court.office_of(a.id)?)))
            .take(8)
            .collect();
        if attendees.is_empty() {
            return Err(GovernanceError::InvalidExam("朝中无人在任，殿试无人应考".into()).into());
        }
        let mut answers: Vec<Speech> = Vec::new();
        let mut stopped = None;
        for (agent, office) in &attendees {
            let model = court
                .model_of(agent.id)
                .or(backend.model_name())
                .map(str::to_owned);
            let system = format!(
                "你是紫垣朝廷{}{}，今日殿试，天子亲策于奉天殿。{}\n\\
                 对策须切题、有见地，不超过 200 字；不要客套，不要复述题目。",
                office.label(),
                agent.name,
                Persona::of(agent.id).prompt()
            );
            let spent = answers.iter().map(|a| a.usage.total()).sum();
            let answer = court
                .check_treasury(spent)
                .map_err(BoxError::from)
                .and_then(|()| backend.complete(model.as_deref(), &system, question));
            match answer {
                Ok((reply, usage)) => {
                    let speech = Speech {
                        speaker: agent.id,
                        office: *office,
                        text: truncate(reply.trim(), 800),
                        model,
                        usage,
                    };
                    on_answer(agent, &speech);
                    answers.push(speech);
                }
                Err(e) => {
                    stopped = Some(format!("{}{}未能作答（{e}）", office.label(), agent.name));
                    break;
                }
            }
        }
        if answers.is_empty() {
            let why = stopped.unwrap_or_default();
            return Err(GovernanceError::InvalidExam(why).into());
        }
        let question = question.trim().to_owned();
        self.commit(|court| {
            court.propose_palace_exam(&question, &answers)?;
            Ok(vec![CourtEvent::PalaceExamHeld {
                question: question.clone(),
                answers: answers.clone(),
            }])
        })?;
        Ok(stopped)
    }
}
