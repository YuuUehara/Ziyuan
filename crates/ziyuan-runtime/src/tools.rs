//! 工作区工具。可用的工具与可查阅的路径由诏令的玉玺（[`Seal`]）决定。
//!
//! 所有路径先 canonicalize 再检查，因此 `..`、绝对路径与符号链接都无法越出玉玺的查阅范围。
//! 无论玉玺如何，事件簿与版本库元数据（`.ziyuan`、`.git`、`.jj`）及 `.env*` 文件一律不可读写。
//!
//! `write_file` 不写项目：改动暂存在内存里，执行结束后由 [`Sealed::changes`] 取出呈报，
//! 皇帝准奏后才由 [`apply_changes`] 写入。`run_tests` 在项目的临时副本（叠加暂存改动）中
//! 运行皇帝设定的命令，子进程只继承少数无害的环境变量（不含 API Key）。
use crate::BoxError;
use crate::agent_loop::{Tool, ToolSpec};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use ziyuan_core::{Capability, FileChange, Seal};

/// 单次 `read_file` 最多返回的字节数。
const MAX_READ_BYTES: usize = 64 * 1024;
/// 单次 `search_code` 最多返回的匹配行数。
const MAX_MATCHES: usize = 50;
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
    let tools = seal
        .capabilities
        .iter()
        .map(|capability| -> Box<dyn Tool> {
            match capability {
                Capability::ReadFile => Box::new(ReadFile(sandbox.clone())),
                Capability::SearchCode => Box::new(SearchCode(sandbox.clone())),
                Capability::WriteFile => Box::new(WriteFile(sandbox.clone())),
                Capability::RunTests => Box::new(RunTests(sandbox.clone())),
            }
        })
        .collect();
    Ok(Sealed { tools, sandbox })
}

/// 把皇帝准奏的改动写入项目。先逐一核对：路径仍在项目内且不属禁区，文件内容与呈报时的原文
/// 一致（新建文件则仍不存在）；有任何一处不符则一处也不写。
pub fn apply_changes(root: &Path, changes: &[FileChange]) -> Result<(), BoxError> {
    let root = root.canonicalize()?;
    let mut targets = Vec::new();
    for change in changes {
        let target = contained(&root, &change.path)?;
        let current = match fs::read(&target) {
            Ok(bytes) => Some(String::from_utf8(bytes).map_err(|_| "非 UTF-8 文件")?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        if current != change.before {
            return Err(format!("{} 自呈报以来已被改动，拒绝落盘", change.path).into());
        }
        targets.push((target, &change.after));
    }
    for (target, after) in targets {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(target, after)?;
    }
    Ok(())
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
            let resolved = root
                .join(path)
                .canonicalize()
                .map_err(|e| format!("查阅范围 {path} 无法访问: {e}"))?;
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
            Ok(rel) => rel.display().to_string(),
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
        let result = self.run_in_copy(command, &base, &copy);
        let _ = fs::remove_dir_all(&base);
        result
    }
}

impl RunTests {
    fn run_in_copy(&self, command: &str, base: &Path, copy: &Path) -> Result<String, BoxError> {
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
        let mut child = Command::new("sh");
        child
            .arg("-c")
            .arg(command)
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
        let report = format!("测试 `{command}`：{verdict}\n输出{note}：\n{tail}");
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
        }
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
        for path in [
            "../secret.txt",
            outside.as_str(),
            "link.txt",
            ".env",
            ".ziyuan/court.log",
        ] {
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
    fn seal_limits_tools_and_scope() {
        let dir = workspace("seal");
        let root = dir.join("project");
        let only_read = sealed_tools(&root, &seal(&[Capability::ReadFile], &["src"]), None)
            .unwrap()
            .tools;
        let names: Vec<_> = only_read.iter().map(|t| t.spec().name).collect();
        assert_eq!(names, ["read_file"]);
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
        let check = "grep -q staged src/main.rs && test -f src/new/a.rs && test ! -e .env && test ! -d .ziyuan && echo 副本无误";
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
        ] {
            let args = serde_json::json!({ "path": path, "content": "x" }).to_string();
            assert!(write.execute(&args).is_err(), "{path}");
        }

        let output = test.execute("{}").unwrap();
        assert!(
            output.contains("通过（退出码 0）") && output.contains("副本无误"),
            "{output}"
        );
        let failing = sealed_tools(
            &root,
            &seal(&[Capability::RunTests], &["."]),
            Some("exit 3"),
        )
        .unwrap();
        let output = failing.tools[0].execute("{}").unwrap_err().to_string();
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
        // 事件簿被篡改成越界路径时同样拒绝。
        let evil = [FileChange {
            path: "../evil".into(),
            before: None,
            after: String::new(),
        }];
        assert!(apply_changes(&root, &evil).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
