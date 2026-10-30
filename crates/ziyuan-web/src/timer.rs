//! The HTTP serve future owns this guard; dropping it aborts future polls.
//! An already running blocking source request may finish (production timeout: 30s).
use crate::state::AppState;
use crate::subscriptions::scheduled_tick;
use std::time::Duration;
use ziyuan_app::FollowCourierStatus;

pub(crate) struct Scheduler(tokio::task::JoinHandle<()>);
impl Scheduler {
    pub(crate) fn start(app: AppState) -> Self {
        Self(tokio::spawn(async move {
            loop {
                // No catch-up burst; each poll claims at most 50. Subsequent polls cover
                // remaining due works, while app cooldowns/backoff prevent busy retries.
                tokio::time::sleep(Duration::from_secs(5)).await;
                match scheduled_tick(&app).await {
                    Err(error) => eprintln!("作品关注定时检查失败：{}", error.1),
                    Ok((_, courier)) => {
                        if courier.status == FollowCourierStatus::Error {
                            eprintln!(
                                "作品关注入驿站失败：{}",
                                courier.message.unwrap_or_default()
                            );
                        }
                        for error in courier.errors {
                            eprintln!("作品关注 {} 未入驿站：{}", error.station, error.message);
                        }
                    }
                }
            }
        }))
    }
}
impl Drop for Scheduler {
    fn drop(&mut self) {
        self.0.abort();
    }
}
