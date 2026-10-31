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
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use ziyuan_core::{CHANGELOG, Capability, FileChange, Record, Seal, search_records};

mod replace;
pub mod temp_cleanup;

/// 单次 `read_file` 最多返回的字节数。
const MAX_READ_BYTES: usize = 64 * 1024;
/// 单次 `search_code` 最多返回的匹配行数。
const MAX_MATCHES: usize = 50;
/// 单次 `list_files` 最多列出的文件数。
const MAX_LISTED: usize = 300;
/// 永远不可查阅的目录：朝廷事件簿与版本库元数据。
const DENIED_DIRS: &[&str] = &[".ziyuan", ".git", ".jj"];
/// 搜索时跳过（但可直接读取）的目录：构建产物。测试副本也不复制它们。
const SKIPPED_DIRS: &[&str] = &["target", "node_modules"];
/// 单次 `write_file` 最多写入的字节数。
const MAX_WRITE_BYTES: usize = 256 * 1024;
/// `run_tests` 的时限与返回的输出末尾字节数。
const TEST_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_TEST_OUTPUT: u64 = 8 * 1024;
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
pub fn sealed_tools(
    root: &Path,
    seal: &Seal,
    test_command: Option<&str>,
) -> Result<Sealed, BoxError> {
    let sandbox = Arc::new(Sandbox::new(root, &seal.scope, test_command)?);
    let mut tools: Vec<Box<dyn Tool>> = Vec::new();
    for capability in &seal.capabilities {
        match capability {
            // 能读文件就能看目录：`list_files` 随 `read_file` 授予，不另设权限，玉玺编码不变。
            Capability::ReadFile => {
                tools.push(Box::new(ReadFile(sandbox.clone())));
                tools.push(Box::new(ListFiles(sandbox.clone())));
            }
            Capability::SearchCode => tools.push(Box::new(SearchCode(sandbox.clone()))),
            Capability::WriteFile => tools.push(Box::new(WriteFile(sandbox.clone()))),
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

/// 审计所记的工具参数：`write_file` 的全文已在奏本中，只记行数与字节数。
pub fn audit_args(tool: &str, args_json: &str) -> String {
    if tool != "write_file" {
        return args_json.to_owned();
    }
    let Ok(mut args) = serde_json::from_str::<Value>(args_json) else {
        return args_json.to_owned();
    };
    if let Some(content) = args.get("content").and_then(|c| c.as_str()) {
        let summary = format!(
            "〔{} 行，{} 字节，全文见奏本〕",
            content.lines().count(),
            content.len()
        );
        args["content"] = summary.into();
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
    let state = if current.as_deref() == Some(change.after.as_bytes()) {
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
                targets.push((target, &change.after, relative_parent));
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
            // 先建目录再扫：首次写入新目录时目录尚不存在，`sweep_dir` 会把“读不到”
            // 记成一条 I/O 失败并算作没扫全，正常的第一次写入就被报成清理失败。
            fs::create_dir_all(parent)?;
            if let Some(protected) = protected
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
        replace::replace_target(&target, after.as_bytes())?;
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
struct Sandbox {
    root: PathBuf,
    scope: Vec<PathBuf>,
    test_command: Option<String>,
    /// 以相对路径为键。
    staged: Mutex<BTreeMap<String, FileChange>>,
}

impl Sandbox {
    fn new(root: &Path, scope: &[String], test_command: Option<&str>) -> Result<Self, BoxError> {
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

    fn staged(&self, relative: &str) -> Option<String> {
        let (_, key) = self.resolve_write(relative).ok()?;
        let staged = self.staged.lock().expect("暂存区锁未中毒");
        staged.get(&key).map(|c| c.after.clone())
    }

    fn resolve(&self, relative: &str) -> Result<PathBuf, BoxError> {
        let path = self.root.join(relative).canonicalize()?;
        if !self.scope.iter().any(|s| path.starts_with(s)) || is_denied(&self.root, &path) {
            return Err(format!("拒绝访问玉玺查阅范围之外的路径: {relative}").into());
        }
        Ok(path)
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

    /// 深度优先搜索；返回是否因达到上限而提前停止。不进入符号链接。
    fn search(&self, path: &Path, query: &str, out: &mut Vec<String>) -> io::Result<bool> {
        if !path.is_dir() {
            return Ok(self.search_file(path, query, out));
        }
        let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let file_type = entry.file_type()?;
            let name = entry.file_name();
            let skipped = SKIPPED_DIRS.iter().any(|d| name == *d);
            let truncated = if file_type.is_dir() && !skipped {
                !is_denied(&self.root, &entry.path()) && self.search(&entry.path(), query, out)?
            } else if file_type.is_file() {
                self.search_file(&entry.path(), query, out)
            } else {
                false
            };
            if truncated {
                return Ok(true);
            }
        }
        Ok(false)
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

    fn search_file(&self, path: &Path, query: &str, out: &mut Vec<String>) -> bool {
        // 禁区文件与非 UTF-8（二进制）文件直接跳过。
        if is_denied(&self.root, path) {
            return false;
        }
        let Ok(text) = fs::read_to_string(path) else {
            return false;
        };
        let relative = self.display(path);
        for (number, line) in text.lines().enumerate() {
            if line.contains(query) {
                if out.len() == MAX_MATCHES {
                    return true;
                }
                out.push(format!("{relative}:{}: {}", number + 1, line.trim()));
            }
        }
        false
    }
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
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Capability::ReadFile.code().into(),
            description: format!(
                "读取项目内的文本文件（相对项目根目录的路径）。只能读取授权范围：{}。超过 {MAX_READ_BYTES} 字节会被截断。",
                self.0.scope_label()
            ),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"相对项目根目录的文件路径"}},"required":["path"],"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let relative = string_arg(args_json, "path")?;
        let bytes = match self.0.staged(&relative) {
            Some(text) => text.into_bytes(),
            None => fs::read(self.0.resolve(&relative)?)?,
        };
        let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_READ_BYTES)]);
        if bytes.len() > MAX_READ_BYTES {
            Ok(format!("{text}\n[已截断：文件共 {} 字节]", bytes.len()))
        } else {
            Ok(text.into_owned())
        }
    }
}

struct ListFiles(Arc<Sandbox>);

impl Tool for ListFiles {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_files".into(),
            description: format!(
                "列出授权范围（{}）内某目录下的全部文件（递归，含本次 write_file 暂存的新文件），返回 `路径（字节数）`，最多 {MAX_LISTED} 个；不给 path 则列整个授权范围。跳过 target、node_modules。",
                self.0.scope_label()
            ),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"相对项目根目录的目录路径；省略则列整个授权范围"}},"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args: Value = serde_json::from_str(args_json)?;
        let dirs = match args.get("path").and_then(Value::as_str) {
            Some(p) if !p.trim().is_empty() && p.trim() != "." => vec![self.0.resolve(p)?],
            _ => self.0.scope.clone(),
        };
        let mut files = BTreeMap::new();
        for dir in &dirs {
            self.0.list(dir, &mut files)?;
        }
        // 暂存而尚未落盘的新文件也列出，免得官员以为自己写的文件不存在。
        let prefixes: Vec<String> = dirs.iter().map(|d| self.0.display(d)).collect();
        let staged = self.0.staged.lock().expect("暂存区锁未中毒");
        let mut lines: Vec<String> = Vec::new();
        for (path, change) in staged.iter() {
            let under = prefixes
                .iter()
                .any(|p| p == "." || path == p || path.starts_with(&format!("{p}/")));
            if under && !files.contains_key(path) {
                lines.push(format!("{path}（暂存新建，{} 字节）", change.after.len()));
            }
        }
        drop(staged);
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
                "\n[已截断：共 {total} 个，仅显示前 {MAX_LISTED} 个；可指定子目录]"
            ));
        }
        Ok(out)
    }
}

