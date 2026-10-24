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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Office {
    /// 中书右丞相（元以右为尊）。
    Chancellor,
    /// 中书左丞相。
    ViceChancellor,
    /// 内阁首辅。
    FirstGrandSecretary,
    /// 内阁次辅。
    SecondGrandSecretary,
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
            Self::Minister(_) | Self::ViceMinister(_) | Self::Clerk(_) => None,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    pub speciality: Department,
}

/// 一次或多次模型调用消耗的 token（以服务端报告为准）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
impl Seal {
    /// 受 `ceiling`（官职上限）约束后的玉玺：工具取交集；查阅范围只保留落在上限内的部分
    /// （诏令路径在上限路径之下则保留，上限路径在诏令路径之下则收窄为上限路径）。
    /// 两者没有共同范围时返回 `None`。
    pub fn within(&self, ceiling: &Seal) -> Option<Seal> {
        fn parts(path: &str) -> Vec<&str> {
            path.split('/')
                .filter(|p| !p.is_empty() && *p != ".")
                .collect()
        }
        let under = |inner: &str, outer: &str| parts(inner).starts_with(&parts(outer));
        let mut scope: Vec<String> = Vec::new();
        for path in &self.scope {
            let narrowed: Vec<&String> = if ceiling.scope.iter().any(|c| under(path, c)) {
                vec![path]
            } else {
                ceiling.scope.iter().filter(|c| under(c, path)).collect()
            };
            for p in narrowed {
                if !scope.contains(p) {
                    scope.push(p.clone());
                }
            }
        }
        (!scope.is_empty()).then(|| Seal {
            capabilities: self
                .capabilities
                .intersection(&ceiling.capabilities)
                .copied()
                .collect(),
            scope,
        })
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
    /// 革职：除去官职并削去仕籍，此后不得再任官；身份与履历保留。
    OfficialPurged {
        agent: AgentId,
    },
    InstitutionReformed(GovernanceMode),
    /// 皇帝为某一官职指定承办模型；`None` 恢复默认模型。
    OfficeModelSet {
        office: Office,
        model: Option<String>,
    },
    /// 皇帝为某一官职设定玉玺上限：该职承办的诏令，玉玺不得超出此上限；`None` 取消上限。
    OfficeCeilingSet {
        office: Office,
        ceiling: Option<Seal>,
    },
    /// 诏令入队；此后依次可有 `TaskAssigned`、若干 `ToolInvoked`，并以完成/失败/撤销之一结束。
    EdictIssued(Edict),
    /// 内阁票拟：拟定承办者并附票拟意见。`model`/`usage` 为首辅票拟所用模型与用量；
    /// 皇帝改票时 `model` 为 `None`、`usage` 为零。可多次票拟，以最后一次为准。
    EdictDrafted {
        task: TaskId,
        worker: AgentId,
        note: String,
        model: Option<String>,
        usage: TokenUsage,
    },
    /// 内阁制下皇帝对内阁票拟的批红：批红后诏令方可指派执行。
    EdictApproved {
        task: TaskId,
    },
    /// 派给某位官员。进程中断后未结束的任务可重新指派。
    TaskAssigned {
        task: TaskId,
        worker: AgentId,
        /// 承办所用的模型（演示后端与早期日志为 `None`），用于按模型估算花费。
        model: Option<String>,
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
    /// 一次模型调用的 token 用量。指派后的任何时刻都可记录：失败、超时的诏令同样消耗了 token。
    ModelUsage {
        task: TaskId,
        usage: TokenUsage,
    },
    /// 皇帝评定已完成的诏令（每件一次）。这是吏部考绩唯一采信的质量依据。
    TaskReviewed {
        task: TaskId,
        verdict: Verdict,
        note: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seal(tools: &[Capability], scope: &[&str]) -> Seal {
        Seal {
            capabilities: tools.iter().copied().collect(),
            scope: scope.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn seal_within_ceiling_narrows_tools_and_scope() {
        use Capability::*;
        let ceiling = seal(&[SearchCode], &["crates/core", "docs"]);
        // 默认玉玺（整个项目、全部工具）收窄为上限本身。
        assert_eq!(
            Seal::default().within(&ceiling),
            Some(seal(&[SearchCode], &["crates/core", "docs"]))
        );
        // 上限内的路径保留；上限外的去掉；`./` 前缀与尾部斜杠不影响比较。
        assert_eq!(
            seal(
                &[ReadFile, SearchCode],
                &["./crates/core/src", "README.md", "docs/"]
            )
            .within(&ceiling),
            Some(seal(&[SearchCode], &["./crates/core/src", "docs/"]))
        );
        // 同名前缀不算在内：crates/core2 不在 crates/core 之下。
        assert_eq!(seal(&[ReadFile], &["crates/core2"]).within(&ceiling), None);
        // 上限为整个项目时不收窄范围。
        let open = seal(&[ReadFile], &["."]);
        assert_eq!(
            seal(&[ReadFile, SearchCode], &["src"]).within(&open),
            Some(seal(&[ReadFile], &["src"]))
        );
    }
}
