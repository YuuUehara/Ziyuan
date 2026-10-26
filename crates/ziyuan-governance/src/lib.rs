//! 治理引擎。职权由代码校验，不由 Agent 的角色提示词决定。
pub mod names;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path};
use ziyuan_core::{
    Agent, AgentId, BudgetScope, CHANGELOG, Capability, CensorVerdict, CourtEvent, Department,
    Edict, Evidence, FileChange, GovernanceMode, Nomination, Office, Record, Seal, Speech, Subtask,
    TaskId, Template, Threat, TokenUsage, Verdict,
};

/// 违反朝廷规则的操作；回放与提议共用同一套规则。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GovernanceError {
    NotFounded,
    DuplicateAgent(AgentId),
    UnknownAgent(AgentId),
    EmptyName,
    AlreadyAppointed(AgentId),
    NotAppointed(AgentId),
    OfficeNotInMode(Office),
    SpecialityMismatch {
        name: String,
        department: Department,
    },
    OfficeOccupied(Office),
    SameMode(GovernanceMode),
    EmptyEdict,
    /// 玉玺的查阅范围为空，或含绝对路径、`..` 等越出项目的路径。
    InvalidScope(String),
    DuplicateEdict(TaskId),
    UnknownEdict(TaskId),
    TaskFinished(TaskId),
    TaskNotAssigned(TaskId),
    WrongWorker {
        task: TaskId,
        worker: AgentId,
    },
    /// 只有已完成的诏令可以评定。
    NotCompleted(TaskId),
    AlreadyReviewed(TaskId),
    Vacant(Office),
    /// 已革职的官员永不叙用。
    Purged(AgentId),
    /// 只有内阁制下的诏令需要批红。
    ApprovalNotNeeded,
    AlreadyApproved(TaskId),
    /// 一键朱元璋只能在丞相制下使用。
    NoChancellery,
    /// 模型名为空或含空白。
    InvalidModel(String),
    /// 只有内阁制下才票拟。
    NoCabinet,
    /// 票拟只能拟派本部在任的尚书、侍郎或郎中。
    NotEligible {
        worker: AgentId,
        department: Department,
    },
    /// 诏令的查阅范围全在承办者官职的玉玺上限之外。
    SealExceedsCeiling(Office),
    /// 写文件、跑测试只能授予工部诏令。
    WorksOnly(Capability),
    /// 诏令没有待决的改动（未呈报，或已落盘/驳回）。
    NoPendingChanges(TaskId),
    /// 复核者不是现任左都御史。
    NotCensor(AgentId),
    /// 御史不得复核自己经手的诏令。
    SelfReview(AgentId),
    AlreadyCensured(TaskId),
    /// 分办须在诏令待办时进行，子诏令不再分办，一件诏令只分办一次。
    CannotSplit(TaskId),
    /// 分办须为 2–8 件，每件有内容。
    InvalidSplit(String),
    /// 已在修道（`true`）或已在朝（`false`）。
    RetreatUnchanged(bool),
    /// 皇帝修道期间，批红与评定由司礼监代批，须先回朝。
    InRetreat,
    /// 科举或殿试不合规矩（原因）。
    InvalidExam(String),
    /// 驿传或加急不合规矩。
    Military(String),
    /// 翰林院模板不合规矩。
    Template(String),
    /// 廷推、廷杖不合规矩。
    Court(String),
    /// 逐段准奏的段号不合规矩。
    Changes(String),
    /// 复勘、平反不合规矩。
    Suspicion(String),
    /// 廷议须在诏令待办时进行。
    CannotDebate(TaskId),
    /// 廷议须有 2–6 位在任官员，发言者须为现任官员。
    InvalidDebate(String),
    /// 度支须为正数。
    InvalidBudget,
    /// 朝廷度支（tokens）已用尽。
    TreasuryExhausted(u64),
    /// 诏令已用尽其度支（tokens）。
    TaskOverBudget {
        task: TaskId,
        limit: u64,
    },
}
impl fmt::Display for GovernanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFounded => f.write_str("事件日志必须以 Founded 开始"),
            Self::DuplicateAgent(id) => write!(f, "重复官员 ID: {}", id.0),
            Self::UnknownAgent(id) => write!(f, "官员 #{} 不存在", id.0),
            Self::EmptyName => f.write_str("姓名不能为空"),
            Self::AlreadyAppointed(_) => {
                f.write_str("该官员已有官职。请先执行 dismiss，再重新任命。")
            }
            Self::NotAppointed(_) => f.write_str("该官员未任职"),
            Self::OfficeNotInMode(office) => write!(f, "现行制度不设{}", office.label()),
            Self::SpecialityMismatch { name, department } => {
                write!(f, "{name}的专长不属于{}", department.label())
            }
            Self::OfficeOccupied(office) => write!(f, "{}已有任职者", office.label()),
            Self::SameMode(_) => f.write_str("已经是该制度"),
            Self::EmptyEdict => f.write_str("诏令不能为空"),
            Self::InvalidScope(path) if path.is_empty() => {
                f.write_str("玉玺必须至少给出一个查阅范围")
            }
            Self::InvalidScope(path) => {
                write!(f, "查阅范围必须是项目内的相对路径，且不含 `..`: {path}")
            }
            Self::DuplicateEdict(id) => write!(f, "重复诏令 ID: {}", id.0),
            Self::UnknownEdict(id) => write!(f, "未记录下达的诏令 #{}", id.0),
            Self::TaskFinished(id) => write!(f, "诏令 #{} 已结束，不能再变更", id.0),
            Self::TaskNotAssigned(id) => write!(f, "诏令 #{} 未在执行中", id.0),
            Self::WrongWorker { task, worker } => {
                write!(f, "诏令 #{} 并未派给官员 #{}", task.0, worker.0)
            }
            Self::NotCompleted(id) => write!(f, "诏令 #{} 尚未完成，无从评定", id.0),
            Self::AlreadyReviewed(id) => write!(f, "诏令 #{} 已评定过", id.0),
            Self::Vacant(Office::Chancellor) => {
                f.write_str("中书右丞相、左丞相俱缺。请先任命丞相，或改制。")
            }
            Self::Vacant(Office::FirstGrandSecretary) => {
                f.write_str("内阁首辅、次辅俱缺。请先 appoint 一名首辅。")
            }
            Self::Vacant(Office::Minister(d)) => {
                write!(f, "{0}尚书、{0}侍郎俱缺，无法承接诏令", d.label())
            }
            Self::Vacant(Office::Censor) => {
                f.write_str("左都御史空缺。请先 appoint 一名御史（censor）。")
            }
            Self::Vacant(office) => write!(f, "{}空缺，无法承接诏令", office.label()),
            Self::Purged(id) => write!(f, "官员 #{} 已革职，永不叙用", id.0),
            Self::ApprovalNotNeeded => {
                f.write_str("现行制度无须批红：只有内阁制下的票拟须经皇帝批红")
            }
            Self::AlreadyApproved(id) => write!(f, "诏令 #{} 已批红或已在执行", id.0),
            Self::NoChancellery => f.write_str("现行制度不设中书省，已无丞相可废"),
            Self::InvalidModel(m) => write!(f, "模型名不能为空或含空白: {m:?}"),
            Self::NoCabinet => f.write_str("现行制度不设内阁，无从票拟"),
            Self::NotEligible { worker, department } => write!(
                f,
                "官员 #{} 不是{}在任官员，不能拟派",
                worker.0,
                department.label()
            ),
            Self::SealExceedsCeiling(office) => write!(
                f,
                "诏令的查阅范围全在{}的玉玺上限之外，不予承办",
                office.label()
            ),
            Self::WorksOnly(capability) => {
                write!(
                    f,
                    "{} 只能授予工部诏令（礼部修 CHANGELOG 时可授 write_file，范围须只是 {CHANGELOG}）",
                    capability.code()
                )
            }
            Self::NoPendingChanges(id) => write!(f, "诏令 #{} 没有待决的改动", id.0),
            Self::NotCensor(id) => write!(f, "官员 #{} 不是现任左都御史", id.0),
            Self::SelfReview(id) => write!(f, "官员 #{} 经手此诏令，须回避，不得自行复核", id.0),
            Self::AlreadyCensured(id) => write!(f, "诏令 #{} 已经都察院复核", id.0),
            Self::CannotSplit(id) => write!(
                f,
                "诏令 #{} 不能分办：须尚未执行、未曾分办，且本身不是分办出的子诏令",
                id.0
            ),
            Self::InvalidSplit(why) => write!(f, "分办方案不合法：{why}"),
            Self::RetreatUnchanged(true) => f.write_str("陛下已在西苑修道"),
            Self::RetreatUnchanged(false) => f.write_str("陛下本在朝中"),
            Self::CannotDebate(id) => write!(f, "诏令 #{} 不能廷议：须尚未执行", id.0),
            Self::InvalidDebate(why) => write!(f, "廷议未成：{why}"),
            Self::InvalidExam(why) => write!(f, "礼部不准：{why}"),
            Self::Military(why) => write!(f, "兵部不准：{why}"),
            Self::Template(why) => write!(f, "翰林院不准：{why}"),
            Self::Court(why) => write!(f, "{why}"),
            Self::Changes(why) => write!(f, "逐段准奏不成：{why}"),
            Self::Suspicion(why) => write!(f, "刑部不准：{why}"),
            Self::InvalidBudget => f.write_str("度支须为正数（tokens）"),
            Self::TreasuryExhausted(limit) => write!(
                f,
                "国库告罄：朝廷度支 {limit} tokens 已用尽（户部奏：太仓无银）。\
                 追加请 `ziyuan budget court <额度>`，取消请 `ziyuan budget court none`"
            ),
            Self::TaskOverBudget { task, limit } => {
                write!(
                    f,
                    "度支超限：诏令 #{} 已用尽其 {limit} tokens 的度支",
                    task.0
                )
            }
            Self::InRetreat => f.write_str(
                "陛下正在西苑修道，批红与评定由司礼监代批。要亲自处置请先 `ziyuan return` 回朝",
            ),
        }
    }
}
impl std::error::Error for GovernanceError {}

/// 由事件重放得到的朝廷状态。字段私有：唯一的修改途径是 [`Court::apply`]，
/// 它与 `propose_*` 共用同一套规则，因此回放永远不会接受提议阶段会拒绝的事件。
#[derive(Clone, Debug)]
pub struct Court {
    mode: GovernanceMode,
    agents: BTreeMap<AgentId, Agent>,
    appointments: BTreeMap<AgentId, Office>,
    /// 革职削籍的官员。
    purged: BTreeSet<AgentId>,
    /// 皇帝修道怠政中。
    retreat: bool,
    /// 按官职（`Office::code`）指定的承办模型与玉玺上限。
    office_models: BTreeMap<String, String>,
    office_ceilings: BTreeMap<String, Seal>,
    /// 度支：朝廷总额，与按官职（`Office::code`）的每件诏令限额（tokens）。
    court_budget: Option<u64>,
    office_budgets: BTreeMap<String, u64>,
    next_task_id: u64,
    next_agent_id: u64,
    tasks: BTreeMap<TaskId, Task>,
    /// 礼部科举的成绩，按应试先后。
    exams: Vec<ExamRecord>,
    /// 历次殿试。
    palace_exams: Vec<PalaceExam>,
    /// 翰林院自定（或覆盖内置）的模板。
    templates: BTreeMap<String, Template>,
    /// 各官职（`Office::code`）最近一次廷推，任命该官职后清空。
    nominations: BTreeMap<String, Vec<Nomination>>,
    /// 历次廷推所用的 token，与各官员累计所得的票数。
    nomination_usage: TokenUsage,
    nomination_votes: BTreeMap<AgentId, u32>,
    /// 受过廷杖的官员。
    canings: BTreeMap<AgentId, Caning>,
}

/// 一位官员的廷杖记录。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caning {
    /// 受杖次数（历来累计）。
    pub times: u32,
    /// 现降几级；宽宥或戴罪立功后为 0。
    pub demoted: u32,
    /// 最近一次受杖时的弹劾数与获采纳数，用来计「再遭弹劾」与「戴罪立功」。
    pub impeached_at: u32,
    pub accepted_at: u32,
}

impl Caning {
    /// 受杖后再获采纳这么多件回奏，即戴罪立功、复其品级与玉玺。
    pub const REDEMPTION: u32 = 3;
    /// 受杖前须（自上次受杖以来）遭弹劾这么多次。
    pub const IMPEACHMENTS: u32 = 2;
}

/// 一次科举应试（见 `CourtEvent::ExamTaken`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExamRecord {
    pub agent: AgentId,
    pub exam: String,
    pub score: u8,
    pub answer: String,
    pub model: Option<String>,
    pub usage: TokenUsage,
}
impl ExamRecord {
    /// 及格线。
    pub const PASS: u8 = 60;
    pub fn passed(&self) -> bool {
        self.score >= Self::PASS
    }
}

/// 一次殿试：皇帝的题目、各官员的答卷与钦点的状元。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PalaceExam {
    pub question: String,
    pub answers: Vec<Speech>,
    pub zhuangyuan: Option<AgentId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub edict: Edict,
    pub status: TaskStatus,
    /// 执行中的工具调用次数与其中失败的次数（含越出玉玺的尝试）。
    pub tool_calls: u32,
    pub tool_failures: u32,
    /// 承办期间模型调用的累计用量。
    pub usage: TokenUsage,
    /// 皇帝的评定；只有已完成的诏令才有。
    pub review: Option<(Verdict, String)>,
    /// 皇帝是否已对内阁票拟批红（只在内阁制下要求）。
    pub approved: bool,
    /// 最近一次指派时承办所用的模型（演示后端与早期日志为 `None`）。
    pub model: Option<String>,
    /// 内阁票拟（只在内阁制下产生）。
    pub draft: Option<Draft>,
    /// 承办官员呈报的文件改动（只有授予 `write_file` 的工部诏令才会有）。
    pub changes: Option<Changes>,
    /// 都察院的复核（只有已完成的诏令才有）。
    pub censure: Option<Censure>,
    /// 分办出此诏令的原诏令。
    pub parent: Option<TaskId>,
    /// 分办时指定的承办者（子诏令才有）。
    pub assignee: Option<AgentId>,
    /// 此诏令已分办（原诏令才有）。
    pub split: Option<Split>,
    /// 批红或评定是皇帝修道期间由司礼监代批的。
    pub proxied: bool,
    /// 历次廷议的发言（用量已计入 `usage`）；承办时附上最近一次的纪要。
    pub debates: Vec<Vec<Speech>>,
    /// 本件的度支（tokens），计入票拟、分办、廷议与复核；子诏令各自计。
    pub budget: Option<u64>,
    /// 刑部与锦衣卫的检出。
    pub suspicions: Vec<Suspicion>,
    /// 经兵部驿站入队时的来处。
    pub courier: Option<String>,
    /// 驿站送回的回奏评论与草稿 PR 的网址。
    pub replied: Option<String>,
    pub pull_request: Option<String>,
    /// 八百里加急。
    pub urgent: bool,
    /// 按翰林院模板下达时的模板名与验收标准。
    pub template: Option<TemplateUse>,
}

/// 诏令所用的模板（见 `CourtEvent::TemplateApplied`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateUse {
    pub name: String,
    pub criteria: Vec<String>,
}

