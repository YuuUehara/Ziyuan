//! 内阁：票拟、批红、廷议与分办。

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use ziyuan_core::{
    Agent, AgentId, CourtEvent, DagNode, Office, Persona, Record, Speech, TaskId, TokenUsage,
    search_records,
};
use ziyuan_governance::{Court, GovernanceError, Task, TaskStatus};
use ziyuan_runtime::{BoxError, WorkerBackend, guard};
use ziyuan_storage::EventStore;

use crate::reports::describe_record;
use crate::{AppError, Session, truncate};

/// Creation-time scheduling input. Dependencies are 1-based positions in this plan,
/// not task IDs. Keys identify nodes within this parent, not arbitrary tool calls.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduledSubtask {
    pub worker: AgentId,
    pub text: String,
    #[serde(default)]
    pub dependencies: Vec<usize>,
    #[serde(default)]
    pub attempts: Option<u32>,
    #[serde(default)]
    pub deadline: Option<u64>,
    #[serde(default)]
    pub budget: Option<u64>,
    #[serde(default)]
    pub artifact: Option<String>,
    #[serde(default)]
    pub approval_required: bool,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

fn scheduled_nodes(
    plan: &[ScheduledSubtask],
    ids: &[TaskId],
    writes: bool,
) -> Result<Vec<DagNode>, GovernanceError> {
    let invalid = |why: &str| GovernanceError::InvalidDag(why.into());
    let mut keys = BTreeSet::new();
    let mut nodes = Vec::new();
    for (i, sub) in plan.iter().enumerate() {
        let mut seen = BTreeSet::new();
        for &dep in &sub.dependencies {
            if dep == 0 || dep > plan.len() || dep == i + 1 || !seen.insert(dep) {
                return Err(invalid("依赖须为同批 1-based 索引，不得重复或依赖自身"));
            }
        }
        if sub.attempts == Some(0) || sub.budget == Some(0) {
            return Err(invalid("尝试次数与度支须为正数"));
        }
        if let Some(key) = &sub.idempotency_key
            && (key.trim().is_empty() || !keys.insert(key))
        {
            return Err(invalid("幂等键不得为空或在同一原诏令内重复"));
        }
        nodes.push(DagNode {
            task: ids[i],
            dependencies: sub.dependencies.iter().map(|d| ids[d - 1]).collect(),
            attempts: sub.attempts,
            deadline: sub.deadline,
            budget: sub.budget,
            artifact: sub.artifact.clone(),
            approval_required: sub.approval_required,
            idempotency_key: sub.idempotency_key.clone(),
        });
    }
    // Stable topological order (lowest submitted index first). Serializing in
    // array order would introduce cycles for legitimate backwards edges.
    let mut remaining: BTreeSet<usize> = (0..plan.len()).collect();
    let mut order = Vec::new();
    while !remaining.is_empty() {
        let next = remaining
            .iter()
            .copied()
            .find(|&i| {
                plan[i]
                    .dependencies
                    .iter()
                    .all(|d| !remaining.contains(&(d - 1)))
            })
            .ok_or_else(|| invalid("依赖成环，无从排程"))?;
        remaining.remove(&next);
        order.push(next);
    }
    if writes {
        for pair in order.windows(2) {
            let previous = ids[pair[0]];
            let deps = &mut nodes[pair[1]].dependencies;
            if !deps.contains(&previous) {
                deps.push(previous);
            }
        }
    }
    for node in &mut nodes {
        node.dependencies.sort();
    }
    Ok(nodes)
}

/// 从票拟回复中取拟派的官员 ID：「拟派」之后（没有则全文）的第一个 `#数字`。
pub(crate) fn parse_draft(reply: &str) -> Option<AgentId> {
    let after = reply.find("拟派").map_or(reply, |i| &reply[i..]);
    let (start, mark) = after
        .char_indices()
        .find(|(_, c)| matches!(c, '#' | '＃'))?;
    let digits: String = after[start + mark.len_utf8()..]
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().map(AgentId)
}

