//! 应用服务层：CLI 与未来的 UI 共用的用例入口。
//!
//! 每个用例都遵循同一流程：治理层提议事件 → 在 `Court` 副本上校验 → 写入事件簿 →
//! 更新内存状态。因此被拒绝的操作不会留下任何事件，写入的事件也一定能被回放。
use std::fmt;
use ziyuan_core::{AgentId, CourtEvent, Department, Edict, GovernanceMode, Office};
use ziyuan_governance::{Court, GovernanceError, founding_events};
use ziyuan_runtime::{DispatchError, ExecutionReport, WorkerBackend, dispatch};
use ziyuan_storage::{EventStore, StorageError};

#[derive(Debug)]
pub enum AppError {
    NotFounded,
    AlreadyFounded,
    Governance(GovernanceError),
    Storage(StorageError),
    Dispatch(DispatchError),
}
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFounded => f.write_str("朝廷尚未建立，请先在项目目录执行 `ziyuan init`"),
            Self::AlreadyFounded => f.write_str("朝廷已存在，拒绝覆盖日志"),
            Self::Governance(e) => e.fmt(f),
            Self::Storage(e) => e.fmt(f),
            Self::Dispatch(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotFounded | Self::AlreadyFounded => None,
            Self::Governance(e) => Some(e),
            Self::Storage(e) => Some(e),
            Self::Dispatch(e) => Some(e),
        }
    }
}
impl From<GovernanceError> for AppError {
    fn from(e: GovernanceError) -> Self {
        Self::Governance(e)
    }
}
impl From<StorageError> for AppError {
    fn from(e: StorageError) -> Self {
        Self::Storage(e)
    }
}
impl From<DispatchError> for AppError {
    fn from(e: DispatchError) -> Self {
        Self::Dispatch(e)
    }
}

/// 一个已打开的朝廷：事件簿与由它重放出的当前状态。
pub struct Session<S: EventStore> {
    store: S,
    court: Court,
    history: Vec<CourtEvent>,
}

impl<S: EventStore> Session<S> {
    /// 在空事件簿上建立默认朝廷（丞相制）。
    pub fn found(mut store: S) -> Result<Self, AppError> {
        if !store.load()?.is_empty() {
            return Err(AppError::AlreadyFounded);
        }
        let history = founding_events();
        let court = Court::replay(&history)?;
        store.append(&history)?;
        Ok(Self {
            store,
            court,
            history,
        })
    }

    /// 打开已有朝廷并重放其事件簿。
    pub fn open(store: S) -> Result<Self, AppError> {
        let history = store.load()?;
        if history.is_empty() {
            return Err(AppError::NotFounded);
        }
        let court = Court::replay(&history)?;
        Ok(Self {
            store,
            court,
            history,
        })
    }

    pub fn court(&self) -> &Court {
        &self.court
    }

    pub fn history(&self) -> &[CourtEvent] {
        &self.history
    }

    /// 招募新官员（不授官职），返回其 ID。
    pub fn recruit(&mut self, name: String, speciality: Department) -> Result<AgentId, AppError> {
        let event = self.court.propose_recruitment(name, speciality)?;
        let CourtEvent::AgentRegistered(agent) = &event else {
            unreachable!("propose_recruitment 只产生 AgentRegistered");
        };
        let id = agent.id;
        self.commit(vec![event])?;
        Ok(id)
    }

    pub fn appoint(&mut self, agent: AgentId, office: Office) -> Result<(), AppError> {
        let event = self.court.propose_appointment(agent, office)?;
        self.commit(vec![event])
    }

    pub fn dismiss(&mut self, agent: AgentId) -> Result<(), AppError> {
        let event = self.court.propose_dismissal(agent)?;
        self.commit(vec![event])
    }

    pub fn reform(&mut self, mode: GovernanceMode) -> Result<(), AppError> {
        let event = self.court.propose_reform(mode)?;
        self.commit(vec![event])
    }

    /// 下诏并同步派工；路由或执行失败时不记录任何事件。
    pub fn issue_edict(
        &mut self,
        department: Department,
        text: String,
        backend: &dyn WorkerBackend,
    ) -> Result<(Edict, ExecutionReport), AppError> {
        let edict = self.court.new_edict(department, text)?;
        let report = dispatch(&self.court, &edict, backend)?;
        self.commit(vec![
            CourtEvent::EdictIssued(edict.clone()),
            CourtEvent::TaskCompleted {
                task: edict.id,
                worker: report.worker,
                summary: report.summary.clone(),
            },
        ])?;
        Ok((edict, report))
    }

    fn commit(&mut self, events: Vec<CourtEvent>) -> Result<(), AppError> {
        let mut next = self.court.clone();
        for event in &events {
            next.apply(event)?;
        }
        self.store.append(&events)?;
        self.court = next;
        self.history.extend(events);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ziyuan_runtime::DemoBackend;
    use ziyuan_storage::MemoryEventLog;

    #[test]
    fn found_and_open_guard_the_store() {
        let mut store = MemoryEventLog::default();
        assert!(matches!(
            Session::open(&mut store),
            Err(AppError::NotFounded)
        ));
        Session::found(&mut store).unwrap();
        assert!(matches!(
            Session::found(&mut store),
            Err(AppError::AlreadyFounded)
        ));
    }

    #[test]
    fn rejected_edict_records_nothing() {
        let mut store = MemoryEventLog::default();
        let mut session = Session::found(&mut store).unwrap();
        session.reform(GovernanceMode::Cabinet).unwrap();
        let before = session.history().len();
        let result = session.issue_edict(Department::Works, "无首辅".into(), &DemoBackend);
        assert!(matches!(
            result,
            Err(AppError::Dispatch(DispatchError::Route(
                GovernanceError::Vacant(Office::FirstGrandSecretary)
            )))
        ));
        assert_eq!(session.history().len(), before);
        drop(session);
        assert_eq!(store.load().unwrap().len(), before);
    }

    #[test]
    fn reopened_session_matches_live_state() {
        let mut store = MemoryEventLog::default();
        let mut session = Session::found(&mut store).unwrap();
        let id = session.recruit("新工".into(), Department::Works).unwrap();
        session
            .appoint(id, Office::Clerk(Department::Works))
            .unwrap();
        session
            .issue_edict(Department::Works, "一".into(), &DemoBackend)
            .unwrap();
        let (_, second) = session
            .issue_edict(Department::Works, "二".into(), &DemoBackend)
            .unwrap();
        assert_eq!(second.worker, id, "第二件诏令应派给任务较少的新官员");
        drop(session);

        let reopened = Session::open(&mut store).unwrap();
        let court = reopened.court();
        assert_eq!(court.office_of(id), Some(Office::Clerk(Department::Works)));
        assert_eq!(court.completed_count(), 2);
        assert_eq!(court.recorded_completions(id), 1);
    }
}
