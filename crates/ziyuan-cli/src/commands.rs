//! 执行各子命令：调用 `Session` 的用例并打印结果。

use std::env;
use std::error::Error;
use std::io::{self, IsTerminal, Write};
use std::sync::Arc;
use ziyuan_app::{
    ABOLITION_NOTE, EXECUTOR_BUSY, RunUpdate, Session, WatchOptions, WatchUpdate, demotion_note,
    describe_record, duzhi_report, format_time, history_entries, kaocheng_grade, kaocheng_report,
    render_diffs, seal_note, skipped_note, suspicion_status, tz_offset,
};
use ziyuan_core::{
    AgentId, BudgetScope, GovernanceMode, NO_ART, Persona, Seal, TaskId, missing_sections,
};
use ziyuan_governance::{ChangeState, TaskStatus};
use ziyuan_llm::price_for;
use ziyuan_runtime::{CancelToken, Limits, Outcome, Scheduler, WorkerBackend};
use ziyuan_storage::{EventLog, ExecutorLock};

use crate::args::{Command, GitHubAction};
use crate::backend::{DEFAULT_LIMITS, env_model, model_label, plan_free, worker_backend};
use crate::output::{StreamPrinter, changes_note, cost_note, print_draft, print_report};

pub(crate) fn execute(
    session: &mut Session<EventLog>,
    command: Command,
) -> Result<(), Box<dyn Error>> {
    match command {
        Command::Help
        | Command::Version
        | Command::Init
        | Command::Migrate
        | Command::Login(_)
        | Command::Logout
        | Command::Serve { .. }
        | Command::Tui
        | Command::Completions(_) => {
            unreachable!("在 run 中处理")
        }
        Command::Court => {
            let court = session.court();
            println!("朝廷制度：{} ({})", court.mode().label(), court.mode());
            if court.in_retreat() {
                println!("皇帝：西苑修道中，司礼监代批（`ziyuan return` 回朝）");
            }
            println!(
                "在册官员：{}；有职官员：{}；已完成任务：{}",
                court.agents().count(),
                court.appointed_count(),
                court.completed_count()
            );
            println!("默认承办模型：{}", model_label());
            println!(
                "模型用量：{}{}",
                court.total_usage(),
                cost_note(court.tasks())
            );
            if let (Some(limit), Some(left)) =
                (court.budget(BudgetScope::Court), court.treasury_remaining())
            {
                println!("朝廷度支：{limit} tokens，余 {left}（`ziyuan budget` 查看奏销）");
            }
            println!(
                "调度中枢：{}",
                match court.mode() {
                    GovernanceMode::Chancellor => "中书省（右丞相，左丞相署理）",
                    GovernanceMode::DirectRule => "皇帝直领六部",
                    GovernanceMode::Cabinet => "内阁（首辅，次辅署理）",
                }
            );
            let vacancies = ziyuan_app::vacancies(court);
            if !vacancies.is_empty() {
                println!("朝堂缺位：");
                for note in vacancies {
                    println!("  - {note}");
                }
            }
        }
        Command::Officials => {
            let court = session.court();
            let default = model_label();
            for agent in court.agents() {
                let model = court.model_of(agent.id).unwrap_or(&default);
                let office = match court.office_of(agent.id) {
                    Some(o) => match demotion_note(court, agent.id) {
                        Some(note) => format!("{}（{}，{note}）", o.label(), o.rank(court.mode())),
                        None => format!("{}（{}）", o.label(), o.rank(court.mode())),
                    },
                    None if court.is_purged(agent.id) => "已革职".into(),
                    None => "未任官职".into(),
                };
                let art = court.art_name(agent.id);
                println!(
                    "#{:<3} {:<6} 字{} 号{} {:<18} {} {} 模型 {} (完成 {} 件)",
                    agent.id.0,
                    agent.name,
                    court.courtesy_name(agent.id),
                    if art.is_empty() { NO_ART } else { &art },
                    office,
                    agent.speciality.label(),
                    Persona::of(agent.id).label(),
                    model,
                    court.recorded_completions(agent.id)
                );
            }
        }
        Command::Recruit(name, department) => {
            let id = session.recruit(name, department)?;
            let name = &session.court().agent(id).expect("刚招募").name;
            println!("诏令已颁：招募官员 #{} {}（未授官职）。", id.0, name);
        }
        Command::Rename {
            agent,
            name,
            courtesy,
            art,
        } => println!("{}", session.rename(agent, name, courtesy, art)?),
        Command::Edict {
            department,
            text,
            queue,
            mut seal,
            template,
        } => {
            // 先取得执行者锁再下诏，避免被拒绝的诏令残留在队列中。
            let executor = if queue { None } else { Some(lock_executor()?) };
            let edict = match template {
                None => session.issue(department, text, seal)?,
                Some(choice) => {
                    let defaults = session.court().template(&choice.name).ok_or(format!(
                        "翰林院没有模板「{}」；`ziyuan template` 查看",
                        choice.name
                    ))?;
                    if !choice.tools {
                        seal.capabilities = defaults.seal.capabilities;
                    }
                    if !choice.mcp {
                        seal.mcp = defaults.seal.mcp;
                    }
                    if !choice.scope {
                        seal.scope = defaults.seal.scope;
                    }
                    let department = if choice.department {
                        department
                    } else {
                        defaults.department
                    };
                    // 命令行给出的内容附在模板内容之后。
                    let text = match defaults.text.chars().last() {
                        _ if text.trim().is_empty() => defaults.text,
                        Some(c) if c.is_ascii_alphanumeric() => format!("{} {text}", defaults.text),
                        _ => format!("{}{text}", defaults.text),
                    };
                    session.issue_templated(&choice.name, department, text, seal)?
                }
            };
            let backend = if queue { None } else { Some(worker_backend()?) };
            if let Some(backend) = &backend
                && session.court().mode() == GovernanceMode::Cabinet
            {
                // 皇帝亲自下诏并立即执行：内阁票拟后当即批红。
                session.draft(edict.id, backend.as_ref())?;
                print_draft(session, edict.id);
                if session.court().in_retreat() {
                    println!("陛下修道，司礼监代批红。");
                } else {
                    session.approve(edict.id)?;
                    println!("朱笔批红。");
                }
            }
            if queue {
                println!(
                    "诏令 #{} 已入队：{}{}",
                    edict.id.0,
                    edict.text,
                    seal_note(&edict.seal)
                );
            } else {
                let limits = Limits {
                    max_concurrent: 1,
                    ..DEFAULT_LIMITS
                };
                let backend = backend.expect("立即执行时已构造后端");
                run_tasks(session, Some(&[edict.id]), backend, limits)?;
            }
            drop(executor);
        }
        Command::Run(limits, Some(options)) => {
            let _executor = lock_executor()?;
            hold_court(session, worker_backend()?, limits, &options)?;
        }
        Command::Run(limits, None) => {
            let _executor = lock_executor()?;
            let court = session.court();
            let tasks = court.runnable_tasks();
            let waiting = court.tasks().filter(|t| court.needs_approval(t)).count();
            let blocked: Vec<_> = court
                .tasks()
                .filter(|task| task.status.is_open())
                .filter_map(|task| {
                    court
                        .blocked_by(task)
                        .map(|dependency| (task.edict.id, dependency))
                })
                .map(|(task, dependency)| format!("#{}←#{}", task.0, dependency.0))
                .collect();
            if waiting > 0 && !court.in_retreat() {
                eprintln!(
                    "另有 {waiting} 件诏令待批红（`ziyuan approve <诏令ID|all>`），本次不执行。"
                );
            }
            // 修道期间待批红的诏令由司礼监在执行时代批红。
            if tasks.is_empty() && !(court.in_retreat() && waiting > 0) {
                if blocked.is_empty() {
                    println!("没有未结束的诏令。");
                } else {
                    println!(
                        "没有可执行的诏令：依赖受阻 {} 件（{}）。用 `ziyuan tasks` 查看，并撤销受阻子诏令后再汇总。",
                        blocked.len(),
                        blocked.join("、")
                    );
                }
            } else {
                run_tasks(session, None, worker_backend()?, limits)?;
            }
        }
        Command::Tasks => {
            let court = session.court();
            for task in court.tasks() {
                let name = |id: AgentId| court.agent(id).map_or("?", |a| a.name.as_str());
                let status = match &task.status {
                    TaskStatus::Pending if task.split.is_some() => {
                        match court.open_subtasks(task) {
                            0 => "待尚书汇总".to_owned(),
                            n => format!("分办中（{n} 件未结）"),
                        }
                    }
                    TaskStatus::Pending if court.needs_approval(task) => {
                        let label = if task.draft.is_some() {
                            "票拟"
                        } else {
                            "未票拟，旧例预拟"
                        };
                        match court.route_task(task) {
                            Ok(route) => format!("待批红（{label}：{}）", route.path.join(" → ")),
                            Err(e) => format!("待批红（票拟不成：{e}）"),
                        }
                    }
                    TaskStatus::Pending if court.blocked_by(task).is_some() => {
                        let blocker = court.blocked_by(task).expect("刚查过");
                        format!("受阻（先办 #{} 未成）", blocker.0)
                    }
                    TaskStatus::Pending => "待办".to_owned(),
                    TaskStatus::Assigned(w) => format!("已指派 {}（未结束）", name(*w)),
                    TaskStatus::Completed { worker, .. } => {
                        let review = match &task.review {
                            Some((verdict, _)) if task.proxied => {
                                format!("司礼监代批{}", verdict.label())
                            }
                            Some((verdict, _)) => verdict.label().to_owned(),
                            None => "待评定".to_owned(),
                        };
                        let censure = task
                            .censure
                            .as_ref()
                            .map_or(String::new(), |c| format!("；御史{}", c.verdict.label()));
                        // 礼部：回奏发回重写后仍不合体例的，照收并标注。
                        let style = match &task.status {
                            TaskStatus::Completed { summary, .. }
                                if !missing_sections(summary).is_empty() =>
                            {
                                "；体例不合"
                            }
                            _ => "",
                        };
                        format!(
                            "已完成（{}），{review}{censure}{style}{}",
                            name(*worker),
                            changes_note(task)
                        )
                    }
                    TaskStatus::Failed { reason, .. } => format!("失败：{reason}"),
                    TaskStatus::Cancelled => "已撤销".to_owned(),
                };
                let usage = if task.usage.total() > 0 {
                    format!("（{} tokens{}）", task.usage.total(), cost_note([task]))
                } else {
                    String::new()
                };
                let parent = task
                    .parent
                    .map_or(String::new(), |p| format!("↳ 分办自 #{} ", p.0));
                let flagged = match task.suspicions.len() {
                    0 => String::new(),
                    n => format!("（密报 {n} 条，`ziyuan flags {}`）", task.edict.id.0),
                };
                let debated = match task.debates.len() {
                    0 => String::new(),
                    n => format!("（廷议 {n} 场）"),
                };
                // 兵部：加急与驿传来处标在部门之前。
                let military = format!(
                    "{}{}",
                    if task.urgent {
                        "【八百里加急】"
                    } else {
                        ""
                    },
                    task.courier
                        .as_ref()
                        .map_or(String::new(), |c| format!("【驿传·{c}】"))
                ) + &task
                    .template
                    .as_ref()
                    .map_or(String::new(), |t| format!("【模板·{}】", t.name));
                println!(
                    "#{:<3} {parent}{military}{} {}  —  {}{flagged}{debated}{usage}",
                    task.edict.id.0,
                    task.edict.department.label(),
                    task.edict.text,
                    status
                );
            }
        }
        Command::Diff(task, side, plain) => {
            let entry = session
                .court()
                .task(task)
                .ok_or(format!("未记录下达的诏令 #{}", task.0))?;
            let Some(changes) = &entry.changes else {
                println!("诏令 #{} 没有呈报改动。", task.0);
                return Ok(());
            };
            println!("诏令 #{} 呈报改动{}：", task.0, changes_note(entry));
            // 在终端且找到 git-delta 时经它显示（段头附段号）；管道、--plain 或 delta 出错时印统一 diff。
            let shown = !plain
                && io::stdout().is_terminal()
                && ziyuan_app::delta().is_some_and(|delta| {
                    io::stdout().flush().ok();
                    delta
                        .show(&ziyuan_app::unified_diff(&changes.files), side)
                        .map_err(|e| eprintln!("git-delta 显示失败，改印统一 diff：{e}"))
                        .is_ok()
                });
            if !shown {
                print!("{}", render_diffs(&changes.files).concat());
            }
            if changes.state == ChangeState::Proposed {
                println!(
                    "逐段准奏：`ziyuan apply {} --skip <段号,…>` 驳回所列各段，其余落盘。",
                    task.0
                );
            }
        }
        Command::Apply(task, skipped, commit) => {
            let root = env::current_dir()?;
            let applied = |session: &Session<EventLog>| {
                session
                    .court()
                    .task(task)
                    .and_then(|t| t.changes.as_ref())
                    .is_some_and(|c| c.state == ChangeState::Applied)
            };
            // 已落盘而上次未能提交：`--commit` 只补提交（已有同一尾注的提交则认作已提交）。
            session.reload()?;
            if commit && skipped.is_empty() && applied(session) {
                println!("诏令 #{} 的改动早已落盘，补记提交。", task.0);
            } else {
                session.apply_changes(task, &root, skipped)?;
                let skipped = session
                    .court()
                    .task(task)
                    .and_then(|t| t.changes.as_ref())
                    .map_or(String::new(), |c| skipped_note(&c.skipped));
                println!(
                    "诏令已颁：准奏，诏令 #{} 的改动已写入项目{skipped}。",
                    task.0
                );
            }
            if commit {
                let made = session.commit_applied(task, &root).map_err(|e| {
                    format!(
                        "改动已落盘，但未能提交：{e}（修好后可重跑 `ziyuan apply {} --commit` 补提交）",
                        task.0
                    )
                })?;
                println!(
                    "已记为 {} 提交 {}（只含本诏令落盘的文件）。",
                    made.vcs, made.id
                );
            }
        }
        Command::Discard(task) => {
            session.discard_changes(task)?;
            println!("诏令已颁：驳回诏令 #{} 的改动，未写入。", task.0);
        }
        Command::Cancel(task) => {
            session.cancel(task)?;
            println!("诏令 #{} 已撤销。", task.0);
        }
        Command::Retreat(on) => {
            let note = session.retreat(on)?;
            if on {
                println!("诏令已颁：朕移居西苑修道，朝政付内阁与司礼监。");
            } else {
                println!("诏令已颁：朕回朝亲政。");
            }
            println!("史官记：{note}");
        }
        Command::Reform(mode) => {
            let note = session.reform(mode)?;
            println!(
                "诏令已颁：制度改为{}。旧制度的中书省/内阁官职自动撤销。",
                mode.label()
            );
            println!("史官记：{note}");
            if mode != GovernanceMode::DirectRule {
                println!("提示：新制度如无协调者，请先任命。 ");
            }
        }
        Command::Model(office, model) => {
            session.set_office_model(office, model.clone())?;
            match model {
                Some(m) => println!("诏令已颁：{}改用模型 {m}。", office.label()),
                None => println!("诏令已颁：{}恢复默认模型。", office.label()),
            }
        }
        Command::Ceiling(office, ceiling) => {
            session.set_office_ceiling(office, ceiling.clone())?;
            match ceiling {
                Some(seal) => println!("诏令已颁：{}玉玺上限为 {seal}。", office.label()),
                None => println!("诏令已颁：{}取消玉玺上限。", office.label()),
            }
        }
        Command::Purge(id) => {
            session.purge(id)?;
            println!("诏令已颁：官员 #{} 革职，削去仕籍，永不叙用。", id.0);
        }
        Command::Abolish => {
            println!("{}", session.abolish_chancellery()?);
            println!("诏令已颁：罢中书省，制度改为废相亲政。");
            println!("史官记：{ABOLITION_NOTE}");
        }
        Command::Draft(task) => {
            let tasks = match task {
                Some(task) => vec![task],
                None => {
                    let court = session.court();
                    court
                        .tasks()
                        .filter(|t| court.needs_approval(t) && t.draft.is_none())
                        .map(|t| t.edict.id)
                        .collect()
                }
            };
            if tasks.is_empty() {
                println!("没有待票拟的诏令。");
            } else {
                let backend = worker_backend()?;
                for task in tasks {
                    session.draft(task, backend.as_ref())?;
                    print_draft(session, task);
                }
                println!("请 `ziyuan approve <诏令ID|all>` 批红，或 `ziyuan redraft` 改票。");
            }
        }
        Command::Censor(task) => {
            let tasks = match task {
                Some(task) => vec![task],
                None => session
                    .court()
                    .tasks()
                    .filter(|t| {
                        matches!(t.status, TaskStatus::Completed { .. }) && t.censure.is_none()
                    })
                    .map(|t| t.edict.id)
                    .collect(),
            };
            if tasks.is_empty() {
                println!("没有待复核的诏令。");
            }
            let backend = if tasks.is_empty() {
                None
            } else {
                Some(worker_backend()?)
            };
            for task in tasks {
                let backend = backend.as_deref().expect("有诏令时已构造后端");
                session.censor(task, &env::current_dir()?, backend)?;
                let court = session.court();
                let censure = court
                    .task(task)
                    .and_then(|t| t.censure.as_ref())
                    .expect("刚复核");
                let censor = court.agent(censure.censor).map_or("?", |a| a.name.as_str());
                println!(
                    "诏令 #{} 都察院复核：左都御史{censor}{}（{}）",
                    task.0,
                    censure.verdict.label(),
                    censure.evidence
                );
                for line in censure.note.lines().filter(|l| !l.trim().is_empty()) {
                    println!("  {line}");
                }
            }
        }
        Command::Kaocheng => println!("{}", kaocheng_report(session.court())),
        Command::Post {
            department,
            station,
            urgent,
            text,
        } => {
            let edict = session.post(department, text, Seal::default(), &station, urgent)?;
            let task = session.court().task(edict.id).expect("刚入队");
            let speed = if urgent { "，八百里加急" } else { "" };
            println!(
                "驿传：诏令 #{} 自「{station}」入队（{}{speed}）。",
                edict.id.0,
                department.label()
            );
            if !task.suspicions.is_empty() {
                println!(
                    "锦衣卫密报 {} 条，`ziyuan flags {}` 查看；修道期间司礼监不代批红。",
                    task.suspicions.len(),
                    edict.id.0
                );
            }
            if session.court().needs_approval(task) {
                println!("待批红：`ziyuan approve {}`。", edict.id.0);
            }
        }
        Command::Templates => {
            for (name, t, builtin) in session.court().templates() {
                let origin = if builtin { "内置" } else { "自定" };
                println!(
                    "「{name}」（{origin}，{}，玉玺 {}）",
                    t.department.label(),
                    t.seal
                );
                println!("  {}", t.text);
                for (i, c) in t.criteria.iter().enumerate() {
                    println!("  验收 {}. {c}", i + 1);
                }
            }
            println!(
                "用法：`ziyuan edict --template <名> [补充内容]`；验收标准随诏令附给承办官员，`ziyuan censor` 时都察院逐条核验。"
            );
        }
        Command::SaveTemplate { name, template } => {
            let overrides = session.court().template(&name).is_some();
            session.save_template(&name, template)?;
            let verb = if overrides { "覆盖" } else { "新存" };
            println!("翰林院{verb}模板「{name}」。");
        }
        Command::RemoveTemplate(name) => {
            session.remove_template(&name)?;
            println!("翰林院撤去模板「{name}」。");
        }
        Command::Urgent(task) => {
            session.expedite(task)?;
            println!("诏令 #{} 八百里加急：插队先行，时限加倍。", task.0);
        }
        Command::Exam(agent) => {
            let backend = worker_backend()?;
            let name = session
                .court()
                .agent(agent)
                .map_or("?".to_owned(), |a| a.name.clone());
            println!("礼部开科：#{} {name} 应试。", agent.0);
            let taken = session.exam(
                agent,
                &env::current_dir()?,
                backend.as_ref(),
                |paper, score| {
                    let mark = if score >= 60 { "及格" } else { "不及格" };
                    println!("  「{}」{score} 分（{mark}）", paper.name);
                },
            )?;
            let record = session.court().service_record(agent);
            println!(
                "考毕 {taken} 题；履历：科举及格 {}/{}。成绩供吏部荐举参考，任命仍由皇帝决定。",
                record.exams_passed, record.exams
            );
        }
        Command::Palace(question) => {
            let backend = worker_backend()?;
            println!("殿试，天子亲策：{question}");
            let stopped = session.palace_exam(&question, backend.as_ref(), |agent, answer| {
                println!(
                    "【#{} {}{}·{}】{}",
                    agent.id.0,
                    answer.office.label(),
                    agent.name,
                    Persona::of(agent.id).label(),
                    answer.text
                );
                let _ = io::stdout().flush();
            })?;
            if let Some(reason) = stopped {
                println!("殿试中止：{reason}；已交的答卷照记。");
            }
            println!("请 `ziyuan crown <官员ID>` 钦点状元。");
        }
        Command::Crown(agent) => println!("{}", session.crown(agent)?),
        Command::Flags(only) => {
            let court = session.court();
            let mut any = false;
            for task in court
                .tasks()
                .filter(|t| only.is_none_or(|id| t.edict.id == id))
            {
                for (i, s) in task.suspicions.iter().enumerate() {
                    any = true;
                    let source = s.origin();
                    println!(
                        "诏令 #{} 第 {} 处 {}（{source}）：{}「{}」{}",
                        task.edict.id.0,
                        i + 1,
                        s.bureau(),
                        s.threat.label(),
                        s.excerpt,
                        suspicion_status(s)
                    );
                }
            }
            if !any {
                println!("刑部与锦衣卫无所奏报。");
            } else {
                println!(
                    "（关键词机械检出，可能误报；玉玺不因此改变。可 `ziyuan recheck <诏令ID>` 请刑部复勘，\
                     `ziyuan acquit <诏令ID> [序号]` 平反误报。）"
                );
            }
        }
        Command::GitHub(action) => github(session, action)?,
        Command::Mcp => {
            let servers =
                ziyuan_runtime::mcp::survey(&env::current_dir()?).map_err(|e| e.to_string())?;
            if servers.is_empty() {
                println!(
                    "外藩名册为空：在 {} 登记 MCP 服务，下诏时以 `--mcp <服务>` 授权。",
                    ziyuan_runtime::mcp::CONFIG
                );
            }
            for (name, tools) in servers {
                match tools {
                    Ok(tools) => {
                        println!("外藩「{name}」：{} 件工具", tools.len());
                        for t in tools {
                            let line = t.description.lines().next().unwrap_or_default();
                            println!("  {name}/{}  {line}", t.name);
                            let schema = &t.input_schema;
                            let required: Vec<_> = schema["required"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|r| r.as_str())
                                .collect();
                            for (param, spec) in
                                schema["properties"].as_object().into_iter().flatten()
                            {
                                let kind = spec["type"].as_str().unwrap_or("任意");
                                let must = if required.contains(&param.as_str()) {
                                    "，必填"
                                } else {
                                    ""
                                };
                                let doc = spec["description"].as_str().unwrap_or_default();
                                let doc = doc.lines().next().unwrap_or_default();
                                println!("      {param}（{kind}{must}）{doc}");
                            }
                        }
                    }
                    Err(e) => println!("外藩「{name}」：未能朝贡（{e}）"),
                }
            }
        }
        Command::Changelog { version, since } => {
            let (range, commits) = ziyuan_app::changelog_commits(&env::current_dir()?, since)?;
            let edict = session.draft_changelog(&version, &commits)?;
            println!(
                "诏令 #{} 已入队：礼部修 CHANGELOG「{version}」，收提交 {} 条（{range}）。玉玺只授 read_file、write_file，范围只限 {}。",
                edict.id.0,
                commits.len(),
                ziyuan_core::CHANGELOG
            );
            if session
                .court()
                .needs_approval(session.court().task(edict.id).expect("刚入队"))
            {
                println!("待批红：`ziyuan approve {}`。", edict.id.0);
            }
            println!(
                "执行 `ziyuan run` 后，`ziyuan diff {0}` 审阅，`ziyuan apply {0}` 准奏落盘。",
                edict.id.0
            );
            println!("{}", ziyuan_app::CHANGELOG_NOTE);
        }
        Command::Recheck(only) => {
            let tasks: Vec<TaskId> = match only {
                Some(task) => vec![task],
                None => session
                    .court()
                    .tasks()
                    .filter(|t| {
                        t.suspicions
                            .iter()
                            .any(|s| s.review.is_none() && !s.acquitted)
                    })
                    .map(|t| t.edict.id)
                    .collect(),
            };
            if tasks.is_empty() {
                println!("没有待复勘的检出。");
            } else {
                let backend = worker_backend()?;
                for task in tasks {
                    session.recheck(task, backend.as_ref())?;
                    let court = session.court();
                    let entry = court.task(task).expect("刚复勘");
                    for (i, s) in entry.suspicions.iter().enumerate() {
                        let Some(r) = &s.review else { continue };
                        let name = court.agent(r.reviewer).map_or("?", |a| a.name.as_str());
                        println!(
                            "诏令 #{} 第 {} 处 {}「{}」：刑部{name}复勘为{}——{}",
                            task.0,
                            i + 1,
                            s.threat.label(),
                            s.excerpt,
                            if r.upheld { "维持" } else { "误报" },
                            r.note
                        );
                    }
                }
                println!(
                    "复勘只是意见：平反请 `ziyuan acquit <诏令ID>`（平反刑部判为误报的各处）。"
                );
            }
        }
        Command::Acquit(task, indices) => {
            let indices = match indices {
                Some(list) if list.is_empty() => {
                    let entry = session
                        .court()
                        .task(task)
                        .ok_or(format!("诏令 #{} 不存在", task.0))?;
                    Some(
                        (0..entry.suspicions.len() as u32)
                            .filter(|&i| !entry.suspicions[i as usize].acquitted)
                            .collect(),
                    )
                }
                other => other,
            };
            let (done, note) = session.acquit(task, indices)?;
            let list: Vec<String> = done.iter().map(|i| (i + 1).to_string()).collect();
            println!(
                "诏令已颁：平反诏令 #{} 第 {} 处检出，不再妨碍司礼监代批。",
                task.0,
                list.join("、")
            );
            println!("{note}");
        }
        Command::Budget(None) => {
            let model = env_model();
            let default = model.as_ref().map(|m| m.model().to_owned());
            let price = |m: &str| plan_free(model.as_ref()).then(|| price_for(m)).flatten();
            println!(
                "{}",
                duzhi_report(session.court(), default.as_deref(), price)
            );
        }
        Command::Budget(Some((scope, tokens))) => {
            session.set_budget(scope, tokens)?;
            match tokens {
                Some(t) => println!("诏令已颁：户部度支 · {}：限 {t} tokens。", scope.label()),
                None => println!("诏令已颁：户部度支 · {}：取消限额。", scope.label()),
            }
        }
        Command::Debate(task, members, rounds) => {
            let backend = worker_backend()?;
            println!("诏令 #{} 廷议：", task.0);
            let stopped =
                session.debate(task, &members, rounds, backend.as_ref(), |agent, speech| {
                    println!(
                        "【{}{}·{}】{}",
                        speech.office.label(),
                        agent.name,
                        Persona::of(agent.id).label(),
                        speech.text
                    );
                    let _ = io::stdout().flush();
                })?;
            if let Some(reason) = stopped {
                println!("廷议中止：{reason}；已有发言照记。");
            }
            println!("纪要已记入事件簿，票拟与承办时附上，供参考；派工与玉玺不变。");
        }
        Command::Split(task, plan, file) => {
            let subtasks = if let Some(file) = file {
                let json = std::fs::read_to_string(&file)
                    .map_err(|e| format!("无法读取排程方案 {file}：{e}"))?;
                let schedule: Vec<ziyuan_app::ScheduledSubtask> = serde_json::from_str(&json)
                    .map_err(|e| format!("无效的排程方案 {file}：{e}"))?;
                session.split_scheduled(task, schedule)?
            } else if plan.is_empty() {
                session.plan_split(task, worker_backend()?.as_ref())?
            } else {
                session.split(task, plan)?
            };
            let court = session.court();
            let note = court
                .task(task)
                .and_then(|t| t.split.as_ref())
                .map_or("", |s| s.note.as_str());
            println!("诏令 #{} 分办（{note}）：", task.0);
            let mut chained = false;
            for id in subtasks {
                let sub = court.task(id).expect("刚分办");
                let worker = sub.assignee.and_then(|a| court.agent(a));
                let name = worker.map_or("?", |a| a.name.as_str());
                let deps = sub
                    .dag
                    .as_ref()
                    .map(|d| d.dependencies.as_slice())
                    .unwrap_or_default();
                let order = if deps.is_empty() {
                    String::new()
                } else {
                    chained = true;
                    format!(
                        "（先办 {}）",
                        deps.iter()
                            .map(|d| format!("#{}", d.0))
                            .collect::<Vec<_>>()
                            .join("、")
                    )
                };
                println!("  #{} → {name}：{}{order}", id.0, sub.edict.text);
            }
            if chained {
                println!("写子诏令查阅范围重叠，已按上述次序自动串行；上游失败则下游受阻。");
            }
            println!("`ziyuan run` 执行；子诏令全部结束后由尚书汇总回奏。");
        }
        Command::Redraft(task, worker, note) => {
            session.redraft(task, worker, note)?;
            print_draft(session, task);
        }
        Command::Approve(task) => {
            let tasks = match task {
                Some(task) => vec![task],
                None => {
                    let court = session.court();
                    court
                        .tasks()
                        .filter(|t| court.needs_approval(t))
                        .map(|t| t.edict.id)
                        .collect()
                }
            };
            if tasks.is_empty() {
                println!("没有待批红的诏令。");
            }
            for task in tasks {
                session.approve(task)?;
                println!("诏令 #{} 已批红。", task.0);
            }
        }
        Command::Dismiss(id) => {
            session.dismiss(id)?;
            println!("诏令已颁：官员 #{} 已罢免现职（身份与履历保留）。", id.0);
        }
        Command::Appoint(id, office) => {
            let note = session.appoint(id, office)?;
            println!("诏令已颁：任命官员 #{} 为{}。", id.0, office.label());
            if let Some(note) = note {
                println!("{note}");
            }
        }
        Command::Nominate { office, lots: true } => {
            let (agent, note) = session.draw_lots(office)?;
            println!("诏令已颁：枚卜得官员 #{}，任{}。", agent.0, office.label());
            println!("{note}");
        }
        Command::Nominate {
            office,
            lots: false,
        } => {
            let backend = worker_backend()?;
            println!("吏部会同九卿，廷推{}：", office.label());
            let mut lines = Vec::new();
            let votes = session.nominate(office, backend.as_ref(), |voter, vote| {
                lines.push((voter.name.clone(), vote.candidate, vote.note.clone()));
            })?;
            let names = |id: AgentId| {
                session
                    .court()
                    .agent(id)
                    .map_or("?".to_owned(), |a| format!("#{} {}", id.0, a.name))
            };
            for (voter, candidate, note) in lines {
                let note = if note.is_empty() {
                    String::new()
                } else {
                    format!("——{note}")
                };
                println!("  {voter} 推 {}{note}", names(candidate));
            }
            let tally: Vec<String> = session
                .court()
                .nomination_tally(office)
                .into_iter()
                .map(|(id, n)| format!("{} {n} 票", names(id)))
                .collect();
            println!("得票：{}（共 {} 票）", tally.join("、"), votes.len());
            println!(
                "廷推只是建议：`ziyuan appoint <官员ID> {}` 任命（被推者现任他职的径直擢升），或 `ziyuan nominate {} --lots` 枚卜。",
                office.code(),
                office.code()
            );
        }
        Command::Cane(id) => println!("{}", session.cane(id)?),
        Command::Pardon(id) => println!("{}", session.pardon(id)?),
        Command::Examine(id) => {
            let court = session.court();
            let agent = court.agent(id).ok_or("官员不存在")?;
            let r = court.service_record(id);
            println!("吏部考绩：{}（#{}）", agent.name, id.0);
            println!("  {}；考成：{}", describe_record(&r), kaocheng_grade(&r));
            println!(
                "  工具调用 {} 次，其中失败 {} 次（含越出玉玺的尝试）",
                r.tool_calls, r.tool_failures
            );
            let handled = court.tasks().filter(|t| t.handler() == Some(id));
            println!("  模型用量：{}{}", r.usage, cost_note(handled));
            if let Some(office) = court.office_of(id) {
                println!(
                    "  承办模型：{}",
                    court.office_model(office).unwrap_or(&model_label())
                );
                if let Some(ceiling) = court.office_ceiling(office) {
                    println!("  {}玉玺上限：{ceiling}", office.label());
                }
            }
            if r.unreviewed > 0 {
                println!(
                    "  另有 {} 件回奏未经评定，不计入考绩。可用 `ziyuan review <诏令ID> accept|reject` 评定。",
                    r.unreviewed
                );
            }
        }
        Command::Review(task, verdict, note) => {
            session.review(task, verdict, note)?;
            println!("诏令 #{} 评定为{}。", task.0, verdict.label());
        }
        Command::Recommend(office) => {
            let court = session.court();
            let candidates = court.recommend(office)?;
            if candidates.is_empty() {
                println!("无人可任{}。可先 `ziyuan recruit`。", office.label());
                return Ok(());
            }
            println!(
                "吏部荐举{}（按 采纳−驳回、失败、工具失败 排序）：",
                office.label()
            );
            for c in &candidates {
                let current = c.current.map_or("未任官职".into(), |o| o.label());
                println!(
                    "#{:<3} {:<8} {:<12} {}",
                    c.agent.id.0,
                    c.agent.name,
                    current,
                    describe_record(&c.record)
                );
            }
            println!("仅为建议：任命须由皇帝 `ziyuan appoint`（如有现职须先 dismiss）。");
        }
        Command::Records(query) => {
            let records = session.court().records();
            let found = ziyuan_core::find_records(&records, &query);
            if found.is_empty() {
                println!(
                    "{}",
                    if records.is_empty() {
                        "翰林院尚无实录：诏令完成后自动归档。"
                    } else {
                        "实录中无相关前例。"
                    }
                );
            }
            for r in found {
                println!(
                    "#{} {} · #{} {} · {}",
                    r.task.0,
                    r.department.label(),
                    r.worker.0,
                    r.handler,
                    r.verdict
                );
                println!("    诏令：{}", r.edict);
                let summary = r
                    .summary
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("");
                let short: String = summary.trim().chars().take(80).collect();
                let more = if short.len() < summary.trim().len() {
                    "…"
                } else {
                    ""
                };
                println!("    回奏：{short}{more}");
            }
        }
        Command::History(filter) => {
            let offset = tz_offset();
            for entry in history_entries(session.history()?, &filter)? {
                let at = entry.at.map_or("—".repeat(16), |t| format_time(t, offset));
                println!("{:>3}: {at}  {}", entry.seq, entry.text);
            }
        }
    }
    Ok(())
}

