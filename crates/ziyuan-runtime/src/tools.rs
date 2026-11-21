//! 工作区工具。可用的工具与可查阅的路径由诏令的玉玺（[`Seal`]）决定。
//!
//! 所有路径先 canonicalize 再检查，因此 `..`、绝对路径与符号链接都无法越出玉玺的查阅范围。
//! 无论玉玺如何，事件簿与版本库元数据（`.ziyuan`、`.git`、`.jj`）及 `.env*` 文件一律不可读写。
//!
//! `write_file` 不写项目：改动暂存在内存里，执行结束后由 [`Sealed::changes`] 取出呈报，
//! 皇帝准奏后才由 [`apply_changes`] 写入。`run_tests` 在项目的临时副本（叠加暂存改动）中
//! 运行皇帝设定的命令，子进程只继承少数无害的环境变量（不含 API Key），并关进号舍
//! （见 [`crate::haoshe`]）：不能联网，只能写副本。
use crate::BoxError;
use crate::agent_loop::{Tool, ToolSpec};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ziyuan_core::{CHANGELOG, Capability, FileChange, Record, Seal, search_records};

mod failures;
mod glob;
mod replace;
mod symbols;
pub mod temp_cleanup;
mod vcs;

/// 单次 `read_file` 最多返回的字节数。
const MAX_READ_BYTES: usize = 64 * 1024;
/// 单次 `search_code` 一页最多返回的匹配行数，以及上下文行上限。
const MAX_MATCHES: usize = 50;
const MAX_CONTEXT_LINES: usize = 5;
/// 单次 `find_references` 一页最多返回的引用数。
const MAX_REF_MATCHES: usize = 50;
/// 单次 `list_files` 最多列出的文件数。
const MAX_LISTED: usize = 300;
/// 单次 `file_outline` 默认与最多返回的符号数、可扫描的文件字节上限。
const DEFAULT_OUTLINE_SYMBOLS: usize = 100;
const MAX_OUTLINE_SYMBOLS: usize = 200;
const MAX_OUTLINE_BYTES: usize = 2 * 1024 * 1024;
/// 单次 `find_symbols` 一页最多返回的符号数、跨文件扫描时单文件字节上限。
const MAX_SYMBOL_MATCHES: usize = 50;
const MAX_SYMBOL_FILE_BYTES: u64 = 1024 * 1024;
/// 永远不可查阅的目录：朝廷事件簿与版本库元数据。
const DENIED_DIRS: &[&str] = &[".ziyuan", ".git", ".jj"];
/// 搜索时跳过（但可直接读取）的目录：构建产物。测试副本也不复制它们。
const SKIPPED_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
];
/// 单次 `write_file` 最多写入的字节数。
const MAX_WRITE_BYTES: usize = 256 * 1024;
/// 单次 `replace_text` 批量补丁最多允许的条数。
const MAX_REPLACE_EDITS: usize = 50;
/// `run_tests` 的时限、返回的输出末尾字节数，以及从末尾回扫 cargo 失败摘录的上限。
const TEST_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_TEST_OUTPUT: u64 = 8 * 1024;
const MAX_FAILURE_SCAN: u64 = 1024 * 1024;
/// 测试子进程可继承的环境变量；其余（API Key 等）一概不传。
const TEST_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "LC_ALL",
    "TERM",
    "TMPDIR",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "CARGO_TARGET_DIR",
    // Windows：cmd 与 cargo 少了这些跑不起来。
    "SystemRoot",
    "SystemDrive",
    "PATHEXT",
    "ComSpec",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
];

/// 按玉玺构造的工具，及其暂存的改动。
pub struct Sealed {
    pub tools: Vec<Box<dyn Tool>>,
    sandbox: Arc<Sandbox>,
}

impl Sealed {
    /// 执行期间暂存的改动（按路径排序；写回原文的不算改动）。
    pub fn changes(&self) -> Vec<FileChange> {
        let staged = self.sandbox.staged.lock().expect("暂存区锁未中毒");
        staged
            .values()
            .filter(|c| c.before.as_deref() != Some(c.after.as_str()))
            .cloned()
            .collect()
    }
}

/// 按玉玺构造工具：只包含其授予的工具，且都限定在其查阅范围内。
/// `test_command` 为皇帝设定的测试命令（`run_tests` 用；未设定时该工具报错）。
/// 查阅范围不存在、越出 `root` 或落在禁区时返回错误。
/// 解析皇帝设定的命名测试／检查白名单（`ZIYUAN_TEST_COMMANDS`）。
///
/// 格式为分号或换行分隔的 `名称=命令`（如 `check=cargo check;clippy=cargo clippy;test=cargo test`）；
/// 名称限 1–32 位 ASCII 字母、数字、`_` 或 `-`，禁止重名或空命令。模型只能按名称选用，绝不能注入或改写命令。
pub fn parse_test_presets(raw: &str) -> Result<BTreeMap<String, String>, BoxError> {
    let mut presets = BTreeMap::new();
    for entry in raw.split([';', '\n']) {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((name, command)) = trimmed.split_once('=') else {
            return Err(
                format!("ZIYUAN_TEST_COMMANDS 格式错误（须为 `名称=命令`）：{trimmed}").into(),
            );
        };
        let name = name.trim();
        let command = command.trim();
        if name.is_empty()
            || name.len() > 32
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        {
            return Err(format!(
                "ZIYUAN_TEST_COMMANDS 预设名 `{name}` 不合法（限 1–32 位 ASCII 字母、数字、_ 或 -）"
            )
            .into());
        }
        if command.is_empty() {
            return Err(format!("ZIYUAN_TEST_COMMANDS 预设 `{name}` 缺少命令").into());
        }
        if presets
            .insert(name.to_owned(), command.to_owned())
            .is_some()
        {
            return Err(format!("ZIYUAN_TEST_COMMANDS 预设名重复：{name}").into());
        }
    }
    Ok(presets)
}

/// 按玉玺构造工具：只包含其授予的工具，且都限定在其查阅范围内。
/// `test_command` 为皇帝设定的默认测试命令（`run_tests` 用；未设定时该工具报错）。
/// 查阅范围不存在、越出 `root` 或落在禁区时返回错误。
pub fn sealed_tools(
    root: &Path,
    seal: &Seal,
    test_command: Option<&str>,
) -> Result<Sealed, BoxError> {
    sealed_tools_with_presets(root, seal, test_command, BTreeMap::new())
}

/// 与 [`sealed_tools`] 相同，另附皇帝通过 `ZIYUAN_TEST_COMMANDS` 设定的命名检查／测试命令白名单。
pub fn sealed_tools_with_presets(
    root: &Path,
    seal: &Seal,
    test_command: Option<&str>,
    test_presets: BTreeMap<String, String>,
) -> Result<Sealed, BoxError> {
    let sandbox = Arc::new(Sandbox::new(root, &seal.scope, test_command, test_presets)?);
    let mut tools: Vec<Box<dyn Tool>> = Vec::new();
    for capability in &seal.capabilities {
        match capability {
            // 能读文件就能看目录与符号大纲：`list_files`、`file_outline` 随 `read_file` 授予，不另设权限，玉玺编码不变。
            Capability::ReadFile => {
                tools.push(Box::new(ReadFile(sandbox.clone())));
                tools.push(Box::new(ListFiles(sandbox.clone())));
                tools.push(Box::new(FileOutline(sandbox.clone())));
                tools.push(Box::new(vcs::VcsStatus(sandbox.clone())));
                tools.push(Box::new(vcs::VcsDiff(sandbox.clone())));
                tools.push(Box::new(vcs::VcsLog(sandbox.clone())));
            }
            // `find_symbols`、`find_references` 随 `search_code` 授予，不另设权限，玉玺编码不变。
            Capability::SearchCode => {
                tools.push(Box::new(SearchCode(sandbox.clone())));
                tools.push(Box::new(FindSymbols(sandbox.clone())));
                tools.push(Box::new(FindReferences(sandbox.clone())));
            }
            // `replace_text`、`delete_file`、`rename_file` 随 `write_file` 授予，不另设权限，玉玺编码不变。
            Capability::WriteFile => {
                tools.push(Box::new(WriteFile(sandbox.clone())));
                tools.push(Box::new(ReplaceText(sandbox.clone())));
                tools.push(Box::new(DeleteFile(sandbox.clone())));
                tools.push(Box::new(RenameFile(sandbox.clone())));
            }
            Capability::RunTests => tools.push(Box::new(RunTests(sandbox.clone()))),
        }
    }
    Ok(Sealed { tools, sandbox })
}

/// 单次 `search_records` 最多返回的实录件数。
const MAX_RECORDS: usize = 5;

/// 只读工具 `search_records`：查阅翰林院实录（已结诏令的回奏与评定）中的前例。
/// 不读文件，故不受玉玺范围所限；输出与其他工具一样作为外来文书包上边界。
pub fn record_search(records: Arc<Vec<Record>>) -> Box<dyn Tool> {
    Box::new(SearchRecords(records))
}

struct SearchRecords(Arc<Vec<Record>>);

impl Tool for SearchRecords {
    fn is_read_only(&self) -> bool {
        true
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search_records".into(),
            description: format!(
                "查阅翰林院实录：按关键词检索已结诏令的诏令、回奏与皇帝评定，作为前例参考，最多 {MAX_RECORDS} 件。只读；实录只增不改。"
            ),
            input_schema_json: r#"{"type":"object","properties":{"query":{"type":"string","description":"关键词，如函数名、文件名或事由"}},"required":["query"],"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let query = string_arg(args_json, "query")?;
        if query.trim().is_empty() {
            return Err("query 不能为空".into());
        }
        let found = search_records(&self.0, &query, MAX_RECORDS);
        if found.is_empty() {
            return Ok("实录中无相关前例".into());
        }
        let clip = |text: &str, max: usize| {
            let text = text.trim();
            match text.char_indices().nth(max) {
                Some((at, _)) => format!("{}…", &text[..at]),
                None => text.to_owned(),
            }
        };
        Ok(found
            .iter()
            .map(|r| {
                format!(
                    "#{} {} 承办 #{} {}（{}）\n诏令：{}\n回奏：{}",
                    r.task.0,
                    r.department.label(),
                    r.worker.0,
                    r.handler,
                    r.verdict,
                    clip(&r.edict, 200),
                    clip(&r.summary, 600)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n"))
    }
}

/// 审计所记的工具参数：写入类工具的全文已在奏本中，只记行数与字节数。
pub fn audit_args(tool: &str, args_json: &str) -> String {
    if tool != "write_file" && tool != "replace_text" {
        return args_json.to_owned();
    }
    let Ok(mut args) = serde_json::from_str::<Value>(args_json) else {
        return args_json.to_owned();
    };
    let redact_str = |content: &str| {
        format!(
            "〔{} 行，{} 字节，全文见奏本〕",
            content.lines().count(),
            content.len()
        )
    };
    for key in ["content", "old", "new"] {
        if let Some(content) = args.get(key).and_then(|c| c.as_str()) {
            args[key] = redact_str(content).into();
        }
    }
    if let Some(edits) = args.get_mut("edits").and_then(Value::as_array_mut) {
        for edit in edits {
            for key in ["old", "new"] {
                if let Some(content) = edit.get(key).and_then(Value::as_str) {
                    edit[key] = redact_str(content).into();
                }
            }
        }
    }
    args.to_string()
}

/// 准奏前项目中文件的现状。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseState {
    /// 仍是呈报时的原文（新建文件则仍不存在）。
    Original,
    /// 已是改后内容：上次落盘写到一半或写完未及记账就中断。
    Written,
    /// 已被别处改动（含改成非 UTF-8 或二进制内容）。
    Changed,
}

/// `change` 所指文件的现状，逐字节比较（CRLF 与 LF 视为不同）。路径越界或属禁区时报错。
pub fn base_state(root: &Path, change: &FileChange) -> Result<BaseState, BoxError> {
    Ok(base_state_in(&root.canonicalize()?, change)?.0)
}

fn base_state_in(root: &Path, change: &FileChange) -> Result<(BaseState, PathBuf), BoxError> {
    let target = contained(root, &change.path)?;
    let current = match fs::read(&target) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let state = if change.deleted {
        if current.is_none() {
            BaseState::Written
        } else if current.as_deref() == change.before.as_deref().map(str::as_bytes) {
            BaseState::Original
        } else {
            BaseState::Changed
        }
    } else if current.as_deref() == Some(change.after.as_bytes()) {
        BaseState::Written
    } else if current.as_deref() == change.before.as_deref().map(str::as_bytes) {
        BaseState::Original
    } else {
        BaseState::Changed
    };
    Ok((state, target))
}

/// 一次成功落盘的结果：写入已完成，清理诊断交给调用方决定去处。
#[derive(Debug)]
pub struct Applied {
    /// 自动清理或删租约时的 I/O 失败；写入本身成功。
    pub problems: Vec<temp_cleanup::Problem>,
}

/// 落盘失败：`error` 才是失败原因，`problems` 是同一轮里另记下的清理诊断。
///
/// 两者分开：设计明确“清理失败不阻断写入、也不能写成替换失败”，所以 `Display`
/// 只呈现 `error`，调用方可另行报告 `problems`。
pub struct ApplyFailure {
    pub error: BoxError,
    pub problems: Vec<temp_cleanup::Problem>,
}

impl std::fmt::Display for ApplyFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::fmt::Debug for ApplyFailure {
    // 手动实现：`BoxError`（`Box<dyn Error + Send + Sync>`）不是 `Debug`，
    // 且诊断向量不该整份出现在调试输出里。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApplyFailure")
            .field("error", &self.error.to_string())
            .field("problems", &self.problems.len())
            .finish()
    }
}

impl std::error::Error for ApplyFailure {}

/// 把皇帝准奏的改动写入项目，**不做**临时文件清理。
///
/// 需要“写入前清掉同目录陈旧临时文件”的调用方一律用 [`apply_changes_protected`]：
/// 只有它能拿到调用方从恢复记录构造的受保护集合。这里保持无清理行为，是为了不让
/// 不知道受保护集合的调用方（含测试与将来的调用者）在有待恢复记录的工作区里扫掉
/// 不该扫的东西——设计明确“集合无法构造时整轮不删”。
///
/// 不清理不等于不管诊断：替换本身若删不掉自己的租约，只会记进线程局部通道；
/// 本函数取走并随返回值交给调用方，免得那一条漂到下一次调用头上。
pub fn apply_changes(root: &Path, changes: &[FileChange]) -> Result<Applied, ApplyFailure> {
    apply(root, changes, None)
}

/// 同 [`apply_changes`]，另在写每个目标之前清掉该目录里可证明陈旧的临时文件。
///
/// `protected` 是调用方从恢复记录构造的工作区相对路径集合；集合构造失败时调用方
/// 不应调用本函数（宁可不清理），传空集合等于“没有受保护项”。
/// 自动清理绝不阻断写入：任何清理失败都只进诊断，写入照旧。
///
/// 诊断一律随返回值交给调用方：设计里的“诊断走 stderr”约束的是命令行前端，
/// 不是这个库函数——`ziyuan-tui` 在备用屏里调用本函数，往 stderr 写一行就落进
/// 画面并被下一次重绘抹掉。所以这里既不打印也不丢弃。
pub fn apply_changes_protected(
    root: &Path,
    changes: &[FileChange],
    protected: &BTreeSet<String>,
) -> Result<Applied, ApplyFailure> {
    apply(root, changes, Some(protected))
}

fn apply(
    root: &Path,
    changes: &[FileChange],
    protected: Option<&BTreeSet<String>>,
) -> Result<Applied, ApplyFailure> {
    // 先取走上次遗留的诊断：线程局部状态不能跨调用漂移，否则失败会被算到别人头上。
    let mut problems = temp_cleanup::drain_problems();
    let outcome = write_all(root, changes, protected, &mut problems);
    // 无论成功还是提前返回，本线程累积的失败都要交出去，不留在 thread_local 里。
    problems.extend(temp_cleanup::drain_problems());
    match outcome {
        Ok(()) => Ok(Applied { problems }),
        // 清理诊断不能顶替写入错误，也不能被它吞掉：各占一个字段。
        Err(error) => Err(ApplyFailure { error, problems }),
    }
}

/// 把改动写入项目。先逐一核对：路径仍在项目内且不属禁区，文件内容与呈报时的原文
/// 一致（新建文件则仍不存在）；有任何一处不符则一处也不写。已等于改后内容的文件视为已写
/// （上次落盘写到一半或写完未及记账就中断），跳过不写，重试因此能续完。
/// 每个文件经同目录临时文件整体替换，不会留下写了一半的文件；多个文件之间不是原子的。
///
/// `protected` 为 `None` 时整轮不清理（给不掌握受保护集合的调用方用）。
fn write_all(
    root: &Path,
    changes: &[FileChange],
    protected: Option<&BTreeSet<String>>,
    problems: &mut Vec<temp_cleanup::Problem>,
) -> Result<(), BoxError> {
    let root = root.canonicalize()?;
    let mut targets = Vec::new();
    for change in changes {
        let (state, target) = base_state_in(&root, change)?;
        match state {
            BaseState::Written => {}
            BaseState::Original => {
                // 目录标签取**词法**路径，与 canonical 的 `target` 分开存：受保护集合里的
                // 条目来自恢复记录的 `targets[].path`，也是词法形式。
                let relative_parent = relative_parent_of(&change.path);
                let payload = (!change.deleted).then_some(change.after.as_str());
                targets.push((target, payload, relative_parent));
            }
            BaseState::Changed => {
                return Err(format!("{} 自呈报以来已被改动，拒绝落盘", change.path).into());
            }
        }
    }
    // 同一个目录只扫一次：一批 50 个同目录文件否则要扫 50 遍，每遍都是 `read_dir` 加
    // 逐个候选的 `open`／`try_lock`。这不只是省事——第一遍替换之后若有一份租约没删掉，
    // 第二遍就会把本调用自己的残渣当成孤儿租约处理，而恢复记录正要靠它认领临时文件。
    let mut swept: BTreeSet<PathBuf> = BTreeSet::new();
    for (target, after, relative_parent) in targets {
        if let Some(parent) = target.parent() {
            if after.is_some() {
                // 先建目录再扫：首次写入新目录时目录尚不存在，`sweep_dir` 会把“读不到”
                // 记成一条 I/O 失败并算作没扫全，正常的第一次写入就被报成清理失败。
                fs::create_dir_all(parent)?;
            }
            if let Some(protected) = protected
                && parent.is_dir()
                && swept.insert(parent.to_path_buf())
            {
                // 只清本次写入目标所在目录；不取工作目录锁，调用方本就持锁。
                // 扫在替换之前：目录里此刻没有本次调用建出的临时文件与租约，
                // 本轮自己建的写入证据绝不可能被自己判成陈旧删掉。反过来（先写后扫）
                // 就会把替换失败留下的租约当孤儿处理，恢复记录正要靠它认领临时文件。
                let report = temp_cleanup::sweep_dir(parent, &relative_parent, protected, true);
                problems.extend(report.problems);
            }
        }
        match after {
            Some(content) => replace::replace_target(&target, content.as_bytes())?,
            None => replace::remove_target(&target)?,
        }
    }
    Ok(())
}

/// 改动路径的父目录，工作区相对、`/` 分隔；根目录是空串。
///
/// 用**词法**路径而不是 canonical 路径：受保护集合里的条目来自恢复记录的
/// `targets[].path`，也是词法形式。两者一旦用了不同的拼写（符号链接、盘符大小写），
/// 比较就永远失败，受保护项会被当成普通候选删掉。
///
/// 这里不做“从 `root` 剥前缀”那一步，因此也没有剥失败时的回落值：单组件路径
/// （工作区根下的文件）得到空串，正是根目录的标签。带 `..`、`.` 之外的非普通组件
/// 或绝对路径的改动在 `contained` 那一步就被拒了，到不了这里，所以“空串＝工作区根”
/// 没有第二种来源——错把别的目录当成根来扫这条退路根本不存在。
fn relative_parent_of(change_path: &str) -> String {
    match Path::new(change_path).parent() {
        Some(parent) => temp_cleanup::normalize_relative(parent),
        None => String::new(),
    }
}

/// `relative` 在 `root`（canonical）内对应的路径，文件可以尚不存在：取最近的已存在祖先
/// canonicalize（符号链接无法借此越界），再接上其余部分。越出项目或属于禁区时报错。
fn contained(root: &Path, relative: &str) -> Result<PathBuf, BoxError> {
    let denied = || -> BoxError {
        format!("拒绝访问玉玺查阅范围之外的路径: {relative}").into()
    };
    let lexical = Path::new(relative);
    if relative.trim().is_empty()
        || lexical
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(denied());
    }
    let mut existing = root.join(lexical);
    let mut rest = Vec::new();
    while fs::symlink_metadata(&existing).is_err() {
        rest.push(existing.file_name().ok_or_else(denied)?.to_owned());
        existing = existing.parent().ok_or_else(denied)?.to_path_buf();
    }
    let mut path = existing.canonicalize()?;
    path.extend(rest.iter().rev());
    if !path.starts_with(root) || path == root || is_denied(root, &path) {
        return Err(denied());
    }
    Ok(path)
}

/// 项目根目录与玉玺查阅范围（均为 canonical 路径；互相包含的范围已合并），及暂存的改动。
struct CollectedTextFiles {
    files: BTreeMap<String, Option<PathBuf>>,
    staged: BTreeMap<String, Option<String>>,
    explicit_file: bool,
}

enum StagedEntry {
    Content(String),
    Deleted,
}

struct Sandbox {
    root: PathBuf,
    scope: Vec<PathBuf>,
    test_command: Option<String>,
    test_presets: BTreeMap<String, String>,
    /// 以相对路径为键。
    staged: Mutex<BTreeMap<String, FileChange>>,
}

impl Sandbox {
    fn new(
        root: &Path,
        scope: &[String],
        test_command: Option<&str>,
        test_presets: BTreeMap<String, String>,
    ) -> Result<Self, BoxError> {
        let root = root.canonicalize()?;
        let mut entries = Vec::new();
        for path in scope {
            // 范围须已存在；唯礼部要新建的 CHANGELOG.md 可尚不存在（按其最近的已存在祖先解析）。
            let resolved = match root.join(path).canonicalize() {
                Ok(resolved) => resolved,
                Err(e) if e.kind() == io::ErrorKind::NotFound && path == CHANGELOG => {
                    contained(&root, path)?
                }
                Err(e) => return Err(format!("查阅范围 {path} 无法访问: {e}").into()),
            };
            if !resolved.starts_with(&root) || is_denied(&root, &resolved) {
                return Err(format!("查阅范围越出项目或属于禁区: {path}").into());
            }
            entries.push(resolved);
        }
        // 排序后父目录在子路径之前，去掉已被覆盖的范围，避免搜索结果重复。
        entries.sort();
        let mut scope: Vec<PathBuf> = Vec::new();
        for entry in entries {
            if !scope.iter().any(|kept| entry.starts_with(kept)) {
                scope.push(entry);
            }
        }
        Ok(Self {
            root,
            scope,
            test_command: test_command.map(str::to_owned),
            test_presets,
            staged: Mutex::default(),
        })
    }

