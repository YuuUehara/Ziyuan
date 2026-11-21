//! 考成与督职：用量、花费与官员政绩。

use std::collections::BTreeMap;
use ziyuan_core::{AgentId, BudgetScope, Department, GovernanceMode, Office, Price, TokenUsage};
use ziyuan_governance::{Court, ServiceRecord, Task};

/// 承办所用的 token：诏令累计用量减去票拟、拆分、复核与廷议的部分。
pub(crate) fn execution_usage(task: &Task) -> TokenUsage {
    let mut usage = task.usage;
    for (_, extra, _) in overheads(task) {
        usage.input = usage.input.saturating_sub(extra.input);
        usage.output = usage.output.saturating_sub(extra.output);
        usage.cache_read = usage.cache_read.saturating_sub(extra.cache_read);
        usage.cache_write = usage.cache_write.saturating_sub(extra.cache_write);
    }
    usage
}

/// 承办之外的模型开销及所用模型：内阁票拟、尚书拆分、都察院复核、历次廷议发言与刑部复勘。
pub(crate) fn overheads(task: &Task) -> Vec<(&'static str, TokenUsage, Option<&str>)> {
    let draft = task
        .draft
        .as_ref()
        .map(|d| ("票拟", d.usage, d.model.as_deref()));
    let split = task
        .split
        .as_ref()
        .map(|s| ("分办", s.usage, s.model.as_deref()));
    let censure = task
        .censure
        .as_ref()
        .map(|c| ("复核", c.usage, c.model.as_deref()));
    let speeches = task.debates.iter().flatten();
    let rechecks = task.suspicions.iter().filter_map(|s| s.review.as_ref());
    [draft, split, censure]
        .into_iter()
        .flatten()
        .chain(speeches.map(|s| ("廷议", s.usage, s.model.as_deref())))
        .chain(rechecks.map(|r| ("复勘", r.usage, r.model.as_deref())))
        .collect()
}

/// 一件诏令的全部开销：承办与各项额外开销，各附用途与所用模型。
pub(crate) fn spending(task: &Task) -> Vec<(&'static str, TokenUsage, Option<&str>)> {
    let executing = ("承办", execution_usage(task), task.model.as_deref());
    std::iter::once(executing).chain(overheads(task)).collect()
}

/// 按各诏令实际所用的模型估算花费，如「约 0.0123 元」。`default_model` 用于未记下模型的早期诏令；
/// 查不到单价的诏令不计入。全部查不到（或没有用量）时返回 `None`。
fn format_currency_totals(costs: &BTreeMap<String, f64>) -> Option<String> {
    if costs.is_empty() {
        return None;
    }
    let parts: Vec<String> = costs
        .iter()
        .map(|(currency, cost)| format!("{cost:.4} {currency}"))
        .collect();
    Some(format!("约 {}", parts.join(" + ")))
}

pub fn estimate_cost<'a>(
    tasks: impl IntoIterator<Item = &'a Task>,
    default_model: Option<&str>,
    price: impl Fn(&str) -> Option<Price>,
) -> Option<String> {
    let mut totals: BTreeMap<String, f64> = BTreeMap::new();
    for task in tasks {
        // 承办、票拟、拆分、复核与廷议可能用不同模型，按币种分别计价，避免混加不同币种。
        for (_, usage, model) in spending(task) {
            if usage.total() == 0 {
                continue;
            }
            let Some(p) = model.or(default_model).and_then(&price) else {
                continue;
            };
            *totals.entry(p.currency.clone()).or_insert(0.0) += p.cost(usage);
        }
    }
    format_currency_totals(&totals)
}

pub fn describe_record(r: &ServiceRecord) -> String {
    format!(
        "采纳 {}，驳回 {}，失败 {}，待评定 {}；御史背书 {}，弹劾 {}；实证 {}{}{}{}{}",
        r.accepted,
        r.rejected,
        r.failed,
        r.unreviewed,
        r.endorsed,
        r.impeached,
        r.verified,
        match r.exams {
            0 => String::new(),
            n => format!("；科举及格 {}/{n}", r.exams_passed),
        },
        match r.zhuangyuan {
            0 => String::new(),
            n => format!("；状元 {n}"),
        },
        match r.nominated {
            0 => String::new(),
            n => format!("；廷推得票 {n}"),
        },
        match r.caned {
            0 => String::new(),
            n => format!("；受廷杖 {n} 次"),
        }
    )
}