/// 兵部驿站 · GitHub。凭据只在 `ziyuan_github` 中使用，不经朝廷与模型。
pub(crate) fn github(
    session: &mut Session<EventLog>,
    action: GitHubAction,
) -> Result<(), Box<dyn Error>> {
    let hub = ziyuan_github::GitHub::from_env()?
        .ok_or("未设 ZIYUAN_GITHUB_REPO（owner/repo）与 ZIYUAN_GITHUB_TOKEN，驿站不通 GitHub")?;
    match action {
        GitHubAction::Pull {
            label,
            department,
            seal,
        } => {
            let report = session.github_pull(&hub, &label, department, &seal)?;
            for (edict, station, flags) in &report.queued {
                let note = if *flags > 0 {
                    format!("，锦衣卫密报 {flags} 条")
                } else {
                    String::new()
                };
                println!("驿传：诏令 #{} 自「{station}」入队{note}。", edict.id.0);
            }
            println!(
                "GitHub {}：标签「{label}」的 issue/PR {} 件，新入队 {} 件（只入队，执行请 `ziyuan run`）。",
                hub.repo(),
                report.issues,
                report.queued.len()
            );
        }
        GitHubAction::Reply(only) => {
            let tasks = match only {
                Some(task) => vec![task],
                None => session.github_replyable(&hub)?,
            };
            if tasks.is_empty() {
                println!(
                    "没有可回报的诏令（须经 GitHub 驿传入队、已评定、无未平反的密报、尚未回报）。"
                );
            }
            for task in tasks {
                let (number, url) = session.github_reply(&hub, task)?;
                println!("诏令 #{} 回奏已送回 #{number}：{url}", task.0);
            }
        }
        GitHubAction::PullRequest(task) => {
            let url = session.github_pull_request(&hub, task)?;
            println!("诏令 #{} 的改动已开为草稿 PR：{url}", task.0);
        }
    }
    Ok(())
}

