# 译书并行写安全实现计划

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 按 [设计文档](../specs/2026-10-31-translation-write-safety-design.md) 让多个 CLI 进程与 Web 同时操作同一工作目录的译书数据时，不静默覆盖、不以过期状态批准、中断后可识别并显式恢复。

**Architecture:** 三层：`ziyuan-storage` 新增工作目录写锁、提交世代号与恢复记录；改动集由 `ziyuan-runtime` 的 `BaseState` 推广并改为原子替换；`ziyuan-app` 的协调器把通用准奏与译书材料操作接到同一协议上。CLI / Web 只负责传版本号和映射错误。

**Tech Stack:** Rust（std 文件锁 `File::try_lock`，MSRV 1.89）、serde_json、axum（Web）、现有 `JsonFile` 与事件簿。

**执行方式说明：** Chunk 1、2 给出完整代码，可直接执行。Chunk 3–7 依赖前两块落地后的确切接口，这里先锁定任务、文件、测试与验收；执行到该块前，先按本文件格式补全代码与命令，并经计划审查后再动手。这样避免对尚未存在的接口写出会失效的代码。

**通用约定：**

- 每次命令前：`cd D:/rust-project/ziyuan/.worktrees/v1.1.0; $env:CARGO_TARGET_DIR='D:/rust-project/ziyuan/target'`
- 提交用 jj：`jj commit <文件…> -m <信息>`，信息格式 `type(scope): 中文摘要`，空一行后附 `Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>`。只本地提交，不推送。
- 每块结束跑：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked --no-fail-fast`（输出重定向到 `D:/rust-project/ziyuan/target/<块名>.log` 再检索 `test result:|FAILED|error`）。

---

## Chunk 1: 存储层工作目录写锁与提交世代号

**执行状态：已实现并通过 Windows 验证及两阶段代码审查。** 下方复选框保留原计划步骤，实际执行结果及测试先行偏差以文末执行记录为准；未据此声明 Unix 验收完成。

**Files:**
- Create: `crates/ziyuan-storage/src/workspace.rs`（写锁、世代号、协议标记及其测试）
- Modify: `crates/ziyuan-storage/src/lib.rs:8-11`（声明模块并导出）

设计要点（对应设计文档「存储层」「读取一致性」）：

- 锁文件 `.ziyuan/write.lock` 只加锁、不替换不删除；`.ziyuan` 须为普通目录，符号链接或非目录即拒绝。
- 同进程先在全局「已持有集合」里登记规范化根路径（`Mutex` + `Condvar`），再取跨进程文件锁；两层共用一个等锁预算（默认 5 秒）。不依赖文件锁排斥同进程的其他句柄。
- `lock(LockScope)` 带范围参数，首版只有 `LockScope::Workspace`。
- 世代号文件 `.ziyuan/write.generation`，内容为十进制数加换行；不存在读作 0；原子替换；只有持锁的 `WriteGuard` 能推进：`begin` 要求偶数→奇数，`finish` 要求奇数→偶数。
- 首次加锁写入协议标记 `.ziyuan/write-protocol`（内容 `1\n`），已存在不改。
- 只读的 `Workspace::generation()` 不创建任何文件或目录。

### Task 1.1: 世代号读取不创建文件

- [ ] **Step 1: 写失败测试**

在新文件 `crates/ziyuan-storage/src/workspace.rs` 末尾放测试模块（文件其余部分在 Step 3 补齐，此时先只放测试与最小声明会编译失败，这正是预期）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ziyuan-workspace-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn generation_starts_at_zero_and_reading_creates_nothing() {
        let root = temp_root("gen-read");
        let ws = Workspace::open(&root).unwrap();
        assert_eq!(ws.generation().unwrap(), 0);
        assert!(!root.join(".ziyuan").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
```

在 `crates/ziyuan-storage/src/lib.rs` 的 `pub mod json_file;` 下一行加：

```rust
pub mod workspace;
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ziyuan-storage --lib workspace::tests::generation_starts_at_zero_and_reading_creates_nothing`
Expected: 编译失败，`cannot find type Workspace`。

- [ ] **Step 3: 写最小实现**

在 `workspace.rs` 测试模块之前写入：

