//! 官制：六部、政体、官职与预算范围。

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::{ParseError, TaskId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Department {
    Personnel,
    Revenue,
    Rites,
    War,
    Justice,
    Works,
}

impl Department {
    pub const ALL: [Self; 6] = [
        Self::Personnel,
        Self::Revenue,
        Self::Rites,
        Self::War,
        Self::Justice,
        Self::Works,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Self::Personnel => "personnel",
            Self::Revenue => "revenue",
            Self::Rites => "rites",
            Self::War => "war",
            Self::Justice => "justice",
            Self::Works => "works",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Personnel => "吏部",
            Self::Revenue => "户部",
            Self::Rites => "礼部",
            Self::War => "兵部",
            Self::Justice => "刑部",
            Self::Works => "工部",
        }
    }
}
impl fmt::Display for Department {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl FromStr for Department {
    type Err = ParseError;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input.to_ascii_lowercase().as_str() {
            "personnel" => Ok(Self::Personnel),
            "revenue" => Ok(Self::Revenue),
            "rites" => Ok(Self::Rites),
            "war" => Ok(Self::War),
            "justice" => Ok(Self::Justice),
            "works" => Ok(Self::Works),
            _ => Err(ParseError::UnknownDepartment(input.into())),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovernanceMode {
    Chancellor,
    DirectRule,
    Cabinet,
}
impl GovernanceMode {
    pub const ALL: [Self; 3] = [Self::Chancellor, Self::DirectRule, Self::Cabinet];

    pub fn code(self) -> &'static str {
        match self {
            Self::Chancellor => "chancellor",
            Self::DirectRule => "direct",
            Self::Cabinet => "cabinet",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Chancellor => "丞相制",
            Self::DirectRule => "废相亲政",
            Self::Cabinet => "内阁制",
        }
    }
}
impl fmt::Display for GovernanceMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl FromStr for GovernanceMode {
    type Err = ParseError;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input.to_ascii_lowercase().as_str() {
            "chancellor" => Ok(Self::Chancellor),
            "direct" => Ok(Self::DirectRule),
            "cabinet" => Ok(Self::Cabinet),
            _ => Err(ParseError::UnknownMode(input.into())),
        }
    }
}

/// 官职。协调机构的官职只在对应制度下设置，六部官职在各制度下都保留（品级随制度变化）：
///
/// - 丞相制（仿元·中书省）：中书右丞相为首，中书左丞相为副；
/// - 废相亲政（仿明初，洪武十三年罢中书省）：不设协调机构，皇帝直领六部；
/// - 内阁制（仿明中后期）：内阁首辅为首，内阁次辅为副。
///
/// 每个机构的正职空缺时由副职署理；六部尚书空缺时由侍郎署理。
///
/// 都察院左都御史（元为御史台御史大夫）各制度都设，独立于派工路线，专司复核回奏、纠劾百官。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Office {
    /// 中书右丞相（元以右为尊）。
    Chancellor,
    /// 中书左丞相。
    ViceChancellor,
    /// 内阁首辅。
    FirstGrandSecretary,
    /// 内阁次辅。
    SecondGrandSecretary,
    /// 都察院左都御史：复核回奏，出具背书或弹劾。
    Censor,
    /// 尚书：一部之长。
    Minister(Department),
    /// 侍郎：一部之副。
    ViceMinister(Department),
    /// 郎中：承办具体事务，可有多人。
    Clerk(Department),
}
impl Office {
    /// 全部官职：先协调机构，再按部门列出尚书、侍郎、郎中。
    pub fn all() -> Vec<Self> {
        let mut offices = vec![
            Self::Chancellor,
            Self::ViceChancellor,
            Self::FirstGrandSecretary,
            Self::SecondGrandSecretary,
            Self::Censor,
        ];
        for d in Department::ALL {
            offices.extend([Self::Minister(d), Self::ViceMinister(d), Self::Clerk(d)]);
        }
        offices
    }

