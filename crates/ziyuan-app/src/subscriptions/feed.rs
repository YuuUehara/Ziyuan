//! Metadata-only RSS 2.0 / Atom 1.0 sources for Kakuyomu and explicit `feed:https://...` URLs.
//! Strictly inspects channel title, author, timestamps, and episode/item count only — never stores
//! or downloads chapter body text, never follows redirects, and rejects private/loopback targets.
use super::{
    ObservedWork, SourceError, SyosetuSource, WorkCodeKind, WorkSource, classify_code,
    is_forbidden_feed_host,
};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::time::Duration;

const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_FEED_ENTRIES: u64 = 100_000;

fn build_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .proxy(None)
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build()
        .into()
}

fn transport_error(error: ureq::Error) -> SourceError {
    match error {
        ureq::Error::Timeout(_) => SourceError::Timeout,
        ureq::Error::BodyExceedsLimit(_) => SourceError::Malformed,
        _ => SourceError::Unavailable,
    }
}

trait FeedTransport {
    fn get_xml(&self, url: &str) -> Result<(u16, Vec<u8>), SourceError>;
}

struct UreqFeedTransport {
    agent: ureq::Agent,
}

impl UreqFeedTransport {
    fn new() -> Self {
        Self {
            agent: build_agent(),
        }
    }
}

impl FeedTransport for UreqFeedTransport {
    fn get_xml(&self, url: &str) -> Result<(u16, Vec<u8>), SourceError> {
        let mut response = self
            .agent
            .get(url)
            .header(
                "Accept",
                "application/atom+xml, application/rss+xml, application/xml, text/xml",
            )
            .call()
            .map_err(transport_error)?;
        let status = response.status().as_u16();
        if status != 200 {
            return Ok((status, vec![]));
        }
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES as u64)
            .read_to_vec()
            .map_err(transport_error)?;
        Ok((status, body))
    }
}

/// Fixed official Kakuyomu public Atom feed (`https://kakuyomu.jp/works/<id>/episodes.rss`).
/// Never downloads episode HTML or body prose.
pub struct KakuyomuSource {
    transport: UreqFeedTransport,
}

impl KakuyomuSource {
    pub fn new() -> Self {
        Self {
            transport: UreqFeedTransport::new(),
        }
    }
}