    /// 可写的路径（文件可尚不存在）及其相对路径；须在查阅范围内。
    fn resolve_write(&self, relative: &str) -> Result<(PathBuf, String), BoxError> {
        let path = contained(&self.root, relative)?;
        if !self.scope.iter().any(|s| path.starts_with(s)) {
            return Err(format!("拒绝访问玉玺查阅范围之外的路径: {relative}").into());
        }
        let key = self.display(&path);
        Ok((path, key))
    }

    fn staged_entry(&self, relative: &str) -> Option<StagedEntry> {
        let (_, key) = self.resolve_write(relative).ok()?;
        let staged = self.staged.lock().expect("暂存区锁未中毒");
        staged.get(&key).map(|c| {
            if c.deleted {
                StagedEntry::Deleted
            } else {
                StagedEntry::Content(c.after.clone())
            }
        })
    }

    fn resolve(&self, relative: &str) -> Result<PathBuf, BoxError> {
        let path = self.root.join(relative).canonicalize()?;
        if !self.scope.iter().any(|s| path.starts_with(s)) || is_denied(&self.root, &path) {
            return Err(format!("拒绝访问玉玺查阅范围之外的路径: {relative}").into());
        }
        Ok(path)
    }

    pub(super) fn scope_rel_paths(&self) -> Vec<String> {
        self.scope
            .iter()
            .map(|p| {
                let rel = self.display(p);
                if rel.is_empty() { ".".to_owned() } else { rel }
            })
            .collect()
    }

    pub(super) fn resolve_vcs_path(&self, relative: &str) -> Result<String, BoxError> {
        let path = contained(&self.root, relative)?;
        if !self.scope.iter().any(|s| path.starts_with(s)) {
            return Err(format!("拒绝访问玉玺查阅范围之外的路径: {relative}").into());
        }
        Ok(self.display(&path))
    }

    fn display(&self, path: &Path) -> String {
        match path.strip_prefix(&self.root) {
            Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
            // 交给模型的路径一律用 `/`，与 read_file、write_file 所收的一致（Windows 上不出 `\`）。
            Ok(rel) => rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
            Err(_) => path.display().to_string(),
        }
    }

    fn scope_label(&self) -> String {
        let labels: Vec<_> = self.scope.iter().map(|p| self.display(p)).collect();
        labels.join("、")
    }

    /// 收集 `path_arg`（或整个玉玺范围）内全部可读文本文件候选（磁盘 + 暂存新建，剔除暂存删除与构建产物）。
    fn collect_text_files(&self, path_arg: Option<&str>) -> Result<CollectedTextFiles, BoxError> {
        let staged_snapshot: BTreeMap<String, Option<String>> = {
            let staged = self.staged.lock().expect("暂存区锁未中毒");
            staged
                .iter()
                .map(|(k, v)| (k.clone(), (!v.deleted).then(|| v.after.clone())))
                .collect()
        };
        let (roots, explicit_file) = if let Some(relative) = path_arg {
            let trimmed = relative.trim();
            if trimmed.is_empty() || trimmed == "." {
                (self.scope.clone(), false)
            } else if let Ok((_, key)) = self.resolve_write(relative)
                && let Some(entry) = staged_snapshot.get(&key)
            {
                if under_skipped_rel(&key) {
                    return Err(format!("不搜索构建产物: {relative}").into());
                }
                let mut files = BTreeMap::new();
                if entry.is_some() {
                    files.insert(key, None);
                }
                return Ok(CollectedTextFiles {
                    files,
                    staged: staged_snapshot,
                    explicit_file: true,
                });
            } else {
                let start = self.resolve(relative)?;
                if under_skipped(&self.root, &start) {
                    return Err(format!("不搜索构建产物: {relative}").into());
                }
                let is_file = !start.is_dir();
                (vec![start], is_file)
            }
        } else {
            (self.scope.clone(), false)
        };

        let mut files: BTreeMap<String, Option<PathBuf>> = BTreeMap::new();
        for root in &roots {
            self.collect_symbol_files(root, &mut files)?;
        }
        let prefixes: Vec<String> = roots.iter().map(|d| self.display(d)).collect();
        for (rel, content) in &staged_snapshot {
            if under_skipped_rel(rel) {
                continue;
            }
            let under = prefixes
                .iter()
                .any(|p| p == "." || rel == p || rel.starts_with(&format!("{p}/")));
            if !under {
                continue;
            }
            if content.is_some() {
                files.entry(rel.clone()).or_insert(None);
            } else {
                files.remove(rel);
            }
        }
        Ok(CollectedTextFiles {
            files,
            staged: staged_snapshot,
            explicit_file,
        })
    }

    /// 列出 `path` 下的文件（相对路径），跳过构建产物与禁区，不进入符号链接。
    fn list(&self, path: &Path, out: &mut BTreeMap<String, u64>) -> io::Result<()> {
        if !path.is_dir() {
            out.insert(self.display(path), fs::metadata(path)?.len());
            return Ok(());
        }
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let (file_type, name) = (entry.file_type()?, entry.file_name());
            if is_denied(&self.root, &entry.path()) {
                continue;
            }
            if file_type.is_dir() && !SKIPPED_DIRS.iter().any(|d| name == *d) {
                self.list(&entry.path(), out)?;
            } else if file_type.is_file() {
                out.insert(self.display(&entry.path()), entry.metadata()?.len());
            }
        }
        Ok(())
    }

    /// 收集 `path` 下可扫描符号的文件（跳过禁区、构建产物与符号链接）。
    fn collect_symbol_files(
        &self,
        path: &Path,
        out: &mut BTreeMap<String, Option<PathBuf>>,
    ) -> io::Result<()> {
        if is_denied(&self.root, path) {
            return Ok(());
        }
        if !path.is_dir() {
            out.insert(self.display(path), Some(path.to_path_buf()));
            return Ok(());
        }
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let (file_type, name) = (entry.file_type()?, entry.file_name());
            let entry_path = entry.path();
            if is_denied(&self.root, &entry_path) {
                continue;
            }
            if file_type.is_dir() && !SKIPPED_DIRS.iter().any(|d| name == *d) {
                self.collect_symbol_files(&entry_path, out)?;
            } else if file_type.is_file() {
                out.insert(self.display(&entry_path), Some(entry_path));
            }
        }
        Ok(())
    }
}

/// 一页搜索结果。默认搜索在凑满一页后再遇到一条就停，以免改掉原来的截断语义。
struct MatchPage {
    offset: usize,
    limit: usize,
    stop_when_full: bool,
    total: usize,
    more: bool,
    hits: Vec<String>,
}

impl MatchPage {
    fn consider(&mut self, hit: String) -> bool {
        if self.stop_when_full {
            if self.hits.len() == self.limit {
                self.more = true;
                return true;
            }
            self.hits.push(hit);
            self.total += 1;
            return false;
        }
        if self.total >= self.offset && self.hits.len() < self.limit {
            self.hits.push(hit);
        }
        self.total += 1;
        false
    }
}

enum SearchMatcher {
    Literal { query: String, ignore_case: bool },
    Regex(regex::Regex),
}

impl SearchMatcher {
    fn is_match(&self, line: &str) -> bool {
        match self {
            Self::Literal { query, ignore_case } => {
                if *ignore_case {
                    line.to_lowercase().contains(query)
                } else {
                    line.contains(query.as_str())
                }
            }
            Self::Regex(re) => re.is_match(line),
        }
    }
}

fn under_skipped_rel(rel: &str) -> bool {
    rel.split('/').any(|seg| SKIPPED_DIRS.contains(&seg))
}

fn is_reference_candidate(rel: &str, explicit_target: bool) -> bool {
    if explicit_target {
        return true;
    }
    if let Some(lang) = symbols::Language::from_path(Path::new(rel)) {
        return lang != symbols::Language::Markdown;
    }
    let Some(ext) = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
    else {
        return false;
    };
    matches!(
        ext.as_str(),
        "java"
            | "kt"
            | "kts"
            | "cs"
            | "swift"
            | "rb"
            | "php"
            | "scala"
            | "zig"
            | "lua"
            | "sh"
            | "bash"
            | "zsh"
            | "ps1"
            | "sql"
            | "vue"
            | "svelte"
            | "css"
            | "scss"
            | "html"
            | "xml"
            | "toml"
            | "yaml"
            | "yml"
    )
}

/// 路径落在 `target` 或 `node_modules` 下。搜索不进入这些目录。
fn under_skipped(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .map(|rel| {
            rel.components().any(|c| {
                let name = c.as_os_str().to_string_lossy();
                SKIPPED_DIRS.contains(&name.as_ref())
            })
        })
        .unwrap_or(false)
}

/// `path` 相对 `root` 的任一组成部分是禁区目录或 `.env*`。
fn is_denied(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .any(|c| {
            let name = c.as_os_str().to_string_lossy();
            DENIED_DIRS.contains(&name.as_ref()) || name.starts_with(".env")
        })
}

struct ReadFile(Arc<Sandbox>);

impl Tool for ReadFile {
    fn is_read_only(&self) -> bool {
        true
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Capability::ReadFile.code().into(),
            description: format!(
                "读取项目内的文本文件（相对项目根目录的路径）。只能读取授权范围：{}。不给 offset、lines 与 end_line 时从开头读，超过 {MAX_READ_BYTES} 字节截断。给出 offset（从 1 起的行号）、lines（行数）或 end_line（结束行号，含，与 lines 互斥）时按行窗口读取，并说明总行数和后面是否还有。",
                self.0.scope_label()
            ),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"相对项目根目录的文件路径"},"offset":{"type":"integer","description":"从第几行开始读，从 1 起"},"lines":{"type":"integer","description":"最多读几行（与 end_line 互斥）"},"end_line":{"type":"integer","description":"读到第几行（含，从 1 起，与 lines 互斥；可直接使用 file_outline / find_symbols 返回的行号范围）"}},"required":["path"],"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(args_json, &["path", "offset", "lines", "end_line"])?;
        let relative = required_string(&args, "path")?;
        let offset = optional_i64(&args, "offset")?;
        let lines = optional_i64(&args, "lines")?;
        let end_line = optional_i64(&args, "end_line")?;
        if lines.is_some() && end_line.is_some() {
            return Err("lines 与 end_line 不能同时指定".into());
        }
        let effective_lines = match end_line {
            None => lines,
            Some(e) if e < 1 => return Err("end_line 从 1 起".into()),
            Some(e) => {
                let start = offset.unwrap_or(1);
                if start < 1 {
                    return Err("offset 从 1 起".into());
                }
                if e < start {
                    return Err(format!("end_line ({e}) 不能小于 offset ({start})").into());
                }
                let span = e
                    .checked_sub(start)
                    .and_then(|d| d.checked_add(1))
                    .ok_or("行号范围过大")?;
                Some(span)
            }
        };
        let bytes = match self.0.staged_entry(&relative) {
            Some(StagedEntry::Deleted) => return Err(format!("{relative} 已被暂存删除").into()),
            Some(StagedEntry::Content(text)) => text.into_bytes(),
            None => fs::read(self.0.resolve(&relative)?)?,
        };
        if offset.is_none() && effective_lines.is_none() {
            let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_READ_BYTES)]);
            return if bytes.len() > MAX_READ_BYTES {
                Ok(format!("{text}\n[已截断：文件共 {} 字节]", bytes.len()))
            } else {
                Ok(text.into_owned())
            };
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| "不是文本文件，不能按行读取")?;
        read_window(text, offset, effective_lines)
    }
}

struct ListFiles(Arc<Sandbox>);

impl Tool for ListFiles {
    fn is_read_only(&self) -> bool {
        true
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_files".into(),
            description: format!(
                "列出授权范围（{}）内某目录下的全部文件（递归，含本次 write_file 暂存的新文件），返回 `路径（字节数）`，最多 {MAX_LISTED} 个；不给 path 则列整个授权范围。可用 glob 过滤路径（如 `*.rs`、`crates/**/tests/*.rs`、`*.{{rs,toml}}`）。跳过 target、node_modules 与 Python 缓存目录。",
                self.0.scope_label()
            ),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"相对项目根目录的目录路径；省略则列整个授权范围"},"glob":{"type":"string","description":"可选：路径通配过滤，如 *.rs、crates/**/*.rs、*.{rs,toml}"}},"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(args_json, &["path", "glob"])?;
        let path_arg = optional_string(&args, "path")?;
        let glob_matcher = match optional_string(&args, "glob")? {
            Some(pat) if !pat.trim().is_empty() => Some(glob::GlobMatcher::compile(&pat)?),
            Some(_) => return Err("glob 不能为空".into()),
            None => None,
        };
        let dirs = match path_arg.as_deref() {
            Some(p) if !p.trim().is_empty() && p.trim() != "." => vec![self.0.resolve(p)?],
            _ => self.0.scope.clone(),
        };
        let mut files = BTreeMap::new();
        for dir in &dirs {
            self.0.list(dir, &mut files)?;
        }
        let prefixes: Vec<String> = dirs.iter().map(|d| self.0.display(d)).collect();
        let staged = self.0.staged.lock().expect("暂存区锁未中毒");
        let mut lines: Vec<String> = Vec::new();
        for (path, change) in staged.iter() {
            let under = prefixes
                .iter()
                .any(|p| p == "." || path == p || path.starts_with(&format!("{p}/")));
            if !under {
                continue;
            }
            if change.deleted {
                files.remove(path);
            } else if let Some(size) = files.get_mut(path) {
                *size = change.after.len() as u64;
            } else if glob_matcher.as_ref().is_none_or(|g| g.is_match(path)) {
                lines.push(format!("{path}（暂存新建，{} 字节）", change.after.len()));
            }
        }
        drop(staged);
        if let Some(matcher) = &glob_matcher {
            files.retain(|path, _| matcher.is_match(path));
        }
        let total = files.len() + lines.len();
        lines.extend(
            files
                .iter()
                .map(|(path, size)| format!("{path}（{size} 字节）")),
        );
        lines.sort();
        if lines.is_empty() {
            return Ok("无文件".into());
        }
        lines.truncate(MAX_LISTED);
        let mut out = lines.join("\n");
        if total > MAX_LISTED {
            out.push_str(&format!(
                "\n[已截断：共 {total} 个，仅显示前 {MAX_LISTED} 个；可指定子目录或 glob]"
            ));
        }
        Ok(out)
    }
}