/// 内置的翰林院模板：审查 unsafe、补测试、写文档。可以同名覆盖，不能撤去。
pub fn builtin_templates() -> Vec<(&'static str, Template)> {
    let seal = |tools: &[Capability]| Seal {
        capabilities: tools.iter().copied().collect(),
        scope: vec![".".into()],
        mcp: Default::default(),
    };
    let template = |text: &str, tools: &[Capability], criteria: &[&str]| Template {
        department: Department::Works,
        text: text.into(),
        seal: seal(tools),
        criteria: criteria.iter().map(|c| (*c).to_owned()).collect(),
    };
    use Capability::{ReadFile, RunTests, SearchCode, WriteFile};
    vec![
        (
            "审查unsafe",
            template(
                "审查项目中的 unsafe 代码：逐处说明用途、所依赖的安全前提，以及能否改为安全写法。",
                &[ReadFile, SearchCode],
                &[
                    "列出每一处 unsafe 的文件与行号",
                    "每一处都说明它依赖的安全前提",
                    "指出可以去掉的 unsafe，或说明为何不能去掉",
                ],
            ),
        ),
        (
            "补测试",
            template(
                "为下列代码补充单元测试，并运行测试确认通过：",
                &[ReadFile, SearchCode, WriteFile, RunTests],
                &[
                    "新增的测试覆盖诏令所指的行为",
                    "run_tests 运行通过",
                    "不改变被测代码的行为",
                ],
            ),
        ),
        (
            "写文档",
            template(
                "为下列内容撰写或更新文档：",
                &[ReadFile, SearchCode, WriteFile],
                &[
                    "文档与代码现状一致，不写不存在的功能",
                    "说明用法并给出示例",
                    "只改动文档文件",
                ],
            ),
        ),
    ]
}

/// 一处检出（见 `CourtEvent::SuspicionRaised`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suspicion {
    pub source: String,
    pub threat: Threat,
    pub excerpt: String,
    /// 刑部复勘（见 `CourtEvent::SuspicionReviewed`）。
    pub review: Option<SuspicionReview>,
    /// 皇帝已平反：不再妨碍代批。
    pub acquitted: bool,
}

/// 刑部复勘的意见（用量已计入 `Task::usage`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuspicionReview {
    pub reviewer: AgentId,
    /// 维持原检；否则判为误报。
    pub upheld: bool,
    pub note: String,
    pub model: Option<String>,
    pub usage: TokenUsage,
}

impl Task {
    /// 尚未平反的检出。有这样的检出时，司礼监不代批。
    pub fn standing_suspicions(&self) -> impl Iterator<Item = &Suspicion> {
        self.suspicions.iter().filter(|s| !s.acquitted)
    }
}

impl Suspicion {
    /// 驿传入队时对诏令内容的检出所记的来源。
    pub const COURIER: &'static str = "courier";

    /// 检出者：外来文书（工具输出、驿传）归锦衣卫，回奏归刑部。
    pub fn bureau(&self) -> &'static str {
        if self.source.starts_with("tool:") || self.source == Self::COURIER {
            "锦衣卫"
        } else {
            "刑部"
        }
    }

    /// 检出之处：工具名、「驿传」或「回奏」。
    pub fn origin(&self) -> &str {
        match self.source.strip_prefix("tool:") {
            Some(tool) => tool,
            None if self.source == Self::COURIER => "驿传",
            None => "回奏",
        }
    }
}

/// 分办方案。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split {
    pub subtasks: Vec<TaskId>,
    /// 尚书的拆分说明，或「御定分办」。
    pub note: String,
    pub model: Option<String>,
    /// 拆分所用的 token（已计入 `Task::usage`）。
    pub usage: TokenUsage,
}

/// 都察院复核的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Censure {
    pub censor: AgentId,
    pub verdict: CensorVerdict,
    pub note: String,
    pub evidence: Evidence,
    pub model: Option<String>,
    /// 复核所用的 token（已计入 `Task::usage`）。
    pub usage: TokenUsage,
}

/// 呈报的文件改动及皇帝的处置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changes {
    pub files: Vec<FileChange>,
    pub state: ChangeState,
    /// 逐段准奏时驳回的段号（从 1 起，跨文件连续编号）。
    pub skipped: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeState {
    /// 待皇帝落盘或驳回。
    Proposed,
    /// 已写入项目。
    Applied,
    /// 驳回，未写入。
    Discarded,
}

/// 内阁票拟的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    /// 拟派的承办者。
    pub worker: AgentId,
    /// 票拟意见（首辅所写，或「御笔改票」）。
    pub note: String,
    /// 最近一次由模型票拟时所用的模型。
    pub model: Option<String>,
    /// 历次票拟的累计用量（已计入 `Task::usage`）。
    pub usage: TokenUsage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    /// 已入队，尚未指派。
    Pending,
    /// 已指派、尚未结束（执行中，或上次运行中断）。
    Assigned(AgentId),
    Completed {
        worker: AgentId,
        summary: String,
    },
    /// `worker` 为失败时的承办官员；指派前（如路由失败）为 `None`。
    Failed {
        worker: Option<AgentId>,
        reason: String,
    },
    Cancelled,
}

impl Task {
    /// 经手此诏令的官员（执行中、完成或失败时）；撤销不算经手。
    pub fn handler(&self) -> Option<AgentId> {
        match &self.status {
            TaskStatus::Assigned(worker) | TaskStatus::Completed { worker, .. } => Some(*worker),
            TaskStatus::Failed { worker, .. } => *worker,
            // 撤销不计入：可能是皇帝改了主意，不代表官员表现。
            TaskStatus::Pending | TaskStatus::Cancelled => None,
        }
    }
}

impl TaskStatus {
    /// 未结束的任务可被（重新）执行或撤销。
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Pending | Self::Assigned(_))
    }
}

impl Court {
    pub fn replay(events: &[CourtEvent]) -> Result<Self, GovernanceError> {
        let mode = match events.first() {
            Some(CourtEvent::Founded(mode)) => *mode,
            _ => return Err(GovernanceError::NotFounded),
        };
        let mut court = Self {
            mode,
            agents: BTreeMap::new(),
            appointments: BTreeMap::new(),
            purged: BTreeSet::new(),
            retreat: false,
            office_models: BTreeMap::new(),
            office_ceilings: BTreeMap::new(),
            court_budget: None,
            office_budgets: BTreeMap::new(),
            next_task_id: 1,
            next_agent_id: 1,
            tasks: BTreeMap::new(),
            exams: Vec::new(),
            palace_exams: Vec::new(),
            templates: BTreeMap::new(),
            nominations: BTreeMap::new(),
            nomination_usage: TokenUsage::default(),
            nomination_votes: BTreeMap::new(),
            canings: BTreeMap::new(),
        };
        for event in events {
            court.apply(event)?;
        }
        Ok(court)
    }

    pub fn apply(&mut self, event: &CourtEvent) -> Result<(), GovernanceError> {
        match event {
            CourtEvent::Clock(_) => {}
            CourtEvent::Founded(mode) => self.mode = *mode,
            CourtEvent::AgentRegistered(agent) => {
                if agent.name.trim().is_empty() {
                    return Err(GovernanceError::EmptyName);
                }
                if self.agents.contains_key(&agent.id) {
                    return Err(GovernanceError::DuplicateAgent(agent.id));
                }
                self.next_agent_id = self.next_agent_id.max(agent.id.0 + 1);
                self.agents.insert(agent.id, agent.clone());
            }
            CourtEvent::OfficialAppointed { agent, office } => {
                self.propose_appointment(*agent, *office)?;
                self.appointments.insert(*agent, *office);
                // 官缺已补，此前的廷推作罢。
                self.nominations.remove(office.code().as_str());
            }
            CourtEvent::CourtNominated { office, votes } => {
                self.propose_nomination(*office, votes.clone())?;
                for vote in votes {
                    self.nomination_usage += vote.usage;
                    *self.nomination_votes.entry(vote.candidate).or_default() += 1;
                }
                self.nominations.insert(office.code(), votes.clone());
            }
            CourtEvent::OfficialCaned { agent } => {
                self.propose_caning(*agent)?;
                let record = self.service_record(*agent);
                // 戴罪未立功又受杖的，再降一级。
                let demoted = self.demotion(*agent) + 1;
                let entry = self.canings.entry(*agent).or_default();
                entry.times += 1;
                entry.demoted = demoted;
                entry.impeached_at = record.impeached;
                entry.accepted_at = record.accepted;
            }
            CourtEvent::OfficialPardoned { agent } => {
                self.propose_pardon(*agent)?;
                self.canings.get_mut(agent).expect("已确认受杖").demoted = 0;
            }
            CourtEvent::OfficialDismissed { agent } => {
                self.propose_dismissal(*agent)?;
                self.appointments.remove(agent);
            }
            CourtEvent::OfficialPurged { agent } => {
                self.propose_purge(*agent)?;
                self.appointments.remove(agent);
                self.purged.insert(*agent);
            }
            CourtEvent::EdictDrafted {
                task,
                worker,
                note,
                model,
                usage,
            } => {
                self.propose_draft(*task, *worker, note.clone(), model.clone(), *usage)?;
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                entry.usage += *usage;
                let (mut total, mut model) = (*usage, model.clone());
                if let Some(previous) = &entry.draft {
                    total += previous.usage;
                    model = model.or_else(|| previous.model.clone());
                }
                entry.draft = Some(Draft {
                    worker: *worker,
                    note: note.clone(),
                    model,
                    usage: total,
                });
            }
            CourtEvent::EdictApproved { task } => {
                self.propose_approval(*task)?;
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                entry.approved = true;
                entry.proxied |= self.retreat;
            }
            CourtEvent::EmperorRetreat(on) => {
                self.propose_retreat(*on)?;
                self.retreat = *on;
            }
            CourtEvent::InstitutionReformed(mode) => {
                self.propose_reform(*mode)?;
                // 改制时撤销旧制度协调机构的官职；六部官员保留（品级随制度变化）。
                self.appointments
                    .retain(|_, office| office.mode().is_none());
                self.mode = *mode;
            }
            CourtEvent::EdictIssued(edict) => {
                if edict.text.trim().is_empty() {
                    return Err(GovernanceError::EmptyEdict);
                }
                validate_edict_seal(edict.department, &edict.seal)?;
                if self.tasks.contains_key(&edict.id) {
                    return Err(GovernanceError::DuplicateEdict(edict.id));
                }
                self.next_task_id = self.next_task_id.max(edict.id.0 + 1);
                self.tasks.insert(
                    edict.id,
                    Task {
                        edict: edict.clone(),
                        status: TaskStatus::Pending,
                        tool_calls: 0,
                        tool_failures: 0,
                        usage: TokenUsage::default(),
                        review: None,
                        approved: false,
                        model: None,
                        draft: None,
                        changes: None,
                        censure: None,
                        parent: None,
                        assignee: None,
                        split: None,
                        proxied: false,
                        debates: Vec::new(),
                        budget: None,
                        suspicions: Vec::new(),
                        courier: None,
                        replied: None,
                        pull_request: None,
                        urgent: false,
                        template: None,
                    },
                );
            }
            CourtEvent::TaskAssigned {
                task,
                worker,
                model,
            } => {
                self.open_task(*task)?;
                if !self.agents.contains_key(worker) {
                    return Err(GovernanceError::UnknownAgent(*worker));
                }
                self.set_status(*task, TaskStatus::Assigned(*worker));
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                entry.model = model.clone();
                // 重新指派（如上次运行中断）：上次暂存的改动作废。
                entry.changes = None;
            }
            CourtEvent::OfficeModelSet { office, model } => {
                self.propose_office_model(*office, model.clone())?;
                match model {
                    Some(m) => self.office_models.insert(office.code(), m.clone()),
                    None => self.office_models.remove(&office.code()),
                };
            }
            CourtEvent::BudgetSet { scope, tokens } => {
                self.propose_budget(*scope, *tokens)?;
                match scope {
                    BudgetScope::Court => self.court_budget = *tokens,
                    BudgetScope::Office(office) => match tokens {
                        Some(t) => {
                            self.office_budgets.insert(office.code(), *t);
                        }
                        None => {
                            self.office_budgets.remove(&office.code());
                        }
                    },
                    BudgetScope::Task(task) => {
                        self.tasks.get_mut(task).expect("已确认存在").budget = *tokens;
                    }
                }
            }
            CourtEvent::OfficeCeilingSet { office, ceiling } => {
                self.propose_office_ceiling(*office, ceiling.clone())?;
                match ceiling {
                    Some(c) => self.office_ceilings.insert(office.code(), c.clone()),
                    None => self.office_ceilings.remove(&office.code()),
                };
            }
            CourtEvent::ToolInvoked { task, ok, .. } => {
                if !matches!(self.open_task(*task)?, TaskStatus::Assigned(_)) {
                    return Err(GovernanceError::TaskNotAssigned(*task));
                }
                let entry = self.tasks.get_mut(task).expect("open_task 已确认存在");
                entry.tool_calls += 1;
                entry.tool_failures += u32::from(!ok);
            }
            CourtEvent::TaskCompleted {
                task,
                worker,
                summary,
            } => {
                // v0.1 的日志没有 TaskAssigned，直接从 Pending 完成。
                match self.open_task(*task)? {
                    TaskStatus::Assigned(assigned) if assigned != worker => {
                        return Err(GovernanceError::WrongWorker {
                            task: *task,
                            worker: *worker,
                        });
                    }
                    _ => {}
                }
                if !self.agents.contains_key(worker) {
                    return Err(GovernanceError::UnknownAgent(*worker));
                }
                self.set_status(
                    *task,
                    TaskStatus::Completed {
                        worker: *worker,
                        summary: summary.clone(),
                    },
                );
            }
            CourtEvent::ChangesProposed { task, changes } => {
                if !matches!(self.open_task(*task)?, TaskStatus::Assigned(_)) {
                    return Err(GovernanceError::TaskNotAssigned(*task));
                }
                self.tasks.get_mut(task).expect("已确认存在").changes = Some(Changes {
                    files: changes.clone(),
                    state: ChangeState::Proposed,
                    skipped: Vec::new(),
                });
            }
            CourtEvent::EdictSplit {
                task,
                subtasks,
                note,
                model,
                usage,
            } => {
                let plan: Vec<_> = subtasks
                    .iter()
                    .map(|s| (s.worker, s.text.clone()))
                    .collect();
                self.check_split(*task, &plan)?;
                for (i, sub) in subtasks.iter().enumerate() {
                    if self.tasks.contains_key(&sub.id)
                        || subtasks[..i].iter().any(|s| s.id == sub.id)
                    {
                        return Err(GovernanceError::DuplicateEdict(sub.id));
                    }
                }
                let parent = self.tasks.get(task).expect("已确认存在").clone();
                for sub in subtasks {
                    self.next_task_id = self.next_task_id.max(sub.id.0 + 1);
                    self.tasks.insert(
                        sub.id,
                        Task {
                            edict: Edict {
                                id: sub.id,
                                text: sub.text.clone(),
                                ..parent.edict.clone()
                            },
                            status: TaskStatus::Pending,
                            tool_calls: 0,
                            tool_failures: 0,
                            usage: TokenUsage::default(),
                            review: None,
                            approved: false,
                            model: None,
                            draft: None,
                            changes: None,
                            censure: None,
                            parent: Some(*task),
                            courier: None,
                            replied: None,
                            pull_request: None,
                            urgent: parent.urgent,
                            template: None,
                            assignee: Some(sub.worker),
                            split: None,
                            proxied: false,
                            debates: Vec::new(),
                            budget: None,
                            suspicions: Vec::new(),
                        },
                    );
                }
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                entry.usage += *usage;
                entry.split = Some(Split {
                    subtasks: subtasks.iter().map(|s| s.id).collect(),
                    note: note.clone(),
                    model: model.clone(),
                    usage: *usage,
                });
            }
            CourtEvent::ExamTaken {
                agent,
                exam,
                score,
                answer,
                model,
                usage,
            } => {
                self.propose_exam(*agent)?;
                if *score > 100 {
                    return Err(GovernanceError::InvalidExam("分数须为 0–100".into()));
                }
                self.exams.push(ExamRecord {
                    agent: *agent,
                    exam: exam.clone(),
                    score: *score,
                    answer: answer.clone(),
                    model: model.clone(),
                    usage: *usage,
                });
            }
            CourtEvent::PalaceExamHeld { question, answers } => {
                self.propose_palace_exam(question, answers)?;
                self.palace_exams.push(PalaceExam {
                    question: question.clone(),
                    answers: answers.clone(),
                    zhuangyuan: None,
                });
            }
            CourtEvent::TemplateSaved { name, template } => {
                self.propose_template(name.clone(), template.clone())?;
                self.templates.insert(name.clone(), template.clone());
            }
            CourtEvent::TemplateRemoved { name } => {
                self.propose_template_removal(name.clone())?;
                self.templates.remove(name);
            }
            CourtEvent::TemplateApplied {
                task,
                name,
                criteria,
            } => {
                self.check_template_use(*task)?;
                check_criteria(criteria)?;
                self.tasks.get_mut(task).expect("已确认存在").template = Some(TemplateUse {
                    name: name.clone(),
                    criteria: criteria.clone(),
                });
            }
            CourtEvent::CourierDelivered { task, station } => {
                self.propose_courier(*task, station.clone())?;
                self.tasks.get_mut(task).expect("已确认存在").courier = Some(station.clone());
            }
            CourtEvent::CourierReplied {
                task,
                pull_request,
                url,
            } => {
                self.propose_courier_reply(*task, *pull_request, url)?;
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                if *pull_request {
                    entry.pull_request = Some(url.clone());
                } else {
                    entry.replied = Some(url.clone());
                }
            }
            CourtEvent::EdictExpedited { task } => {
                self.propose_expedite(*task)?;
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                entry.urgent = true;
                // 已分办的，未结的子诏令一并加急。
                let subtasks = entry
                    .split
                    .as_ref()
                    .map(|s| s.subtasks.clone())
                    .unwrap_or_default();
                for id in subtasks {
                    if let Some(sub) = self.tasks.get_mut(&id).filter(|t| t.status.is_open()) {
                        sub.urgent = true;
                    }
                }
            }
            CourtEvent::ZhuangyuanNamed { agent } => {
                self.propose_zhuangyuan(*agent)?;
                let last = self.palace_exams.last_mut().expect("已确认有殿试");
                last.zhuangyuan = Some(*agent);
            }
            CourtEvent::CourtDebated { task, speeches } => {
                self.propose_debate(*task, speeches)?;
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                for speech in speeches {
                    entry.usage += speech.usage;
                }
                entry.debates.push(speeches.clone());
            }
            CourtEvent::CensorReported {
                task,
                censor,
                verdict,
                note,
                evidence,
                model,
                usage,
            } => {
                self.propose_censure_check(*task, *censor)?;
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                entry.usage += *usage;
                entry.censure = Some(Censure {
                    censor: *censor,
                    verdict: *verdict,
                    note: note.clone(),
                    evidence: *evidence,
                    model: model.clone(),
                    usage: *usage,
                });
            }
            CourtEvent::ChangesApplied { task, skipped } => {
                self.propose_application(*task, skipped.clone())?;
                let changes = self.tasks.get_mut(task).and_then(|t| t.changes.as_mut());
                let changes = changes.expect("已确认有待决改动");
                changes.state = ChangeState::Applied;
                changes.skipped = skipped.clone();
            }
            CourtEvent::ChangesDiscarded { task } => {
                self.propose_changes_decision(*task, false)?;
                let changes = self.tasks.get_mut(task).and_then(|t| t.changes.as_mut());
                changes.expect("已确认有待决改动").state = ChangeState::Discarded;
            }
            CourtEvent::TaskFailed { task, reason } => {
                let worker = match self.open_task(*task)? {
                    TaskStatus::Assigned(worker) => Some(*worker),
                    _ => None,
                };
                self.set_status(
                    *task,
                    TaskStatus::Failed {
                        worker,
                        reason: reason.clone(),
                    },
                );
            }
            CourtEvent::SuspicionRaised {
                task,
                source,
                threat,
                excerpt,
            } => {
                let entry = self
                    .tasks
                    .get_mut(task)
                    .ok_or(GovernanceError::UnknownEdict(*task))?;
                // 驿传的检出在入队时记录，其余须在执行中或之后。
                if entry.status == TaskStatus::Pending && source != Suspicion::COURIER {
                    return Err(GovernanceError::TaskNotAssigned(*task));
                }
                entry.suspicions.push(Suspicion {
                    source: source.clone(),
                    threat: *threat,
                    excerpt: excerpt.clone(),
                    review: None,
                    acquitted: false,
                });
            }
            CourtEvent::SuspicionReviewed {
                task,
                index,
                reviewer,
                upheld,
                note,
                model,
                usage,
            } => {
                self.propose_recheck(*task, *index, *reviewer)?;
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                entry.usage += *usage;
                entry.suspicions[*index as usize].review = Some(SuspicionReview {
                    reviewer: *reviewer,
                    upheld: *upheld,
                    note: note.clone(),
                    model: model.clone(),
                    usage: *usage,
                });
            }
            CourtEvent::SuspicionAcquitted { task, index } => {
                self.propose_acquittal(*task, *index)?;
                let entry = self.tasks.get_mut(task).expect("已确认存在");
                entry.suspicions[*index as usize].acquitted = true;
            }
            CourtEvent::ModelUsage { task, usage, .. } => {
                let entry = self
                    .tasks
                    .get_mut(task)
                    .ok_or(GovernanceError::UnknownEdict(*task))?;
                if entry.status == TaskStatus::Pending {
                    return Err(GovernanceError::TaskNotAssigned(*task));
                }
                entry.usage += *usage;
            }
            CourtEvent::TaskReviewed {
                task,
                verdict,
                note,
            } => {
                self.propose_review(*task, *verdict, note.clone())?;
                let entry = self.tasks.get_mut(task).expect("propose_review 已确认存在");
                entry.review = Some((*verdict, note.clone()));
                entry.proxied |= self.retreat;
            }
            CourtEvent::TaskCancelled { task } => {
                self.propose_cancellation(*task)?;
                self.set_status(*task, TaskStatus::Cancelled);
            }
        }
        Ok(())
    }

