//! 朝廷事件：事件簿里的每一条记录，以及诏令、廷推与模板。

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::agent::Agent;
use crate::changes::FileChange;
use crate::office::{BudgetScope, Department, GovernanceMode, Office};
use crate::review::{CensorVerdict, Evidence, Threat, Verdict};
use crate::seal::Seal;
use crate::usage::TokenUsage;
use crate::{AgentId, TaskId};

/// 廷议中一位官员的一段发言。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Speech {
    pub speaker: AgentId,
    /// 发言时的官职。
    pub office: Office,
    pub text: String,
    pub model: Option<String>,
    pub usage: TokenUsage,
}

/// 分办出去的一件子诏令：沿用原诏令的部门与玉玺，由指定官员承办。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subtask {
    pub id: TaskId,
    pub worker: AgentId,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edict {
    pub id: TaskId,
    pub department: Department,
    pub text: String,
    pub seal: Seal,
}

/// 廷推中一位大臣的推举。`model`/`usage` 为推举所用模型与用量（按吏部考绩代推时为 `None` 与零）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Nomination {
    pub voter: AgentId,
    pub candidate: AgentId,
    pub note: String,
    pub model: Option<String>,
    pub usage: TokenUsage,
}

/// 翰林院诏令模板：常用诏令的内容、默认部门与玉玺，以及交都察院核验的验收标准。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// 皇帝为官员改名、改字或改号（如徐珵改名徐有贞）；`None` 的一项不变。身份、履历与人格不变。
    AgentRenamed {
        agent: AgentId,
        name: Option<String>,
        courtesy: Option<String>,
        art: Option<String>,
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
    /// 皇帝设定度支（token 限额）；`None` 取消。超支即停；每次调用前预留，最多超出实际用量高于预留的部分。
    BudgetSet {
        scope: BudgetScope,
        tokens: Option<u64>,
    },
    /// 诏令入队；此后依次可有 `TaskAssigned`、若干 `ToolInvoked`，并以完成/失败/撤销之一结束。
    /// 装箱：诏令较大，而事件簿中大多是工具调用、用量等小事件；不装箱则每条事件都按最大的占内存。
    EdictIssued(Box<Edict>),
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
        /// 回奏全文；与朝廷状态中的同一件共用一份（`Arc`），长期运行时不重复占内存。
        summary: Arc<str>,
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
    /// 驿站即将把结果送回来处（外发前先记意图）：`op` 为幂等键，写进外发的评论或 PR。
    /// 记了而无对应的 `CourierReplied` 即「待确认」：重试时先按 `op` 在远端查找，找到就补记，
    /// 不重发。
    CourierDispatched {
        task: TaskId,
        pull_request: bool,
        op: String,
    },
    /// 驿站把结果送回来处：`pull_request` 为假是回奏评论，为真是准奏改动开的草稿 PR；`url` 为其网址。
    /// `op` 为先前 `CourierDispatched` 的幂等键（旧版记下的没有）。
    CourierReplied {
        task: TaskId,
        pull_request: bool,
        url: String,
        op: Option<String>,
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
        template: Box<Template>,
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
