//! 紫禁城终端界面（TUI）：在终端里查看朝廷、下诏、执行、评定与任免。
//!
//! 与 CLI、Web 一样只是 [`Session`] 的一个前端：写入以乐观并发提交，执行诏令前取得执行者锁，
//! 并定时重新加载事件簿，因此能看到其他 ziyuan 进程的变更。
//!
//! [`App`] 不依赖真实终端：按键经 [`App::handle`] 处理，画面由 [`App::draw`] 绘制，
//! 测试用 ratatui 的 `TestBackend` 即可驱动。
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Tabs, Wrap,
};
use ratatui::{DefaultTerminal, Frame};
use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use ziyuan_app::{
    ABOLITION_NOTE, AppError, RunUpdate, Session, describe_event, describe_record, duzhi_report,
    estimate_cost, kaocheng_report, render_diff, seal_note,
};
use ziyuan_core::{
    AgentId, BudgetScope, CensorVerdict, Department, GovernanceMode, Office, Persona, Seal, Speech,
    TaskId, Verdict,
};
use ziyuan_governance::{ChangeState, Court, Task, TaskStatus};
use ziyuan_runtime::{CancelToken, Limits, Outcome, Scheduler, WorkerBackend};
use ziyuan_storage::EventLog;

pub struct Config {
    /// 朝廷所在目录（含 `.ziyuan/`）。
    pub root: PathBuf,
    pub backend: Arc<dyn WorkerBackend>,
    pub limits: Limits,
}

/// 接管终端运行界面，直到用户退出。
pub async fn run(config: Config) -> Result<(), Box<dyn Error>> {
    let (mut app, mut messages) = App::new(config)?;
    // crossterm 的读取是阻塞的；放在独立线程，退出时不必等它。
    let input = app.sender.clone();
    std::thread::spawn(move || {
        while let Ok(event) = event::read() {
            if input.send(Message::Input(event)).is_err() {
                break;
            }
        }
    });
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &mut messages).await;
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    messages: &mut UnboundedReceiver<Message>,
) -> Result<(), Box<dyn Error>> {
    // 定时重新加载，看到其他进程的变更。
    let mut refresh = tokio::time::interval(Duration::from_secs(1));
    while !app.quit {
        terminal.draw(|frame| app.draw(frame))?;
        tokio::select! {
            Some(message) = messages.recv() => app.handle(message),
            _ = refresh.tick() => app.reload(),
        }
    }
    if let Some(cancel) = &app.run {
        cancel.cancel();
    }
    Ok(())
}

