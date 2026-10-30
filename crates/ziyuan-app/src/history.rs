//! 起居注：按诏令、官员、类型与时间筛选事件，并描述每条事件。

use ziyuan_core::{AgentId, CourtEvent, TaskId};
use ziyuan_governance::Suspicion;

use crate::edicts::{seal_note, skipped_note};
use crate::time::{format_time, tz_offset};
use crate::truncate;

/// 起居注的筛选条件；各项同时满足。时间为 Unix 秒。
#[derive(Clone, Debug, Default)]
pub struct HistoryFilter {
    pub task: Option<TaskId>,
    pub agent: Option<AgentId>,
    /// 事件类型代码（见 [`ziyuan_storage::event_kind`]），如 `tool`、`usage`。
    pub kind: Option<String>,
    pub since: Option<u64>,
    pub until: Option<u64>,
    /// 只取相符者中最后（最新）的若干条；只为这些生成中文描述，事件簿很长时也快。
    pub limit: Option<usize>,
}

/// 起居注的一条：`seq` 为事件簿中的序号（从 1 起，时辰不单列，故可能跳号），`at` 为所属时辰。
#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub seq: usize,
    pub at: Option<u64>,
    pub kind: String,
    pub text: String,
}

/// 按条件筛选起居注。早于第一条时辰的事件（v0.5 以前的日志）没有时间，按时间筛选时不列出。
/// 事件类型不认识时报错，并列出本事件簿中有的类型。
pub fn history_entries(
    history: &[CourtEvent],
    filter: &HistoryFilter,
) -> Result<Vec<HistoryEntry>, String> {
    if let Some(kind) = &filter.kind {
        let kinds: std::collections::BTreeSet<String> =
            history.iter().map(ziyuan_storage::event_kind).collect();
        if !kinds.contains(kind) {
            let known: Vec<_> = kinds.into_iter().filter(|k| k != "clock").collect();
            return Err(format!(
                "没有「{kind}」类事件；本事件簿有：{}",
                known.join(" ")
            ));
        }
    }
    // 开国须为第一条，其时辰紧随其后。
    let mut at = match history.get(1) {
        Some(CourtEvent::Clock(t)) => Some(*t),
        _ => None,
    };
    let mut matched = Vec::new();
    for (i, event) in history.iter().enumerate() {
        if let CourtEvent::Clock(t) = event {
            at = Some(*t);
            continue;
        }
        let keep = filter
            .kind
            .as_ref()
            .is_none_or(|k| *k == ziyuan_storage::event_kind(event))
            && filter.task.is_none_or(|t| event_tasks(event).contains(&t))
            && filter
                .agent
                .is_none_or(|a| event_agents(event).contains(&a))
            && filter.since.is_none_or(|s| at.is_some_and(|t| t >= s))
            && filter.until.is_none_or(|u| at.is_some_and(|t| t <= u));
        if keep {
            matched.push((i, at));
        }
    }
    let skip = filter
        .limit
        .map_or(0, |limit| matched.len().saturating_sub(limit));
    Ok(matched
        .into_iter()
        .skip(skip)
        .map(|(i, at)| HistoryEntry {
            seq: i + 1,
            at,
            kind: ziyuan_storage::event_kind(&history[i]),
            text: describe_event(&history[i]),
        })
        .collect())
}

/// 事件涉及的诏令（分办含各子诏令）。
pub(crate) fn event_tasks(event: &CourtEvent) -> Vec<TaskId> {
    use CourtEvent::*;
    match event {
        EdictIssued(edict) => vec![edict.id],
        EdictSplit { task, subtasks, .. } => std::iter::once(*task)
            .chain(subtasks.iter().map(|s| s.id))
            .collect(),
        EdictDag { task, nodes } => std::iter::once(*task)
            .chain(nodes.iter().map(|n| n.task))
            .collect(),
        EdictDrafted { task, .. }
        | CourtDebated { task, .. }
        | EdictApproved { task }
        | TaskAssigned { task, .. }
        | ToolInvoked { task, .. }
        | TaskCompleted { task, .. }
        | TaskFailed { task, .. }
        | TaskCancelled { task }
        | ChangesProposed { task, .. }
        | ChangesApplied { task, .. }
        | ChangesDiscarded { task }
        | CensorReported { task, .. }
        | SuspicionRaised { task, .. }
        | SuspicionReviewed { task, .. }
        | SuspicionAcquitted { task, .. }
        | CourierDelivered { task, .. }
        | CourierDispatched { task, .. }
        | CourierReplied { task, .. }
        | EdictExpedited { task }
        | TemplateApplied { task, .. }
        | ModelUsage { task, .. }
        | TaskReviewed { task, .. }
        | TranslationLinked { task, .. } => vec![*task],
        Founded(_)
        | Clock(_)
        | AgentRegistered(_)
        | AgentRenamed { .. }
        | OfficialAppointed { .. }
        | OfficialDismissed { .. }
        | OfficialPurged { .. }
        | InstitutionReformed(_)
        | EmperorRetreat(_)
        | OfficeModelSet { .. }
        | OfficeCeilingSet { .. }
        | BudgetSet { .. }
        | ExamTaken { .. }
        | PalaceExamHeld { .. }
        | ZhuangyuanNamed { .. }
        | CourtNominated { .. }
        | OfficialCaned { .. }
        | OfficialPardoned { .. }
        | TemplateSaved { .. }
        | TemplateRemoved { .. }
        | TranslationFinalized { .. }
        | TranslationReopened { .. } => Vec::new(),
    }
}

