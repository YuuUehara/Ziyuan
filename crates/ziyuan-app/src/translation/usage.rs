//! Read-only, event-backed accounting. Missing reports are never synthetic zeroes.
use std::collections::BTreeMap;

use serde::Serialize;
use ziyuan_core::{CourtEvent, Price, TaskId, TokenUsage, TranslationKind};
use ziyuan_governance::{Court, TaskStatus};
use ziyuan_storage::EventStore;

use crate::{AppError, Session};

#[derive(Clone, Debug, Serialize)]
pub struct TranslationUsage {
    pub project: String,
    pub stages: Vec<StageUsage>,
    pub tasks: Vec<TranslationTaskUsage>,
    pub totals: UsageTotals,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct StageUsage {
    pub kind: TranslationKind,
    pub totals: UsageTotals,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct UsageTotals {
    pub tasks: usize,
    pub statuses: BTreeMap<String, usize>,
    /// Known subtotal only; Some does not establish complete usage coverage.
    pub recorded_usage: Option<TokenUsage>,
    pub tasks_without_execution_usage: usize,
    /// Execution ModelUsage events, NOT the number of HTTP requests.
    pub usage_events: usize,
    pub tool_calls: u64,
    pub tool_failures: u64,
    /// Priced subtotals separated by currency; never a complete bill.
    pub estimates: BTreeMap<String, f64>,
    pub unpriced_usage: TokenUsage,
}

#[derive(Clone, Debug, Serialize)]
pub struct TranslationTaskUsage {
    pub task: TaskId,
    pub block: String,
    pub kind: TranslationKind,
    pub status: String,
    pub attempts: u32,
    /// Explicit per-task budget only, not permission to spend or remaining funds.
    pub budget: Option<u64>,
    pub recorded_execution_usage: Option<TokenUsage>,
    pub usage_reconciled: bool,
    pub totals: UsageTotals,
    pub charges: Vec<UsageCharge>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageCharge {
    pub purpose: String,
    pub model: Option<String>,
    pub usage: TokenUsage,
    pub rate: Option<UsageRate>,
    pub estimated_cost: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageRate {
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub currency: String,
    pub source: String,
}

impl UsageTotals {
    fn add(&mut self, other: &Self) {
        self.tasks += other.tasks;
        for (status, count) in &other.statuses {
            *self.statuses.entry(status.clone()).or_default() += count;
        }
        if let Some(usage) = other.recorded_usage {
            *self.recorded_usage.get_or_insert_default() += usage;
        }
        self.tasks_without_execution_usage += other.tasks_without_execution_usage;
        self.usage_events += other.usage_events;
        self.tool_calls += other.tool_calls;
        self.tool_failures += other.tool_failures;
        self.unpriced_usage += other.unpriced_usage;
        for (currency, cost) in &other.estimates {
            *self.estimates.entry(currency.clone()).or_default() += cost;
        }
    }
}

#[derive(Default)]
struct Recorded {
    model: Option<String>,
    execution: Option<TokenUsage>,
    events: usize,
    charges: BTreeMap<(&'static str, Option<String>), TokenUsage>,
}

impl Recorded {
    fn charge(&mut self, purpose: &'static str, model: &Option<String>, usage: TokenUsage) {
        // Governance events also represent manual decisions or missing usage with
        // zeroes. Unlike ModelUsage, they cannot prove a reported zero-token call.
        if purpose == "execution" || usage.total() > 0 {
            *self.charges.entry((purpose, model.clone())).or_default() += usage;
        }
    }
}

impl<S: EventStore> Session<S> {
    /// Accounts for every linked task, including failures, retries and old drafts.
    /// Does not read translation files or alter signoff/budget policy.
    pub fn translation_usage(
        &self,
        project: &str,
        price: impl Fn(&str) -> Option<(Price, &'static str)>,
    ) -> Result<TranslationUsage, AppError> {
        Ok(translation_usage(
            self.court(),
            self.history()?,
            project,
            price,
        ))
    }
}

/// Summarizes the supplied consistent court/history snapshot. An empty project
/// ledger is valid; project existence is checked by the caller, not by accounting.
pub fn translation_usage(
    court: &Court,
    history: &[CourtEvent],
    project: &str,
    price: impl Fn(&str) -> Option<(Price, &'static str)>,
) -> TranslationUsage {
    let mut recorded: BTreeMap<_, Recorded> = court
        .tasks()
        .filter(|task| {
            task.translation
                .as_ref()
                .is_some_and(|link| link.project == project)
        })
        .map(|task| (task.edict.id, Recorded::default()))
        .collect();
    for event in history {
        match event {
            CourtEvent::TaskAssigned { task, model, .. } => {
                if let Some(record) = recorded.get_mut(task) {
                    record.model = model.clone();
                }
            }
            CourtEvent::ModelUsage { task, usage, .. } => {
                if let Some(record) = recorded.get_mut(task) {
                    record.events += 1;
                    *record.execution.get_or_insert_default() += *usage;
                    record.charge("execution", &record.model.clone(), *usage);
                }
            }
            CourtEvent::EdictDrafted {
                task, model, usage, ..
            }
            | CourtEvent::EdictSplit {
                task, model, usage, ..
            }
            | CourtEvent::CensorReported {
                task, model, usage, ..
            }
            | CourtEvent::SuspicionReviewed {
                task, model, usage, ..
            } => {
                if let Some(record) = recorded.get_mut(task) {
                    let purpose = match event {
                        CourtEvent::EdictDrafted { .. } => "draft",
                        CourtEvent::EdictSplit { .. } => "split",
                        CourtEvent::CensorReported { .. } => "censure",
                        _ => "recheck",
                    };
                    record.charge(purpose, model, *usage);
                }
            }
            CourtEvent::CourtDebated { task, speeches } => {
                if let Some(record) = recorded.get_mut(task) {
                    for speech in speeches {
                        record.charge("debate", &speech.model, speech.usage);
                    }
                }
            }
            _ => {}
        }
    }
    let mut report = TranslationUsage {
        project: project.into(),
        stages: TranslationKind::ALL.into_iter().map(|kind| StageUsage {
            kind, totals: UsageTotals::default(),
        }).collect(),
        tasks: Vec::new(),
        totals: UsageTotals::default(),
        notes: vec![
            "仅按关联任务的事件簿记账；已记录用量是小计，不是完整用量。缺失 usage 为未知，不按零计算；有 usage 的任务也可能存在未报告的调用。".into(),
            "usage 事件数、任务数、指派尝试次数和工具调用数不是 HTTP 请求数。重试可共用 trace；历史 HTTP 请求总数未知。".into(),
            "费用按本次查询的费率估算，非执行时锁价、非服务商账单；缺模型或费率的用量单列，各币种不合并。订阅额度不能据此认定为按 token 付费。".into(),
            "未核对服务端缓存命中、折扣或账单；不据字节减少推算 token/费用节省。历史精确耗时未知，分钟级时辰不作计时。".into(),
            "预算与停止规则不变；本件预算不等于全局/官员余额。未减少词义核对、朱批复核、审校或人工分批验收；质量与操作者用时需另行记录。".into(),
        ],
    };
    for task in court.tasks() {
        let Some(record) = recorded.remove(&task.edict.id) else {
            continue;
        };
        let link = task.translation.as_ref().expect("selected linked task");
        let status = match task.status {
            TaskStatus::Pending => "pending",
            TaskStatus::Assigned(_) => "assigned",
            TaskStatus::Completed { .. } => "completed",
            TaskStatus::Failed { .. } => "failed",
            TaskStatus::Cancelled => "cancelled",
        }
        .to_string();
        let mut totals = UsageTotals {
            tasks: 1,
            statuses: BTreeMap::from([(status.clone(), 1)]),
            tasks_without_execution_usage: usize::from(record.execution.is_none()),
            usage_events: record.events,
            tool_calls: task.tool_calls.into(),
            tool_failures: task.tool_failures.into(),
            ..UsageTotals::default()
        };
        let mut charges = Vec::new();
        for ((purpose, model), usage) in record.charges {
            *totals.recorded_usage.get_or_insert_default() += usage;
            let quote = model.as_deref().and_then(&price).filter(|(p, _)| {
                p.input_per_million.is_finite()
                    && p.input_per_million >= 0.0
                    && p.output_per_million.is_finite()
                    && p.output_per_million >= 0.0
                    && !p.currency.trim().is_empty()
                    && p.cost(usage).is_finite()
            });
            let estimated_cost = quote.as_ref().map(|(p, _)| p.cost(usage));
            if let Some((p, _)) = &quote {
                *totals.estimates.entry(p.currency.clone()).or_default() += p.cost(usage);
            } else {
                totals.unpriced_usage += usage;
            }
            charges.push(UsageCharge {
                purpose: purpose.into(),
                model,
                usage,
                estimated_cost,
                rate: quote.map(|(p, source)| UsageRate {
                    input_per_million: p.input_per_million,
                    output_per_million: p.output_per_million,
                    currency: p.currency,
                    source: source.into(),
                }),
            });
        }
        let usage_reconciled = totals.recorded_usage.unwrap_or_default() == task.usage;
        if !usage_reconciled {
            report.notes.push(format!(
                "任务 {} 的事件用量与任务累计用量不一致；不能视作完整账本或完整费用。",
                task.edict.id.0
            ));
        }
        report.totals.add(&totals);
        report
            .stages
            .iter_mut()
            .find(|stage| stage.kind == link.kind)
            .expect("all translation stages")
            .totals
            .add(&totals);
        report.tasks.push(TranslationTaskUsage {
            task: task.edict.id,
            block: link.block.clone(),
            kind: link.kind,
            status,
            attempts: task.attempts,
            budget: task.budget,
            recorded_execution_usage: record.execution,
            usage_reconciled,
            totals,
            charges,
        });
    }
    report
}
