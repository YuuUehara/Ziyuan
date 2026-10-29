//! 绘制：页头、诏令、官员、起居注、页脚与弹窗。

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Tabs, Wrap,
};
use ziyuan_app::{
    HistoryEntry, HistoryFilter, describe_record, estimate_cost, format_time, history_entries,
    preview_changes, seal_note, skipped_note, suspicion_status, tz_offset,
};
use ziyuan_core::{
    AgentId, BudgetScope, CensorVerdict, Department, Office, Persona, Seal, Speech, Verdict,
    missing_sections,
};
use ziyuan_governance::{ChangeState, Court, Task, TaskStatus};

use crate::App;
use crate::app::{ChooseKind, ConfirmKind, InputKind, Modal, TABS, Tab};

/// 起居注页只列最近这么多条：事件簿有几十万条时，逐条描述会让每一帧都卡住。
pub(crate) const HISTORY_LIMIT: usize = 1000;

pub(crate) const VERMILION: Color = Color::Rgb(178, 34, 34);
pub(crate) const GOLD: Color = Color::Rgb(218, 165, 32);

/// 改动预览的语法配色（深色终端）。
fn token_color(token: Option<&str>) -> Option<Color> {
    Some(match token? {
        "keyword" => Color::Rgb(230, 110, 95),
        "string" => Color::Rgb(120, 190, 180),
        "comment" => Color::Rgb(140, 130, 110),
        "number" | "constant" => Color::Rgb(190, 150, 220),
        "function" => Color::Rgb(220, 180, 100),
        "type" | "macro" => Color::Rgb(120, 160, 220),
        _ => return None,
    })
}

/// 廷议发言的一行：「官职姓名（人格）：内容」。
pub(crate) fn speech_line(name: &str, speaker: AgentId, speech: &Speech) -> String {
    let persona = Persona::of(speaker).label();
    format!(
        "{}{name}（{persona}）：{}",
        speech.office.label(),
        speech.text
    )
}

pub(crate) fn panel(title: String) -> Block<'static> {
    Block::bordered()
        .border_style(Style::new().fg(VERMILION))
        .title(Span::styled(
            format!(" {title} "),
            Style::new().fg(GOLD).bold(),
        ))
}

pub(crate) fn modal_block(title: String) -> Block<'static> {
    panel(title).border_type(ratatui::widgets::BorderType::Double)
}

pub(crate) fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

pub(crate) fn status_badge(court: &Court, task: &Task) -> (String, Style) {
    if court.needs_approval(task) {
        return (
            "待批红".into(),
            Style::new().fg(Color::White).bg(VERMILION).bold(),
        );
    }
    match &task.status {
        TaskStatus::Pending if task.split.is_some() => match court.open_subtasks(task) {
            0 => ("待汇总".into(), Style::new().fg(Color::Yellow)),
            _ => ("分办中".into(), Style::new().fg(Color::Yellow)),
        },
        TaskStatus::Pending => ("待办".into(), Style::new().fg(Color::Gray)),
        TaskStatus::Assigned(_) => ("执行中".into(), Style::new().fg(Color::Yellow).bold()),
        TaskStatus::Completed { .. } => match &task.review {
            Some((Verdict::Accepted, _)) => ("已采纳".into(), Style::new().fg(Color::Green)),
            Some((Verdict::Rejected, _)) => ("已驳回".into(), Style::new().fg(Color::Magenta)),
            None => ("待评定".into(), Style::new().fg(Color::Cyan).bold()),
        },
        TaskStatus::Failed { .. } => ("失败".into(), Style::new().fg(Color::Red)),
        TaskStatus::Cancelled => ("已撤销".into(), Style::new().fg(Color::DarkGray)),
    }
}