```rust
//! 工作目录写锁与提交世代号：同一工作目录内受管写入的互斥与可信读取的依据。
//!
//! - 写锁：同进程先在全局已持有集合中登记规范化根路径，再取 `.ziyuan/write.lock` 的跨进程文件锁，
//!   两层共用一个等锁预算。锁文件只加锁，从不替换或删除；进程退出时由系统释放。
//! - 世代号：`.ziyuan/write.generation`，不存在读作 0。受管写入开始时由偶数推进到奇数，完成后推进到
//!   下一个偶数；只读查询据此判断读到的是否是一致的状态。只有持锁的 [`WriteGuard`] 能推进。
//!
//! 只约束遵守本协议的紫垣进程，不能阻止外部编辑器或旧版本程序写入。
use std::collections::HashSet;
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 默认等锁预算（同进程与跨进程两层合计）。
pub const DEFAULT_WRITE_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
/// 本版写入协议的版本，首次加锁时记入 `.ziyuan/write-protocol`。
pub const WRITE_PROTOCOL: u32 = 1;
const LOCK_POLL: Duration = Duration::from_millis(10);
const CONTROL_DIR: &str = ".ziyuan";
const LOCK_FILE: &str = "write.lock";
const GENERATION_FILE: &str = "write.generation";
const PROTOCOL_FILE: &str = "write-protocol";

/// 写锁的范围。首版只有整个工作目录。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockScope {
    Workspace,
}

#[derive(Debug)]
pub enum WorkspaceError {
    Io(io::Error),
    /// 在 `waited` 内没有等到写锁。
    Timeout { waited: Duration },
    /// 控制目录或其中文件不是普通目录／文件（如符号链接）。
    Unsafe(String),
    /// 世代号文件内容无法解析。
    Corrupt(String),
    /// 写入协议由更新版本登记。
    Newer { found: u64 },
    /// 世代号停在奇数：上次受管写入没有完成，须先恢复。
    Unfinished { generation: u64 },
    /// 要完成的写入并未开始（世代号是偶数）。
    NotStarted { generation: u64 },
}
impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "工作目录写锁读写失败: {e}"),
            Self::Timeout { waited } => write!(
                f,
                "等待工作目录写锁超时（{} 毫秒），可能有其他紫垣进程正在写入",
                waited.as_millis()
            ),
            Self::Unsafe(what) => write!(f, "{what} 不是普通文件或目录（拒绝符号链接）"),
            Self::Corrupt(reason) => write!(f, "提交世代号已损坏: {reason}"),
            Self::Newer { found } => write!(f, "写入协议 {found} 高于本版支持的 {WRITE_PROTOCOL}，请升级"),
            Self::Unfinished { generation } => write!(
                f,
                "上次受管写入未完成（世代 {generation}），须先恢复"
            ),
            Self::NotStarted { generation } => {
                write!(f, "世代 {generation} 没有进行中的受管写入")
            }
        }
    }
}
impl std::error::Error for WorkspaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}
impl From<io::Error> for WorkspaceError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// 一个工作目录的句柄：只记规范化路径与等锁预算，不持有打开的文件。
#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
    timeout: Duration,
}

impl Workspace {
    /// `root` 须已存在；内部以规范化绝对路径为身份与锁键。
    pub fn open(root: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        let root = root.as_ref().canonicalize()?;
        if !fs::metadata(&root)?.is_dir() {
            return Err(WorkspaceError::Unsafe(root.display().to_string()));
        }
        Ok(Self {
            root,
            timeout: DEFAULT_WRITE_LOCK_TIMEOUT,
        })
    }

    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// 规范化绝对路径，也是版本号中的工作目录身份。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 控制目录 `.ziyuan`：不存在时 `create` 为假返回 `None`，为真则创建。存在但不是普通目录即报错。
    fn control_dir(&self, create: bool) -> Result<Option<PathBuf>, WorkspaceError> {
        let dir = self.root.join(CONTROL_DIR);
        match fs::symlink_metadata(&dir) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => Ok(Some(dir)),
            Ok(_) => Err(WorkspaceError::Unsafe(dir.display().to_string())),
            Err(e) if e.kind() == io::ErrorKind::NotFound && create => {
                match fs::create_dir(&dir) {
                    Ok(()) => Ok(Some(dir)),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => self.control_dir(false),
                    Err(e) => Err(e.into()),
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// 当前世代号；不创建任何文件或目录。
    pub fn generation(&self) -> Result<u64, WorkspaceError> {
        match self.control_dir(false)? {
            None => Ok(0),
            Some(dir) => read_generation(&dir),
        }
    }
}

fn read_generation(dir: &Path) -> Result<u64, WorkspaceError> {
    let path = dir.join(GENERATION_FILE);
    let bytes = match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
        Ok(m) if !m.is_file() || m.file_type().is_symlink() => {
            return Err(WorkspaceError::Unsafe(path.display().to_string()));
        }
        Ok(_) => read_control(&path)?,
    };
    std::str::from_utf8(&bytes)
        .ok()
        .and_then(|s| s.strip_suffix('\n'))
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| WorkspaceError::Corrupt(format!("{} 内容不是十进制数", path.display())))
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ziyuan-storage --lib workspace::tests::generation_starts_at_zero_and_reading_creates_nothing`
Expected: `1 passed`。

