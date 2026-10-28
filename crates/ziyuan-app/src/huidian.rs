//! 《大明会典》：朝廷现状的快照，打开时据此接着回放。

use ziyuan_governance::Court;
use ziyuan_storage::EventStore;

use crate::Session;

/// 新增至少这么多条事件（且不少于上部会典所收的十分之一）才修一次《大明会典》（快照），
/// 下次打开只回放其后的事件。按比例重修，事件簿再长，修会典的总开销也只与新增的事件成正比。
pub const HUIDIAN_EVERY: usize = 5000;

/// 会典所存：朝廷状态与最近一条时辰。只是缓存，随时可删，删了从头回放。
#[derive(serde::Deserialize)]
pub(crate) struct Huidian {
    pub(crate) clock: Option<u64>,
    pub(crate) court: Court,
}
#[derive(serde::Serialize)]
pub(crate) struct HuidianRef<'a> {
    pub(crate) clock: Option<u64>,
    pub(crate) court: &'a Court,
}

/// 解析会典（读完即释放原文）。从缓冲区解析比从流快近一倍：axum 开了 serde_json 的
/// `raw_value`，流式解析因此逐字节多一道手续，与从头回放一样慢。
pub(crate) fn read_huidian(data: Vec<u8>) -> serde_json::Result<Huidian> {
    serde_json::from_slice(&data)
}

/// 会典由哪一版所修：版本不同即重修，不跨版本沿用状态的格式。
pub(crate) fn huidian_tag() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

impl<S: EventStore> Session<S> {
    /// 自上部会典以来新增够条数（见 [`HUIDIAN_EVERY`]）或须重修时修一部。只是缓存：修不成不影响朝政。
    pub(crate) fn revise_huidian(&mut self) {
        let due = match self.huidian_at {
            usize::MAX => self.len > 0,
            at => self.len >= at + HUIDIAN_EVERY.max(at / 10),
        };
        if !due {
            return;
        }
        let huidian = HuidianRef {
            clock: self.clock,
            court: &self.court,
        };
        let write = |out: &mut dyn std::io::Write| {
            serde_json::to_writer(out, &huidian).map_err(std::io::Error::from)
        };
        if let Ok(true) = self.store.save_snapshot(&huidian_tag(), self.len, &write) {
            let note = format!(
                "史官记：敕修《大明会典》成，收前 {} 条事件；后人开卷，自会典续读实录。",
                self.len
            );
            self.chronicle = Some(match self.chronicle.take() {
                Some(earlier) => format!("{earlier}\n{note}"),
                None => note,
            });
            self.huidian_at = self.len;
        }
    }
}