/// 考成等第：采纳、背书、实证为功，驳回、弹劾、失败为过。
pub fn kaocheng_grade(r: &ServiceRecord) -> &'static str {
    let handled = r.accepted + r.rejected + r.unreviewed + r.failed;
    let score = r.merit() - i64::from(r.failed);
    match () {
        _ if handled == 0 => "无事可考",
        _ if score >= 2 => "称职",
        _ if score < 0 => "不及",
        _ => "平常",
    }
}

/// 考成簿（仿万历初张居正考成法：立限考事，以事责人）：在任官员逐一列出功过与等第。
/// 只是呈现，不入事件簿，也不自动黜陟。
pub fn kaocheng_report(court: &Court) -> String {
    let mut out = String::from(match court.mode() {
        GovernanceMode::Cabinet => {
            "考成簿　万历元年，首辅张居正上《请稽查章奏随事考成以修实政疏》：立限考事，以事责人，月有考，岁有稽。"
        }
        GovernanceMode::DirectRule => {
            "考成簿　考成法本是内阁首辅张居正所行；今已废相，无人代劳，只好由天子亲自考成。"
        }
        GovernanceMode::Chancellor => {
            "考成簿　元无考成之法，今借明制，由中书省代为考成——丞相考百官，谁来考丞相？"
        }
    });
    out.push('\n');
    let mut secretary_failed = false;
    let mut any = false;
    for agent in court.agents() {
        let Some(office) = court.office_of(agent.id) else {
            continue;
        };
        any = true;
        let record = court.service_record(agent.id);
        let grade = kaocheng_grade(&record);
        secretary_failed |= office == Office::FirstGrandSecretary && grade == "不及";
        out.push_str(&format!(
            "#{} {}（{}）：{} —— {grade}\n",
            agent.id.0,
            agent.name,
            office.label(),
            describe_record(&record)
        ));
    }
    if !any {
        out.push_str("朝中无一人在任，无事可考。\n");
    }
    if secretary_failed {
        out.push_str("首辅亦考成不及。万历十年张居正卒，次年即被抄家，考成法随之而废。\n");
    }
    let proxied = court.tasks().filter(|t| t.proxied).count();
    if court.in_retreat() || proxied > 0 {
        let now = if court.in_retreat() {
            "，今仍在西苑"
        } else {
            ""
        };
        out.push_str(&format!(
            "御前：怠政 —— 司礼监代批 {proxied} 件{now}。考成法考得了百官，考不了皇帝。\n"
        ));
    }
    out.push_str("称职者可擢，不及者宜黜；然黜陟出自圣裁，考成簿不罢免任何人。");
    out
}

