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
    UnknownThreat(String),
}
impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownDepartment(s) => write!(f, "未知部门: {s}"),
            Self::UnknownMode(s) => write!(f, "未知制度: {s}（可用：chancellor/direct/cabinet）"),
            Self::UnknownOffice(s) => write!(f, "未知官职: {s}"),
            Self::MalformedOffice(s) => write!(f, "官职格式无效: {s}"),
            Self::UnknownVerdict(s) => write!(f, "未知评定: {s}（可用：accept/reject）"),
            Self::UnknownThreat(s) => write!(f, "未知可疑类别: {s}"),
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
///
/// 都察院左都御史（元为御史台御史大夫）各制度都设，独立于派工路线，专司复核回奏、纠劾百官。
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    pub speciality: Department,
}

/// 官员的性格与文风，由官员 ID 决定：不记入事件簿，旧事件簿同样适用。
///
/// 人格只影响措辞与论证方式，不改变职权、玉玺与派工路线。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Persona {
    temperament: usize,
    style: usize,
}

/// 性格：名称与在提示词中的描述。
const TEMPERAMENTS: [(&str, &str); 6] = [
    ("刚直", "直言敢谏，见弊即言，不避忤旨"),
    ("持重", "老成持重，凡事先虑后患，主张稳妥"),
    ("锐进", "锐意进取，主张速办，敢于任事"),
    ("圆融", "长于调和，常求折中两全之策"),
    ("精细", "务求实证，好究细节，言必有据"),
    ("宏阔", "好论大体与长远，不拘小节"),
];

/// 文风：名称与在提示词中的描述。
const STYLES: [(&str, &str); 4] = [
    ("简劲", "文辞简劲，多用短句"),
    ("典雅", "好引经据典，多用四字句"),
    ("平实", "文风平实，如白话奏对"),
    ("条陈", "好分条陈述：一、二、三"),
];

impl Persona {
    pub fn of(agent: AgentId) -> Self {
        // splitmix64：相邻的 ID 也得到不相关的人格。
        let mut z = agent.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        Self {
            temperament: (z % TEMPERAMENTS.len() as u64) as usize,
            style: ((z >> 32) % STYLES.len() as u64) as usize,
        }
    }

    /// 如「刚直·简劲」。
    pub fn label(self) -> String {
        format!(
            "{}·{}",
            TEMPERAMENTS[self.temperament].0, STYLES[self.style].0
        )
    }

    /// 写入官员系统提示词的一句话，并申明人格不越权。
    pub fn prompt(self) -> String {
        let (t, t_hint) = TEMPERAMENTS[self.temperament];
        let (s, s_hint) = STYLES[self.style];
        format!(
            "你性格{t}：{t_hint}；文风{s}：{s_hint}。\
             性格与文风只影响你的措辞与论证方式，不改变你的职权、玉玺与所述事实。"
        )
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

/// 廷议中一位官员的一段发言。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Speech {
    pub speaker: AgentId,
    /// 发言时的官职。
    pub office: Office,
    pub text: String,
    pub model: Option<String>,
    pub usage: TokenUsage,
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

/// 都察院对一件回奏的结论。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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

/// 官员执行诏令时可使用的工具。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    ReadFile,
    SearchCode,
    /// 写文件：改动只暂存，呈报皇帝落盘后才写入项目。
    WriteFile,
    /// 在项目副本中（含暂存改动）运行皇帝设定的测试命令。
    RunTests,
}
impl Capability {
    pub const ALL: [Self; 4] = [
        Self::ReadFile,
        Self::SearchCode,
        Self::WriteFile,
        Self::RunTests,
    ];
    /// 默认玉玺授予的只读工具。
    pub const READ_ONLY: [Self; 2] = [Self::ReadFile, Self::SearchCode];

    /// 与工具名一致。
    pub fn code(self) -> &'static str {
        match self {
            Self::ReadFile => "read_file",
            Self::SearchCode => "search_code",
            Self::WriteFile => "write_file",
            Self::RunTests => "run_tests",
        }
    }

    /// 动土木之权（写文件、跑测试）：唯工部诏令可授。
    pub fn is_works_only(self) -> bool {
        matches!(self, Self::WriteFile | Self::RunTests)
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
            capabilities: Capability::READ_ONLY.into(),
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

/// 承办官员暂存的一处文件改动（只支持新建与改写）。`before` 为改动时项目中的原文，
/// 新建文件为 `None`；落盘前据此确认文件在此期间未被他人改动。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    /// 相对项目根目录的路径（`/` 分隔）。
    pub path: String,
    pub before: Option<String>,
    pub after: String,
}

/// 统一 diff 中一段改动：`start` 为原文起始行号（从 1 起），`lines` 为（标记, 行）——
/// `' '` 上下文、`'-'` 删除、`'+'` 新增；行不含换行符。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub start: usize,
    pub lines: Vec<(char, String)>,
}

