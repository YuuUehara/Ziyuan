//! 紫垣核心领域模型：官员、官职、诏令与朝廷事件。
use std::fmt;
use std::str::FromStr;

/// 字符串到领域类型的解析错误（CLI 参数、事件簿字段）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    UnknownDepartment(String),
    UnknownMode(String),
    UnknownOffice(String),
    MalformedOffice(String),
}
impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownDepartment(s) => write!(f, "未知部门: {s}"),
            Self::UnknownMode(s) => write!(f, "未知制度: {s}（可用：chancellor/direct/cabinet）"),
            Self::UnknownOffice(s) => write!(f, "未知官职: {s}"),
            Self::MalformedOffice(s) => write!(f, "官职格式无效: {s}"),
        }
    }
}
impl std::error::Error for ParseError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Department {
    Personnel,
    Revenue,
    Rites,
    War,
    Justice,
    Works,
}

impl Department {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GovernanceMode {
    Chancellor,
    DirectRule,
    Cabinet,
}
impl GovernanceMode {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Office {
    Chancellor,
    FirstGrandSecretary,
    Minister(Department),
    Clerk(Department),
}
impl Office {
    pub fn code(self) -> String {
        match self {
            Self::Chancellor => "chancellor".into(),
            Self::FirstGrandSecretary => "first-secretary".into(),
            Self::Minister(d) => format!("minister:{}", d.code()),
            Self::Clerk(d) => format!("clerk:{}", d.code()),
        }
    }
    pub fn label(self) -> String {
        match self {
            Self::Chancellor => "丞相".into(),
            Self::FirstGrandSecretary => "内阁首辅".into(),
            Self::Minister(d) => format!("{}尚书", d.label()),
            Self::Clerk(d) => format!("{}属官", d.label()),
        }
    }
}
impl FromStr for Office {
    type Err = ParseError;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "chancellor" => Ok(Self::Chancellor),
            "first-secretary" => Ok(Self::FirstGrandSecretary),
            _ => {
                let (kind, department) = input
                    .split_once(':')
                    .ok_or_else(|| ParseError::MalformedOffice(input.into()))?;
                let department = department.parse::<Department>()?;
                match kind {
                    "minister" => Ok(Self::Minister(department)),
                    "clerk" => Ok(Self::Clerk(department)),
                    _ => Err(ParseError::UnknownOffice(input.into())),
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    pub speciality: Department,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edict {
    pub id: TaskId,
    pub department: Department,
    pub text: String,
}

/// Event Sourcing: 朝廷状态由此事件列表重放而来。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CourtEvent {
    Founded(GovernanceMode),
    AgentRegistered(Agent),
    OfficialAppointed {
        agent: AgentId,
        office: Office,
    },
    OfficialDismissed {
        agent: AgentId,
    },
    InstitutionReformed(GovernanceMode),
    /// 诏令入队；此后依次可有 `TaskAssigned`、若干 `ToolInvoked`，并以完成/失败/撤销之一结束。
    EdictIssued(Edict),
    /// 派给某位官员。进程中断后未结束的任务可重新指派。
    TaskAssigned {
        task: TaskId,
        worker: AgentId,
    },
    /// 审计：执行中的一次工具调用（只记参数与成败，不记输出）。
    ToolInvoked {
        task: TaskId,
        tool: String,
        args_json: String,
        ok: bool,
    },
    TaskCompleted {
        task: TaskId,
        worker: AgentId,
        summary: String,
    },
    TaskFailed {
        task: TaskId,
        reason: String,
    },
    TaskCancelled {
        task: TaskId,
    },
}