/// 户部「度支奏销」：各级度支与余额，以及按用途、模型、承办官员汇总的用量与花费。
/// `default_model` 用于未记下模型的早期诏令；`price` 查单价（查不到则只列 tokens）。
pub fn duzhi_report(
    court: &Court,
    default_model: Option<&str>,
    price: impl Fn(&str) -> Option<Price>,
) -> String {
    let minister = court.department_head(Department::Revenue).map_or(
        "户部尚书、侍郎俱缺，天子亲览。".to_owned(),
        |(a, o)| format!("{}{}谨奏。", o.label(), a.name),
    );
    let mut out = format!("度支奏销　{minister}\n");
    let total_usage = court.total_usage();
    let used = total_usage.total();
    out.push_str(&match court.budget(BudgetScope::Court) {
        Some(limit) => format!(
            "朝廷度支：{limit} tokens，已用 {used}，余 {}\n",
            limit.saturating_sub(used)
        ),
        None => format!("朝廷度支：未设上限，已用 {used} tokens\n"),
    });
    if total_usage.cache_read > 0 || total_usage.cache_write > 0 {
        let rate = total_usage
            .cache_hit_rate()
            .map_or(String::new(), |r| format!("（命中率 {:.1}%）", r * 100.0));
        let write = match total_usage.cache_write {
            0 => String::new(),
            w => format!("，写入 {w} tokens"),
        };
        out.push_str(&format!(
            "提示词缓存：命中 {} tokens{rate}{write}\n",
            total_usage.cache_read
        ));
    }
    for office in Office::all() {
        if let Some(limit) = court.budget(BudgetScope::Office(office)) {
            out.push_str(&format!("{}每件限 {limit} tokens\n", office.label()));
        }
    }
    for task in court.tasks() {
        if let Some(limit) = task.budget {
            let used = task.usage.total();
            out.push_str(&format!(
                "诏令 #{} 限 {limit} tokens，已用 {used}\n",
                task.edict.id.0
            ));
        }
    }
    // (tokens, 各币种花费)；花费只计查得到单价的部分，不同币种分开汇总。
    type Tally = BTreeMap<String, (u64, BTreeMap<String, f64>)>;
    let (mut by_use, mut by_model, mut by_official) = (Tally::new(), Tally::new(), Tally::new());
    let who = |id: AgentId| {
        court
            .agent(id)
            .map_or("?".to_owned(), |a| format!("#{} {}", a.id.0, a.name))
    };
    // (用途, 用量, 模型, 经手官员)：诏令各项开销，以及不属于任何诏令的科举、殿试。
    let mut items: Vec<(&str, TokenUsage, Option<&str>, String)> = Vec::new();
    for task in court.tasks() {
        let handler = task.handler().map_or("未承办".to_owned(), who);
        for (purpose, usage, model) in spending(task) {
            items.push((purpose, usage, model, handler.clone()));
        }
    }
    for exam in court.exams() {
        items.push(("科举", exam.usage, exam.model.as_deref(), who(exam.agent)));
    }
    for answer in court.palace_exams().iter().flat_map(|p| &p.answers) {
        let model = answer.model.as_deref();
        items.push(("殿试", answer.usage, model, who(answer.speaker)));
    }
    for (purpose, usage, model, official) in items {
        if usage.total() == 0 {
            continue;
        }
        let model = model.or(default_model);
        let cost = model.and_then(&price).map(|p| (p.cost(usage), p.currency));
        let keys = [
            (&mut by_use, purpose.to_owned()),
            (&mut by_model, model.unwrap_or("未记模型").to_owned()),
            (&mut by_official, official),
        ];
        for (tally, key) in keys {
            let entry = tally.entry(key).or_default();
            entry.0 += usage.total();
            if let Some((cost, currency)) = &cost {
                *entry.1.entry(currency.clone()).or_insert(0.0) += cost;
            }
        }
    }
    if by_use.is_empty() {
        out.push_str("尚无开销。\n");
    }
    for (title, tally) in [
        ("按用途", by_use),
        ("按模型", by_model),
        ("按承办官员", by_official),
    ] {
        if tally.is_empty() {
            continue;
        }
        let rows: Vec<_> = tally
            .iter()
            .map(
                |(key, (tokens, costs))| match format_currency_totals(costs) {
                    None => format!("{key} {tokens}"),
                    Some(summary) => format!("{key} {tokens}（{summary}）"),
                },
            )
            .collect();
        out.push_str(&format!("{title}：{}\n", rows.join("；")));
    }
    out.push_str(match court.treasury_remaining() {
        Some(0) => {
            "太仓空虚，户部已停支。嘉靖末年太仓岁入二百万两、岁出逾三百万，户部尚书也只能奏请节用。"
        }
        Some(_) => "量入为出，度支有常。",
        None => {
            "未设度支：仿洪武「量入为出」之训，可用 `ziyuan budget court <tokens>` 定一个总额。"
        }
    });
    out
}

/// 户部度支导出的一条明细行（按用途、模型或承办官员汇总）。
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct DuzhiRow {
    pub dimension: String,
    pub key: String,
    pub tokens: u64,
    pub cost: Option<f64>,
    pub currency: Option<String>,
}

/// 户部度支限额项（朝廷总额、官职单件限额或诏令限额）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct DuzhiBudget {
    pub scope: String,
    pub limit: u64,
    pub used: Option<u64>,
    pub remaining: Option<u64>,
}

/// 户部度支结构化导出数据（供 CSV / JSON 报表共用）。
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct DuzhiExport {
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_hit_rate: Option<f64>,
    pub court_limit: Option<u64>,
    pub court_remaining: Option<u64>,
    pub budgets: Vec<DuzhiBudget>,
    pub rows: Vec<DuzhiRow>,
}