/// 每段前后保留的上下文行数；相隔不超过两倍上下文的改动并为一段。
const HUNK_CONTEXT: usize = 3;

impl FileChange {
    /// 逐行比较（行含换行符，以保留行尾）：(标记, 原文行号, 行)。
    fn ops(&self) -> Vec<(char, usize, &str)> {
        let old: Vec<&str> = self
            .before
            .as_deref()
            .map_or(Vec::new(), |b| b.split_inclusive('\n').collect());
        let new: Vec<&str> = self.after.split_inclusive('\n').collect();
        let (n, m) = (old.len(), new.len());
        let mut ops = Vec::with_capacity(n.max(m));
        if n.saturating_mul(m) > 4_000_000 {
            // 太大时不求最小差异：整段删去、整段新增。
            ops.extend(old.iter().enumerate().map(|(i, l)| ('-', i, *l)));
            ops.extend(new.iter().map(|l| ('+', n, *l)));
            return ops;
        }
        // 最长公共子序列：lcs[i][j] 为 old[i..] 与 new[j..] 的公共行数。
        let mut lcs = vec![vec![0u32; m + 1]; n + 1];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[i][j] = if old[i] == new[j] {
                    lcs[i + 1][j + 1] + 1
                } else {
                    lcs[i + 1][j].max(lcs[i][j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && old[i] == new[j] {
                ops.push((' ', i, old[i]));
                (i, j) = (i + 1, j + 1);
            } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
                ops.push(('-', i, old[i]));
                i += 1;
            } else {
                ops.push(('+', i, new[j]));
                j += 1;
            }
        }
        ops
    }

    /// 各改动行（`ops` 的下标）所属的段号（从 0 起），与 [`hunks`](Self::hunks) 一致。
    fn hunk_of(ops: &[(char, usize, &str)]) -> Vec<Option<usize>> {
        let mut owner = vec![None; ops.len()];
        let mut current: Option<(usize, usize)> = None; // (段号, 上一改动行)
        for (k, op) in ops.iter().enumerate() {
            if op.0 == ' ' {
                continue;
            }
            let hunk = match current {
                Some((h, last)) if k - last - 1 <= 2 * HUNK_CONTEXT => h,
                Some((h, _)) => h + 1,
                None => 0,
            };
            owner[k] = Some(hunk);
            current = Some((hunk, k));
        }
        owner
    }

    /// 按段切分的差异，供逐段准奏。新建文件只有一段。
    pub fn hunks(&self) -> Vec<Hunk> {
        let ops = self.ops();
        let owner = Self::hunk_of(&ops);
        let mut hunks: Vec<(usize, usize, usize)> = Vec::new(); // (段号, 首改动行, 末改动行)
        for (k, h) in owner.iter().enumerate() {
            match (h, hunks.last_mut()) {
                (Some(h), Some(last)) if last.0 == *h => last.2 = k,
                (Some(h), _) => hunks.push((*h, k, k)),
                (None, _) => {}
            }
        }
        hunks
            .into_iter()
            .map(|(_, first, last)| {
                let from = first.saturating_sub(HUNK_CONTEXT);
                let to = (last + HUNK_CONTEXT).min(ops.len() - 1);
                Hunk {
                    start: ops[from].1 + 1,
                    lines: ops[from..=to]
                        .iter()
                        .map(|(mark, _, text)| {
                            let text = text.strip_suffix('\n').unwrap_or(text);
                            (*mark, text.strip_suffix('\r').unwrap_or(text).to_owned())
                        })
                        .collect(),
                }
            })
            .collect()
    }

    /// 只采纳 `keep(段号)` 为真的段（段号从 0 起）后的文件内容；`None` 表示文件不存在
    /// （新建文件被整段驳回）。全部采纳即 `after`，全部驳回即 `before`。
    pub fn select(&self, keep: impl Fn(usize) -> bool) -> Option<String> {
        let ops = self.ops();
        let owner = Self::hunk_of(&ops);
        let mut out = String::new();
        let mut any_new = false;
        for ((mark, _, text), hunk) in ops.iter().zip(owner) {
            let kept = hunk.is_some_and(&keep);
            match mark {
                ' ' => out.push_str(text),
                '-' if !kept => out.push_str(text),
                '+' if kept => {
                    any_new = true;
                    out.push_str(text);
                }
                _ => {}
            }
        }
        let created = any_new || !ops.iter().any(|op| op.0 == '+');
        (self.before.is_some() || created).then_some(out)
    }
}

/// 翰林院实录：一件已结诏令的始末（由事件簿推出，不另记事件）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub task: TaskId,
    pub department: Department,
    /// 诏令内容。
    pub edict: String,
    /// 承办官员及其姓名（已削籍者为空）。
    pub worker: AgentId,
    pub handler: String,
    /// 回奏全文。
    pub summary: String,
    /// 皇帝评定与都察院复核，如「采纳：好」「御史弹劾」；尚无则为「待评定」。
    pub verdict: String,
}

