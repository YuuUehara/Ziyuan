//! The HTTP serve future owns this guard; dropping it aborts future polls.
//! An already running blocking source request may finish (production timeout: 30s).
use crate::subscriptions::Service;
use std::time::Duration;
use ziyuan_app::subscriptions::CheckMode;

pub(crate) struct Scheduler(tokio::task::JoinHandle<()>);
impl Scheduler {
    pub(crate) fn start(service: Service) -> Self {
        Self(tokio::spawn(async move {
            loop {
                // No catch-up burst; each poll claims at most 50. Subsequent polls cover
                // remaining due works, while app cooldowns/backoff prevent busy retries.
                tokio::time::sleep(Duration::from_secs(5)).await;
                if let Err(error) = service.check(CheckMode::Scheduled).await {
                    eprintln!("作品关注定时检查失败：{}", error.1);
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