### Task 1.2: 写锁（同进程与跨线程）

- [ ] **Step 1: 写失败测试**（加到 `tests` 模块）

```rust
    #[test]
    fn write_lock_excludes_the_same_process_and_is_released_on_drop() {
        let root = temp_root("lock-same");
        let ws = Workspace::open(&root)
            .unwrap()
            .with_lock_timeout(Duration::from_millis(100));
        let held = ws.lock(LockScope::Workspace).unwrap();
        let other = Workspace::open(&root)
            .unwrap()
            .with_lock_timeout(Duration::from_millis(100));
        assert!(matches!(
            other.lock(LockScope::Workspace),
            Err(WorkspaceError::Timeout { .. })
        ));
        drop(held);
        drop(other.lock(LockScope::Workspace).unwrap());
        assert_eq!(fs::read_to_string(root.join(".ziyuan/write-protocol")).unwrap(), "1\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_waiting_thread_gets_the_lock_once_it_is_released() {
        let root = temp_root("lock-wait");
        let ws = Workspace::open(&root).unwrap();
        let held = ws.lock(LockScope::Workspace).unwrap();
        let waiter = {
            let ws = ws.clone();
            std::thread::spawn(move || ws.lock(LockScope::Workspace).map(drop))
        };
        std::thread::sleep(Duration::from_millis(50));
        assert!(!waiter.is_finished());
        drop(held);
        waiter.join().unwrap().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_control_dir_is_refused() {
        let root = temp_root("lock-symlink");
        let elsewhere = temp_root("lock-symlink-target");
        std::os::unix::fs::symlink(&elsewhere, root.join(".ziyuan")).unwrap();
        let ws = Workspace::open(&root).unwrap();
        assert!(matches!(ws.lock(LockScope::Workspace), Err(WorkspaceError::Unsafe(_))));
        assert!(matches!(ws.generation(), Err(WorkspaceError::Unsafe(_))));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(elsewhere).unwrap();
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ziyuan-storage --lib workspace::tests`
Expected: 编译失败，`no method named lock`。

- [ ] **Step 3: 写实现**（加到 `impl Workspace` 之后）