/// 一件实录的一行摘要（票拟、廷推参考前例用）。
pub(crate) fn precedent(r: &Record) -> String {
    format!(
        "#{}「{}」，#{} {}承办，{}",
        r.task.0,
        truncate(&r.edict, 40),
        r.worker.0,
        r.handler,
        truncate(&r.verdict, 40)
    )
}

/// 尚书汇总时收到的诏令内容：原诏令与各件子诏令的结局。
/// 模型有时照抄纪要格式，以「【第 1 轮】#4 张三（工部郎中）：」开头：去掉这段自报家门。
pub(crate) fn strip_own_label<'a>(reply: &'a str, name: &str) -> &'a str {
    let head: String = reply.chars().take(40).collect();
    match head.find(['：', ':']) {
        Some(end) if head[..end].contains(name) => {
            let colon = head[end..].chars().next().expect("有冒号").len_utf8();
            reply[end + colon..].trim_start()
        }
        _ => reply,
    }
}

/// 按模板下达的诏令的验收标准，附在承办与复核的提示中；不是按模板下达的为 `None`。
pub fn criteria_note(task: &Task) -> Option<String> {
    let used = task.template.as_ref()?;
    let mut note = format!(
        "验收标准（翰林院模板「{}」，都察院将逐条核验）：\n",
        used.name
    );
    for (i, c) in used.criteria.iter().enumerate() {
        note.push_str(&format!("{}. {c}\n", i + 1));
    }
    Some(note.trim_end().to_owned())
}

/// 最近一次廷议的纪要，附在票拟与承办的提示中；没有廷议则为 `None`。
pub fn debate_minutes(court: &Court, task: &Task) -> Option<String> {
    let speeches = task.debates.last()?;
    let mut minutes = "廷议纪要（仅供参考，以诏令为准）：\n".to_owned();
    for s in speeches {
        let name = court.agent(s.speaker).map_or("?", |a| a.name.as_str());
        minutes.push_str(&format!("{}{name}：{}\n", s.office.label(), s.text));
    }
    Some(minutes)
}

pub(crate) fn summary_prompt(court: &Court, task: &Task) -> String {
    let nonce = guard::nonce();
    let mut out = format!(
        "原诏令：{}\n\n此诏令已分给本部官员分头办理，各件结局如下。请核对各份回奏（可用工具抽查），\
         汇总成一份回奏呈皇帝：逐项说明结论，指出未办成或互相矛盾之处，不得把未办成的说成已办成。\n{}\n",
        task.edict.text,
        guard::warning("各份回奏", &nonce)
    );
    for id in task.split.iter().flat_map(|s| &s.subtasks) {
        let Some(sub) = court.task(*id) else {
            continue;
        };
        let name = sub
            .handler()
            .or(sub.assignee)
            .and_then(|a| court.agent(a))
            .map_or("?", |a| a.name.as_str());
        let outcome = match &sub.status {
            TaskStatus::Completed { summary, .. } => {
                format!("回奏：\n{}", guard::fence(&truncate(summary, 3000), &nonce))
            }
            TaskStatus::Failed { reason, .. } => format!("失败：{reason}"),
            TaskStatus::Cancelled => "已撤销".to_owned(),
            TaskStatus::Pending | TaskStatus::Assigned(_) => "未结束".to_owned(),
        };
        let artifact = sub
            .dag
            .as_ref()
            .and_then(|d| d.artifact.as_deref())
            .map_or(String::new(), |a| format!("（约定产出：{a}）"));
        out.push_str(&format!(
            "\n## 子诏令 #{}（{name}）：{}{artifact}\n{outcome}\n",
            id.0, sub.edict.text
        ));
    }
    out
}