struct SearchCode(Arc<Sandbox>);

impl Tool for SearchCode {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Capability::SearchCode.code().into(),
            description: format!(
                "在授权范围（{}）内的文本文件中按字面量搜索（区分大小写），返回 `路径:行号: 内容`，最多 {MAX_MATCHES} 条。",
                self.0.scope_label()
            ),
            input_schema_json: r#"{"type":"object","properties":{"query":{"type":"string","description":"要查找的字面量文本"}},"required":["query"],"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let query = string_arg(args_json, "query")?;
        if query.is_empty() {
            return Err("query 不能为空".into());
        }
        let mut matches = Vec::new();
        let mut truncated = false;
        for path in &self.0.scope {
            if self.0.search(path, &query, &mut matches)? {
                truncated = true;
                break;
            }
        }
        if matches.is_empty() {
            return Ok("无匹配".into());
        }
        let mut out = matches.join("\n");
        if truncated {
            out.push_str(&format!("\n[已截断：仅显示前 {MAX_MATCHES} 条]"));
        }
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
            },
        );
        Ok(format!(
            "已暂存 {key}（{bytes} 字节）；诏令完成后呈报皇帝，准奏后才写入项目"
        ))
    }
}

struct RunTests(Arc<Sandbox>);

impl Tool for RunTests {
    fn spec(&self) -> ToolSpec {
        let command = self.0.test_command.as_deref().unwrap_or("（皇帝尚未设定）");
        ToolSpec {
            name: Capability::RunTests.code().into(),
            description: format!(
                "在项目的临时副本（含已暂存的改动）中运行皇帝设定的测试命令 `{command}`，返回退出码与输出末尾（未通过时作为工具错误返回）。不能指定其他命令。"
            ),
            input_schema_json: r#"{"type":"object","properties":{},"additionalProperties":false}"#
                .into(),
        }
    }

