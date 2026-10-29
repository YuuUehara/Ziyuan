//! 国库账本：同一次执行的各件诏令共用的度支余额。
//!
//! 每次模型调用前预留（预计输入加输出上限），调用后按实际用量结算并释放预留，
//! 可预留之数为 `余额 - 他件的预留`，故并发诏令不会同时把同一笔余额当作自己的。
//! 预留只在本次执行的内存中：事件簿只记实际用量，重启后预留清零，余额由已记的用量重算。
//! 服务端用量可能超出预留（输入估算偏低、模型不守输出上限），超出部分照实记账：
//! 度支是软上限，账面余额可为负，之后的调用一律不再放行。
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// 等他件释放预留时，每隔这么久查一次是否已撤销。
const WAIT_SLICE: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub struct Treasury {
    ledger: Mutex<Ledger>,
    released: Condvar,
}

#[derive(Debug)]
struct Ledger {
    /// 扣去已花的余额；超支后为负。
    left: i64,
    reserved: u64,
}

impl Ledger {
    fn available(&self) -> u64 {
        (self.left - self.reserved as i64).max(0) as u64
    }
}

impl Treasury {
    pub fn new(left: u64) -> Self {
        Self {
            ledger: Mutex::new(Ledger {
                left: i64::try_from(left).unwrap_or(i64::MAX),
                reserved: 0,
            }),
            released: Condvar::new(),
        }
    }

    fn ledger(&self) -> MutexGuard<'_, Ledger> {
        self.ledger.lock().expect("国库账本锁未中毒")
    }

    /// 扣去已花的余额（不扣预留）；超支后为负。
    pub fn left(&self) -> i64 {
        self.ledger().left
    }

    /// 尚未结算的预留合计。
    pub fn reserved(&self) -> u64 {
        self.ledger().reserved
    }

    /// 记下实际花费，返回记账后的余额。
    pub fn charge(&self, tokens: u64) -> i64 {
        let mut ledger = self.ledger();
        ledger.left -= tokens as i64;
        let left = ledger.left;
        drop(ledger);
        self.released.notify_all();
        left
    }

    /// 在 `min..=max` 内尽量多预留，返回所留之数。可留之数不足 `min` 而他件仍有预留时，
    /// 等其释放后再试；他件都已结算仍不足，或 `cancelled()` 为真，返回 `None`。
    pub fn reserve(&self, min: u64, max: u64, cancelled: &dyn Fn() -> bool) -> Option<u64> {
        let max = max.max(min);
        let mut ledger = self.ledger();
        loop {
            if cancelled() {
                return None;
            }
            let available = ledger.available();
            if available >= min {
                let got = available.min(max);
                ledger.reserved += got;
                return Some(got);
            }
            if ledger.reserved == 0 {
                return None;
            }
            ledger = self
                .released
                .wait_timeout(ledger, WAIT_SLICE)
                .expect("国库账本锁未中毒")
                .0;
        }
    }

    /// 释放 `tokens` 的预留（实际花费另以 [`Self::charge`] 记账）。
    pub fn release(&self, tokens: u64) {
        let mut ledger = self.ledger();
        ledger.reserved = ledger.reserved.saturating_sub(tokens);
        drop(ledger);
        self.released.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::Instant;

    const NEVER: &dyn Fn() -> bool = &|| false;

    #[test]
    fn reserves_as_much_as_it_can_within_bounds() {
        let treasury = Treasury::new(1000);
        assert_eq!(treasury.reserve(100, 300, NEVER), Some(300));
        assert_eq!(treasury.reserve(100, 900, NEVER), Some(700));
        assert_eq!(treasury.reserved(), 1000);
        // 结算一笔：实花 250，释放其预留 300。
        treasury.charge(250);
        treasury.release(300);
        assert_eq!((treasury.left(), treasury.reserved()), (750, 700));
        assert_eq!(treasury.reserve(10, 20, NEVER), Some(20));
        treasury.release(720);
        // 他件都已结算仍不足：立即拒绝，不等。
        assert_eq!(treasury.reserve(751, 800, NEVER), None);
        // 超支后余额为负，一律不放行。
        treasury.charge(800);
        assert_eq!(treasury.left(), -50);
        assert_eq!(treasury.reserve(1, 1, NEVER), None);
    }

    #[test]
    fn waits_for_others_to_settle_then_proceeds() {
        let treasury = Arc::new(Treasury::new(500));
        assert_eq!(treasury.reserve(400, 400, NEVER), Some(400));
        let waiter = {
            let treasury = treasury.clone();
            thread::spawn(move || treasury.reserve(200, 300, NEVER))
        };
        thread::sleep(Duration::from_millis(80));
        assert!(!waiter.is_finished(), "余额被他件预留时应等待");
        // 他件实际只花了 150：余量释放后等待者得以预留。
        treasury.charge(150);
        treasury.release(400);
        assert_eq!(waiter.join().unwrap(), Some(300));
        assert_eq!((treasury.left(), treasury.reserved()), (350, 300));
    }

    #[test]
    fn a_cancelled_waiter_gives_up() {
        let treasury = Arc::new(Treasury::new(100));
        treasury.reserve(100, 100, NEVER).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let waiter = {
            let (treasury, stop) = (treasury.clone(), stop.clone());
            thread::spawn(move || treasury.reserve(50, 50, &|| stop.load(Ordering::SeqCst)))
        };
        thread::sleep(Duration::from_millis(30));
        let started = Instant::now();
        stop.store(true, Ordering::SeqCst);
        assert_eq!(waiter.join().unwrap(), None);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(treasury.reserved(), 100);
    }

    /// 多线程反复「预留 → 花费不超过所留 → 结算」：总花费不超过国库，预留最终归零。
    #[test]
    fn concurrent_callers_never_oversell() {
        const LIMIT: u64 = 2_000;
        let treasury = Arc::new(Treasury::new(LIMIT));
        let workers: Vec<_> = (0..8u64)
            .map(|i| {
                let treasury = treasury.clone();
                thread::spawn(move || {
                    let mut spent = 0;
                    for round in 0..50u64 {
                        let Some(got) = treasury.reserve(20, 120, NEVER) else {
                            break;
                        };
                        assert!(treasury.left() >= 0);
                        thread::yield_now();
                        // 花费在 1..=所留 之间变化。
                        let used = 1 + (i * 31 + round * 17) % got;
                        treasury.charge(used);
                        treasury.release(got);
                        spent += used;
                    }
                    spent
                })
            })
            .collect();
        let spent: u64 = workers.into_iter().map(|w| w.join().unwrap()).sum();
        assert!(spent <= LIMIT, "超卖：花了 {spent}");
        assert_eq!(treasury.left(), (LIMIT - spent) as i64);
        assert_eq!(treasury.reserved(), 0);
    }
}
