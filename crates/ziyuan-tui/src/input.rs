//! 按键处理：各标签页与弹窗的按键、输入与选择。

use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ziyuan_app::{ABOLITION_NOTE, duzhi_report, kaocheng_report, skipped_note};
use ziyuan_core::{Department, GovernanceMode, Seal, Verdict};
use ziyuan_runtime::Outcome;

use crate::App;
use crate::app::{
    ChooseKind, ConfirmKind, InputKind, Message, Modal, TABS, Tab, appointable, drawable, nominable,
};

impl App {
    pub fn handle(&mut self, message: Message) {
        match message {
            Message::Input(Event::Key(key)) if key.kind == KeyEventKind::Press => self.key(key),
            Message::Input(_) => {}
            Message::Assigned(_) => self.reload(),
            Message::Text(task, text) => self.live.entry(task).or_default().push_str(&text),
            Message::Finished { task, outcome } => {
                self.live.remove(&task);
                self.reload();
                match outcome {
                    Outcome::Completed(_) => self.info(format!("诏令 #{} 已回奏", task.0)),
                    Outcome::Failed(reason) => {
                        self.error(format!("诏令 #{} 失败：{reason}", task.0))
                    }
                    Outcome::Cancelled => self.info(format!("诏令 #{} 已撤销", task.0)),
                    Outcome::NotStarted => {}
                }
            }
            Message::Split(task, result) => {
                self.reload();
                match result {
                    Ok(n) => self.info(format!("诏令 #{} 已分办为 {n} 件（按 r 执行）", task.0)),
                    Err(e) => self.error(format!("分办失败：{e}")),
                }
            }
            Message::Drafted(task, result) => {
                self.reload();
                match result {
                    Ok(worker) => {
                        let name = self.court().agent(worker).map_or("?", |a| a.name.as_str());
                        let message =
                            format!("诏令 #{} 票拟：拟派 {name}（按 b 批红、e 改票）", task.0);
                        self.info(message);
                    }
                    Err(e) => self.error(format!("票拟失败：{e}")),
                }
            }
            Message::Speech(task, line) => {
                if let Some(speeches) = self.debating.get_mut(&task) {
                    speeches.push(line);
                }
            }
            Message::Debated(task, result) => {
                self.debating.remove(&task);
                self.reload();
                match result {
                    Ok(None) => self.info(format!("诏令 #{} 廷议毕，纪要已记入事件簿", task.0)),
                    Ok(Some(reason)) => self.error(format!("廷议中止：{reason}；已有发言照记")),
                    Err(e) => self.error(format!("廷议失败：{e}")),
                }
            }
            Message::Examined(agent, result) => {
                self.reload();
                match result {
                    Ok(scores) => {
                        self.info(format!("礼部判卷 #{}：{}", agent.0, scores.join("，")))
                    }
                    Err(e) => self.error(format!("科举未成：{e}")),
                }
            }
            Message::Nominated(office, result) => {
                self.reload();
                match result {
                    Ok(note) => {
                        self.info(format!("廷推{}毕，按 p 任命或 L 枚卜", office.label()));
                        self.modal = Some(Modal::Notice(note));
                    }
                    Err(e) => self.error(format!("廷推未成：{e}")),
                }
            }
            Message::Censured(task, result) => {
                self.reload();
                match result {
                    Ok(label) => self.info(format!("诏令 #{} 都察院{label}", task.0)),
                    Err(e) => self.error(format!("复核失败：{e}")),
                }
            }
            Message::Rechecked(task, result) => {
                self.reload();
                match result {
                    Ok((cleared, upheld)) => self.info(format!(
                        "诏令 #{} 复勘毕：误报 {cleared} 处，维持 {upheld} 处（按 X 平反误报）",
                        task.0
                    )),
                    Err(e) => self.error(format!("复勘失败：{e}")),
                }
            }
            Message::RunDone(error) => {
                self.run = None;
                self.live.clear();
                self.reload();
                match error {
                    Some(e) => self.error(format!("执行出错：{e}")),
                    None if self.court().open_tasks().is_empty() => self.info("诏令已全部结束"),
                    None => self.info("已停止；未开始的诏令仍在队列中"),
                }
            }
        }
    }

