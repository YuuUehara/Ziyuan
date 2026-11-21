//! 用量与价目。

use serde::{Deserialize, Serialize};
use std::fmt;

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// 一次或多次模型调用消耗的 token（以服务端报告为准）。
///
/// `cache_read` 与 `cache_write` 均统计在 `input` 之内，不重复计入 [`TokenUsage::total`]。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    /// 命中服务端提示词缓存的输入 token 数（含在 `input` 内）。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_read: u64,
    /// 新写入服务端提示词缓存的输入 token 数（含在 `input` 内）。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_write: u64,
}
impl TokenUsage {
    pub const fn new(input: u64, output: u64) -> Self {
        Self {
            input,
            output,
            cache_read: 0,
            cache_write: 0,
        }
    }

    pub const fn with_cache(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_write,
        }
    }

    pub fn total(self) -> u64 {
        self.input + self.output
    }

    /// 提示词缓存命中率（0.0–1.0）；未发生缓存命中或无输入时为 `None`。
    pub fn cache_hit_rate(self) -> Option<f64> {
        if self.input == 0 || self.cache_read == 0 {
            return None;
        }
        Some(self.cache_read.min(self.input) as f64 / self.input as f64)
    }
}
impl std::ops::AddAssign for TokenUsage {
    fn add_assign(&mut self, other: Self) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }
}
impl fmt::Display for TokenUsage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "输入 {} / 输出 {} tokens", self.input, self.output)?;
        match (self.cache_read, self.cache_write) {
            (0, 0) => Ok(()),
            (read, 0) => {
                if let Some(rate) = self.cache_hit_rate() {
                    write!(f, "（缓存命中 {read} [{:.0}%]）", rate * 100.0)
                } else {
                    write!(f, "（缓存命中 {read}）")
                }
            }
            (0, write) => write!(f, "（缓存写入 {write}）"),
            (read, write) => {
                if let Some(rate) = self.cache_hit_rate() {
                    write!(
                        f,
                        "（缓存命中 {read} [{:.0}%]，写入 {write}）",
                        rate * 100.0
                    )
                } else {
                    write!(f, "（缓存命中 {read}，写入 {write}）")
                }
            }
        }
    }
}

/// 模型单价：每百万 token 的价格（输入按未命中缓存计）。只用于估算花费，以服务商账单为准。
#[derive(Clone, Debug, PartialEq)]
pub struct Price {
    pub input_per_million: f64,
    pub output_per_million: f64,
    /// 币种，如「元」「USD」。
    pub currency: String,
}
impl Price {
    pub fn cost(&self, usage: TokenUsage) -> f64 {
        (usage.input as f64 * self.input_per_million
            + usage.output as f64 * self.output_per_million)
            / 1_000_000.0
    }

    /// 如「约 0.0123 元」。
    pub fn describe(&self, usage: TokenUsage) -> String {
        format!("约 {:.4} {}", self.cost(usage), self.currency)
    }
}

#[cfg(test)]
mod tests {
    use super::TokenUsage;

    #[test]
    fn token_usage_cache_accounting_and_display_are_backward_compatible() {
        let plain = TokenUsage::new(100, 20);
        assert_eq!(plain.total(), 120);
        assert_eq!(plain.cache_hit_rate(), None);
        assert_eq!(plain.to_string(), "输入 100 / 输出 20 tokens");

        let mut cached = TokenUsage::with_cache(1000, 100, 800, 150);
        assert_eq!(cached.total(), 1100);
        assert!((cached.cache_hit_rate().unwrap() - 0.8).abs() < 1e-9);
        assert_eq!(
            cached.to_string(),
            "输入 1000 / 输出 100 tokens（缓存命中 800 [80%]，写入 150）"
        );
        cached += TokenUsage::with_cache(1000, 50, 200, 0);
        assert_eq!(cached, TokenUsage::with_cache(2000, 150, 1000, 150));
        assert_eq!(
            TokenUsage::with_cache(500, 40, 250, 0).to_string(),
            "输入 500 / 输出 40 tokens（缓存命中 250 [50%]）"
        );
    }
}