struct SearchCode(Arc<Sandbox>);

impl Tool for SearchCode {
    fn is_read_only(&self) -> bool {
        true
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Capability::SearchCode.code().into(),
            description: format!(
                "在授权范围（{}）内的文本文件中搜索（含暂存新建／改动，跳过暂存删除），返回 `路径:行号: 内容`。默认区分大小写按字面量查，一页最多 {MAX_MATCHES} 条。可用 path 限定目录或文件、glob 过滤路径（如 `*.rs`、`crates/**/*.rs`）、regex 启用正则搜索、ignore_case 忽略大小写、context_lines（0–{MAX_CONTEXT_LINES}）附带上下邻行、offset 与 limit 翻页。先按授权范围和禁区过滤，不查 .git、.jj、.ziyuan、.env*、target、node_modules 与 Python 缓存。",
                self.0.scope_label()
            ),
            input_schema_json: format!(
                r#"{{"type":"object","properties":{{"query":{{"type":"string","description":"要查找的字面量文本，或 regex=true 时的正则表达式"}},"path":{{"type":"string","description":"只查这个相对目录或文件；省略则查整个授权范围"}},"glob":{{"type":"string","description":"可选：路径通配过滤，如 *.rs、crates/**/*.rs、*.{{rs,toml}}"}},"regex":{{"type":"boolean","description":"为 true 时按 Rust 正则表达式匹配"}},"ignore_case":{{"type":"boolean","description":"为 true 时忽略大小写"}},"context_lines":{{"type":"integer","description":"可选：每条匹配前后附带的上下文行数，0 到 {MAX_CONTEXT_LINES}"}},"offset":{{"type":"integer","description":"跳过前若干条匹配，从 0 起"}},"limit":{{"type":"integer","description":"本页条数，1 到 {MAX_MATCHES}"}}}},"required":["query"],"additionalProperties":false}}"#
            ),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(
            args_json,
            &[
                "query",
                "path",
                "glob",
                "regex",
                "ignore_case",
                "context_lines",
                "offset",
                "limit",
            ],
        )?;
        let query = required_string(&args, "query")?;
        if query.is_empty() {
            return Err("query 不能为空".into());
        }
        let use_regex = optional_bool(&args, "regex")?.unwrap_or(false);
        let ignore_case = optional_bool(&args, "ignore_case")?.unwrap_or(false);
        let context_lines = match optional_i64(&args, "context_lines")? {
            None => 0usize,
            Some(n) if (0..=MAX_CONTEXT_LINES as i64).contains(&n) => n as usize,
            Some(_) => {
                return Err(format!("context_lines 须在 0 到 {MAX_CONTEXT_LINES} 之间").into());
            }
        };
        let offset = optional_i64(&args, "offset")?.unwrap_or(0);
        if offset < 0 {
            return Err("offset 不能为负".into());
        }
        let limit = match optional_i64(&args, "limit")? {
            None => MAX_MATCHES as i64,
            Some(n) if (1..=MAX_MATCHES as i64).contains(&n) => n,
            Some(_) => return Err(format!("limit 须在 1 到 {MAX_MATCHES} 之间").into()),
        };
        let path = optional_string(&args, "path")?;
        let glob_matcher = match optional_string(&args, "glob")? {
            Some(pat) if !pat.trim().is_empty() => Some(glob::GlobMatcher::compile(&pat)?),
            Some(_) => return Err("glob 不能为空".into()),
            None => None,
        };
        let matcher = if use_regex {
            let compiled = regex::RegexBuilder::new(&query)
                .case_insensitive(ignore_case)
                .size_limit(1 << 20)
                .build()
                .map_err(|e| format!("正则表达式无效：{e}"))?;
            SearchMatcher::Regex(compiled)
        } else {
            SearchMatcher::Literal {
                query: if ignore_case {
                    query.to_lowercase()
                } else {
                    query
                },
                ignore_case,
            }
        };
        let enhanced = path.is_some()
            || glob_matcher.is_some()
            || use_regex
            || ignore_case
            || context_lines > 0
            || args.contains_key("offset")
            || args.contains_key("limit");
        let mut page = MatchPage {
            offset: usize::try_from(offset).unwrap_or(usize::MAX),
            limit: usize::try_from(limit).unwrap_or(MAX_MATCHES),
            stop_when_full: !enhanced,
            total: 0,
            more: false,
            hits: Vec::new(),
        };
        let CollectedTextFiles {
            files,
            staged: staged_snapshot,
            ..
        } = self.0.collect_text_files(path.as_deref())?;
        'outer: for (relative, disk_path) in files {
            if let Some(g) = &glob_matcher
                && !g.is_match(&relative)
            {
                continue;
            }
            let text = if let Some(entry) = staged_snapshot.get(&relative) {
                let Some(staged) = entry else {
                    continue;
                };
                staged.clone()
            } else if let Some(disk) = disk_path {
                match fs::read_to_string(&disk) {
                    Ok(text) => text,
                    Err(_) => continue,
                }
            } else {
                continue;
            };
            let lines: Vec<&str> = text.lines().collect();
            for (number, line) in lines.iter().enumerate() {
                if !matcher.is_match(line) {
                    continue;
                }
                let hit = if context_lines == 0 {
                    format!("{relative}:{}: {}", number + 1, line.trim())
                } else {
                    let from = number.saturating_sub(context_lines);
                    let to = (number + context_lines).min(lines.len().saturating_sub(1));
                    let mut block = Vec::with_capacity(to - from + 1);
                    for (idx, ctx_line) in lines.iter().enumerate().take(to + 1).skip(from) {
                        let sep = if idx == number { ':' } else { '-' };
                        block.push(format!(
                            "{relative}:{}{sep} {}",
                            idx + 1,
                            ctx_line.trim_end()
                        ));
                    }
                    block.join("\n")
                };
                if page.consider(hit) {
                    break 'outer;
                }
            }
        }
        Ok(format_matches(&page, enhanced, context_lines > 0))
    }
}

struct FileOutline(Arc<Sandbox>);

impl Tool for FileOutline {
    fn is_read_only(&self) -> bool {
        true
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "file_outline".into(),
            description: format!(
                "列出授权范围（{}）内单个代码或 Markdown 文件的符号大纲（函数、结构体、枚举、trait、impl、类、接口、常量、标题及起止行号，如 `42-98`）。支持 Rust、JS/TS、Python、Go、C/C++、Markdown；暂存改动优先于磁盘。可用 kind 按类别过滤，offset 与 limit 翻页（默认 {DEFAULT_OUTLINE_SYMBOLS} 条，最多 {MAX_OUTLINE_SYMBOLS} 条）。",
                self.0.scope_label()
            ),
            input_schema_json: format!(
                r#"{{"type":"object","properties":{{"path":{{"type":"string","description":"相对项目根目录的文件路径"}},"kind":{{"type":"string","description":"可选：只看某类符号，如 fn、struct、enum、trait、impl、class、interface、type、const、mod、macro、heading"}},"offset":{{"type":"integer","description":"跳过前若干个符号，从 0 起"}},"limit":{{"type":"integer","description":"本页条数，1 到 {MAX_OUTLINE_SYMBOLS}"}}}},"required":["path"],"additionalProperties":false}}"#
            ),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(args_json, &["path", "kind", "offset", "limit"])?;
        let relative = required_string(&args, "path")?;
        let kind = optional_string(&args, "kind")?
            .map(|k| k.trim().to_owned())
            .filter(|k| !k.is_empty());
        let offset = optional_i64(&args, "offset")?.unwrap_or(0);
        if offset < 0 {
            return Err("offset 不能为负".into());
        }
        let limit = match optional_i64(&args, "limit")? {
            None => DEFAULT_OUTLINE_SYMBOLS as i64,
            Some(n) if (1..=MAX_OUTLINE_SYMBOLS as i64).contains(&n) => n,
            Some(_) => return Err(format!("limit 须在 1 到 {MAX_OUTLINE_SYMBOLS} 之间").into()),
        };
        let (display_path, text) = match self.0.staged_entry(&relative) {
            Some(StagedEntry::Deleted) => return Err(format!("{relative} 已被暂存删除").into()),
            Some(StagedEntry::Content(staged)) => {
                let (_, key) = self.0.resolve_write(&relative)?;
                (key, staged)
            }
            None => {
                let resolved = self.0.resolve(&relative)?;
                if resolved.is_dir() {
                    return Err(
                        format!("{relative} 是目录，请指定单个文件或使用 find_symbols").into(),
                    );
                }
                let len = fs::metadata(&resolved)?.len();
                if len > MAX_OUTLINE_BYTES as u64 {
                    return Err(format!("文件过大（{len} 字节），请改用 read_file 按行读取").into());
                }
                let bytes = fs::read(&resolved)?;
                let text = String::from_utf8(bytes)
                    .map_err(|_| "不是 UTF-8 文本文件，无法提取符号大纲")?;
                (self.0.display(&resolved), text)
            }
        };
        let Some((lang, all)) = symbols::extract_for_path(Path::new(&display_path), &text) else {
            return Ok(format!(
                "{display_path}：暂不支持该文件类型的符号大纲（支持 Rust、JS/TS、Python、Go、C/C++、Markdown），可用 read_file 查看"
            ));
        };
        let filtered: Vec<_> = match kind.as_deref() {
            Some(filter) => all
                .into_iter()
                .filter(|sym| symbols::kind_matches(sym.kind, filter))
                .collect(),
            None => all,
        };
        if filtered.is_empty() {
            return Ok(format!(
                "{display_path}（{}）：未识别到匹配的符号",
                lang.label()
            ));
        }
        let total = filtered.len();
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(total);
        let lim = usize::try_from(limit).unwrap_or(DEFAULT_OUTLINE_SYMBOLS);
        let slice = &filtered[start..(start + lim).min(total)];
        if slice.is_empty() {
            return Ok(format!(
                "{display_path}（{}）：共 {total} 个符号，offset={start} 已超出范围",
                lang.label()
            ));
        }
        let mut out = format!(
            "{display_path}（{}，共 {total} 个符号，显示第 {}–{} 个）
",
            lang.label(),
            start + 1,
            start + slice.len()
        );
        for sym in slice {
            let indent = "  ".repeat(sym.depth.min(4));
            out.push_str(&format!(
                "{}: {indent}[{}] {} — {}
",
                sym.line_span(),
                sym.kind,
                sym.name,
                sym.signature
            ));
        }
        if start + slice.len() < total {
            out.push_str(&format!(
                "[还有 {} 个符号，可用 offset={} 翻页]",
                total - (start + slice.len()),
                start + slice.len()
            ));
        } else {
            out.pop();
        }
        Ok(out)
    }
}

struct FindSymbols(Arc<Sandbox>);

impl Tool for FindSymbols {
    fn is_read_only(&self) -> bool {
        true
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "find_symbols".into(),
            description: format!(
                "在授权范围（{}）内的代码与 Markdown 文件中按符号名查找定义（函数、结构体、枚举、trait、impl、类、接口、常量、标题），返回 `路径:起止行号: [类别] 限定名 — 签名`（单行只写行号，跨行写 `起-止`）。只匹配符号定义而非注释或普通引用；暂存改动优先于磁盘。可用 path 限目录或文件、kind 限类别、exact 精确匹配、ignore_case 忽略大小写、offset 与 limit 翻页（一页最多 {MAX_SYMBOL_MATCHES} 条）。先按授权范围和禁区过滤，跳过 target 与 node_modules。",
                self.0.scope_label()
            ),
            input_schema_json: format!(
                r#"{{"type":"object","properties":{{"query":{{"type":"string","description":"要查找的符号名或子串"}},"path":{{"type":"string","description":"可选：只查这个相对目录或文件；省略则查整个授权范围"}},"kind":{{"type":"string","description":"可选：只看某类符号，如 fn、struct、enum、trait、impl、class、interface、type、const、mod、macro、heading"}},"exact":{{"type":"boolean","description":"为 true 时要求符号短名完全一致"}},"ignore_case":{{"type":"boolean","description":"为 true 时忽略大小写"}},"offset":{{"type":"integer","description":"跳过前若干条匹配，从 0 起"}},"limit":{{"type":"integer","description":"本页条数，1 到 {MAX_SYMBOL_MATCHES}"}}}},"required":["query"],"additionalProperties":false}}"#
            ),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(
            args_json,
            &[
                "query",
                "path",
                "kind",
                "exact",
                "ignore_case",
                "offset",
                "limit",
            ],
        )?;
        let query = required_string(&args, "query")?;
        if query.trim().is_empty() {
            return Err("query 不能为空".into());
        }
        let kind = optional_string(&args, "kind")?
            .map(|k| k.trim().to_owned())
            .filter(|k| !k.is_empty());
        let exact = optional_bool(&args, "exact")?.unwrap_or(false);
        let ignore_case = optional_bool(&args, "ignore_case")?.unwrap_or(false);
        let offset = optional_i64(&args, "offset")?.unwrap_or(0);
        if offset < 0 {
            return Err("offset 不能为负".into());
        }
        let limit = match optional_i64(&args, "limit")? {
            None => MAX_SYMBOL_MATCHES as i64,
            Some(n) if (1..=MAX_SYMBOL_MATCHES as i64).contains(&n) => n,
            Some(_) => return Err(format!("limit 须在 1 到 {MAX_SYMBOL_MATCHES} 之间").into()),
        };
        let path = optional_string(&args, "path")?;
        let roots = if let Some(relative) = path.as_deref() {
            if relative.trim().is_empty() || relative.trim() == "." {
                self.0.scope.clone()
            } else {
                let start = self.0.resolve(relative)?;
                if under_skipped(&self.0.root, &start) {
                    return Err(format!("不搜索构建产物: {relative}").into());
                }
                vec![start]
            }
        } else {
            self.0.scope.clone()
        };

        // 收集范围内全部候选文件（磁盘 + 暂存新建），按相对路径有序遍历。
        let mut files: BTreeMap<String, Option<PathBuf>> = BTreeMap::new();
        for root in &roots {
            self.0.collect_symbol_files(root, &mut files)?;
        }
        let prefixes: Vec<String> = roots.iter().map(|d| self.0.display(d)).collect();
        let staged_snapshot: BTreeMap<String, Option<String>> = {
            let staged = self.0.staged.lock().expect("暂存区锁未中毒");
            staged
                .iter()
                .map(|(k, v)| (k.clone(), (!v.deleted).then(|| v.after.clone())))
                .collect()
        };
        for (rel, content) in &staged_snapshot {
            let under = prefixes
                .iter()
                .any(|p| p == "." || rel == p || rel.starts_with(&format!("{p}/")));
            if !under {
                continue;
            }
            if content.is_some() {
                files.entry(rel.clone()).or_insert(None);
            } else {
                files.remove(rel);
            }
        }

        let mut page = MatchPage {
            offset: usize::try_from(offset).unwrap_or(usize::MAX),
            limit: usize::try_from(limit).unwrap_or(MAX_SYMBOL_MATCHES),
            stop_when_full: false,
            total: 0,
            more: false,
            hits: Vec::new(),
        };
        let q_lower = query.to_lowercase();

        for (rel, disk_path) in files {
            if symbols::Language::from_path(Path::new(&rel)).is_none() {
                continue;
            }
            let text = if let Some(entry) = staged_snapshot.get(&rel) {
                let Some(staged) = entry else {
                    continue;
                };
                staged.clone()
            } else if let Some(disk) = disk_path {
                let Ok(meta) = fs::metadata(&disk) else {
                    continue;
                };
                if meta.len() > MAX_SYMBOL_FILE_BYTES {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&disk) else {
                    continue;
                };
                text
            } else {
                continue;
            };
            let Some((_, syms)) = symbols::extract_for_path(Path::new(&rel), &text) else {
                continue;
            };
            for sym in syms {
                if let Some(filter) = kind.as_deref()
                    && !symbols::kind_matches(sym.kind, filter)
                {
                    continue;
                }
                let qualified = sym.qualified_name();
                let matched = if exact {
                    if ignore_case {
                        sym.name.to_lowercase() == q_lower
                    } else {
                        sym.name == query
                    }
                } else if ignore_case {
                    sym.name.to_lowercase().contains(&q_lower)
                        || qualified.to_lowercase().contains(&q_lower)
                } else {
                    sym.name.contains(&query) || qualified.contains(&query)
                };
                if matched {
                    page.consider(format!(
                        "{rel}:{}: [{}] {} — {}",
                        sym.line_span(),
                        sym.kind,
                        qualified,
                        sym.signature
                    ));
                }
            }
        }

        if page.total == 0 {
            return Ok("未找到匹配的符号".into());
        }
        if page.hits.is_empty() {
            return Ok(format!(
                "共 {} 个符号，offset={} 已超出范围",
                page.total, page.offset
            ));
        }
        let from = page.offset + 1;
        let to = page.offset + page.hits.len();
        let mut out = page.hits.join(
            "
",
        );
        if page.total > page.hits.len() || page.offset > 0 {
            out.push_str(&format!(
                "
[共 {} 个符号，当前第 {from}–{to} 个]",
                page.total
            ));
        }
        Ok(out)
    }
}

struct FindReferences(Arc<Sandbox>);

