//! 兵部驿站 · 作品关注：未读的新章节提醒入队为诏令，交工部，只入队不执行。
//! 每次都重扫全部未读提醒、按站名去重，因此上次入队失败的下次自动补上。
use crate::subscriptions::{Notice, NoticeKind, SubscriptionError, Subscriptions};
use crate::{AppError, Session};
use serde::Serialize;
use std::path::Path;
use ziyuan_core::{Department, Seal};
use ziyuan_storage::{EventLog, EventStore};

/// 驿站名：同一作品同一章数只入队一次。
pub fn follow_station(code: &str, chapter_count: u64) -> String {
    format!("syosetu:{code}#{chapter_count}")
}

fn follow_text(notice: &Notice, increase: u64) -> String {
    let work = &notice.observation;
    format!(
        "syosetu 作品更新：《{}》（{}）新增 {increase} 章，共 {} 章\n作者：{}\n\
         https://ncode.syosetu.com/{}/\n\n\
         本程序不下载正文；原文需用户自行取得并确认翻译与上传权限。",
        work.title, notice.code, work.chapter_count, work.author, notice.code
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowCourierStatus {
    /// 未开启「新章节自动入队」，未打开朝廷。
    Disabled,
    NotFounded,
    Done,
    /// 读不了关注状态或打不开朝廷；提醒保留，下次再扫。
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FollowQueued {
    pub notice: String,
    pub station: String,
    pub task: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FollowQueueError {
    pub notice: String,
    pub station: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FollowCourier {
    pub status: FollowCourierStatus,
    pub queued: Vec<FollowQueued>,
    pub already_queued: usize,
    pub errors: Vec<FollowQueueError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl FollowCourier {
    fn status(status: FollowCourierStatus, message: Option<String>) -> Self {
        Self {
            status,
            queued: Vec::new(),
            already_queued: 0,
            errors: Vec::new(),
            message,
        }
    }
    pub fn disabled() -> Self {
        Self::status(FollowCourierStatus::Disabled, None)
    }
    /// 把 [`Session::follow_intake`] 的结果（含打开会话的错误）归为一份报告。
    pub fn from_result(result: Result<Self, AppError>) -> Self {
        match result {
            Ok(report) => report,
            Err(AppError::NotFounded) => Self::status(FollowCourierStatus::NotFounded, None),
            Err(e) => Self::status(FollowCourierStatus::Error, Some(e.to_string())),
        }
    }
}

fn state_error(e: SubscriptionError) -> AppError {
    AppError::External(format!("关注列表 .ziyuan/subscriptions/state.json：{e}"))
}

impl<S: EventStore> Session<S> {
    /// 未读的新章节提醒逐条入队（不看设置）；已有同站名诏令的不再入队。
    /// 单条入队失败（如工部无人可派）记入报告，不影响其余；不改提醒的已读状态。
    pub fn follow_intake(&mut self, service: &Subscriptions) -> Result<FollowCourier, AppError> {
        let notices = service.updates(true).map_err(state_error)?;
        let mut report = FollowCourier::status(FollowCourierStatus::Done, None);
        for notice in &notices {
            let NoticeKind::NewChapters { observed_increase } = notice.kind else {
                continue;
            };
            let station = follow_station(&notice.code, notice.observation.chapter_count);
            let text = follow_text(notice, observed_increase);
            match self.post_unique(Department::Works, text, Seal::default(), &station, false) {
                Ok(None) => report.already_queued += 1,
                Ok(Some(edict)) => report.queued.push(FollowQueued {
                    notice: notice.id.clone(),
                    station,
                    task: edict.id.0,
                }),
                Err(e) => report.errors.push(FollowQueueError {
                    notice: notice.id.clone(),
                    station,
                    message: e.to_string(),
                }),
            }
        }
        Ok(report)
    }
}

/// 开启「新章节自动入队」时打开 `root` 的朝廷并入队；未开启时不打开朝廷。
/// 从不返回错误：出错只写在报告里，不影响作品检查本身的结果。
pub fn follow_courier(root: &Path) -> FollowCourier {
    let service = Subscriptions::new(root);
    match service.settings() {
        Ok(settings) if !settings.queue_new_chapters => return FollowCourier::disabled(),
        Ok(_) => {}
        Err(e) => return FollowCourier::from_result(Err(state_error(e))),
    }
    FollowCourier::from_result(
        Session::open(EventLog::new(root)).and_then(|mut session| session.follow_intake(&service)),
    )
}
