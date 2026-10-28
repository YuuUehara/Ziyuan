//! 用量与价目。

use serde::{Deserialize, Serialize};
use std::fmt;

/// 一次或多次模型调用消耗的 token（以服务端报告为准）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
}
impl TokenUsage {
    pub fn total(self) -> u64 {
        self.input + self.output
    }
}
impl std::ops::AddAssign for TokenUsage {
    fn add_assign(&mut self, other: Self) {
        self.input += other.input;
        self.output += other.output;
    }
}
impl fmt::Display for TokenUsage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "输入 {} / 输出 {} tokens", self.input, self.output)
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