impl Record {
    /// 检索用的全文。
    fn haystack(&self) -> String {
        format!("{}\n{}\n{}", self.edict, self.summary, self.verdict)
    }
}

/// 检索词：ASCII 词（小写，≥2 字符）与中文等其他字符的相邻二字组。
fn search_terms(text: &str) -> std::collections::BTreeSet<String> {
    let mut terms = std::collections::BTreeSet::new();
    let mut word = String::new();
    let mut run: Vec<char> = Vec::new();
    let flush_word = |word: &mut String, terms: &mut std::collections::BTreeSet<String>| {
        if word.chars().count() >= 2 {
            terms.insert(word.to_lowercase());
        }
        word.clear();
    };
    let flush_run = |run: &mut Vec<char>, terms: &mut std::collections::BTreeSet<String>| {
        for pair in run.windows(2) {
            terms.insert(pair.iter().collect());
        }
        if run.len() == 1 {
            terms.insert(run[0].to_string());
        }
        run.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            flush_run(&mut run, &mut terms);
            word.push(c);
        } else if c.is_alphanumeric() {
            flush_word(&mut word, &mut terms);
            run.push(c);
        } else {
            flush_word(&mut word, &mut terms);
            flush_run(&mut run, &mut terms);
        }
    }
    flush_word(&mut word, &mut terms);
    flush_run(&mut run, &mut terms);
    terms
}

/// 按与 `query` 共有的检索词数检索实录，得分高者在前（同分时新者在前），至多 `limit` 件。
pub fn search_records<'a>(records: &'a [Record], query: &str, limit: usize) -> Vec<&'a Record> {
    let wanted = search_terms(query);
    let mut scored: Vec<(usize, &Record)> = records
        .iter()
        .map(|r| {
            let have = search_terms(&r.haystack());
            (wanted.intersection(&have).count(), r)
        })
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.task.cmp(&a.1.task)));
    scored.into_iter().take(limit).map(|(_, r)| r).collect()
}

/// 分办出去的一件子诏令：沿用原诏令的部门与玉玺，由指定官员承办。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subtask {
    pub id: TaskId,
    pub worker: AgentId,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edict {
    pub id: TaskId,
    pub department: Department,
    pub text: String,
    pub seal: Seal,
}

/// 廷推中一位大臣的推举。`model`/`usage` 为推举所用模型与用量（按吏部考绩代推时为 `None` 与零）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nomination {
    pub voter: AgentId,
    pub candidate: AgentId,
    pub note: String,
    pub model: Option<String>,
    pub usage: TokenUsage,
}

/// 翰林院诏令模板：常用诏令的内容、默认部门与玉玺，以及交都察院核验的验收标准。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    pub department: Department,
    pub text: String,
    pub seal: Seal,
    /// 验收标准，各条单行。
    pub criteria: Vec<String>,
}