/// 界面收到的消息：终端输入，或后台执行的进度。
pub enum Message {
    Input(Event),
    Assigned(TaskId),
    Text(TaskId, String),
    Finished {
        task: TaskId,
        outcome: Outcome,
    },
    /// 后台执行结束；出错时带上错误。
    RunDone(Option<String>),
    /// 后台内阁票拟结束：拟派的官员，或错误。
    Drafted(TaskId, Result<AgentId, String>),
    /// 后台尚书分办结束：子诏令数，或错误。
    Split(TaskId, Result<usize, String>),
    /// 后台都察院复核结束：结论（背书/弹劾），或错误。
    Censured(TaskId, Result<&'static str, String>),
    /// 廷议中一段发言（已排好「官职姓名（人格）：内容」）。
    Speech(TaskId, String),
    /// 后台廷议结束：中止原因（`None` 为议毕），或错误。
    Debated(TaskId, Result<Option<String>, String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Edicts,
    Officials,
    History,
}
const TABS: [(Tab, &str); 3] = [
    (Tab::Edicts, "诏令"),
    (Tab::Officials, "百官"),
    (Tab::History, "起居注"),
];

enum Modal {
    /// 单行输入；`department` 只用于下诏与招募。
    Input {
        kind: InputKind,
        text: String,
        department: usize,
    },
    Choose {
        kind: ChooseKind,
        selected: usize,
    },
    /// 不可逆操作的确认：`y` 确认，其他键取消。
    Confirm(ConfirmKind),
    /// 史官记：任意键关闭。
    Notice(String),
}

#[derive(Clone, Copy)]
enum ConfirmKind {
    Purge(AgentId),
}

#[derive(Clone, Copy)]
enum InputKind {
    Edict,
    Recruit,
    Review(TaskId, Verdict),
}

#[derive(Clone, Copy)]
enum ChooseKind {
    Reform,
    Appoint(AgentId),
    /// 御笔改票：从本部在任官员中改选承办人。
    Redraft(TaskId),
    /// 丞相制下废相：罢免丞相，或一键朱元璋（革职削籍）。
    Abolish,
}

/// 现行制度下 `agent` 可任的官职：本制度的协调官职，以及本部（专长所在部门）的官职。
fn appointable(court: &Court, agent: AgentId) -> Vec<Office> {
    let speciality = court.agent(agent).map(|a| a.speciality);
    Office::all()
        .into_iter()
        .filter(|o| o.exists_in(court.mode()))
        .filter(|o| match o {
            Office::Minister(d) | Office::ViceMinister(d) | Office::Clerk(d) => {
                Some(*d) == speciality
            }
            _ => true,
        })
        .collect()
}

pub struct App {
    config: Config,
    /// 承办官员所用的模型；演示后端为 `None`。
    model: Option<String>,
    session: Session<EventLog>,
    sender: UnboundedSender<Message>,
    tab: Tab,
    /// 选中的诏令；列表按新到旧排列。
    selected_task: Option<TaskId>,
    selected_official: usize,
    /// `None` 表示跟随最新事件。
    selected_event: Option<usize>,
    /// 详情面板的滚动行数（执行中的诏令自动跟随末尾）。
    detail_scroll: u16,
    modal: Option<Modal>,
    /// 最近一次操作的结果或错误。
    status: Option<(String, bool)>,
    /// 本进程正在进行的执行。
    run: Option<CancelToken>,
    /// 执行中诏令的流式输出（不入事件簿）。
    live: HashMap<TaskId, String>,
    /// 廷议进行中已收到的发言（议毕后以事件簿为准）。
    debating: HashMap<TaskId, Vec<String>>,
    quit: bool,
}

impl App {
    pub fn new(config: Config) -> Result<(Self, UnboundedReceiver<Message>), AppError> {
        let session = Session::open(EventLog::new(&config.root))?;
        let (sender, receiver) = unbounded_channel();
        let selected_task = session.court().tasks().last().map(|t| t.edict.id);
        let app = Self {
            model: config.backend.model_name().map(str::to_owned),
            config,
            session,
            sender,
            tab: Tab::Edicts,
            selected_task,
            selected_official: 0,
            selected_event: None,
            detail_scroll: 0,
            modal: None,
            status: None,
            run: None,
            live: HashMap::new(),
            debating: HashMap::new(),
            quit: false,
        };
        Ok((app, receiver))
    }

    fn court(&self) -> &Court {
        self.session.court()
    }

    fn info(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), false));
    }

    fn error(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), true));
    }

    pub fn reload(&mut self) {
        if let Err(e) = self.session.reload() {
            self.error(e.to_string());
        }
        if self.selected_task.is_none() {
            self.selected_task = self.court().tasks().last().map(|t| t.edict.id);
        }
    }

    /// 执行一个变更；成功时显示 `done`，失败时显示错误。
    fn mutate<T>(
        &mut self,
        f: impl FnOnce(&mut Session<EventLog>) -> Result<T, AppError>,
        done: impl FnOnce(&T) -> String,
    ) -> Option<T> {
        match f(&mut self.session) {
            Ok(value) => {
                self.info(done(&value));
                Some(value)
            }
            Err(e) => {
                self.error(e.to_string());
                None
            }
        }
    }

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
            Message::Censured(task, result) => {
                self.reload();
                match result {
                    Ok(label) => self.info(format!("诏令 #{} 都察院{label}", task.0)),
                    Err(e) => self.error(format!("复核失败：{e}")),
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

    fn key(&mut self, key: KeyEvent) {
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

    fn edict_key(&mut self, code: KeyCode) {
        let Some(task) = self.selected_task else {
            return;
        };
        match code {
            KeyCode::Char('c') => {
                self.mutate(|s| s.cancel(task), |_| format!("诏令 #{} 已撤销", task.0));
            }
            KeyCode::Char('t') => self.start_draft(task),
            KeyCode::Char('i') => self.start_censor(task),
            KeyCode::Char('f') => self.start_split(task),
            KeyCode::Char('v') => self.start_debate(task),
            KeyCode::Char('w') => {
                let root = self.config.root.clone();
                self.mutate(
                    |s| s.apply_changes(task, &root),
                    |_| format!("诏令 #{} 准奏，改动已写入项目", task.0),
                );
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

    fn official_key(&mut self, code: KeyCode) {
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
            KeyCode::Char('d') => {
                self.mutate(
                    |s| s.dismiss(agent),
                    |_| format!("官员 #{} 已罢免现职", agent.0),
                );
            }
            _ => {}
        }
    }

    fn modal_key(&mut self, key: KeyEvent) {
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

    fn submit_input(&mut self) {
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
            InputKind::Review(task, verdict) => {
                self.mutate(
                    |s| s.review(task, verdict, text),
                    |_| format!("诏令 #{} 评定为{}", task.0, verdict.label()),
                );
            }
        }
    }

    fn confirm(&mut self, kind: ConfirmKind) {
        match kind {
            ConfirmKind::Purge(agent) => {
                self.mutate(
                    |s| s.purge(agent),
                    |_| format!("官员 #{} 革职，削去仕籍，永不叙用", agent.0),
                );
            }
        }
    }

    fn submit_choice(&mut self) {
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
                self.mutate(
                    |s| s.appoint(agent, office),
                    |_| format!("任命官员 #{} 为{}", agent.0, office.label()),
                );
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

    /// 改票可选的官员：本部在任的尚书、侍郎、郎中。
    fn redraft_candidates(&self, task: TaskId) -> Vec<(AgentId, String)> {
        let court = self.court();
        let Some(entry) = court.task(task) else {
            return Vec::new();
        };
        court
            .draft_candidates(entry.edict.department)
            .iter()
            .map(|(a, o)| (a.id, format!("{}（{}）", a.name, o.label())))
            .collect()
    }

    /// 在阻塞线程中请尚书分办（会等待模型返回），结果经消息送回。
    fn start_split(&mut self, task: TaskId) {
        self.info(format!("诏令 #{} 尚书分办中…", task.0));
        let root = self.config.root.clone();
        let backend = self.config.backend.clone();
        let sender = self.sender.clone();
        tokio::task::spawn_blocking(move || {
            let result = Session::open(EventLog::new(&root))
                .and_then(|mut s| s.plan_split(task, backend.as_ref()))
                .map(|subtasks| subtasks.len())
                .map_err(|e| e.to_string());
            let _ = sender.send(Message::Split(task, result));
        });
    }

    /// 在阻塞线程中廷议（默认人选，两轮；会等待全部发言），发言与结果经消息送回。
    fn start_debate(&mut self, task: TaskId) {
        if self.debating.contains_key(&task) {
            return;
        }
        self.info(format!("诏令 #{} 廷议中…", task.0));
        self.debating.insert(task, Vec::new());
        let root = self.config.root.clone();
        let backend = self.config.backend.clone();
        let sender = self.sender.clone();
        tokio::task::spawn_blocking(move || {
            let result = Session::open(EventLog::new(&root))
                .and_then(|mut s| {
                    s.debate(task, &[], 2, backend.as_ref(), |agent, speech| {
                        let line = speech_line(&agent.name, agent.id, speech);
                        let _ = sender.send(Message::Speech(task, line));
                    })
                })
                .map_err(|e| e.to_string());
            let _ = sender.send(Message::Debated(task, result));
        });
    }

    /// 在阻塞线程中请都察院复核（会等待御史的模型返回），结果经消息送回。
    fn start_censor(&mut self, task: TaskId) {
        self.info(format!("诏令 #{} 都察院复核中…", task.0));
        let root = self.config.root.clone();
        let backend = self.config.backend.clone();
        let sender = self.sender.clone();
        tokio::task::spawn_blocking(move || {
            let result = Session::open(EventLog::new(&root))
                .and_then(|mut s| s.censor(task, &root, backend.as_ref()))
                .map(|v| v.label())
                .map_err(|e| e.to_string());
            let _ = sender.send(Message::Censured(task, result));
        });
    }

    /// 在阻塞线程中请内阁票拟（会等待模型返回），结果经消息送回。
    fn start_draft(&mut self, task: TaskId) {
        self.info(format!("诏令 #{} 内阁票拟中…", task.0));
        let root = self.config.root.clone();
        let backend = self.config.backend.clone();
        let sender = self.sender.clone();
        tokio::task::spawn_blocking(move || {
            let result = Session::open(EventLog::new(&root))
                .and_then(|mut s| s.draft(task, backend.as_ref()))
                .map_err(|e| e.to_string());
            let _ = sender.send(Message::Drafted(task, result));
        });
    }

    fn options(&self, kind: ChooseKind) -> Vec<String> {
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
        }
    }

    fn switch_tab(&mut self, step: usize) {
        let index = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
        self.tab = TABS[(index + step) % TABS.len()].0;
    }

    /// 新到旧排列的诏令。
    fn task_order(&self) -> Vec<&Task> {
        self.court().tasks().rev().collect()
    }

    fn move_selection(&mut self, delta: isize) {
        let step = |index: usize, len: usize| {
            index
                .saturating_add_signed(delta)
                .min(len.saturating_sub(1))
        };
        match self.tab {
            Tab::Edicts => {
                let order: Vec<TaskId> = self.task_order().iter().map(|t| t.edict.id).collect();
                let index = order
                    .iter()
                    .position(|&id| Some(id) == self.selected_task)
                    .unwrap_or(0);
                self.selected_task = order.get(step(index, order.len())).copied();
                self.detail_scroll = 0;
            }
            Tab::Officials => {
                self.selected_official =
                    step(self.selected_official, self.court().agents().count());
            }
            Tab::History => {
                let len = self.session.history().len();
                let index = self.selected_event.unwrap_or(len.saturating_sub(1));
                let next = step(index, len);
                self.selected_event = (next + 1 < len).then_some(next);
            }
        }
    }

    /// 在后台执行全部未结束的诏令（含执行期间新入队的）。
    fn start_run(&mut self) {
        if self.run.is_some() {
            return self.error("已在执行中");
        }
        let lock = match EventLog::new(&self.config.root).try_lock_executor() {
            Ok(Some(lock)) => lock,
            Ok(None) => return self.error("另一个 ziyuan 进程正在执行诏令，请待其结束"),
            Err(e) => return self.error(e.to_string()),
        };
        if self.court().runnable_tasks().is_empty() {
            return if self.court().open_tasks().is_empty() {
                self.info("没有未结束的诏令")
            } else {
                self.error("诏令尚待批红：内阁制下须先按 b 朱笔批红")
            };
        }
        let cancel = CancelToken::new();
        self.run = Some(cancel.clone());
        self.info("执行中…（按 s 停止）");
        let scheduler = Scheduler::new(self.config.backend.clone(), self.config.limits);
        let root = self.config.root.clone();
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let _lock = lock;
            let result = async {
                let mut session = Session::open(EventLog::new(&root))?;
                session
                    .run_open(&scheduler, &cancel, |update| {
                        let _ = sender.send(match update {
                            RunUpdate::Assigned(task) => Message::Assigned(task),
                            RunUpdate::Text { task, text } => Message::Text(task, text.to_owned()),
                            RunUpdate::Finished(report) => Message::Finished {
                                task: report.edict.id,
                                outcome: report.outcome.clone(),
                            },
                        });
                    })
                    .await
            }
            .await;
            let _ = sender.send(Message::RunDone(result.err().map(|e| e.to_string())));
        });
    }

    fn stop_run(&mut self) {
        match &self.run {
            Some(cancel) => {
                cancel.cancel();
                self.info("正在停止：撤销执行中的诏令，排队中的保留");
            }
            None => self.error("当前没有执行"),
        }
    }

    // ---- 绘制 ----

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
    }

    /// 按各诏令实际所用模型估算的花费；查不到单价时为空。
    fn cost_note<'a>(&self, tasks: impl IntoIterator<Item = &'a Task>) -> String {
        let backend = &self.config.backend;
        estimate_cost(tasks, self.model.as_deref(), |m| backend.price(m))
            .map_or(String::new(), |c| format!("（{c}）"))
    }

    fn model_label(&self) -> &str {
        self.model.as_deref().unwrap_or("演示")
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let court = self.court();
        let usage = court.total_usage();
        let mut spans = vec![
            Span::styled(
                " 紫垣 · 奉天门 ",
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

    fn draw_edicts(&self, frame: &mut Frame, area: Rect) {
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
    fn task_detail(&self, task: &Task) -> (Text<'static>, bool) {
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
        if !task.suspicions.is_empty() {
            lines.push(Line::default());
            lines.push(Line::styled(
                "刑部与锦衣卫密报（关键词检出，可能误报）",
                Style::new().fg(Color::Red).bold(),
            ));
            for s in &task.suspicions {
                let source = s.source.strip_prefix("tool:").unwrap_or("回奏");
                lines.push(Line::from(format!(
                    "  {}（{source}）{}：「{}」",
                    s.bureau(),
                    s.threat.label(),
                    s.excerpt
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
                ChangeState::Proposed => "待准奏（w 准奏落盘，u 驳回）",
                ChangeState::Applied => "已落盘",
                ChangeState::Discarded => "已驳回",
            };
            lines.push(Line::default());
            lines.push(Line::from(vec![
                Span::styled("工部奏本　", Style::new().fg(GOLD).bold()),
                Span::raw(format!("改动 {} 个文件，{state}", changes.files.len())),
            ]));
            for file in &changes.files {
                for line in render_diff(file).lines() {
                    let style = match line.chars().next() {
                        Some('+') if !line.starts_with("+++") => Style::new().fg(Color::Green),
                        Some('-') if !line.starts_with("---") => Style::new().fg(Color::Red),
                        Some('@') => Style::new().dim(),
                        _ => Style::new(),
                    };
                    lines.push(Line::styled(line.to_owned(), style));
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

    fn draw_officials(&self, frame: &mut Frame, area: Rect) {
        let court = self.court();
        let [table_area, routes_area] = Layout::vertical([
            Constraint::Min(5),
            Constraint::Length(Department::ALL.len() as u16 + 2),
        ])
        .areas(area);
        let rows: Vec<Row> = court
            .agents()
            .map(|a| {
                let office = court
                    .office_of(a.id)
                    .map(|o| format!("{}·{}", o.label(), o.rank(court.mode())));
                let record = court.service_record(a.id);
                Row::new(vec![
                    Cell::from(format!("#{}", a.id.0)).dim(),
                    Cell::from(a.name.clone()),
                    match office {
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
            .collect();
        frame.render_widget(
            Paragraph::new(routes).block(panel(format!("派工路线 · {}", court.mode().label()))),
            routes_area,
        );
    }

    fn draw_history(&self, frame: &mut Frame, area: Rect) {
        let history = self.session.history();
        let items: Vec<ListItem> = history
            .iter()
            .enumerate()
            .map(|(i, e)| {
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:>4} ", i + 1), Style::new().dim()),
                    Span::raw(describe_event(e)),
                ]))
            })
            .collect();
        let selected = self
            .selected_event
            .unwrap_or(history.len().saturating_sub(1));
        let mut state = ListState::default().with_selected(Some(selected));
        frame.render_stateful_widget(
            List::new(items)
                .block(panel(format!("起居注（{} 条事件）", history.len())))
                .highlight_style(Style::new().bg(Color::DarkGray)),
            area,
            &mut state,
        );
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
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
            (Some(Modal::Input { .. }), _) => {
                &[("Enter", "确定"), ("Tab", "换部门"), ("Esc", "取消")]
            }
            (Some(Modal::Choose { .. }), _) => {
                &[("↑↓", "选择"), ("Enter", "确定"), ("Esc", "取消")]
            }
            (Some(Modal::Confirm(_)), _) => &[("y", "确认"), ("其他键", "取消")],
            (Some(Modal::Notice(_)), _) => &[("任意键", "退朝")],
            (None, Tab::Edicts) => &[
                ("n", "下诏"),
                ("r", "执行"),
                ("s", "停止"),
                ("c", "撤销"),
                ("a/d", "采纳/驳回"),
                ("t/e", "票拟/改票"),
                ("b", "批红"),
                ("w/u", "准奏/驳回改动"),
                ("f", "分办"),
                ("v", "廷议"),
                ("i", "复核"),
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

    fn draw_modal(&self, frame: &mut Frame, modal: &Modal) {
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
                };
                let title = match kind {
                    InputKind::Review(task, _) => format!("{title} · 诏令 #{}", task.0),
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

const VERMILION: Color = Color::Rgb(178, 34, 34);
const GOLD: Color = Color::Rgb(218, 165, 32);

/// 廷议发言的一行：「官职姓名（人格）：内容」。
fn speech_line(name: &str, speaker: AgentId, speech: &Speech) -> String {
    let persona = Persona::of(speaker).label();
    format!(
        "{}{name}（{persona}）：{}",
        speech.office.label(),
        speech.text
    )
}

fn panel(title: String) -> Block<'static> {
    Block::bordered()
        .border_style(Style::new().fg(VERMILION))
        .title(Span::styled(
            format!(" {title} "),
            Style::new().fg(GOLD).bold(),
        ))
}

fn modal_block(title: String) -> Block<'static> {
    panel(title).border_type(ratatui::widgets::BorderType::Double)
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn status_badge(court: &Court, task: &Task) -> (String, Style) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ziyuan_runtime::DemoBackend;

    fn temp_app(name: &str) -> (App, UnboundedReceiver<Message>, PathBuf) {
        let root = std::env::temp_dir().join(format!("ziyuan-tui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Session::found(EventLog::new(&root)).unwrap();
        let (app, messages) = App::new(Config {
            root: root.clone(),
            backend: Arc::new(DemoBackend),
            limits: Limits {
                max_concurrent: 2,
                timeout: Duration::from_secs(10),
            },
        })
        .unwrap();
        (app, messages, root)
    }

    fn press(app: &mut App, keys: &str) {
        for c in keys.chars() {
            let code = match c {
                '\n' => KeyCode::Enter,
                '\t' => KeyCode::Tab,
                '↓' => KeyCode::Down,
                '⎋' => KeyCode::Esc,
                c => KeyCode::Char(c),
            };
            app.handle(Message::Input(Event::Key(KeyEvent::from(code))));
        }
    }

    fn screen(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                // 宽字符（汉字）之后的占位格不输出。
                let mut line = String::new();
                let mut x = 0;
                while x < buffer.area.width {
                    let symbol = buffer[(x, y)].symbol();
                    line.push_str(symbol);
                    x += Span::raw(symbol).width().max(1) as u16;
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 等待后台执行结束，期间把进度交给界面。
    async fn finish_run(app: &mut App, messages: &mut UnboundedReceiver<Message>) {
        loop {
            let message = tokio::time::timeout(Duration::from_secs(10), messages.recv())
                .await
                .expect("执行应在 10 秒内结束")
                .unwrap();
            let done = matches!(message, Message::RunDone(_));
            app.handle(message);
            if done {
                return;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn issue_run_and_review_from_the_keyboard() {
        let (mut app, mut messages, root) = temp_app("flow");
        // 下诏时 Tab 从工部切到下一个部门（吏部之后依次为户、礼、兵、刑、工；工部后回到吏部）。
        press(&mut app, "n\t查 unsafe\n");
        let task = app.court().task(TaskId(1)).expect("应已下诏");
        assert_eq!(task.edict.department, Department::Personnel);
        assert!(screen(&app).contains("诏令 #1 已入队"));

        // v 廷议：发言陆续显示在详情里；演示后端不调用模型，廷议失败、不留纪要。
        press(&mut app, "v");
        let line = "工部尚书某（刚直·简劲）：臣以为可".to_owned();
        app.handle(Message::Speech(TaskId(1), line));
        let shown = screen(&app);
        assert!(
            shown.contains("廷议进行中") && shown.contains("臣以为可"),
            "{shown}"
        );
        let message = tokio::time::timeout(Duration::from_secs(10), messages.recv())
            .await
            .expect("廷议应在 10 秒内结束")
            .unwrap();
        assert!(matches!(message, Message::Debated(TaskId(1), Err(_))));
        app.handle(message);
        let shown = screen(&app);
        assert!(
            shown.contains("廷议失败") && !shown.contains("臣以为可"),
            "{shown}"
        );
        assert!(app.court().task(TaskId(1)).unwrap().debates.is_empty());

        // 另一个进程持有执行者锁时拒绝执行，诏令仍在队列。
        let held = EventLog::new(&root).try_lock_executor().unwrap().unwrap();
        press(&mut app, "r");
        assert!(app.run.is_none());
        assert!(screen(&app).contains("另一个 ziyuan 进程"));
        drop(held);

        press(&mut app, "r");
        finish_run(&mut app, &mut messages).await;
        let shown = screen(&app);
        assert!(
            shown.contains("待评定") && shown.contains("DEMO ONLY"),
            "{shown}"
        );

        press(&mut app, "a属实\n");
        let task = app.court().task(TaskId(1)).unwrap();
        assert_eq!(task.review, Some((Verdict::Accepted, "属实".into())));
        // 已评定的回奏不能再评定，错误显示在状态栏。
        press(&mut app, "d\n");
        assert!(screen(&app).contains("已评定"));

        // i 请都察院复核：御史空缺，错误经后台消息显示在状态栏。
        press(&mut app, "i");
        let message = tokio::time::timeout(Duration::from_secs(10), messages.recv())
            .await
            .expect("复核应在 10 秒内结束")
            .unwrap();
        assert!(matches!(message, Message::Censured(TaskId(1), Err(_))));
        app.handle(message);
        assert!(screen(&app).contains("左都御史空缺"), "{}", screen(&app));
        // K 考成簿：承办官员一件采纳，等第平常。
        // D 修道：史官记弹出，标题栏标出修道中；再按 D 回朝。
        press(&mut app, "D");
        assert!(app.court().in_retreat() && screen(&app).contains("西苑"));
        press(&mut app, " ");
        assert!(screen(&app).contains("西苑修道中"));
        press(&mut app, "D ");
        assert!(!app.court().in_retreat());
        // H 度支奏销；设了朝廷总额后标题栏显示余额。
        app.session
            .set_budget(BudgetScope::Court, Some(9000))
            .unwrap();
        press(&mut app, "H");
        let shown = screen(&app);
        assert!(
            shown.contains("度支奏销") && shown.contains("度支余 9000/9000"),
            "{shown}"
        );
        press(&mut app, " ");
        press(&mut app, "K");
        let shown = screen(&app);
        assert!(
            shown.contains("考成簿") && shown.contains("平常"),
            "{shown}"
        );
        press(&mut app, " ");
        assert!(app.modal.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reform_and_appointments_follow_court_rules() {
        let (mut app, mut messages, root) = temp_app("court");
        // 改为内阁制（第三项）：首辅空缺，派工路线显示空缺，下诏被拒且不留记录。
        press(&mut app, "g↓↓\n");
        assert!(screen(&app).contains("四叔来看你了"));
        press(&mut app, " 2");
        assert_eq!(app.court().mode(), GovernanceMode::Cabinet);
        assert!(screen(&app).contains("内阁首辅、次辅俱缺"));
        press(&mut app, "1n无首辅\n");
        assert!(app.court().tasks().next().is_none());
        assert!(screen(&app).contains("俱缺"));

        // 在百官页招募新官（留空姓名则随机取名），任命为首辅后即可下诏。
        press(&mut app, "2n\n");
        let id = AgentId(app.court().agents().count() as u64);
        let name = app.court().agent(id).unwrap().name.clone();
        assert!(screen(&app).contains(&format!("已招募官员 #{} {name}", id.0)));
        for _ in 0..id.0 {
            press(&mut app, "↓");
        }
        // 内阁制下可任的官职只有内阁与本部（工部）官职，首辅为第一项；品级随制度显示。
        press(&mut app, "p");
        let shown = screen(&app);
        assert!(shown.contains("内阁首辅（正五品）") && shown.contains("工部尚书（正二品）"));
        assert!(
            !shown.contains("丞相") && !shown.contains("吏部尚书（"),
            "{shown}"
        );
        press(&mut app, "\n");
        assert_eq!(app.court().office_of(id), Some(Office::FirstGrandSecretary));
        press(&mut app, "1n内阁承办\n");
        assert!(app.court().task(TaskId(1)).is_some());

        // 内阁制：新诏令待批红，批红后方可执行。
        let shown = screen(&app);
        assert!(
            shown.contains("待批红") && shown.contains("内阁票拟"),
            "{shown}"
        );
        press(&mut app, "r");
        assert!(app.run.is_none() && screen(&app).contains("须先按 b"));
        assert!(screen(&app).contains("尚未票拟"));

        // t 后台票拟（演示后端无模型，按旧例拟派并注明），e 御笔改票。
        press(&mut app, "t");
        let message = tokio::time::timeout(Duration::from_secs(10), messages.recv())
            .await
            .expect("票拟应在 10 秒内结束")
            .unwrap();
        assert!(matches!(message, Message::Drafted(TaskId(1), Ok(_))));
        app.handle(message);
        let drafted = app.court().task(TaskId(1)).unwrap().draft.clone().unwrap();
        assert!(screen(&app).contains("拟派"), "{}", screen(&app));
        let others = app.redraft_candidates(TaskId(1));
        let pick = others
            .iter()
            .position(|(id, _)| *id != drafted.worker)
            .unwrap_or(0);
        press(&mut app, "e");
        for _ in 0..pick {
            press(&mut app, "↓");
        }
        press(&mut app, "\n");
        let redrafted = app.court().task(TaskId(1)).unwrap().draft.clone().unwrap();
        assert_eq!(redrafted.worker, others[pick].0);
        assert!(redrafted.note.contains("御笔改票"));
        press(&mut app, "b");
        assert!(app.court().runnable_tasks() == [TaskId(1)]);

        // 改回丞相制：土木堡之变，史官记弹出，任意键关闭。
        press(&mut app, "g↓\n");
        assert_eq!(app.court().mode(), GovernanceMode::Chancellor);
        assert!(screen(&app).contains("大元，杀回来了"));
        press(&mut app, " ");
        assert!(app.modal.is_none());
        // 丞相制下改为亲政时选丞相的处置：Esc 取消不生效；选一键朱元璋则株连百官、改为亲政。
        press(&mut app, "g↓\n");
        assert!(screen(&app).contains("一键朱元璋（株连百官"));
        press(&mut app, "⎋");
        assert_eq!(app.court().mode(), GovernanceMode::Chancellor);
        press(&mut app, "g↓\n↓\n");
        assert_eq!(app.court().mode(), GovernanceMode::DirectRule);
        assert!(screen(&app).contains("胡惟庸") && screen(&app).contains("株连所及"));
        let court = app.court();
        assert!(court.agents().all(|a| court.office_of(a.id).is_none()));
        press(&mut app, " ");

        // 其他进程的写入在重新加载后可见。
        let mut other = Session::open(EventLog::new(&root)).unwrap();
        other.cancel(TaskId(1)).unwrap();
        app.reload();
        assert!(screen(&app).contains("已撤销"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
