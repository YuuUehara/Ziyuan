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
