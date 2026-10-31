//! 覆盖水位：构建开始时记下，发布前重比，不一致就整批放弃。
//!
//! 水位类型只有一处定义：复用 [`crate::record::Watermark`]，**不另立第二个类型**。
//! 本模块只放判断，不放字段。
//!
//! 判定分两半，合起来才是「资料在构建期间有没有变」：
//!
//! - [`watermark_is_unchanged`] 比对资料面的三项（事件条数、事件历史摘要、历史字节数）；
//! - [`require_stable_generation`] 单独看世代是否为偶数——奇数说明有未完成的受管写入。
//!
//! 为什么世代不并进前者：世代的第 8 位含义（写入中／已落定）与「资料到哪一步」是两件事，
//! 混在一个 bool 里就分不清该报「资料变了」还是「有写入没做完」。

use crate::MemoryError;
pub use crate::record::Watermark;

/// 构建期间资料是否有变：`events`、`history_sha256`、`history_bytes` 三项都必须相等。
///
/// 只有事件条数相同不够：历史可以被重写（同一位置换了内容），因此摘要必须一起比。
pub fn watermark_is_unchanged(before: &Watermark, after: &Watermark) -> bool {
    before.events == after.events
        && before.history_sha256 == after.history_sha256
        && before.history_bytes == after.history_bytes
}

/// 世代须为偶数；奇数表示工作区有未完成的受管写入，此时不得发布索引。
pub fn require_stable_generation(generation: u64) -> Result<(), MemoryError> {
    if generation % 2 == 1 {
        return Err(MemoryError::Stale {
            reason: format!("工作区世代 {generation} 为奇数，存在未完成的受管写入"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::sha256_hex;

    fn watermark() -> Watermark {
        Watermark {
            events: 12,
            history_sha256: sha256_hex(b"history"),
            history_bytes: 4096,
            generation: 8,
        }
    }

    #[test]
    fn identical_watermarks_are_unchanged() {
        assert!(watermark_is_unchanged(&watermark(), &watermark()));
    }

    #[test]
    fn any_of_the_three_fields_differing_counts_as_changed() {
        let before = watermark();
        let mut events = before.clone();
        events.events += 1;
        assert!(!watermark_is_unchanged(&before, &events));

        let mut digest = before.clone();
        digest.history_sha256 = sha256_hex(b"rewritten history");
        assert!(!watermark_is_unchanged(&before, &digest));

        let mut bytes = before.clone();
        bytes.history_bytes += 1;
        assert!(!watermark_is_unchanged(&before, &bytes));
    }

    /// 事件条数与摘要都相同、只有世代不同：资料面没变（世代由
    /// [`require_stable_generation`] 单独看），因此这一项不算「资料变了」。
    #[test]
    fn generation_alone_does_not_change_the_material_watermark() {
        let before = watermark();
        let mut after = before.clone();
        after.generation = before.generation + 2;
        assert!(watermark_is_unchanged(&before, &after));
    }

    #[test]
    fn even_generations_are_stable() {
        for generation in [0, 2, 8, u64::MAX - 1] {
            require_stable_generation(generation).unwrap();
        }
    }

    #[test]
    fn odd_generation_reports_an_unfinished_managed_write() {
        for generation in [1, 3, 7] {
            match require_stable_generation(generation) {
                Err(MemoryError::Stale { reason }) => {
                    assert!(reason.contains("未完成的受管写入"), "{reason}");
                    assert!(reason.contains(&generation.to_string()), "{reason}");
                }
                other => panic!("世代 {generation} 预期被拒，得到 {other:?}"),
            }
        }
    }
}