```rust
/// 同进程内已持有写锁的根路径；文件锁不保证排斥同进程的其他句柄，所以另记一份。
struct Held {
    roots: Mutex<HashSet<PathBuf>>,
    freed: Condvar,
}

fn held() -> &'static Held {
    static HELD: OnceLock<Held> = OnceLock::new();
    HELD.get_or_init(|| Held {
        roots: Mutex::new(HashSet::new()),
        freed: Condvar::new(),
    })
}

/// 同进程登记；drop 时注销并唤醒等待者。
struct Reservation(PathBuf);
impl Drop for Reservation {
    fn drop(&mut self) {
        let h = held();
        h.roots.lock().expect("锁未中毒").remove(&self.0);
        h.freed.notify_all();
    }
}

/// 持有期间本进程是该工作目录唯一的受管写入者。文件锁在同进程登记之前释放。
pub struct WriteGuard {
    workspace: Workspace,
    dir: PathBuf,
    _file: File,
    _reserved: Reservation,
}

impl Workspace {
    /// 取写锁；超出等锁预算返回 [`WorkspaceError::Timeout`]。会阻塞线程，异步代码须在阻塞线程里调用。
    pub fn lock(&self, scope: LockScope) -> Result<WriteGuard, WorkspaceError> {
        let LockScope::Workspace = scope;
        let dir = self.control_dir(true)?.expect("create 为真时总有目录");
        let started = Instant::now();
        let reserved = {
            let h = held();
            let mut roots = h.roots.lock().expect("锁未中毒");
            while roots.contains(&self.root) {
                let waited = started.elapsed();
                if waited >= self.timeout {
                    return Err(WorkspaceError::Timeout { waited });
                }
                roots = h
                    .freed
                    .wait_timeout(roots, self.timeout - waited)
                    .expect("锁未中毒")
                    .0;
            }
            roots.insert(self.root.clone());
            Reservation(self.root.clone())
        };
        let lock_path = dir.join(LOCK_FILE);
        require_regular_if_present(&lock_path)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        if !file.metadata()?.is_file() {
            return Err(WorkspaceError::Unsafe(lock_path.display().to_string()));
        }
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(e)) => return Err(e.into()),
            }
            let waited = started.elapsed();
            if waited >= self.timeout {
                return Err(WorkspaceError::Timeout { waited });
            }
            std::thread::sleep(LOCK_POLL.min(self.timeout - waited));
        }
        let protocol = dir.join(PROTOCOL_FILE);
        if require_regular_if_present(&protocol)? {
            let bytes = read_control(&protocol)?;
            if bytes != format!("{WRITE_PROTOCOL}\n").as_bytes() {
                let found = std::str::from_utf8(&bytes).ok()
                    .and_then(|s| s.strip_suffix('\n'))
                    .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|s| s.parse::<u64>().ok());
                return Err(match found {
                    Some(found) if found > u64::from(WRITE_PROTOCOL) => WorkspaceError::Newer { found },
                    _ => WorkspaceError::Corrupt("write-protocol 内容无效".into()),
                });
            }
        } else {
            replace_file(&protocol, format!("{WRITE_PROTOCOL}\n").as_bytes())?;
        }
        Ok(WriteGuard {
            workspace: self.clone(),
            dir,
            _file: file,
            _reserved: reserved,
        })
    }
}

/// 稳定路径的普通文件检查；不承诺防住恶意并发路径替换（设计范围外）。
fn require_regular_if_present(path: &Path) -> Result<bool, WorkspaceError> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && !m.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(WorkspaceError::Unsafe(path.display().to_string())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn read_control(path: &Path) -> Result<Vec<u8>, WorkspaceError> {
    let mut bytes = Vec::new();
    File::open(path)?.take(33).read_to_end(&mut bytes)?;
    if bytes.len() > 32 {
        return Err(WorkspaceError::Corrupt(format!("{} 超过 32 字节", path.display())));
    }
    Ok(bytes)
}

fn rename_control(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        let started = Instant::now();
        loop {
            match fs::rename(from, to) {
                Err(e) if matches!(e.raw_os_error(), Some(5 | 32 | 33))
                    && started.elapsed() < Duration::from_millis(250) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                result => return result,
            }
        }
    }
    #[cfg(not(windows))]
    fs::rename(from, to)
}

/// 同目录唯一临时文件、刷盘、改名覆盖；失败时删掉临时文件，旧文件不动。
fn replace_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().expect("路径有上级目录");
    let name = path.file_name().expect("路径有文件名");
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let (tmp, mut file) = loop {
        let mut tmp_name = std::ffi::OsString::from(".");
        tmp_name.push(name);
        tmp_name.push(format!(
            ".{}.{nanos}.{}.tmp",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let tmp = dir.join(tmp_name);
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(file) => break (tmp, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    match written.and_then(|()| rename_control(&tmp, path)) {
        Ok(()) => {
            // 与 JsonFile 的既有策略一致：平台支持时尽力同步目录，不承诺断电安全。
            #[cfg(unix)]
            if let Ok(dir) = File::open(dir) {
                let _ = dir.sync_all();
            }
            Ok(())
        },
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p ziyuan-storage --lib workspace::tests`
Expected: 全部通过（Windows 上 symlink 用例不编译）。

### Task 1.3: 跨进程互斥

- [ ] **Step 1: 写失败测试**（沿用 `json_file.rs` 的子进程模式）

```rust
    const CHILD_ENV: &str = "ZIYUAN_WORKSPACE_LOCK_CHILD";

    /// 由 [`another_process_holding_the_lock_blocks_writers`] 以子进程运行；直接运行时什么都不做。
    /// 取锁后写 `held` 标记，等父进程写 `release` 后退出。
    #[test]
    fn child_holds_the_write_lock() {
        let Some(dir) = std::env::var_os(CHILD_ENV) else {
            return;
        };
        let root = PathBuf::from(dir);
        let ws = Workspace::open(&root).unwrap();
        let guard = ws.lock(LockScope::Workspace).unwrap();
        fs::write(root.join("held"), "").unwrap();
        let started = Instant::now();
        while !root.join("release").exists() {
            assert!(started.elapsed() < Duration::from_secs(60), "父进程没有放行");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(guard);
    }

    #[test]
    fn another_process_holding_the_lock_blocks_writers() {
        let root = temp_root("lock-process");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "workspace::tests::child_holds_the_write_lock",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, &root)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let started = Instant::now();
        while !root.join("held").exists() {
            assert!(started.elapsed() < Duration::from_secs(60), "子进程没有取到锁");
            std::thread::sleep(Duration::from_millis(10));
        }
        let ws = Workspace::open(&root)
            .unwrap()
            .with_lock_timeout(Duration::from_millis(100));
        assert!(matches!(
            ws.lock(LockScope::Workspace),
            Err(WorkspaceError::Timeout { .. })
        ));
        fs::write(root.join("release"), "").unwrap();
        assert!(child.wait().unwrap().success());
        let ws = ws.with_lock_timeout(Duration::from_secs(10));
        drop(ws.lock(LockScope::Workspace).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
```

