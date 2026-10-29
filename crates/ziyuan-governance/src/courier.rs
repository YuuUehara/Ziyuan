//! 兵部驿站：外界（GitHub）来件入队与回报。

use crate::edicts::ChangeState;
use crate::{Court, GovernanceError, TaskStatus};
use ziyuan_core::{CourtEvent, TaskId};

impl Court {
    /// 驿站回报可否：诏令经驿传入队、尚未回报过同类。回奏评论须已完成、经皇帝评定（含代批），
    /// 且没有未平反的密报；草稿 PR 须改动已准奏落盘。`url` 须为单行。
    pub fn propose_courier_reply(
        &self,
        task: TaskId,
        pull_request: bool,
        url: &str,
    ) -> Result<(), GovernanceError> {
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        let refuse = |why: String| Err(GovernanceError::Military(why));
        if entry.courier.is_none() {
            return refuse(format!("诏令 #{} 不是经驿传入队的，无处回报", task.0));
        }
        if url.trim().is_empty() || url.chars().any(char::is_control) {
            return refuse("回报网址须为单行文字".into());
        }
        if pull_request {
            if entry.pull_request.is_some() {
                return refuse(format!("诏令 #{} 已开过草稿 PR", task.0));
            }
            if entry.changes.as_ref().map(|c| c.state) != Some(ChangeState::Applied) {
                return refuse(format!("诏令 #{} 没有已准奏的改动", task.0));
            }
        } else {
            if entry.replied.is_some() {
                return refuse(format!("诏令 #{} 已回报过", task.0));
            }
            if !matches!(entry.status, TaskStatus::Completed { .. }) {
                return refuse(format!("诏令 #{} 尚未办完，无可回报", task.0));
            }
            if entry.review.is_none() {
                return refuse(format!("诏令 #{} 尚未评定，评定后才送回来处", task.0));
            }
            if entry.standing_suspicions().next().is_some() {
                return refuse(format!(
                    "诏令 #{} 有未平反的密报，不送出；复勘平反后再回报",
                    task.0
                ));
            }
        }
        Ok(())
    }

    /// 驿站外发前记意图：回报须可行（同 [`Self::propose_courier_reply`]），且尚无同类待确认的意图
    /// （有则须按原键续作，不另起）。`op` 须为 1–64 个 ASCII 字母、数字或 `-`。
    pub fn propose_courier_dispatch(
        &self,
        task: TaskId,
        pull_request: bool,
        op: &str,
    ) -> Result<CourtEvent, GovernanceError> {
        self.propose_courier_reply(task, pull_request, "-")?;
        if op.is_empty()
            || op.len() > 64
            || !op.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(GovernanceError::Military(
                "幂等键须为 1–64 个字母、数字或连字符".into(),
            ));
        }
        if let Some(pending) = self.courier_op(task, pull_request) {
            return Err(GovernanceError::Military(format!(
                "诏令 #{} 已有待确认的驿传（{pending}），须按原键续作",
                task.0
            )));
        }
        Ok(CourtEvent::CourierDispatched {
            task,
            pull_request,
            op: op.to_owned(),
        })
    }

    /// 诏令同类回报待确认的幂等键：已记意图而尚无网址。
    pub fn courier_op(&self, task: TaskId, pull_request: bool) -> Option<&str> {
        let entry = self.tasks.get(&task)?;
        let (op, url) = if pull_request {
            (&entry.pull_request_op, &entry.pull_request)
        } else {
            (&entry.reply_op, &entry.replied)
        };
        op.as_deref().filter(|_| url.is_none())
    }

    /// 回报所带的幂等键须与待确认的意图相符：记过意图的须带上同一键，未记的（旧版）不带。
    pub fn check_courier_op(
        &self,
        task: TaskId,
        pull_request: bool,
        op: Option<&str>,
    ) -> Result<(), GovernanceError> {
        if self.courier_op(task, pull_request) == op {
            return Ok(());
        }
        Err(GovernanceError::Military(format!(
            "诏令 #{} 的回报与驿传意图的幂等键不符",
            task.0
        )))
    }

    /// 驿传入队：诏令须待办且尚未记来处；来处须非空、不含控制字符、至多 80 字。
    pub fn propose_courier(
        &self,
        task: TaskId,
        station: String,
    ) -> Result<CourtEvent, GovernanceError> {
        self.open_task(task)?;
        let station = station.trim().to_owned();
        if station.is_empty()
            || station.chars().count() > 80
            || station.chars().any(char::is_control)
        {
            return Err(GovernanceError::Military(
                "驿站名须为 1–80 字的单行文字".into(),
            ));
        }
        if self.tasks[&task].courier.is_some() {
            return Err(GovernanceError::Military(format!(
                "诏令 #{} 已记驿传来处",
                task.0
            )));
        }
        Ok(CourtEvent::CourierDelivered { task, station })
    }
}