impl App {
    pub fn draw(&self, frame: &mut Frame) {
        let [header, tabs, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .areas(frame.area());
        self.draw_header(frame, header);
        let titles = TABS
            .iter()
            .enumerate()
            .map(|(i, (_, name))| format!(" {} {name} ", i + 1));
        let selected = TABS.iter().position(|(t, _)| *t == self.tab);
        frame.render_widget(
            Tabs::new(titles)
                .select(selected)
                .highlight_style(Style::new().fg(GOLD).bold().reversed())
                .divider(""),
            tabs,
        );
        match self.tab {
            Tab::Edicts => self.draw_edicts(frame, body),
            Tab::Officials => self.draw_officials(frame, body),
            Tab::History => self.draw_history(frame, body),
        }
        self.draw_footer(frame, footer);
        if let Some(modal) = &self.modal {
            self.draw_modal(frame, modal);
        }
        narrow_ambiguous(frame.buffer_mut());
    }

    /// 按各诏令实际所用模型估算的花费；查不到单价时为空。
    pub(crate) fn cost_note<'a>(&self, tasks: impl IntoIterator<Item = &'a Task>) -> String {
        let backend = &self.config.backend;
        estimate_cost(tasks, self.model.as_deref(), |m| backend.price(m))
            .map_or(String::new(), |c| format!("（{c}）"))
    }

    pub(crate) fn model_label(&self) -> &str {
        self.model.as_deref().unwrap_or("演示")
    }