/// 生成户部度支结构化报表：包含总用量、缓存命中、各级限额与按用途／模型／承办官员汇总明细。
pub fn duzhi_export(
    court: &Court,
    default_model: Option<&str>,
    price: impl Fn(&str) -> Option<Price>,
) -> DuzhiExport {
    let total_usage = court.total_usage();
    let used = total_usage.total();
    let court_limit = court.budget(BudgetScope::Court);
    let court_remaining = court_limit.map(|limit| limit.saturating_sub(used));

    let mut budgets = Vec::new();
    if let Some(limit) = court_limit {
        budgets.push(DuzhiBudget {
            scope: "朝廷".into(),
            limit,
            used: Some(used),
            remaining: Some(limit.saturating_sub(used)),
        });
    }
    for office in Office::all() {
        if let Some(limit) = court.budget(BudgetScope::Office(office)) {
            budgets.push(DuzhiBudget {
                scope: format!("{}（每件）", office.label()),
                limit,
                used: None,
                remaining: None,
            });
        }
    }
    for task in court.tasks() {
        if let Some(limit) = task.budget {
            let task_used = task.usage.total();
            budgets.push(DuzhiBudget {
                scope: format!("诏令 #{}", task.edict.id.0),
                limit,
                used: Some(task_used),
                remaining: Some(limit.saturating_sub(task_used)),
            });
        }
    }

    type Tally = BTreeMap<String, (u64, BTreeMap<String, f64>)>;
    let (mut by_use, mut by_model, mut by_official) = (Tally::new(), Tally::new(), Tally::new());
    let who = |id: AgentId| {
        court
            .agent(id)
            .map_or("?".to_owned(), |a| format!("#{} {}", a.id.0, a.name))
    };
    let mut items: Vec<(&str, TokenUsage, Option<&str>, String)> = Vec::new();
    for task in court.tasks() {
        let handler = task.handler().map_or("未承办".to_owned(), who);
        for (purpose, usage, model) in spending(task) {
            items.push((purpose, usage, model, handler.clone()));
        }
    }
    for exam in court.exams() {
        items.push(("科举", exam.usage, exam.model.as_deref(), who(exam.agent)));
    }
    for answer in court.palace_exams().iter().flat_map(|p| &p.answers) {
        let model = answer.model.as_deref();
        items.push(("殿试", answer.usage, model, who(answer.speaker)));
    }
    for (purpose, usage, model, official) in items {
        if usage.total() == 0 {
            continue;
        }
        let model = model.or(default_model);
        let cost = model.and_then(&price).map(|p| (p.cost(usage), p.currency));
        let keys = [
            (&mut by_use, purpose.to_owned()),
            (&mut by_model, model.unwrap_or("未记模型").to_owned()),
            (&mut by_official, official),
        ];
        for (tally, key) in keys {
            let entry = tally.entry(key).or_default();
            entry.0 += usage.total();
            if let Some((c, currency)) = &cost {
                *entry.1.entry(currency.clone()).or_insert(0.0) += c;
            }
        }
    }

    let mut rows = Vec::new();
    for (dimension, tally) in [
        ("用途", by_use),
        ("模型", by_model),
        ("承办官员", by_official),
    ] {
        for (key, (tokens, costs)) in tally {
            if costs.is_empty() {
                rows.push(DuzhiRow {
                    dimension: dimension.into(),
                    key,
                    tokens,
                    cost: None,
                    currency: None,
                });
            } else if costs.len() == 1 {
                let (currency, cost) = costs.into_iter().next().expect("单币种");
                rows.push(DuzhiRow {
                    dimension: dimension.into(),
                    key,
                    tokens,
                    cost: Some((cost * 10_000.0).round() / 10_000.0),
                    currency: Some(currency),
                });
            } else {
                for (currency, cost) in costs {
                    rows.push(DuzhiRow {
                        dimension: dimension.into(),
                        key: format!("{key} [{currency}]"),
                        tokens,
                        cost: Some((cost * 10_000.0).round() / 10_000.0),
                        currency: Some(currency),
                    });
                }
            }
        }
    }

    DuzhiExport {
        total_tokens: used,
        input_tokens: total_usage.input,
        output_tokens: total_usage.output,
        cache_read_tokens: total_usage.cache_read,
        cache_write_tokens: total_usage.cache_write,
        cache_hit_rate: total_usage.cache_hit_rate(),
        court_limit,
        court_remaining,
        budgets,
        rows,
    }
}