实现测试时为子进程加 RAII 清理（Drop 时 kill + wait），避免父进程断言失败后留下子进程。

- [ ] **Step 2: 运行**

Run: `cargo test -p ziyuan-storage --lib workspace::tests::another_process_holding_the_lock_blocks_writers`
Expected: 通过（实现已在 Task 1.2 完成）。若子进程模式本身有错（如测试名拼错），会表现为“子进程没有取到锁”的断言失败，须先修正测试。为确认测试真的在测锁，临时把 `lock` 里的 `try_lock` 循环注释掉运行一次，必须失败，再恢复。

### Task 1.4: 世代号推进

- [ ] **Step 1: 写失败测试**

```rust
    #[test]
    fn generation_moves_even_to_odd_to_even_only_under_the_lock() {
        let root = temp_root("gen-move");
        let ws = Workspace::open(&root).unwrap();
        let guard = ws.lock(LockScope::Workspace).unwrap();
        assert!(matches!(guard.finish(), Err(WorkspaceError::NotStarted { generation: 0 })));
        assert_eq!(guard.begin().unwrap(), 1);
        assert!(matches!(guard.begin(), Err(WorkspaceError::Unfinished { generation: 1 })));
        assert_eq!(ws.generation().unwrap(), 1);
        assert_eq!(guard.finish().unwrap(), 2);
        drop(guard);
        assert_eq!(ws.generation().unwrap(), 2);
        fs::write(root.join(".ziyuan/write.generation"), "two\n").unwrap();
        assert!(matches!(ws.generation(), Err(WorkspaceError::Corrupt(_))));
        fs::remove_dir_all(root).unwrap();
    }
```

- [ ] **Step 2: 运行确认失败**：`no method named begin`。

- [ ] **Step 3: 写实现**

```rust
impl WriteGuard {
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    pub fn generation(&self) -> Result<u64, WorkspaceError> {
        read_generation(&self.dir)
    }

    /// 开始一项受管写入：偶数推进到奇数。已是奇数说明上次没有完成。
    pub fn begin(&self) -> Result<u64, WorkspaceError> {
        let current = self.generation()?;
        if !current.is_multiple_of(2) {
            return Err(WorkspaceError::Unfinished { generation: current });
        }
        self.advance(current)
    }

    /// 完成一项受管写入：奇数推进到下一个偶数。
    pub fn finish(&self) -> Result<u64, WorkspaceError> {
        let current = self.generation()?;
        if current.is_multiple_of(2) {
            return Err(WorkspaceError::NotStarted { generation: current });
        }
        self.advance(current)
    }

    fn advance(&self, current: u64) -> Result<u64, WorkspaceError> {
        let next = current.checked_add(1)
            .ok_or_else(|| WorkspaceError::Corrupt("世代号已到上限".into()))?;
        self.store(next)
    }

    fn store(&self, next: u64) -> Result<u64, WorkspaceError> {
        replace_file(&self.dir.join(GENERATION_FILE), format!("{next}\n").as_bytes())?;
        Ok(next)
    }
}
```

- [ ] **Step 4: 运行确认通过**：`cargo test -p ziyuan-storage --lib workspace::tests` 全部通过。

- [ ] **Step 5: 导出并提交**

`lib.rs` 在 `pub use json_file::…;` 下加：

```rust
pub use workspace::{LockScope, Workspace, WorkspaceError, WriteGuard};
```

跑本块收尾检查（fmt、clippy、`cargo test -p ziyuan-storage`），然后：

```powershell
jj commit crates/ziyuan-storage/src/workspace.rs crates/ziyuan-storage/src/lib.rs -m <见通用约定，摘要：feat(storage): 工作目录写锁与提交世代号>
```

---

### 第一块审查决定与补充测试（2026-10-31）

本节为 Chunk 1 的必需验收条件，补充测试均须先于实现编写并见证失败，且在 Task 1.4 Step 5 提交前全部通过；平台专用测试仅在对应平台运行。前面代码已合入限长读取、时间戳与 Windows 重试，新增测试按这里的条件逐一实现。