    pub(crate) fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let court = self.court();
        let usage = court.total_usage();
        let mut spans = vec![
            Span::styled(
                " 紫垣 · 紫禁城 ",
                Style::new().fg(Color::White).bg(VERMILION).bold(),
            ),
            Span::raw("  制度 "),
            Span::styled(court.mode().label(), Style::new().fg(GOLD).bold()),
            Span::styled(
                if court.in_retreat() {
                    " · 西苑修道中"
                } else {
                    ""
                },
                Style::new().fg(Color::Magenta).bold(),
            ),
            Span::raw(format!(
                "  在册 {} · 有职 {}  模型 {}  用量 输入 {} / 输出 {}{}",
                court.agents().count(),
                court.appointed_count(),
                self.model_label(),
                usage.input,
                usage.output,
                self.cost_note(court.tasks()),
            )),
        ];
        if let (Some(limit), Some(left)) =
            (court.budget(BudgetScope::Court), court.treasury_remaining())
        {
            let style = match left {
                0 => Style::new().fg(Color::Red).bold(),
                _ => Style::new(),
            };
            spans.push(Span::styled(format!("  度支余 {left}/{limit}"), style));
        }
        if self.run.is_some() {
            spans.push(Span::styled(
                "  ● 执行中",
                Style::new().fg(Color::Yellow).bold(),
            ));
        }
        frame.render_widget(Line::from(spans), area);
    }

    pub(crate) fn draw_edicts(&self, frame: &mut Frame, area: Rect) {
        let [list_area, detail_area] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(area);
        let order = self.task_order();
        let items: Vec<ListItem> = order
            .iter()
            .map(|task| {
                let (label, style) = status_badge(self.court(), task);
                let indent = if task.parent.is_some() { "↳" } else { "" };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("#{:<3}{indent}", task.edict.id.0),
                        Style::new().dim(),
                    ),
                    Span::styled(format!("{:<6}", label), style),
                    Span::styled(
                        if task.urgent { " 急" } else { "" },
                        Style::new().fg(Color::LightRed).bold(),
                    ),
                    Span::styled(
                        if task.courier.is_some() { " 驿" } else { "" },
                        Style::new().fg(Color::Cyan),
                    ),
                    Span::raw(format!(
                        " {} {}",
                        task.edict.department.label(),
                        task.edict.text
                    )),
                ]))
            })
            .collect();
        let mut state = ListState::default().with_selected(
            order
                .iter()
                .position(|t| Some(t.edict.id) == self.selected_task),
        );
        let empty = items.is_empty();
        frame.render_stateful_widget(
            List::new(items)
                .block(panel(format!("诏令（{}）", order.len())))
                .highlight_style(Style::new().bg(Color::DarkGray).bold())
                .highlight_symbol("▌"),
            list_area,
            &mut state,
        );
        if empty {
            let hint = Paragraph::new("尚无诏令。按 n 下诏。").dim();
            frame.render_widget(hint, list_area.inner(ratatui::layout::Margin::new(2, 1)));
        }

        let selected = self.selected_task.and_then(|id| self.court().task(id));
        let Some(task) = selected else {
            frame.render_widget(Paragraph::new("").block(panel("详情".into())), detail_area);
            return;
        };
        let (text, following) = self.task_detail(task);
        let paragraph = Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(panel(format!(
                "诏令 #{} · {}",
                task.edict.id.0,
                task.edict.department.label()
            )));
        let inner_height = detail_area.height.saturating_sub(2) as usize;
        let lines = paragraph.line_count(detail_area.width.saturating_sub(2));
        let max_scroll = lines.saturating_sub(inner_height) as u16;
        let scroll = if following {
            max_scroll
        } else {
            self.detail_scroll.min(max_scroll)
        };
        frame.render_widget(paragraph.scroll((scroll, 0)), detail_area);
    }

    /// 诏令详情，以及是否应跟随末尾（正在流式输出）。
    pub(crate) fn task_detail(&self, task: &Task) -> (Text<'static>, bool) {
        let court = self.court();
        let name = |id: AgentId| court.agent(id).map_or("?".to_owned(), |a| a.name.clone());
        let label = |s: &str| Span::styled(format!("{s}　"), Style::new().fg(GOLD));
        let (status, style) = status_badge(court, task);
        let mut lines = vec![Line::from(task.edict.text.clone()).bold(), Line::default()];
        let seal = seal_note(&task.edict.seal);
        lines.push(Line::from(vec![
            label("玉玺"),
            Span::raw(if seal.is_empty() {
                "默认（全部只读工具，整个项目）".into()
            } else {
                seal
            }),
        ]));
        let worker = match &task.status {
            TaskStatus::Assigned(w) | TaskStatus::Completed { worker: w, .. } => Some(*w),
            TaskStatus::Failed { worker, .. } => *worker,
            _ => None,
        };
        let mut status_line = vec![label("状态"), Span::styled(status, style)];
        if let Some(w) = worker {
            status_line.push(Span::raw(format!("  承办 {}", name(w))));
        }
        lines.push(Line::from(status_line));
        if let Some(used) = &task.template {
            lines.push(Line::from(vec![
                label("翰林院"),
                Span::raw(format!("模板「{}」，都察院复核时逐条核验：", used.name)),
            ]));
            for (i, c) in used.criteria.iter().enumerate() {
                lines.push(Line::from(format!("  {}. {c}", i + 1)).dim());
            }
        }
        if task.urgent || task.courier.is_some() {
            let mut military = vec![label("兵部")];
            if task.urgent {
                military.push(Span::styled(
                    "八百里加急（插队先行，时限加倍）",
                    Style::new().fg(Color::LightRed).bold(),
                ));
            }
            if let Some(station) = &task.courier {
                military.push(Span::raw(format!("  驿传自「{station}」")));
            }
            lines.push(Line::from(military));
        }
        if let Some(parent) = task.parent {
            lines.push(Line::from(vec![
                label("分办"),
                Span::raw(format!("自诏令 #{}", parent.0)),
            ]));
        }
        if let Some(split) = &task.split {
            lines.push(Line::from(vec![
                label("分办"),
                Span::raw(split.note.clone()),
            ]));
            for id in &split.subtasks {
                let Some(sub) = court.task(*id) else {
                    continue;
                };
                let (badge, style) = status_badge(court, sub);
                let who = sub.handler().or(sub.assignee).map_or("?".to_owned(), name);
                lines.push(Line::from(vec![
                    Span::raw(format!("  #{} ", id.0)),
                    Span::styled(badge, style),
                    Span::raw(format!(" {who}：{}", sub.edict.text)),
                ]));
            }
        }
        if task.usage.total() > 0 || task.tool_calls > 0 {
            lines.push(Line::from(vec![
                label("用量"),
                Span::raw(format!(
                    "{}{}；工具调用 {} 次（失败 {}）",
                    task.usage,
                    self.cost_note([task]),
                    task.tool_calls,
                    task.tool_failures
                )),
            ]));
        }
        if let Some((verdict, note)) = &task.review {
            let note = if note.is_empty() {
                String::new()
            } else {
                format!("：{note}")
            };
            lines.push(Line::from(vec![
                label("评定"),
                Span::raw(format!(
                    "{}{}{note}",
                    if task.proxied { "司礼监代批" } else { "" },
                    verdict.label()
                )),
            ]));
        }
        let speeches: Vec<String> = match self.debating.get(&task.edict.id) {
            Some(live) => live.clone(),
            None => task.debates.last().map_or_else(Vec::new, |last| {
                last.iter()
                    .map(|sp| speech_line(&name(sp.speaker), sp.speaker, sp))
                    .collect()
            }),
        };
        if self.debating.contains_key(&task.edict.id) || !speeches.is_empty() {
            let title = if self.debating.contains_key(&task.edict.id) {
                "廷议进行中…".to_owned()
            } else {
                format!(
                    "廷议纪要（第 {} 场，供参考，不改派工与玉玺）",
                    task.debates.len()
                )
            };
            lines.push(Line::default());
            lines.push(Line::styled(title, Style::new().fg(Color::Magenta).bold()));
            lines.extend(speeches.into_iter().map(Line::from));
        }
        if let TaskStatus::Completed { summary, .. } = &task.status {
            let missing = missing_sections(summary);
            if !missing.is_empty() {
                lines.push(Line::default());
                lines.push(Line::styled(
                    format!(
                        "礼部：回奏体例不合，缺「{}」（发回重写后照收）",
                        missing.join("」「")
                    ),
                    Style::new().fg(Color::Yellow).bold(),
                ));
            }
        }
        if !task.suspicions.is_empty() {
            lines.push(Line::default());
            lines.push(Line::styled(
                "刑部与锦衣卫密报（关键词检出，可能误报；`ziyuan recheck` 复勘、`acquit` 平反）",
                Style::new().fg(Color::Red).bold(),
            ));
            for s in &task.suspicions {
                let source = s.origin();
                lines.push(Line::from(format!(
                    "  {}（{source}）{}：「{}」{}",
                    s.bureau(),
                    s.threat.label(),
                    s.excerpt,
                    suspicion_status(s)
                )));
            }
        }
        if let Some(c) = &task.censure {
            let censor = self
                .court()
                .agent(c.censor)
                .map_or("?", |a| a.name.as_str());
            let color = match c.verdict {
                CensorVerdict::Endorsed => Color::Cyan,
                CensorVerdict::Impeached => Color::Red,
            };
            lines.push(Line::default());
            lines.push(Line::from(vec![
                Span::styled(
                    format!("都察院{}　", c.verdict.label()),
                    Style::new().fg(color).bold(),
                ),
                Span::raw(format!("左都御史{censor}；证据核验：{}", c.evidence)),
            ]));
            lines.extend(c.note.lines().map(|l| Line::from(l.to_owned())));
        }
        if let Some(changes) = &task.changes {
            let state = match changes.state {
                ChangeState::Proposed => "待准奏（w 全部落盘，m 落盘并提交，W 逐段驳回，u 驳回）",
                ChangeState::Applied => "已落盘",
                ChangeState::Discarded => "已驳回",
            };
            lines.push(Line::default());
            lines.push(Line::from(vec![
                Span::styled("工部奏本　", Style::new().fg(GOLD).bold()),
                Span::raw(format!(
                    "改动 {} 个文件，{state}{}",
                    changes.files.len(),
                    skipped_note(&changes.skipped)
                )),
            ]));
            // 与 Web 同一份预览：行号、按语言着色，成对删增行中改动的字词加底色。
            for file in preview_changes(&changes.files) {
                let created = if file.created { "（新建）" } else { "" };
                lines.push(Line::from(format!("▸ {}{created}", file.path)).bold());
                for hunk in &file.hunks {
                    lines.push(Line::from(format!("@@ 第 {} 段 @@", hunk.number)).dim());
                    for line in &hunk.lines {
                        // (记号, 记号色, 行底色, 改动字词的底色)
                        let (sign, tint, back, mark) = match line.kind {
                            '+' => (
                                "+",
                                Color::Green,
                                Color::Rgb(20, 45, 25),
                                Color::Rgb(40, 105, 55),
                            ),
                            '-' => (
                                "-",
                                Color::Red,
                                Color::Rgb(55, 20, 18),
                                Color::Rgb(125, 40, 35),
                            ),
                            _ => (" ", Color::Reset, Color::Reset, Color::Reset),
                        };
                        let number =
                            |n: Option<usize>| n.map_or("    ".into(), |n| format!("{n:>4}"));
                        let mut spans = vec![
                            Span::styled(
                                format!("{} {} ", number(line.old), number(line.new)),
                                Style::new().dim(),
                            ),
                            Span::styled(format!("{sign} "), Style::new().fg(tint)),
                        ];
                        spans.extend(line.spans.iter().map(|s| {
                            let fg = token_color(s.token).unwrap_or(Color::Reset);
                            let style = if s.changed {
                                Style::new().fg(fg).bg(mark).bold()
                            } else {
                                Style::new().fg(fg).bg(back)
                            };
                            Span::styled(s.text.clone(), style)
                        }));
                        lines.push(Line::from(spans));
                    }
                }
            }
        }
        lines.push(Line::default());
        let mut following = false;
        match &task.status {
            TaskStatus::Completed { summary, .. } => {
                lines.push(Line::from("回奏").fg(GOLD).bold());
                lines.extend(summary.lines().map(|l| Line::from(l.to_owned())));
                if task.review.is_none() {
                    lines.push(Line::default());
                    lines.push(Line::from("按 a 采纳、d 驳回").dim());
                }
                if task.censure.is_none() {
                    lines.push(Line::from("按 i 请都察院复核").dim());
                }
            }
            TaskStatus::Failed { reason, .. } => {
                lines.push(Line::from(format!("失败：{reason}")).fg(Color::Red));
            }
            TaskStatus::Pending | TaskStatus::Assigned(_) => {
                if let Some(live) = self.live.get(&task.edict.id) {
                    lines.push(Line::from("实时输出").fg(GOLD).bold());
                    lines.extend(live.lines().map(|l| Line::from(l.to_owned())));
                    following = true;
                } else if court.needs_approval(task) {
                    let heading = Style::new().fg(VERMILION).bold();
                    match &task.draft {
                        Some(draft) => {
                            let model = draft
                                .model
                                .as_ref()
                                .map_or(String::new(), |m| format!("（{m}）"));
                            lines.push(Line::from(vec![
                                Span::styled("内阁票拟　", heading),
                                Span::raw(format!("拟派 {}{model}", name(draft.worker))),
                            ]));
                            lines.extend(draft.note.lines().map(|l| Line::from(l.to_owned())));
                        }
                        None => lines.push(Line::from(vec![
                            Span::styled("内阁票拟　", heading),
                            Span::raw("尚未票拟；未票拟即批红为中旨，按旧例派工。"),
                        ])),
                    }
                    lines.push(Line::from(vec![
                        label("路线"),
                        Span::raw(match court.route_task(task) {
                            Ok(route) => route.path.join(" → "),
                            Err(e) => e.to_string(),
                        }),
                    ]));
                    lines.push(Line::default());
                    lines.push(
                        Line::from("待批红。按 t 内阁票拟，e 御笔改票，b 朱笔批红，c 撤销。").dim(),
                    );
                } else if matches!(task.status, TaskStatus::Pending) {
                    lines.push(Line::from("排队中。按 r 执行，c 撤销。").dim());
                }
            }
            TaskStatus::Cancelled => {}
        }
        (Text::from(lines), following)
    }

    pub(crate) fn draw_officials(&self, frame: &mut Frame, area: Rect) {
        let court = self.court();
        // 各部的缺位已见于派工路线；另列中枢与都察院的缺位及其影响。
        let vacancies: Vec<String> = ziyuan_app::vacancies(court)
            .into_iter()
            .filter(|v| !matches!(v.office, Office::Minister(_)))
            // 只留缺位与影响的头一句，附吏部荐举的人选，一行放得下。
            .map(|v| {
                let head = v.impact.split('（').next().unwrap_or(&v.impact).to_owned();
                match v.candidate {
                    Some((id, name)) => format!("{head}；吏部荐 #{} {name}", id.0),
                    None => format!("{head}；无人可荐"),
                }
            })
            .collect();
        let [table_area, routes_area] = Layout::vertical([
            Constraint::Min(5),
            Constraint::Length((Department::ALL.len() + vacancies.len()) as u16 + 2),
        ])
        .areas(area);
        let rows: Vec<Row> = court
            .agents()
            .map(|a| {
                let demoted = court.demotion(a.id);
                let office = court.office_of(a.id).map(|o| {
                    let mut label = format!("{}·{}", o.label(), o.rank(court.mode()));
                    if demoted > 0 {
                        label.push_str(&format!("（降{demoted}级）"));
                    }
                    label
                });
                let record = court.service_record(a.id);
                Row::new(vec![
                    Cell::from(format!("#{}", a.id.0)).dim(),
                    Cell::from(a.name.clone()),
                    Cell::from(format!(
                        "{} {}",
                        court.courtesy_name(a.id),
                        court.art_name(a.id)
                    ))
                    .dim(),
                    match office {
                        Some(o) if demoted > 0 => Cell::from(o).fg(Color::Red),
                        Some(o) => Cell::from(o).fg(GOLD),
                        None if court.is_purged(a.id) => Cell::from("已革职").fg(Color::Red),
                        None => Cell::from("未任官职").dim(),
                    },
                    Cell::from(a.speciality.label()),
                    Cell::from(Persona::of(a.id).label()).fg(Color::Magenta),
                    Cell::from(
                        court
                            .model_of(a.id)
                            .unwrap_or(self.model_label())
                            .to_owned(),
                    )
                    .dim(),
                    Cell::from(describe_record(&record)),
                    Cell::from(record.usage.total().to_string()),
                ])
            })
            .collect();
        let count = rows.len();
        let table = Table::new(
            rows,
            [
                Constraint::Length(4),
                Constraint::Length(8),
                Constraint::Length(12),
                Constraint::Length(20),
                Constraint::Length(6),
                Constraint::Length(10),
                Constraint::Length(16),
                Constraint::Min(30),
                Constraint::Length(8),
            ],
        )
        .header(
            Row::new([
                "ID",
                "姓名",
                "字 号",
                "官职·品级",
                "专长",
                "人格",
                "模型",
                "考绩",
                "tokens",
            ])
            .fg(GOLD)
            .bold(),
        )
        .row_highlight_style(Style::new().bg(Color::DarkGray).bold())
        .highlight_symbol("▌")
        .block(panel(format!("百官（{count}）")));
        let mut state = TableState::default().with_selected(Some(self.selected_official));
        frame.render_stateful_widget(table, table_area, &mut state);

        let routes: Vec<Line> = Department::ALL
            .iter()
            .map(|&d| {
                let route = court
                    .new_edict(d, "预览".into(), Seal::default())
                    .and_then(|edict| court.route(&edict));
                let mut spans = vec![Span::styled(
                    format!("{}　", d.label()),
                    Style::new().fg(GOLD),
                )];
                spans.push(match route {
                    Ok(route) => Span::raw(route.path.join(" → ")),
                    Err(e) => Span::styled(e.to_string(), Style::new().fg(Color::Red)),
                });
                Line::from(spans)
            })
            .chain(vacancies.into_iter().map(|n| {
                Line::from(vec![
                    Span::styled("缺位　", Style::new().fg(GOLD)),
                    Span::styled(n, Style::new().fg(Color::Red)),
                ])
            }))
            .collect();
        frame.render_widget(
            Paragraph::new(routes).block(panel(format!("派工路线 · {}", court.mode().label()))),
            routes_area,
        );
    }

    /// 起居注最近的 [`HISTORY_LIMIT`] 条（时辰并入各条的时间，不单列）；更早的用 `ziyuan history` 筛选。
    pub(crate) fn history(&self) -> Vec<HistoryEntry> {
        let filter = HistoryFilter {
            limit: Some(HISTORY_LIMIT),
            ..HistoryFilter::default()
        };
        self.session
            .history()
            .ok()
            .and_then(|h| history_entries(h, &filter).ok())
            .unwrap_or_default()
    }

    pub(crate) fn draw_history(&self, frame: &mut Frame, area: Rect) {
        let history = self.history();
        let offset = tz_offset();
        let items: Vec<ListItem> = history
            .iter()
            .map(|e| {
                let at =
                    e.at.map_or(String::new(), |t| format_time(t, offset)[5..].to_owned());
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:>4} {at:<11} ", e.seq), Style::new().dim()),
                    Span::raw(e.text.clone()),
                ]))
            })
            .collect();
        let selected = self
            .selected_event
            .unwrap_or(history.len().saturating_sub(1));
        let mut state = ListState::default().with_selected(Some(selected));
        frame.render_stateful_widget(
            List::new(items)
                .block(panel(if history.len() < HISTORY_LIMIT {
                    format!("起居注（{} 条事件）", history.len())
                } else {
                    format!("起居注（最近 {HISTORY_LIMIT} 条；更早的用 ziyuan history 筛选）")
                }))
                .highlight_style(Style::new().bg(Color::DarkGray)),
            area,
            &mut state,
        );
    }

    pub(crate) fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        let [status, keys] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
        if let Some((message, is_error)) = &self.status {
            let style = if *is_error {
                Style::new().fg(Color::Red).bold()
            } else {
                Style::new().fg(Color::Green)
            };
            frame.render_widget(Line::styled(format!(" {message}"), style), status);
        }
        let hints: &[(&str, &str)] = match (&self.modal, self.tab) {
            // 只有下诏与招募可换部门。
            (
                Some(Modal::Input {
                    kind: InputKind::Edict | InputKind::Recruit,
                    ..
                }),
                _,
            ) => &[("Enter", "确定"), ("Tab", "换部门"), ("Esc", "取消")],
            (Some(Modal::Input { .. }), _) => &[("Enter", "确定"), ("Esc", "取消")],
            (Some(Modal::Choose { .. }), _) => {
                &[("↑↓", "选择"), ("Enter", "确定"), ("Esc", "取消")]
            }
            (Some(Modal::Confirm(_)), _) => &[("y", "确认"), ("其他键", "取消")],
            (Some(Modal::Notice(_)), _) => &[("任意键", "退朝")],
            (None, Tab::Edicts) => &[
                ("n", "下诏"),
                ("T", "模板"),
                ("r", "执行"),
                ("s", "停止"),
                ("c", "撤销"),
                ("a/d", "采纳/驳回"),
                ("t/e", "票拟/改票"),
                ("b", "批红"),
                ("w/W/u", "准奏/逐段/驳回改动"),
                ("f", "分办"),
                ("v", "廷议"),
                ("i", "复核"),
                ("x/X", "复勘/平反"),
                ("R", "实录"),
                ("U", "加急"),
                ("K", "考成"),
                ("H", "度支"),
                ("D", "修道/回朝"),
                ("g", "改制"),
                ("Tab", "切换"),
                ("q", "退出"),
            ],
            (None, Tab::Officials) => &[
                ("n", "招募"),
                ("p", "任命"),
                ("d", "罢免"),
                ("x", "革职"),
                ("e", "应试"),
                ("m", "易名"),
                ("N/L", "廷推/枚卜"),
                ("R", "实录"),
                ("c/P", "廷杖/宽宥"),
                ("g", "改制"),
                ("Tab", "切换"),
                ("q", "退出"),
            ],
            (None, Tab::History) => &[("↑↓", "浏览"), ("Tab", "切换"), ("q", "退出")],
        };
        let spans: Vec<Span> = hints
            .iter()
            .flat_map(|(key, action)| {
                [
                    Span::styled(format!(" {key} "), Style::new().fg(Color::Black).bg(GOLD)),
                    Span::raw(format!(" {action}  ")),
                ]
            })
            .collect();
        frame.render_widget(Line::from(spans), keys);
    }

    pub(crate) fn draw_modal(&self, frame: &mut Frame, modal: &Modal) {
        let area = frame.area();
        match modal {
            Modal::Input {
                kind,
                text,
                department,
            } => {
                let (title, prompt, with_department) = match kind {
                    InputKind::Edict => ("下诏", "诏令内容：", true),
                    InputKind::Recruit => ("招募官员", "姓名（留空随机）：", true),
                    InputKind::Review(_, verdict) => (
                        if *verdict == Verdict::Accepted {
                            "采纳回奏"
                        } else {
                            "驳回回奏"
                        },
                        "备注（可空）：",
                        false,
                    ),
                    InputKind::Skip(_) => ("逐段准奏", "驳回第几段（如 1,3）：", false),
                    InputKind::Records => ("翰林院实录", "检索词（留空看最近）：", false),
                    InputKind::Rename(_) => ("易名", "姓名 字 号：", false),
                };
                let title = match kind {
                    InputKind::Review(task, _) | InputKind::Skip(task) => {
                        format!("{title} · 诏令 #{}", task.0)
                    }
                    InputKind::Rename(agent) => format!("{title} · 官员 #{}", agent.0),
                    _ => title.to_owned(),
                };
                let popup = centered(area, 64, if with_department { 6 } else { 5 });
                let mut lines = Vec::new();
                if with_department {
                    let label = if matches!(kind, InputKind::Recruit) {
                        "专长"
                    } else {
                        "部门"
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("{label}："), Style::new().fg(GOLD)),
                        Span::styled(
                            format!("◀ {} ▶", Department::ALL[*department].label()),
                            Style::new().bold(),
                        ),
                        Span::raw("  Tab 切换").dim(),
                    ]));
                }
                let input = Line::from(vec![
                    Span::styled(prompt, Style::new().fg(GOLD)),
                    Span::raw(text.clone()),
                ]);
                let cursor_x = popup.x + 2 + input.width() as u16;
                let cursor_y = popup.y + 1 + lines.len() as u16 + 1;
                lines.push(Line::default());
                lines.push(input);
                frame.render_widget(Clear, popup);
                frame.render_widget(Paragraph::new(lines).block(modal_block(title)), popup);
                frame.set_cursor_position((cursor_x.min(popup.right() - 2), cursor_y));
            }
            Modal::Choose { kind, selected } => {
                let options = self.options(*kind);
                let title = match kind {
                    ChooseKind::Reform => "改制".to_owned(),
                    ChooseKind::Appoint(agent) => {
                        let name = self.court().agent(*agent).map_or("", |a| a.name.as_str());
                        format!("任命 #{} {name}", agent.0)
                    }
                    ChooseKind::Redraft(task) => format!("御笔改票 · 诏令 #{}", task.0),
                    ChooseKind::Abolish => "罢中书省，丞相如何处置？".to_owned(),
                    ChooseKind::Template => "翰林院模板 · 选定即入队".to_owned(),
                    ChooseKind::Nominate => "廷推 · 九卿会推出缺之职".to_owned(),
                    ChooseKind::Lots => "枚卜 · 金瓯覆名，掣签即任".to_owned(),
                };
                let popup = centered(area, 36, options.len() as u16 + 2);
                frame.render_widget(Clear, popup);
                let mut state = ListState::default().with_selected(Some(*selected));
                frame.render_stateful_widget(
                    List::new(options)
                        .block(modal_block(title))
                        .highlight_style(Style::new().fg(Color::Black).bg(GOLD).bold())
                        .highlight_symbol("▶ "),
                    popup,
                    &mut state,
                );
            }
            Modal::Confirm(kind) => {
                let text = match kind {
                    ConfirmKind::Purge(agent) => {
                        let name = self.court().agent(*agent).map_or("", |a| a.name.as_str());
                        format!("革去官员 #{} {name}？削去仕籍，永不叙用。", agent.0)
                    }
                    ConfirmKind::Cane(agent) => {
                        let name = self.court().agent(*agent).map_or("", |a| a.name.as_str());
                        format!("廷杖官员 #{} {name}？降一级留用，收回写权。", agent.0)
                    }
                };
                let paragraph = Paragraph::new(vec![
                    Line::from(text),
                    Line::default(),
                    Line::from("按 y 确认，其他键取消").dim(),
                ])
                .wrap(Wrap { trim: false })
                .block(modal_block("圣裁".into()));
                let height = paragraph.line_count(50) as u16; // 已含边框
                let popup = centered(area, 50, height);
                frame.render_widget(Clear, popup);
                frame.render_widget(paragraph, popup);
            }
            Modal::Notice(note) => {
                let paragraph = Paragraph::new(Text::from(
                    note.lines()
                        .map(|l| Line::from(l.to_owned()))
                        .collect::<Vec<_>>(),
                ))
                .wrap(Wrap { trim: false })
                .block(modal_block("史官记".into()));
                let height = paragraph.line_count(60) as u16; // 已含边框
                let popup = centered(area, 60, height);
                frame.render_widget(Clear, popup);
                frame.render_widget(paragraph, popup);
            }
        }
    }
}

/// 把宽度有歧义的符号（`·`、`—`、`…`、`→` 等）换成 ASCII。
///
/// 这些符号在 CJK 终端（或用中文字体）里常显示为两格，ratatui 却按一格排版，后面的字便被挤开
/// 或被盖住。界面文字多来自 CLI、Web 共用的描述，故在每帧画完后统一替换，只改终端界面。
/// 边框线与方块字符（U+2500–U+259F）各终端都按一格画，不动；没列出的（如回奏里的希腊字母）也不动。
pub(crate) fn narrow_ambiguous(buffer: &mut ratatui::buffer::Buffer) {
    for cell in &mut buffer.content {
        let mut chars = cell.symbol().chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            continue;
        };
        let ascii = match c {
            '·' | '‧' | '∙' => "/",
            '—' | '–' | '―' => "-",
            '…' => "~",
            '→' | '▶' | '▸' => ">",
            '←' | '◀' => "<",
            '↑' => "^",
            '↓' => "v",
            '•' | '●' | '★' | '※' => "*",
            '○' => "o",
            '×' => "x",
            _ => continue,
        };
        cell.set_symbol(ascii);
    }
}
