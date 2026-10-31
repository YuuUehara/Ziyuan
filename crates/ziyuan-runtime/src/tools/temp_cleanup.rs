//! 原子替换的临时文件与租约：命名、归属核验、清理。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 临时文件名前缀；清理器只认这一种命名。
pub(crate) const TEMP_PREFIX: &str = ".ziyuan-tmp-";

/// 本次写入的临时文件名：不含目标文件名，长度有界——目标名接近文件名上限时也能建出临时文件。
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
///
/// 这是删除的准入门槛，不是提示：放宽一点就会把用户的普通文件当成自己的残留删掉。
pub fn is_candidate_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(TEMP_PREFIX) else {
        return false;
    };
    let mut parts = rest.split('-');
    let (Some(pid), Some(nanos), Some(counter), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    // 先逐字节判十进制：`parse` 会接受 `+1`，也会接受全角数字之外的形式。
    // 再用 parse 判范围：逐字节判断管不了溢出。
    [pid, nanos, counter]
        .iter()
        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        && pid.parse::<u32>().is_ok()
        && nanos.parse::<u128>().is_ok()
        && counter.parse::<u64>().is_ok()
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
        let maxima = format!("{TEMP_PREFIX}{}-{}-{}", u32::MAX, u128::MAX, u64::MAX);
        assert!(is_candidate_name(&maxima), "{maxima}");
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
        // 各段上界再 +1 必须被拒绝。u128 无法在类型内加一，故直接写 `u128::MAX + 1` 的字面量。
        for overflow in [
            format!("{TEMP_PREFIX}{}-1-1", u32::MAX as u64 + 1),
            format!(
                "{TEMP_PREFIX}1-{}-1",
                "340282366920938463463374607431768211456"
            ),
            format!("{TEMP_PREFIX}1-1-{}", u64::MAX as u128 + 1),
        ] {
            assert!(!is_candidate_name(&overflow), "{overflow}");
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