    pub fn mode(&self) -> GovernanceMode {
        self.mode
    }

    pub fn agent(&self, id: AgentId) -> Option<&Agent> {
        self.agents.get(&id)
    }

    /// 按 ID 升序列出全部在册官员。
    pub fn agents(&self) -> impl Iterator<Item = &Agent> {
        self.agents.values()
    }

    pub fn office_of(&self, id: AgentId) -> Option<Office> {
        self.appointments.get(&id).copied()
    }

    pub fn appointed_count(&self) -> usize {
        self.appointments.len()
    }

    pub fn completed_count(&self) -> usize {
        self.tasks
            .values()
            .filter(|t| matches!(t.status, TaskStatus::Completed { .. }))
            .count()
    }

    /// 全部诏令的模型 token 用量。
    pub fn total_usage(&self) -> TokenUsage {
        let mut total = TokenUsage::default();
        for task in self.tasks.values() {
            total += task.usage;
        }
        total += self.exam_usage();
        total += self.nomination_usage;
        total
    }

    /// 科举与殿试所用的 token（不属于任何诏令）。
    pub fn exam_usage(&self) -> TokenUsage {
        let mut total = TokenUsage::default();
        for exam in &self.exams {
            total += exam.usage;
        }
        for answer in self.palace_exams.iter().flat_map(|p| &p.answers) {
            total += answer.usage;
        }
        total
    }

    pub fn task(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(&id)
    }

    /// 按 ID 升序列出全部诏令。
    pub fn tasks(&self) -> impl DoubleEndedIterator<Item = &Task> {
        self.tasks.values()
    }

    /// 翰林院实录：已完成的原诏令（子诏令随原诏令归档），按 ID 升序。
    pub fn records(&self) -> Vec<Record> {
        self.tasks
            .values()
            .filter(|t| t.parent.is_none())
            .filter_map(|t| {
                let TaskStatus::Completed { worker, summary } = &t.status else {
                    return None;
                };
                let mut verdict = Vec::new();
                if let Some((v, note)) = &t.review {
                    verdict.push(if note.is_empty() {
                        v.label().to_owned()
                    } else {
                        format!("{}：{note}", v.label())
                    });
                }
                if let Some(c) = &t.censure {
                    verdict.push(format!("都察院{}", c.verdict.label()));
                }
                Some(Record {
                    task: t.edict.id,
                    department: t.edict.department,
                    edict: t.edict.text.clone(),
                    worker: *worker,
                    handler: self
                        .agent(*worker)
                        .map(|a| a.name.clone())
                        .unwrap_or_default(),
                    summary: summary.clone(),
                    verdict: if verdict.is_empty() {
                        "待评定".into()
                    } else {
                        verdict.join("；")
                    },
                })
            })
            .collect()
    }

    /// 未结束的诏令，按 ID 升序。
    pub fn open_tasks(&self) -> Vec<TaskId> {
        self.tasks
            .iter()
            .filter(|(_, t)| t.status.is_open())
            .map(|(id, _)| *id)
            .collect()
    }

    /// 内阁制下尚未批红的待办诏令：须先批红才能执行。子诏令随原诏令批红，自身不批红。
    pub fn needs_approval(&self, task: &Task) -> bool {
        self.mode == GovernanceMode::Cabinet
            && !task.approved
            && task.status == TaskStatus::Pending
            && task.parent.is_none()
    }

    /// 已分办的原诏令中尚未结束的子诏令数。
    pub fn open_subtasks(&self, task: &Task) -> usize {
        task.split.as_ref().map_or(0, |split| {
            split
                .subtasks
                .iter()
                .filter(|id| self.tasks.get(id).is_some_and(|t| t.status.is_open()))
                .count()
        })
    }

    /// 现在就可以执行的诏令：未结束，（内阁制下）已批红；子诏令须原诏令已批红，
    /// 已分办的原诏令须子诏令全部结束（届时由尚书汇总）。按 ID 升序。
    pub fn runnable_tasks(&self) -> Vec<TaskId> {
        let mut tasks = self
            .tasks
            .values()
            .filter(|t| t.status.is_open() && !self.needs_approval(t))
            .filter(|t| {
                t.parent
                    .and_then(|p| self.tasks.get(&p))
                    .is_none_or(|p| !self.needs_approval(p))
            })
            .filter(|t| self.open_subtasks(t) == 0)
            .collect::<Vec<_>>();
        // 兵部：八百里加急插队先行，其余按诏令先后（排序稳定）。
        tasks.sort_by_key(|t| !t.urgent);
        tasks.into_iter().map(|t| t.edict.id).collect()
    }

    /// 分办：把待办诏令 `task` 拆给本部官员，`plan` 为 (承办者, 子诏令内容)。
    pub fn propose_split(
        &self,
        task: TaskId,
        plan: Vec<(AgentId, String)>,
        note: String,
        model: Option<String>,
        usage: TokenUsage,
    ) -> Result<CourtEvent, GovernanceError> {
        self.check_split(task, &plan)?;
        let subtasks = plan
            .into_iter()
            .zip(self.next_task_id..)
            .map(|((worker, text), id)| Subtask {
                id: TaskId(id),
                worker,
                text,
            })
            .collect();
        Ok(CourtEvent::EdictSplit {
            task,
            subtasks,
            note,
            model,
            usage,
        })
    }

    fn check_split(&self, task: TaskId, plan: &[(AgentId, String)]) -> Result<(), GovernanceError> {
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        if entry.status != TaskStatus::Pending || entry.split.is_some() || entry.parent.is_some() {
            return Err(GovernanceError::CannotSplit(task));
        }
        if !(2..=8).contains(&plan.len()) {
            return Err(GovernanceError::InvalidSplit(format!(
                "须分为 2–8 件，实为 {} 件",
                plan.len()
            )));
        }
        if plan.iter().any(|(_, text)| text.trim().is_empty()) {
            return Err(GovernanceError::InvalidSplit("子诏令内容不能为空".into()));
        }
        let department = entry.edict.department;
        let staff = self.draft_candidates(department);
        if let Some((worker, _)) = plan
            .iter()
            .find(|(w, _)| !staff.iter().any(|(a, _)| a.id == *w))
        {
            return Err(GovernanceError::NotEligible {
                worker: *worker,
                department,
            });
        }
        Ok(())
    }

    /// 为官职指定承办模型；`None` 恢复默认。
    pub fn propose_office_model(
        &self,
        office: Office,
        model: Option<String>,
    ) -> Result<CourtEvent, GovernanceError> {
        if let Some(m) = &model
            && (m.is_empty() || m.chars().any(char::is_whitespace))
        {
            return Err(GovernanceError::InvalidModel(m.clone()));
        }
        Ok(CourtEvent::OfficeModelSet { office, model })
    }

    /// 为官职设定玉玺上限；`None` 取消上限。
    pub fn propose_office_ceiling(
        &self,
        office: Office,
        ceiling: Option<Seal>,
    ) -> Result<CourtEvent, GovernanceError> {
        if let Some(seal) = &ceiling {
            validate_seal(seal)?;
        }
        Ok(CourtEvent::OfficeCeilingSet { office, ceiling })
    }

    /// 设定度支：须为正数；诏令须尚未结束。
    pub fn propose_budget(
        &self,
        scope: BudgetScope,
        tokens: Option<u64>,
    ) -> Result<CourtEvent, GovernanceError> {
        if tokens == Some(0) {
            return Err(GovernanceError::InvalidBudget);
        }
        if let BudgetScope::Task(task) = scope {
            self.open_task(task)?;
        }
        Ok(CourtEvent::BudgetSet { scope, tokens })
    }

    pub fn budget(&self, scope: BudgetScope) -> Option<u64> {
        match scope {
            BudgetScope::Court => self.court_budget,
            BudgetScope::Office(office) => self.office_budgets.get(&office.code()).copied(),
            BudgetScope::Task(task) => self.tasks.get(&task)?.budget,
        }
    }

    /// 朝廷度支的余额；未设总额时为 `None`。
    pub fn treasury_remaining(&self) -> Option<u64> {
        let limit = self.court_budget?;
        Some(limit.saturating_sub(self.total_usage().total()))
    }

    /// 国库已告罄（计入尚未记账的 `pending` tokens）则报错。
    pub fn check_treasury(&self, pending: u64) -> Result<(), GovernanceError> {
        match (self.court_budget, self.treasury_remaining()) {
            (Some(limit), Some(left)) if left <= pending => {
                Err(GovernanceError::TreasuryExhausted(limit))
            }
            _ => Ok(()),
        }
    }

    /// 为 `task` 再调用模型（票拟、分办、廷议、复核）之前：国库与本件度支都须有余。
    pub fn check_funds(&self, task: &Task, pending: u64) -> Result<(), GovernanceError> {
        self.check_treasury(pending)?;
        self.check_task_funds(task, None, pending)
    }