- 采纳：协议标记严格校验、不吞掉非 NotFound 错误；锁文件拒绝已存在的符号链接／目录；根必须是目录；世代溢出拒绝且保持文件原样；存储错误不引用尚未实现的 CLI；持锁句柄拥有 Workspace，便于阻塞线程使用。
- 保持设计边界：静态符号链接／Windows junction 必须拒绝，但不新加 libc 或特定平台 API 来防恶意检查后替换；恶意路径替换不在本设计支持范围内。
- 原子替换暂不抽出并修改既有 JsonFile，避免本块扩大回归面；同目录临时文件使用 PID、时间纳秒和计数器，替换后尽力同步目录。
- Windows `rename` 遇到访问拒绝／共享冲突（原始错误 5、32、33）可以在独立 250ms 预算内重试，超时仍返回原始 I/O 错误，绝不先删目标。此预算不是等锁预算，也不能据此宣称故障已恢复。
- 先测试再实现以下补充用例：缺失／非目录根；协议 1 不改写、协议 2 拒绝、垃圾／目录协议拒绝；世代 `u64::MAX` 完成失败且文件原样；世代损坏；200 次 begin/finish 与只读线程并行且不残留 tmp；Unix 的锁／世代／协议符号链接；Windows 的控制目录 junction。允许因明确的 OS 权限限制跳过创建 symlink，但 junction 用例不得静默跳过。
- 增加子进程被终止后的锁释放测试；通过进程间标记同步，所有子进程由 RAII guard 在失败时终止并回收。超时测试检查持锁期间实际等待，阈值为预算下限与宽松上限，不用易抖动的“严格小于两倍”作为 CI 门槛。共享预算测试先由同进程占用一部分预算，再由子进程持锁占用剩余预算，验证没有重新获得完整预算。
- `read_generation` 保留既定十进制加 LF 格式，首版允许前导零，不加无必要的迁移限制；限长读取（至多 32 字节并确认 EOF），避免损坏控制文件导致无界内存读取。协议标记同样限长读取。
- 本块是基础设施，尚无领域写入接入；不得将它描述成已交付译书并行写安全。Chunk 2–7 仍独立完成。

---

## Chunk 2: 准奏落盘改为原子替换

**Files:**
- Modify: `crates/ziyuan-runtime/src/tools.rs:217-237`（`apply_changes`）
- Test: 同文件测试模块（`tools.rs:1090` 附近的现有落盘用例旁）

只解决“半份文件”：每个目标改用同目录临时文件、刷盘、改名覆盖。多文件之间的一致性与恢复由 Chunk 3 的恢复记录负责，本块不改 `BaseState` 语义与函数签名。

### Task 2.1: 落盘不留临时文件、不出现半份内容

- [ ] **Step 1: 写失败测试**（加到 `tools.rs` 测试模块）

```rust
    #[test]
    fn apply_changes_replaces_targets_atomically() {
        let dir = std::env::temp_dir().join(format!("ziyuan-apply-atomic-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/a.rs"), "old").unwrap();
        let changes = [
            FileChange { path: "src/a.rs".into(), before: Some("old".into()), after: "new".into() },
            FileChange { path: "src/b/c.rs".into(), before: None, after: "made".into() },
        ];
        fault::fail_replace_after(1);
        assert!(apply_changes(&dir, &changes).is_err());
        fault::clear();
        // 第一个已替换，第二个替换失败：第一个是完整新内容，第二个不存在，没有残留的临时文件。
        assert_eq!(fs::read_to_string(dir.join("src/a.rs")).unwrap(), "new");
        assert!(!dir.join("src/b/c.rs").exists());
        let leftovers: Vec<_> = walk(&dir).into_iter().filter(|p| p.ends_with(".tmp")).collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        apply_changes(&dir, &changes).unwrap();
        assert_eq!(fs::read_to_string(dir.join("src/b/c.rs")).unwrap(), "made");
        fs::remove_dir_all(dir).unwrap();
    }

    fn walk(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path.display().to_string());
            }
        }
        out
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p ziyuan-runtime --lib tools::tests::apply_changes_replaces_targets_atomically`
Expected: 编译失败，`cannot find module fault`。

- [ ] **Step 3: 写实现**

把 `apply_changes` 末尾循环里的 `fs::write(target, after)?;` 改为 `replace_target(&target, after.as_bytes())?;`，并在 `apply_changes` 之后加：