impl Default for KakuyomuSource {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkSource for KakuyomuSource {
    fn fetch(&self, codes: &[String]) -> Result<Vec<ObservedWork>, SourceError> {
        fetch_kakuyomu_with(&self.transport, codes)
    }
}

fn kakuyomu_feed_url(id: &str) -> String {
    format!("https://kakuyomu.jp/works/{id}/episodes.rss")
}

fn fetch_kakuyomu_with(
    transport: &impl FeedTransport,
    codes: &[String],
) -> Result<Vec<ObservedWork>, SourceError> {
    if codes.is_empty() || codes.len() > 50 {
        return Err(SourceError::Malformed);
    }
    let mut works = Vec::new();
    for code in codes {
        let Some(WorkCodeKind::Kakuyomu(id)) = classify_code(code) else {
            return Err(SourceError::Malformed);
        };
        let url = kakuyomu_feed_url(id);
        let (status, body) = transport.get_xml(&url)?;
        match status {
            200 => works.push(parse_feed_metadata(code, &body)?),
            404 | 410 => {
                if codes.len() == 1 {
                    return Err(SourceError::NotFound);
                }
            }
            429 => return Err(SourceError::RateLimited),
            _ => return Err(SourceError::Unavailable),
        }
    }
    Ok(works)
}

/// Metadata-only generic HTTPS RSS/Atom feed source (`feed:https://...`).
/// Enforces HTTPS, no redirects, public DNS/IP validation, and 2 MiB limit.
pub struct FeedSource {
    transport: UreqFeedTransport,
}

impl FeedSource {
    pub fn new() -> Self {
        Self {
            transport: UreqFeedTransport::new(),
        }
    }
}

impl Default for FeedSource {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkSource for FeedSource {
    fn fetch(&self, codes: &[String]) -> Result<Vec<ObservedWork>, SourceError> {
        fetch_feeds_with(&self.transport, codes, validate_feed_dns)
    }
}

fn fetch_feeds_with(
    transport: &impl FeedTransport,
    codes: &[String],
    check_host: impl Fn(&str) -> Result<(), SourceError>,
) -> Result<Vec<ObservedWork>, SourceError> {
    if codes.is_empty() || codes.len() > 50 {
        return Err(SourceError::Malformed);
    }
    let mut works = Vec::new();
    for code in codes {
        let Some(WorkCodeKind::Feed(url)) = classify_code(code) else {
            return Err(SourceError::Malformed);
        };
        check_host(url)?;
        let (status, body) = transport.get_xml(url)?;
        match status {
            200 => works.push(parse_feed_metadata(code, &body)?),
            404 | 410 => {
                if codes.len() == 1 {
                    return Err(SourceError::NotFound);
                }
            }
            429 => return Err(SourceError::RateLimited),
            _ => return Err(SourceError::Unavailable),
        }
    }
    Ok(works)
}

/// Routes Syosetu, Kakuyomu, and generic HTTPS RSS/Atom codes to their dedicated metadata-only adapters.
pub struct CompositeSource {
    syosetu: SyosetuSource,
    kakuyomu: KakuyomuSource,
    feed: FeedSource,
}

impl CompositeSource {
    pub fn new() -> Self {
        Self {
            syosetu: SyosetuSource::new(),
            kakuyomu: KakuyomuSource::new(),
            feed: FeedSource::new(),
        }
    }
}

impl Default for CompositeSource {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkSource for CompositeSource {
    fn fetch(&self, codes: &[String]) -> Result<Vec<ObservedWork>, SourceError> {
        fetch_composite(&self.syosetu, &self.kakuyomu, &self.feed, codes)
    }
}

fn fetch_composite(
    syosetu: &impl WorkSource,
    kakuyomu: &impl WorkSource,
    feed: &impl WorkSource,
    codes: &[String],
) -> Result<Vec<ObservedWork>, SourceError> {
    if codes.is_empty() || codes.len() > 50 {
        return Err(SourceError::Malformed);
    }
    let mut syosetu_codes = Vec::new();
    let mut kakuyomu_codes = Vec::new();
    let mut feed_codes = Vec::new();
    for code in codes {
        match classify_code(code) {
            Some(WorkCodeKind::Syosetu(_)) => syosetu_codes.push(code.clone()),
            Some(WorkCodeKind::Kakuyomu(_)) => kakuyomu_codes.push(code.clone()),
            Some(WorkCodeKind::Feed(_)) => feed_codes.push(code.clone()),
            None => return Err(SourceError::Malformed),
        }
    }
    let groups = [
        (!syosetu_codes.is_empty()) as u8,
        (!kakuyomu_codes.is_empty()) as u8,
        (!feed_codes.is_empty()) as u8,
    ]
    .into_iter()
    .sum::<u8>();
    let mut results = Vec::new();
    if !syosetu_codes.is_empty() {
        match syosetu.fetch(&syosetu_codes) {
            Ok(mut w) => results.append(&mut w),
            Err(SourceError::NotFound) if groups > 1 => {}
            Err(e) => return Err(e),
        }
    }
    if !kakuyomu_codes.is_empty() {
        match kakuyomu.fetch(&kakuyomu_codes) {
            Ok(mut w) => results.append(&mut w),
            Err(SourceError::NotFound) if groups > 1 => {}
            Err(e) => return Err(e),
        }
    }
    if !feed_codes.is_empty() {
        match feed.fetch(&feed_codes) {
            Ok(mut w) => results.append(&mut w),
            Err(SourceError::NotFound) if groups > 1 => {}
            Err(e) => return Err(e),
        }
    }
    Ok(results)
}

pub(crate) fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_ipv4(v4),
        IpAddr::V6(v6) => is_public_ipv6(v6),
    }
}

fn is_public_ipv4(v4: Ipv4Addr) -> bool {
    let [a, b, _, _] = v4.octets();
    !(v4.is_private()
        || v4.is_loopback()
        || v4.is_link_local()
        || v4.is_broadcast()
        || v4.is_documentation()
        || v4.is_unspecified()
        || v4.is_multicast()
        || (a == 100 && (64..=127).contains(&b))
        || a == 0
        || (a == 198 && (b == 18 || b == 19))
        || a >= 240)
}

fn is_public_ipv6(v6: Ipv6Addr) -> bool {
    if let Some(v4) = v6.to_ipv4_mapped() {
        return is_public_ipv4(v4);
    }
    let segs = v6.segments();
    !(v6.is_loopback()
        || v6.is_unspecified()
        || v6.is_multicast()
        || (segs[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
        || (segs[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (segs[0] == 0x2001 && segs[1] == 0x0db8)) // documentation
}

fn validate_feed_dns(url: &str) -> Result<(), SourceError> {
    let rest = url.strip_prefix("https://").ok_or(SourceError::Malformed)?;
    let (host, _) = rest.split_once('/').ok_or(SourceError::Malformed)?;
    if is_forbidden_feed_host(host) {
        return Err(SourceError::Unavailable);
    }
    let addrs: Vec<_> = (host, 443)
        .to_socket_addrs()
        .map_err(|_| SourceError::Unavailable)?
        .collect();
    if addrs.is_empty() || addrs.iter().any(|addr| !is_public_ip(addr.ip())) {
        return Err(SourceError::Unavailable);
    }
    Ok(())
}

/// Metadata-only parser for Atom 1.0 and RSS 2.0 XML documents.
/// Intentionally discards `<content>`, `<summary>`, and `<description>` bodies.
pub fn parse_feed_metadata(code: &str, bytes: &[u8]) -> Result<ObservedWork, SourceError> {
    if bytes.is_empty() || bytes.len() > MAX_RESPONSE_BYTES {
        return Err(SourceError::Malformed);
    }
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum FeedKind {
        Unknown,
        Atom,
        Rss,
    }

    let mut kind = FeedKind::Unknown;
    let mut stack: Vec<String> = Vec::new();
    let mut title: Option<String> = None;
    let mut author: Option<String> = None;
    let mut feed_updated: Option<u64> = None;
    let mut entry_count: u64 = 0;
    let mut latest_entry: Option<u64> = None;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = local_tag_name(e.name().as_ref());
                if stack.is_empty() {
                    kind = match name.as_str() {
                        "feed" => FeedKind::Atom,
                        "rss" | "rdf" => FeedKind::Rss,
                        _ => return Err(SourceError::Malformed),
                    };
                }
                if (kind == FeedKind::Atom && stack.len() == 1 && name == "entry")
                    || (kind == FeedKind::Rss
                        && (stack.last().map(String::as_str) == Some("channel")
                            || stack.len() == 1)
                        && name == "item")
                {
                    entry_count = entry_count.saturating_add(1);
                    if entry_count > MAX_FEED_ENTRIES {
                        return Err(SourceError::Malformed);
                    }
                }
                stack.push(name);
            }
            Ok(Event::Empty(e)) => {
                let name = local_tag_name(e.name().as_ref());
                if (kind == FeedKind::Atom && stack.len() == 1 && name == "entry")
                    || (kind == FeedKind::Rss
                        && (stack.last().map(String::as_str) == Some("channel")
                            || stack.len() == 1)
                        && name == "item")
                {
                    entry_count = entry_count.saturating_add(1);
                }
            }
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Text(e)) => {
                let Ok(text) = e.xml_content(quick_xml::XmlVersion::Implicit1_0) else {
                    return Err(SourceError::Malformed);
                };
                let text = text.trim();
                if text.is_empty() {
                    buf.clear();
                    continue;
                }
                handle_feed_text(
                    kind == FeedKind::Atom,
                    &stack,
                    text,
                    &mut title,
                    &mut author,
                    &mut feed_updated,
                    &mut latest_entry,
                );
            }
            Ok(Event::CData(e)) => {
                if let Ok(text) = std::str::from_utf8(e.as_ref()) {
                    let text = text.trim();
                    if !text.is_empty() {
                        handle_feed_text(
                            kind == FeedKind::Atom,
                            &stack,
                            text,
                            &mut title,
                            &mut author,
                            &mut feed_updated,
                            &mut latest_entry,
                        );
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => return Err(SourceError::Malformed),
            _ => {}
        }
        buf.clear();
    }

    if kind == FeedKind::Unknown || entry_count == 0 {
        return Err(SourceError::Malformed);
    }
    let title = title
        .filter(|t| !t.trim().is_empty())
        .ok_or(SourceError::Malformed)?;
    let author = author
        .filter(|a| !a.trim().is_empty())
        .unwrap_or_else(|| "未知作者".into());
    let last_published = latest_entry
        .or(feed_updated)
        .ok_or(SourceError::Malformed)?;
    let work_updated = feed_updated.map_or(last_published, |u| u.max(last_published));
    let work = ObservedWork {
        code: code.to_owned(),
        title,
        author,
        chapter_count: entry_count,
        last_published,
        work_updated,
    };
    if !work.valid() {
        return Err(SourceError::Malformed);
    }
    Ok(work)
}

fn local_tag_name(raw: &[u8]) -> String {
    let s = std::str::from_utf8(raw).unwrap_or("");
    let local = s.rsplit_once(':').map_or(s, |(_, rhs)| rhs);
    local.to_ascii_lowercase()
}

fn handle_feed_text(
    is_atom: bool,
    stack: &[String],
    text: &str,
    title: &mut Option<String>,
    author: &mut Option<String>,
    feed_updated: &mut Option<u64>,
    latest_entry: &mut Option<u64>,
) {
    let path: Vec<&str> = stack.iter().map(String::as_str).collect();
    if is_atom {
        match path.as_slice() {
            ["feed", "title"] if title.is_none() => *title = Some(text.to_owned()),
            ["feed", "author", "name"] if author.is_none() => *author = Some(text.to_owned()),
            ["feed", "updated"] => {
                if let Some(ts) = parse_feed_timestamp(text) {
                    *feed_updated = Some(feed_updated.map_or(ts, |old| old.max(ts)));
                }
            }
            ["feed", "entry", "updated"] | ["feed", "entry", "published"] => {
                if let Some(ts) = parse_feed_timestamp(text) {
                    *latest_entry = Some(latest_entry.map_or(ts, |old| old.max(ts)));
                }
            }
            ["feed", "entry", "author", "name"] if author.is_none() => {
                *author = Some(text.to_owned());
            }
            _ => {}
        }
    } else {
        match path.as_slice() {
            ["rss", "channel", "title"] | ["rdf", "channel", "title"] if title.is_none() => {
                *title = Some(text.to_owned());
            }
            ["rss", "channel", "managingeditor"]
            | ["rss", "channel", "creator"]
            | ["rss", "channel", "author"]
            | ["rdf", "channel", "creator"]
                if author.is_none() =>
            {
                *author = Some(text.to_owned());
            }
            ["rss", "channel", "lastbuilddate"]
            | ["rss", "channel", "pubdate"]
            | ["rss", "channel", "date"]
            | ["rdf", "channel", "date"] => {
                if let Some(ts) = parse_feed_timestamp(text) {
                    *feed_updated = Some(feed_updated.map_or(ts, |old| old.max(ts)));
                }
            }
            ["rss", "channel", "item", "pubdate"]
            | ["rss", "channel", "item", "date"]
            | ["rss", "channel", "item", "updated"]
            | ["rdf", "item", "date"]
            | ["rdf", "item", "pubdate"] => {
                if let Some(ts) = parse_feed_timestamp(text) {
                    *latest_entry = Some(latest_entry.map_or(ts, |old| old.max(ts)));
                }
            }
            ["rss", "channel", "item", "creator"] | ["rss", "channel", "item", "author"]
                if author.is_none() =>
            {
                *author = Some(text.to_owned());
            }
            _ => {}
        }
    }
}

fn parse_feed_timestamp(input: &str) -> Option<u64> {
    let s = input.trim();
    parse_rfc3339(s).or_else(|| parse_rfc2822(s))
}

/// Parses ISO-8601 / RFC-3339 timestamps like `2024-02-29T18:00:00+09:00` or `2024-02-29T09:00:00Z`.
fn parse_rfc3339(input: &str) -> Option<u64> {
    let b = input.as_bytes();
    if b.len() < 20 {
        return None;
    }
    if b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let y: i64 = input.get(0..4)?.parse().ok()?;
    let m: i64 = input.get(5..7)?.parse().ok()?;
    let d: i64 = input.get(8..10)?.parse().ok()?;
    let h: i64 = input.get(11..13)?.parse().ok()?;
    let min: i64 = input.get(14..16)?.parse().ok()?;
    let sec: i64 = input.get(17..19)?.parse().ok()?;
    let mut rest = &input[19..];
    if let Some(after_dot) = rest.strip_prefix('.') {
        let frac_len = after_dot.bytes().take_while(u8::is_ascii_digit).count();
        if frac_len == 0 {
            return None;
        }
        rest = &after_dot[frac_len..];
    }
    let offset_secs: i64 = if rest == "Z" || rest == "z" {
        0
    } else if rest.len() == 6
        && matches!(rest.as_bytes()[0], b'+' | b'-')
        && rest.as_bytes()[3] == b':'
    {
        let sign = if rest.as_bytes()[0] == b'+' { 1 } else { -1 };
        let oh: i64 = rest[1..3].parse().ok()?;
        let om: i64 = rest[4..6].parse().ok()?;
        if oh > 23 || om > 59 {
            return None;
        }
        sign * (oh * 3600 + om * 60)
    } else {
        return None;
    };
    calendar_to_unix(y, m, d, h, min, sec, offset_secs)
}

/// Parses RFC-2822 / RFC-822 timestamps like `Thu, 29 Feb 2024 09:00:00 +0000` or `29 Feb 2024 18:00:00 +0900`.
fn parse_rfc2822(input: &str) -> Option<u64> {
    let rest = match input.split_once(',') {
        Some((_, after)) => after.trim(),
        None => input.trim(),
    };
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() < 5 {
        return None;
    }
    let d: i64 = parts[0].parse().ok()?;
    let m: i64 = match parts[1].to_ascii_lowercase().as_str() {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    };
    let y: i64 = parts[2].parse().ok()?;
    let t: Vec<&str> = parts[3].split(':').collect();
    if t.len() < 2 || t.len() > 3 {
        return None;
    }
    let h: i64 = t[0].parse().ok()?;
    let min: i64 = t[1].parse().ok()?;
    let sec: i64 = if t.len() == 3 { t[2].parse().ok()? } else { 0 };
    let tz = parts[4];
    let offset_secs: i64 = match tz.to_ascii_uppercase().as_str() {
        "GMT" | "UTC" | "UT" | "Z" => 0,
        "JST" => 9 * 3600,
        "EST" => -5 * 3600,
        "EDT" => -4 * 3600,
        "CST" => -6 * 3600,
        "CDT" => -5 * 3600,
        "MST" => -7 * 3600,
        "MDT" => -6 * 3600,
        "PST" => -8 * 3600,
        "PDT" => -7 * 3600,
        _ if tz.len() == 5 && matches!(tz.as_bytes()[0], b'+' | b'-') => {
            let sign = if tz.as_bytes()[0] == b'+' { 1 } else { -1 };
            let oh: i64 = tz[1..3].parse().ok()?;
            let om: i64 = tz[3..5].parse().ok()?;
            if oh > 23 || om > 59 {
                return None;
            }
            sign * (oh * 3600 + om * 60)
        }
        _ => return None,
    };
    calendar_to_unix(y, m, d, h, min, sec, offset_secs)
}

fn calendar_to_unix(
    y: i64,
    m: i64,
    d: i64,
    h: i64,
    min: i64,
    sec: i64,
    offset_secs: i64,
) -> Option<u64> {
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = match m {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return None,
    };
    if !(1970..=9999).contains(&y)
        || !(1..=days).contains(&d)
        || !(0..24).contains(&h)
        || !(0..60).contains(&min)
        || !(0..60).contains(&sec)
    {
        return None;
    }
    let utc =
        crate::time::days_from_civil(y, m, d) * 86400 + h * 3600 + min * 60 + sec - offset_secs;
    u64::try_from(utc).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subscriptions::normalize_code;

    const KAKUYOMU_ATOM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>星辰の記録</title>
  <author><name>夜雨</name></author>
  <updated>2024-02-29T18:00:05+09:00</updated>
  <entry>
    <title>第一話</title>
    <updated>2024-02-28T18:00:00+09:00</updated>
    <summary>SHOULD NEVER BE STORED</summary>
  </entry>
  <entry>
    <title>第二話</title>
    <updated>2024-02-29T18:00:00+09:00</updated>
    <content>BODY PROSE MUST BE IGNORED</content>
  </entry>
</feed>"#;

    const GENERIC_RSS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <channel>
    <title>Chroniques de Paris</title>
    <dc:creator>Camille</dc:creator>
    <lastBuildDate>Thu, 29 Feb 2024 09:00:05 +0000</lastBuildDate>
    <item>
      <title>Chapitre 1</title>
      <pubDate>Wed, 28 Feb 2024 09:00:00 GMT</pubDate>
      <description>Full text ignored</description>
    </item>
    <item>
      <title>Chapitre 2</title>
      <pubDate>Thu, 29 Feb 2024 09:00:00 +0000</pubDate>
      <description>Full text ignored</description>
    </item>
    <item>
      <title>Chapitre 3</title>
      <pubDate>Thu, 29 Feb 2024 09:00:02 +0000</pubDate>
    </item>
  </channel>
</rss>"#;

    #[test]
    fn parses_kakuyomu_atom_metadata_only() {
        let code = normalize_code("https://kakuyomu.jp/works/16817330656114206864/").unwrap();
        assert_eq!(code, "kakuyomu:16817330656114206864");
        let work = parse_feed_metadata(&code, KAKUYOMU_ATOM.as_bytes()).unwrap();
        assert_eq!(work.title, "星辰の記録");
        assert_eq!(work.author, "夜雨");
        assert_eq!(work.chapter_count, 2);
        assert_eq!(work.last_published, 1_709_197_200);
        assert_eq!(work.work_updated, 1_709_197_205);
    }

    #[test]
    fn parses_rss2_metadata_only_and_rejects_empty_or_invalid_feeds() {
        let code = normalize_code("feed:https://novels.example.org/work/feed.xml").unwrap();
        assert_eq!(code, "feed:https://novels.example.org/work/feed.xml");
        let work = parse_feed_metadata(&code, GENERIC_RSS).unwrap();
        assert_eq!(work.title, "Chroniques de Paris");
        assert_eq!(work.author, "Camille");
        assert_eq!(work.chapter_count, 3);
        assert_eq!(work.last_published, 1_709_197_202);
        assert_eq!(work.work_updated, 1_709_197_205);

        assert_eq!(
            parse_feed_metadata(&code, b"<html>not a feed</html>"),
            Err(SourceError::Malformed)
        );
        assert_eq!(
            parse_feed_metadata(
                &code,
                b"<feed><title>Empty</title><updated>2024-02-29T09:00:00Z</updated></feed>"
            ),
            Err(SourceError::Malformed)
        );
    }

    #[test]
    fn rejects_private_ips_and_unsafe_feed_urls() {
        for bad in [
            "https://evil.test/n1234ab/",
            "feed:http://novels.example.org/rss.xml",
            "feed:https://localhost/rss.xml",
            "feed:https://127.0.0.1/rss.xml",
            "feed:https://192.168.1.1/rss.xml",
            "feed:https://evil.test/rss.xml",
            "feed:https://host.local/rss.xml",
            "feed:https://novels.example.org:8443/rss.xml",
            "feed:https://user@novels.example.org/rss.xml",
            "feed:https://novels.example.org/rss.xml?token=1",
            "feed:https://novels.example.org/../secret",
            "https://kakuyomu.jp/works/16817330656114206864/episodes/12345",
            "kakuyomu:123",
        ] {
            assert!(normalize_code(bad).is_err(), "should reject {bad}");
        }
        for private in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "fe80::1",
            "fc00::1",
            "::ffff:127.0.0.1",
        ] {
            let ip: IpAddr = private.parse().unwrap();
            assert!(!is_public_ip(ip), "must reject private IP {private}");
        }
        assert!(is_public_ip("93.184.216.34".parse().unwrap()));
        assert!(is_public_ip(
            "2606:2800:220:1:248:1893:25c8:1946".parse().unwrap()
        ));
    }

    #[test]
    fn composite_routes_each_source_kind() {
        struct Mock(Vec<ObservedWork>);
        impl WorkSource for Mock {
            fn fetch(&self, codes: &[String]) -> Result<Vec<ObservedWork>, SourceError> {
                Ok(self
                    .0
                    .iter()
                    .filter(|w| codes.contains(&w.code))
                    .cloned()
                    .collect())
            }
        }
        let s = Mock(vec![ObservedWork {
            code: "n1234ab".into(),
            title: "S".into(),
            author: "A".into(),
            chapter_count: 1,
            last_published: 100,
            work_updated: 100,
        }]);
        let k = Mock(vec![ObservedWork {
            code: "kakuyomu:16817330656114206864".into(),
            title: "K".into(),
            author: "B".into(),
            chapter_count: 2,
            last_published: 200,
            work_updated: 200,
        }]);
        let f = Mock(vec![ObservedWork {
            code: "feed:https://novels.example.org/rss.xml".into(),
            title: "F".into(),
            author: "C".into(),
            chapter_count: 3,
            last_published: 300,
            work_updated: 300,
        }]);
        let all = fetch_composite(
            &s,
            &k,
            &f,
            &[
                "n1234ab".into(),
                "kakuyomu:16817330656114206864".into(),
                "feed:https://novels.example.org/rss.xml".into(),
            ],
        )
        .unwrap();
        assert_eq!(all.len(), 3);
    }
}
