use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::IpAddr;
use ziyuan_storage::JsonFileError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkCodeKind<'a> {
    Syosetu(&'a str),
    Kakuyomu(&'a str),
    Feed(&'a str),
}

pub fn classify_code(code: &str) -> Option<WorkCodeKind<'_>> {
    if let Some(id) = code.strip_prefix("kakuyomu:") {
        is_kakuyomu_id(id).then_some(WorkCodeKind::Kakuyomu(id))
    } else if let Some(url) = code.strip_prefix("feed:") {
        normalize_feed_url(url)
            .ok()
            .filter(|canonical| canonical == url)
            .map(|_| WorkCodeKind::Feed(url))
    } else if is_syosetu_ncode(code) {
        Some(WorkCodeKind::Syosetu(code))
    } else {
        None
    }
}

pub fn work_url(code: &str) -> Option<String> {
    match classify_code(code)? {
        WorkCodeKind::Syosetu(ncode) => Some(format!("https://ncode.syosetu.com/{ncode}/")),
        WorkCodeKind::Kakuyomu(id) => Some(format!("https://kakuyomu.jp/works/{id}")),
        WorkCodeKind::Feed(url) => Some(url.to_owned()),
    }
}

fn is_syosetu_ncode(code: &str) -> bool {
    let b = code.as_bytes();
    (6..=11).contains(&b.len())
        && b[0] == b'n'
        && b[1..5].iter().all(u8::is_ascii_digit)
        && b[5..].iter().all(u8::is_ascii_lowercase)
}

fn is_kakuyomu_id(id: &str) -> bool {
    (10..=24).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}

pub(crate) fn is_forbidden_feed_host(host: &str) -> bool {
    if host.is_empty()
        || host.len() > 253
        || host.contains(':')
        || host.starts_with('[')
        || host.starts_with('.')
        || host.ends_with('.')
        || !host.contains('.')
        || host.parse::<IpAddr>().is_ok()
    {
        return true;
    }
    let lower = host.to_ascii_lowercase();
    for suffix in [
        ".localhost",
        ".local",
        ".internal",
        ".test",
        ".invalid",
        ".example",
        ".arpa",
        ".lan",
        ".localdomain",
    ] {
        if lower.ends_with(suffix) {
            return true;
        }
    }
    if lower == "localhost" {
        return true;
    }
    lower.split('.').any(|label| {
        label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

fn normalize_feed_url(url: &str) -> Result<String, SubscriptionError> {
    let rest = url
        .strip_prefix("https://")
        .ok_or(SubscriptionError::InvalidInput)?;
    if url.len() > 512
        || url
            .bytes()
            .any(|b| !b.is_ascii_graphic() || matches!(b, b'?' | b'#' | b'@' | b'\\'))
    {
        return Err(SubscriptionError::InvalidInput);
    }
    let Some((host, path_rest)) = rest.split_once('/') else {
        return Err(SubscriptionError::InvalidInput);
    };
    if is_forbidden_feed_host(host) || path_rest.is_empty() {
        return Err(SubscriptionError::InvalidInput);
    }
    if path_rest.split('/').any(|seg| seg == ".." || seg == ".") {
        return Err(SubscriptionError::InvalidInput);
    }
    Ok(format!("https://{}/{path_rest}", host.to_ascii_lowercase()))
}

/// Accept a strict Syosetu NCode/URL, Kakuyomu work ID/URL, or explicit `feed:https://...` RSS/Atom URL.
/// Chapter URLs, ports, credentials, query strings and fragments are deliberately not accepted.
pub fn normalize_code(input: &str) -> Result<String, SubscriptionError> {
    if let Some(rest) = input.strip_prefix("https://kakuyomu.jp/works/") {
        let id = rest.strip_suffix('/').unwrap_or(rest);
        return if is_kakuyomu_id(id) {
            Ok(format!("kakuyomu:{id}"))
        } else {
            Err(SubscriptionError::InvalidInput)
        };
    }
    if let Some(id) = input
        .strip_prefix("kakuyomu:")
        .or_else(|| input.strip_prefix("KAKUYOMU:"))
    {
        return if is_kakuyomu_id(id) {
            Ok(format!("kakuyomu:{id}"))
        } else {
            Err(SubscriptionError::InvalidInput)
        };
    }
    if let Some(url) = input
        .strip_prefix("feed:")
        .or_else(|| input.strip_prefix("FEED:"))
    {
        let canonical = normalize_feed_url(url)?;
        return Ok(format!("feed:{canonical}"));
    }
    let raw = if let Some(path) = input.strip_prefix("https://ncode.syosetu.com/") {
        path.strip_suffix('/').unwrap_or(path)
    } else {
        input
    };
    let code = raw.to_ascii_lowercase();
    if is_syosetu_ncode(&code) {
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
    /// 新章节提醒自动入兵部驿站（交工部，只入队不执行）。1.1.0 的状态文件没有此项，读作关闭。
    #[serde(default)]
    pub queue_new_chapters: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_seconds: 1800,
            queue_new_chapters: false,
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
