//! 紫垣核心领域模型：官员、官职、诏令与朝廷事件。
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

/// 字符串到领域类型的解析错误（CLI 参数、事件簿字段）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    UnknownDepartment(String),
    UnknownMode(String),
    UnknownOffice(String),
    MalformedOffice(String),
    UnknownCapability(String),
    UnknownVerdict(String),
}
impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownDepartment(s) => write!(f, "未知部门: {s}"),
            Self::UnknownMode(s) => write!(f, "未知制度: {s}（可用：chancellor/direct/cabinet）"),
            Self::UnknownOffice(s) => write!(f, "未知官职: {s}"),
            Self::MalformedOffice(s) => write!(f, "官职格式无效: {s}"),
            Self::UnknownVerdict(s) => write!(f, "未知评定: {s}（可用：accept/reject）"),
            Self::UnknownCapability(s) => {
                write!(f, "未知工具权限: {s}（可用：read_file/search_code）")
            }
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

/// 皇帝对一件已完成诏令的评定：吏部考绩只采信经评定的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// 官员执行诏令时可使用的工具。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    ReadFile,
    SearchCode,
}
impl Capability {
    pub const ALL: [Self; 2] = [Self::ReadFile, Self::SearchCode];

    /// 与工具名一致。
    pub fn code(self) -> &'static str {
        match self {
            Self::ReadFile => "read_file",
            Self::SearchCode => "search_code",
        }
    }
}
impl FromStr for Capability {
    type Err = ParseError;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|c| c.code() == input)
            .ok_or_else(|| ParseError::UnknownCapability(input.into()))
    }
}

/// 玉玺（Yuxi）：盖在诏令上的权限令牌，限定承办官员可用的工具与可查阅的项目路径。
///
/// 由皇帝下诏时给定、随 `EdictIssued` 记入事件簿；运行时只按此令牌构造工具，官员无法越权。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seal {
    /// 允许的工具；为空表示只能凭诏令文本作答。
    pub capabilities: BTreeSet<Capability>,
    /// 可查阅的范围：相对项目根目录的目录或文件，`.` 表示整个项目。至少一项。
    pub scope: Vec<String>,
}
impl Default for Seal {
    /// 全部只读工具，整个项目。
    fn default() -> Self {
        Self {
            capabilities: Capability::ALL.into(),
            scope: vec![".".into()],
        }
    }
}
impl fmt::Display for Seal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tools: Vec<_> = self.capabilities.iter().map(|c| c.code()).collect();
        let tools = if tools.is_empty() {
            "无".to_owned()
        } else {
            tools.join("、")
        };
        write!(f, "工具：{tools}；范围：{}", self.scope.join("、"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edict {
    pub id: TaskId,
    pub department: Department,
    pub text: String,
    pub seal: Seal,
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
    /// 皇帝评定已完成的诏令（每件一次）。这是吏部考绩唯一采信的质量依据。
    TaskReviewed {
        task: TaskId,
        verdict: Verdict,
        note: String,
    },
}
