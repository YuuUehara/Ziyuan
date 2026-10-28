//! 时辰：事件时间戳、时区与时间的解析和显示。

use ziyuan_core::CourtEvent;

/// `events` 中最后一条时辰；没有则为 `earlier`。
pub(crate) fn last_clock(events: &[CourtEvent], earlier: Option<u64>) -> Option<u64> {
    events
        .iter()
        .rev()
        .find_map(|e| match e {
            CourtEvent::Clock(t) => Some(*t),
            _ => None,
        })
        .or(earlier)
}

/// 当前 Unix 时间（秒）。
pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 显示时间所用的时区（秒）：`ZIYUAN_TZ` 如 `+08:00`、`-05:30`，未设或不合法为 UTC。
pub fn tz_offset() -> i64 {
    std::env::var("ZIYUAN_TZ")
        .ok()
        .and_then(|s| parse_offset(&s))
        .unwrap_or(0)
}

pub(crate) fn parse_offset(s: &str) -> Option<i64> {
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    let (h, m) = rest.split_once(':').unwrap_or((rest, "0"));
    let (h, m): (i64, i64) = (h.parse().ok()?, m.parse().ok()?);
    (h <= 14 && m < 60).then_some(sign * (h * 3600 + m * 60))
}

/// 公历日期距 1970-01-01 的天数（Howard Hinnant 的算法）。
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub(crate) fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `YYYY-MM-DD HH:MM`（按 `offset` 秒的时区）。
pub fn format_time(at: u64, offset: i64) -> String {
    let local = at as i64 + offset;
    let (y, m, d) = civil_from_days(local.div_euclid(86_400));
    let secs = local.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        secs / 3600,
        secs % 3600 / 60
    )
}

/// 解析起居注筛选的时间：`30m`、`2h`、`3d`（距 `now` 之前），或 `YYYY-MM-DD[ HH:MM]`
/// （按 `offset` 秒的时区）。
pub fn parse_time(input: &str, now: u64, offset: i64) -> Result<u64, String> {
    let bad = || format!("时间不合法：{input}（如 30m、2h、3d、2026-10-25 或 2026-10-25 14:30）");
    let s = input.trim();
    if let Some(unit) = s.chars().last().filter(|c| "mhd".contains(*c)) {
        let n: u64 = s[..s.len() - 1].parse().map_err(|_| bad())?;
        let secs = match unit {
            'm' => 60,
            'h' => 3600,
            _ => 86_400,
        };
        return Ok(now.saturating_sub(n.saturating_mul(secs)));
    }
    let (date, clock) = s.split_once([' ', 'T']).unwrap_or((s, "00:00"));
    let date: Vec<i64> = date
        .split('-')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| bad())?;
    let clock: Vec<i64> = clock
        .split(':')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| bad())?;
    let (&[y, m, d], &[h, min]) = (date.as_slice(), clock.as_slice()) else {
        return Err(bad());
    };
    if !(1..=12).contains(&m)
        || !(1..=31).contains(&d)
        || !(0..24).contains(&h)
        || !(0..60).contains(&min)
    {
        return Err(bad());
    }
    let local = days_from_civil(y, m, d) * 86_400 + h * 3600 + min * 60;
    u64::try_from(local - offset).map_err(|_| bad())
}