impl Tool for FindReferences {
    fn is_read_only(&self) -> bool {
        true
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "find_references".into(),
            description: format!(
                "在授权范围（{}）内的源码文件中按整词查找符号的调用点与引用（跳过注释、字符串与更长标识符的子串；暂存改动优先于磁盘），区分 `[定义]` 与 `[引用]`。可用 path 限目录或文件、glob 过滤路径（如 `*.rs`）、include_definitions 控制是否包含定义行（默认 true）、offset 与 limit 翻页（一页最多 {MAX_REF_MATCHES} 条）。",
                self.0.scope_label()
            ),
            input_schema_json: format!(
                r#"{{"type":"object","properties":{{"symbol":{{"type":"string","description":"要查找的完整符号标识符（如 parse_test_presets、Seal）"}},"path":{{"type":"string","description":"可选：只查这个相对目录或文件；省略则查整个授权范围"}},"glob":{{"type":"string","description":"可选：路径通配过滤，如 *.rs、crates/**/*.rs"}},"include_definitions":{{"type":"boolean","description":"是否包含符号定义行；默认 true，设为 false 时只列调用／引用点"}},"offset":{{"type":"integer","description":"跳过前若干条结果，从 0 起"}},"limit":{{"type":"integer","description":"本页条数，1 到 {MAX_REF_MATCHES}"}}}},"required":["symbol"],"additionalProperties":false}}"#
            ),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(
            args_json,
            &[
                "symbol",
                "path",
                "glob",
                "include_definitions",
                "offset",
                "limit",
            ],
        )?;
        let symbol = required_string(&args, "symbol")?.trim().to_owned();
        if !symbols::is_valid_symbol_name(&symbol) {
            return Err("symbol 须为 1–120 字节的单个标识符（不含空格或标点）".into());
        }
        let include_definitions = optional_bool(&args, "include_definitions")?.unwrap_or(true);
        let offset = optional_i64(&args, "offset")?.unwrap_or(0);
        if offset < 0 {
            return Err("offset 不能为负".into());
        }
        let limit = match optional_i64(&args, "limit")? {
            None => MAX_REF_MATCHES as i64,
            Some(n) if (1..=MAX_REF_MATCHES as i64).contains(&n) => n,
            Some(_) => return Err(format!("limit 须在 1 到 {MAX_REF_MATCHES} 之间").into()),
        };
        let path = optional_string(&args, "path")?;
        let glob_matcher = match optional_string(&args, "glob")? {
            Some(pat) if !pat.trim().is_empty() => Some(glob::GlobMatcher::compile(&pat)?),
            Some(_) => return Err("glob 不能为空".into()),
            None => None,
        };
        let CollectedTextFiles {
            files,
            staged: staged_snapshot,
            explicit_file,
        } = self.0.collect_text_files(path.as_deref())?;
        let explicit_target = explicit_file || glob_matcher.is_some();

        let mut page = MatchPage {
            offset: usize::try_from(offset).unwrap_or(usize::MAX),
            limit: usize::try_from(limit).unwrap_or(MAX_REF_MATCHES),
            stop_when_full: false,
            total: 0,
            more: false,
            hits: Vec::new(),
        };
        let mut def_count = 0usize;

        for (rel, disk_path) in files {
            if let Some(g) = &glob_matcher
                && !g.is_match(&rel)
            {
                continue;
            }
            if !is_reference_candidate(&rel, explicit_target) {
                continue;
            }
            let text = if let Some(entry) = staged_snapshot.get(&rel) {
                let Some(staged) = entry else {
                    continue;
                };
                staged.clone()
            } else if let Some(disk) = disk_path {
                let Ok(meta) = fs::metadata(&disk) else {
                    continue;
                };
                if meta.len() > MAX_SYMBOL_FILE_BYTES {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&disk) else {
                    continue;
                };
                text
            } else {
                continue;
            };
            if !text.contains(&symbol) {
                continue;
            }
            let rel_path = Path::new(&rel);
            let def_lines: BTreeSet<usize> = symbols::extract_for_path(rel_path, &text)
                .map(|(_, syms)| {
                    syms.into_iter()
                        .filter(|s| s.name == symbol)
                        .map(|s| s.line)
                        .collect()
                })
                .unwrap_or_default();
            let stripped = symbols::strip_code_lines(rel_path, &text);
            for (idx, (raw_line, code_line)) in text.lines().zip(stripped.iter()).enumerate() {
                if !symbols::has_whole_ident(code_line, &symbol) {
                    continue;
                }
                let line_no = idx + 1;
                let is_def = def_lines.contains(&line_no);
                if is_def && !include_definitions {
                    continue;
                }
                if is_def {
                    def_count += 1;
                }
                let tag = if is_def { "定义" } else { "引用" };
                page.consider(format!("{rel}:{line_no} [{tag}]: {}", raw_line.trim()));
            }
        }

        if page.total == 0 {
            return Ok(format!("未找到符号 `{symbol}` 的引用"));
        }
        if page.hits.is_empty() {
            return Ok(format!(
                "共 {} 处匹配，offset={} 已超出范围",
                page.total, page.offset
            ));
        }
        let from = page.offset + 1;
        let to = page.offset + page.hits.len();
        let mut out = page.hits.join("\n");
        let summary = if def_count > 0 {
            format!(
                "共 {} 处匹配（含 {def_count} 处定义），当前第 {from}–{to} 处",
                page.total
            )
        } else {
            format!("共 {} 处引用，当前第 {from}–{to} 处", page.total)
        };
        out.push_str(&format!("\n[{summary}]"));
        Ok(out)
    }
}

struct WriteFile(Arc<Sandbox>);

impl Tool for WriteFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Capability::WriteFile.code().into(),
            description: format!(
                "以 content 整体改写（或新建）授权范围（{}）内的文本文件。改动只暂存，诏令完成后呈报皇帝，准奏后才写入项目；暂存后 read_file 读到的是新内容。最多 {MAX_WRITE_BYTES} 字节。",
                self.0.scope_label()
            ),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"相对项目根目录的文件路径"},"content":{"type":"string","description":"文件的完整新内容"}},"required":["path","content"],"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let content = string_arg(args_json, "content")?;
        if content.len() > MAX_WRITE_BYTES {
            return Err(format!("内容超过 {MAX_WRITE_BYTES} 字节").into());
        }
        let (path, key) = self.0.resolve_write(&string_arg(args_json, "path")?)?;
        if path.is_dir() {
            return Err(format!("{key} 是目录").into());
        }
        let mut staged = self.0.staged.lock().expect("暂存区锁未中毒");
        let before = match staged.get(&key) {
            Some(change) => change.before.clone(),
            None if path.exists() => Some(
                String::from_utf8(fs::read(&path)?).map_err(|_| format!("{key} 不是文本文件"))?,
            ),
            None => None,
        };
        let bytes = content.len();
        staged.insert(
            key.clone(),
            FileChange {
                path: key.clone(),
                before,
                after: content,
                deleted: false,
            },
        );
        Ok(format!(
            "已暂存 {key}（{bytes} 字节）；诏令完成后呈报皇帝，准奏后才写入项目"
        ))
    }
}

struct ReplaceText(Arc<Sandbox>);

struct ReplaceEdit {
    old: String,
    new: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

impl Tool for ReplaceText {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "replace_text".into(),
            description: format!(
                "把授权范围（{}）内某个文本文件里唯一的一段 old 换成 new，或通过 edits 数组在单次调用内按顺序原子应用多处补丁（最多 {MAX_REPLACE_EDITS} 处；任一处找不到或出现不止一处即整批拒绝，不留半截改动）。支持可选的 start_line/end_line（1 起始含首尾行号）将匹配限定在指定行范围以消除歧义；匹配失败时会返回候选行号或歧义行号便于定位。不做模糊替换，也不新建或删除文件。改动只暂存，与 write_file 一样等准奏后才落盘。结果最多 {MAX_WRITE_BYTES} 字节。",
                self.0.scope_label()
            ),
            input_schema_json: format!(
                r#"{{"type":"object","properties":{{"path":{{"type":"string","description":"相对项目根目录的文件路径"}},"old":{{"type":"string","description":"单处替换时的原文，须在文件（或指定行范围）中恰好出现一次（与 edits 二选一）"}},"new":{{"type":"string","description":"单处替换后的文本，可以是空字符串（与 edits 二选一）"}},"start_line":{{"type":"integer","description":"可选，单处替换时限定的起始行号（从 1 起，含该行；须与 old/new 同用）"}},"end_line":{{"type":"integer","description":"可选，单处替换时限定的结束行号（从 1 起，含该行；须与 old/new 同用）"}},"edits":{{"type":"array","description":"批量补丁数组（最多 {MAX_REPLACE_EDITS} 项，与 old/new 二选一），按顺序基于当前文本原子应用","items":{{"type":"object","properties":{{"old":{{"type":"string","description":"该处要替换的原文，须在文件或行范围内唯一匹配"}},"new":{{"type":"string","description":"替换后的文本，可以是空字符串"}},"start_line":{{"type":"integer","description":"可选，该处补丁限定的起始行号（从 1 起，含该行）"}},"end_line":{{"type":"integer","description":"可选，该处补丁限定的结束行号（从 1 起，含该行）"}}}},"required":["old","new"],"additionalProperties":false}}}}}},"required":["path"],"additionalProperties":false}}"#
            ),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(
            args_json,
            &["path", "old", "new", "start_line", "end_line", "edits"],
        )?;
        let relative = required_string(&args, "path")?;
        let has_edits = args.contains_key("edits");
        let has_single = args.contains_key("old")
            || args.contains_key("new")
            || args.contains_key("start_line")
            || args.contains_key("end_line");
        let (batch_mode, edits): (bool, Vec<ReplaceEdit>) = if has_edits {
            if has_single {
                return Err("edits 与 old/new/start_line/end_line 不能同时传入".into());
            }
            let arr = args
                .get("edits")
                .and_then(Value::as_array)
                .ok_or("edits 须为对象数组")?;
            if arr.is_empty() {
                return Err("edits 不能为空".into());
            }
            if arr.len() > MAX_REPLACE_EDITS {
                return Err(format!("edits 最多支持 {MAX_REPLACE_EDITS} 处补丁").into());
            }
            let mut parsed = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let obj = item
                    .as_object()
                    .ok_or_else(|| format!("第 {} 处补丁须为 JSON 对象", i + 1))?;
                for key in obj.keys() {
                    if !matches!(key.as_str(), "old" | "new" | "start_line" | "end_line") {
                        return Err(format!("第 {} 处补丁含未知字段：{key}", i + 1).into());
                    }
                }
                let old = obj
                    .get("old")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("第 {} 处补丁缺少字符串参数 old", i + 1))?;
                let new = obj
                    .get("new")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("第 {} 处补丁缺少字符串参数 new", i + 1))?;
                let start_line = parse_optional_line(obj, "start_line")
                    .map_err(|e| format!("第 {} 处补丁：{e}", i + 1))?;
                let end_line = parse_optional_line(obj, "end_line")
                    .map_err(|e| format!("第 {} 处补丁：{e}", i + 1))?;
                parsed.push(ReplaceEdit {
                    old: old.to_owned(),
                    new: new.to_owned(),
                    start_line,
                    end_line,
                });
            }
            (true, parsed)
        } else {
            let old = required_string(&args, "old")?;
            let new = required_string(&args, "new")?;
            let start_line = parse_optional_line(&args, "start_line")?;
            let end_line = parse_optional_line(&args, "end_line")?;
            (
                false,
                vec![ReplaceEdit {
                    old,
                    new,
                    start_line,
                    end_line,
                }],
            )
        };
        // 路径先过玉玺和禁区。空 old、新旧相同都不能让越界路径报成功。
        let (path, key) = self.0.resolve_write(&relative)?;
        if path.is_dir() {
            return Err(format!("{key} 是目录").into());
        }
        for (i, edit) in edits.iter().enumerate() {
            if edit.old.is_empty() {
                return Err(if batch_mode {
                    format!("第 {} 处补丁的 old 不能为空", i + 1).into()
                } else {
                    "old 不能为空".into()
                });
            }
            if let (Some(start), Some(end)) = (edit.start_line, edit.end_line)
                && start > end
            {
                let msg = format!("start_line ({start}) 不能大于 end_line ({end})");
                return Err(if batch_mode {
                    format!("第 {} 处补丁：{msg}", i + 1).into()
                } else {
                    msg.into()
                });
            }
        }
        if !batch_mode
            && edits[0].start_line.is_none()
            && edits[0].end_line.is_none()
            && edits[0].old == edits[0].new
        {
            return Ok("新旧文本相同，未写入暂存".into());
        }
        let mut staged = self.0.staged.lock().expect("暂存区锁未中毒");
        let (before, current) = if let Some(change) = staged.get(&key) {
            if change.deleted {
                return Err(format!("{key} 已被暂存删除，不能替换").into());
            }
            (change.before.clone(), change.after.clone())
        } else if path.exists() {
            let text =
                String::from_utf8(fs::read(&path)?).map_err(|_| format!("{key} 不是文本文件"))?;
            (Some(text.clone()), text)
        } else {
            return Err(format!("{key} 不存在，不能替换").into());
        };
        let mut working = current.clone();
        let mut old_bytes = 0usize;
        let mut new_bytes = 0usize;
        for (i, edit) in edits.iter().enumerate() {
            let spans = line_spans(&working);
            let win = resolve_line_window(&spans, working.len(), edit.start_line, edit.end_line)
                .map_err(|e| {
                    if batch_mode {
                        BoxError::from(format!("第 {} 处补丁：{e}", i + 1))
                    } else {
                        e
                    }
                })?;
            let window = &working[win.start..win.end];
            let matches: Vec<usize> = window
                .match_indices(&edit.old)
                .map(|(pos, _)| pos)
                .collect();
            if matches.is_empty() {
                let detail = format_missing_match(
                    &working, &spans, win.start, win.end, &edit.old, win.range,
                );
                return Err(if batch_mode {
                    format!("第 {} 处补丁：{detail}", i + 1).into()
                } else {
                    detail.into()
                });
            }
            if matches.len() > 1 {
                let detail = format_ambiguous_matches(&spans, win.start, &matches, win.range);
                return Err(if batch_mode {
                    format!("第 {} 处补丁：{detail}", i + 1).into()
                } else {
                    detail.into()
                });
            }
            let abs_start = win.start + matches[0];
            let abs_end = abs_start + edit.old.len();
            working.replace_range(abs_start..abs_end, &edit.new);
            old_bytes += edit.old.len();
            new_bytes += edit.new.len();
        }
        if working.len() > MAX_WRITE_BYTES {
            return Err(format!("替换后超过 {MAX_WRITE_BYTES} 字节").into());
        }
        if working == current {
            return Ok("新旧文本相同，未写入暂存".into());
        }
        staged.insert(
            key.clone(),
            FileChange {
                path: key.clone(),
                before,
                after: working,
                deleted: false,
            },
        );
        Ok(format!(
            "已暂存 {key}：替换 {} 处（{old_bytes} 字节 → {new_bytes} 字节）；诏令完成后呈报皇帝，准奏后才写入项目",
            edits.len(),
        ))
    }
}

fn parse_optional_line(
    args: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<usize>, BoxError> {
    match optional_i64(args, key)? {
        None => Ok(None),
        Some(n) if n < 1 => Err(format!("`{key}` 从 1 起").into()),
        Some(n) => usize::try_from(n)
            .map(Some)
            .map_err(|_| format!("`{key}` 过大").into()),
    }
}

struct LineWindow {
    start: usize,
    end: usize,
    range: Option<(usize, usize)>,
}

fn resolve_line_window(
    spans: &[(usize, usize)],
    text_len: usize,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<LineWindow, BoxError> {
    if start_line.is_none() && end_line.is_none() {
        return Ok(LineWindow {
            start: 0,
            end: text_len,
            range: None,
        });
    }
    if spans.is_empty() {
        return Err("文件为空，无法按行范围替换".into());
    }
    let total = spans.len();
    let start = start_line.unwrap_or(1);
    let end = end_line.unwrap_or(total);
    if start > end {
        return Err(format!("start_line ({start}) 不能大于 end_line ({end})").into());
    }
    if start > total || end > total {
        return Err(format!("行范围 {start}..={end} 超出文件总行数 {total}").into());
    }
    Ok(LineWindow {
        start: spans[start - 1].0,
        end: spans[end - 1].1,
        range: Some((start, end)),
    })
}

fn byte_offset_to_line(spans: &[(usize, usize)], offset: usize) -> usize {
    if spans.is_empty() {
        return 1;
    }
    (spans.partition_point(|&(_, end)| end <= offset) + 1).min(spans.len())
}

fn format_line_list(lines: &[usize]) -> String {
    let preview: Vec<String> = lines.iter().take(5).map(usize::to_string).collect();
    if lines.len() > 5 {
        format!("第 {} 行等共 {} 处", preview.join("、"), lines.len())
    } else {
        format!("第 {} 行", preview.join("、"))
    }
}

fn format_ambiguous_matches(
    spans: &[(usize, usize)],
    win_start: usize,
    matches: &[usize],
    range: Option<(usize, usize)>,
) -> String {
    let lines: Vec<usize> = matches
        .iter()
        .map(|&rel| byte_offset_to_line(spans, win_start + rel))
        .collect();
    let count = matches.len();
    let lines_desc = format_line_list(&lines);
    match range {
        Some((start, end)) => format!(
            "在指定行范围 {start}..={end} 内匹配到 {count} 处（{lines_desc}），须唯一才能替换；请缩小 start_line/end_line 或补充前后上下文"
        ),
        None => format!(
            "匹配到 {count} 处（{lines_desc}），须唯一才能替换；可补充前后上下文，或通过 start_line/end_line 指定行范围唯一锚定"
        ),
    }
}

fn format_missing_match(
    working: &str,
    spans: &[(usize, usize)],
    win_start: usize,
    win_end: usize,
    old: &str,
    range: Option<(usize, usize)>,
) -> String {
    if let Some((start, end)) = range {
        let anywhere: Vec<usize> = working
            .match_indices(old)
            .map(|(pos, _)| byte_offset_to_line(spans, pos))
            .collect();
        if !anywhere.is_empty() {
            return format!(
                "在指定行范围 {start}..={end} 内找不到要替换的文本（但文件其他位置{}存在完全匹配，请核对行号）",
                format_line_list(&anywhere)
            );
        }
    }
    let base = match range {
        Some((start, end)) => format!("在指定行范围 {start}..={end} 内找不到要替换的文本"),
        None => "找不到要替换的文本".to_owned(),
    };
    let candidates = find_candidate_lines(working, spans, win_start, win_end, old);
    if candidates.is_empty() {
        return base;
    }
    let formatted: Vec<String> = candidates
        .into_iter()
        .map(|(line_no, snippet)| format!("  L{line_no}: {snippet}"))
        .collect();
    format!("{base}。最接近的候选行：\n{}", formatted.join("\n"))
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extract_tokens(text: &str) -> Vec<&str> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|tok| tok.len() >= 2)
        .collect()
}

fn find_candidate_lines(
    working: &str,
    spans: &[(usize, usize)],
    win_start: usize,
    win_end: usize,
    old: &str,
) -> Vec<(usize, String)> {
    let anchor = old
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_else(|| old.trim());
    if anchor.is_empty() {
        return Vec::new();
    }
    let anchor_ws = collapse_whitespace(anchor);
    let anchor_tokens = extract_tokens(anchor);
    let mut scored: Vec<(u32, usize, String)> = Vec::new();
    for (idx, &(s, e)) in spans.iter().enumerate() {
        if e <= win_start || s >= win_end {
            continue;
        }
        let raw = working[s..e].trim_end_matches(['\r', '\n']);
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let score = if trimmed == anchor {
            1000
        } else if !anchor_ws.is_empty() && collapse_whitespace(trimmed) == anchor_ws {
            850
        } else if (anchor.len() >= 4 && trimmed.contains(anchor))
            || (trimmed.len() >= 4 && anchor.contains(trimmed))
        {
            650
        } else if anchor_tokens.len() >= 2 {
            let line_tokens = extract_tokens(trimmed);
            let matched = anchor_tokens
                .iter()
                .filter(|tok| line_tokens.contains(tok))
                .count();
            if matched >= 2 && matched * 2 >= anchor_tokens.len() {
                200 + ((matched as u32 * 300) / anchor_tokens.len() as u32)
            } else {
                0
            }
        } else {
            0
        };
        if score > 0 {
            let preview = clip_str(raw, 100);
            let suffix = if preview.len() < raw.len() { "…" } else { "" };
            scored.push((score, idx + 1, format!("{preview}{suffix}")));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(3)
        .map(|(_, line_no, preview)| (line_no, preview))
        .collect()
}

struct DeleteFile(Arc<Sandbox>);

impl Tool for DeleteFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "delete_file".into(),
            description: format!(
                "暂存删除授权范围（{}）内的单个文本文件（不能删目录、二进制文件或超过 {MAX_WRITE_BYTES} 字节的文件）。改动只暂存，诏令完成后呈报皇帝，准奏后才从项目删除。",
                self.0.scope_label()
            ),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"相对项目根目录的文件路径"}},"required":["path"],"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(args_json, &["path"])?;
        let relative = required_string(&args, "path")?;
        let (path, key) = self.0.resolve_write(&relative)?;
        if path.is_dir() {
            return Err(format!("{key} 是目录，不能删除").into());
        }
        let mut staged = self.0.staged.lock().expect("暂存区锁未中毒");
        if let Some(existing) = staged.get(&key) {
            if existing.deleted {
                return Ok(format!("{key} 已在暂存中标记删除"));
            }
            if existing.before.is_none() {
                staged.remove(&key);
                return Ok(format!("已取消暂存新建的 {key}"));
            }
            let before = existing.before.clone();
            staged.insert(
                key.clone(),
                FileChange {
                    path: key.clone(),
                    before,
                    after: String::new(),
                    deleted: true,
                },
            );
            return Ok(format!(
                "已暂存删除 {key}；诏令完成后呈报皇帝，准奏后才从项目删除"
            ));
        }
        if !path.exists() {
            return Err(format!("{key} 不存在，不能删除").into());
        }
        let bytes = fs::read(&path)?;
        if bytes.len() > MAX_WRITE_BYTES {
            return Err(format!("{key} 超过 {MAX_WRITE_BYTES} 字节，暂不支持暂存删除").into());
        }
        let text = String::from_utf8(bytes).map_err(|_| format!("{key} 不是文本文件，不能删除"))?;
        staged.insert(
            key.clone(),
            FileChange {
                path: key.clone(),
                before: Some(text),
                after: String::new(),
                deleted: true,
            },
        );
        Ok(format!(
            "已暂存删除 {key}；诏令完成后呈报皇帝，准奏后才从项目删除"
        ))
    }
}

