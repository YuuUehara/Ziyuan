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
pub fn estimate_cost<'a>(
    tasks: impl IntoIterator<Item = &'a Task>,
    default_model: Option<&str>,
    price: impl Fn(&str) -> Option<Price>,
) -> Option<String> {
    let mut total = None::<(f64, String)>;
    for task in tasks {
        // 承办、票拟、拆分、复核与廷议可能用不同模型，分别计价。
        for (_, usage, model) in spending(task) {
            if usage.total() == 0 {
                continue;
            }
            let Some(p) = model.or(default_model).and_then(&price) else {
                continue;
            };
            let entry = total.get_or_insert((0.0, p.currency.clone()));
            entry.0 += p.cost(usage);
        }
    }
    total.map(|(cost, currency)| format!("约 {cost:.4} {currency}"))
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
    let used = court.total_usage().total();
    out.push_str(&match court.budget(BudgetScope::Court) {
        Some(limit) => format!(
            "朝廷度支：{limit} tokens，已用 {used}，余 {}\n",
            limit.saturating_sub(used)
        ),
        None => format!("朝廷度支：未设上限，已用 {used} tokens\n"),
    });
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
    // (tokens, 花费, 币种)；花费只计查得到单价的部分。
    type Tally = BTreeMap<String, (u64, f64, String)>;
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
                entry.1 += cost;
                entry.2.clone_from(currency);
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
                |(key, (tokens, cost, currency))| match currency.is_empty() {
                    true => format!("{key} {tokens}"),
                    false => format!("{key} {tokens}（约 {cost:.4} {currency}）"),
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