/// 解析尚书的拆分方案：每行「#官员ID：内容」，以及「说明：……」。
pub(crate) fn parse_split(reply: &str) -> (Vec<(AgentId, String)>, String) {
    let mut plan = Vec::new();
    let mut note = String::new();
    for line in reply.lines() {
        let line = line.trim().trim_start_matches(['-', '*', '•', ' ']);
        if let Some(rest) = line
            .strip_prefix("说明")
            .map(|r| r.trim_start_matches(['：', ':', ' ']))
        {
            note = truncate(rest.trim(), 200);
            continue;
        }
        // `#` 前只允许列表编号与项目符号（如「1. 」「(2)」「- 」）。
        let Some(start) = line.find(['#', '＃']) else {
            continue;
        };
        let marker = |c: char| c.is_ascii_digit() || " .、)）(（-*•".contains(c);
        if !line[..start].chars().all(marker) {
            continue;
        }
        let rest = &line[start + line[start..].chars().next().expect("有 #").len_utf8()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        let text = rest[digits.len()..]
            .trim_start_matches(['：', ':', ' ', '、', '.'])
            .trim();
        if let (Ok(id), false) = (digits.parse(), text.is_empty()) {
            plan.push((AgentId(id), truncate(text, 2000)));
        }
    }
    (plan, note)
}

impl<S: EventStore> Session<S> {
    /// 内阁制下对内阁票拟批红，诏令方可执行。
    pub fn approve(&mut self, task: TaskId) -> Result<(), AppError> {
        self.commit(|court| {
            if court.in_retreat() {
                return Err(GovernanceError::InRetreat);
            }
            Ok(vec![court.propose_approval(task)?])
        })?;
        Ok(())
    }

    /// 内阁票拟：首辅（以其官职所配模型，否则默认模型）读诏令，从本部在任官员中拟定承办人，
    /// 附票拟意见记入事件簿。模型不可用、回复未指明合格人选时，按旧例（任务量均衡）拟派并在
    /// 意见中注明。阻塞直到模型返回。返回拟派的官员。
    pub fn draft(
        &mut self,
        task: TaskId,
        backend: &dyn WorkerBackend,
    ) -> Result<AgentId, AppError> {
        let court = &self.court;
        let entry = court
            .task(task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        // 旧例人选；同时校验制度、诏令状态与内阁、本部是否有人。
        let fallback = court.route(&entry.edict)?.worker;
        court.propose_draft(task, fallback, String::new(), None, TokenUsage::default())?;
        let (secretary, office) = court.coordinator().expect("路由已确认内阁有人");
        let candidates = court.draft_candidates(entry.edict.department);
        let model = court
            .model_of(secretary.id)
            .or(backend.model_name())
            .map(str::to_owned);
        let system = format!(
            "你是紫垣朝廷{}{}，为皇帝票拟：从候选官员中拟定一位承办下面这件诏令，并简述理由。\n\
             只能从候选名单中选。回复第一行必须是「拟派：#官员ID」，其后用一两句话说明理由，不超过 100 字。\n{}",
            office.label(),
            secretary.name,
            Persona::of(secretary.id).prompt()
        );
        let mut user = format!(
            "诏令（{}）：{}\n玉玺：{}\n候选：\n",
            entry.edict.department.label(),
            entry.edict.text,
            entry.edict.seal
        );
        for (agent, office) in &candidates {
            user.push_str(&format!(
                "#{} {} {}，{}\n",
                agent.id.0,
                agent.name,
                office.label(),
                describe_record(&court.service_record(agent.id))
            ));
        }
        if let Some(minutes) = debate_minutes(court, entry) {
            user.push_str(&format!("\n{minutes}"));
        }
        let records = court.records();
        let precedents = search_records(&records, &entry.edict.text, 3);
        if !precedents.is_empty() {
            user.push_str("\n翰林院实录（相关前例，供参考）：\n");
            for r in precedents {
                user.push_str(&format!("{}\n", precedent(r)));
            }
        }
        let answer = court
            .check_funds(entry, 0)
            .map_err(BoxError::from)
            .and_then(|()| backend.complete(model.as_deref(), &system, &user));
        let (worker, note, model, usage) = match answer {
            Ok((reply, usage)) => {
                let reply: String = reply.trim().chars().take(600).collect();
                match parse_draft(&reply).filter(|id| candidates.iter().any(|(a, _)| a.id == *id)) {
                    Some(id) => (id, reply, model, usage),
                    None => (
                        fallback,
                        format!("票拟未指明合格人选，按旧例拟派。原票：{reply}"),
                        model,
                        usage,
                    ),
                }
            }
            Err(e) => (
                fallback,
                format!("内阁未能票拟（{e}），按旧例拟派。"),
                None,
                TokenUsage::default(),
            ),
        };
        self.commit(|court| {
            Ok(vec![court.propose_draft(
                task,
                worker,
                note.clone(),
                model.clone(),
                usage,
            )?])
        })?;
        Ok(worker)
    }

    /// 廷议：`participants`（为空则取 [`Court::debate_candidates`]）就一件待办诏令轮流发言
    /// `rounds` 轮（1–3），每人以其官职所配模型（否则默认模型）和人格发言，后发言者看得到此前的发言。
    /// 每段发言经 `on_speech` 报告，结束后整场记入事件簿；纪要随后附在票拟与承办的提示中，
    /// 但不改变派工与玉玺。阻塞直到全部发言完毕。
    ///
    /// 有人未能发言（模型出错）时廷议中止：已有的发言照记，返回 `Some(原因)`；
    /// 一段发言也没有（含演示后端）则报错，不留记录。
    pub fn debate(
        &mut self,
        task: TaskId,
        participants: &[AgentId],
        rounds: usize,
        backend: &dyn WorkerBackend,
        mut on_speech: impl FnMut(&Agent, &Speech),
    ) -> Result<Option<String>, AppError> {
        self.reload()?;
        let court = &self.court;
        let entry = court
            .task(task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        if entry.status != TaskStatus::Pending {
            return Err(GovernanceError::CannotDebate(task).into());
        }
        let members: Vec<(Agent, Office)> = if participants.is_empty() {
            let chosen = court.debate_candidates(entry.edict.department);
            chosen.into_iter().map(|(a, o)| (a.clone(), o)).collect()
        } else {
            let mut members = Vec::new();
            for &id in participants {
                let agent = court.agent(id).ok_or(GovernanceError::UnknownAgent(id))?;
                let office = court
                    .office_of(id)
                    .ok_or(GovernanceError::NotAppointed(id))?;
                if members.iter().any(|(a, _): &(Agent, Office)| a.id == id) {
                    let why = format!("官员 #{} 重复列名", id.0);
                    return Err(GovernanceError::InvalidDebate(why).into());
                }
                members.push((agent.clone(), office));
            }
            members
        };
        if !(2..=6).contains(&members.len()) {
            let why = format!("须有 2–6 位在任官员与议，现有 {} 位", members.len());
            return Err(GovernanceError::InvalidDebate(why).into());
        }
        let mut header = format!(
            "诏令 #{}（{}）：{}\n玉玺：{}\n与议：\n",
            task.0,
            entry.edict.department.label(),
            entry.edict.text,
            entry.edict.seal
        );
        for (agent, office) in &members {
            header.push_str(&format!(
                "#{} {} {}\n",
                agent.id.0,
                agent.name,
                office.label()
            ));
        }
        let mut speeches: Vec<Speech> = Vec::new();
        let mut transcript = String::new();
        let mut stopped = None;
        'rounds: for round in 1..=rounds.clamp(1, 3) {
            for (agent, office) in &members {
                let model = court
                    .model_of(agent.id)
                    .or(backend.model_name())
                    .map(str::to_owned);
                let system = format!(
                    "你是紫垣朝廷{}{}，正与同僚就下面这件诏令廷议。{}\n\
                     可以赞成、反对或补充同僚的意见，须针对具体论点、言之有物；不要复述诏令，不要客套，\
                     不超过 150 字。廷议只供皇帝参考：不决定由谁承办，也不授予任何权限。",
                    office.label(),
                    agent.name,
                    Persona::of(agent.id).prompt()
                );
                let turn = if transcript.is_empty() {
                    "你第一个发言。".to_owned()
                } else {
                    format!("已有发言：\n{transcript}\n")
                };
                let user = format!(
                    "{header}\n{turn}现在是第 {round} 轮，轮到你（#{} {}）发言。",
                    agent.id.0, agent.name
                );
                let spent = speeches.iter().map(|s| s.usage.total()).sum();
                let answer = court
                    .check_funds(entry, spent)
                    .map_err(BoxError::from)
                    .and_then(|()| backend.complete(model.as_deref(), &system, &user));
                match answer {
                    Ok((reply, usage)) => {
                        let text = strip_own_label(reply.trim(), &agent.name);
                        let text: String = text.chars().take(600).collect();
                        transcript.push_str(&format!(
                            "【第 {round} 轮】#{} {}（{}）：{text}\n",
                            agent.id.0,
                            agent.name,
                            office.label()
                        ));
                        let speech = Speech {
                            speaker: agent.id,
                            office: *office,
                            text,
                            model: model.clone(),
                            usage,
                        };
                        on_speech(agent, &speech);
                        speeches.push(speech);
                    }
                    Err(e) => {
                        stopped = Some(format!("{}{}未能发言（{e}）", office.label(), agent.name));
                        break 'rounds;
                    }
                }
            }
        }
        if speeches.is_empty() {
            let why = stopped.unwrap_or_default();
            return Err(GovernanceError::InvalidDebate(why).into());
        }
        self.commit(|court| {
            court.propose_debate(task, &speeches)?;
            Ok(vec![CourtEvent::CourtDebated {
                task,
                speeches: speeches.clone(),
            }])
        })?;
        Ok(stopped)
    }

    /// 御定分办：皇帝亲自把待办诏令拆给本部官员，`plan` 为 (承办者, 子诏令内容)。
    /// 写子诏令（玉玺含动土木之权）查阅范围彼此重叠，自动按 ID 次序串行。返回子诏令 ID。
    pub fn split(
        &mut self,
        task: TaskId,
        plan: Vec<(AgentId, String)>,
    ) -> Result<Vec<TaskId>, AppError> {
        self.commit(|court| {
            let note = "御定分办".to_owned();
            court.propose_split_serial_writes(task, plan.clone(), note, None, TokenUsage::default())
        })?;
        Ok(self.subtasks_of(task))
    }

    /// Create an immutable scheduled split. Identical retries on this parent
    /// return its original IDs without events, even after execution has started.
    /// Equality compares workers, text and effective scheduling metadata after
    /// dependency sorting and automatic write serialization; it does not compare
    /// JSON spelling or redundant safety edges. Keys are optional and parent-local.
    /// Conflicting retries fail; keys do not deduplicate execution or tool calls.
    pub fn split_scheduled(
        &mut self,
        task: TaskId,
        plan: Vec<ScheduledSubtask>,
    ) -> Result<Vec<TaskId>, AppError> {
        self.reload()?;
        self.commit(|court| {
            let parent = court
                .task(task)
                .ok_or(GovernanceError::UnknownEdict(task))?;
            let basic: Vec<_> = plan.iter().map(|s| (s.worker, s.text.clone())).collect();
            let ids = if let Some(split) = &parent.split {
                if split.subtasks.len() != plan.len() {
                    return Err(GovernanceError::InvalidDag("分办重试方案不一致".into()));
                }
                split.subtasks.clone()
            } else {
                let CourtEvent::EdictSplit { subtasks, .. } = court.propose_split(
                    task,
                    basic.clone(),
                    "御定分办".into(),
                    None,
                    TokenUsage::default(),
                )?
                else {
                    unreachable!()
                };
                subtasks.iter().map(|s| s.id).collect()
            };
            let writes = parent
                .edict
                .seal
                .capabilities
                .iter()
                .any(|c| c.is_works_only());
            let nodes = scheduled_nodes(&plan, &ids, writes)?;
            if parent.split.is_some() {
                let identical = plan.iter().zip(&nodes).all(|(sub, node)| {
                    court.task(node.task).is_some_and(|old| {
                        old.assignee == Some(sub.worker)
                            && old.edict.text == sub.text
                            && match &old.dag {
                                Some(dag) => dag == node,
                                None => node.is_trivial(),
                            }
                    })
                });
                return if identical {
                    Ok(Vec::new())
                } else {
                    Err(GovernanceError::InvalidDag("分办重试方案不一致".into()))
                };
            }
            court.propose_split_dag(
                task,
                basic,
                "御定分办".into(),
                None,
                TokenUsage::default(),
                nodes,
            )
        })?;
        Ok(self.subtasks_of(task))
    }

    /// 尚书分办：本部尚书（其官职所配模型，否则默认模型）读诏令与本部官员，拆成 2–8 件子诏令。
    /// 模型不可用（含演示后端）或拆分方案不合法时报错，不留记录。阻塞直到模型返回。
    pub fn plan_split(
        &mut self,
        task: TaskId,
        backend: &dyn WorkerBackend,
    ) -> Result<Vec<TaskId>, AppError> {
        self.reload()?;
        let court = &self.court;
        let entry = court
            .task(task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        let department = entry.edict.department;
        let (minister, office) = court
            .department_head(department)
            .ok_or(GovernanceError::Vacant(Office::Minister(department)))?;
        // 先用占位方案校验诏令能否分办，免得白白调用模型。汇总是尚书之责，尚书不领子诏令。
        let staff: Vec<_> = court
            .draft_candidates(department)
            .into_iter()
            .filter(|(a, _)| a.id != minister.id)
            .collect();
        if staff.is_empty() {
            return Err(GovernanceError::InvalidSplit("本部除尚书外别无官员可分办".into()).into());
        }
        let placeholder: Vec<_> = staff.iter().map(|(a, _)| (a.id, "-".to_owned())).collect();
        let placeholder = [placeholder.clone(), placeholder].concat();
        if let Err(e @ GovernanceError::CannotSplit(_)) = court.propose_split(
            task,
            placeholder,
            String::new(),
            None,
            TokenUsage::default(),
        ) {
            return Err(e.into());
        }
        let model = court
            .model_of(minister.id)
            .or(backend.model_name())
            .map(str::to_owned);
        let system = format!(
            "你是紫垣朝廷{}{}，奉诏把下面这件诏令拆给本部官员分头办理，事后由你汇总回奏。\n\
             拆成 2–8 件可独立完成的子诏令，同一官员可领多件。每行一件，格式为「#官员ID：子诏令内容」；\n\
             子诏令内容要具体，写明查办的范围与要回奏的事项，不必重复官员姓名。汇总由你亲自来做，\n\
             不要列为子诏令。最后可另起一行「说明：……」，不超过 80 字。\n{}",
            office.label(),
            minister.name,
            Persona::of(minister.id).prompt()
        );
        let mut user = format!(
            "诏令（{}）：{}\n玉玺：{}\n本部官员：\n",
            department.label(),
            entry.edict.text,
            entry.edict.seal
        );
        for (agent, office) in &staff {
            user.push_str(&format!(
                "#{} {} {}，{}\n",
                agent.id.0,
                agent.name,
                office.label(),
                describe_record(&court.service_record(agent.id))
            ));
        }
        court.check_funds(entry, 0)?;
        let (reply, usage) = backend
            .complete(model.as_deref(), &system, &user)
            .map_err(|e| GovernanceError::InvalidSplit(format!("尚书未能拆分（{e}）")))?;
        let (plan, note) = parse_split(&reply);
        // 派给尚书本人的（多半是「汇总」）略去；内容开头重复的官员姓名去掉。
        let plan: Vec<_> = plan
            .into_iter()
            .filter(|(worker, _)| *worker != minister.id)
            .map(|(worker, text)| {
                let name = court.agent(worker).map_or("", |a| a.name.as_str());
                let text = match text.strip_prefix(name) {
                    Some(rest) if !name.is_empty() && rest.starts_with(['：', ':']) => rest
                        [rest.chars().next().expect("有冒号").len_utf8()..]
                        .trim()
                        .to_owned(),
                    _ => text,
                };
                (worker, text)
            })
            .collect();
        let note = if note.is_empty() {
            format!("{}{}分办", office.label(), minister.name)
        } else {
            note
        };
        self.commit(|court| {
            court.propose_split_serial_writes(
                task,
                plan.clone(),
                note.clone(),
                model.clone(),
                usage,
            )
        })?;
        Ok(self.subtasks_of(task))
    }

    pub(crate) fn subtasks_of(&self, task: TaskId) -> Vec<TaskId> {
        let split = self.court.task(task).and_then(|t| t.split.as_ref());
        split.map_or_else(Vec::new, |s| s.subtasks.clone())
    }
}
