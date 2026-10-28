//! 回奏的评定与复核：回奏格式、御批、都察院的结论、密报与证据。

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::ParseError;

/// 礼部回奏体例：回奏须分这三段，各段以标题开头（如「结论：」「## 依据」）。
/// 礼部修 CHANGELOG 时唯一可写的文件。
pub const CHANGELOG: &str = "CHANGELOG.md";

pub const REPORT_SECTIONS: [&str; 3] = ["结论", "依据", "未办之事"];

/// 回奏缺少的体例段落（按 [`REPORT_SECTIONS`] 的顺序）；合乎体例时为空。
/// 段落标题须在行首，可带 Markdown 标记或序号；「未办之事」也可写作「未办」。
pub fn missing_sections(report: &str) -> Vec<&'static str> {
    let heads: Vec<&str> = report
        .lines()
        .map(|line| {
            line.trim_start_matches(|c: char| {
                c.is_whitespace() || "#*>-【[（(一二三四1234.、)）".contains(c)
            })
        })
        .collect();
    REPORT_SECTIONS
        .into_iter()
        .filter(|section| {
            let key = if *section == "未办之事" {
                "未办"
            } else {
                section
            };
            !heads.iter().any(|h| h.starts_with(key))
        })
        .collect()
}

/// 皇帝对一件已完成诏令的评定：吏部考绩只采信经评定的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    Accepted,
    Rejected,
}
impl Verdict {
    pub fn code(self) -> &'static str {
        match self {
            Self::Accepted => "accept",
            Self::Rejected => "reject",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Accepted => "采纳",
            Self::Rejected => "驳回",
        }
    }
}
impl FromStr for Verdict {
    type Err = ParseError;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "accept" => Ok(Self::Accepted),
            "reject" => Ok(Self::Rejected),
            _ => Err(ParseError::UnknownVerdict(input.into())),
        }
    }
}

/// 都察院对一件回奏的结论。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CensorVerdict {
    Endorsed,
    Impeached,
}
impl CensorVerdict {
    pub fn code(self) -> &'static str {
        match self {
            Self::Endorsed => "endorse",
            Self::Impeached => "impeach",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Endorsed => "背书",
            Self::Impeached => "弹劾",
        }
    }
}
impl FromStr for CensorVerdict {
    type Err = ParseError;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "endorse" => Ok(Self::Endorsed),
            "impeach" => Ok(Self::Impeached),
            _ => Err(ParseError::UnknownVerdict(input.into())),
        }
    }
}

/// 刑部与锦衣卫检出的可疑内容类别（按关键词机械检出，只标记、不拦截）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Threat {
    /// 劫持指令：外来文书或回奏中出现「忽略之前的指令」之类，企图改写官员的职守。
    Hijack,
    /// 索取机密：要求读取 `.env`、交出 API Key、令牌等。
    Secret,
    /// 越权：要求修改或扩大玉玺、提升权限。
    Escalate,
}
impl Threat {
    pub const ALL: [Self; 3] = [Self::Hijack, Self::Secret, Self::Escalate];
    pub fn code(self) -> &'static str {
        match self {
            Self::Hijack => "hijack",
            Self::Secret => "secret",
            Self::Escalate => "escalate",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Hijack => "劫持指令",
            Self::Secret => "索取机密",
            Self::Escalate => "越权请求",
        }
    }
}
impl FromStr for Threat {
    type Err = ParseError;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|t| t.code() == input)
            .ok_or_else(|| ParseError::UnknownThreat(input.into()))
    }
}

/// 可机械核验的证据：回奏引用的文件（及行号）是否确实存在，最近一次 `run_tests` 是否通过。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// 回奏中引用的文件位置数（带行号的引用，加上查无此文件的路径）。
    pub cited: u32,
    /// 其中查无实据（文件不存在或行号越界）的。
    pub missing: u32,
    /// 最近一次 `run_tests` 的结果；未跑测试为 `None`。
    pub tests: Option<bool>,
}
impl Evidence {
    /// 有据可查：引用全部属实且至少有一项可验证的证据（引用或通过的测试），测试没有失败。
    pub fn verified(self) -> bool {
        self.missing == 0
            && self.tests != Some(false)
            && (self.cited > 0 || self.tests == Some(true))
    }
}
impl fmt::Display for Evidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.cited == 0 {
            f.write_str("回奏未引用代码位置")?;
        } else {
            write!(f, "引用 {} 处，查无实据 {} 处", self.cited, self.missing)?;
        }
        match self.tests {
            None => f.write_str("；未跑测试"),
            Some(true) => f.write_str("；测试通过"),
            Some(false) => f.write_str("；测试未通过"),
        }
    }
}