```rust
/// 同目录唯一临时文件、刷盘、改名覆盖：中断时目标要么是旧内容，要么是完整的新内容。
fn replace_target(target: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = target.parent().expect("目标在项目根目录之内");
    let name = target.file_name().expect("目标有文件名");
    let (tmp, mut file) = loop {
        let mut tmp_name = std::ffi::OsString::from(".");
        tmp_name.push(name);
        tmp_name.push(format!(
            ".{}.{}.tmp",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let tmp = dir.join(tmp_name);
        match fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(file) => break (tmp, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let replaced = written.and_then(|()| {
        #[cfg(test)]
        fault::before_replace()?;
        fs::rename(&tmp, target)
    });
    if replaced.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    replaced
}

/// 测试用：让第 n 次之后的替换失败。用线程局部变量，避免并行测试互相干扰。
#[cfg(test)]
mod fault {
    use std::cell::Cell;
    thread_local!(static LEFT: Cell<Option<usize>> = const { Cell::new(None) });
    pub(super) fn fail_replace_after(n: usize) {
        LEFT.with(|l| l.set(Some(n)));
    }
    pub(super) fn clear() {
        LEFT.with(|l| l.set(None));
    }
    pub(super) fn before_replace() -> std::io::Result<()> {
        LEFT.with(|l| match l.get() {
            Some(0) => Err(std::io::Error::other("注入的替换失败")),
            Some(n) => {
                l.set(Some(n - 1));
                Ok(())
            }
            None => Ok(()),
        })
    }
}
```

（`AtomicU64`、`Ordering` 已在文件顶部导入。）

- [ ] **Step 4: 运行确认通过**：该用例与原有落盘用例（`tools.rs:1090` 起）全部通过。

- [ ] **Step 5: 提交**：收尾检查后 `jj commit crates/ziyuan-runtime/src/tools.rs`，摘要 `fix(runtime): 准奏落盘改为原子替换`。

---

## Chunk 3: 恢复记录与通用准奏接入（执行前补全代码）

**Files:**
- Create: `crates/ziyuan-storage/src/recovery.rs`（记录格式、状态机、读写、版本与上限）
- Create: `crates/ziyuan-app/src/write_txn.rs`（协调器：锁内重读→计划→发布→落盘→记事件→完成）
- Modify: `crates/ziyuan-app/src/edicts.rs:416-450`（`apply_reviewed` 走协调器）
- Modify: `crates/ziyuan-app/src/session.rs:193-226`（`commit` 在持锁时提交确切事件的入口，不自动重提议）
- Modify: `crates/ziyuan-runtime/src/tools.rs`（导出逐目标 `Absent`／`Present(hash)` 分类，供恢复使用）

任务：

1. 记录类型：`prepared`／`published`／`completed`／`abandoned`；每目标旧／新 `Absent`／`Present(sha256)`；准奏类记录诏令号、`changes_version`、跳过段落、预期事件簿位置、待写事件的确切编码；格式版本、清单 8 MiB 上限；更新版本写入的记录拒绝读取。测试：往返、损坏、过新、超限。
2. 协调器把 `apply_reviewed` 改为：取写锁→无未完成记录→重放并核对 `changes_version`→`BaseState` 预检→发布记录→`begin`→逐目标原子替换（复核旧／新摘要）→追加确切事件（期望位置不符即停）→记 `completed`→`finish`。测试：成功路径；版本不符拒绝；外部改动拒绝且不写；发布后写锁阻止其他写入。
3. 所有 `Session::commit` 调用在不经协调器时也取写锁（只锁不记）；协调器内部用“已持锁”入口，避免自锁。测试：两个线程同时准奏同一诏令，只有一个成功，事件只记一次。
4. 进程中断故障注入：在第 1／中间／最后一个替换前后与事件追加前后让子进程退出（沿用 Chunk 1 子进程模式，用环境变量指定中断点）。测试：无半份文件，记录可读，后续写入被拒。

## Chunk 4: 恢复命令（执行前补全代码）

**Files:**
- Create: `crates/ziyuan-app/src/recovery.rs`（status／finish／abandon 的规则）
- Modify: `crates/ziyuan-cli/src/text_commands.rs`（`text recovery status|finish|abandon`）
- Test: `crates/ziyuan-app/tests/recovery.rs`、`crates/ziyuan-cli/tests/text_project.rs`

任务：status 只读列出记录与每目标现状；finish 按设计逐条核对（目标只能是旧或新，事件只能完整或确切前缀，跳过段落与记录一致，否则拒绝）；终态但世代为奇数时只收尾；abandon 只在全部为旧且无事件时允许。测试覆盖设计文档验收表中的中断、前缀、第三种内容、换跳过段落重试、终态未收尾。

