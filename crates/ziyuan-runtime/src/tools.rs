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
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ziyuan_core::{CHANGELOG, Capability, FileChange, Record, Seal, search_records};

mod code_nav;
mod edit;
mod exec;
mod failures;
mod glob;
mod index;
mod read;
mod replace;
mod symbols;
pub mod temp_cleanup;
mod vcs;

use code_nav::{FileOutline, FindReferences, FindSymbols, SymbolImpact};
use edit::{DeleteFile, RenameFile, ReplaceText, WriteFile};
use exec::RunTests;
#[cfg(windows)]
pub(crate) use exec::copy_project_tree;
use read::{ListFiles, ReadFile, SearchCode};

/// 单次 `read_file` 最多返回的字节数。
const MAX_READ_BYTES: usize = 64 * 1024;
/// 单次 `search_code` 一页最多返回的匹配行数，以及上下文行上限。
const MAX_MATCHES: usize = 50;
const MAX_CONTEXT_LINES: usize = 5;
/// 单次 `find_references` 一页最多返回的引用数。
const MAX_REF_MATCHES: usize = 50;
/// 单次 `symbol_impact` 的默认与最多调用点数、上下文行上限、定义列出上限。
const DEFAULT_IMPACT_CALL_SITES: usize = 30;
const MAX_IMPACT_CALL_SITES: usize = 100;
const MAX_IMPACT_CONTEXT_LINES: usize = 3;
const MAX_IMPACT_DEFS: usize = 30;
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
            // `find_symbols`、`find_references`、`symbol_impact` 随 `search_code` 授予，不另设权限，玉玺编码不变。
            Capability::SearchCode => {
                tools.push(Box::new(SearchCode(sandbox.clone())));
                tools.push(Box::new(FindSymbols(sandbox.clone())));
                tools.push(Box::new(FindReferences(sandbox.clone())));
                tools.push(Box::new(SymbolImpact(sandbox.clone())));
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
    /// 增量符号与标识符索引缓存（进程内 + `.ziyuan/index/symbols-v1.json`）。
    symbol_index: Mutex<index::SymbolIndex>,
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
        let index_root = root.clone();
        Ok(Self {
            root,
            scope,
            test_command: test_command.map(str::to_owned),
            test_presets,
            staged: Mutex::default(),
            symbol_index: Mutex::new(index::SymbolIndex::new(index_root)),
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

fn under_skipped_rel(rel: &str) -> bool {
    rel.split('/').any(|seg| SKIPPED_DIRS.contains(&seg))
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

fn string_arg(args_json: &str, key: &str) -> Result<String, BoxError> {
    let mut args: Value = serde_json::from_str(args_json)?;
    match args.get_mut(key).map(Value::take) {
        Some(Value::String(value)) => Ok(value),
        _ => Err(format!("缺少字符串参数 `{key}`").into()),
    }
}

#[cfg(test)]
mod tests;