    /// `task` 由 `worker` 承办时的度支：本件限额与承办者官职的每件限额中较小者。
    pub fn task_limit(&self, task: &Task, worker: Option<AgentId>) -> Option<u64> {
        let office = worker
            .and_then(|w| self.office_of(w))
            .and_then(|o| self.budget(BudgetScope::Office(o)));
        [task.budget, office].into_iter().flatten().min()
    }

    /// 诏令已用尽 `task_limit`（计入尚未记账的 `pending` tokens）则报错。
    pub fn check_task_funds(
        &self,
        task: &Task,
        worker: Option<AgentId>,
        pending: u64,
    ) -> Result<(), GovernanceError> {
        match self.task_limit(task, worker) {
            Some(limit) if task.usage.total() + pending >= limit => {
                Err(GovernanceError::TaskOverBudget {
                    task: task.edict.id,
                    limit,
                })
            }
            _ => Ok(()),
        }
    }

    /// 官职指定的承办模型；未指定时用默认模型。
    pub fn office_model(&self, office: Office) -> Option<&str> {
        self.office_models.get(&office.code()).map(String::as_str)
    }

    pub fn office_ceiling(&self, office: Office) -> Option<&Seal> {
        self.office_ceilings.get(&office.code())
    }

    /// `agent` 承办诏令时所用的模型（按其现任官职）；`None` 表示默认模型。
    pub fn model_of(&self, agent: AgentId) -> Option<&str> {
        self.office_of(agent).and_then(|o| self.office_model(o))
    }

    /// `agent` 承办 `edict` 时实际可用的玉玺：受其现任官职上限约束。
    /// 诏令的查阅范围与上限毫无交集时返回 `Err(SealExceedsCeiling)`。
    pub fn effective_seal(&self, agent: AgentId, edict: &Edict) -> Result<Seal, GovernanceError> {
        let mut seal = match self
            .office_of(agent)
            .and_then(|o| Some((o, self.office_ceiling(o)?)))
        {
            None => edict.seal.clone(),
            Some((office, ceiling)) => edict
                .seal
                .within(ceiling)
                .ok_or(GovernanceError::SealExceedsCeiling(office))?,
        };
        // 廷杖降级留用期间，收回工部动土之权与外藩工具。
        if self.demotion(agent) > 0 {
            seal.mcp.clear();
            seal.capabilities.retain(|c| !c.is_works_only());
        }
        Ok(seal)
    }

    /// 皇帝是否在修道怠政。
    pub fn in_retreat(&self) -> bool {
        self.retreat
    }

    pub fn propose_retreat(&self, on: bool) -> Result<CourtEvent, GovernanceError> {
        if self.retreat == on {
            return Err(GovernanceError::RetreatUnchanged(on));
        }
        Ok(CourtEvent::EmperorRetreat(on))
    }

    /// 司礼监代批红：`tasks` 中（`None` 为全部）待批红的诏令。只在修道期间有事可做。
    pub fn proxy_approvals(&self, tasks: Option<&[TaskId]>) -> Vec<CourtEvent> {
        if !self.retreat {
            return Vec::new();
        }
        self.tasks
            .values()
            .filter(|t| tasks.is_none_or(|ids| ids.contains(&t.edict.id)))
            .filter(|t| self.needs_approval(t))
            // 有未平反的密报（如驿传内容可疑），司礼监不敢代批红。
            .filter(|t| t.standing_suspicions().next().is_none())
            .map(|t| CourtEvent::EdictApproved { task: t.edict.id })
            .collect()
    }

    pub fn is_purged(&self, agent: AgentId) -> bool {
        self.purged.contains(&agent)
    }

    /// 革职：除去官职并削去仕籍，此后不得再任官。皇帝可革任何在籍官员。
    pub fn propose_purge(&self, agent: AgentId) -> Result<CourtEvent, GovernanceError> {
        if !self.agents.contains_key(&agent) {
            return Err(GovernanceError::UnknownAgent(agent));
        }
        if self.purged.contains(&agent) {
            return Err(GovernanceError::Purged(agent));
        }
        Ok(CourtEvent::OfficialPurged { agent })
    }

    /// 批红：内阁制下，诏令经内阁票拟（派工路线）后须皇帝批红才能执行。
    pub fn propose_approval(&self, task: TaskId) -> Result<CourtEvent, GovernanceError> {
        if self.mode != GovernanceMode::Cabinet {
            return Err(GovernanceError::ApprovalNotNeeded);
        }
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        if !entry.status.is_open() {
            return Err(GovernanceError::TaskFinished(task));
        }
        if !self.needs_approval(entry) {
            return Err(GovernanceError::AlreadyApproved(task));
        }
        Ok(CourtEvent::EdictApproved { task })
    }

    /// 票拟：内阁制下、批红之前，拟派本部在任官员承办。
    pub fn propose_draft(
        &self,
        task: TaskId,
        worker: AgentId,
        note: String,
        model: Option<String>,
        usage: TokenUsage,
    ) -> Result<CourtEvent, GovernanceError> {
        if self.mode != GovernanceMode::Cabinet {
            return Err(GovernanceError::NoCabinet);
        }
        // 诏令须存在、未结束且尚未批红。
        self.propose_approval(task)?;
        let department = self.tasks[&task].edict.department;
        if !self
            .draft_candidates(department)
            .iter()
            .any(|(a, _)| a.id == worker)
        {
            return Err(GovernanceError::NotEligible { worker, department });
        }
        Ok(CourtEvent::EdictDrafted {
            task,
            worker,
            note,
            model,
            usage,
        })
    }

    /// 可拟派承办 `department` 诏令的官员：本部在任尚书、侍郎、郎中（依次排列）。
    pub fn draft_candidates(&self, department: Department) -> Vec<(&Agent, Office)> {
        let mut staff: Vec<_> = self
            .appointments
            .iter()
            .filter(|(_, office)| department_of(**office) == Some(department))
            .filter_map(|(id, office)| Some((self.agents.get(id)?, *office)))
            .collect();
        staff.sort_by_key(|(agent, office)| {
            let rank = match office {
                Office::Minister(_) => 0,
                Office::ViceMinister(_) => 1,
                _ => 2,
            };
            (rank, agent.id)
        });
        staff
    }

    /// 本部长官：尚书，空缺则由侍郎署理。
    pub fn department_head(&self, department: Department) -> Option<(&Agent, Office)> {
        self.head_or_deputy(
            Office::Minister(department),
            Office::ViceMinister(department),
        )
    }

    /// 现行制度下的协调者（正职，空缺则副职）及其官职；废相亲政时为 `None`。
    pub fn coordinator(&self) -> Option<(&Agent, Office)> {
        match self.mode {
            GovernanceMode::Chancellor => {
                self.head_or_deputy(Office::Chancellor, Office::ViceChancellor)
            }
            GovernanceMode::Cabinet => {
                self.head_or_deputy(Office::FirstGrandSecretary, Office::SecondGrandSecretary)
            }
            GovernanceMode::DirectRule => None,
        }
    }

    /// 廷议可否：诏令待办，发言者在发言时担任所记官职。
    pub fn propose_debate(&self, task: TaskId, speeches: &[Speech]) -> Result<(), GovernanceError> {
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        if entry.status != TaskStatus::Pending {
            return Err(GovernanceError::CannotDebate(task));
        }
        if speeches.is_empty() {
            return Err(GovernanceError::InvalidDebate("没有发言".into()));
        }
        for speech in speeches {
            if self.appointments.get(&speech.speaker) != Some(&speech.office) {
                return Err(GovernanceError::NotAppointed(speech.speaker));
            }
        }
        Ok(())
    }

    /// 应试者须在册、未被革职。
    pub fn propose_exam(&self, agent: AgentId) -> Result<(), GovernanceError> {
        if !self.agents.contains_key(&agent) {
            return Err(GovernanceError::UnknownAgent(agent));
        }
        if self.purged.contains(&agent) {
            return Err(GovernanceError::Purged(agent));
        }
        Ok(())
    }

    /// 殿试：题目非空，至少一份答卷，答卷者在任所记官职且不重复。
    pub fn propose_palace_exam(
        &self,
        question: &str,
        answers: &[Speech],
    ) -> Result<(), GovernanceError> {
        if question.trim().is_empty() {
            return Err(GovernanceError::InvalidExam("殿试须有题目".into()));
        }
        if answers.is_empty() {
            return Err(GovernanceError::InvalidExam("无人作答".into()));
        }
        for (i, answer) in answers.iter().enumerate() {
            if self.appointments.get(&answer.speaker) != Some(&answer.office) {
                return Err(GovernanceError::NotAppointed(answer.speaker));
            }
            if answers[..i].iter().any(|a| a.speaker == answer.speaker) {
                let why = format!("官员 #{} 重复作答", answer.speaker.0);
                return Err(GovernanceError::InvalidExam(why));
            }
        }
        Ok(())
    }

    /// 参与廷推的九卿：六部尚书、左都御史与丞相或首辅（在任者）。须有吏部尚书主持。
    pub fn nine_ministers(&self) -> Result<Vec<(&Agent, Office)>, GovernanceError> {
        let personnel = Office::Minister(Department::Personnel);
        if self.appointed_as(personnel).is_none() {
            return Err(GovernanceError::Court(
                "廷推须由吏部会同九卿，吏部尚书空缺，不能廷推".into(),
            ));
        }
        let mut offices: Vec<Office> = Department::ALL
            .iter()
            .map(|&d| Office::Minister(d))
            .collect();
        offices.extend([
            Office::Censor,
            Office::Chancellor,
            Office::FirstGrandSecretary,
        ]);
        Ok(offices
            .into_iter()
            .filter_map(|o| self.appointed_as(o).map(|a| (a, o)))
            .collect())
    }