## Chunk 5: 译书材料操作接入（执行前补全代码）

**Files:**
- Modify: `crates/ziyuan-app/src/translation/context.rs:240-283`（草稿写入、批准）
- Modify: `crates/ziyuan-app/src/translation/zhupi.rs:51-110`（朱批）
- Modify: `crates/ziyuan-app/src/notes.rs:259-360`（札记增删改、晋升）
- Modify: `crates/ziyuan-app/src/translation/export.rs`、`session.rs:347-455`（导出、下诏、定稿、退回）
- Modify: `crates/ziyuan-app/src/text_project/store.rs`（建项、导入）

任务：逐入口列清单并接入写锁与世代号；多文件材料操作（晋升＝札记＋术语草稿；上下文批准＝快照目录＋清单）走暂存内容的恢复记录；删除只用于材料操作。每个入口一个独立进程竞争测试（CLI 子进程对 CLI 子进程）。

## Chunk 6: 版本号与可信读取、CLI／Web 错误映射（执行前补全代码）

**Files:**
- Create: `crates/ziyuan-app/src/versions.rs`（按设计文档版本表计算 `sha256:` 版本）
- Modify: `crates/ziyuan-web/src/text.rs`、`crates/ziyuan-web/src/edicts.rs:139`、`crates/ziyuan-web/src/state.rs`（`expected_version`，409／503+`Retry-After`／500 映射，等锁进阻塞线程）
- Modify: `crates/ziyuan-web/src/index.html`（过期时保留输入、提示刷新，不自动重试）
- Modify: `crates/ziyuan-cli/src/text_commands.rs`、`translation_commands.rs`（`--expected-version`，预览输出带版本，退出码区分）

任务：准奏版本保持现有 `changes_version` 不变；只读 API 按世代协议读取，不稳定时返回“写入中／待恢复”；Web 编辑和批准缺版本返回 400。测试：同进程两个 Web 请求竞争；写入期间读三栏材料不返回混合结果；过期预览冲突。

## Chunk 7: 文档、跨平台验证与验收记录（执行前补全）

**Files:**
- Modify: `docs/novel-translation.md`、`docs/guide.md`（如有 CLI 参考）、`CHANGELOG.md`、`SECURITY.md`（承诺边界）
- Create: `docs/acceptance/1.2.1.md`（版本号在此确定）

任务：写明支持边界（本地磁盘、遵守协议的紫垣进程、外部改动检测后拒绝、不与旧版本混用）、新参数与状态码、恢复步骤与人工停机恢复；Windows 本机全量检查，Linux／macOS 以 CI 结果记录，未覆盖的场景如实列为未测。

## 执行记录（2026-10-31）

- main 的既有 CI `41100667588` 已完成且成功，对应 `3a910d4bad87728d56a072c62d59bc376e4a0091`；不是本次未推送改动的 CI 结果。
- Chunk 1 实现采用 `Arc<PathBuf>` 共享规范化根路径，持锁句柄拥有 Workspace；`begin`／`finish` 接收可变句柄，显式表达写入动作。规格审查认为不改变协议。
- 工作目录根身份仍使用标准库 canonicalize，后续版本摘要必须复用同一路径；静态 Windows reparse point 也拒绝，不只检查符号链接。
- 测试先行过程的实际限制：根／锁 API 首轮 RED 为缺少 API 的编译失败；世代号有真实行为断言失败再通过的记录。跨进程、压力、路径防护等补充测试对前一轮实现直接通过，不能声称每项补充测试均见证过失败。保留这些覆盖，不用事后伪造历史来满足计划文字。
- 定向验证：Windows 的存储单元测试 37 通过（其中工作目录模块 13 个），doc-tests 0；格式检查与 `cargo clippy -p ziyuan-storage --all-targets --locked -- -D warnings` 通过。日志位于 `D:/rust-project/ziyuan/target/workspace-lock-{test,clippy,fmt}.log`。
- Windows 全工作区验证通过：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked --no-fail-fast` 均退出 0；28 组结果合计 314 个测试通过、0 失败，包含 0 个用例的 doc-tests 组。日志位于 `D:/rust-project/ziyuan/target/workspace-lock-full-{fmt,clippy,test}.log`。
- 规格审查与随后独立的代码质量审查均批准，未发现阻塞性问题；Unix 专用符号链接测试已编写但在本机未运行，尚不能据此声称 Linux／macOS 验收通过。
- 目前只实现基础设施；恢复记录、通用准奏、译书材料写入、CLI／Web 版本与错误映射及向量记忆库仍未实现。
