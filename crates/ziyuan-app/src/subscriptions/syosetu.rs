//! Fixed official ordinary API only; R18 and search-excluded works may be unavailable.
//! Official metadata latency is approximately five minutes on average, up to two hours,
//! not a real-time guarantee. No response links or user-supplied URLs are fetched.
use super::{ObservedWork, SourceError, WorkSource, normalize_code};
use serde::Deserialize;
use std::collections::HashSet;
use std::time::Duration;

const ENDPOINT: &str = "https://api.syosetu.com/novelapi/api/";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// A fresh, credential-free client with a fixed HTTPS endpoint and no redirects or proxy.
/// There is intentionally no configurable production URL or transport.
pub struct SyosetuSource {
    agent: ureq::Agent,
}
impl SyosetuSource {
    pub fn new() -> Self {
        Self {
            agent: ureq::Agent::config_builder()
                .https_only(true)
                .max_redirects(0)
                .proxy(None)
                .timeout_global(Some(Duration::from_secs(30)))
                .http_status_as_error(false)
                .build()
                .into(),
        }
    }
}
impl Default for SyosetuSource {
    fn default() -> Self {
        Self::new()
    }
}
impl WorkSource for SyosetuSource {
    fn fetch(&self, codes: &[String]) -> Result<Vec<ObservedWork>, SourceError> {
        fetch_with(self, codes)
    }
}
trait Transport {
    fn get(&self, url: &str) -> Result<(u16, Vec<u8>), SourceError>;
}
fn transport_error(error: ureq::Error) -> SourceError {
    match error {
        ureq::Error::Timeout(_) => SourceError::Timeout,
        ureq::Error::BodyExceedsLimit(_) => SourceError::Malformed,
        _ => SourceError::Unavailable,
    }
}
impl Transport for SyosetuSource {
    fn get(&self, url: &str) -> Result<(u16, Vec<u8>), SourceError> {
        let mut response = self
            .agent
            .get(url)
            .header("Accept", "application/json")
            .call()
            .map_err(transport_error)?;
        let status = response.status().as_u16();
        // Do not spend memory/time reading an error or redirect body.
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
fn request_url(codes: &[String]) -> Result<String, SourceError> {
    let mut seen = HashSet::new();
    if codes.is_empty()
        || codes.len() > 50
        || codes.iter().any(|c| {
            normalize_code(c).is_err()
                || normalize_code(c).ok().as_ref() != Some(c)
                || !seen.insert(c)
        })
    {
        return Err(SourceError::Malformed);
    }
    // Only ASCII NCode characters and separators enter this query; no URL input is used.
    // lim is mandatory: the official default is only 20 even for larger NCode queries.
    Ok(format!(
        "{ENDPOINT}?out=json&of=t-n-w-ga-gl-nu&ncode={}&lim={}",
        codes.join("-"),
        codes.len()
    ))
}
fn fetch_with(
    transport: &impl Transport,
    codes: &[String],
) -> Result<Vec<ObservedWork>, SourceError> {
    let (status, body) = transport.get(&request_url(codes)?)?;
    match status {
        200 => parse_response(&body, codes),
        404 => Err(SourceError::NotFound),
        429 => Err(SourceError::RateLimited),
        _ => Err(SourceError::Unavailable),
    }
}
#[derive(Deserialize)]
struct ApiWork {
    ncode: String,
    title: String,
    writer: String,
    general_all_no: u64,
    general_lastup: String,
    novelupdated_at: String,
}
struct ApiResponse {
    count: u64,
    entries: Vec<ApiWork>,
}
impl<'de> Deserialize<'de> for ApiResponse {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ResponseVisitor;
        impl<'de> serde::de::Visitor<'de> for ResponseVisitor {
            type Value = ApiResponse;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an allcount header followed by at most 50 works")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                #[derive(Deserialize)]
                struct Header {
                    allcount: u64,
                }
                let header: Header = seq
                    .next_element()?
                    .ok_or_else(|| serde::de::Error::custom("missing allcount"))?;
                let mut entries = Vec::new();
                while let Some(entry) = seq.next_element::<ApiWork>()? {
                    if entries.len() == 50 {
                        return Err(serde::de::Error::custom("too many works"));
                    }
                    entries.push(entry);
                }
                Ok(ApiResponse {
                    count: header.allcount,
                    entries,
                })
            }
        }
        deserializer.deserialize_seq(ResponseVisitor)
    }
}
fn parse_response(bytes: &[u8], codes: &[String]) -> Result<Vec<ObservedWork>, SourceError> {
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(SourceError::Malformed);
    }
    let ApiResponse { count, entries } =
        serde_json::from_slice(bytes).map_err(|_| SourceError::Malformed)?;
    if count != entries.len() as u64 || entries.len() > codes.len() || entries.len() > 50 {
        return Err(SourceError::Malformed);
    }
    let mut found = HashSet::new();
    entries
        .into_iter()
        .map(|work| {
            let code = normalize_code(&work.ncode).map_err(|_| SourceError::Malformed)?;
            // Unlike add, the API ncode field must be a code, not a URL.
            if !work.ncode.eq_ignore_ascii_case(&code)
                || !codes.contains(&code)
                || !found.insert(code.clone())
            {
                return Err(SourceError::Malformed);
            }
            let work = ObservedWork {
                code,
                title: work.title,
                author: work.writer,
                chapter_count: work.general_all_no,
                last_published: parse_japan_time(&work.general_lastup)?,
                work_updated: parse_japan_time(&work.novelupdated_at)?,
            };
            if !work.valid() {
                return Err(SourceError::Malformed);
            }
            Ok(work)
        })
        .collect()
}