    /// 廷推 `office` 的候选人：可任此职（专长、制度符合，未革职）且现不任此职，按吏部考绩排序。
    pub fn nomination_candidates(
        &self,
        office: Office,
    ) -> Result<Vec<Candidate<'_>>, GovernanceError> {
        let candidates = self.recommend(office)?;
        Ok(candidates
            .into_iter()
            .filter(|c| c.current != Some(office))
            .collect())
    }

    /// 廷推的投票者与候选人；官职未出缺、无吏部尚书、无候选人时报错。
    pub fn nomination_ballot(
        &self,
        office: Office,
    ) -> Result<NominationBallot<'_>, GovernanceError> {
        let bad = |why: String| Err(GovernanceError::Court(format!("廷推不成：{why}")));
        if office.is_unique() && self.appointed_as(office).is_some() {
            return bad(format!("{}未出缺", office.label()));
        }
        let ministers = self.nine_ministers()?;
        let candidates = self.nomination_candidates(office)?;
        if candidates.is_empty() {
            return bad(format!("无人可任{}", office.label()));
        }
        Ok((ministers, candidates))
    }

    /// 廷推：唯一的官职须出缺；每位投票者须是九卿之一、各投一次，所推须是候选人且不是自己。
    pub fn propose_nomination(
        &self,
        office: Office,
        votes: Vec<Nomination>,
    ) -> Result<CourtEvent, GovernanceError> {
        let bad = |why: String| Err(GovernanceError::Court(format!("廷推不成：{why}")));
        let (ministers, candidates) = self.nomination_ballot(office)?;
        if votes.is_empty() {
            return bad("无人推举".into());
        }
        let mut seen = BTreeSet::new();
        for vote in &votes {
            if !ministers.iter().any(|(a, _)| a.id == vote.voter) {
                return bad(format!("#{} 不在九卿之列", vote.voter.0));
            }
            if !seen.insert(vote.voter) {
                return bad(format!("#{} 重复推举", vote.voter.0));
            }
            if vote.candidate == vote.voter
                || !candidates.iter().any(|c| c.agent.id == vote.candidate)
            {
                return bad(format!("#{} 不可任{}", vote.candidate.0, office.label()));
            }
        }
        Ok(CourtEvent::CourtNominated { office, votes })
    }

    /// `office` 最近一次（任命之前）的廷推。
    pub fn nomination(&self, office: Office) -> Option<&[Nomination]> {
        self.nominations
            .get(office.code().as_str())
            .map(Vec::as_slice)
    }

    /// 廷推得票，按票数从多到少（同票者先得票的在前）。
    pub fn nomination_tally(&self, office: Office) -> Vec<(AgentId, u32)> {
        let mut tally: Vec<(AgentId, u32)> = Vec::new();
        for vote in self.nomination(office).unwrap_or_default() {
            match tally.iter_mut().find(|(id, _)| *id == vote.candidate) {
                Some((_, n)) => *n += 1,
                None => tally.push((vote.candidate, 1)),
            }
        }
        tally.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        tally
    }

    /// 廷杖：须为在任、未革职的官员，且自上次受杖以来遭都察院弹劾至少 [`Caning::IMPEACHMENTS`] 次。
    pub fn propose_caning(&self, agent: AgentId) -> Result<CourtEvent, GovernanceError> {
        if !self.agents.contains_key(&agent) {
            return Err(GovernanceError::UnknownAgent(agent));
        }
        if self.purged.contains(&agent) {
            return Err(GovernanceError::Purged(agent));
        }
        if !self.appointments.contains_key(&agent) {
            return Err(GovernanceError::Court(
                "廷杖是降级留用之罚，须为在任官员；未任官职者无级可降".into(),
            ));
        }
        let since = self.impeachments_since_caning(agent);
        if since < Caning::IMPEACHMENTS {
            return Err(GovernanceError::Court(format!(
                "廷杖须屡遭弹劾：#{} 自上次受杖以来遭弹劾 {since} 次，不足 {} 次",
                agent.0,
                Caning::IMPEACHMENTS
            )));
        }
        Ok(CourtEvent::OfficialCaned { agent })
    }

    /// 自上次受杖（未受过则自始）以来遭弹劾的次数。
    pub fn impeachments_since_caning(&self, agent: AgentId) -> u32 {
        let at = self.canings.get(&agent).map_or(0, |c| c.impeached_at);
        self.service_record(agent).impeached.saturating_sub(at)
    }

    /// 宽宥：须正在降级留用。
    pub fn propose_pardon(&self, agent: AgentId) -> Result<CourtEvent, GovernanceError> {
        if self.demotion(agent) == 0 {
            return Err(GovernanceError::Court(format!(
                "#{} 并未降级留用，无须宽宥",
                agent.0
            )));
        }
        Ok(CourtEvent::OfficialPardoned { agent })
    }

    /// 廷杖记录。
    pub fn caning(&self, agent: AgentId) -> Option<Caning> {
        self.canings.get(&agent).copied()
    }

    /// 现降几级留用；未受杖、已宽宥或已戴罪立功的为 0。
    pub fn demotion(&self, agent: AgentId) -> u32 {
        match self.canings.get(&agent) {
            Some(c) if !self.redeemed(agent) => c.demoted,
            _ => 0,
        }
    }

    /// 受杖后已获采纳的件数，与戴罪立功尚差几件（不在降级中为 `None`）。
    pub fn redemption_left(&self, agent: AgentId) -> Option<u32> {
        let c = self.canings.get(&agent).filter(|c| c.demoted > 0)?;
        let done = self
            .service_record(agent)
            .accepted
            .saturating_sub(c.accepted_at);
        Caning::REDEMPTION
            .checked_sub(done)
            .filter(|&left| left > 0)
    }

    fn redeemed(&self, agent: AgentId) -> bool {
        self.canings.get(&agent).is_none_or(|c| c.demoted == 0)
            || self.redemption_left(agent).is_none()
    }

    /// 翰林院存模板：名为 1–20 字、不含空白；内容非空；玉玺合乎部门（写权限唯工部）；
    /// 验收标准 1–8 条，各条单行、至多 200 字。
    pub fn propose_template(
        &self,
        name: String,
        template: Template,
    ) -> Result<CourtEvent, GovernanceError> {
        let bad = |why: &str| Err(GovernanceError::Template(why.into()));
        let count = name.chars().count();
        if count == 0 || count > 20 || name.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return bad("模板名须为 1–20 字，不含空白");
        }
        if template.text.trim().is_empty() {
            return bad("模板内容不能为空");
        }
        validate_edict_seal(template.department, &template.seal)?;
        check_criteria(&template.criteria)?;
        Ok(CourtEvent::TemplateSaved { name, template })
    }

    /// 撤去自定的模板；内置模板不能撤去。
    pub fn propose_template_removal(&self, name: String) -> Result<CourtEvent, GovernanceError> {
        if !self.templates.contains_key(&name) {
            let why = if builtin_templates().iter().any(|(n, _)| *n == name) {
                format!("「{name}」是内置模板，只能覆盖，不能撤去")
            } else {
                format!("没有自定的模板「{name}」")
            };
            return Err(GovernanceError::Template(why));
        }
        Ok(CourtEvent::TemplateRemoved { name })
    }

    /// 按名取模板：自定的优先，其次内置。
    pub fn template(&self, name: &str) -> Option<Template> {
        self.templates.get(name).cloned().or_else(|| {
            builtin_templates()
                .into_iter()
                .find(|(n, _)| *n == name)
                .map(|(_, t)| t)
        })
    }

    /// 全部模板（名、模板、是否内置原样）：内置的在前（自定的覆盖同名内置的），其余自定的按名排序。
    pub fn templates(&self) -> Vec<(String, Template, bool)> {
        let builtin = builtin_templates();
        let mut all: Vec<_> = builtin
            .iter()
            .map(|(name, t)| match self.templates.get(*name) {
                Some(custom) => ((*name).to_owned(), custom.clone(), false),
                None => ((*name).to_owned(), t.clone(), true),
            })
            .collect();
        all.extend(
            self.templates
                .iter()
                .filter(|(name, _)| !builtin.iter().any(|(b, _)| b == name))
                .map(|(name, t)| (name.clone(), t.clone(), false)),
        );
        all
    }

    /// 诏令按模板下达：诏令须待办、尚未用过模板；验收标准取当前模板的。
    pub fn propose_template_use(
        &self,
        task: TaskId,
        name: &str,
    ) -> Result<CourtEvent, GovernanceError> {
        self.check_template_use(task)?;
        let template = self
            .template(name)
            .ok_or_else(|| GovernanceError::Template(format!("没有模板「{name}」")))?;
        Ok(CourtEvent::TemplateApplied {
            task,
            name: name.to_owned(),
            criteria: template.criteria,
        })
    }

    fn check_template_use(&self, task: TaskId) -> Result<(), GovernanceError> {
        self.open_task(task)?;
        let entry = &self.tasks[&task];
        if entry.status != TaskStatus::Pending {
            return Err(GovernanceError::Template(format!(
                "诏令 #{} 已开办",
                task.0
            )));
        }
        if entry.template.is_some() {
            return Err(GovernanceError::Template(format!(
                "诏令 #{} 已按模板下达",
                task.0
            )));
        }
        Ok(())
    }

    /// 驿站回报可否：诏令经驿传入队、尚未回报过同类。回奏评论须已完成、经皇帝评定（含代批），
    /// 且没有未平反的密报；草稿 PR 须改动已准奏落盘。`url` 须为单行。
    pub fn propose_courier_reply(
        &self,
        task: TaskId,
        pull_request: bool,
        url: &str,
    ) -> Result<(), GovernanceError> {
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        let refuse = |why: String| Err(GovernanceError::Military(why));
        if entry.courier.is_none() {
            return refuse(format!("诏令 #{} 不是经驿传入队的，无处回报", task.0));
        }
        if url.trim().is_empty() || url.chars().any(char::is_control) {
            return refuse("回报网址须为单行文字".into());
        }
        if pull_request {
            if entry.pull_request.is_some() {
                return refuse(format!("诏令 #{} 已开过草稿 PR", task.0));
            }
            if entry.changes.as_ref().map(|c| c.state) != Some(ChangeState::Applied) {
                return refuse(format!("诏令 #{} 没有已准奏的改动", task.0));
            }
        } else {
            if entry.replied.is_some() {
                return refuse(format!("诏令 #{} 已回报过", task.0));
            }
            if !matches!(entry.status, TaskStatus::Completed { .. }) {
                return refuse(format!("诏令 #{} 尚未办完，无可回报", task.0));
            }
            if entry.review.is_none() {
                return refuse(format!("诏令 #{} 尚未评定，评定后才送回来处", task.0));
            }
            if entry.standing_suspicions().next().is_some() {
                return refuse(format!(
                    "诏令 #{} 有未平反的密报，不送出；复勘平反后再回报",
                    task.0
                ));
            }
        }
        Ok(())
    }

    /// 驿传入队：诏令须待办且尚未记来处；来处须非空、不含控制字符、至多 80 字。
    pub fn propose_courier(
        &self,
        task: TaskId,
        station: String,
    ) -> Result<CourtEvent, GovernanceError> {
        self.open_task(task)?;
        let station = station.trim().to_owned();
        if station.is_empty()
            || station.chars().count() > 80
            || station.chars().any(char::is_control)
        {
            return Err(GovernanceError::Military(
                "驿站名须为 1–80 字的单行文字".into(),
            ));
        }
        if self.tasks[&task].courier.is_some() {
            return Err(GovernanceError::Military(format!(
                "诏令 #{} 已记驿传来处",
                task.0
            )));
        }
        Ok(CourtEvent::CourierDelivered { task, station })
    }

    /// 八百里加急：诏令须未结束且尚未加急。
    pub fn propose_expedite(&self, task: TaskId) -> Result<CourtEvent, GovernanceError> {
        self.open_task(task)?;
        if self.tasks[&task].urgent {
            return Err(GovernanceError::Military(format!(
                "诏令 #{} 已是八百里加急",
                task.0
            )));
        }
        Ok(CourtEvent::EdictExpedited { task })
    }

    /// 钦点状元：须在最近一次殿试的答卷者中，且该次尚未点过。
    pub fn propose_zhuangyuan(&self, agent: AgentId) -> Result<CourtEvent, GovernanceError> {
        let last = self
            .palace_exams
            .last()
            .ok_or_else(|| GovernanceError::InvalidExam("尚未举行殿试".into()))?;
        if last.zhuangyuan.is_some() {
            return Err(GovernanceError::InvalidExam("本科状元已点".into()));
        }
        if !last.answers.iter().any(|a| a.speaker == agent) {
            let why = format!("官员 #{} 未参加本科殿试", agent.0);
            return Err(GovernanceError::InvalidExam(why));
        }
        Ok(CourtEvent::ZhuangyuanNamed { agent })
    }

    pub fn exams(&self) -> &[ExamRecord] {
        &self.exams
    }

    pub fn palace_exams(&self) -> &[PalaceExam] {
        &self.palace_exams
    }

    /// 默认的廷议人选（至多四位）：协调者（丞相或首辅）、本部长官、左都御史，不足再由本部官员补上。
    pub fn debate_candidates(&self, department: Department) -> Vec<(&Agent, Office)> {
        let mut chosen: Vec<(&Agent, Office)> = Vec::new();
        let censor = self.censor().map(|a| (a, Office::Censor));
        let staff = self.draft_candidates(department);
        let picks = [self.coordinator(), self.department_head(department), censor];
        for pick in picks.into_iter().flatten().chain(staff) {
            if chosen.len() < 4 && chosen.iter().all(|(a, _)| a.id != pick.0.id) {
                chosen.push(pick);
            }
        }
        chosen
    }

    /// 一键朱元璋：罢中书省，株连百官——丞相与全部在任官员革职削籍，改为废相亲政。
    /// 未任官职者（白身）不受牵连。
    pub fn propose_abolition(&self) -> Result<Vec<CourtEvent>, GovernanceError> {
        if self.mode != GovernanceMode::Chancellor {
            return Err(GovernanceError::NoChancellery);
        }
        let mut events: Vec<_> = self
            .appointments
            .iter()
            .map(|(&agent, _)| CourtEvent::OfficialPurged { agent })
            .collect();
        events.push(CourtEvent::InstitutionReformed(GovernanceMode::DirectRule));
        Ok(events)
    }

    fn open_task(&self, id: TaskId) -> Result<&TaskStatus, GovernanceError> {
        let task = self
            .tasks
            .get(&id)
            .ok_or(GovernanceError::UnknownEdict(id))?;
        if !task.status.is_open() {
            return Err(GovernanceError::TaskFinished(id));
        }
        Ok(&task.status)
    }

    fn set_status(&mut self, id: TaskId, status: TaskStatus) {
        if let Some(task) = self.tasks.get_mut(&id) {
            task.status = status;
        }
    }

    /// 皇帝评定一件已完成的诏令；每件只能评定一次。
    pub fn propose_review(
        &self,
        task: TaskId,
        verdict: Verdict,
        note: String,
    ) -> Result<CourtEvent, GovernanceError> {
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        if !matches!(entry.status, TaskStatus::Completed { .. }) {
            return Err(GovernanceError::NotCompleted(task));
        }
        if entry.review.is_some() {
            return Err(GovernanceError::AlreadyReviewed(task));
        }
        Ok(CourtEvent::TaskReviewed {
            task,
            verdict,
            note,
        })
    }

    /// 皇帝处置已完成诏令呈报的改动：`apply` 为真是落盘（准奏），否则驳回。
    /// 实际写入文件由调用方在提交事件前完成。
    pub fn propose_changes_decision(
        &self,
        task: TaskId,
        apply: bool,
    ) -> Result<CourtEvent, GovernanceError> {
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        if !matches!(entry.status, TaskStatus::Completed { .. }) {
            return Err(GovernanceError::NotCompleted(task));
        }
        match &entry.changes {
            Some(c) if c.state == ChangeState::Proposed => Ok(if apply {
                CourtEvent::ChangesApplied {
                    task,
                    skipped: Vec::new(),
                }
            } else {
                CourtEvent::ChangesDiscarded { task }
            }),
            _ => Err(GovernanceError::NoPendingChanges(task)),
        }
    }

    /// 逐段准奏：驳回 `skipped` 中的段（从 1 起，跨文件连续编号），其余写入项目。
    /// 段号须在范围内；不能全部驳回（那是驳回改动）。
    pub fn propose_application(
        &self,
        task: TaskId,
        mut skipped: Vec<u32>,
    ) -> Result<CourtEvent, GovernanceError> {
        self.propose_changes_decision(task, true)?;
        let files = &self.tasks[&task]
            .changes
            .as_ref()
            .expect("已确认有待决改动")
            .files;
        let total: usize = files.iter().map(|f| f.hunks().len()).sum();
        skipped.sort_unstable();
        skipped.dedup();
        let bad = |why: String| Err(GovernanceError::Changes(why));
        if let Some(n) = skipped.iter().find(|&&n| n == 0 || n as usize > total) {
            return bad(format!("没有第 {n} 段：改动共 {total} 段"));
        }
        if !skipped.is_empty() && skipped.len() == total {
            return bad("各段都驳回了，请直接驳回改动".into());
        }
        Ok(CourtEvent::ChangesApplied { task, skipped })
    }

    /// 诏令的第 `index` 处检出（从 0 起）。
    fn suspicion(&self, task: TaskId, index: u32) -> Result<(&Task, &Suspicion), GovernanceError> {
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        let found = entry.suspicions.get(index as usize).ok_or_else(|| {
            GovernanceError::Suspicion(format!(
                "诏令 #{} 没有第 {} 处检出（共 {} 处）",
                task.0,
                index + 1,
                entry.suspicions.len()
            ))
        })?;
        Ok((entry, found))
    }

    /// 刑部复勘可否：检出存在且未复勘、未平反，`reviewer` 现任刑部官职且未经手此诏令。
    pub fn propose_recheck(
        &self,
        task: TaskId,
        index: u32,
        reviewer: AgentId,
    ) -> Result<(), GovernanceError> {
        let (entry, found) = self.suspicion(task, index)?;
        if found.review.is_some() || found.acquitted {
            return Err(GovernanceError::Suspicion(format!(
                "诏令 #{} 第 {} 处检出已复勘或已平反",
                task.0,
                index + 1
            )));
        }
        let justice = matches!(
            self.appointments.get(&reviewer),
            Some(
                Office::Minister(Department::Justice)
                    | Office::ViceMinister(Department::Justice)
                    | Office::Clerk(Department::Justice)
            )
        );
        if !justice {
            return Err(GovernanceError::Suspicion(format!(
                "官员 #{} 不是现任刑部官员",
                reviewer.0
            )));
        }
        if entry.handler() == Some(reviewer) {
            return Err(GovernanceError::SelfReview(reviewer));
        }
        Ok(())
    }

    /// 复勘此诏令检出的刑部官员：尚书（空缺则侍郎），回避经手者时取刑部郎中。
    pub fn recheck_official(&self, task: TaskId) -> Option<&Agent> {
        let handler = self.tasks.get(&task).and_then(Task::handler);
        let justice = [
            Office::Minister(Department::Justice),
            Office::ViceMinister(Department::Justice),
            Office::Clerk(Department::Justice),
        ];
        justice.into_iter().find_map(|office| {
            self.appointments
                .iter()
                .find(|(id, o)| **o == office && Some(**id) != handler)
                .and_then(|(id, _)| self.agents.get(id))
        })
    }

    /// 平反可否：检出存在且未平反，皇帝在朝。
    pub fn propose_acquittal(&self, task: TaskId, index: u32) -> Result<(), GovernanceError> {
        if self.retreat {
            return Err(GovernanceError::InRetreat);
        }
        let (_, found) = self.suspicion(task, index)?;
        if found.acquitted {
            return Err(GovernanceError::Suspicion(format!(
                "诏令 #{} 第 {} 处检出已平反",
                task.0,
                index + 1
            )));
        }
        Ok(())
    }

    /// 现任左都御史。
    pub fn censor(&self) -> Option<&Agent> {
        self.appointed_as(Office::Censor)
    }

    /// 都察院可否由 `censor` 复核 `task`：诏令已完成且未复核，`censor` 为现任左都御史且未经手此诏令。
    pub fn propose_censure_check(
        &self,
        task: TaskId,
        censor: AgentId,
    ) -> Result<(), GovernanceError> {
        let entry = self
            .tasks
            .get(&task)
            .ok_or(GovernanceError::UnknownEdict(task))?;
        if !matches!(entry.status, TaskStatus::Completed { .. }) {
            return Err(GovernanceError::NotCompleted(task));
        }
        if entry.censure.is_some() {
            return Err(GovernanceError::AlreadyCensured(task));
        }
        if self.appointments.get(&censor) != Some(&Office::Censor) {
            return Err(GovernanceError::NotCensor(censor));
        }
        if entry.handler() == Some(censor) {
            return Err(GovernanceError::SelfReview(censor));
        }
        Ok(())
    }

    pub fn propose_cancellation(&self, task: TaskId) -> Result<CourtEvent, GovernanceError> {
        self.open_task(task)?;
        Ok(CourtEvent::TaskCancelled { task })
    }

    /// 招募新官员，登记身份但不自动授职。
    pub fn propose_recruitment(
        &self,
        name: String,
        speciality: Department,
    ) -> Result<CourtEvent, GovernanceError> {
        if name.trim().is_empty() {
            return Err(GovernanceError::EmptyName);
        }
        Ok(CourtEvent::AgentRegistered(Agent {
            id: AgentId(self.next_agent_id),
            name,
            speciality,
        }))
    }

    /// 皇帝的任命：当前 MVP 只有 CLI 皇帝可以调用此入口。
    pub fn propose_appointment(
        &self,
        agent: AgentId,
        office: Office,
    ) -> Result<CourtEvent, GovernanceError> {
        let person = self
            .agents
            .get(&agent)
            .ok_or(GovernanceError::UnknownAgent(agent))?;
        if self.purged.contains(&agent) {
            return Err(GovernanceError::Purged(agent));
        }
        if self.appointments.contains_key(&agent) {
            return Err(GovernanceError::AlreadyAppointed(agent));
        }
        if !office.exists_in(self.mode) {
            return Err(GovernanceError::OfficeNotInMode(office));
        }
        if let Some(d) = department_of(office)
            && d != person.speciality
        {
            return Err(GovernanceError::SpecialityMismatch {
                name: person.name.clone(),
                department: d,
            });
        }
        if office.is_unique() && self.appointments.values().any(|o| *o == office) {
            return Err(GovernanceError::OfficeOccupied(office));
        }
        Ok(CourtEvent::OfficialAppointed { agent, office })
    }

    pub fn propose_dismissal(&self, agent: AgentId) -> Result<CourtEvent, GovernanceError> {
        if !self.appointments.contains_key(&agent) {
            return Err(GovernanceError::NotAppointed(agent));
        }
        Ok(CourtEvent::OfficialDismissed { agent })
    }

    pub fn propose_reform(&self, mode: GovernanceMode) -> Result<CourtEvent, GovernanceError> {
        if self.mode == mode {
            return Err(GovernanceError::SameMode(mode));
        }
        Ok(CourtEvent::InstitutionReformed(mode))
    }

    pub fn new_edict(
        &self,
        department: Department,
        text: String,
        seal: Seal,
    ) -> Result<Edict, GovernanceError> {
        if text.trim().is_empty() {
            return Err(GovernanceError::EmptyEdict);
        }
        validate_edict_seal(department, &seal)?;
        Ok(Edict {
            id: TaskId(self.next_task_id),
            department,
            text,
            seal,
        })
    }

    /// 吏部考绩：完全由事件簿计算的履历。质量只采信皇帝的评定。
    pub fn service_record(&self, agent: AgentId) -> ServiceRecord {
        let mut record = ServiceRecord::default();
        for task in self.tasks.values() {
            if task.handler() != Some(agent) {
                continue;
            }
            record.tool_calls += task.tool_calls;
            record.usage += task.usage;
            record.tool_failures += task.tool_failures;
            match (&task.status, &task.review) {
                (TaskStatus::Completed { .. }, Some((Verdict::Accepted, _))) => {
                    record.accepted += 1
                }
                (TaskStatus::Completed { .. }, Some((Verdict::Rejected, _))) => {
                    record.rejected += 1
                }
                (TaskStatus::Completed { .. }, None) => record.unreviewed += 1,
                (TaskStatus::Failed { .. }, _) => record.failed += 1,
                _ => {}
            }
            if let Some(censure) = &task.censure {
                match censure.verdict {
                    CensorVerdict::Endorsed => record.endorsed += 1,
                    CensorVerdict::Impeached => record.impeached += 1,
                }
                record.verified += u32::from(censure.evidence.verified());
            }
        }
        for exam in self.exams.iter().filter(|e| e.agent == agent) {
            record.exams += 1;
            record.exams_passed += u32::from(exam.passed());
        }
        let crowned = self
            .palace_exams
            .iter()
            .filter(|p| p.zhuangyuan == Some(agent));
        record.zhuangyuan = crowned.count() as u32;
        record.nominated = self.nomination_votes.get(&agent).copied().unwrap_or(0);
        record.caned = self.canings.get(&agent).map_or(0, |c| c.times);
        record
    }

    /// 吏部荐举：列出可任 `office` 的官员（专长与制度均符合），按已评定的履历排序。
    /// 仅供参考，不产生事件；任命仍须皇帝 `appoint`。
    pub fn recommend(&self, office: Office) -> Result<Vec<Candidate<'_>>, GovernanceError> {
        if !office.exists_in(self.mode) {
            return Err(GovernanceError::OfficeNotInMode(office));
        }
        let mut candidates: Vec<_> = self
            .agents
            .values()
            .filter(|agent| !self.purged.contains(&agent.id))
            .filter(|agent| department_of(office).is_none_or(|d| agent.speciality == d))
            .map(|agent| Candidate {
                agent,
                current: self.office_of(agent.id),
                record: self.service_record(agent.id),
            })
            .collect();
        candidates.sort_by_key(|c| {
            (
                std::cmp::Reverse(c.record.merit()),
                c.record.failed,
                c.record.tool_failures,
                c.agent.id,
            )
        });
        Ok(candidates)
    }

    fn appointed_as(&self, office: Office) -> Option<&Agent> {
        self.appointments.iter().find_map(|(id, o)| {
            if *o == office {
                self.agents.get(id)
            } else {
                None
            }
        })
    }

    /// 正职在任则为正职，否则由副职署理；两者俱缺时为 `None`。返回 (官员, 实际官职)。
    fn head_or_deputy(&self, head: Office, deputy: Office) -> Option<(&Agent, Office)> {
        self.appointed_as(head)
            .map(|a| (a, head))
            .or_else(|| self.appointed_as(deputy).map(|a| (a, deputy)))
    }

    /// 路由说明是可观察性日志，不是给 Worker 的权限凭证。
    ///
    /// 皇帝 →（中书省或内阁）→ 本部尚书 → 郎中。正职空缺时由副职署理，在路线中注明。
    pub fn route(&self, edict: &Edict) -> Result<Route, GovernanceError> {
        self.route_to(edict, Pick::Default)
    }

    /// 诏令的派工路线：子诏令派给分办时指定的官员；已分办的原诏令由尚书汇总；内阁制下若已
    /// 票拟，派给拟派者。指定者已不在本部任职时按旧例。
    pub fn route_task(&self, task: &Task) -> Result<Route, GovernanceError> {
        let pick = if let Some(worker) = task.assignee {
            Pick::Named(worker, "（分办）")
        } else if task.split.is_some() {
            Pick::Head
        } else if let Some(draft) = task
            .draft
            .as_ref()
            .filter(|_| self.mode == GovernanceMode::Cabinet)
        {
            Pick::Named(draft.worker, "（票拟）")
        } else {
            Pick::Default
        };
        self.route_to(&task.edict, pick)
    }

    fn route_to(&self, edict: &Edict, pick: Pick) -> Result<Route, GovernanceError> {
        let mut path = vec!["皇帝".to_owned()];
        let step = |(agent, office): (&Agent, Office), head: Office| {
            let acting = if office == head { "" } else { "（署理）" };
            format!("{}·{}{acting}", office.label(), agent.name)
        };
        let coordinator = match self.mode {
            GovernanceMode::Chancellor => Some((Office::Chancellor, Office::ViceChancellor)),
            GovernanceMode::Cabinet => {
                Some((Office::FirstGrandSecretary, Office::SecondGrandSecretary))
            }
            GovernanceMode::DirectRule => None,
        };
        if let Some((head, deputy)) = coordinator {
            let found = self
                .head_or_deputy(head, deputy)
                .ok_or(GovernanceError::Vacant(head))?;
            path.push(step(found, head));
        }
        let department = edict.department;
        let head = Office::Minister(department);
        let found = self
            .head_or_deputy(head, Office::ViceMinister(department))
            .ok_or(GovernanceError::Vacant(head))?;
        path.push(step(found, head));
        let minister = found.0;
        let staff = self.draft_candidates(department);
        if matches!(pick, Pick::Head) {
            path.last_mut()
                .expect("至少有尚书一步")
                .push_str("（汇总）");
            return Ok(Route {
                path,
                worker: minister.id,
            });
        }
        let (drafted, mark) = match pick {
            Pick::Named(id, mark) => (staff.iter().find(|(a, _)| a.id == id), mark),
            _ => (None, ""),
        };
        // 未票拟时按任务量均衡派给郎中；本部无郎中则由尚书（或署理的侍郎）亲办。
        let (worker, office) = drafted.copied().unwrap_or_else(|| {
            staff
                .iter()
                .filter(|(_, office)| *office == Office::Clerk(department))
                .min_by_key(|(candidate, _)| (self.workload(candidate.id), candidate.id))
                .copied()
                .unwrap_or(found)
        });
        let mark = if drafted.is_some() { mark } else { "" };
        if worker.id != minister.id {
            path.push(format!("{}·{}{mark}", office.label(), worker.name));
        } else if drafted.is_some() {
            let last = path.last_mut().expect("至少有尚书一步");
            last.push_str(mark);
        }
        Ok(Route {
            path,
            worker: worker.id,
        })
    }

    pub fn recorded_completions(&self, agent: AgentId) -> usize {
        self.tasks
            .values()
            .filter(|t| matches!(t.status, TaskStatus::Completed { worker, .. } if worker == agent))
            .count()
    }

    /// 派工负载：已完成与执行中的任务数。并发派工时，先派出的任务也计入。
    fn workload(&self, agent: AgentId) -> usize {
        self.tasks
            .values()
            .filter(|t| match t.status {
                TaskStatus::Assigned(worker) | TaskStatus::Completed { worker, .. } => {
                    worker == agent
                }
                _ => false,
            })
            .count()
    }
}