/// 事件点名的官员（承办、拟派、复核、发言、推举等）。
pub(crate) fn event_agents(event: &CourtEvent) -> Vec<AgentId> {
    use CourtEvent::*;
    match event {
        AgentRegistered(a) => vec![a.id],
        OfficialAppointed { agent, .. }
        | AgentRenamed { agent, .. }
        | OfficialDismissed { agent }
        | OfficialPurged { agent }
        | ExamTaken { agent, .. }
        | ZhuangyuanNamed { agent }
        | OfficialCaned { agent }
        | OfficialPardoned { agent } => vec![*agent],
        EdictDrafted { worker, .. }
        | TaskAssigned { worker, .. }
        | TaskCompleted { worker, .. } => vec![*worker],
        CensorReported { censor, .. } => vec![*censor],
        SuspicionReviewed { reviewer, .. } => vec![*reviewer],
        EdictSplit { subtasks, .. } => subtasks.iter().map(|s| s.worker).collect(),
        CourtDebated { speeches, .. } => speeches.iter().map(|s| s.speaker).collect(),
        PalaceExamHeld { answers, .. } => answers.iter().map(|s| s.speaker).collect(),
        CourtNominated { votes, .. } => votes.iter().flat_map(|v| [v.voter, v.candidate]).collect(),
        _ => Vec::new(),
    }
}