fn csv_escape(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_owned()
    }
}

/// 把度支结构化报表编码为 RFC 4180 CSV（`bom` 为 `true` 时加 UTF-8 BOM，便于 Excel 直接打开）。
pub fn duzhi_export_csv(export: &DuzhiExport, bom: bool) -> String {
    let mut out = String::new();
    if bom {
        out.push('\u{feff}');
    }
    out.push_str("维度,名称,tokens,估算花费,币种\n");
    out.push_str(&format!("总计,全部用量,{},,\n", export.total_tokens));
    out.push_str(&format!("总计,输入,{},,\n", export.input_tokens));
    out.push_str(&format!("总计,输出,{},,\n", export.output_tokens));
    if export.cache_read_tokens > 0 || export.cache_write_tokens > 0 {
        out.push_str(&format!("总计,缓存命中,{},,\n", export.cache_read_tokens));
        out.push_str(&format!("总计,缓存写入,{},,\n", export.cache_write_tokens));
    }
    for row in &export.rows {
        let cost = row.cost.map(|c| format!("{c:.4}")).unwrap_or_default();
        let currency = row.currency.as_deref().unwrap_or("");
        out.push_str(&format!(
            "{},{},{},{},{}\n",
            csv_escape(&row.dimension),
            csv_escape(&row.key),
            row.tokens,
            cost,
            csv_escape(currency),
        ));
    }
    out
}