/// 同一朝廷同一时刻只有一个进程执行诏令；入队、撤销、任免等不受限制。
pub(crate) fn lock_executor() -> Result<ExecutorLock, Box<dyn Error>> {
    EventLog::new(&env::current_dir()?)
        .try_lock_executor()?
        .ok_or_else(|| {
            format!("{EXECUTOR_BUSY}。可用 `edict --queue` 入队：常朝（`run --watch`）中随到随办，否则待其结束后 `ziyuan run`。").into()
        })
}

/// 在 Tokio 上执行 `tasks` 并逐件打印结局；有未完成的诏令时返回错误。
/// 执行 `tasks`；`None` 则反复执行全部可执行的诏令直到队列清空（分办的汇总随后接上）。
pub(crate) fn run_tasks(
    session: &mut Session<EventLog>,
    tasks: Option<&[TaskId]>,
    backend: Arc<dyn WorkerBackend>,
    limits: Limits,
) -> Result<(), Box<dyn Error>> {
    let scheduler = Scheduler::new(backend, limits);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let mut unfinished = 0;
    let mut finished = Vec::new();
    let result = runtime.block_on(async {
        let cancel = CancelToken::new();
        let on_interrupt = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("收到中断：撤销执行中的诏令，排队中的保留。");
                on_interrupt.cancel();
            }
        });
        let mut stream = StreamPrinter::new(io::stderr());
        let on_update = |update: RunUpdate<'_>| match update {
            RunUpdate::Assigned(_) => {}
            RunUpdate::Text { task, text } => stream.text(task, text),
            RunUpdate::Finished(report) => {
                stream.finish(report.edict.id);
                if !matches!(report.outcome, Outcome::Completed(_)) {
                    unfinished += 1;
                }
                finished.push(report.edict.id);
                print_report(report);
            }
        };
        match tasks {
            Some(tasks) => session.run(tasks, &scheduler, &cancel, on_update).await,
            None => session.run_open(&scheduler, &cancel, on_update).await,
        }
    });
    for &task in &finished {
        let entry = session.court().task(task);
        if let Some(entry) = entry.filter(|t| t.changes.is_some()) {
            println!(
                "诏令 #{} 呈报改动{}：`ziyuan diff {0}` 查看，`ziyuan apply {0}` 准奏落盘，`ziyuan discard {0}` 驳回。",
                task.0,
                changes_note(entry)
            );
        }
    }
    // 超时或撤销后，阻塞中的模型请求可能仍未返回；不等待它们。
    runtime.shutdown_background();
    result?;
    if unfinished > 0 {
        return Err(format!("{unfinished} 件诏令未完成").into());
    }
    Ok(())
}

