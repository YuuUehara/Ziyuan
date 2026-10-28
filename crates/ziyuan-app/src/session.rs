//! 会话的开启、回放与提交：所有状态变更都经 `commit` 写入事件簿。

use std::sync::OnceLock;
use ziyuan_core::{CourtEvent, TaskId};
use ziyuan_governance::{Court, GovernanceError, founding_events};
use ziyuan_storage::{EventStore, StorageError};

use crate::huidian::{huidian_tag, read_huidian};
use crate::time::{last_clock, now};
use crate::{AppError, Session};

/// 每次调用都不同的随机种子（标准库的 `RandomState` 由系统随机数初始化）。
pub(crate) fn random_seed() -> u64 {
    use std::hash::{BuildHasher, RandomState};
    RandomState::new().hash_one(std::time::SystemTime::now())
}

impl<S: EventStore> Session<S> {
    /// 在空事件簿上建立默认朝廷（丞相制）。并发 init 只有一个成功。
    pub fn found(mut store: S) -> Result<Self, AppError> {
        let mut history = founding_events(random_seed());
        if store.stamps_time() {
            history.insert(1, CourtEvent::Clock(now()));
        }
        let court = Court::replay(&history)?;
        match store.append(0, &history) {
            Err(StorageError::Conflict { .. }) => return Err(AppError::AlreadyFounded),
            result => result?,
        }
        Ok(Self::replayed(store, court, history))
    }

    /// 由整本事件簿重放出的会话（全部事件已在手）。
    pub(crate) fn replayed(store: S, court: Court, history: Vec<CourtEvent>) -> Self {
        Self {
            store,
            court,
            len: history.len(),
            clock: last_clock(&history, None),
            history: OnceLock::from(history),
            huidian_at: 0,
            chronicle: None,
        }
    }

    /// 打开已有朝廷：有会典则从会典接着回放其后的事件，否则从头回放整本事件簿。
    /// 会典用不得（他版所修、与事件簿对不上、损坏）时以事件簿为准，并重修会典。
    pub fn open(store: S) -> Result<Self, AppError> {
        let (rejected, from_huidian) = match store.load_snapshot(&huidian_tag()) {
            Ok(None) => (None, None),
            Ok(Some(snapshot)) => match read_huidian(snapshot.data) {
                Ok(huidian) => (None, Some((snapshot.events, huidian))),
                Err(e) => (Some(e.to_string()), None),
            },
            Err(reason) => (Some(reason), None),
        };
        let mut session = match from_huidian {
            Some((events, huidian)) => match store.load_since(events) {
                Ok(tail) => {
                    let mut court = huidian.court;
                    for event in &tail {
                        court.apply(event)?;
                    }
                    Self {
                        store,
                        court,
                        len: events + tail.len(),
                        clock: last_clock(&tail, huidian.clock),
                        history: OnceLock::new(),
                        huidian_at: events,
                        chronicle: None,
                    }
                }
                // 事件簿比会典短：会典不是这本事件簿的。
                Err(StorageError::Conflict { .. }) => {
                    let history = store.load()?;
                    if history.is_empty() {
                        return Err(AppError::NotFounded);
                    }
                    let court = Court::replay(&history)?;
                    let mut session = Self::replayed(store, court, history);
                    session.huidian_at = usize::MAX;
                    session
                }
                Err(e) => return Err(e.into()),
            },
            None => {
                let history = store.load()?;
                if history.is_empty() {
                    return Err(AppError::NotFounded);
                }
                let court = Court::replay(&history)?;
                Self::replayed(store, court, history)
            }
        };
        if let Some(reason) = rejected {
            session.chronicle = Some(format!(
                "史官记：会典与祖制不符（{reason}），以实录为准，已自开国重读。"
            ));
            session.huidian_at = usize::MAX;
        }
        session.revise_huidian();
        Ok(session)
    }

    pub fn court(&self) -> &Court {
        &self.court
    }

