//! 覆盖水位：构建开始时记下，发布前重比，不一致就整批放弃。
//!
//! 水位类型只有一处定义：复用 [`crate::record::Watermark`]，**不另立第二个类型**。
//! 本模块只放判断，不放字段。
//!
//! 判定分两半，合起来才是「资料在构建期间有没有变」：
//!
//! - `watermark_is_unchanged` 比对资料面的三项（事件条数、事件历史摘要、历史字节数）；
//! - `require_stable_generation` 单独看世代是否为偶数——奇数说明有未完成的受管写入。
//!
//! 为什么世代不并进前者：世代的第 8 位含义（写入中／已落定）与「资料到哪一步」是两件事，
//! 混在一个 bool 里就分不清该报「资料变了」还是「有写入没做完」。
//!
//! 两半由 [`require_publishable`] 合并成发布前的唯一入口：调用方只需比一对水位，
//! 不必记得「还得看一眼世代」。因此那两个单项判断都是 `pub(crate)`——外部只能走合并的那一道。

use crate::MemoryError;
pub use crate::record::Watermark;

/// 读取当前水位；由调用方传入，便于测试注入。
///
/// 生产调用点给的是「现在真的去读事件簿与世代」的闭包；测试给的是一个反例闭包，
/// 于是「构建期间资料被改了」这件事不必靠真实并发来复现。
pub type WatermarkReader<'a> = &'a dyn Fn() -> Result<Watermark, MemoryError>;

/// 构建期间资料是否有变：`events`、`history_sha256`、`history_bytes` 三项都必须相等。
///
/// 只有事件条数相同不够：历史可以被重写（同一位置换了内容），因此摘要必须一起比。
pub(crate) fn watermark_is_unchanged(before: &Watermark, after: &Watermark) -> bool {
    before.events == after.events
        && before.history_sha256 == after.history_sha256
        && before.history_bytes == after.history_bytes
}

/// 世代须为偶数；奇数表示工作区有未完成的受管写入，此时不得发布索引。
pub(crate) fn require_stable_generation(generation: u64) -> Result<(), MemoryError> {
    if generation % 2 == 1 {
        return Err(MemoryError::RecoveryPending { generation });
    }
    Ok(())
}

/// 发布前的最后一道核对：构建期间资料有变，或工作区处于未收尾状态，一律不切换。
/// 读取器可注入，便于在不造真实并发的前提下测「构建期间事件簿变了」。
///
/// 次序是刻意的：
///
/// 1. 先看**两端**的世代（`before` 与 `after` 都要偶数）。构建开始时有半截写入，
///    那么这次构建读到的资料本身就不完整；结束时若还是奇数，说明又有新写入没收尾。
///    这两种情况都报 [`MemoryError::RecoveryPending`]，让人先去 `text recovery status`
///    收尾，而不是重试一次再失败一次。
/// 2. 再看资料面三项是否一致，不一致报 [`MemoryError::Stale`] 并**点名是哪一项漂了**，
///    免得只看到一句「有变」却不知道该等什么。
///
/// 消息里只出现数字与截断到 8 位的摘要，绝不出现路径。
pub fn require_publishable(before: &Watermark, after: &Watermark) -> Result<(), MemoryError> {
    require_stable_generation(before.generation)?;
    require_stable_generation(after.generation)?;
    if watermark_is_unchanged(before, after) {
        return Ok(());
    }
    Err(MemoryError::Stale {
        reason: drift_reason(before, after),
    })
}

/// 逐项说清哪里漂了；多项同时漂就都列出来。
fn drift_reason(before: &Watermark, after: &Watermark) -> String {
    let mut parts: Vec<String> = Vec::new();
    if before.events != after.events {
        parts.push(format!("events {}→{}", before.events, after.events));
    }
    if before.history_sha256 != after.history_sha256 {
        parts.push(format!(
            "history_sha256 由 {} 变为 {}",
            short_digest(&before.history_sha256),
            short_digest(&after.history_sha256)
        ));
    }
    if before.history_bytes != after.history_bytes {
        parts.push(format!(
            "history_bytes {}→{}",
            before.history_bytes, after.history_bytes
        ));
    }
    parts.join("、")
}

/// 摘要只留前 8 位：够认出「换了哪一版」，又不把整串摘要写进日志。
fn short_digest(digest: &str) -> String {
    digest.chars().take(8).collect()
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

    /// 奇数世代让人先去收尾，而不是「资料变了」——两件事的下一步不同。
    #[test]
    fn odd_generation_reports_an_unfinished_managed_write() {
        for generation in [1, 3, 7] {
            assert_eq!(
                require_stable_generation(generation),
                Err(MemoryError::RecoveryPending { generation })
            );
        }
    }

    #[test]
    fn unchanged_watermarks_are_publishable() {
        assert_eq!(require_publishable(&watermark(), &watermark()), Ok(()));
    }

    /// 逐项点名漂移的字段；摘要只留前 8 位，消息里没有路径。
    #[test]
    fn drift_names_the_field_that_moved() {
        let before = watermark();
        let mut events = before.clone();
        events.events += 1;
        assert_eq!(
            require_publishable(&before, &events),
            Err(MemoryError::Stale {
                reason: "events 12→13".into()
            })
        );

        let mut digest = before.clone();
        digest.history_sha256 = sha256_hex(b"rewritten history");
        match require_publishable(&before, &digest) {
            Err(MemoryError::Stale { reason }) => {
                assert!(reason.starts_with("history_sha256 由 "), "{reason}");
                assert!(reason.contains(&before.history_sha256[..8]), "{reason}");
                assert!(reason.contains(&digest.history_sha256[..8]), "{reason}");
                // 截断：不把整串摘要写进日志。
                assert!(!reason.contains(&before.history_sha256), "{reason}");
            }
            other => panic!("预期资料漂移被拒，得到 {other:?}"),
        }

        let mut bytes = before.clone();
        bytes.history_bytes = 120;
        assert_eq!(
            require_publishable(&before, &bytes),
            Err(MemoryError::Stale {
                reason: "history_bytes 4096→120".into()
            })
        );
    }

    /// 多项同时漂移时都列出来，用顿号分隔。
    #[test]
    fn several_drifting_fields_are_all_named() {
        let before = watermark();
        let mut after = before.clone();
        after.events = 13;
        after.history_bytes = 120;
        assert_eq!(
            require_publishable(&before, &after),
            Err(MemoryError::Stale {
                reason: "events 12→13、history_bytes 4096→120".into()
            })
        );
    }

    /// 两端任一端有未收尾的写入：报 `RecoveryPending`（而不是 `Stale`），并带上世代号。
    #[test]
    fn unfinished_writes_at_either_end_outrank_material_drift() {
        let even = watermark();
        let mut odd_before = even.clone();
        odd_before.generation = 9;
        let mut odd_after = even.clone();
        odd_after.generation = 11;
        // 同时让资料漂移：世代的问题在先，所以报的仍是「先收尾」。
        odd_after.events += 1;

        assert_eq!(
            require_publishable(&odd_before, &even),
            Err(MemoryError::RecoveryPending { generation: 9 })
        );
        assert_eq!(
            require_publishable(&even, &odd_after),
            Err(MemoryError::RecoveryPending { generation: 11 })
        );
    }
}
