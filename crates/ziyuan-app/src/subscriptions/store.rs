use super::model::*;
use std::path::{Path, PathBuf};
use ziyuan_storage::JsonFile;

/// Local state in `.ziyuan/subscriptions/state.json`, independent of the event store.
pub struct Subscriptions {
    pub(crate) file: JsonFile<State>,
}
impl Subscriptions {
    pub fn new(workspace: impl AsRef<Path>) -> Self {
        Self::with_max_bytes(workspace, 8 * 1024 * 1024)
    }
    /// Both reads and atomic writes use this byte limit. Failed saves preserve old state.
    pub fn with_max_bytes(workspace: impl AsRef<Path>, max_bytes: u64) -> Self {
        Self {
            file: JsonFile::new(
                workspace.as_ref().join(".ziyuan/subscriptions/state.json"),
                1,
                max_bytes,
            ),
        }
    }
    pub fn state_path(&self) -> PathBuf {
        self.file.path().to_path_buf()
    }
    pub(crate) fn load(&self) -> Result<State, SubscriptionError> {
        Ok(self.file.load()?.map(|s| s.value).unwrap_or_default())
    }
    pub fn add(&self, input: &str, now: u64) -> Result<Subscription, SubscriptionError> {
        let code = normalize_code(input)?;
        self.file
            .update(|current| {
                let mut state = current.map(|s| s.value).unwrap_or_default();
                if let Some(item) = state.items.iter().find(|i| i.code == code) {
                    let item = item.clone();
                    return Ok((state, item));
                }
                let item = Subscription {
                    id: random_id()?,
                    code,
                    added_at: now,
                    baseline: None,
                    last_error: None,
                    next_check: now,
                    last_attempt: None,
                    consecutive_failures: 0,
                    baseline_revision: 0,
                    lease: None,
                };
                state.items.push(item.clone());
                Ok((state, item))
            })
            .map(|(_, result)| result)
    }
    pub fn list(&self) -> Result<Vec<Subscription>, SubscriptionError> {
        Ok(self.load()?.items)
    }
    /// Removes the work and its reminders. Re-add starts a fresh baseline and identity.
    pub fn remove(&self, input: &str) -> Result<bool, SubscriptionError> {
        let code = normalize_code(input)?;
        self.file
            .update(|current| {
                let mut state = current.map(|s| s.value).unwrap_or_default();
                let before = state.items.len();
                state.items.retain(|i| i.code != code);
                state.notices.retain(|n| n.code != code);
                let removed = state.items.len() != before;
                Ok((state, removed))
            })
            .map(|(_, result)| result)
    }
    pub fn settings(&self) -> Result<Settings, SubscriptionError> {
        Ok(self.load()?.settings)
    }
    pub fn set_settings(&self, settings: Settings) -> Result<(), SubscriptionError> {
        if settings.interval_seconds < 900 {
            return Err(SubscriptionError::InvalidInput);
        }
        self.file
            .update(|current| {
                let mut state = current.map(|s| s.value).unwrap_or_default();
                state.settings = settings;
                Ok((state, ()))
            })
            .map(|(_, result)| result)
    }
    /// 在同一次保存中读出、修改并校验设置，返回新设置；校验不过时不保存。
    pub fn update_settings(
        &self,
        f: impl FnOnce(&mut Settings),
    ) -> Result<Settings, SubscriptionError> {
        self.file
            .update(|current| {
                let mut state = current.map(|s| s.value).unwrap_or_default();
                f(&mut state.settings);
                if state.settings.interval_seconds < 900 {
                    return Err(SubscriptionError::InvalidInput);
                }
                let settings = state.settings.clone();
                Ok((state, settings))
            })
            .map(|(_, result)| result)
    }
    pub fn updates(&self, unread_only: bool) -> Result<Vec<Notice>, SubscriptionError> {
        Ok(self
            .load()?
            .notices
            .into_iter()
            .filter(|n| !unread_only || !n.read)
            .collect())
    }
    /// Marks exactly one persisted reminder read. Checks never mark reminders read.
    pub fn read(&self, id: &str) -> Result<bool, SubscriptionError> {
        self.file
            .update(|current| {
                let mut state = current.map(|s| s.value).unwrap_or_default();
                let found = if let Some(notice) = state.notices.iter_mut().find(|n| n.id == id) {
                    notice.read = true;
                    true
                } else {
                    false
                };
                Ok((state, found))
            })
            .map(|(_, result)| result)
    }
}