/// 常朝：常驻执行，直到 Ctrl-C 退朝（执行中的诏令撤销，排队的保留）。
pub(crate) fn hold_court(
    session: &mut Session<EventLog>,
    backend: Arc<dyn WorkerBackend>,
    limits: Limits,
    options: &WatchOptions,
) -> Result<(), Box<dyn Error>> {
    let scheduler = Scheduler::new(backend.clone(), limits);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let censor = if options.censor.is_some() {
        "办完自动请都察院复核"
    } else {
        "不自动复核"
    };
    println!(
        "常朝开始：每 {} 秒查看一次队列，{censor}；采纳与落盘仍须亲裁。Ctrl-C 退朝。",
        options.poll.as_secs()
    );
    let result = runtime.block_on(async {
        let cancel = CancelToken::new();
        let on_interrupt = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("退朝：撤销执行中的诏令，排队中的保留。");
                on_interrupt.cancel();
            }
        });
        let mut stream = StreamPrinter::new(io::stderr());
        let on_update = |update: WatchUpdate<'_>| match update {
            WatchUpdate::Run(RunUpdate::Assigned(_)) => {}
            WatchUpdate::Run(RunUpdate::Text { task, text }) => stream.text(task, text),
            WatchUpdate::Run(RunUpdate::Finished(report)) => {
                stream.finish(report.edict.id);
                print_report(report);
            }
            WatchUpdate::Censured { task, verdict } => {
                println!("诏令 #{} 都察院自动复核：{}", task.0, verdict.label());
            }
            WatchUpdate::Notice(note) => eprintln!("{note}"),
            WatchUpdate::Neglect(note) => println!("史官记：{note}"),
        };
        session
            .watch(&scheduler, backend.as_ref(), &cancel, options, on_update)
            .await
    });
    // 撤销后，阻塞中的模型请求可能仍未返回；不等待它们。
    runtime.shutdown_background();
    result?;
    Ok(())
}