    /// 全部事件。从会典打开的会话在第一次调用时才读入整本事件簿（之后随提交与重载追加），
    /// 因此只看朝廷现状的命令不必解码全部事件。
    pub fn history(&self) -> Result<&[CourtEvent], AppError> {
        if let Some(history) = self.history.get() {
            return Ok(history);
        }
        let mut all = self.store.load()?;
        if all.len() < self.len {
            return Err(StorageError::Conflict {
                expected: self.len,
                actual: all.len(),
            }
            .into());
        }
        // 读的同时别的进程可能又写了：只取与朝廷状态一致的前 `len` 条，其余由下次重载补上。
        all.truncate(self.len);
        Ok(self.history.get_or_init(|| all))
    }

    /// 一件诏令的指派与工具调用（复核所需）。整本事件簿已在手就从中取，
    /// 否则逐条读过只留这几条，免得常朝自动复核把整本事件簿留在内存里。
    pub(crate) fn task_events(&self, task: TaskId) -> Result<Vec<CourtEvent>, AppError> {
        let keep = |e: &CourtEvent| match e {
            CourtEvent::TaskAssigned { task: t, .. } | CourtEvent::ToolInvoked { task: t, .. } => {
                *t == task
            }
            _ => false,
        };
        Ok(match self.history.get() {
            Some(history) => history.iter().filter(|e| keep(e)).cloned().collect(),
            None => self.store.load_where(self.len, &keep)?,
        })
    }

    /// 取走史官对修会典的记述（修成或弃用旧会典时才有）。
    pub fn take_chronicle(&mut self) -> Option<String> {
        self.chronicle.take()
    }

    /// 事件簿中的事件条数。
    pub fn event_count(&self) -> usize {
        self.len
    }

    /// 应用已写入事件簿的 `events`（自己提交的或其他进程写的）。
    pub(crate) fn extend(&mut self, court: Court, events: Vec<CourtEvent>) {
        self.court = court;
        self.len += events.len();
        self.clock = last_clock(&events, self.clock);
        if let Some(history) = self.history.get_mut() {
            history.extend(events);
        }
    }

    /// 重新加载事件簿，看到其他进程（或会话）写入的变更：只读取并应用新增的事件。
    /// 事件簿比已知的短（被替换过）时从头重放。
    pub fn reload(&mut self) -> Result<(), AppError> {
        let fresh = match self.store.load_since(self.len) {
            Ok(fresh) => fresh,
            Err(StorageError::Conflict { .. }) => {
                let history = self.store.load()?;
                self.court = Court::replay(&history)?;
                self.len = history.len();
                self.clock = last_clock(&history, None);
                self.history = OnceLock::from(history);
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        if fresh.is_empty() {
            return Ok(());
        }
        // 先在副本上应用，出错时会话保持原状。
        let mut court = self.court.clone();
        for event in &fresh {
            court.apply(event)?;
        }
        self.extend(court, fresh);
        Ok(())
    }

    /// 由 `propose` 基于当前状态生成事件，校验后写入。事件簿若已被其他进程更新，
    /// 则重新加载并重新生成，因此 ID 分配与规则检查总是基于最新状态。返回实际写入的事件。
    pub(crate) fn commit(
        &mut self,
        mut propose: impl FnMut(&Court) -> Result<Vec<CourtEvent>, GovernanceError>,
    ) -> Result<Vec<CourtEvent>, AppError> {
        loop {
            let events = propose(&self.court)?;
            if events.is_empty() {
                return Ok(events);
            }
            let mut next = self.court.clone();
            for event in &events {
                next.apply(event)?;
            }
            // 分钟变了才记一条时辰，置于本批之前。
            let mut written = Vec::with_capacity(events.len() + 1);
            if self.store.stamps_time() {
                let at = now();
                if self.clock.is_none_or(|t| t / 60 != at / 60) {
                    written.push(CourtEvent::Clock(at));
                }
            }
            written.extend(events.iter().cloned());
            match self.store.append(self.len, &written) {
                Ok(()) => {
                    self.extend(next, written);
                    self.revise_huidian();
                    return Ok(events);
                }
                Err(StorageError::Conflict { .. }) => self.reload()?,
                Err(e) => return Err(e.into()),
            }
        }
    }
}