    pub(crate) fn key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.modal.is_some() {
            self.modal_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Tab | KeyCode::Right => self.switch_tab(1),
            KeyCode::BackTab | KeyCode::Left => self.switch_tab(TABS.len() - 1),
            KeyCode::Char(c @ '1'..='3') => self.tab = TABS[c as usize - '1' as usize].0,
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(5),
            KeyCode::PageDown => self.detail_scroll = self.detail_scroll.saturating_add(5),
            KeyCode::Char('r') => self.start_run(),
            KeyCode::Char('s') => self.stop_run(),
            KeyCode::Char('D') => {
                let on = !self.court().in_retreat();
                let done = if on {
                    "陛下移居西苑修道"
                } else {
                    "陛下回朝亲政"
                };
                if let Some(note) = self.mutate(|s| s.retreat(on), |_| done.into()) {
                    self.modal = Some(Modal::Notice(note));
                }
            }
            KeyCode::Char('K') => {
                self.modal = Some(Modal::Notice(kaocheng_report(self.court())));
            }
            KeyCode::Char('H') => {
                let backend = &self.config.backend;
                let report =
                    duzhi_report(self.court(), self.model.as_deref(), |m| backend.price(m));
                self.modal = Some(Modal::Notice(report));
            }
            KeyCode::Char('g') => {
                let selected = GovernanceMode::ALL
                    .iter()
                    .position(|&m| m == self.court().mode())
                    .unwrap_or(0);
                self.modal = Some(Modal::Choose {
                    kind: ChooseKind::Reform,
                    selected,
                });
            }
            KeyCode::Char('R') => {
                self.modal = Some(Modal::Input {
                    kind: InputKind::Records,
                    text: String::new(),
                    department: 0,
                });
            }
            KeyCode::Char('T') if self.tab == Tab::Edicts => {
                self.modal = Some(Modal::Choose {
                    kind: ChooseKind::Template,
                    selected: 0,
                });
            }
            KeyCode::Char('n') => {
                let kind = match self.tab {
                    Tab::Officials => InputKind::Recruit,
                    _ => InputKind::Edict,
                };
                let works = Department::ALL.iter().position(|&d| d == Department::Works);
                self.modal = Some(Modal::Input {
                    kind,
                    text: String::new(),
                    department: works.unwrap_or(0),
                });
            }
            code => match self.tab {
                Tab::Edicts => self.edict_key(code),
                Tab::Officials => self.official_key(code),
                Tab::History => {}
            },
        }
    }

    pub(crate) fn edict_key(&mut self, code: KeyCode) {
        let Some(task) = self.selected_task else {
            return;
        };
        match code {
            KeyCode::Char('c') => {
                self.mutate(|s| s.cancel(task), |_| format!("诏令 #{} 已撤销", task.0));
            }
            KeyCode::Char('t') => self.start_draft(task),
            KeyCode::Char('U') => {
                self.mutate(
                    |s| s.expedite(task),
                    |_| format!("诏令 #{} 八百里加急：插队先行，时限加倍", task.0),
                );
            }
            KeyCode::Char('i') => self.start_censor(task),
            KeyCode::Char('x') => self.start_recheck(task),
            KeyCode::Char('X') => {
                // 不指定序号：平反刑部复勘判为误报的各处；规则在治理层。
                if let Some(note) = self.mutate(
                    |s| s.acquit(task, None),
                    |(done, _)| format!("诏令 #{} 平反 {} 处", task.0, done.len()),
                ) {
                    self.modal = Some(Modal::Notice(note.1.to_owned()));
                }
            }
            KeyCode::Char('f') => self.start_split(task),
            KeyCode::Char('v') => self.start_debate(task),
            KeyCode::Char('w') => {
                let root = self.config.root.clone();
                self.mutate(
                    |s| s.apply_changes(task, &root, Vec::new()),
                    |_| format!("诏令 #{} 准奏，改动已写入项目", task.0),
                );
            }
            KeyCode::Char('m') => {
                let root = self.config.root.clone();
                // 落盘成功后提交；提交失败只报告，改动仍已落盘。
                if self
                    .mutate(
                        |s| s.apply_changes(task, &root, Vec::new()),
                        |_| String::new(),
                    )
                    .is_some()
                {
                    match self.session.commit_applied(task, &root) {
                        Ok(made) => self.info(format!(
                            "诏令 #{} 准奏，改动已写入项目，记为 {} 提交 {}",
                            task.0, made.vcs, made.id
                        )),
                        Err(e) => {
                            self.error(format!("诏令 #{} 已准奏落盘，但未能提交：{e}", task.0))
                        }
                    }
                }
            }
            KeyCode::Char('W') => {
                self.modal = Some(Modal::Input {
                    kind: InputKind::Skip(task),
                    text: String::new(),
                    department: 0,
                })
            }
            KeyCode::Char('u') => {
                self.mutate(
                    |s| s.discard_changes(task),
                    |_| format!("诏令 #{} 的改动已驳回", task.0),
                );
            }
            KeyCode::Char('e') => {
                self.modal = Some(Modal::Choose {
                    kind: ChooseKind::Redraft(task),
                    selected: 0,
                })
            }
            KeyCode::Char('b') => {
                self.mutate(
                    |s| s.approve(task),
                    |_| format!("诏令 #{} 已批红（按 r 执行）", task.0),
                );
            }
            KeyCode::Char('a' | 'd') => {
                let verdict = if code == KeyCode::Char('a') {
                    Verdict::Accepted
                } else {
                    Verdict::Rejected
                };
                self.modal = Some(Modal::Input {
                    kind: InputKind::Review(task, verdict),
                    text: String::new(),
                    department: 0,
                });
            }
            _ => {}
        }
    }

    pub(crate) fn official_key(&mut self, code: KeyCode) {
        let Some(agent) = self
            .court()
            .agents()
            .nth(self.selected_official)
            .map(|a| a.id)
        else {
            return;
        };
        match code {
            KeyCode::Char('p') => {
                self.modal = Some(Modal::Choose {
                    kind: ChooseKind::Appoint(agent),
                    selected: 0,
                })
            }
            KeyCode::Char('x') => self.modal = Some(Modal::Confirm(ConfirmKind::Purge(agent))),
            KeyCode::Char('c') => self.modal = Some(Modal::Confirm(ConfirmKind::Cane(agent))),
            KeyCode::Char('P') => {
                if let Some(note) =
                    self.mutate(|s| s.pardon(agent), |_| format!("官员 #{} 蒙宽宥", agent.0))
                {
                    self.modal = Some(Modal::Notice(note));
                }
            }
            KeyCode::Char(c @ ('N' | 'L')) => {
                let kind = if c == 'N' {
                    ChooseKind::Nominate
                } else {
                    ChooseKind::Lots
                };
                if self.options(kind).is_empty() {
                    self.error(if c == 'N' {
                        "无可廷推的官职：须有出缺之职，且吏部尚书在任"
                    } else {
                        "尚无廷推，无从枚卜：先按 N 廷推"
                    });
                } else {
                    self.modal = Some(Modal::Choose { kind, selected: 0 });
                }
            }
            KeyCode::Char('e') => self.start_exam(agent),
            KeyCode::Char('m') => {
                let court = self.court();
                let text = format!(
                    "{} {} {}",
                    court.agent(agent).map_or("", |a| a.name.as_str()),
                    court.courtesy_name(agent),
                    court.art_name(agent)
                );
                self.modal = Some(Modal::Input {
                    kind: InputKind::Rename(agent),
                    text,
                    department: 0,
                });
            }
            KeyCode::Char('d') => {
                self.mutate(
                    |s| s.dismiss(agent),
                    |_| format!("官员 #{} 已罢免现职", agent.0),
                );
            }
            _ => {}
        }
    }

    pub(crate) fn modal_key(&mut self, key: KeyEvent) {
        match self.modal {
            Some(Modal::Notice(_)) => {
                self.modal = None;
                return;
            }
            Some(Modal::Confirm(kind)) => {
                self.modal = None;
                if matches!(key.code, KeyCode::Char('y' | 'Y')) {
                    self.confirm(kind);
                }
                return;
            }
            _ => {}
        }
        let count = match &self.modal {
            Some(Modal::Choose { kind, .. }) => self.options(*kind).len().max(1),
            _ => 1,
        };
        let Some(modal) = &mut self.modal else {
            return;
        };
        if key.code == KeyCode::Esc {
            self.modal = None;
            return;
        }
        match modal {
            Modal::Input {
                text, department, ..
            } => match key.code {
                KeyCode::Char(c) => text.push(c),
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Tab => *department = (*department + 1) % Department::ALL.len(),
                KeyCode::BackTab => {
                    *department = (*department + Department::ALL.len() - 1) % Department::ALL.len()
                }
                KeyCode::Enter => self.submit_input(),
                _ => {}
            },
            Modal::Choose { selected, .. } => match key.code {
                KeyCode::Up | KeyCode::Char('k') => *selected = (*selected + count - 1) % count,
                KeyCode::Down | KeyCode::Char('j') => *selected = (*selected + 1) % count,
                KeyCode::Enter => self.submit_choice(),
                _ => {}
            },
            // 已在本函数开头处理。
            Modal::Confirm(_) | Modal::Notice(_) => {}
        }
    }

    pub(crate) fn submit_input(&mut self) {
        let Some(Modal::Input {
            kind,
            text,
            department,
        }) = self.modal.take()
        else {
            return;
        };
        let text = text.trim().to_owned();
        let department = Department::ALL[department];
        match kind {
            InputKind::Edict if text.is_empty() => {
                self.error("内容不能为空");
            }
            InputKind::Edict => {
                if let Some(id) = self.mutate(
                    |s| Ok(s.issue(department, text, Seal::default())?.id),
                    |id| format!("诏令 #{} 已入队（按 r 执行）", id.0),
                ) {
                    self.selected_task = Some(id);
                    self.detail_scroll = 0;
                }
            }
            InputKind::Recruit => {
                self.mutate(
                    |s| {
                        let id = s.recruit((!text.is_empty()).then_some(text), department)?;
                        Ok((id, s.court().agent(id).expect("刚招募").name.clone()))
                    },
                    |(id, name)| format!("已招募官员 #{} {name}（按 p 授官）", id.0),
                );
            }
            InputKind::Rename(agent) => {
                let court = self.court();
                let now = [
                    court.agent(agent).map_or(String::new(), |a| a.name.clone()),
                    court.courtesy_name(agent),
                    court.art_name(agent),
                ];
                let words: Vec<&str> = text.split_whitespace().collect();
                if words.len() != 3 {
                    self.error("请依次写姓名、字、号，以空格分开");
                    return;
                }
                // 与现在相同的项不改。
                let [name, courtesy, art] =
                    [0, 1, 2].map(|i| (words[i] != now[i]).then(|| words[i].to_owned()));
                if let Some(note) = self.mutate(
                    |s| s.rename(agent, name, courtesy, art),
                    |_| format!("官员 #{} 易名", agent.0),
                ) {
                    self.modal = Some(Modal::Notice(note));
                }
            }
            InputKind::Review(task, verdict) => {
                self.mutate(
                    |s| s.review(task, verdict, text),
                    |_| format!("诏令 #{} 评定为{}", task.0, verdict.label()),
                );
            }
            InputKind::Records => {
                let records = self.court().records();
                let found = ziyuan_core::find_records(&records, &text);
                let mut note = if text.is_empty() {
                    format!("翰林院实录 · 共 {} 件（最近 10 件）", records.len())
                } else {
                    format!("翰林院实录 ·「{text}」相符 {} 件", found.len())
                };
                for r in found.iter().take(10) {
                    let summary: String = r.summary.chars().take(60).collect();
                    let more = if r.summary.chars().count() > 60 {
                        "…"
                    } else {
                        ""
                    };
                    note.push_str(&format!(
                        "\n\n#{} {}（{}）· {}：{}\n  {}{more}",
                        r.task.0,
                        r.department.label(),
                        if r.handler.is_empty() {
                            "已削籍"
                        } else {
                            &r.handler
                        },
                        r.verdict,
                        r.edict,
                        summary.replace('\n', " "),
                    ));
                }
                if found.is_empty() {
                    note.push_str("\n\n无相符的实录");
                }
                self.modal = Some(Modal::Notice(note));
            }
            InputKind::Skip(task) => {
                let parsed: Result<Vec<u32>, _> = text
                    .split([',', '，', ' ', '、'])
                    .filter(|n| !n.is_empty())
                    .map(str::parse)
                    .collect();
                let Ok(skipped) = parsed else {
                    self.error("段号须是数字，以逗号分隔");
                    return;
                };
                let root = self.config.root.clone();
                self.mutate(
                    |s| s.apply_changes(task, &root, skipped.clone()),
                    |_| {
                        format!(
                            "诏令 #{} 准奏，改动已写入项目{}",
                            task.0,
                            skipped_note(&skipped)
                        )
                    },
                );
            }
        }
    }

    pub(crate) fn confirm(&mut self, kind: ConfirmKind) {
        match kind {
            ConfirmKind::Purge(agent) => {
                self.mutate(
                    |s| s.purge(agent),
                    |_| format!("官员 #{} 革职，削去仕籍，永不叙用", agent.0),
                );
            }
            ConfirmKind::Cane(agent) => {
                if let Some(note) =
                    self.mutate(|s| s.cane(agent), |_| format!("官员 #{} 受廷杖", agent.0))
                {
                    self.modal = Some(Modal::Notice(note));
                }
            }
        }
    }

    pub(crate) fn submit_choice(&mut self) {
        let Some(Modal::Choose { kind, selected }) = self.modal.take() else {
            return;
        };
        match kind {
            ChooseKind::Reform => {
                let mode = GovernanceMode::ALL[selected];
                if mode == GovernanceMode::DirectRule
                    && self.court().mode() == GovernanceMode::Chancellor
                {
                    self.modal = Some(Modal::Choose {
                        kind: ChooseKind::Abolish,
                        selected: 0,
                    });
                    return;
                }
                if let Some(note) =
                    self.mutate(|s| s.reform(mode), |_| format!("制度改为{}", mode.label()))
                {
                    self.modal = Some(Modal::Notice(note.to_owned()));
                }
            }
            ChooseKind::Appoint(agent) => {
                let Some(&office) = appointable(self.court(), agent).get(selected) else {
                    return;
                };
                if let Some(Some(note)) = self.mutate(
                    |s| s.appoint(agent, office),
                    |_| format!("任命官员 #{} 为{}", agent.0, office.label()),
                ) {
                    self.modal = Some(Modal::Notice(note));
                }
            }
            ChooseKind::Nominate => {
                if let Some(&office) = nominable(self.court()).get(selected) {
                    self.start_nomination(office);
                }
            }
            ChooseKind::Lots => {
                let Some(&office) = drawable(self.court()).get(selected) else {
                    return;
                };
                if let Some((_, note)) = self.mutate(
                    |s| s.draw_lots(office),
                    |(agent, _)| format!("枚卜得 #{}，任{}", agent.0, office.label()),
                ) {
                    self.modal = Some(Modal::Notice(note));
                }
            }
            ChooseKind::Abolish if selected == 0 => {
                let mode = GovernanceMode::DirectRule;
                if let Some(note) =
                    self.mutate(|s| s.reform(mode), |_| format!("制度改为{}", mode.label()))
                {
                    self.modal = Some(Modal::Notice(note.to_owned()));
                }
            }
            ChooseKind::Abolish => {
                if let Some(summary) = self.mutate(
                    |s| s.abolish_chancellery(),
                    |_| "罢中书省，株连百官，废相亲政".into(),
                ) {
                    self.modal = Some(Modal::Notice(format!("{summary}\n\n{ABOLITION_NOTE}")));
                }
            }
            ChooseKind::Template => {
                let Some((name, t, _)) = self.court().templates().into_iter().nth(selected) else {
                    return;
                };
                let issued = self.mutate(
                    |s| s.issue_templated(&name, t.department, t.text, t.seal),
                    |e| format!("诏令 #{} 按模板「{name}」入队（按 r 执行）", e.id.0),
                );
                if let Some(edict) = issued {
                    self.selected_task = Some(edict.id);
                }
            }
            ChooseKind::Redraft(task) => {
                let Some(worker) = self
                    .redraft_candidates(task)
                    .get(selected)
                    .map(|(id, _)| *id)
                else {
                    return;
                };
                self.mutate(
                    |s| s.redraft(task, worker, String::new()),
                    |_| format!("诏令 #{} 御笔改票（按 b 批红）", task.0),
                );
            }
        }
    }

    pub(crate) fn options(&self, kind: ChooseKind) -> Vec<String> {
        let mode = self.court().mode();
        match kind {
            ChooseKind::Reform => GovernanceMode::ALL
                .iter()
                .map(|m| m.label().to_owned())
                .collect(),
            ChooseKind::Appoint(agent) => appointable(self.court(), agent)
                .iter()
                .map(|o| format!("{}（{}）", o.label(), o.rank(mode)))
                .collect(),
            ChooseKind::Abolish => vec![
                "罢免丞相（留仕籍，可再起用）".to_owned(),
                "一键朱元璋（株连百官，革职削籍）".to_owned(),
            ],
            ChooseKind::Redraft(task) => self
                .redraft_candidates(task)
                .into_iter()
                .map(|(_, label)| label)
                .collect(),
            ChooseKind::Template => self
                .court()
                .templates()
                .into_iter()
                .map(|(name, t, builtin)| {
                    let origin = if builtin { "内置" } else { "自定" };
                    format!("{name}（{origin}·验收{}条）", t.criteria.len())
                })
                .collect(),
            ChooseKind::Nominate => nominable(self.court())
                .iter()
                .map(|o| format!("{}（{}）", o.label(), o.rank(mode)))
                .collect(),
            ChooseKind::Lots => drawable(self.court())
                .iter()
                .map(|o| {
                    let n = self.court().nomination_tally(*o).len();
                    format!("{}（{n} 人得票）", o.label())
                })
                .collect(),
        }
    }
}
