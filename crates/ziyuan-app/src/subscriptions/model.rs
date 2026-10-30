use serde::{Deserialize, Serialize};
use std::fmt;
use ziyuan_storage::JsonFileError;

/// Accept a strict NCode (four digits, one to six ASCII letters) or canonical work URL.
/// Chapter URLs, ports, credentials, query strings and fragments are deliberately not accepted.
pub fn normalize_code(input: &str) -> Result<String, SubscriptionError> {
    let raw = if let Some(path) = input.strip_prefix("https://ncode.syosetu.com/") {
        path.strip_suffix('/').unwrap_or(path)
    } else {
        input
    };
    let code = raw.to_ascii_lowercase();
    let b = code.as_bytes();
    if (6..=11).contains(&b.len())
        && b[0] == b'n'
        && b[1..5].iter().all(u8::is_ascii_digit)
        && b[5..].iter().all(u8::is_ascii_lowercase)
    {
        Ok(code)
    } else {
        Err(SubscriptionError::InvalidInput)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedWork {
    pub code: String,
    pub title: String,
    pub author: String,
    pub chapter_count: u64,
    /// Unix seconds (Japan local API times are converted to UTC by the adapter).
    pub last_published: u64,
    pub work_updated: u64,
}
impl ObservedWork {
    pub(crate) fn valid(&self) -> bool {
        normalize_code(&self.code).is_ok_and(|code| code == self.code)
            && !self.title.trim().is_empty()
            && self.title.len() <= 65536
            && !self.author.trim().is_empty()
            && self.author.len() <= 65536
            && (1..=10_000_000).contains(&self.chapter_count)
            && (1..=253_402_267_199).contains(&self.last_published)
            && (1..=253_402_267_199).contains(&self.work_updated)
    }
}

/// Sources return only requested works; an omitted work is NotFound, never a deletion.
/// Implementations must bound execution time to <90s, preferably <=30s for the entire batch.
pub trait WorkSource {
    fn fetch(&self, codes: &[String]) -> Result<Vec<ObservedWork>, SourceError>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceError {
    NotFound,
    Timeout,
    Malformed,
    RateLimited,
    Unavailable,
    Suspicious,
}
impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for SourceError {}

#[derive(Debug)]
pub enum SubscriptionError {
    Storage(JsonFileError),
    InvalidInput,
    Random,
}
impl From<JsonFileError> for SubscriptionError {
    fn from(value: JsonFileError) -> Self {
        Self::Storage(value)
    }
}
impl fmt::Display for SubscriptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(e) => e.fmt(f),
            Self::InvalidInput => f.write_str("invalid subscription input"),
            Self::Random => f.write_str("random ID generation failed"),
        }
    }
}
impl std::error::Error for SubscriptionError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub enabled: bool,
    pub interval_seconds: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_seconds: 1800,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    /// A new identity is assigned after remove + re-add, even for the same code.
    pub id: String,
    pub code: String,
    pub added_at: u64,
    pub baseline: Option<ObservedWork>,
    pub last_error: Option<SourceError>,
    pub next_check: u64,
    pub last_attempt: Option<u64>,
    pub consecutive_failures: u32,
    pub(crate) baseline_revision: u64,
    pub(crate) lease: Option<Lease>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Lease {
    pub token: String,
    pub expires: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// Observed difference, not a guarantee of exactly this many new chapters.
    NewChapters {
        observed_increase: u64,
    },
    InformationChanged,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub id: String,
    pub subscription_id: String,
    pub code: String,
    pub observed_at: u64,
    pub kind: NoticeKind,
    pub observation: ObservedWork,
    pub read: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckMode {
    Manual,
    Scheduled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Complete,
    Disabled,
    Empty,
    NotDue,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckSummary {
    pub status: CheckStatus,
    pub checked: usize,
    pub skipped: usize,
    pub failed: usize,
    pub notices: usize,
    pub discarded: usize,
}
impl Default for CheckSummary {
    fn default() -> Self {
        Self {
            status: CheckStatus::Complete,
            checked: 0,
            skipped: 0,
            failed: 0,
            notices: 0,
            discarded: 0,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct State {
    pub settings: Settings,
    pub items: Vec<Subscription>,
    pub notices: Vec<Notice>,
}

pub(crate) fn random_id() -> Result<String, SubscriptionError> {
    use ring::rand::SecureRandom;
    let mut bytes = [0; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| SubscriptionError::Random)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