struct RenameFile(Arc<Sandbox>);

impl Tool for RenameFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "rename_file".into(),
            description: format!(
                "暂存把授权范围（{}）内的单个文本文件从 from 改名或移动到 to（等价于原子暂存删除旧路径并新建目标路径；不允许覆盖已有文件，不支持目录与二进制文件，上限 {MAX_WRITE_BYTES} 字节）。改动只暂存，准奏后才落盘。",
                self.0.scope_label()
            ),
            input_schema_json: r#"{"type":"object","properties":{"from":{"type":"string","description":"原相对文件路径"},"to":{"type":"string","description":"目标相对文件路径"}},"required":["from","to"],"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(args_json, &["from", "to"])?;
        let from_rel = required_string(&args, "from")?;
        let to_rel = required_string(&args, "to")?;
        let (from_path, from_key) = self.0.resolve_write(&from_rel)?;
        let (to_path, to_key) = self.0.resolve_write(&to_rel)?;
        if from_key == to_key {
            return Ok("源路径与目标路径相同，未写入暂存".into());
        }
        if from_path.is_dir() {
            return Err(format!("{from_key} 是目录，不能改名").into());
        }
        if to_path.is_dir() {
            return Err(format!("{to_key} 是目录，不能作为目标路径").into());
        }
        let mut staged = self.0.staged.lock().expect("暂存区锁未中毒");
        let (from_before, content) = if let Some(change) = staged.get(&from_key) {
            if change.deleted {
                return Err(format!("{from_key} 已被暂存删除，不能改名").into());
            }
            (change.before.clone(), change.after.clone())
        } else if from_path.exists() {
            let bytes = fs::read(&from_path)?;
            if bytes.len() > MAX_WRITE_BYTES {
                return Err(
                    format!("{from_key} 超过 {MAX_WRITE_BYTES} 字节，暂不支持暂存改名").into(),
                );
            }
            let text = String::from_utf8(bytes)
                .map_err(|_| format!("{from_key} 不是文本文件，不能改名"))?;
            (Some(text.clone()), text)
        } else {
            return Err(format!("{from_key} 不存在，不能改名").into());
        };
        if content.len() > MAX_WRITE_BYTES {
            return Err(format!("改名内容超过 {MAX_WRITE_BYTES} 字节").into());
        }
        let to_exists = match staged.get(&to_key) {
            Some(c) => !c.deleted,
            None => to_path.exists(),
        };
        if to_exists {
            return Err(format!("{to_key} 已存在，拒绝覆盖改名").into());
        }
        let to_before = staged.get(&to_key).and_then(|c| c.before.clone());
        match from_before {
            Some(orig) => {
                staged.insert(
                    from_key.clone(),
                    FileChange {
                        path: from_key.clone(),
                        before: Some(orig),
                        after: String::new(),
                        deleted: true,
                    },
                );
            }
            None => {
                staged.remove(&from_key);
            }
        }
        if to_before.as_deref() == Some(content.as_str()) {
            staged.remove(&to_key);
        } else {
            staged.insert(
                to_key.clone(),
                FileChange {
                    path: to_key.clone(),
                    before: to_before,
                    after: content,
                    deleted: false,
                },
            );
        }
        Ok(format!(
            "已暂存改名 {from_key} → {to_key}；诏令完成后呈报皇帝，准奏后才落盘"
        ))
    }
}

struct RunTests(Arc<Sandbox>);

impl Tool for RunTests {
    fn spec(&self) -> ToolSpec {
        let default_cmd = self
            .resolve_command(None)
            .ok()
            .unwrap_or("（皇帝尚未设定）");
        let presets_note = if self.0.test_presets.is_empty() {
            String::new()
        } else {
            let list = self
                .0
                .test_presets
                .iter()
                .map(|(k, v)| format!("{k}=`{v}`"))
                .collect::<Vec<_>>()
                .join("、");
            format!("另可通过可选参数 `preset` 选用皇帝白名单中的预设命令：{list}。")
        };
        ToolSpec {
            name: Capability::RunTests.code().into(),
            description: format!(
                "在项目的临时副本（含已暂存的改动）中运行皇帝设定的检查／测试命令（默认 `{default_cmd}`），返回退出码与输出末尾（未通过时作为工具错误返回）。{presets_note}命令由皇帝预先定好，模型不能改写或拼接参数。若输出是 cargo test、pytest、node --test、go test、jest/vitest 或 rustc/tsc/mypy 编译诊断且未通过，另附结构化失败／诊断摘录；其他格式只给输出末尾。"
            ),
            input_schema_json: r#"{"type":"object","properties":{"preset":{"type":"string","description":"可选，从皇帝 ZIYUAN_TEST_COMMANDS 白名单中选用的预设名（如 check、clippy、test）；留空运行默认命令"}},"additionalProperties":false}"#
                .into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        self.execute_with_cancel(args_json, None)
    }

    fn execute_with_cancel(
        &self,
        args_json: &str,
        cancel: Option<&AtomicBool>,
    ) -> Result<String, BoxError> {
        let args = object_args(args_json, &["preset"])?;
        let preset = optional_string(&args, "preset")?;
        self.run_preset(preset.as_deref(), crate::haoshe::Mode::from_env()?, cancel)
    }
}

impl RunTests {
    fn resolve_command(&self, preset: Option<&str>) -> Result<&str, BoxError> {
        if let Some(name) = preset {
            let name = name.trim();
            if name.is_empty() {
                return Err("preset 不能为空字符串（不指定预设请省略该字段）".into());
            }
            if let Some(cmd) = self.0.test_presets.get(name) {
                return Ok(cmd.as_str());
            }
            if self.0.test_presets.is_empty() {
                return Err(format!(
                    "皇帝尚未通过 ZIYUAN_TEST_COMMANDS 设定命名预设，不能使用 preset `{name}`"
                )
                .into());
            }
            let allowed = self
                .0
                .test_presets
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join("、");
            return Err(format!("未知测试预设 `{name}`；皇帝允许的预设仅限：{allowed}").into());
        }
        if let Some(cmd) = self.0.test_command.as_deref() {
            return Ok(cmd);
        }
        if let Some(cmd) = self.0.test_presets.get("default") {
            return Ok(cmd.as_str());
        }
        if let Some((_, cmd)) = self.0.test_presets.iter().next() {
            return Ok(cmd.as_str());
        }
        Err("皇帝尚未设定测试命令（ZIYUAN_TEST_COMMAND 或 ZIYUAN_TEST_COMMANDS）".into())
    }

    #[cfg(test)]
    fn run(&self, mode: crate::haoshe::Mode) -> Result<String, BoxError> {
        self.run_preset(None, mode, None)
    }

    fn run_preset(
        &self,
        preset: Option<&str>,
        mode: crate::haoshe::Mode,
        cancel: Option<&AtomicBool>,
    ) -> Result<String, BoxError> {
        let command = self.resolve_command(preset)?;
        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            return Err(format!("测试 `{command}`：已取消（启动前已收到撤销信号）").into());
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "ziyuan-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let copy = base.join("project");
        let result = self.run_in_copy(command, mode, &base, &copy, cancel);
        let _ = fs::remove_dir_all(&base);
        result
    }

    fn run_in_copy(
        &self,
        command: &str,
        mode: crate::haoshe::Mode,
        base: &Path,
        copy: &Path,
        cancel: Option<&AtomicBool>,
    ) -> Result<String, BoxError> {
        copy_tree(&self.0.root, &self.0.root, copy)?;
        for change in self.0.staged.lock().expect("暂存区锁未中毒").values() {
            let target = copy.join(&change.path);
            if change.deleted {
                match fs::remove_file(&target) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
                continue;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(target, &change.after)?;
        }
        let target_dir: Vec<PathBuf> = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .into_iter()
            .collect();
        let env: Vec<(String, std::ffi::OsString)> = TEST_ENV
            .iter()
            .filter_map(|key| std::env::var_os(key).map(|v| ((*key).to_owned(), v)))
            .collect();
        let hidden = [self.0.root.join(".ziyuan")];
        let toolchains = crate::haoshe::toolchain_dirs_from_env();
        let run = crate::haoshe::run_shell(&crate::haoshe::ShellRunSpec {
            command,
            mode,
            network: crate::haoshe::Network::from_env()?,
            work_dir: copy,
            sandbox_base: base,
            extra_writable: &target_dir,
            readonly_toolchains: &toolchains,
            hidden: &hidden,
            env: &env,
            timeout: TEST_TIMEOUT,
            max_output: MAX_TEST_OUTPUT,
            max_failure_scan: MAX_FAILURE_SCAN,
            cancel,
        })?;
        let note = if run.tail_truncated {
            format!("（仅最后 {MAX_TEST_OUTPUT} 字节）")
        } else {
            String::new()
        };
        let mut report = format!(
            "测试 `{command}`：{}（{}）\n输出{note}：\n{}",
            run.verdict, run.haoshe_note, run.tail
        );
        if !run.passed
            && let Some(scan) = run.failure_scan.as_deref()
            && let Some(failures) = failures::failure_note(scan)
        {
            report.push('\n');
            report.push_str(&failures);
        }
        // 未通过记为工具失败：审计中 `run_tests` 的成败即测试的成败，供都察院核验。
        if run.passed {
            Ok(report)
        } else {
            Err(report.into())
        }
    }
}

/// 将项目根目录复制到任务私有目录，跳过禁区（`.ziyuan`、`.git`、`.jj`、`.env*`）、构建产物与符号链接／重解析点。
#[cfg(windows)]
pub(crate) fn copy_project_tree(root: &Path, dest: &Path) -> io::Result<()> {
    copy_tree(root, root, dest)
}

/// 复制 `from` 到 `to`：跳过禁区、构建产物与符号链接／重解析点。
fn copy_tree(root: &Path, from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if is_denied(root, &path) || SKIPPED_DIRS.iter().any(|d| name == *d) {
            continue;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
            if fs::symlink_metadata(&path)
                .map(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
                .unwrap_or(true)
            {
                continue;
            }
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_tree(root, &path, &to.join(&name))?;
        } else if file_type.is_file() {
            fs::copy(&path, to.join(&name))?;
        }
    }
    Ok(())
}

fn object_args(
    args_json: &str,
    allowed: &[&str],
) -> Result<serde_json::Map<String, Value>, BoxError> {
    let args: Value = serde_json::from_str(args_json)?;
    let Some(object) = args.as_object() else {
        return Err("参数须是对象".into());
    };
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("不认识字段 `{key}`").into());
    }
    Ok(object.clone())
}

fn required_string(args: &serde_json::Map<String, Value>, key: &str) -> Result<String, BoxError> {
    match args.get(key) {
        Some(Value::String(value)) => Ok(value.clone()),
        _ => Err(format!("缺少字符串参数 `{key}`").into()),
    }
}

fn optional_string(
    args: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>, BoxError> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("`{key}` 须是字符串").into()),
    }
}

fn optional_i64(args: &serde_json::Map<String, Value>, key: &str) -> Result<Option<i64>, BoxError> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::Number(number)) => number
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("`{key}` 须是整数").into()),
        Some(_) => Err(format!("`{key}` 须是整数").into()),
    }
}

fn optional_bool(
    args: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<bool>, BoxError> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("`{key}` 须是布尔值").into()),
    }
}

fn line_spans(text: &str) -> Vec<(usize, usize)> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            spans.push((start, index + 1));
            start = index + 1;
        }
    }
    if start < text.len() {
        spans.push((start, text.len()));
    }
    spans
}

