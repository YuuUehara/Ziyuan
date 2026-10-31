//! 原子替换的临时文件与租约：命名、归属核验、清理。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 临时文件名前缀；清理器只认这一种命名。
pub(crate) const TEMP_PREFIX: &str = ".ziyuan-tmp-";

/// 本次写入的临时文件名：不含目标文件名，长度有界。
pub(crate) fn fresh_temp_name() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!(
        "{TEMP_PREFIX}{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// 严格匹配 `.ziyuan-tmp-<u32>-<u128>-<u64>`。
pub fn is_candidate_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(TEMP_PREFIX) else {
        return false;
    };
    let parts: Vec<&str> = rest.split('-').collect();
    let [pid, nanos, counter] = parts.as_slice() else {
        return false;
    };
    // 逐字节判十进制（`parse` 会接受 `+1`），再用 parse 判范围。
    pid.parse::<u32>().is_ok()
        && nanos.parse::<u128>().is_ok()
        && counter.parse::<u64>().is_ok()
        && [pid, nanos, counter]
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_names_are_strict() {
        assert!(is_candidate_name(&format!(
            "{TEMP_PREFIX}{}-1-1",
            std::process::id()
        )));
        assert!(is_candidate_name(".ziyuan-tmp-0-0-0"));
        // 前导零合法：协议只要求十进制可解析。
        assert!(is_candidate_name(".ziyuan-tmp-007-0-0"));
        // 各段类型自身的上界必须被接受。
        assert!(is_candidate_name(".ziyuan-tmp-4294967295-0-0"));
        assert!(is_candidate_name(
            ".ziyuan-tmp-0-340282366920938463463374607431768211455-0"
        ));
        assert!(is_candidate_name(".ziyuan-tmp-0-0-18446744073709551615"));
        for bad in [
            // 段数
            ".ziyuan-tmp-1-1",
            ".ziyuan-tmp-1-1-1-1",
            ".ziyuan-tmp-",
            // 空段
            ".ziyuan-tmp--1-1",
            ".ziyuan-tmp-1--1",
            ".ziyuan-tmp-1-1-",
            // 符号与空白：`parse::<u32>()` 会接受 `+1`
            ".ziyuan-tmp-+1-1-1",
            ".ziyuan-tmp-1-+1-1",
            ".ziyuan-tmp- 1-1-1",
            ".ziyuan-tmp-1-1- 1",
            // 非 ASCII 数字
            ".ziyuan-tmp-１-1-1",
            // 超出各段最大值
            ".ziyuan-tmp-4294967296-1-1",
            ".ziyuan-tmp-1-340282366920938463463374607431768211456-1",
            ".ziyuan-tmp-1-1-18446744073709551616",
            // 额外后缀与空白
            ".ziyuan-tmp-1-1-1.bak",
            ".ziyuan-tmp-1-1-1 ",
            // 前缀相似
            ".ziyuan-tmpx-1-1-1",
            "ziyuan-tmp-1-1-1",
            ".ziyuan-lease-0123456789abcdef0123456789abcdef",
        ] {
            assert!(!is_candidate_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn fresh_names_are_candidates_and_distinct() {
        let first = fresh_temp_name();
        let second = fresh_temp_name();
        assert!(is_candidate_name(&first), "{first}");
        assert!(is_candidate_name(&second), "{second}");
        assert_ne!(first, second);
    }
}