/// 把度支结构化报表序列化为 JSON 值。
pub fn duzhi_export_json(export: &DuzhiExport) -> serde_json::Value {
    serde_json::to_value(export).expect("DuzhiExport 序列化不失败")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ziyuan_core::{CourtEvent, Seal, TaskId};

    #[test]
    fn duzhi_report_renders_prompt_cache_summary_only_when_nonzero() {
        let mut court = Court::replay(&ziyuan_governance::founding_events(1)).unwrap();
        let plain = duzhi_report(&court, None, |_| None);
        assert!(!plain.contains("提示词缓存"), "{plain}");

        let edict = court
            .new_edict(Department::Works, "查缓存".into(), Seal::default())
            .unwrap();
        let worker = court.route(&edict).unwrap().worker;
        for event in [
            CourtEvent::EdictIssued(Box::new(edict)),
            CourtEvent::TaskAssigned {
                task: TaskId(1),
                worker,
                model: Some("claude-sonnet-4".into()),
            },
            CourtEvent::ModelUsage {
                task: TaskId(1),
                usage: TokenUsage::with_cache(1000, 100, 600, 200),
                trace: None,
            },
        ] {
            court.apply(&event).unwrap();
        }
        let report = duzhi_report(&court, None, |_| None);
        assert!(
            report.contains("提示词缓存：命中 600 tokens（命中率 60.0%），写入 200 tokens"),
            "{report}"
        );
    }

    #[test]
    fn duzhi_export_produces_structured_json_and_rfc4180_csv_with_optional_bom() {
        let mut court = Court::replay(&ziyuan_governance::founding_events(1)).unwrap();
        let edict = court
            .new_edict(Department::Works, "查度支".into(), Seal::default())
            .unwrap();
        let worker = court.route(&edict).unwrap().worker;
        for event in [
            CourtEvent::BudgetSet {
                scope: BudgetScope::Court,
                tokens: Some(10_000),
            },
            CourtEvent::EdictIssued(Box::new(edict)),
            CourtEvent::TaskAssigned {
                task: TaskId(1),
                worker,
                model: Some("model,with\"quote".into()),
            },
            CourtEvent::ModelUsage {
                task: TaskId(1),
                usage: TokenUsage::with_cache(1000, 200, 400, 100),
                trace: None,
            },
        ] {
            court.apply(&event).unwrap();
        }
        let price = |m: &str| {
            (m == "model,with\"quote").then(|| Price {
                input_per_million: 10.0,
                output_per_million: 20.0,
                currency: "元".into(),
            })
        };
        let exported = duzhi_export(&court, None, price);
        assert_eq!(exported.total_tokens, 1200);
        assert_eq!(exported.court_limit, Some(10_000));
        assert_eq!(exported.court_remaining, Some(8_800));
        assert!(
            exported
                .rows
                .iter()
                .any(|r| r.dimension == "用途" && r.key == "承办")
        );
        assert!(
            exported
                .rows
                .iter()
                .any(|r| r.dimension == "模型" && r.cost.is_some())
        );

        let csv_plain = duzhi_export_csv(&exported, false);
        assert!(csv_plain.starts_with("维度,名称,tokens,估算花费,币种\n"));
        assert!(
            csv_plain.contains("模型,\"model,with\"\"quote\",1200,0.0140,元\n"),
            "{csv_plain}"
        );
        assert!(csv_plain.contains("总计,缓存命中,400,,\n"));

        let csv_bom = duzhi_export_csv(&exported, true);
        assert!(csv_bom.starts_with('\u{feff}'));
        assert_eq!(&csv_bom['\u{feff}'.len_utf8()..], csv_plain);

        let val = duzhi_export_json(&exported);
        assert_eq!(val["total_tokens"], 1200);
        assert_eq!(val["budgets"][0]["scope"], "朝廷");
    }

    #[test]
    fn estimate_cost_and_duzhi_report_keep_multiple_currencies_separate() {
        let events = [
            ziyuan_core::CourtEvent::Founded(GovernanceMode::DirectRule),
            ziyuan_core::CourtEvent::AgentRegistered(ziyuan_core::Agent {
                id: AgentId(1),
                name: "陈工".into(),
                speciality: Department::Works,
            }),
            ziyuan_core::CourtEvent::OfficialAppointed {
                agent: AgentId(1),
                office: Office::Minister(Department::Works),
            },
            ziyuan_core::CourtEvent::EdictIssued(Box::new(ziyuan_core::Edict {
                id: ziyuan_core::TaskId(1),
                department: Department::Works,
                text: "任务一".into(),
                seal: ziyuan_core::Seal::default(),
            })),
            ziyuan_core::CourtEvent::TaskAssigned {
                task: ziyuan_core::TaskId(1),
                worker: AgentId(1),
                model: Some("deepseek-v4-pro".into()),
            },
            ziyuan_core::CourtEvent::ModelUsage {
                task: ziyuan_core::TaskId(1),
                usage: TokenUsage::new(1_000_000, 0),
                trace: None,
            },
            ziyuan_core::CourtEvent::EdictIssued(Box::new(ziyuan_core::Edict {
                id: ziyuan_core::TaskId(2),
                department: Department::Works,
                text: "任务二".into(),
                seal: ziyuan_core::Seal::default(),
            })),
            ziyuan_core::CourtEvent::TaskAssigned {
                task: ziyuan_core::TaskId(2),
                worker: AgentId(1),
                model: Some("claude-sonnet-4-5".into()),
            },
            ziyuan_core::CourtEvent::ModelUsage {
                task: ziyuan_core::TaskId(2),
                usage: TokenUsage::new(1_000_000, 0),
                trace: None,
            },
        ];
        let court = Court::replay(&events).unwrap();
        let price = |m: &str| match m {
            "deepseek-v4-pro" => Some(Price {
                input_per_million: 9.0,
                output_per_million: 27.0,
                currency: "元".into(),
            }),
            "claude-sonnet-4-5" => Some(Price {
                input_per_million: 3.0,
                output_per_million: 15.0,
                currency: "USD".into(),
            }),
            _ => None,
        };
        let est = estimate_cost(court.tasks(), None, price).unwrap();
        assert_eq!(est, "约 3.0000 USD + 9.0000 元");
        let report = duzhi_report(&court, None, price);
        assert!(report.contains("3.0000 USD + 9.0000 元"), "{report}");
        assert!(
            report.contains("deepseek-v4-pro 1000000（约 9.0000 元）"),
            "{report}"
        );
        assert!(
            report.contains("claude-sonnet-4-5 1000000（约 3.0000 USD）"),
            "{report}"
        );
    }
}