    /// 只在某一制度下设置的协调官职返回该制度；六部官职返回 `None`。
    pub fn mode(self) -> Option<GovernanceMode> {
        match self {
            Self::Chancellor | Self::ViceChancellor => Some(GovernanceMode::Chancellor),
            Self::FirstGrandSecretary | Self::SecondGrandSecretary => Some(GovernanceMode::Cabinet),
            Self::Censor | Self::Minister(_) | Self::ViceMinister(_) | Self::Clerk(_) => None,
        }
    }

    /// 现行制度 `mode` 下是否设置此官职。
    pub fn exists_in(self, mode: GovernanceMode) -> bool {
        self.mode().is_none_or(|m| m == mode)
    }

    /// 同一时刻只能有一人担任（郎中可有多人）。
    pub fn is_unique(self) -> bool {
        !matches!(self, Self::Clerk(_))
    }

    /// 品级。元代六部隶中书省，尚书正三品；明洪武十三年罢中书省后升为正二品。
    /// 内阁大学士本官仅正五品，其权出于票拟而非品级。
    pub fn rank(self, mode: GovernanceMode) -> &'static str {
        let yuan = mode == GovernanceMode::Chancellor;
        match self {
            Self::Chancellor | Self::ViceChancellor => "正一品",
            Self::FirstGrandSecretary | Self::SecondGrandSecretary => "正五品",
            // 元御史大夫从一品；明左都御史正二品。
            Self::Censor if yuan => "从一品",
            Self::Censor => "正二品",
            Self::Minister(_) if yuan => "正三品",
            Self::Minister(_) => "正二品",
            Self::ViceMinister(_) if yuan => "正四品",
            Self::ViceMinister(_) => "正三品",
            Self::Clerk(_) if yuan => "从五品",
            Self::Clerk(_) => "正五品",
        }
    }

    pub fn code(self) -> String {
        match self {
            Self::Chancellor => "chancellor".into(),
            Self::ViceChancellor => "vice-chancellor".into(),
            Self::FirstGrandSecretary => "first-secretary".into(),
            Self::SecondGrandSecretary => "second-secretary".into(),
            Self::Censor => "censor".into(),
            Self::Minister(d) => format!("minister:{}", d.code()),
            Self::ViceMinister(d) => format!("vice-minister:{}", d.code()),
            Self::Clerk(d) => format!("clerk:{}", d.code()),
        }
    }
    pub fn label(self) -> String {
        match self {
            Self::Chancellor => "中书右丞相".into(),
            Self::ViceChancellor => "中书左丞相".into(),
            Self::FirstGrandSecretary => "内阁首辅".into(),
            Self::SecondGrandSecretary => "内阁次辅".into(),
            Self::Censor => "左都御史".into(),
            Self::Minister(d) => format!("{}尚书", d.label()),
            Self::ViceMinister(d) => format!("{}侍郎", d.label()),
            Self::Clerk(d) => format!("{}郎中", d.label()),
        }
    }
}
impl FromStr for Office {
    type Err = ParseError;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "chancellor" => Ok(Self::Chancellor),
            "vice-chancellor" => Ok(Self::ViceChancellor),
            "first-secretary" => Ok(Self::FirstGrandSecretary),
            "second-secretary" => Ok(Self::SecondGrandSecretary),
            "censor" => Ok(Self::Censor),
            _ => {
                let (kind, department) = input
                    .split_once(':')
                    .ok_or_else(|| ParseError::MalformedOffice(input.into()))?;
                let department = department.parse::<Department>()?;
                match kind {
                    "minister" => Ok(Self::Minister(department)),
                    "vice-minister" => Ok(Self::ViceMinister(department)),
                    "clerk" => Ok(Self::Clerk(department)),
                    _ => Err(ParseError::UnknownOffice(input.into())),
                }
            }
        }
    }
}

/// 度支（token 限额）的适用范围。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetScope {
    /// 朝廷总额：全部诏令（含票拟、廷议等）累计用量的上限。
    Court,
    /// 某一官职承办的每件诏令的上限。
    Office(Office),
    /// 某件诏令的上限（含其票拟、分办、廷议与复核）。
    Task(TaskId),
}
impl BudgetScope {
    pub fn label(self) -> String {
        match self {
            Self::Court => "朝廷总额".into(),
            Self::Office(office) => format!("{}每件", office.label()),
            Self::Task(task) => format!("诏令 #{}", task.0),
        }
    }
}