/// 查阅范围只能是项目内的相对路径。路径是否存在、是否经符号链接逃逸，由运行时在构造工具时检查。
/// 诏令的玉玺：在 [`validate_seal`] 之外，写文件与跑测试只能授予工部。
/// （官职上限不受此限：上限只会收窄诏令的玉玺。）
fn check_criteria(criteria: &[String]) -> Result<(), GovernanceError> {
    let bad = |why: &str| Err(GovernanceError::Template(why.into()));
    if criteria.is_empty() || criteria.len() > 8 {
        return bad("验收标准须为 1–8 条");
    }
    if criteria
        .iter()
        .any(|c| c.trim().is_empty() || c.chars().count() > 200 || c.contains(['\n', '\r']))
    {
        return bad("每条验收标准须为单行、1–200 字");
    }
    Ok(())
}

fn validate_edict_seal(department: Department, seal: &Seal) -> Result<(), GovernanceError> {
    // 礼部修 CHANGELOG：可授 write_file，但范围只限 CHANGELOG.md 一个文件，且不能跑测试。
    let changelog = department == Department::Rites && seal.scope == [CHANGELOG];
    if department != Department::Works
        && let Some(&c) = seal
            .capabilities
            .iter()
            .find(|c| c.is_works_only() && !(changelog && **c == Capability::WriteFile))
    {
        return Err(GovernanceError::WorksOnly(c));
    }
    validate_seal(seal)
}

fn validate_seal(seal: &Seal) -> Result<(), GovernanceError> {
    if seal.scope.is_empty() {
        return Err(GovernanceError::InvalidScope(String::new()));
    }
    if let Some(bad) = seal.mcp.iter().find(|m| !ziyuan_core::valid_mcp_grant(m)) {
        return Err(GovernanceError::Court(format!(
            "外藩授权不合法：{bad}（须为 *、服务 或 服务/工具，由字母、数字、-、_、. 组成）"
        )));
    }
    for path in &seal.scope {
        let escapes = Path::new(path)
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir));
        if path.trim().is_empty() || escapes {
            return Err(GovernanceError::InvalidScope(path.clone()));
        }
    }
    Ok(())
}

/// 一位官员经手诏令的统计。`unreviewed` 不计入评价：未经皇帝评定的回奏不算政绩。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServiceRecord {
    pub accepted: u32,
    pub rejected: u32,
    pub unreviewed: u32,
    /// 承办时失败（含超时）。
    pub failed: u32,
    pub tool_calls: u32,
    /// 失败的工具调用，含越出玉玺的尝试。
    pub tool_failures: u32,
    /// 经手诏令（含失败）的模型 token 用量。
    pub usage: TokenUsage,
    /// 都察院背书、弹劾的件数。
    pub endorsed: u32,
    pub impeached: u32,
    /// 经核验有据可查（引用属实或测试通过）的件数。
    pub verified: u32,
    /// 科举应试次数、及格次数，与殿试钦点状元的次数。
    pub exams: u32,
    pub exams_passed: u32,
    pub zhuangyuan: u32,
    /// 历次廷推所得的票数，与受廷杖的次数（不计入荐举排序）。
    pub nominated: u32,
    pub caned: u32,
}

impl ServiceRecord {
    /// 采纳数减驳回数。
    pub fn net_accepted(&self) -> i64 {
        i64::from(self.accepted) - i64::from(self.rejected)
    }

    /// 荐举排序的依据：皇帝评定（采纳 − 驳回）加都察院复核（背书 − 弹劾）、实证件数、
    /// 科举及格次数与状元次数。
    pub fn merit(&self) -> i64 {
        self.net_accepted() + i64::from(self.endorsed) - i64::from(self.impeached)
            + i64::from(self.verified)
            + i64::from(self.exams_passed)
            + i64::from(self.zhuangyuan)
    }
}

/// 廷推的选票：九卿（及其官职）与候选人。
pub type NominationBallot<'a> = (Vec<(&'a Agent, Office)>, Vec<Candidate<'a>>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate<'a> {
    pub agent: &'a Agent,
    /// 现任官职；荐举不会自动罢免或调任。
    pub current: Option<Office>,
    pub record: ServiceRecord,
}