    fn execute(&self, _args_json: &str) -> Result<String, BoxError> {
        self.run(crate::haoshe::Mode::from_env()?)
    }
}

impl RunTests {
    fn run(&self, mode: crate::haoshe::Mode) -> Result<String, BoxError> {
        let command = self
            .0
            .test_command
            .as_deref()
            .ok_or("皇帝尚未设定测试命令（ZIYUAN_TEST_COMMAND）")?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "ziyuan-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let copy = base.join("project");
        let result = self.run_in_copy(command, mode, &base, &copy);
        let _ = fs::remove_dir_all(&base);
        result
    }

    fn run_in_copy(
        &self,
        command: &str,
        mode: crate::haoshe::Mode,
        base: &Path,
        copy: &Path,
    ) -> Result<String, BoxError> {
        copy_tree(&self.0.root, &self.0.root, copy)?;
        for change in self.0.staged.lock().expect("暂存区锁未中毒").values() {
            let target = copy.join(&change.path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(target, &change.after)?;
        }
        let log_path = base.join("output.log");
        let log = fs::File::create(&log_path)?;
        let target_dir: Vec<PathBuf> = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .into_iter()
            .collect();
        let crate::haoshe::Confined {
            command: mut child,
            note: mut haoshe,
            proxy,
        } = crate::haoshe::shell(
            command,
            mode,
            crate::haoshe::Network::from_env()?,
            base,
            &target_dir,
            &[self.0.root.join(".ziyuan")],
        )?;
        child
            .current_dir(copy)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        for key in TEST_ENV {
            if let Some(value) = std::env::var_os(key) {
                child.env(key, value);
            }
        }
        // 临时文件写在副本旁，号舍里也写得了，事后一并清除。
        #[cfg(unix)]
        {
            let tmp = base.join("tmp");
            fs::create_dir_all(&tmp)?;
            child.env("TMPDIR", tmp);
        }
        let mut child = child.spawn()?;
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break Some(status);
            }
            if started.elapsed() > TEST_TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        if let Some(denied) = proxy.map(|p| p.denied()).filter(|d| !d.is_empty()) {
            haoshe.push_str(&format!("；拦下 {}", denied.join("、")));
        }
        let mut log = fs::File::open(&log_path)?;
        let len = log.metadata()?.len();
        log.seek(SeekFrom::Start(len.saturating_sub(MAX_TEST_OUTPUT)))?;
        let mut tail = Vec::new();
        log.read_to_end(&mut tail)?;
        let tail = String::from_utf8_lossy(&tail);
        let passed = status.is_some_and(|s| s.success());
        let verdict = match status {
            None => format!("超时（{} 秒）被终止", TEST_TIMEOUT.as_secs()),
            Some(s) if s.success() => "通过（退出码 0）".to_owned(),
            Some(s) => match s.code() {
                Some(code) => format!("未通过（退出码 {code}）"),
                None => "未通过（被信号终止）".to_owned(),
            },
        };
        let note = if len > MAX_TEST_OUTPUT {
            format!("（仅最后 {MAX_TEST_OUTPUT} 字节）")
        } else {
            String::new()
        };
        let report = format!("测试 `{command}`：{verdict}（{haoshe}）\n输出{note}：\n{tail}");
        // 未通过记为工具失败：审计中 `run_tests` 的成败即测试的成败，供都察院核验。
        if passed {
            Ok(report)
        } else {
            Err(report.into())
        }
    }
}

/// 复制 `from` 到 `to`：跳过禁区、构建产物与符号链接。
fn copy_tree(root: &Path, from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if is_denied(root, &path) || SKIPPED_DIRS.iter().any(|d| name == *d) {
            continue;
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
        assert_eq!(names, ["read_file", "list_files"]);
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
        #[cfg(windows)]
        let check = r"findstr /c:staged src\main.rs >nul && if exist src\new\a.rs if not exist .env if not exist .ziyuan echo COPY-OK";
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

        // 本机没有号舍（如 Windows）时 run_tests 拒绝运行；副本的内容改以 off 照常运行来核对。
        let run_tests = |sealed: &Sealed, tool: &dyn Tool| {
            let result = tool.execute("{}");
            let unavailable = crate::haoshe::confine("true", &[], &Default::default()).is_err()
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
            },
            FileChange {
                path: "src/new/b.rs".into(),
                before: None,
                after: "second file content".into(),
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
}