pub fn describe_event(event: &CourtEvent) -> String {
    match event {
        CourtEvent::Founded(m) => format!("开国：{}", m.label()),
        CourtEvent::AgentRegistered(a) => format!("登记官员 #{} {}", a.id.0, a.name),
        CourtEvent::OfficialAppointed { agent, office } => {
            format!("任命 #{} 为{}", agent.0, office.label())
        }
        CourtEvent::OfficialDismissed { agent } => format!("罢免官员 #{}", agent.0),
        CourtEvent::AgentRenamed {
            agent,
            name,
            courtesy,
            art,
        } => {
            let parts: Vec<String> = [("改名", name), ("改字", courtesy), ("改号", art)]
                .into_iter()
                .filter_map(|(verb, v)| v.as_ref().map(|v| format!("{verb}{v}")))
                .collect();
            format!("官员 #{} {}", agent.0, parts.join("，"))
        }
        CourtEvent::OfficialPurged { agent } => format!("革职 #{}，削去仕籍", agent.0),
        CourtEvent::EdictApproved { task } => format!("诏令 #{} 批红", task.0),
        CourtEvent::EdictDrafted {
            task,
            worker,
            model,
            ..
        } => match model {
            Some(model) => format!(
                "诏令 #{} 内阁票拟：拟派 #{}（模型 {model}）",
                task.0, worker.0
            ),
            None => format!("诏令 #{} 票拟：拟派 #{}", task.0, worker.0),
        },
        CourtEvent::EmperorRetreat(true) => "皇帝移居西苑修道，司礼监代批".into(),
        CourtEvent::EmperorRetreat(false) => "皇帝回朝亲政".into(),
        CourtEvent::InstitutionReformed(m) => format!("改制：{}", m.label()),
        CourtEvent::EdictIssued(e) => {
            format!("诏令 #{}：{}{}", e.id.0, e.text, seal_note(&e.seal))
        }
        CourtEvent::TaskAssigned {
            task,
            worker,
            model,
        } => match model {
            Some(model) => format!("诏令 #{} 派给 #{}（模型 {model}）", task.0, worker.0),
            None => format!("诏令 #{} 派给 #{}", task.0, worker.0),
        },
        CourtEvent::OfficeModelSet { office, model } => match model {
            Some(model) => format!("{}改用模型 {model}", office.label()),
            None => format!("{}恢复默认模型", office.label()),
        },
        CourtEvent::BudgetSet { scope, tokens } => match tokens {
            Some(t) => format!("户部度支 · {}：限 {t} tokens", scope.label()),
            None => format!("户部度支 · {}：取消限额", scope.label()),
        },
        CourtEvent::OfficeCeilingSet { office, ceiling } => match ceiling {
            Some(seal) => format!("{}玉玺上限：{seal}", office.label()),
            None => format!("{}取消玉玺上限", office.label()),
        },
        CourtEvent::ToolInvoked {
            task,
            tool,
            args_json,
            ok,
        } => format!(
            "诏令 #{} 调用 {tool} {args_json}（{}）",
            task.0,
            if *ok { "成功" } else { "失败" }
        ),
        CourtEvent::TaskCompleted { task, worker, .. } => {
            format!("诏令 #{} 由 #{} 回奏", task.0, worker.0)
        }
        CourtEvent::TaskFailed { task, reason } => format!("诏令 #{} 失败：{reason}", task.0),
        CourtEvent::TaskCancelled { task } => format!("诏令 #{} 撤销", task.0),
        CourtEvent::ChangesProposed { task, changes } => {
            let paths: Vec<_> = changes.iter().map(|c| c.path.as_str()).collect();
            format!("诏令 #{} 呈报改动：{}", task.0, paths.join("、"))
        }
        CourtEvent::ChangesApplied { task, skipped } => {
            format!("诏令 #{} 改动准奏落盘{}", task.0, skipped_note(skipped))
        }
        CourtEvent::CourtDebated { task, speeches } => {
            let mut speakers: Vec<String> = Vec::new();
            for s in speeches {
                let id = format!("#{}", s.speaker.0);
                if !speakers.contains(&id) {
                    speakers.push(id);
                }
            }
            format!(
                "诏令 #{} 廷议：{} 段发言（{}）",
                task.0,
                speeches.len(),
                speakers.join("、")
            )
        }
        CourtEvent::EdictSplit { task, subtasks, .. } => {
            let parts: Vec<_> = subtasks
                .iter()
                .map(|s| format!("#{}→#{}", s.id.0, s.worker.0))
                .collect();
            format!("诏令 #{} 分办：{}", task.0, parts.join("、"))
        }
        CourtEvent::EdictDag { task, nodes } => {
            let parts: Vec<_> = nodes
                .iter()
                .map(|n| {
                    let deps = if n.dependencies.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "（先办 {}）",
                            n.dependencies
                                .iter()
                                .map(|d| format!("#{}", d.0))
                                .collect::<Vec<_>>()
                                .join("、")
                        )
                    };
                    format!("#{}{deps}", n.task.0)
                })
                .collect();
            format!("诏令 #{} 排程：{}", task.0, parts.join("、"))
        }
        CourtEvent::CensorReported {
            task,
            censor,
            verdict,
            evidence,
            ..
        } => format!(
            "都察院 #{} 复核诏令 #{}：{}（{evidence}）",
            censor.0,
            task.0,
            verdict.label()
        ),
        CourtEvent::ChangesDiscarded { task } => format!("诏令 #{} 改动驳回", task.0),
        CourtEvent::SuspicionRaised {
            task,
            source,
            threat,
            excerpt,
        } => {
            let bureau = match source.strip_prefix("tool:") {
                Some(tool) => format!("锦衣卫密报（{tool}）"),
                None if source == Suspicion::COURIER => "锦衣卫密报（驿传）".to_owned(),
                None => "刑部勘问回奏".to_owned(),
            };
            format!("诏令 #{} {bureau}：{}「{excerpt}」", task.0, threat.label())
        }
        CourtEvent::SuspicionReviewed {
            task,
            index,
            reviewer,
            upheld,
            ..
        } => format!(
            "刑部 #{} 复勘诏令 #{} 第 {} 处检出：{}",
            reviewer.0,
            task.0,
            index + 1,
            if *upheld { "维持" } else { "误报" }
        ),
        CourtEvent::SuspicionAcquitted { task, index } => {
            format!("平反诏令 #{} 第 {} 处检出", task.0, index + 1)
        }
        CourtEvent::ExamTaken {
            agent, exam, score, ..
        } => format!("礼部科举：#{} 应试「{exam}」，得 {score} 分", agent.0),
        CourtEvent::PalaceExamHeld { question, answers } => format!(
            "殿试：「{}」，{} 人对策",
            truncate(question, 40),
            answers.len()
        ),
        CourtEvent::ZhuangyuanNamed { agent } => format!("殿试传胪：钦点 #{} 为状元", agent.0),
        CourtEvent::CourierDelivered { task, station } => {
            format!("兵部驿传：诏令 #{} 自「{station}」入队", task.0)
        }
        CourtEvent::EdictExpedited { task } => format!("八百里加急：诏令 #{} 插队先行", task.0),
        CourtEvent::CourierDispatched {
            task,
            pull_request,
            op,
        } => format!(
            "兵部驿传：诏令 #{} 即将{}（幂等键 {op}）",
            task.0,
            if *pull_request {
                "开草稿 PR"
            } else {
                "送回回奏"
            }
        ),
        CourtEvent::CourierReplied {
            task,
            pull_request,
            url,
            ..
        } => format!(
            "兵部驿传：诏令 #{} {}送回来处（{url}）",
            task.0,
            if *pull_request {
                "改动开为草稿 PR，"
            } else {
                "回奏"
            }
        ),
        CourtEvent::TemplateSaved { name, template } => format!(
            "翰林院存模板「{name}」（{}，验收 {} 条）",
            template.department.label(),
            template.criteria.len()
        ),
        CourtEvent::TemplateRemoved { name } => format!("翰林院撤去模板「{name}」"),
        CourtEvent::CourtNominated { office, votes } => {
            let mut tally: Vec<(AgentId, usize)> = Vec::new();
            for v in votes {
                match tally.iter_mut().find(|(id, _)| *id == v.candidate) {
                    Some((_, n)) => *n += 1,
                    None => tally.push((v.candidate, 1)),
                }
            }
            let ranked: Vec<String> = {
                tally.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
                tally
                    .iter()
                    .map(|(id, n)| format!("#{} {n} 票", id.0))
                    .collect()
            };
            format!(
                "廷推{}：{} 位大臣推举，{}",
                office.label(),
                votes.len(),
                ranked.join("、")
            )
        }
        CourtEvent::OfficialCaned { agent } => format!("廷杖 #{}，降级留用", agent.0),
        CourtEvent::OfficialPardoned { agent } => format!("宽宥 #{}，复其品级与玉玺", agent.0),
        CourtEvent::TemplateApplied {
            task,
            name,
            criteria,
        } => format!(
            "诏令 #{} 按模板「{name}」下达，验收 {} 条",
            task.0,
            criteria.len()
        ),
        CourtEvent::ModelUsage { task, usage, trace } => {
            let trace = trace
                .as_ref()
                .map_or(String::new(), |t| format!("（trace {t}）"));
            format!("诏令 #{} 模型用量：{usage}{trace}", task.0)
        }
        CourtEvent::Clock(at) => format!("时辰：{}", format_time(*at, tz_offset())),
        CourtEvent::TaskReviewed {
            task,
            verdict,
            note,
        } => {
            let note = if note.is_empty() {
                String::new()
            } else {
                format!("：{note}")
            };
            format!("诏令 #{} 评定为{}{note}", task.0, verdict.label())
        }
        CourtEvent::TranslationLinked {
            task, kind, block, ..
        } => format!("译书关联：#{} {} {block}", task.0, kind.label()),
        CourtEvent::TranslationFinalized { block, .. } => format!("御览定稿：{block}"),
        CourtEvent::TranslationReopened { block, reason, .. } => {
            format!("退回重修：{block}（{reason}）")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ziyuan_core::TranslationKind;

    #[test]
    fn translation_events_are_described_and_linked_to_their_task() {
        let linked = CourtEvent::TranslationLinked {
            task: TaskId(12),
            project: "0123456789abcdef0123456789abcdef".into(),
            block: "c0001-b0001".into(),
            kind: TranslationKind::Translate,
            source_hash: "a".repeat(64),
            context_version: 0,
            output_path: "translations/0123456789abcdef0123456789abcdef/c0001-b0001/zh.json".into(),
        };
        assert_eq!(event_tasks(&linked), [TaskId(12)]);
        assert_eq!(describe_event(&linked), "译书关联：#12 初译 c0001-b0001");
        let review = CourtEvent::TranslationLinked {
            task: TaskId(12),
            project: "0123456789abcdef0123456789abcdef".into(),
            block: "c0001-b0001".into(),
            kind: TranslationKind::ReviewFidelity,
            source_hash: "a".repeat(64),
            context_version: 0,
            output_path: String::new(),
        };
        assert_eq!(
            describe_event(&review),
            "译书关联：#12 忠实审校 c0001-b0001"
        );
        let finalized = CourtEvent::TranslationFinalized {
            project: "0123456789abcdef0123456789abcdef".into(),
            block: "c0001-b0001".into(),
            translation_hash: "b".repeat(64),
            context_version: 3,
            zhupi_version: 2,
        };
        assert!(event_tasks(&finalized).is_empty());
        assert_eq!(describe_event(&finalized), "御览定稿：c0001-b0001");
        let reopened = CourtEvent::TranslationReopened {
            project: "0123456789abcdef0123456789abcdef".into(),
            block: "c0001-b0001".into(),
            reason: "人名前后不一".into(),
        };
        assert!(event_tasks(&reopened).is_empty());
        assert_eq!(
            describe_event(&reopened),
            "退回重修：c0001-b0001（人名前后不一）"
        );
    }
}