/// 派工时如何选承办者。
#[derive(Clone, Copy)]
enum Pick {
    /// 旧例：任务量均衡派给郎中。
    Default,
    /// 指定官员（票拟或分办），路线上附注标记。
    Named(AgentId, &'static str),
    /// 尚书（或署理的侍郎）亲办：汇总分办的回奏。
    Head,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub path: Vec<String>,
    pub worker: AgentId,
}

/// 六部官职所属的部门。
fn department_of(office: Office) -> Option<Department> {
    match office {
        Office::Minister(d) | Office::ViceMinister(d) | Office::Clerk(d) => Some(d),
        _ => None,
    }
}

/// 最小开局（丞相制）：只有工部与吏部有人，其余四部留空待扩展。官员姓名由 `seed` 随机生成。
pub fn founding_events(seed: u64) -> Vec<CourtEvent> {
    use CourtEvent::*;
    use Department::*;
    let mut generator = names::NameGenerator::new(seed);
    let mut taken: Vec<String> = Vec::new();
    let recruits = [Personnel, Works, Personnel, Works, Personnel, Personnel]
        .into_iter()
        .zip(1..)
        .map(|(speciality, id)| {
            let name = generator.name(|n| taken.iter().any(|t| t == n));
            taken.push(name.clone());
            (id, name, speciality)
        })
        .collect::<Vec<_>>();
    let mut events = vec![Founded(GovernanceMode::Chancellor)];
    for (id, name, speciality) in recruits {
        events.push(AgentRegistered(Agent {
            id: AgentId(id),
            name,
            speciality,
        }));
    }
    events.extend([
        OfficialAppointed {
            agent: AgentId(1),
            office: Office::Chancellor,
        },
        OfficialAppointed {
            agent: AgentId(2),
            office: Office::Minister(Works),
        },
        OfficialAppointed {
            agent: AgentId(3),
            office: Office::Minister(Personnel),
        },
        OfficialAppointed {
            agent: AgentId(4),
            office: Office::Clerk(Works),
        },
        OfficialAppointed {
            agent: AgentId(5),
            office: Office::Clerk(Personnel),
        },
        OfficialAppointed {
            agent: AgentId(6),
            office: Office::Clerk(Personnel),
        },
    ]);
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reform_changes_route() {
        let mut court = Court::replay(&founding_events(1)).unwrap();
        let edict = court
            .new_edict(Department::Works, "检查源码".into(), Seal::default())
            .unwrap();
        assert!(
            court
                .route(&edict)
                .unwrap()
                .path
                .iter()
                .any(|p| p.contains("丞相"))
        );
        court
            .apply(&court.propose_reform(GovernanceMode::DirectRule).unwrap())
            .unwrap();
        assert!(
            !court
                .route(&edict)
                .unwrap()
                .path
                .iter()
                .any(|p| p.contains("丞相"))
        );
        court
            .apply(&court.propose_reform(GovernanceMode::Cabinet).unwrap())
            .unwrap();
        assert!(court.route(&edict).is_err());
        court
            .apply(&court.propose_dismissal(AgentId(6)).unwrap())
            .unwrap();
        court
            .apply(
                &court
                    .propose_appointment(AgentId(6), Office::FirstGrandSecretary)
                    .unwrap(),
            )
            .unwrap();
        assert!(
            court
                .route(&edict)
                .unwrap()
                .path
                .iter()
                .any(|p| p.contains("首辅"))
        );
    }
    #[test]
    fn deputies_act_for_vacant_heads_and_ranks_follow_the_mode() {
        let mut court = Court::replay(&founding_events(1)).unwrap();
        let works = |court: &Court| {
            let edict = court
                .new_edict(Department::Works, "查".into(), Seal::default())
                .unwrap();
            court.route(&edict).map(|r| r.path)
        };
        let apply =
            |court: &mut Court, propose: &dyn Fn(&Court) -> Result<CourtEvent, GovernanceError>| {
                let event = propose(court).unwrap();
                court.apply(&event).unwrap()
            };
        // 丞相制：右丞相（#1）罢免后，左丞相署理；尚书（#2）罢免后，侍郎署理。
        apply(&mut court, &|c| c.propose_dismissal(AgentId(1)));
        assert_eq!(
            works(&court).unwrap_err(),
            GovernanceError::Vacant(Office::Chancellor)
        );
        apply(&mut court, &|c| {
            c.propose_appointment(AgentId(1), Office::ViceChancellor)
        });
        apply(&mut court, &|c| c.propose_dismissal(AgentId(2)));
        assert_eq!(
            works(&court).unwrap_err(),
            GovernanceError::Vacant(Office::Minister(Department::Works))
        );
        apply(&mut court, &|c| {
            c.propose_appointment(AgentId(2), Office::ViceMinister(Department::Works))
        });
        let path = works(&court).unwrap();
        let names: Vec<String> = court.agents().map(|a| a.name.clone()).collect();
        let name = |id: usize| names[id - 1].clone();
        assert_eq!(
            path,
            [
                "皇帝".to_owned(),
                format!("中书左丞相·{}（署理）", name(1)),
                format!("工部侍郎·{}（署理）", name(2)),
                format!("工部郎中·{}", name(4)),
            ]
        );
        let minister = Office::Minister(Department::Works);
        assert_eq!(minister.rank(court.mode()), "正三品");

        // 废相亲政：中书省官职随之撤销，六部官职保留并升为明制品级；内阁官职不可任命。
        apply(&mut court, &|c| {
            c.propose_reform(GovernanceMode::DirectRule)
        });
        assert_eq!(court.office_of(AgentId(1)), None);
        assert_eq!(
            court.office_of(AgentId(2)),
            Some(Office::ViceMinister(Department::Works))
        );
        assert_eq!(minister.rank(court.mode()), "正二品");
        assert_eq!(works(&court).unwrap().len(), 3);
        assert_eq!(
            court.propose_appointment(AgentId(1), Office::SecondGrandSecretary),
            Err(GovernanceError::OfficeNotInMode(
                Office::SecondGrandSecretary
            ))
        );
        // 内阁制：次辅在首辅空缺时署理。
        apply(&mut court, &|c| c.propose_reform(GovernanceMode::Cabinet));
        apply(&mut court, &|c| {
            c.propose_appointment(AgentId(1), Office::SecondGrandSecretary)
        });
        assert_eq!(
            works(&court).unwrap()[1],
            format!("内阁次辅·{}（署理）", name(1))
        );
    }
    #[test]
    fn abolition_purges_chancellors_and_cabinet_edicts_need_approval() {
        let mut court = Court::replay(&founding_events(1)).unwrap();
        let apply = |events: Vec<CourtEvent>, court: &mut Court| {
            for e in events {
                court.apply(&e).unwrap();
            }
        };
        // 革去右丞相 #1 并废相；被革者不能再任官，也不入荐举（一键朱元璋见本测试末尾）。
        apply(
            vec![
                court.propose_purge(AgentId(1)).unwrap(),
                court.propose_reform(GovernanceMode::DirectRule).unwrap(),
            ],
            &mut court,
        );
        assert!(court.is_purged(AgentId(1)) && court.office_of(AgentId(1)).is_none());
        assert!(!court.is_purged(AgentId(2)));
        assert_eq!(
            court.propose_abolition(),
            Err(GovernanceError::NoChancellery)
        );
        apply(
            vec![court.propose_reform(GovernanceMode::Cabinet).unwrap()],
            &mut court,
        );
        assert_eq!(
            court.propose_appointment(AgentId(1), Office::FirstGrandSecretary),
            Err(GovernanceError::Purged(AgentId(1)))
        );
        assert!(
            court
                .recommend(Office::FirstGrandSecretary)
                .unwrap()
                .iter()
                .all(|c| c.agent.id != AgentId(1))
        );
        assert_eq!(
            court.propose_purge(AgentId(1)),
            Err(GovernanceError::Purged(AgentId(1)))
        );

        // 内阁制：未批红的诏令不可执行；批红一次后可执行，不能重复批红。
        apply(
            vec![court.propose_dismissal(AgentId(6)).unwrap()],
            &mut court,
        );
        apply(
            vec![
                court
                    .propose_appointment(AgentId(6), Office::FirstGrandSecretary)
                    .unwrap(),
            ],
            &mut court,
        );
        let edict = court
            .new_edict(Department::Works, "查".into(), Seal::default())
            .unwrap();
        let id = edict.id;
        apply(vec![CourtEvent::EdictIssued(edict)], &mut court);
        assert!(court.runnable_tasks().is_empty());
        assert_eq!(court.open_tasks(), [id]);
        // 票拟只能拟派本部在任官员；拟派尚书后，派工改给尚书（路线注明票拟），不再按旧例给郎中。
        let none = TokenUsage::default();
        assert_eq!(
            court.propose_draft(id, AgentId(3), String::new(), None, none),
            Err(GovernanceError::NotEligible {
                worker: AgentId(3),
                department: Department::Works
            })
        );
        assert_eq!(
            court.route(&court.task(id).unwrap().edict).unwrap().worker,
            AgentId(4)
        );
        let used = TokenUsage {
            input: 100,
            output: 10,
        };
        apply(
            vec![
                court
                    .propose_draft(id, AgentId(2), "尚书亲办".into(), Some("pro".into()), used)
                    .unwrap(),
            ],
            &mut court,
        );
        let route = court.route_task(court.task(id).unwrap()).unwrap();
        assert_eq!(route.worker, AgentId(2));
        assert!(
            route.path.last().unwrap().ends_with("（票拟）"),
            "{:?}",
            route.path
        );
        let task = court.task(id).unwrap();
        assert_eq!(
            (task.usage, task.draft.as_ref().unwrap().usage),
            (used, used)
        );
        apply(vec![court.propose_approval(id).unwrap()], &mut court);
        assert_eq!(court.runnable_tasks(), [id]);
        assert_eq!(
            court.propose_approval(id),
            Err(GovernanceError::AlreadyApproved(id))
        );
        // 其他制度不批红，诏令直接可执行。
        apply(
            vec![court.propose_reform(GovernanceMode::DirectRule).unwrap()],
            &mut court,
        );
        assert_eq!(
            court.propose_approval(id),
            Err(GovernanceError::ApprovalNotNeeded)
        );

        // 一键朱元璋：复设丞相后一键株连——在任官员（含六部）全部革职，白身 #7 幸免。
        apply(
            vec![
                court.propose_reform(GovernanceMode::Chancellor).unwrap(),
                CourtEvent::AgentRegistered(Agent {
                    id: AgentId(7),
                    name: "白身".into(),
                    speciality: Department::Works,
                }),
            ],
            &mut court,
        );
        apply(
            vec![
                court
                    .propose_appointment(AgentId(6), Office::Chancellor)
                    .unwrap(),
            ],
            &mut court,
        );
        let holders: Vec<_> = court
            .agents()
            .filter(|a| court.office_of(a.id).is_some())
            .map(|a| a.id)
            .collect();
        assert!(holders.len() >= 3, "{holders:?}");
        apply(court.propose_abolition().unwrap(), &mut court);
        assert_eq!(court.mode(), GovernanceMode::DirectRule);
        assert!(holders.iter().all(|&a| court.is_purged(a)));
        assert!(court.agents().all(|a| court.office_of(a.id).is_none()));
        assert!(!court.is_purged(AgentId(7)));
    }
    #[test]
    fn censorate_reviews_completed_edicts_and_feeds_the_service_record() {
        let mut court = Court::replay(&founding_events(1)).unwrap();
        let mut apply = |events: Vec<CourtEvent>| {
            for e in events {
                court.apply(&e).unwrap();
            }
        };
        let edict = Edict {
            id: TaskId(1),
            department: Department::Works,
            text: "查".into(),
            seal: Seal::default(),
        };
        apply(vec![
            CourtEvent::EdictIssued(edict),
            CourtEvent::TaskAssigned {
                task: TaskId(1),
                worker: AgentId(4),
                model: None,
            },
        ]);
        let report = |censor: u64, verdict, evidence| CourtEvent::CensorReported {
            task: TaskId(1),
            censor: AgentId(censor),
            verdict,
            note: "核".into(),
            evidence,
            model: None,
            usage: TokenUsage::default(),
        };
        // 未完成不复核；完成后，非御史不能复核，经手者任御史也须回避。
        assert_eq!(
            court.propose_censure_check(TaskId(1), AgentId(4)),
            Err(GovernanceError::NotCompleted(TaskId(1)))
        );
        for e in [
            CourtEvent::TaskCompleted {
                task: TaskId(1),
                worker: AgentId(4),
                summary: "src/lib.rs:3".into(),
            },
            CourtEvent::OfficialDismissed { agent: AgentId(4) },
            CourtEvent::OfficialAppointed {
                agent: AgentId(4),
                office: Office::Censor,
            },
        ] {
            court.apply(&e).unwrap();
        }
        assert_eq!(
            court.propose_censure_check(TaskId(1), AgentId(5)),
            Err(GovernanceError::NotCensor(AgentId(5)))
        );
        assert_eq!(
            court.apply(&report(4, CensorVerdict::Endorsed, Evidence::default())),
            Err(GovernanceError::SelfReview(AgentId(4)))
        );
        // 御史不随改制撤销；改由吏部郎中 #5 任御史后背书，证据属实。
        court
            .apply(&CourtEvent::InstitutionReformed(GovernanceMode::Cabinet))
            .unwrap();
        assert_eq!(court.office_of(AgentId(4)), Some(Office::Censor));
        for e in [
            CourtEvent::OfficialDismissed { agent: AgentId(4) },
            CourtEvent::OfficialDismissed { agent: AgentId(5) },
            CourtEvent::OfficialAppointed {
                agent: AgentId(5),
                office: Office::Censor,
            },
        ] {
            court.apply(&e).unwrap();
        }
        let sound = Evidence {
            cited: 1,
            missing: 0,
            tests: None,
        };
        court
            .apply(&report(5, CensorVerdict::Endorsed, sound))
            .unwrap();
        assert_eq!(
            court.apply(&report(5, CensorVerdict::Impeached, sound)),
            Err(GovernanceError::AlreadyCensured(TaskId(1)))
        );
        let record = court.service_record(AgentId(4));
        assert_eq!(
            (
                record.endorsed,
                record.impeached,
                record.verified,
                record.unreviewed
            ),
            (1, 0, 1, 1)
        );
        assert_eq!(record.merit(), 2);
        // 证据：引用有一处查无实据，或测试失败，都不算有据可查。
        let flawed = Evidence {
            cited: 2,
            missing: 1,
            tests: Some(true),
        };
        let failing = Evidence {
            cited: 0,
            missing: 0,
            tests: Some(false),
        };
        assert!(!flawed.verified() && !failing.verified() && !Evidence::default().verified());
    }

    #[test]
    fn caning_needs_repeated_impeachments_and_lifts_after_redemption_or_pardon() {
        let mut court = Court::replay(&founding_events(1)).unwrap();
        // #6 改任左都御史；#4（工部郎中）承办的诏令由他复核。
        for e in [
            CourtEvent::OfficialDismissed { agent: AgentId(6) },
            CourtEvent::OfficialAppointed {
                agent: AgentId(6),
                office: Office::Censor,
            },
        ] {
            court.apply(&e).unwrap();
        }
        let mut next = 0;
        let mut finish = |court: &mut Court, impeach: bool| {
            next += 1;
            let task = TaskId(next);
            let verdict = if impeach {
                CourtEvent::CensorReported {
                    task,
                    censor: AgentId(6),
                    verdict: CensorVerdict::Impeached,
                    note: "不实".into(),
                    evidence: Evidence::default(),
                    model: None,
                    usage: TokenUsage::default(),
                }
            } else {
                CourtEvent::TaskReviewed {
                    task,
                    verdict: Verdict::Accepted,
                    note: String::new(),
                }
            };
            for e in [
                CourtEvent::EdictIssued(Edict {
                    id: task,
                    department: Department::Works,
                    text: "办".into(),
                    seal: Seal::default(),
                }),
                CourtEvent::TaskAssigned {
                    task,
                    worker: AgentId(4),
                    model: None,
                },
                CourtEvent::TaskCompleted {
                    task,
                    worker: AgentId(4),
                    summary: "毕".into(),
                },
                verdict,
            ] {
                court.apply(&e).unwrap();
            }
        };
        let clerk = AgentId(4);
        let cane = |court: &mut Court| court.apply(&CourtEvent::OfficialCaned { agent: clerk });
        // 弹劾不足两次不能杖；未任官职者无级可降。
        finish(&mut court, true);
        assert!(
            matches!(cane(&mut court), Err(GovernanceError::Court(why)) if why.contains("遭弹劾 1 次"))
        );
        finish(&mut court, true);
        assert!(court.propose_caning(AgentId(2)).is_err());
        cane(&mut court).unwrap();
        assert_eq!(
            (court.demotion(clerk), court.redemption_left(clerk)),
            (1, Some(3))
        );
        // 降级留用期间收回写权；同一批弹劾不能再杖。
        let works = Edict {
            id: TaskId(99),
            department: Department::Works,
            text: "改".into(),
            seal: Seal {
                capabilities: [
                    Capability::ReadFile,
                    Capability::WriteFile,
                    Capability::RunTests,
                ]
                .into(),
                scope: vec![".".into()],
                mcp: Default::default(),
            },
        };
        let seal = court.effective_seal(clerk, &works).unwrap();
        assert_eq!(seal.capabilities, [Capability::ReadFile].into());
        assert!(cane(&mut court).is_err());
        // 未立功又遭两次弹劾，再杖则再降一级，立功的件数重新算起。
        finish(&mut court, false);
        finish(&mut court, false);
        assert_eq!(court.redemption_left(clerk), Some(1));
        finish(&mut court, true);
        finish(&mut court, true);
        cane(&mut court).unwrap();
        assert_eq!(
            (court.demotion(clerk), court.redemption_left(clerk)),
            (2, Some(3))
        );
        // 宽宥即复；再宽宥报错。
        court
            .apply(&CourtEvent::OfficialPardoned { agent: clerk })
            .unwrap();
        assert_eq!(court.demotion(clerk), 0);
        assert_eq!(court.effective_seal(clerk, &works).unwrap(), works.seal);
        assert!(court.propose_pardon(clerk).is_err());
        // 宽宥后再杖从一级起；获采纳三件即戴罪立功，自动复职。
        finish(&mut court, true);
        finish(&mut court, true);
        cane(&mut court).unwrap();
        assert_eq!(court.demotion(clerk), 1);
        for _ in 0..3 {
            finish(&mut court, false);
        }
        assert_eq!(
            (court.demotion(clerk), court.redemption_left(clerk)),
            (0, None)
        );
        assert!(court.propose_pardon(clerk).is_err(), "已立功，无须宽宥");
        assert_eq!(court.service_record(clerk).caned, 3);
    }

    #[test]
    fn split_edicts_run_subtasks_first_then_the_minister_summarizes() {
        let mut court = Court::replay(&founding_events(1)).unwrap();
        let apply = |court: &mut Court, e: CourtEvent| court.apply(&e).unwrap();
        apply(
            &mut court,
            CourtEvent::AgentRegistered(Agent {
                id: AgentId(7),
                name: "新工".into(),
                speciality: Department::Works,
            }),
        );
        apply(
            &mut court,
            CourtEvent::OfficialAppointed {
                agent: AgentId(7),
                office: Office::Clerk(Department::Works),
            },
        );
        let edict = court
            .new_edict(Department::Works, "查全库".into(), Seal::default())
            .unwrap();
        apply(&mut court, CourtEvent::EdictIssued(edict));
        let none = TokenUsage::default();
        let split = |court: &Court, task: u64, plan: &[(u64, &str)]| {
            let plan = plan
                .iter()
                .map(|(w, t)| (AgentId(*w), t.to_string()))
                .collect();
            court.propose_split(TaskId(task), plan, "分办".into(), None, none)
        };
        // 只分一件、分给他部官员（#3 吏部尚书）都不行。
        assert!(matches!(
            split(&court, 1, &[(4, "甲")]),
            Err(GovernanceError::InvalidSplit(_))
        ));
        assert!(matches!(
            split(&court, 1, &[(4, "甲"), (3, "乙")]),
            Err(GovernanceError::NotEligible { .. })
        ));
        let event = split(&court, 1, &[(4, "查 storage"), (7, "查 runtime")]).unwrap();
        apply(&mut court, event);
        // 子诏令 #2、#3 先执行，原诏令等子诏令结束；子诏令不再分办，原诏令不能再分。
        assert_eq!(court.runnable_tasks(), [TaskId(2), TaskId(3)]);
        assert_eq!(court.task(TaskId(3)).unwrap().edict.seal, Seal::default());
        for task in [1, 2] {
            assert_eq!(
                split(&court, task, &[(4, "甲"), (7, "乙")]),
                Err(GovernanceError::CannotSplit(TaskId(task)))
            );
        }
        let route = court.route_task(court.task(TaskId(3)).unwrap()).unwrap();
        assert_eq!(route.worker, AgentId(7));
        assert!(
            route.path.last().unwrap().ends_with("（分办）"),
            "{:?}",
            route.path
        );

        apply(
            &mut court,
            CourtEvent::TaskAssigned {
                task: TaskId(2),
                worker: AgentId(4),
                model: None,
            },
        );
        apply(
            &mut court,
            CourtEvent::TaskCompleted {
                task: TaskId(2),
                worker: AgentId(4),
                summary: "无 unsafe".into(),
            },
        );
        assert_eq!(court.runnable_tasks(), [TaskId(3)]);
        apply(&mut court, CourtEvent::TaskCancelled { task: TaskId(3) });
        // 子诏令全部结束（撤销也算），原诏令派给工部尚书 #2 汇总。
        assert_eq!(court.runnable_tasks(), [TaskId(1)]);
        let route = court.route_task(court.task(TaskId(1)).unwrap()).unwrap();
        assert_eq!(route.worker, AgentId(2));
        assert!(
            route.path.last().unwrap().ends_with("（汇总）"),
            "{:?}",
            route.path
        );

        // 内阁制下子诏令随原诏令批红。
        apply(
            &mut court,
            CourtEvent::InstitutionReformed(GovernanceMode::Cabinet),
        );
        apply(
            &mut court,
            CourtEvent::OfficialDismissed { agent: AgentId(6) },
        );
        apply(
            &mut court,
            CourtEvent::OfficialAppointed {
                agent: AgentId(6),
                office: Office::FirstGrandSecretary,
            },
        );
        let edict = court
            .new_edict(Department::Works, "再查".into(), Seal::default())
            .unwrap();
        let id = edict.id;
        apply(&mut court, CourtEvent::EdictIssued(edict));
        let event = split(&court, id.0, &[(4, "甲"), (7, "乙")]).unwrap();
        apply(&mut court, event);
        // （改制后 #1 本身也须批红。）
        assert!(court.runnable_tasks().is_empty());
        apply(&mut court, CourtEvent::EdictApproved { task: id });
        assert_eq!(court.runnable_tasks(), [TaskId(id.0 + 1), TaskId(id.0 + 2)]);
    }

    #[test]
    fn cannot_create_two_chancellors() {
        let court = Court::replay(&founding_events(1)).unwrap();
        let mut court = court;
        court
            .apply(&court.propose_dismissal(AgentId(6)).unwrap())
            .unwrap();
        assert!(
            court
                .propose_appointment(AgentId(6), Office::Chancellor)
                .is_err()
        );
    }
    #[test]
    fn cannot_replay_duplicate_appointment() {
        let mut events = founding_events(1);
        events.push(CourtEvent::OfficialAppointed {
            agent: AgentId(2),
            office: Office::Minister(Department::Works),
        });
        assert!(Court::replay(&events).is_err());
    }
    #[test]
    fn replay_applies_same_rules_as_proposals() {
        let mut events = founding_events(1);
        events.push(CourtEvent::OfficialDismissed { agent: AgentId(7) });
        assert_eq!(
            Court::replay(&events).unwrap_err(),
            GovernanceError::NotAppointed(AgentId(7))
        );
        let mut events = founding_events(1);
        events.push(CourtEvent::InstitutionReformed(GovernanceMode::Chancellor));
        assert!(Court::replay(&events).is_err());
    }
    #[test]
    fn libu_counts_only_reviewed_results_and_ranks_by_them() {
        let mut events = founding_events(1);
        events.push(CourtEvent::AgentRegistered(Agent {
            id: AgentId(7),
            name: "新工".into(),
            speciality: Department::Works,
        }));
        let mut next = 1;
        // (承办者, 是否完成, 评定, 失败的工具调用数)
        let mut handle = |worker: u64, ok: bool, verdict: Option<Verdict>, bad_tools: u32| {
            let task = TaskId(next);
            next += 1;
            events.push(CourtEvent::EdictIssued(Edict {
                id: task,
                department: Department::Works,
                text: "查".into(),
                seal: Seal::default(),
            }));
            events.push(CourtEvent::TaskAssigned {
                task,
                worker: AgentId(worker),
                model: None,
            });
            events.push(CourtEvent::ModelUsage {
                task,
                usage: TokenUsage {
                    input: 100,
                    output: 7,
                },
                trace: None,
            });
            for _ in 0..bad_tools {
                events.push(CourtEvent::ToolInvoked {
                    task,
                    tool: "read_file".into(),
                    args_json: "{}".into(),
                    ok: false,
                });
            }
            events.push(if ok {
                CourtEvent::TaskCompleted {
                    task,
                    worker: AgentId(worker),
                    summary: "ok".into(),
                }
            } else {
                CourtEvent::TaskFailed {
                    task,
                    reason: "超时".into(),
                }
            });
            if let Some(verdict) = verdict {
                events.push(CourtEvent::TaskReviewed {
                    task,
                    verdict,
                    note: String::new(),
                });
            }
        };
        // 工部郎中 #4 完成数最多，但多为驳回或未评定；新工（#7）完成少但被采纳。
        handle(4, true, Some(Verdict::Rejected), 0);
        handle(4, true, None, 0);
        handle(4, true, None, 0);
        handle(4, false, None, 2);
        handle(7, true, Some(Verdict::Accepted), 1);
        let court = Court::replay(&events).unwrap();

        assert_eq!(
            court.service_record(AgentId(4)),
            ServiceRecord {
                accepted: 0,
                rejected: 1,
                unreviewed: 2,
                failed: 1,
                tool_calls: 2,
                tool_failures: 2,
                // 4 件（含失败的一件）各 100/7。
                usage: TokenUsage {
                    input: 400,
                    output: 28,
                },
                ..ServiceRecord::default()
            }
        );
        assert_eq!(court.total_usage().total(), 5 * 107);
        let ranked: Vec<_> = court
            .recommend(Office::Clerk(Department::Works))
            .unwrap()
            .iter()
            .map(|c| c.agent.id.0)
            .collect();
        // 新工居首；工部尚书 #2（无履历）在被驳回过的 #4 之前；吏部官员不入选。
        assert_eq!(ranked, [7, 2, 4]);
        assert!(court.recommend(Office::FirstGrandSecretary).is_err());

        // 只能评定已完成且未评定过的诏令。
        assert_eq!(
            court.propose_review(TaskId(4), Verdict::Accepted, String::new()),
            Err(GovernanceError::NotCompleted(TaskId(4)))
        );
        assert_eq!(
            court.propose_review(TaskId(5), Verdict::Rejected, String::new()),
            Err(GovernanceError::AlreadyReviewed(TaskId(5)))
        );
        assert!(
            court
                .propose_review(TaskId(2), Verdict::Accepted, "好".into())
                .is_ok()
        );
    }
    #[test]
    fn seal_scope_must_stay_inside_project() {
        let court = Court::replay(&founding_events(1)).unwrap();
        let seal = |scope: &[&str]| Seal {
            capabilities: Default::default(),
            scope: scope.iter().map(|s| s.to_string()).collect(),
            mcp: Default::default(),
        };
        let issue = |scope: &[&str]| court.new_edict(Department::Works, "查".into(), seal(scope));
        assert!(issue(&["."]).is_ok());
        assert!(issue(&["./crates/ziyuan-storage", "README.md"]).is_ok());
        for bad in ["/etc", "../secret", "src/../../x", " "] {
            assert_eq!(
                issue(&["src", bad]).unwrap_err(),
                GovernanceError::InvalidScope(bad.into()),
                "{bad}"
            );
        }
        assert!(issue(&[]).is_err());
        // 回放同样拒绝：手写的日志也不能绕过。
        let mut events = founding_events(1);
        events.push(CourtEvent::EdictIssued(Edict {
            id: TaskId(1),
            department: Department::Works,
            text: "查".into(),
            seal: seal(&["/"]),
        }));
        assert!(Court::replay(&events).is_err());
    }
    #[test]
    fn task_lifecycle_is_enforced_on_replay() {
        let task = TaskId(1);
        let mut base = founding_events(1);
        base.push(CourtEvent::EdictIssued(Edict {
            id: task,
            department: Department::Works,
            text: "查".into(),
            seal: Seal::default(),
        }));
        let with = |extra: &[CourtEvent]| {
            let mut events = base.clone();
            events.extend_from_slice(extra);
            Court::replay(&events)
        };
        let assigned = CourtEvent::TaskAssigned {
            task,
            worker: AgentId(4),
            model: None,
        };
        let tool = CourtEvent::ToolInvoked {
            task,
            tool: "read_file".into(),
            args_json: "{}".into(),
            ok: true,
        };
        let done_by = |worker| CourtEvent::TaskCompleted {
            task,
            worker: AgentId(worker),
            summary: "ok".into(),
        };

        let usage = CourtEvent::ModelUsage {
            task,
            usage: TokenUsage {
                input: 1,
                output: 1,
            },
            trace: None,
        };
        assert_eq!(
            with(std::slice::from_ref(&usage)).unwrap_err(),
            GovernanceError::TaskNotAssigned(task)
        );
        // 结束后仍可补记用量（超时后才返回的调用同样消耗了 token）。
        assert!(with(&[assigned.clone(), CourtEvent::TaskCancelled { task }, usage]).is_ok());
        // v0.1 日志：未指派即完成，仍可回放。
        assert!(with(&[done_by(4)]).is_ok());
        assert!(with(&[assigned.clone(), tool.clone(), done_by(4)]).is_ok());
        assert_eq!(
            with(&[assigned.clone(), done_by(2)]).unwrap_err(),
            GovernanceError::WrongWorker {
                task,
                worker: AgentId(2)
            }
        );
        assert_eq!(
            with(std::slice::from_ref(&tool)).unwrap_err(),
            GovernanceError::TaskNotAssigned(task)
        );
        assert_eq!(
            with(&[assigned, CourtEvent::TaskCancelled { task }, tool]).unwrap_err(),
            GovernanceError::TaskFinished(task)
        );
        let court = with(&[CourtEvent::TaskFailed {
            task,
            reason: "x".into(),
        }])
        .unwrap();
        assert!(court.open_tasks().is_empty());
        assert!(court.propose_cancellation(task).is_err());
    }
    #[test]
    fn recruitment_enables_dynamic_worker_pool() {
        let mut court = Court::replay(&founding_events(1)).unwrap();
        let event = court
            .propose_recruitment("新工".into(), Department::Works)
            .unwrap();
        court.apply(&event).unwrap();
        assert!(court.agents.contains_key(&AgentId(7)));
        court
            .apply(
                &court
                    .propose_appointment(AgentId(7), Office::Clerk(Department::Works))
                    .unwrap(),
            )
            .unwrap();
        let edict = court
            .new_edict(Department::Works, "测试".into(), Seal::default())
            .unwrap();
        assert_eq!(court.route(&edict).unwrap().worker, AgentId(4));
        let completed = [
            CourtEvent::EdictIssued(edict.clone()),
            CourtEvent::TaskCompleted {
                task: edict.id,
                worker: AgentId(4),
                summary: "demo".into(),
            },
        ];
        for e in &completed {
            court.apply(e).unwrap();
        }
        assert_eq!(court.route(&edict).unwrap().worker, AgentId(7));
    }
    #[test]
    fn appoint_validates_department() {
        let mut court = Court::replay(&founding_events(1)).unwrap();
        court
            .apply(&court.propose_dismissal(AgentId(4)).unwrap())
            .unwrap();
        assert!(
            court
                .propose_appointment(AgentId(4), Office::Clerk(Department::Personnel))
                .is_err()
        );
    }
}
