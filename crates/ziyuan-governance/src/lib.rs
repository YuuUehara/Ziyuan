//! 治理引擎。职权由代码校验，不由 Agent 的角色提示词决定。
mod cabinet;
mod censorate;
mod courier;
mod edicts;
mod exam;
pub mod names;
mod offices;
mod personnel;
mod query;
mod replay;
mod templates;
mod translation;
mod treasury;

pub use cabinet::{Draft, Split};
pub use censorate::{Censure, Suspicion, SuspicionReview};
pub use edicts::{ChangeState, Changes, Route};
pub use exam::{ExamRecord, PalaceExam};
pub use personnel::{Candidate, Caning, NominationBallot};
pub use replay::founding_events;
pub use templates::{TemplateUse, builtin_templates};
pub use translation::Signoff;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;
use ziyuan_core::{
    Agent, AgentId, CHANGELOG, Capability, DagNode, Department, Edict, GovernanceMode, Nomination,
    Office, Seal, Speech, TaskId, Template, TokenUsage, TranslationLink, Verdict,
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
    /// 分办排程不合规矩（原因）。
    InvalidDag(String),
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
    /// 改名、改字号不合规矩（说明原因）。
    InvalidName(String),
    /// 翰林院译书（关联、定稿、退回）不合规矩。
    Translation(String),
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
            Self::InvalidName(why) => f.write_str(why),
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
            Self::InvalidDag(why) => write!(f, "分办排程不合法：{why}"),
            Self::RetreatUnchanged(true) => f.write_str("陛下已在西苑修道"),
            Self::RetreatUnchanged(false) => f.write_str("陛下本在朝中"),
            Self::CannotDebate(id) => write!(f, "诏令 #{} 不能廷议：须尚未执行", id.0),
            Self::InvalidDebate(why) => write!(f, "廷议未成：{why}"),
            Self::InvalidExam(why) => write!(f, "礼部不准：{why}"),
            Self::Military(why) => write!(f, "兵部不准：{why}"),
            Self::Template(why) => write!(f, "翰林院不准：{why}"),
            Self::Translation(why) => write!(f, "翰林院译书不准：{why}"),
            Self::Court(why) => write!(f, "{why}"),
            Self::Changes(why) => write!(f, "逐段准奏不成：{why}"),
            Self::Suspicion(why) => write!(f, "刑部不准：{why}"),
            Self::InvalidBudget => f.write_str("度支须为正数（tokens）"),
            Self::TreasuryExhausted(limit) => write!(
                f,
                "国库告罄：朝廷度支 {limit} tokens 已用尽（户部奏：太仓无银）。\
                 追加请 `ziyuan budget court <额度>`，取消请 `ziyuan budget court default`"
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
#[derive(Clone, Debug, Serialize, Deserialize)]
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
    /// 皇帝改易过的字与号（未改的取 `default_styles`）。
    #[serde(default)]
    courtesy_names: BTreeMap<AgentId, String>,
    #[serde(default)]
    art_names: BTreeMap<AgentId, String>,
    /// 译书各块最近一次御览（定稿或退回），按项目、块 ID。
    #[serde(default)]
    translation_signoffs: BTreeMap<String, BTreeMap<String, Signoff>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// 分办排程的节点（子诏令才有）：依赖与限束，见 [`ziyuan_core::DagNode`]。
    #[serde(default)]
    pub dag: Option<DagNode>,
    /// 历次指派的次数（含首次；中断恢复重跑亦计）。
    #[serde(default)]
    pub attempts: u32,
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
    /// 驿站外发回奏评论与草稿 PR 时记下的幂等键（`CourierDispatched`）；有键而无网址即待确认。
    pub reply_op: Option<String>,
    pub pull_request_op: Option<String>,
    /// 八百里加急。
    pub urgent: bool,
    /// 按翰林院模板下达时的模板名与验收标准。
    pub template: Option<TemplateUse>,
    /// 关联的译书块（`TranslationLinked`）。
    #[serde(default)]
    pub translation: Option<TranslationLink>,
}

impl Task {
    /// 尚未平反的检出。有这样的检出时，司礼监不代批。
    pub fn standing_suspicions(&self) -> impl Iterator<Item = &Suspicion> {
        self.suspicions.iter().filter(|s| !s.acquitted)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    /// 已入队，尚未指派。
    Pending,
    /// 已指派、尚未结束（执行中，或上次运行中断）。
    Assigned(AgentId),
    Completed {
        worker: AgentId,
        /// 与事件簿中的 `TaskCompleted` 共用一份。
        summary: Arc<str>,
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

#[cfg(test)]
mod tests;