/// Strict YYYY-MM-DD HH:MM:SS, in JST (UTC+09:00, no daylight saving).
fn parse_japan_time(input: &str) -> Result<u64, SourceError> {
    let b = input.as_bytes();
    if b.len() != 19
        || b.iter().enumerate().any(|(i, c)| match i {
            4 | 7 => *c != b'-',
            10 => *c != b' ',
            13 | 16 => *c != b':',
            _ => !c.is_ascii_digit(),
        })
    {
        return Err(SourceError::Malformed);
    }
    let number = |start, end| {
        input[start..end]
            .parse::<i64>()
            .map_err(|_| SourceError::Malformed)
    };
    let (y, m, d) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (h, min, s) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
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
        _ => return Err(SourceError::Malformed),
    };
    if !(1970..=9999).contains(&y)
        || !(1..=days).contains(&d)
        || !(0..24).contains(&h)
        || !(0..60).contains(&min)
        || !(0..60).contains(&s)
    {
        return Err(SourceError::Malformed);
    }
    let utc = crate::time::days_from_civil(y, m, d) * 86400 + h * 3600 + min * 60 + s - 9 * 3600;
    u64::try_from(utc).map_err(|_| SourceError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn duplicate_required_json_fields_are_malformed() {
        let codes = vec!["n1234ab".into()];
        assert_eq!(
            parse_response(br#"[{"allcount":0,"allcount":0}]"#, &codes),
            Err(SourceError::Malformed)
        );
        let response = String::from_utf8(body(&codes)).unwrap();
        let response = response.replace(
            "\"general_all_no\":3",
            "\"general_all_no\":3,\"general_all_no\":3",
        );
        assert_eq!(
            parse_response(response.as_bytes(), &codes),
            Err(SourceError::Malformed)
        );
    }

    fn codes(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("n{i:04}ab")).collect()
    }
    fn entry(code: &str) -> Value {
        json!({"ncode":code.to_ascii_uppercase(),"title":"<arbitrary>","writer":"作者","general_all_no":3,"general_lastup":"2024-02-29 09:00:00","novelupdated_at":"2024-02-29 09:00:01","global_point":100,"updated_at":"ignored system timestamp","future_field":{"url":"http://evil.test"}})
    }
    fn body(codes: &[String]) -> Vec<u8> {
        let mut values = vec![json!({"allcount": codes.len()})];
        values.extend(codes.iter().map(|c| entry(c)));
        serde_json::to_vec(&values).unwrap()
    }
    struct Memory {
        status: u16,
        body: Vec<u8>,
        expected: String,
    }
    impl Transport for Memory {
        fn get(&self, url: &str) -> Result<(u16, Vec<u8>), SourceError> {
            assert_eq!(url, self.expected);
            Ok((self.status, self.body.clone()))
        }
    }

    #[test]
    fn request_contract_includes_explicit_lim_and_full_21_and_50_work_responses() {
        for n in [21, 50] {
            let codes = codes(n);
            let expected = format!(
                "https://api.syosetu.com/novelapi/api/?out=json&of=t-n-w-ga-gl-nu&ncode={}&lim={n}",
                codes.join("-")
            );
            let transport = Memory {
                status: 200,
                body: body(&codes),
                expected,
            };
            assert_eq!(fetch_with(&transport, &codes).unwrap().len(), n);
        }
        for bad in [
            vec![],
            codes(51),
            vec!["https://evil.test/".into()],
            vec!["N1234AB".into()],
            vec!["n1234ab".into(), "n1234ab".into()],
        ] {
            assert!(request_url(&bad).is_err());
        }
    }

    #[test]
    fn production_config_has_no_redirect_proxy_auth_and_total_timeout() {
        let source = SyosetuSource::new();
        let config = source.agent.config();
        assert!(config.https_only());
        assert_eq!(config.max_redirects(), 0);
        assert!(config.proxy().is_none());
        assert_eq!(
            config.timeouts().global,
            Some(std::time::Duration::from_secs(30))
        );
    }

    #[test]
    fn header_first_required_fields_wrong_codes_duplicates_and_truncation_are_rejected() {
        let requested = vec!["n1234ab".into()];
        let w = entry("n1234ab");
        let mut cases = vec![
            json!([]),
            json!({}),
            json!([w.clone()]),
            json!([{"allcount": "1"},w.clone()]),
            json!([{"allcount": -1},w.clone()]),
            json!([{"allcount": 0},w.clone()]),
            json!([{"allcount": 2}, w.clone()]),
            json!([{"allcount": 2},w.clone(),w.clone()]),
            json!([{"allcount": 1},entry("n9999z")]),
        ];
        for key in [
            "ncode",
            "title",
            "writer",
            "general_all_no",
            "general_lastup",
            "novelupdated_at",
        ] {
            let mut bad = w.clone();
            bad.as_object_mut().unwrap().remove(key);
            cases.push(json!([{"allcount":1},bad]));
        }
        for value in [json!(0), json!(-1), json!(1.2), json!("3"), json!(u64::MAX)] {
            let mut bad = w.clone();
            bad["general_all_no"] = value;
            cases.push(json!([{"allcount":1},bad]));
        }
        for case in cases {
            assert_eq!(
                parse_response(&serde_json::to_vec(&case).unwrap(), &requested),
                Err(SourceError::Malformed),
                "{case}"
            );
        }
        assert!(parse_response(b"[{\"allcount\":1},", &requested).is_err());
        assert!(parse_response(&vec![b' '; MAX_RESPONSE_BYTES + 1], &requested).is_err());
        let many = codes(21);
        let mut truncated: Value = serde_json::from_slice(&body(&many)).unwrap();
        truncated.as_array_mut().unwrap().pop();
        assert_eq!(
            parse_response(&serde_json::to_vec(&truncated).unwrap(), &many),
            Err(SourceError::Malformed)
        );
    }

    #[test]
    fn missing_results_are_not_found_and_score_system_fields_are_ignored() {
        let codes = vec!["n1234ab".into(), "n9999z".into()];
        assert!(
            parse_response(br#"[{"allcount":0}]"#, &codes)
                .unwrap()
                .is_empty()
        );
        let a = entry("n1234ab");
        let mut b = a.clone();
        b["global_point"] = json!(999);
        b["updated_at"] = json!("changed");
        let parse = |w| {
            parse_response(
                &serde_json::to_vec(&json!([{"allcount":1},w])).unwrap(),
                &codes,
            )
            .unwrap()
        };
        assert_eq!(parse(a), parse(b));
        assert_eq!(
            parse_response(
                &serde_json::to_vec(&json!([{"allcount":1},entry("n1234ab")])).unwrap(),
                &codes
            )
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn japan_time_validates_calendar_and_converts_seconds() {
        assert_eq!(
            parse_japan_time("2024-02-29 09:00:00").unwrap(),
            1_709_164_800
        );
        assert_eq!(
            parse_japan_time("2024-02-29 00:00:00").unwrap(),
            1_709_132_400
        );
        assert_eq!(
            parse_japan_time("2000-02-29 09:00:00").unwrap(),
            951_782_400
        );
        for s in [
            "1900-02-29 09:00:00",
            "2023-02-29 00:00:00",
            "2024-04-31 00:00:00",
            "2024-13-01 00:00:00",
            "2024-00-01 00:00:00",
            "2024-01-00 00:00:00",
            "2024-01-01 24:00:00",
            "2024-01-01 00:60:00",
            "2024-01-01 00:00:60",
            "2024-2-29 09:00:00",
            "2024-02-29T09:00:00",
            "2024-02-29 09:00:00+09:00",
            "２０２４-02-29 00:00:00",
        ] {
            assert!(parse_japan_time(s).is_err(), "{s}");
        }
    }

    #[test]
    fn http_status_and_transport_failures_are_explicit() {
        let codes = vec!["n1234ab".into()];
        for (status, error) in [
            (429, SourceError::RateLimited),
            (404, SourceError::NotFound),
            (500, SourceError::Unavailable),
            (302, SourceError::Unavailable),
        ] {
            let transport = Memory {
                status,
                body: vec![],
                expected: request_url(&codes).unwrap(),
            };
            assert_eq!(fetch_with(&transport, &codes), Err(error));
        }
        struct Timeout;
        impl Transport for Timeout {
            fn get(&self, _: &str) -> Result<(u16, Vec<u8>), SourceError> {
                Err(SourceError::Timeout)
            }
        }
        assert_eq!(fetch_with(&Timeout, &codes), Err(SourceError::Timeout));
        let bad = Memory {
            status: 200,
            body: b"not json".to_vec(),
            expected: request_url(&codes).unwrap(),
        };
        assert_eq!(fetch_with(&bad, &codes), Err(SourceError::Malformed));
    }
}