/// Event Sourcing: 朝廷状态由此事件列表重放而来。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CourtEvent {
    Founded(GovernanceMode),
    /// 时辰：此后的事件发生于该时刻（Unix 秒）之后、下一个时辰之前。不改变朝廷状态，
    /// 只供起居注按时间筛选；写入者在分钟变化时才记一条。
    Clock(u64),
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
    /// 皇帝修道怠政（`true`）或回朝亲政（`false`）。怠政期间批红与评定由司礼监代批。
    EmperorRetreat(bool),
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
    /// 皇帝设定度支（token 限额）；`None` 取消。超支即停，最多超出一次模型调用的用量。
    BudgetSet {
        scope: BudgetScope,
        tokens: Option<u64>,
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
    /// 多官协作：尚书（或皇帝御定）把一件待办诏令拆给本部数位官员分办。子诏令随即入队、
    /// 并发执行；全部结束后，原诏令由尚书汇总回奏。`model`/`usage` 为尚书拆分所用（御定为 `None` 与零）。
    EdictSplit {
        task: TaskId,
        subtasks: Vec<Subtask>,
        note: String,
        model: Option<String>,
        usage: TokenUsage,
    },
    /// 廷议：数位在任官员就一件待办诏令依次发言辩论，纪要供票拟与承办参考。
    /// 只影响措辞与论证，不改变派工与玉玺。每段发言的用量计入诏令。
    CourtDebated {
        task: TaskId,
        speeches: Vec<Speech>,
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
    /// 承办官员呈报的文件改动（奏本），在 `TaskCompleted` 之前记录；须皇帝落盘才写入项目。
    ChangesProposed {
        task: TaskId,
        changes: Vec<FileChange>,
    },
    /// 皇帝准奏：改动已写入项目。`skipped` 为逐段准奏时驳回的段号（从 1 起，跨文件连续
    /// 编号，见 [`FileChange::hunks`]），空即全部准奏。
    ChangesApplied {
        task: TaskId,
        skipped: Vec<u32>,
    },
    /// 皇帝驳回改动：不写入项目。
    ChangesDiscarded {
        task: TaskId,
    },
    /// 都察院复核已完成的诏令：证据核验结果与御史的背书或弹劾（每件一次）。
    /// `model`/`usage` 为御史复核所用模型与用量（未调用模型时为 `None` 与零）。
    CensorReported {
        task: TaskId,
        censor: AgentId,
        verdict: CensorVerdict,
        note: String,
        evidence: Evidence,
        model: Option<String>,
        usage: TokenUsage,
    },
    /// 刑部与锦衣卫的检出：`source` 为出处（`tool:<工具名>` 为锦衣卫在外来文书中查得，
    /// `report` 为刑部在回奏中查得），`excerpt` 为可疑原文的片段。只标记、不拦截。
    SuspicionRaised {
        task: TaskId,
        source: String,
        threat: Threat,
        excerpt: String,
    },
    /// 刑部复勘一处检出（`index` 为该诏令检出的序号，从 0 起）：`upheld` 为维持原检，
    /// 否则判为误报。只是复勘意见，不改变代批；平反须皇帝亲自（`SuspicionAcquitted`）。
    SuspicionReviewed {
        task: TaskId,
        index: u32,
        reviewer: AgentId,
        upheld: bool,
        note: String,
        model: Option<String>,
        usage: TokenUsage,
    },
    /// 皇帝平反一处检出：此后不再妨碍司礼监代批。
    SuspicionAcquitted {
        task: TaskId,
        index: u32,
    },
    /// 礼部开科：官员以其官职所配模型（未任官职用默认模型）作答一道考题，按要点程序判分（0–100）。
    ExamTaken {
        agent: AgentId,
        /// 考题名（`.ziyuan/exam/` 下的文件名，不含扩展名）。
        exam: String,
        score: u8,
        answer: String,
        model: Option<String>,
        usage: TokenUsage,
    },
    /// 殿试：皇帝亲自出题，在任官员作答（`Speech` 即一份答卷）。
    PalaceExamHeld {
        question: String,
        answers: Vec<Speech>,
    },
    /// 皇帝在最近一次殿试的答卷中钦点状元。
    ZhuangyuanNamed {
        agent: AgentId,
    },
    /// 兵部驿传：外部事件（CI 失败、新 issue 等）经驿站入队为诏令，`station` 为来处。
    /// 与该诏令的 `EdictIssued` 一同记录；入队后仍受批红、玉玺与度支约束。
    CourierDelivered {
        task: TaskId,
        station: String,
    },
    /// 驿站把结果送回来处：`pull_request` 为假是回奏评论，为真是准奏改动开的草稿 PR；`url` 为其网址。
    CourierReplied {
        task: TaskId,
        pull_request: bool,
        url: String,
    },
    /// 八百里加急：待办诏令插队先行，执行时限放宽。
    EdictExpedited {
        task: TaskId,
    },
    /// 廷推：吏部会同九卿为 `office` 各推一人，只是建议；任命仍由皇帝。
    CourtNominated {
        office: Office,
        votes: Vec<Nomination>,
    },
    /// 廷杖：屡遭弹劾的官员降一级留用，收回工部动土之权，戴罪立功。
    OfficialCaned {
        agent: AgentId,
    },
    /// 宽宥受杖的官员：恢复品级与玉玺。
    OfficialPardoned {
        agent: AgentId,
    },
    /// 翰林院存一份诏令模板（同名的覆盖，包括覆盖内置模板）。
    TemplateSaved {
        name: String,
        template: Template,
    },
    /// 翰林院撤去自定的模板（同名的内置模板随之恢复）。
    TemplateRemoved {
        name: String,
    },
    /// 诏令按模板下达：与 `EdictIssued` 一同记录，验收标准以当时的模板为准（之后改模板不影响）。
    TemplateApplied {
        task: TaskId,
        name: String,
        criteria: Vec<String>,
    },
    /// 一次模型调用的 token 用量。指派后的任何时刻都可记录：失败、超时的诏令同样消耗了 token。
    ModelUsage {
        task: TaskId,
        usage: TokenUsage,
        /// 该次调用的 trace ID（请求头 `X-Client-Request-Id`）；早期日志与演示后端为 `None`。
        trace: Option<String>,
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

    #[test]
    fn records_are_found_by_shared_words_and_chinese_bigrams() {
        let record = |id, edict: &str, summary: &str| Record {
            task: TaskId(id),
            department: Department::Works,
            edict: edict.into(),
            worker: AgentId(4),
            handler: "某".into(),
            summary: summary.into(),
            verdict: "待评定".into(),
        };
        let records = [
            record(1, "检查除零", "div 未处理除数为零"),
            record(2, "整理 README", "补了目录"),
            record(3, "再查除零", "Div 已改为返回 0"),
        ];
        let ids = |q| {
            search_records(&records, q, 5)
                .iter()
                .map(|r| r.task.0)
                .collect::<Vec<_>>()
        };
        // 「除零」二字组命中 1、3；同分新者在前。英文不分大小写。
        assert_eq!(ids("div 函数会不会除零"), [3, 1]);
        assert_eq!(ids("readme"), [2]);
        assert!(ids("数据库").is_empty());
        assert_eq!(search_records(&records, "除零", 1).len(), 1);
    }

    #[test]
    fn hunks_select_keeps_line_endings_and_new_files_can_be_dropped() {
        // CRLF、末行无换行：取舍后逐字节保留；相隔 7 行的两处为两段，相隔 6 行并为一段。
        let before: String = (1..=12).map(|i| format!("l{i}\r\n")).collect::<String>() + "end";
        let change = FileChange {
            path: "a".into(),
            before: Some(before.clone()),
            after: before.replace("l1\r\n", "L1\r\n").replace("end", "END"),
        };
        let hunks = change.hunks();
        assert_eq!(hunks.len(), 2, "{hunks:?}");
        assert_eq!(
            hunks[0].lines[..2],
            [('-', "l1".into()), ('+', "L1".into())]
        );
        assert_eq!(hunks[1].start, 10);
        assert_eq!(
            change.select(|h| h == 1).unwrap(),
            before.replace("end", "END")
        );
        assert_eq!(change.select(|_| true).unwrap(), change.after);
        assert_eq!(change.select(|_| false).unwrap(), before);
        let near = FileChange {
            after: before
                .replace("l1\r\n", "L1\r\n")
                .replace("l8\r\n", "L8\r\n"),
            ..change
        };
        assert_eq!(near.hunks().len(), 1);

        let created = FileChange {
            path: "b".into(),
            before: None,
            after: "x\ny".into(),
        };
        assert_eq!(created.hunks().len(), 1);
        assert_eq!(created.select(|_| false), None);
        assert_eq!(created.select(|_| true).as_deref(), Some("x\ny"));
        let empty = FileChange {
            after: String::new(),
            ..created
        };
        assert!(empty.hunks().is_empty());
        assert_eq!(empty.select(|_| false).as_deref(), Some(""));
    }

    fn seal(tools: &[Capability], scope: &[&str]) -> Seal {
        Seal {
            capabilities: tools.iter().copied().collect(),
            scope: scope.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn report_sections_are_found_at_line_starts() {
        let good = "## 结论\n可以。\n**依据**：src/lib.rs:3\n3. 未办：无";
        assert!(missing_sections(good).is_empty());
        assert!(missing_sections("【结论】行\n- 依据：甲\n未办之事：无").is_empty());
        // 段落标题须在行首；正文里提到「结论」不算。
        assert_eq!(
            missing_sections("我的结论是可以，依据如下。\n依据：甲"),
            ["结论", "未办之事"]
        );
        assert_eq!(missing_sections(""), REPORT_SECTIONS);
    }

    #[test]
    fn personas_vary_and_are_stable() {
        let labels: std::collections::BTreeSet<_> = (1..=40)
            .map(|id| Persona::of(AgentId(id)).label())
            .collect();
        assert!(labels.len() >= 12, "{labels:?}");
        assert_eq!(Persona::of(AgentId(7)), Persona::of(AgentId(7)));
        assert!(Persona::of(AgentId(3)).prompt().contains("不改变你的职权"));
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
