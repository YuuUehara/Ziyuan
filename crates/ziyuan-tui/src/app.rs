//! 界面状态：标签页、弹窗、消息，以及重新加载与提交。

use ratatui::crossterm::event::Event;
use std::collections::HashMap;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use ziyuan_app::{AppError, Session};
use ziyuan_core::{AgentId, Office, TaskId, Verdict};
use ziyuan_governance::{Court, Task};
use ziyuan_runtime::Outcome;
use ziyuan_storage::EventLog;

use crate::{App, Config};

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
    /// 后台刑部复勘结束：(误报数, 维持数)，或错误。
    Rechecked(TaskId, Result<(usize, usize), String>),
    /// 廷议中一段发言（已排好「官职姓名（人格）：内容」）。
    Speech(TaskId, String),
    /// 后台廷议结束：中止原因（`None` 为议毕），或错误。
    Debated(TaskId, Result<Option<String>, String>),
    /// 后台礼部科举结束：各题「题名 分数」，或错误。
    Examined(AgentId, Result<Vec<String>, String>),
    /// 后台廷推结束：各票与得票（已排好），或错误。
    Nominated(Office, Result<String, String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Edicts,
    Officials,
    History,
}
pub(crate) const TABS: [(Tab, &str); 3] = [
    (Tab::Edicts, "诏令"),
    (Tab::Officials, "百官"),
    (Tab::History, "起居注"),
];

pub(crate) enum Modal {
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
pub(crate) enum ConfirmKind {
    Purge(AgentId),
    Cane(AgentId),
}

#[derive(Clone, Copy)]
pub(crate) enum InputKind {
    Edict,
    Recruit,
    Review(TaskId, Verdict),
    /// 逐段准奏：输入要驳回的段号。
    Skip(TaskId),
    /// 检索翰林院实录（空为最近各件）。
    Records,
}

#[derive(Clone, Copy)]
pub(crate) enum ChooseKind {
    Reform,
    Appoint(AgentId),
    /// 御笔改票：从本部在任官员中改选承办人。
    Redraft(TaskId),
    /// 丞相制下废相：罢免丞相，或一键朱元璋（革职削籍）。
    Abolish,
    /// 按翰林院模板下诏（入队）。
    Template,
    /// 廷推出缺之职。
    Nominate,
    /// 于廷推得票者中枚卜，径直任命。
    Lots,
}

/// 现行制度下 `agent` 可任的官职：本制度的协调官职，以及本部（专长所在部门）的官职。
/// 现行制度下可廷推（出缺且有九卿、有候选）的官职。
pub(crate) fn nominable(court: &Court) -> Vec<Office> {
    Office::all()
        .into_iter()
        .filter(|o| o.exists_in(court.mode()) && court.nomination_ballot(*o).is_ok())
        .collect()
}

/// 已有廷推、可枚卜的官职。
pub(crate) fn drawable(court: &Court) -> Vec<Office> {
    Office::all()
        .into_iter()
        .filter(|o| o.exists_in(court.mode()) && court.nomination(*o).is_some())
        .collect()
}

pub(crate) fn appointable(court: &Court, agent: AgentId) -> Vec<Office> {
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

impl App {
    pub fn new(config: Config) -> Result<(Self, UnboundedReceiver<Message>), AppError> {
        let mut session = Session::open(EventLog::new(&config.root))?;
        let chronicle = session.take_chronicle();
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
            status: chronicle.map(|note| (note, false)),
            run: None,
            live: HashMap::new(),
            debating: HashMap::new(),
            quit: false,
        };
        Ok((app, receiver))
    }

    pub(crate) fn court(&self) -> &Court {
        self.session.court()
    }

    pub(crate) fn info(&mut self, message: impl Into<String>) {
        self.status = Some((message.into(), false));
    }

    pub(crate) fn error(&mut self, message: impl Into<String>) {
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
    pub(crate) fn mutate<T>(
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

    /// 改票可选的官员：本部在任的尚书、侍郎、郎中。
    pub(crate) fn redraft_candidates(&self, task: TaskId) -> Vec<(AgentId, String)> {
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

    pub(crate) fn switch_tab(&mut self, step: usize) {
        let index = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
        self.tab = TABS[(index + step) % TABS.len()].0;
    }

    /// 新到旧排列的诏令。
    pub(crate) fn task_order(&self) -> Vec<&Task> {
        self.court().tasks().rev().collect()
    }

    pub(crate) fn move_selection(&mut self, delta: isize) {
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
                let len = self.history().len();
                let index = self.selected_event.unwrap_or(len.saturating_sub(1));
                let next = step(index, len);
                self.selected_event = (next + 1 < len).then_some(next);
            }
        }
    }
}