fn clip_str(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn read_window(text: &str, offset: Option<i64>, lines: Option<i64>) -> Result<String, BoxError> {
    let start = match offset.unwrap_or(1) {
        n if n < 1 => return Err("offset 从 1 起".into()),
        n => usize::try_from(n).map_err(|_| "offset 过大")?,
    };
    let want = match lines {
        None => None,
        Some(n) if n < 1 => return Err("lines 至少为 1".into()),
        Some(n) => Some(usize::try_from(n).map_err(|_| "lines 过大")?),
    };
    let spans = line_spans(text);
    let total = spans.len();
    if total == 0 {
        return Ok("〔共 0 行〕".into());
    }
    if start > total {
        return Err(format!("行号超出范围：文件共 {total} 行").into());
    }
    let mut body = String::new();
    let mut end = start - 1;
    let mut byte_cut = false;
    let last = want.map(|n| (start + n - 1).min(total)).unwrap_or(total);
    for (number, &(from, to)) in spans
        .iter()
        .enumerate()
        .skip(start - 1)
        .take(last - (start - 1))
    {
        let piece = &text[from..to];
        if body.len() + piece.len() > MAX_READ_BYTES {
            let room = MAX_READ_BYTES.saturating_sub(body.len());
            if room == 0 {
                break;
            }
            body.push_str(clip_str(piece, room));
            end = number + 1;
            byte_cut = true;
            break;
        }
        body.push_str(piece);
        end = number + 1;
    }
    let more = byte_cut || end < total;
    let tail = if byte_cut {
        format!("已按 {MAX_READ_BYTES} 字节截断，后面还有")
    } else if more {
        "后面还有".into()
    } else {
        "已到末尾".into()
    };
    Ok(format!(
        "〔第 {start}–{end} 行，共 {total} 行；{tail}〕\n{body}"
    ))
}

fn format_matches(page: &MatchPage, enhanced: bool, with_context: bool) -> String {
    if page.hits.is_empty() {
        return if enhanced && page.offset > 0 && page.total > 0 {
            format!("无匹配（共 {} 条，偏移已越过末尾）", page.total)
        } else {
            "无匹配".into()
        };
    }
    let mut out = String::new();
    if enhanced {
        let from = page.offset + 1;
        let to = page.offset + page.hits.len();
        let tail = if page.offset + page.hits.len() < page.total {
            "；后面还有"
        } else {
            ""
        };
        out.push_str(&format!(
            "〔第 {from}–{to} 条，共 {} 条{tail}〕\n",
            page.total
        ));
    }
    let sep = if with_context { "\n---\n" } else { "\n" };
    out.push_str(&page.hits.join(sep));
    if !enhanced && page.more {
        out.push_str(&format!("\n[已截断：仅显示前 {MAX_MATCHES} 条]"));
    }
    out
}

fn string_arg(args_json: &str, key: &str) -> Result<String, BoxError> {
    let mut args: Value = serde_json::from_str(args_json)?;
    match args.get_mut(key).map(Value::take) {
        Some(Value::String(value)) => Ok(value),
        _ => Err(format!("缺少字符串参数 `{key}`").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_records_reports_precedents_with_handler_and_verdict() {
        let record = |id, edict: &str| Record {
            task: ziyuan_core::TaskId(id),
            department: ziyuan_core::Department::Works,
            edict: edict.into(),
            worker: ziyuan_core::AgentId(4),
            handler: "陈工".into(),
            summary: "回奏".repeat(400).into(),
            verdict: "采纳：好".into(),
        };
        let tool = record_search(Arc::new(vec![record(1, "检查除零"), record(2, "整理文档")]));
        let out = tool.execute(r#"{"query":"除零"}"#).unwrap();
        assert!(out.starts_with("#1 工部 承办 #4 陈工（采纳：好）\n诏令：检查除零\n回奏："));
        assert!(!out.contains("整理文档") && out.ends_with('…'), "{out}");
        assert_eq!(
            tool.execute(r#"{"query":"数据库"}"#).unwrap(),
            "实录中无相关前例"
        );
        assert!(tool.execute(r#"{"query":" "}"#).is_err());
    }

    fn workspace(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ziyuan-tools-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let project = dir.join("project");
        for sub in ["src", "docs", "target", ".ziyuan"] {
            fs::create_dir_all(project.join(sub)).unwrap();
        }
        fs::write(dir.join("secret.txt"), "outside").unwrap();
        fs::write(
            project.join("src/main.rs"),
            "fn main() {\n    unsafe { run() }\n}\n",
        )
        .unwrap();
        fs::write(project.join("docs/a.md"), "unsafe doc\n").unwrap();
        fs::write(project.join("target/gen.rs"), "unsafe { built }").unwrap();
        fs::write(project.join(".env"), "unsafe=1").unwrap();
        fs::write(project.join(".ziyuan/court.log"), "unsafe").unwrap();
        dir
    }

    fn seal(capabilities: &[Capability], scope: &[&str]) -> Seal {
        Seal {
            capabilities: capabilities.iter().copied().collect(),
            scope: scope.iter().map(|s| s.to_string()).collect(),
            mcp: Default::default(),
        }
    }

    #[test]
    fn a_missing_file_scope_can_be_created_but_nothing_else_written() {
        let root = workspace("changelog");
        let project = root.join("project");
        let seal = Seal {
            capabilities: [Capability::ReadFile, Capability::WriteFile].into(),
            scope: vec!["CHANGELOG.md".into()],
            mcp: Default::default(),
        };
        let sealed = sealed_tools(&project, &seal, None).unwrap();
        let write = sealed
            .tools
            .iter()
            .find(|t| t.spec().name == "write_file")
            .unwrap();
        let read = sealed
            .tools
            .iter()
            .find(|t| t.spec().name == "read_file")
            .unwrap();
        assert!(
            read.execute(r#"{"path":"CHANGELOG.md"}"#).is_err(),
            "尚不存在"
        );
        write
            .execute(r##"{"path":"CHANGELOG.md","content":"# Changelog\n"}"##)
            .unwrap();
        assert!(
            write
                .execute(r#"{"path":"src/main.rs","content":"x"}"#)
                .is_err()
        );
        assert!(
            write
                .execute(r#"{"path":"CHANGELOG.md.bak","content":"x"}"#)
                .is_err()
        );
        let changes = sealed.changes();
        assert_eq!(
            (
                changes.len(),
                changes[0].path.as_str(),
                changes[0].before.clone()
            ),
            (1, "CHANGELOG.md", None)
        );
        assert!(!project.join("CHANGELOG.md").exists(), "只暂存，不落盘");
        // 只有 CHANGELOG.md 可以尚不存在。
        let other = Seal {
            scope: vec!["NEWS.md".into()],
            ..seal.clone()
        };
        assert!(sealed_tools(&project, &other, None).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    fn tool(root: &Path, seal: &Seal, name: &str) -> Box<dyn Tool> {
        sealed_tools(root, seal, None)
            .unwrap()
            .tools
            .into_iter()
            .find(|t| t.spec().name == name)
            .unwrap()
    }

    #[test]
    fn read_file_stays_inside_root_and_out_of_denied_paths() {
        let dir = workspace("read");
        let root = dir.join("project");
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("secret.txt"), root.join("link.txt")).unwrap();
        let read = tool(&root, &Seal::default(), "read_file");

        assert!(
            read.execute(r#"{"path":"src/main.rs"}"#)
                .unwrap()
                .contains("unsafe")
        );
        assert!(read.execute(r#"{"path":"target/gen.rs"}"#).is_ok());
        let outside = dir.join("secret.txt").display().to_string();
        // 符号链接只在 Unix 上建。
        for path in [
            "../secret.txt",
            outside.as_str(),
            "link.txt",
            ".env",
            ".ziyuan/court.log",
        ]
        .into_iter()
        .filter(|p| cfg!(unix) || *p != "link.txt")
        {
            let args = serde_json::json!({ "path": path }).to_string();
            let err = read.execute(&args).unwrap_err().to_string();
            assert!(err.contains("拒绝访问"), "{path}: {err}");
        }
        assert!(read.execute(r#"{"file":"src/main.rs"}"#).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn search_reports_locations_and_skips_denied_and_build_output() {
        let dir = workspace("search");
        let search = tool(&dir.join("project"), &Seal::default(), "search_code");
        assert_eq!(
            search.execute(r#"{"query":"unsafe"}"#).unwrap(),
            "docs/a.md:1: unsafe doc\nsrc/main.rs:2: unsafe { run() }"
        );
        assert_eq!(search.execute(r#"{"query":"nothing"}"#).unwrap(), "无匹配");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn read_window_reports_line_counts_and_refuses_denied_paths() {
        let dir = workspace("window");
        let root = dir.join("project");
        let read = tool(&root, &Seal::default(), "read_file");
        let window = read
            .execute(r#"{"path":"src/main.rs","offset":2,"lines":1}"#)
            .unwrap();
        assert!(
            window.starts_with("〔第 2–2 行，共 3 行；后面还有〕\n    unsafe { run() }\n"),
            "{window}"
        );
        let tail = read
            .execute(r#"{"path":"src/main.rs","offset":3}"#)
            .unwrap();
        assert!(tail.contains("已到末尾") && tail.contains("}\n"), "{tail}");
        assert!(
            read.execute(r#"{"path":"src/main.rs","offset":9}"#)
                .unwrap_err()
                .to_string()
                .contains("行号超出范围")
        );
        assert!(
            read.execute(r#"{"path":"src/main.rs","offset":0}"#)
                .is_err()
        );
        assert!(read.execute(r#"{"path":"src/main.rs","lines":0}"#).is_err());
        let err = read
            .execute(r#"{"path":".env","offset":1,"lines":1}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("拒绝访问"), "{err}");
        let err = read
            .execute(r#"{"path":".ziyuan/court.log","lines":1}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("拒绝访问"), "{err}");
        // 不给行窗口时仍是原来的全文，没有行数说明。
        assert_eq!(
            read.execute(r#"{"path":"docs/a.md"}"#).unwrap(),
            "unsafe doc\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn search_can_limit_directory_case_and_page_without_leaving_scope() {
        let dir = workspace("search-page");
        let root = dir.join("project");
        let search = tool(&root, &Seal::default(), "search_code");
        let page = search
            .execute(r#"{"query":"unsafe","path":"src","limit":1}"#)
            .unwrap();
        assert!(
            page.starts_with("〔第 1–1 条，共 1 条〕\nsrc/main.rs:2:"),
            "{page}"
        );
        assert!(!page.contains("docs/a.md"), "{page}");
        let folded = search
            .execute(r#"{"query":"UNSAFE","ignore_case":true,"path":"docs"}"#)
            .unwrap();
        assert!(folded.contains("docs/a.md:1:"), "{folded}");
        let next = search
            .execute(r#"{"query":"unsafe","offset":1,"limit":1}"#)
            .unwrap();
        assert!(
            next.contains("第 2–2 条") && next.contains("src/main.rs"),
            "{next}"
        );
        assert!(
            search
                .execute(r#"{"query":"unsafe","path":".env"}"#)
                .unwrap_err()
                .to_string()
                .contains("拒绝访问")
        );
        assert!(
            search
                .execute(r#"{"query":"unsafe","path":".ziyuan"}"#)
                .unwrap_err()
                .to_string()
                .contains("拒绝访问")
        );
        assert!(
            search
                .execute(r#"{"query":"unsafe","path":"target"}"#)
                .unwrap_err()
                .to_string()
                .contains("不搜索构建产物")
        );
        assert!(
            search
                .execute(r#"{"query":"unsafe","path":"../secret.txt"}"#)
                .is_err()
        );
        assert!(search.execute(r#"{"query":"unsafe","limit":0}"#).is_err());
        assert!(search.execute(r#"{"query":"unsafe","offset":-1}"#).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replace_text_stages_one_unique_span_and_rejects_ambiguous_or_denied() {
        let dir = workspace("replace");
        let root = dir.join("project");
        let sealed = sealed_tools(
            &root,
            &seal(&[Capability::ReadFile, Capability::WriteFile], &["src"]),
            None,
        )
        .unwrap();
        let find = |name: &str| sealed.tools.iter().find(|t| t.spec().name == name).unwrap();
        let replace = find("replace_text");
        replace
            .execute(r#"{"path":"src/main.rs","old":"unsafe { run() }","new":"run()"}"#)
            .unwrap();
        let read = find("read_file");
        assert!(
            read.execute(r#"{"path":"src/main.rs"}"#)
                .unwrap()
                .contains("run()")
        );
        assert!(
            !read
                .execute(r#"{"path":"src/main.rs"}"#)
                .unwrap()
                .contains("unsafe")
        );
        let changes = sealed.changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "src/main.rs");
        assert!(changes[0].before.as_deref().unwrap().contains("unsafe"));
        assert!(changes[0].after.contains("    run()\n"));
        assert_eq!(
            fs::read_to_string(root.join("src/main.rs")).unwrap(),
            "fn main() {\n    unsafe { run() }\n}\n"
        );
        let err = replace
            .execute(r#"{"path":"src/main.rs","old":"n","new":"N"}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("须唯一"), "{err}");
        assert!(
            replace
                .execute(r#"{"path":"src/main.rs","old":"不存在的句子","new":"x"}"#)
                .unwrap_err()
                .to_string()
                .contains("找不到")
        );
        assert!(
            replace
                .execute(r#"{"path":".env","old":"unsafe","new":"x"}"#)
                .unwrap_err()
                .to_string()
                .contains("拒绝访问")
        );
        // 新旧相同或 old 为空，也不能绕过禁区和越界。
        for args in [
            r#"{"path":".env","old":"unsafe","new":"unsafe"}"#,
            r#"{"path":".env","old":"","new":""}"#,
            r#"{"path":"../secret.txt","old":"outside","new":"outside"}"#,
            r#"{"path":"../secret.txt","old":"","new":"x"}"#,
        ] {
            let err = replace.execute(args).unwrap_err().to_string();
            assert!(err.contains("拒绝访问"), "{args}: {err}");
            assert!(!err.contains("未写入暂存"), "{args}: {err}");
        }
        assert_eq!(sealed.changes().len(), 1);
        assert!(
            replace
                .execute(r#"{"path":"docs/a.md","old":"unsafe","new":"x"}"#)
                .unwrap_err()
                .to_string()
                .contains("拒绝访问")
        );
        assert!(
            replace
                .execute(r#"{"path":"src/missing.rs","old":"a","new":"b"}"#)
                .is_err()
        );
        let readonly = sealed_tools(&root, &Seal::default(), None).unwrap();
        assert!(
            !readonly
                .tools
                .iter()
                .any(|tool| tool.spec().name == "replace_text")
        );
        let audited = audit_args(
            "replace_text",
            r#"{"path":"src/main.rs","old":"unsafe { run() }","new":"run()"}"#,
        );
        assert!(!audited.contains("unsafe { run() }"), "{audited}");
        assert!(audited.contains("全文见奏本"), "{audited}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn run_tests_keeps_cargo_failure_names_when_the_assertion_is_not_in_the_tail() {
        let dir = workspace("cargo-fail");
        let root = dir.join("project");
        let mut log = "noise\n".repeat(400);
        log.push_str(
            "thread 'tests::foo' panicked at src/lib.rs:4:5:\n\
             assertion `left == right` failed\n  left: 1\n right: 2\n\n",
        );
        log.push_str(&"padding\n".repeat(2000));
        log.push_str(
            "failures:\n    tests::foo\n\n\
             test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
        );
        fs::write(root.join("fixture.txt"), log).unwrap();
        let show = if cfg!(windows) {
            "type fixture.txt & exit /b 1"
        } else {
            "cat fixture.txt; exit 1"
        };
        let sealed =
            sealed_tools(&root, &seal(&[Capability::RunTests], &["."]), Some(show)).unwrap();
        let result = sealed.tools[0].execute("{}");
        let output = match result {
            Err(error)
                if error
                    .downcast_ref::<crate::haoshe::SandboxUnavailable>()
                    .is_some() =>
            {
                RunTests(sealed.sandbox.clone())
                    .run(crate::haoshe::Mode::Off)
                    .unwrap_err()
                    .to_string()
            }
            other => other.unwrap_err().to_string(),
        };
        let (head, note) = output
            .split_once("〔失败测试〕")
            .unwrap_or_else(|| panic!("应附上失败测试名：{output}"));
        assert!(head.contains("未通过"), "{head}");
        assert!(
            !head.contains("left == right"),
            "断言不在输出末尾时仍须单独摘出：{head}"
        );
        assert!(note.contains("- tests::foo"), "{note}");
        assert!(note.contains("left == right"), "{note}");
        // 非 cargo 的失败仍只给末尾，供都察院按工具成败判断。
        let other = sealed_tools(
            &root,
            &seal(&[Capability::RunTests], &["."]),
            Some("exit 3"),
        )
        .unwrap();
        let output = match other.tools[0].execute("{}") {
            Err(error)
                if error
                    .downcast_ref::<crate::haoshe::SandboxUnavailable>()
                    .is_some() =>
            {
                RunTests(other.sandbox.clone())
                    .run(crate::haoshe::Mode::Off)
                    .unwrap_err()
                    .to_string()
            }
            other => other.unwrap_err().to_string(),
        };
        assert!(output.contains("未通过（退出码 3）"), "{output}");
        assert!(!output.contains("〔失败测试〕"), "{output}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn list_files_shows_scope_and_staged_files_but_not_denied_or_build_output() {
        let dir = workspace("list");
        let root = dir.join("project");
        let sealed = sealed_tools(
            &root,
            &seal(&[Capability::ReadFile, Capability::WriteFile], &["."]),
            None,
        )
        .unwrap();
        let find = |name: &str| sealed.tools.iter().find(|t| t.spec().name == name).unwrap();
        let list = find("list_files");
        assert_eq!(
            list.execute("{}").unwrap(),
            "docs/a.md（11 字节）\nsrc/main.rs（35 字节）"
        );
        // 暂存的新文件也列出，并注明尚未落盘。
        find("write_file")
            .execute(r#"{"path":"src/new.rs","content":"x"}"#)
            .unwrap();
        assert_eq!(
            list.execute(r#"{"path":"src"}"#).unwrap(),
            "src/main.rs（35 字节）\nsrc/new.rs（暂存新建，1 字节）"
        );
        for path in [".ziyuan", "..", "target/../../secret.txt"] {
            let args = serde_json::json!({ "path": path }).to_string();
            assert!(list.execute(&args).is_err(), "{path}");
        }
        // 范围之外的目录不可列。
        let narrow = tool(
            &root,
            &seal(&[Capability::ReadFile], &["src"]),
            "list_files",
        );
        assert_eq!(narrow.execute("{}").unwrap(), "src/main.rs（35 字节）");
        assert!(narrow.execute(r#"{"path":"docs"}"#).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn seal_limits_tools_and_scope() {
        let dir = workspace("seal");
        let root = dir.join("project");
        let only_read = sealed_tools(&root, &seal(&[Capability::ReadFile], &["src"]), None)
            .unwrap()
            .tools;
        let names: Vec<_> = only_read.iter().map(|t| t.spec().name).collect();
        assert_eq!(
            names,
            [
                "read_file",
                "list_files",
                "file_outline",
                "vcs_status",
                "vcs_diff",
                "vcs_log"
            ]
        );
        assert!(only_read[0].execute(r#"{"path":"src/main.rs"}"#).is_ok());
        let err = only_read[0]
            .execute(r#"{"path":"docs/a.md"}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("拒绝访问"), "{err}");

        // 重叠的范围合并，结果不重复；单个文件也可作为范围。
        let search = tool(
            &root,
            &seal(&[Capability::SearchCode], &["src/main.rs", "src", "./src"]),
            "search_code",
        );
        assert_eq!(
            search.execute(r#"{"query":"unsafe"}"#).unwrap(),
            "src/main.rs:2: unsafe { run() }"
        );

        assert!(
            sealed_tools(&root, &seal(&[], &["."]), None)
                .unwrap()
                .tools
                .is_empty()
        );
        #[cfg(unix)]
        std::os::unix::fs::symlink(&dir, root.join("up")).unwrap();
        for bad in [".ziyuan", "missing", "up"] {
            assert!(
                sealed_tools(&root, &seal(&Capability::ALL, &[bad]), None).is_err(),
                "{bad}"
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn writes_are_staged_tested_in_a_copy_and_applied_only_without_conflicts() {
        let dir = workspace("write");
        let root = dir.join("project");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&dir, root.join("src/up")).unwrap();
        #[cfg(not(windows))]
        let check = "grep -q staged src/main.rs && test -f src/new/a.rs && test ! -e .env && test ! -d .ziyuan && echo COPY-OK";
        // cmd 的 echo 按本地代码页输出，标记用 ASCII。
        // 不使用 `>nul`：Windows Server 默认不对 AppContainer 开放 \Device\Null 写权限。
        #[cfg(windows)]
        let check = r"findstr /c:staged src\main.rs && if exist src\new\a.rs if not exist .env if not exist .ziyuan echo COPY-OK";
        let sealed = sealed_tools(&root, &seal(&Capability::ALL, &["src"]), Some(check)).unwrap();
        let find = |name: &str| sealed.tools.iter().find(|t| t.spec().name == name).unwrap();
        let (write, read, test) = (find("write_file"), find("read_file"), find("run_tests"));

        let original = fs::read_to_string(root.join("src/main.rs")).unwrap();
        write
            .execute(r#"{"path":"src/main.rs","content":"staged"}"#)
            .unwrap();
        write
            .execute(r#"{"path":"./src/new/a.rs","content":"新"}"#)
            .unwrap();
        // 暂存只在工具内可见，项目文件不变。
        assert_eq!(read.execute(r#"{"path":"src/main.rs"}"#).unwrap(), "staged");
        assert_eq!(read.execute(r#"{"path":"src/new/a.rs"}"#).unwrap(), "新");
        assert_eq!(
            fs::read_to_string(root.join("src/main.rs")).unwrap(),
            original
        );
        assert!(!root.join("src/new").exists());
        for path in [
            "docs/a.md",
            "../secret.txt",
            "src/up/secret.txt",
            "src",
            ".env",
        ]
        .into_iter()
        .filter(|p| cfg!(unix) || *p != "src/up/secret.txt")
        {
            let args = serde_json::json!({ "path": path, "content": "x" }).to_string();
            assert!(write.execute(&args).is_err(), "{path}");
        }

        // 本机没有号舍（或 Windows 外部 CARGO_TARGET_DIR 被 W2 策略拒绝）时 run_tests 返回 SandboxUnavailable；
        // 副本的内容改以 off 照常运行来核对。
        let run_tests = |sealed: &Sealed, tool: &dyn Tool| {
            let result = tool.execute("{}");
            let unavailable = (!crate::haoshe::shell_sandbox_available()
                || (cfg!(windows) && std::env::var_os("CARGO_TARGET_DIR").is_some()))
                && crate::haoshe::Mode::from_env().unwrap() != crate::haoshe::Mode::Off;
            if !unavailable {
                return result;
            }
            let error = result.unwrap_err();
            assert!(
                error
                    .downcast_ref::<crate::haoshe::SandboxUnavailable>()
                    .is_some(),
                "{error}"
            );
            RunTests(sealed.sandbox.clone()).run(crate::haoshe::Mode::Off)
        };
        let output = run_tests(&sealed, test.as_ref()).unwrap();
        assert!(
            output.contains("通过（退出码 0）") && output.contains("COPY-OK"),
            "{output}"
        );
        let failing = sealed_tools(
            &root,
            &seal(&[Capability::RunTests], &["."]),
            Some("exit 3"),
        )
        .unwrap();
        let output = run_tests(&failing, failing.tools[0].as_ref())
            .unwrap_err()
            .to_string();
        assert!(output.contains("未通过（退出码 3）"), "{output}");
        let unset = sealed_tools(&root, &seal(&[Capability::RunTests], &["."]), None).unwrap();
        assert!(unset.tools[0].execute("{}").is_err());

        let changes = sealed.changes();
        let summary: Vec<_> = changes
            .iter()
            .map(|c| (c.path.as_str(), c.before.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                ("src/main.rs", Some(original.as_str())),
                ("src/new/a.rs", None)
            ]
        );
        // 呈报后文件被他人改动：一处也不写。
        fs::write(root.join("src/main.rs"), "他人改动").unwrap();
        let err = apply_changes(&root, &changes).unwrap_err().to_string();
        assert!(err.contains("已被改动"), "{err}");
        assert!(!root.join("src/new").exists());
        fs::write(root.join("src/main.rs"), &original).unwrap();
        apply_changes(&root, &changes).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("src/main.rs")).unwrap(),
            "staged"
        );
        assert_eq!(fs::read_to_string(root.join("src/new/a.rs")).unwrap(), "新");
        // 写到一半中断（main.rs 已写、a.rs 未写）后重试：续写未写的，已写的不动。
        fs::remove_file(root.join("src/new/a.rs")).unwrap();
        apply_changes(&root, &changes).unwrap();
        assert_eq!(fs::read_to_string(root.join("src/new/a.rs")).unwrap(), "新");
        // 全部已写完：再落盘一次什么也不改，也不报错。
        apply_changes(&root, &changes).unwrap();
        assert_eq!(base_state(&root, &changes[0]).unwrap(), BaseState::Written);
        // 逐字节比较：只把行尾改成 CRLF 也算被改动；改成非 UTF-8 内容同样拒绝。
        let crlf = FileChange {
            path: "src/main.rs".into(),
            before: Some("a\nb\n".into()),
            after: "a\nB\n".into(),
            deleted: false,
        };
        fs::write(root.join("src/main.rs"), "a\r\nb\r\n").unwrap();
        assert_eq!(base_state(&root, &crlf).unwrap(), BaseState::Changed);
        fs::write(root.join("src/main.rs"), [0xff, 0xfe, 0]).unwrap();
        assert_eq!(base_state(&root, &crlf).unwrap(), BaseState::Changed);
        let err = apply_changes(&root, std::slice::from_ref(&crlf))
            .unwrap_err()
            .to_string();
        assert!(err.contains("已被改动"), "{err}");
        fs::write(root.join("src/main.rs"), "a\nb\n").unwrap();
        assert_eq!(base_state(&root, &crlf).unwrap(), BaseState::Original);
        // 事件簿被篡改成越界路径时同样拒绝。
        let evil = [FileChange {
            path: "../evil".into(),
            before: None,
            after: String::new(),
            deleted: false,
        }];
        assert!(apply_changes(&root, &evil).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_write_mid_batch_keeps_later_targets_untouched_and_retry_completes() {
        use replace::faults::{Faults, install};
        let dir = workspace("partial-batch");
        let root = dir.join("project");
        let original = fs::read_to_string(root.join("src/main.rs")).unwrap();
        let changes = [
            FileChange {
                path: "src/main.rs".into(),
                before: Some(original.clone()),
                after: "first".into(),
                deleted: false,
            },
            FileChange {
                path: "src/new/b.rs".into(),
                before: None,
                after: "second file content".into(),
                deleted: false,
            },
        ];
        {
            let _guard = install(Faults {
                skip_calls: 1,
                partial_write: true,
                ..Faults::default()
            });
            let err = apply_changes(&root, &changes).unwrap_err().to_string();
            assert!(err.contains("injected"), "{err}");
        }
        assert_eq!(
            fs::read_to_string(root.join("src/main.rs")).unwrap(),
            "first"
        );
        assert!(!root.join("src/new/b.rs").exists());
        let leftovers = |sub: &str| fs::read_dir(root.join(sub)).map_or(0, |d| d.count());
        assert_eq!((leftovers("src"), leftovers("src/new")), (2, 0));
        assert_eq!(base_state(&root, &changes[0]).unwrap(), BaseState::Written);
        assert_eq!(base_state(&root, &changes[1]).unwrap(), BaseState::Original);
        apply_changes(&root, &changes).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("src/main.rs")).unwrap(),
            "first"
        );
        assert_eq!(
            fs::read_to_string(root.join("src/new/b.rs")).unwrap(),
            "second file content"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// 造一份“已死写入者”的租约与它认领的临时文件。
    fn stale_temp(dir: &Path) -> PathBuf {
        stale_temp_named(dir, ".ziyuan-tmp-1-1-1", "0123456789abcdef0123456789abcdef")
    }

    /// 同 [`stale_temp`]，名字与租约 id 由调用方给：同一目录里两个陈旧临时文件
    /// 各要自己的租约，否则会被判成“两个认领者”而整轮不删。
    fn stale_temp_named(dir: &Path, name: &str, id: &str) -> PathBuf {
        let temp = dir.join(name);
        fs::write(&temp, b"partial").unwrap();
        // 行尾只能是 `\n`：租约里多一个 `\r` 就解析不出 id，会被判成 LeaseUnparsable
        // 而整轮不删，用例就测不到“陈旧即可清理”这件事。
        fs::write(
            dir.join(format!(".ziyuan-lease-{id}")),
            format!(
                "ziyuan-lease v1 temp={} id={id}\n",
                temp.file_name().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
        temp
    }

    fn file_change(path: &str, after: &str) -> FileChange {
        FileChange {
            path: path.into(),
            before: None,
            after: after.into(),
            deleted: false,
        }
    }

    fn temp_name(path: &Path) -> String {
        path.file_name().unwrap().to_string_lossy().into_owned()
    }

    #[test]
    fn relative_parent_of_strips_only_the_last_component() {
        assert_eq!(relative_parent_of("src/deep/a.rs"), "src/deep");
        assert_eq!(relative_parent_of("a.rs"), "");
        // 词法上的 `./` 也一并抹掉：受保护集合里的条目就是这个拼法。
        assert_eq!(relative_parent_of("./src/a.rs"), "src");
    }

    #[test]
    fn apply_changes_sweeps_a_stale_temp_next_to_the_target() {
        let dir = workspace("sweep-stale");
        let project = dir.join("project");
        let stale = stale_temp(&project.join("src"));
        let applied = apply_changes_protected(
            &project,
            &[file_change("src/new.rs", "fn main() {}\n")],
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(applied.problems.is_empty(), "{:?}", applied.problems);
        assert!(!stale.exists(), "写入前应清掉同目录的陈旧临时文件");
        assert_eq!(
            fs::read_to_string(project.join("src/new.rs")).unwrap(),
            "fn main() {}\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn apply_changes_keeps_a_protected_temp() {
        let dir = workspace("sweep-protected");
        let project = dir.join("project");
        let stale = stale_temp(&project.join("src"));
        // 同目录里再放一个不受保护的陈旧临时文件：它必须被删掉，否则“什么都没扫”
        // 也能让上面那条存活断言通过，用例就证明不了受保护集合真的在起作用。
        let doomed = stale_temp_named(
            &project.join("src"),
            ".ziyuan-tmp-2-2-2",
            "fedcba9876543210fedcba9876543210",
        );
        let protected: BTreeSet<String> =
            [format!("src/{}", temp_name(&stale))].into_iter().collect();
        apply_changes_protected(&project, &[file_change("src/new.rs", "x\n")], &protected).unwrap();
        assert!(stale.exists(), "受保护路径不得删除");
        assert!(!doomed.exists(), "不受保护的陈旧临时文件应被清掉");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn apply_changes_leaves_other_directories_alone() {
        let dir = workspace("sweep-other");
        let project = dir.join("project");
        let elsewhere = stale_temp(&project.join("docs"));
        // 工作区根（`project`）自己的陈旧临时文件：只有“扫根而不是扫父目录”才会碰到它。
        let at_root = stale_temp(&project);
        // 目标目录的下一层：`sweep_dir` 不递归，所以它也应当活下来。
        let nested_dir = project.join("src/nested");
        fs::create_dir_all(&nested_dir).unwrap();
        let nested = stale_temp(&nested_dir);
        apply_changes_protected(
            &project,
            &[file_change("src/new.rs", "x\n")],
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(elsewhere.exists(), "自动清理只碰目标所在目录");
        assert!(at_root.exists(), "自动清理不扫工作区根");
        assert!(nested.exists(), "自动清理不递归子目录");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn apply_changes_labels_the_sweep_from_the_lexical_change_path() {
        let dir = workspace("sweep-deep");
        let project = dir.join("project");
        let deep = project.join("src/deep");
        fs::create_dir_all(&deep).unwrap();
        let stale = stale_temp(&deep);
        let doomed = stale_temp_named(
            &deep,
            ".ziyuan-tmp-3-3-3",
            "00112233445566778899aabbccddeeff",
        );
        // 恢复记录的写法：工作区相对、词法拼法，与 `src/deep/a.rs` 的父目录一致。
        let protected: BTreeSet<String> = [format!("src/deep/{}", temp_name(&stale))]
            .into_iter()
            .collect();
        apply_changes_protected(&project, &[file_change("src/deep/a.rs", "a\n")], &protected)
            .unwrap();
        assert!(stale.exists(), "词法标签 src/deep 应与受保护条目对得上");
        assert!(!doomed.exists(), "同目录里不受保护的陈旧临时文件应被清掉");
        assert_eq!(fs::read_to_string(deep.join("a.rs")).unwrap(), "a\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn apply_changes_still_writes_when_the_sweep_finds_nothing() {
        let dir = workspace("sweep-clean");
        let project = dir.join("project");
        let applied = apply_changes_protected(
            &project,
            &[file_change("src/a.rs", "a\n")],
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(applied.problems.is_empty());
        assert_eq!(fs::read_to_string(project.join("src/a.rs")).unwrap(), "a\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn apply_changes_still_writes_when_the_sweep_fails() {
        let dir = workspace("sweep-fails");
        let project = dir.join("project");
        let src = project.join("src");
        let stale = stale_temp(&src);
        // `write_all` 先把根与目标目录 canonicalize，扫描时的候选路径就是那个拼法
        // （Windows 上是 `\\?\` 前缀的 verbatim 路径），布防必须用同一拼法才对得上。
        let swept_dir = src.canonicalize().unwrap();
        let armed = swept_dir.join(stale.file_name().unwrap());
        // 真实的“删不掉”只在特定权限与文件系统下出现，所以用测试钩子注入一次确定
        // 的删除失败：清理诊断要进报告，写入必须照旧落地（自动清理绝不阻断写入）。
        let _armed = temp_cleanup::tests_hooks::arm_fail_remove(&armed);
        let applied = apply_changes_protected(
            &project,
            &[file_change("src/new.rs", "written anyway\n")],
            &BTreeSet::new(),
        )
        .expect("清理失败不得让写入失败");
        assert!(
            !applied.problems.is_empty(),
            "删除失败必须留一条诊断: {:?}",
            applied.problems
        );
        assert_eq!(
            applied.problems[0].path.parent(),
            Some(swept_dir.as_path()),
            "诊断必须指向本次写入所在目录的那个候选: {:?}",
            applied.problems
        );
        assert!(stale.exists(), "注入的是删除失败，候选应当还在");
        assert_eq!(
            fs::read_to_string(project.join("src/new.rs")).unwrap(),
            "written anyway\n",
            "清理失败后目标文件仍须写入新内容"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn plain_apply_changes_never_sweeps() {
        // 不掌握受保护集合的调用方不该顺手清理：宁可不删。
        let dir = workspace("sweep-plain");
        let project = dir.join("project");
        let stale = stale_temp(&project.join("src"));
        apply_changes(&project, &[file_change("src/new.rs", "x\n")]).unwrap();
        assert!(stale.exists(), "apply_changes 不做清理");
        assert!(project.join("src/new.rs").is_file());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn file_outline_and_find_symbols_respect_scope_denied_and_staged_files() {
        let dir = workspace("symbols");
        let root = dir.join("project");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("web")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "// fn fake_in_comment() {}
pub struct Engine;
impl Engine {
    pub fn boot(&self) {}
}
pub fn helper() {}
",
        )
        .unwrap();
        fs::write(
            root.join("web/app.mjs"),
            "export class Panel {
  render() {}
}
export function bootUi() {}
",
        )
        .unwrap();
        fs::write(
            root.join("target/gen.rs"),
            "pub fn generated_boot() {}
",
        )
        .unwrap();
        fs::write(
            root.join(".env"),
            "pub fn secret_boot() {}
",
        )
        .unwrap();

        let sealed = sealed_tools(
            &root,
            &seal(
                &[
                    Capability::ReadFile,
                    Capability::SearchCode,
                    Capability::WriteFile,
                ],
                &["."],
            ),
            None,
        )
        .unwrap();
        let get = |name: &str| {
            sealed
                .tools
                .iter()
                .find(|t| t.spec().name == name)
                .expect(name)
        };

        // 1. file_outline 列出结构与方法，跳过注释；支持 kind 过滤。
        let outline = get("file_outline")
            .execute(r#"{"path":"src/lib.rs"}"#)
            .unwrap();
        assert!(
            outline.contains("Engine") && outline.contains("boot"),
            "{outline}"
        );
        assert!(!outline.contains("fake_in_comment"), "{outline}");
        let only_fn = get("file_outline")
            .execute(r#"{"path":"src/lib.rs","kind":"function"}"#)
            .unwrap();
        assert!(
            only_fn.contains("boot") && only_fn.contains("helper"),
            "{only_fn}"
        );
        assert!(!only_fn.contains("[struct]"), "{only_fn}");

        // 2. find_symbols 跳过 target 与 .env，支持 ignore_case、exact、path 过滤。
        let found = get("find_symbols")
            .execute(r#"{"query":"boot","ignore_case":true}"#)
            .unwrap();
        assert!(
            found.contains("src/lib.rs:4: [fn] impl Engine > boot"),
            "{found}"
        );
        assert!(found.contains("web/app.mjs:4: [fn] bootUi"), "{found}");
        assert!(
            !found.contains("generated_boot") && !found.contains("secret_boot"),
            "{found}"
        );

        let exact = get("find_symbols")
            .execute(r#"{"query":"boot","exact":true}"#)
            .unwrap();
        assert!(exact.contains("src/lib.rs:4:"), "{exact}");
        assert!(!exact.contains("bootUi"), "{exact}");

        // 3. 暂存新建文件对 file_outline 与 find_symbols 立即可见。
        get("write_file")
            .execute(r#"{"path":"src/extra.py","content":"def staged_boot():\n    return 1\n"}"#)
            .unwrap();
        let staged_outline = get("file_outline")
            .execute(r#"{"path":"src/extra.py"}"#)
            .unwrap();
        assert!(staged_outline.contains("staged_boot"), "{staged_outline}");
        let staged_found = get("find_symbols")
            .execute(r#"{"query":"staged_boot","path":"src"}"#)
            .unwrap();
        assert!(
            staged_found.contains("src/extra.py:1-2: [fn] staged_boot"),
            "{staged_found}"
        );
        assert!(outline.contains("3-5: [impl] Engine"), "{outline}");
        assert!(
            staged_outline.contains("1-2: [fn] staged_boot"),
            "{staged_outline}"
        );

        // 3b. read_file 支持 end_line 直接复用符号行号范围，且与 lines 互斥、校验边界。
        let impl_body = get("read_file")
            .execute(r#"{"path":"src/lib.rs","offset":3,"end_line":5}"#)
            .unwrap();
        assert!(
            impl_body.contains("〔第 3–5 行，共 6 行；后面还有〕"),
            "{impl_body}"
        );
        assert!(
            impl_body.contains("impl Engine") && impl_body.contains("pub fn boot"),
            "{impl_body}"
        );
        assert!(!impl_body.contains("pub fn helper"), "{impl_body}");
        let head = get("read_file")
            .execute(r#"{"path":"src/lib.rs","end_line":2}"#)
            .unwrap();
        assert!(head.contains("〔第 1–2 行，共 6 行；后面还有〕"), "{head}");
        assert!(
            get("read_file")
                .execute(r#"{"path":"src/lib.rs","offset":2,"lines":2,"end_line":4}"#)
                .is_err()
        );
        assert!(
            get("read_file")
                .execute(r#"{"path":"src/lib.rs","offset":5,"end_line":3}"#)
                .is_err()
        );
        assert!(
            get("read_file")
                .execute(r#"{"path":"src/lib.rs","end_line":0}"#)
                .is_err()
        );

        // 4. 越界与禁区拒绝。
        assert!(get("file_outline").execute(r#"{"path":".env"}"#).is_err());
        assert!(
            get("file_outline")
                .execute(r#"{"path":"../outside.rs"}"#)
                .is_err()
        );
        assert!(
            get("find_symbols")
                .execute(r#"{"query":"x","path":"target"}"#)
                .is_err()
        );

        let narrow = sealed_tools(
            &root,
            &seal(&[Capability::ReadFile, Capability::SearchCode], &["src"]),
            None,
        )
        .unwrap();
        let narrow_outline = narrow
            .tools
            .iter()
            .find(|t| t.spec().name == "file_outline")
            .unwrap();
        let narrow_find = narrow
            .tools
            .iter()
            .find(|t| t.spec().name == "find_symbols")
            .unwrap();
        assert!(narrow_outline.execute(r#"{"path":"web/app.mjs"}"#).is_err());
        let narrow_hits = narrow_find.execute(r#"{"query":"boot"}"#).unwrap();
        assert!(
            narrow_hits.contains("src/lib.rs") && !narrow_hits.contains("web/app.mjs"),
            "{narrow_hits}"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replace_text_batch_edits_are_atomic_and_audit_redacts_edits() {
        let dir = workspace("batch-replace");
        let root = dir.join("project");
        fs::write(
            root.join("src/main.rs"),
            "fn alpha() -> i32 { 1 }\nfn beta() -> i32 { 2 }\nfn gamma() -> i32 { 3 }\n",
        )
        .unwrap();
        let seal = Seal {
            capabilities: [Capability::ReadFile, Capability::WriteFile].into(),
            scope: vec!["src".into()],
            mcp: Default::default(),
        };
        let sealed = sealed_tools(&root, &seal, None).unwrap();
        let find = |name: &str| sealed.tools.iter().find(|t| t.spec().name == name).unwrap();
        let replace = find("replace_text");

        let bad = serde_json::json!({
            "path": "src/main.rs",
            "edits": [
                {"old": "fn alpha() -> i32 { 1 }", "new": "fn alpha() -> i32 { 10 }"},
                {"old": "fn missing() {}", "new": "fn nope() {}"}
            ]
        })
        .to_string();
        let err = replace.execute(&bad).unwrap_err().to_string();
        assert!(err.contains("第 2 处补丁"), "{err}");
        assert!(sealed.changes().is_empty(), "失败批次不得留下半截改动");

        let mixed = serde_json::json!({
            "path": "src/main.rs",
            "old": "alpha",
            "new": "a",
            "edits": [{"old": "beta", "new": "b"}]
        })
        .to_string();
        assert!(replace.execute(&mixed).is_err());

        let ok = serde_json::json!({
            "path": "src/main.rs",
            "edits": [
                {"old": "fn alpha() -> i32 { 1 }", "new": "fn alpha() -> i32 { 10 }"},
                {"old": "fn gamma() -> i32 { 3 }", "new": "fn gamma() -> i32 { 30 }"}
            ]
        })
        .to_string();
        let out = replace.execute(&ok).unwrap();
        assert!(out.contains("替换 2 处"), "{out}");
        let changes = sealed.changes();
        assert_eq!(changes.len(), 1);
        assert!(!changes[0].deleted);
        assert!(changes[0].after.contains("10") && changes[0].after.contains("30"));

        let audited = audit_args("replace_text", &ok);
        assert!(!audited.contains("fn alpha()"), "{audited}");
        assert!(audited.contains("全文见奏本"), "{audited}");

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn delete_file_and_rename_file_stage_overlay_and_apply_cleanly() {
        let dir = workspace("delete-rename");
        let root = dir.join("project");
        fs::write(root.join("src/obsolete.rs"), "pub fn old() {}\n").unwrap();
        fs::write(root.join("src/moved.rs"), "pub fn stay() -> i32 { 7 }\n").unwrap();
        let seal = Seal {
            capabilities: [
                Capability::ReadFile,
                Capability::SearchCode,
                Capability::WriteFile,
            ]
            .into(),
            scope: vec!["src".into()],
            mcp: Default::default(),
        };
        let sealed = sealed_tools(&root, &seal, None).unwrap();
        let get = |name: &str| sealed.tools.iter().find(|t| t.spec().name == name).unwrap();

        assert!(get("delete_file").execute(r#"{"path":".env"}"#).is_err());
        assert!(
            get("rename_file")
                .execute(r#"{"from":"src/moved.rs","to":".env"}"#)
                .is_err()
        );

        get("delete_file")
            .execute(r#"{"path":"src/obsolete.rs"}"#)
            .unwrap();
        assert!(root.join("src/obsolete.rs").exists());
        assert!(
            get("read_file")
                .execute(r#"{"path":"src/obsolete.rs"}"#)
                .is_err()
        );
        assert!(
            get("file_outline")
                .execute(r#"{"path":"src/obsolete.rs"}"#)
                .is_err()
        );
        let listed = get("list_files").execute(r#"{}"#).unwrap();
        assert!(!listed.contains("obsolete.rs"), "{listed}");
        let searched = get("search_code").execute(r#"{"query":"fn old"}"#).unwrap();
        assert!(!searched.contains("obsolete.rs"), "{searched}");
        let syms = get("find_symbols").execute(r#"{"query":"old"}"#).unwrap();
        assert!(!syms.contains("obsolete.rs"), "{syms}");

        assert!(
            get("rename_file")
                .execute(r#"{"from":"src/moved.rs","to":"src/main.rs"}"#)
                .is_err()
        );
        get("rename_file")
            .execute(r#"{"from":"src/moved.rs","to":"src/renamed.rs"}"#)
            .unwrap();
        assert!(
            get("read_file")
                .execute(r#"{"path":"src/moved.rs"}"#)
                .is_err()
        );
        let renamed_text = get("read_file")
            .execute(r#"{"path":"src/renamed.rs"}"#)
            .unwrap();
        assert!(renamed_text.contains("fn stay()"), "{renamed_text}");

        get("write_file")
            .execute(r#"{"path":"src/temp.rs","content":"// temp\n"}"#)
            .unwrap();
        get("delete_file")
            .execute(r#"{"path":"src/temp.rs"}"#)
            .unwrap();

        let changes = sealed.changes();
        assert_eq!(changes.len(), 3, "{changes:?}");
        assert!(
            changes
                .iter()
                .any(|c| c.path == "src/obsolete.rs" && c.deleted)
        );
        assert!(
            changes
                .iter()
                .any(|c| c.path == "src/moved.rs" && c.deleted)
        );
        assert!(
            changes
                .iter()
                .any(|c| c.path == "src/renamed.rs" && !c.deleted && c.before.is_none())
        );

        apply_changes(&root, &changes).unwrap();
        assert!(!root.join("src/obsolete.rs").exists());
        assert!(!root.join("src/moved.rs").exists());
        assert_eq!(
            fs::read_to_string(root.join("src/renamed.rs")).unwrap(),
            "pub fn stay() -> i32 { 7 }\n"
        );
        assert_eq!(base_state(&root, &changes[0]).unwrap(), BaseState::Written);
        apply_changes(&root, &changes).unwrap();

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn test_presets_parse_and_enforce_strict_allowlist() {
        let presets = parse_test_presets(
            "check=cargo check --workspace; clippy = cargo clippy -- -D warnings\ntest=cargo test",
        )
        .unwrap();
        assert_eq!(presets.len(), 3);
        assert_eq!(presets["check"], "cargo check --workspace");
        assert_eq!(presets["clippy"], "cargo clippy -- -D warnings");
        assert_eq!(presets["test"], "cargo test");

        // 非法名称、缺命令、重复名均拒绝。
        assert!(parse_test_presets("bad name=cargo test").is_err());
        assert!(parse_test_presets("check=").is_err());
        assert!(parse_test_presets("no_equals").is_err());
        assert!(parse_test_presets("a=1;a=2").is_err());

        let dir =
            std::env::temp_dir().join(format!("ziyuan-preset-allowlist-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), "ok").unwrap();
        let sandbox =
            Arc::new(Sandbox::new(&dir, &[".".into()], Some("cargo test"), presets).unwrap());
        let runner = RunTests(sandbox);
        assert_eq!(runner.resolve_command(None).unwrap(), "cargo test");
        assert_eq!(
            runner.resolve_command(Some("check")).unwrap(),
            "cargo check --workspace"
        );
        assert_eq!(
            runner.resolve_command(Some("clippy")).unwrap(),
            "cargo clippy -- -D warnings"
        );
        let err = runner
            .resolve_command(Some("rm -rf /"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("未知测试预设") && err.contains("check"),
            "{err}"
        );
        assert!(runner.resolve_command(Some("")).is_err());
        assert!(runner.spec().description.contains("check="));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn list_files_and_search_code_support_glob_regex_context_and_staged_new_files() {
        let dir = workspace("search-nav");
        let root = dir.join("project");
        fs::write(
            root.join("src/engine.rs"),
            "pub fn boot_alpha() {\n    let x = 1;\n    let y = 2;\n}\npub fn boot_beta() {}\n",
        )
        .unwrap();
        let sealed = sealed_tools(
            &root,
            &seal(
                &[
                    Capability::ReadFile,
                    Capability::SearchCode,
                    Capability::WriteFile,
                ],
                &["."],
            ),
            None,
        )
        .unwrap();
        let get = |name: &str| sealed.tools.iter().find(|t| t.spec().name == name).unwrap();

        get("write_file")
            .execute(r#"{"path":"src/staged_mod.rs","content":"pub fn boot_gamma() {}\n"}"#)
            .unwrap();

        let rs_only = get("list_files").execute(r#"{"glob":"*.rs"}"#).unwrap();
        assert!(
            rs_only.contains("src/main.rs")
                && rs_only.contains("src/engine.rs")
                && rs_only.contains("src/staged_mod.rs")
                && !rs_only.contains("docs/a.md"),
            "{rs_only}"
        );
        let md_only = get("list_files")
            .execute(r#"{"glob":"docs/*.md"}"#)
            .unwrap();
        assert!(
            md_only.contains("docs/a.md") && !md_only.contains("src/main.rs"),
            "{md_only}"
        );
        assert!(get("list_files").execute(r#"{"glob":"*.[rs"}"#).is_err());

        let re_out = get("search_code")
            .execute(r#"{"query":"boot_(alpha|gamma)","regex":true,"glob":"src/*.rs"}"#)
            .unwrap();
        assert!(
            re_out.contains("src/engine.rs:1:")
                && re_out.contains("src/staged_mod.rs:1:")
                && !re_out.contains("boot_beta"),
            "{re_out}"
        );

        let ctx_out = get("search_code")
            .execute(r#"{"query":"let x = 1","path":"src/engine.rs","context_lines":1}"#)
            .unwrap();
        assert!(
            ctx_out.contains("src/engine.rs:1- pub fn boot_alpha() {")
                && ctx_out.contains("src/engine.rs:2:     let x = 1;")
                && ctx_out.contains("src/engine.rs:3-     let y = 2;"),
            "{ctx_out}"
        );

        assert!(
            get("search_code")
                .execute(r#"{"query":"[unclosed","regex":true}"#)
                .unwrap_err()
                .to_string()
                .contains("正则表达式无效")
        );
        assert!(
            get("search_code")
                .execute(r#"{"query":"boot","context_lines":6}"#)
                .is_err()
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn find_references_skips_comments_strings_and_substrings_and_respects_scope_and_staged() {
        let dir = workspace("refs");
        let root = dir.join("project");
        fs::write(
            root.join("src/lib.rs"),
            "// compute() in comment\npub fn compute(v: i32) -> i32 {\n    let msg = \"compute() in string\";\n    let recompute = v + 1;\n    recompute\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("src/caller.rs"),
            "use crate::compute;\npub fn run_all() -> i32 {\n    compute(42)\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("docs/a.md"),
            "Mention compute in markdown prose.\n",
        )
        .unwrap();
        fs::write(root.join(".env"), "compute=secret\n").unwrap();

        let sealed = sealed_tools(
            &root,
            &seal(
                &[
                    Capability::ReadFile,
                    Capability::SearchCode,
                    Capability::WriteFile,
                ],
                &["."],
            ),
            None,
        )
        .unwrap();
        let get = |name: &str| sealed.tools.iter().find(|t| t.spec().name == name).unwrap();

        let all = get("find_references")
            .execute(r#"{"symbol":"compute"}"#)
            .unwrap();
        assert!(all.contains("src/lib.rs:2 [定义]:"), "{all}");
        assert!(all.contains("src/caller.rs:1 [引用]:"), "{all}");
        assert!(all.contains("src/caller.rs:3 [引用]:"), "{all}");
        assert!(
            !all.contains("in comment")
                && !all.contains("in string")
                && !all.contains("recompute")
                && !all.contains("docs/a.md")
                && !all.contains(".env"),
            "{all}"
        );

        let refs_only = get("find_references")
            .execute(r#"{"symbol":"compute","include_definitions":false}"#)
            .unwrap();
        assert!(!refs_only.contains("[定义]"), "{refs_only}");
        assert!(refs_only.contains("src/caller.rs:3 [引用]:"), "{refs_only}");

        get("write_file")
            .execute(r#"{"path":"src/new_caller.rs","content":"pub fn extra() { compute(7); }\n"}"#)
            .unwrap();
        get("delete_file")
            .execute(r#"{"path":"src/caller.rs"}"#)
            .unwrap();
        let after_stage = get("find_references")
            .execute(r#"{"symbol":"compute","include_definitions":false}"#)
            .unwrap();
        assert!(
            after_stage.contains("src/new_caller.rs:1 [引用]:")
                && !after_stage.contains("src/caller.rs"),
            "{after_stage}"
        );

        assert!(
            get("find_references")
                .execute(r#"{"symbol":"bad name"}"#)
                .is_err()
        );
        assert!(
            get("find_references")
                .execute(r#"{"symbol":"compute","path":".env"}"#)
                .is_err()
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replace_text_line_range_disambiguates_and_validates_bounds() {
        let dir = workspace("replace-range");
        let root = dir.join("project");
        fs::write(
            root.join("src/multi.rs"),
            "fn a() -> Result<(), ()> {\n    return Ok(());\n}\n\nfn b() -> Result<(), ()> {\n    return Ok(());\n}\n\nfn c() -> Result<(), ()> {\n    return Ok(());\n}\n",
        )
        .unwrap();
        let sealed = sealed_tools(
            &root,
            &seal(&[Capability::ReadFile, Capability::WriteFile], &["src"]),
            None,
        )
        .unwrap();
        let replace = sealed
            .tools
            .iter()
            .find(|t| t.spec().name == "replace_text")
            .unwrap();

        let ambig = replace
            .execute(r#"{"path":"src/multi.rs","old":"return Ok(());","new":"Ok(())"}"#)
            .unwrap_err()
            .to_string();
        assert!(
            ambig.contains("匹配到 3 处")
                && ambig.contains("第 2、6、10 行")
                && ambig.contains("start_line/end_line"),
            "{ambig}"
        );

        replace
            .execute(r#"{"path":"src/multi.rs","old":"return Ok(());","new":"Ok(())","start_line":5,"end_line":7}"#)
            .unwrap();
        let after_one = &sealed.changes()[0].after;
        assert_eq!(after_one.matches("return Ok(());").count(), 2);
        assert_eq!(after_one.matches("    Ok(())").count(), 1);
        assert!(after_one.contains("fn b() -> Result<(), ()> {\n    Ok(())\n}"));

        replace
            .execute(r#"{"path":"src/multi.rs","edits":[{"old":"return Ok(());","new":"Ok(()) // a","start_line":1,"end_line":3},{"old":"return Ok(());","new":"Ok(()) // c","start_line":9,"end_line":11}]}"#)
            .unwrap();
        let after_batch = &sealed.changes()[0].after;
        assert!(after_batch.contains("Ok(()) // a"));
        assert!(after_batch.contains("Ok(()) // c"));
        assert_eq!(after_batch.matches("return Ok(());").count(), 0);

        assert!(
            replace
                .execute(r#"{"path":"src/multi.rs","old":"fn a","new":"fn x","start_line":0}"#)
                .unwrap_err()
                .to_string()
                .contains("从 1 起")
        );
        assert!(
            replace
                .execute(r#"{"path":"src/multi.rs","old":"fn a","new":"fn x","start_line":5,"end_line":3}"#)
                .unwrap_err()
                .to_string()
                .contains("不能大于")
        );
        assert!(
            replace
                .execute(r#"{"path":"src/multi.rs","old":"fn a","new":"fn x","start_line":1,"end_line":999}"#)
                .unwrap_err()
                .to_string()
                .contains("超出文件总行数")
        );
        assert!(
            replace
                .execute(r#"{"path":"src/multi.rs","start_line":1,"edits":[{"old":"fn a","new":"fn x"}]}"#)
                .is_err()
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replace_text_missing_match_reports_outside_range_or_candidate_lines() {
        let dir = workspace("replace-diag");
        let root = dir.join("project");
        fs::write(
            root.join("src/diag.rs"),
            "fn alpha() {\n    let count = 1;\n}\nfn beta() {\n    let total_items = compute_sum(a, b);\n}\n",
        )
        .unwrap();
        let sealed = sealed_tools(
            &root,
            &seal(&[Capability::ReadFile, Capability::WriteFile], &["src"]),
            None,
        )
        .unwrap();
        let replace = sealed
            .tools
            .iter()
            .find(|t| t.spec().name == "replace_text")
            .unwrap();

        let outside = replace
            .execute(r#"{"path":"src/diag.rs","old":"let total_items = compute_sum(a, b);","new":"let total_items = 0;","start_line":1,"end_line":3}"#)
            .unwrap_err()
            .to_string();
        assert!(
            outside.contains("找不到要替换的文本") && outside.contains("第 5 行存在完全匹配"),
            "{outside}"
        );

        let near = replace
            .execute(r#"{"path":"src/diag.rs","old":"let total_items = compute_sum(a, c);","new":"let total_items = 0;"}"#)
            .unwrap_err()
            .to_string();
        assert!(
            near.contains("找不到要替换的文本")
                && near.contains("最接近的候选行")
                && near.contains("L5:     let total_items = compute_sum(a, b);"),
            "{near}"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn run_tests_cancels_inflight_command_promptly() {
        let dir = workspace("run-tests-cancel");
        let root = dir.join("project");
        #[cfg(windows)]
        let slow_cmd = "ping -n 15 127.0.0.1";
        #[cfg(not(windows))]
        let slow_cmd = "sleep 15";

        let sandbox = Arc::new(
            Sandbox::new(&root, &[".".to_owned()], Some(slow_cmd), BTreeMap::new()).unwrap(),
        );
        let tool = RunTests(sandbox);

        let already = AtomicBool::new(true);
        let early = tool
            .run_preset(None, crate::haoshe::Mode::Off, Some(&already))
            .unwrap_err()
            .to_string();
        assert!(early.contains("已取消"), "{early}");

        let flag = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&flag);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            trigger.store(true, Ordering::SeqCst);
        });
        let started = std::time::Instant::now();
        let err = tool
            .run_preset(None, crate::haoshe::Mode::Off, Some(&flag))
            .unwrap_err()
            .to_string();
        let elapsed = started.elapsed();
        handle.join().unwrap();
        assert!(err.contains("已取消"), "{err}");
        assert!(
            elapsed < Duration::from_secs(5),
            "取消传播过慢: {elapsed:?}"
        );

        fs::remove_dir_all(dir).unwrap();
    }
}
