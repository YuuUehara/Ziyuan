//! Workspace-wide managed write exclusion and generation tracking.
//!
//! Control files are replaced atomically; this does not promise power-loss safety.
//! Static symlinks/reparse points are refused. Concurrent malicious path replacement
//! is outside this API's threat model.
use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::recovery::{
    MAX_RECORD_BYTES, RecordState, RecoveryError, RecoveryRecord, decode_record, encode_record,
};

pub const DEFAULT_WRITE_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
pub const WRITE_PROTOCOL: u64 = 1;
/// The single pending-or-terminal recovery record, relative to `.ziyuan`.
const RECORD_FILE: &str = "recovery.json";

#[derive(Debug)]
pub enum WorkspaceError {
    Io(io::Error),
    Timeout { waited: Duration },
    Unsafe(String),
    Corrupt(String),
    Newer { found: u64 },
    Unfinished { generation: u64 },
    NotStarted { generation: u64 },
    Recovery(RecoveryError),
    PendingRecovery { operation_id: String },
    FinishPending { operation_id: String },
    RecordMismatch { operation_id: String },
    NoPendingRecord,
    RecordExists { operation_id: String },
}
impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "workspace I/O failed: {e}"),
            Self::Timeout { waited } => {
                write!(f, "workspace write lock timed out after {waited:?}")
            }
            Self::Unsafe(reason) => write!(f, "unsafe workspace path: {reason}"),
            Self::Corrupt(reason) => write!(f, "corrupt workspace control file: {reason}"),
            Self::Newer { found } => write!(
                f,
                "workspace write protocol {found} requires a newer version"
            ),
            Self::Unfinished { generation } => {
                write!(f, "workspace write generation {generation} is unfinished")
            }
            Self::NotStarted { generation } => {
                write!(f, "workspace write generation {generation} has not started")
            }
            Self::Recovery(e) => write!(f, "workspace recovery record is unusable: {e}"),
            Self::PendingRecovery { operation_id } => write!(
                f,
                "workspace has a published recovery operation {operation_id}: run `ziyuan text recovery status` and finish it before writing"
            ),
            Self::FinishPending { operation_id } => write!(
                f,
                "recovery operation {operation_id} is finished but not closed: run `ziyuan text recovery finish {operation_id} --yes`"
            ),
            Self::RecordMismatch { operation_id } => {
                write!(f, "recovery record does not match operation {operation_id}")
            }
            Self::NoPendingRecord => write!(f, "workspace has no recovery record"),
            Self::RecordExists { operation_id } => write!(
                f,
                "workspace recovery record for operation {operation_id} already exists"
            ),
        }
    }
}
impl std::error::Error for WorkspaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Recovery(e) => Some(e),
            _ => None,
        }
    }
}
impl From<io::Error> for WorkspaceError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<RecoveryError> for WorkspaceError {
    fn from(error: RecoveryError) -> Self {
        Self::Recovery(error)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum LockScope {
    Workspace,
}

/// A cheap, independently configurable handle to an existing canonical root.
#[derive(Clone, Debug)]
pub struct Workspace {
    root: Arc<PathBuf>,
    timeout: Duration,
}
impl Workspace {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        let root = fs::canonicalize(root)?;
        if !root.is_dir() {
            return Err(WorkspaceError::Unsafe("root is not a directory".into()));
        }
        Ok(Self {
            root: Arc::new(root),
            timeout: DEFAULT_WRITE_LOCK_TIMEOUT,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Read only: an absent generation is zero, without creating any paths.
    pub fn generation(&self) -> Result<u64, WorkspaceError> {
        let dir = self.root.join(".ziyuan");
        if !check_path(&dir, true)? {
            return Ok(0);
        }
        match read_control(&dir.join("write.generation"))? {
            None => Ok(0),
            Some(bytes) => decimal(&bytes),
        }
    }

    /// Acquire managed write exclusion. The scope is reserved for future expansion.
    pub fn lock(&self, _scope: LockScope) -> Result<WriteGuard, WorkspaceError> {
        let started = Instant::now();
        let reserved = Reservation::acquire(self.root.clone(), started, self.timeout)?;
        let dir = self.root.join(".ziyuan");
        if !check_path(&dir, true)? {
            match fs::create_dir(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
            check_path(&dir, true)?;
        }
        let path = dir.join("write.lock");
        check_path(&path, false)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        if !file.metadata()?.is_file() {
            return Err(WorkspaceError::Unsafe(path.display().to_string()));
        }
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::Error(e)) => return Err(e.into()),
                Err(TryLockError::WouldBlock) => {
                    let remaining = self.timeout.saturating_sub(started.elapsed());
                    if remaining.is_zero() {
                        return Err(WorkspaceError::Timeout {
                            waited: started.elapsed(),
                        });
                    }
                    std::thread::sleep(remaining.min(Duration::from_millis(10)));
                }
            }
        }
        let guard = WriteGuard {
            workspace: self.clone(),
            _file: file,
            _reserved: reserved,
        };
        match read_control(&dir.join("write-protocol"))? {
            None => atomic_replace(&dir.join("write-protocol"), b"1\n")?,
            Some(bytes) if bytes == b"1\n" => {}
            Some(bytes) => {
                let found = decimal(&bytes)?;
                return Err(if found > WRITE_PROTOCOL {
                    WorkspaceError::Newer { found }
                } else {
                    WorkspaceError::Corrupt("unsupported write protocol".into())
                });
            }
        }
        Ok(guard)
    }

    /// Read only: the single pending-or-terminal recovery record.
    ///
    /// The file is read without the write lock, so a caller holding the lock sees a
    /// stable record. An absent file is `None`; an unreadable one is an error rather
    /// than a reset.
    pub fn pending_record(&self) -> Result<Option<RecoveryRecord>, WorkspaceError> {
        let dir = self.root.join(".ziyuan");
        if !check_path(&dir, true)? {
            return Ok(None);
        }
        read_record(&dir.join(RECORD_FILE))
    }

    /// 未收尾恢复记录里引用的全部目标路径（`/` 分隔的工作区相对路径）。
    ///
    /// 清理临时文件时用它作受保护集合：记录引用的路径一律不动。
    /// 记录读不出来是错误而不是空集——宁可整轮不清理，也不要误删记录引用的项。
    ///
    /// 记录里的路径确实是目标文件的**最终**路径（如 `src/main.rs`），通常与临时名不同；
    /// 但项目文件可以恰好就叫 `.ziyuan-tmp-<pid>-<nanos>-<counter>`（写入禁区只排除
    /// `.ziyuan`、`.git`、`.jj`、`.env*`），那时它按严格命名正是清理候选，集合命中是
    /// 真实的保护而不只是接口。集合构造放在 storage：`ziyuan-app` 与 `ziyuan-cli` 都要
    /// 用同一份，runtime 不该为此依赖 storage。
    pub fn protected_targets(&self) -> Result<BTreeSet<String>, WorkspaceError> {
        let mut protected = BTreeSet::new();
        if let Some(record) = self.pending_record()? {
            for target in &record.targets {
                protected.insert(target.path.clone());
            }
        }
        Ok(protected)
    }

    /// Refuse ordinary managed writes while an interrupted or unfinished operation
    /// exists. A `Prepared` record is private staging and does not block.
    pub fn ensure_writable(&self) -> Result<(), WorkspaceError> {
        let generation = self.generation()?;
        let record = self.pending_record()?;
        if let Some(record) = &record {
            match record.state {
                RecordState::Published => {
                    return Err(WorkspaceError::PendingRecovery {
                        operation_id: record.operation_id.clone(),
                    });
                }
                RecordState::Completed | RecordState::Abandoned => {
                    if generation % 2 != 0 {
                        return Err(WorkspaceError::FinishPending {
                            operation_id: record.operation_id.clone(),
                        });
                    }
                }
                RecordState::Prepared => {}
            }
        }
        if generation % 2 != 0 {
            return Err(WorkspaceError::Unfinished { generation });
        }
        Ok(())
    }
}

/// The caller's verdict that a published operation left no trace: every target still
/// matches `before` and none of the batch's events reached the event log.
///
/// Constructed by the code that actually read the files and the log, so the decision is
/// named where it is made instead of being passed as a bare `bool`.
#[derive(Debug, Clone, Copy)]
pub struct Untouched {
    confirmed: bool,
}

impl Untouched {
    /// The caller verified nothing was written.
    pub fn confirmed() -> Self {
        Self { confirmed: true }
    }

    /// The caller found traces of the operation, or could not rule them out.
    pub fn ruled_out() -> Self {
        Self { confirmed: false }
    }
}

/// The file is dropped before the in-process reservation. Dropping never finishes
/// an interrupted generation.
pub struct WriteGuard {
    workspace: Workspace,
    _file: File,
    _reserved: Reservation,
}
impl WriteGuard {
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    pub fn generation(&self) -> Result<u64, WorkspaceError> {
        self.workspace.generation()
    }

    /// Persist an odd generation before starting any managed writes.
    pub fn begin(&mut self) -> Result<u64, WorkspaceError> {
        let generation = self.generation()?;
        if generation % 2 != 0 {
            return Err(WorkspaceError::Unfinished { generation });
        }
        self.advance(generation)
    }

    /// Persist an even generation after all managed writes have completed.
    pub fn finish(&mut self) -> Result<u64, WorkspaceError> {
        let generation = self.generation()?;
        if generation % 2 == 0 {
            return Err(WorkspaceError::NotStarted { generation });
        }
        self.advance(generation)
    }

    fn advance(&self, generation: u64) -> Result<u64, WorkspaceError> {
        let next = generation
            .checked_add(1)
            .ok_or_else(|| WorkspaceError::Corrupt("generation overflow".into()))?;
        atomic_replace(
            &self.workspace.root.join(".ziyuan/write.generation"),
            format!("{next}\n").as_bytes(),
        )?;
        Ok(next)
    }

    /// `begin`, for callers that reserve the odd generation before planning.
    pub fn reserve(&mut self) -> Result<u64, WorkspaceError> {
        self.begin()
    }

    /// Write the single recovery record atomically.
    ///
    /// Only `Prepared` and `Published` may be published; a record already present
    /// (in any state) is never overwritten here — `complete`, `abandon` and
    /// `remove_record` are the only ways past it.
    pub fn publish(&mut self, record: &RecoveryRecord) -> Result<(), WorkspaceError> {
        if !matches!(record.state, RecordState::Prepared | RecordState::Published) {
            return Err(WorkspaceError::Unsafe(format!(
                "recovery record state {:?} cannot be published",
                record.state
            )));
        }
        // recovery.rs does not canonicalize, so compare against the root this handle
        // was opened with. Only the separator form and a leading verbatim prefix may
        // differ; any other textual difference is a different workspace.
        let root = self.workspace.root();
        if !same_root(root, &record.workspace) {
            return Err(WorkspaceError::Unsafe(format!(
                "recovery record workspace {} is not this workspace {}",
                record.workspace.display(),
                root.display()
            )));
        }
        let path = self.workspace.root().join(".ziyuan").join(RECORD_FILE);
        if check_path(&self.workspace.root().join(".ziyuan"), true)? && self.readable()?.is_some() {
            return Err(WorkspaceError::RecordExists {
                operation_id: record.operation_id.clone(),
            });
        }
        let bytes = encode_record(record)?;
        atomic_replace(&path, &bytes)
    }

    /// Mark the published record completed. Does not advance the generation.
    pub fn complete(&mut self, operation_id: &str) -> Result<RecoveryRecord, WorkspaceError> {
        let mut record = self.matching(operation_id, RecordState::Published)?;
        record.state = RecordState::Completed;
        self.rewrite(&record)?;
        Ok(record)
    }

    /// Mark the prepared record abandoned. Does not advance the generation.
    pub fn abandon(&mut self, operation_id: &str) -> Result<RecoveryRecord, WorkspaceError> {
        let mut record = self.matching(operation_id, RecordState::Prepared)?;
        record.state = RecordState::Abandoned;
        self.rewrite(&record)?;
        Ok(record)
    }

    /// Mark a **published** record abandoned. Does not advance the generation.
    ///
    /// A published record normally means "files may already be written", so abandoning it
    /// discards an approved write. The caller must have verified that nothing happened:
    /// every target still matches `before` and this batch's events are not in the event log.
    /// `observed` is that verdict — the store cannot check the files itself, but refusing to
    /// act on a bare `false` keeps the decision explicit at the call site instead of implicit.
    pub fn abandon_published(
        &mut self,
        operation_id: &str,
        observed: Untouched,
    ) -> Result<RecoveryRecord, WorkspaceError> {
        if !observed.confirmed {
            return Err(WorkspaceError::Unsafe(format!(
                "published record {operation_id} was not confirmed untouched; refusing to abandon"
            )));
        }
        let mut record = self.matching(operation_id, RecordState::Published)?;
        record.state = RecordState::Abandoned;
        self.rewrite(&record)?;
        Ok(record)
    }

    /// Delete the record. The caller advances the generation with `finish` first;
    /// a still-published record is never removed, so no one can "unlock" by deletion.
    pub fn remove_record(&mut self) -> Result<(), WorkspaceError> {
        let Some(record) = self.readable()? else {
            return Err(WorkspaceError::NoPendingRecord);
        };
        if !matches!(
            record.state,
            RecordState::Completed | RecordState::Abandoned
        ) {
            return Err(WorkspaceError::Unsafe(format!(
                "recovery record state {:?} cannot be removed",
                record.state
            )));
        }
        let path = self.workspace.root().join(".ziyuan").join(RECORD_FILE);
        check_path(&path, false)?;
        fs::remove_file(&path)?;
        #[cfg(unix)]
        if let Ok(dir) = File::open(self.workspace.root().join(".ziyuan")) {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    fn readable(&self) -> Result<Option<RecoveryRecord>, WorkspaceError> {
        self.workspace.pending_record()
    }

    fn matching(
        &self,
        operation_id: &str,
        expected: RecordState,
    ) -> Result<RecoveryRecord, WorkspaceError> {
        let record = self.readable()?.ok_or(WorkspaceError::NoPendingRecord)?;
        if record.operation_id != operation_id
            || record.state != expected
            || !same_root(self.workspace.root(), &record.workspace)
        {
            return Err(WorkspaceError::RecordMismatch {
                operation_id: operation_id.to_string(),
            });
        }
        Ok(record)
    }

    fn rewrite(&self, record: &RecoveryRecord) -> Result<(), WorkspaceError> {
        let path = self.workspace.root().join(".ziyuan").join(RECORD_FILE);
        atomic_replace(&path, &encode_record(record)?)
    }
}

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
struct Reservation(Arc<PathBuf>);
impl Reservation {
    fn acquire(
        root: Arc<PathBuf>,
        started: Instant,
        timeout: Duration,
    ) -> Result<Self, WorkspaceError> {
        let held = held();
        let mut roots = held
            .roots
            .lock()
            .map_err(|_| io::Error::other("workspace registry poisoned"))?;
        loop {
            if roots.insert(root.as_ref().clone()) {
                return Ok(Self(root));
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(WorkspaceError::Timeout {
                    waited: started.elapsed(),
                });
            }
            (roots, _) = held
                .freed
                .wait_timeout(roots, remaining)
                .map_err(|_| io::Error::other("workspace registry poisoned"))?;
        }
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let held = held();
        let mut roots = held.roots.lock().unwrap_or_else(|e| e.into_inner());
        roots.remove(self.0.as_ref());
        held.freed.notify_all();
    }
}

fn redirected(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// Missing is distinct from every other metadata error.
fn check_path(path: &Path, directory: bool) -> Result<bool, WorkspaceError> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if redirected(&meta)
                || if directory {
                    !meta.is_dir()
                } else {
                    !meta.is_file()
                }
            {
                return Err(WorkspaceError::Unsafe(path.display().to_string()));
            }
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn read_control(path: &Path) -> Result<Option<Vec<u8>>, WorkspaceError> {
    if !check_path(path, false)? {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(33).read_to_end(&mut bytes)?;
    if bytes.len() > 32 {
        return Err(WorkspaceError::Corrupt(format!(
            "{} exceeds 32 bytes",
            path.display()
        )));
    }
    Ok(Some(bytes))
}

/// Read the record with bounded reading: one byte past the codec limit is enough for
/// [`decode_record`] to report `TooLarge` without the caller buffering more.
fn read_record(path: &Path) -> Result<Option<RecoveryRecord>, WorkspaceError> {
    if !check_path(path, false)? {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_RECORD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    decode_record(&bytes).map(Some).map_err(Into::into)
}

/// The record codec accepts only `/` or `X:\` leading bytes, while a canonical
/// Same root, allowing either separator and an optional leading verbatim prefix
/// (`\\?\`) on both sides. Nothing else is relaxed:
/// a relative, verbatim, unresolved or different path never matches.
fn same_root(root: &Path, recorded: &Path) -> bool {
    if root == recorded {
        return true;
    }
    let split = |path: &Path| -> Vec<String> {
        path.to_string_lossy()
            .split(['\\', '/'])
            .map(str::to_string)
            .collect()
    };
    let (left, right) = (split(root), split(recorded));
    // `Workspace::open` stores a verbatim root on Windows; the record codec cannot
    // represent the `\\?\` prefix, so it is dropped on both sides before comparing.
    let strip = |mut parts: Vec<String>| {
        if parts.len() > 1 && parts[0].is_empty() && parts[1].is_empty() {
            parts.drain(..2);
            if parts.first().is_some_and(|first| first == "?") {
                parts.remove(0);
            }
        }
        parts
    };
    let (left, right) = (strip(left), strip(right));
    !left.is_empty() && left == right
}

fn decimal(bytes: &[u8]) -> Result<u64, WorkspaceError> {
    let invalid = || WorkspaceError::Corrupt("expected ASCII decimal followed by LF".into());
    let digits = bytes.strip_suffix(b"\n").ok_or_else(invalid)?;
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return Err(invalid());
    }
    std::str::from_utf8(digits)
        .map_err(|_| invalid())?
        .parse()
        .map_err(|_| invalid())
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), WorkspaceError> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().expect("control file has a parent");
    let (temp, mut file) = loop {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp = dir.join(format!(
            "{}.tmp-{}-{nanos}-{}",
            path.file_name()
                .expect("control file has a name")
                .to_string_lossy(),
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new().create_new(true).write(true).open(&temp) {
            Ok(file) => break (temp, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    };
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok::<_, io::Error>(())
    })();
    drop(file);
    let result = result.and_then(|()| replace(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    #[cfg(unix)]
    if let Ok(dir) = File::open(dir) {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn replace(temp: &Path, target: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        let started = Instant::now();
        loop {
            match fs::rename(temp, target) {
                Err(e) if matches!(e.raw_os_error(), Some(5 | 32 | 33)) => {
                    let remaining = Duration::from_millis(250).saturating_sub(started.elapsed());
                    if remaining.is_zero() {
                        return Err(e);
                    }
                    std::thread::sleep(remaining.min(Duration::from_millis(10)));
                }
                result => return result,
            }
        }
    }
    #[cfg(not(windows))]
    fs::rename(temp, target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "ziyuan-workspace-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn readonly_generation_creates_nothing_and_root_must_exist() {
        let root = Temp::new();
        let ws = Workspace::open(&root.0).unwrap();
        assert_eq!(ws.root(), root.0.canonicalize().unwrap());
        assert_eq!(ws.generation().unwrap(), 0);
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
        assert!(Workspace::open(root.0.join("missing")).is_err());
        let file = root.0.join("file");
        fs::write(&file, "").unwrap();
        assert!(Workspace::open(file).is_err());
        assert_eq!(DEFAULT_WRITE_LOCK_TIMEOUT, Duration::from_secs(5));
        assert_eq!(WRITE_PROTOCOL, 1);
    }

    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    impl Child {
        fn holding(root: &Temp) -> Self {
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "workspace::tests::child_holds_the_write_lock",
                    "--nocapture",
                ])
                .env("ZIYUAN_WORKSPACE_LOCK_TEST_ROOT", &root.0)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let mut child = Self(child);
            let deadline = Instant::now() + Duration::from_secs(8);
            while !root.0.join("held").exists() {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "child exited before holding lock"
                );
                assert!(Instant::now() < deadline, "child failed to acquire lock");
                std::thread::sleep(Duration::from_millis(10));
            }
            child
        }
        fn release(mut self, root: &Temp) {
            fs::write(root.0.join("release"), "").unwrap();
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                if let Some(status) = self.0.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                assert!(Instant::now() < deadline, "child failed to exit");
                std::thread::sleep(Duration::from_millis(10));
            }
            fs::remove_file(root.0.join("held")).unwrap();
            fs::remove_file(root.0.join("release")).unwrap();
        }
    }

    #[test]
    fn child_holds_the_write_lock() {
        let Some(root) = std::env::var_os("ZIYUAN_WORKSPACE_LOCK_TEST_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let ws = Workspace::open(&root).unwrap();
        let _guard = ws.lock(LockScope::Workspace).unwrap();
        fs::write(root.join("held"), "").unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !root.join("release").exists() {
            assert!(Instant::now() < deadline, "parent never released child");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn cross_process_exclusion_and_killed_holder_release() {
        let root = Temp::new();
        let ws = workspace(&root);
        let child = Child::holding(&root);
        assert!(matches!(
            ws.lock(LockScope::Workspace),
            Err(WorkspaceError::Timeout { .. })
        ));
        child.release(&root);
        drop(ws.lock(LockScope::Workspace).unwrap());
        let child = Child::holding(&root);
        drop(child); // Kill and reap while it still holds the OS lock.
        fs::remove_file(root.0.join("held")).unwrap();
        let successor = Child::holding(&root);
        successor.release(&root);
        drop(ws.lock(LockScope::Workspace).unwrap());
    }

    #[test]
    fn in_process_and_os_wait_share_one_timeout_budget() {
        let root = Temp::new();
        let ws = workspace(&root);
        let local = ws.lock(LockScope::Workspace).unwrap();
        // Keep the reservation but let the child own the OS lock. This forces
        // the waiter through both independent waiting stages deterministically.
        local._file.unlock().unwrap();
        let child = Child::holding(&root);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let start = Instant::now();
            started_tx.send(()).unwrap();
            let result = ws
                .with_lock_timeout(Duration::from_millis(1000))
                .lock(LockScope::Workspace);
            done_tx
                .send((
                    matches!(result, Err(WorkspaceError::Timeout { .. })),
                    start.elapsed(),
                ))
                .unwrap();
        });
        started_rx.recv().unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(600)).is_err());
        drop(local);
        let (timed_out, elapsed) = done_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        thread.join().unwrap();
        assert!(timed_out);
        assert!(elapsed >= Duration::from_millis(900), "{elapsed:?}");
        assert!(
            elapsed < Duration::from_millis(1450),
            "timeout budget restarted: {elapsed:?}"
        );
        child.release(&root);
    }

    #[test]
    fn generation_reads_strict_bounded_decimal() {
        let root = Temp::new();
        let ws = workspace(&root);
        fs::create_dir(root.0.join(".ziyuan")).unwrap();
        let path = root.0.join(".ziyuan/write.generation");
        for (bytes, expected) in [
            (b"42\n".as_slice(), 42),
            (b"0000000000000000000000000000001\n", 1),
            (b"18446744073709551615\n", u64::MAX),
        ] {
            fs::write(&path, bytes).unwrap();
            assert_eq!(ws.generation().unwrap(), expected);
        }
        for bytes in [
            b"".as_slice(),
            b"1",
            b"1\r\n",
            b"-1\n",
            b"+1\n",
            b" 1\n",
            b"1\n2\n",
            b"\xff\n",
            b"18446744073709551616\n",
            &[b'0'; 33],
        ] {
            fs::write(&path, bytes).unwrap();
            assert!(
                matches!(ws.generation(), Err(WorkspaceError::Corrupt(_))),
                "{bytes:?}"
            );
        }
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(matches!(ws.generation(), Err(WorkspaceError::Unsafe(_))));
    }

    #[test]
    fn generation_transitions_and_overflow_preserve_state() {
        let root = Temp::new();
        let ws = workspace(&root);
        let mut guard = ws.lock(LockScope::Workspace).unwrap();
        assert_eq!(guard.generation().unwrap(), 0);
        assert!(matches!(
            guard.finish(),
            Err(WorkspaceError::NotStarted { generation: 0 })
        ));
        assert_eq!(guard.begin().unwrap(), 1);
        assert!(matches!(
            guard.begin(),
            Err(WorkspaceError::Unfinished { generation: 1 })
        ));
        assert_eq!(guard.finish().unwrap(), 2);
        assert!(matches!(
            guard.finish(),
            Err(WorkspaceError::NotStarted { generation: 2 })
        ));
        assert_eq!(guard.begin().unwrap(), 3);
        drop(guard);
        assert_eq!(ws.generation().unwrap(), 3, "drop must not auto-complete");
        let mut guard = ws.lock(LockScope::Workspace).unwrap();
        assert!(matches!(
            guard.begin(),
            Err(WorkspaceError::Unfinished { generation: 3 })
        ));
        let path = root.0.join(".ziyuan/write.generation");
        fs::write(&path, format!("{}\n", u64::MAX - 1)).unwrap();
        assert_eq!(guard.begin().unwrap(), u64::MAX);
        let before = fs::read(&path).unwrap();
        assert!(matches!(guard.finish(), Err(WorkspaceError::Corrupt(_))));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_no_temps(&root);
    }

    #[test]
    fn concurrent_readers_observe_200_complete_cycles_without_io_errors() {
        use std::sync::atomic::AtomicBool;
        let root = Temp::new();
        let ws = workspace(&root);
        let mut guard = ws.lock(LockScope::Workspace).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let done = Arc::new(AtomicBool::new(false));
        let reader_ws = ws.clone();
        let reader_barrier = barrier.clone();
        let reader_done = done.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            reader_barrier.wait();
            assert_eq!(reader_ws.generation().unwrap(), 0);
            ready_tx.send(()).unwrap();
            let mut count = 0;
            while !reader_done.load(Ordering::Acquire) {
                let generation = reader_ws.generation().unwrap();
                assert!(generation <= 400);
                count += 1;
            }
            count
        });
        barrier.wait();
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        for cycle in 0..200 {
            assert_eq!(guard.begin().unwrap(), cycle * 2 + 1);
            assert_eq!(guard.finish().unwrap(), cycle * 2 + 2);
        }
        done.store(true, Ordering::Release);
        assert!(reader.join().unwrap() > 0);
        assert_eq!(ws.generation().unwrap(), 400);
        assert_no_temps(&root);
    }

    #[test]
    fn non_directory_control_root_and_nonfile_lock_are_refused() {
        let root = Temp::new();
        let ws = workspace(&root);
        let dir = root.0.join(".ziyuan");
        fs::write(&dir, "").unwrap();
        assert!(matches!(ws.generation(), Err(WorkspaceError::Unsafe(_))));
        assert!(matches!(
            ws.lock(LockScope::Workspace),
            Err(WorkspaceError::Unsafe(_))
        ));
        fs::remove_file(&dir).unwrap();
        fs::create_dir(&dir).unwrap();
        let lock = dir.join("write.lock");
        fs::create_dir(&lock).unwrap();
        assert!(matches!(
            ws.lock(LockScope::Workspace),
            Err(WorkspaceError::Unsafe(_))
        ));
        fs::remove_dir(&lock).unwrap();
        fs::write(&lock, "fixed lock identity").unwrap();
        drop(ws.lock(LockScope::Workspace).unwrap());
        assert_eq!(fs::read(&lock).unwrap(), b"fixed lock identity");
    }

    #[test]
    fn atomic_replace_failure_keeps_target_and_cleans_own_temp() {
        let root = Temp::new();
        let dir = root.0.join(".ziyuan");
        fs::create_dir(&dir).unwrap();
        let target = dir.join("write.generation");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("sentinel"), "unchanged").unwrap();
        assert!(atomic_replace(&target, b"2\n").is_err());
        assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"unchanged");
        assert_no_temps(&root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_control_directory_and_files_are_refused() {
        use std::os::unix::fs::symlink;
        let root = Temp::new();
        let target = Temp::new();
        let ws = workspace(&root);
        let dir = root.0.join(".ziyuan");
        symlink(&target.0, &dir).unwrap();
        assert!(matches!(ws.generation(), Err(WorkspaceError::Unsafe(_))));
        assert!(matches!(
            ws.lock(LockScope::Workspace),
            Err(WorkspaceError::Unsafe(_))
        ));
        fs::remove_file(&dir).unwrap();
        fs::create_dir(&dir).unwrap();
        let outside = target.0.join("outside");
        fs::write(&outside, "1\n").unwrap();
        for name in ["write.lock", "write-protocol", "write.generation"] {
            let path = dir.join(name);
            symlink(&outside, &path).unwrap();
            if name == "write.generation" {
                assert!(matches!(ws.generation(), Err(WorkspaceError::Unsafe(_))));
                let mut guard = ws.lock(LockScope::Workspace).unwrap();
                assert!(matches!(guard.begin(), Err(WorkspaceError::Unsafe(_))));
            } else {
                assert!(matches!(
                    ws.lock(LockScope::Workspace),
                    Err(WorkspaceError::Unsafe(_))
                ));
            }
            fs::remove_file(&path).unwrap();
        }
        assert_eq!(fs::read(&outside).unwrap(), b"1\n");
        drop(ws.lock(LockScope::Workspace).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn junction_control_directory_is_refused_without_touching_target() {
        struct Junction(PathBuf);
        impl Drop for Junction {
            fn drop(&mut self) {
                fs::remove_dir(&self.0).unwrap();
            }
        }
        let root = Temp::new();
        let target = Temp::new();
        fs::write(target.0.join("sentinel"), "untouched").unwrap();
        let dir = root.0.join(".ziyuan");
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&dir)
            .arg(&target.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "mklink failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let junction = Junction(dir);
        let ws = workspace(&root);
        assert!(matches!(ws.generation(), Err(WorkspaceError::Unsafe(_))));
        assert!(matches!(
            ws.lock(LockScope::Workspace),
            Err(WorkspaceError::Unsafe(_))
        ));
        drop(junction);
        assert_eq!(fs::read(target.0.join("sentinel")).unwrap(), b"untouched");
        assert_eq!(fs::read_dir(&target.0).unwrap().count(), 1);
        drop(ws.lock(LockScope::Workspace).unwrap());
    }

    #[test]
    fn error_sources_are_generic_and_io_is_preserved() {
        use std::error::Error;
        let io = WorkspaceError::from(io::Error::other("example"));
        assert!(io.source().is_some());
        for error in [
            WorkspaceError::Timeout {
                waited: Duration::ZERO,
            },
            WorkspaceError::Unsafe("path".into()),
            WorkspaceError::Corrupt("bytes".into()),
            WorkspaceError::Newer { found: 2 },
            WorkspaceError::Unfinished { generation: 1 },
            WorkspaceError::NotStarted { generation: 0 },
        ] {
            assert!(error.source().is_none());
            assert!(!error.to_string().is_empty());
            assert!(!error.to_string().contains("recover"));
        }
    }

    fn assert_no_temps(root: &Temp) {
        for entry in fs::read_dir(root.0.join(".ziyuan")).unwrap() {
            assert!(
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".tmp-")
            );
        }
    }

    fn workspace(root: &Temp) -> Workspace {
        Workspace::open(&root.0)
            .unwrap()
            .with_lock_timeout(Duration::from_millis(80))
    }

    #[test]
    fn independent_handles_exclude_and_drop_releases() {
        let root = Temp::new();
        let ws = workspace(&root);
        let guard = ws
            .clone()
            .with_lock_timeout(Duration::ZERO)
            .lock(LockScope::Workspace)
            .unwrap();
        assert_eq!(guard.workspace().root(), ws.root());
        let start = std::time::Instant::now();
        assert!(matches!(
            workspace(&root).lock(LockScope::Workspace),
            Err(WorkspaceError::Timeout { .. })
        ));
        assert!(start.elapsed() >= Duration::from_millis(60));
        let other = Temp::new();
        let unrelated = workspace(&other).lock(LockScope::Workspace).unwrap();
        drop(unrelated);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let waiting = ws.with_lock_timeout(Duration::from_secs(2));
        let thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let guard = waiting.lock(LockScope::Workspace).unwrap();
            done_tx.send(()).unwrap();
            drop(guard);
        });
        started_rx.recv().unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(80)).is_err());
        drop(guard);
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        thread.join().unwrap();
        drop(workspace(&root).lock(LockScope::Workspace).unwrap());
    }

    #[test]
    fn protocol_is_strict_and_errors_release_reservation() {
        let root = Temp::new();
        let ws = workspace(&root);
        drop(ws.lock(LockScope::Workspace).unwrap());
        let marker = root.0.join(".ziyuan/write-protocol");
        assert_eq!(fs::read(&marker).unwrap(), b"1\n");
        let modified = fs::metadata(&marker).unwrap().modified().unwrap();
        drop(ws.lock(LockScope::Workspace).unwrap());
        assert_eq!(fs::metadata(&marker).unwrap().modified().unwrap(), modified);
        for bytes in [b"2\n".as_slice(), b"02\n", b"18446744073709551615\n"] {
            fs::write(&marker, bytes).unwrap();
            assert!(matches!(
                ws.lock(LockScope::Workspace),
                Err(WorkspaceError::Newer { .. })
            ));
        }
        for bytes in [
            b"".as_slice(),
            b"0\n",
            b"01\n",
            b"1",
            b"1\r\n",
            b"+2\n",
            b"garbage\n",
            &[b'9'; 33],
        ] {
            fs::write(&marker, bytes).unwrap();
            assert!(
                matches!(
                    ws.lock(LockScope::Workspace),
                    Err(WorkspaceError::Corrupt(_))
                ),
                "{bytes:?}"
            );
        }
        fs::remove_file(&marker).unwrap();
        fs::create_dir(&marker).unwrap();
        assert!(matches!(
            ws.lock(LockScope::Workspace),
            Err(WorkspaceError::Unsafe(_))
        ));
        fs::remove_dir(&marker).unwrap();
        drop(ws.lock(LockScope::Workspace).unwrap());
        assert_eq!(fs::read_dir(root.0.join(".ziyuan")).unwrap().count(), 2);
    }

    /// A record as a real caller stores it: the root in OS form, which is what the
    /// record codec can represent.
    fn record(ws: &Workspace, state: RecordState) -> RecoveryRecord {
        build_record(&os_path(ws.root()), state)
    }

    /// The record codec accepts only `/` or `X:\` leading bytes, while a canonical
    /// Windows root is verbatim (`\\?\C:\...`). This mirrors the conversion the
    /// storage caller must do before publishing; `Workspace` never rewrites the root.
    fn os_path(canonical: &Path) -> PathBuf {
        let text = canonical.to_string_lossy();
        match text.strip_prefix(r"\\?\") {
            Some(rest) => PathBuf::from(rest),
            None => canonical.to_path_buf(),
        }
    }

    fn build_record(workspace: &Path, state: RecordState) -> RecoveryRecord {
        use crate::recovery::{ApplicationSource, EventBatch, FileState, TargetState};
        use ziyuan_core::{CourtEvent, TaskId};
        let applied = CourtEvent::ChangesApplied {
            task: TaskId(7),
            skipped: Vec::new(),
        };
        RecoveryRecord {
            operation_id: "a1b2c3d4e5f60718293a4b5c6d7e8f90".into(),
            workspace: workspace.to_path_buf(),
            generation_before: 0,
            generation_after: 2,
            state,
            application: ApplicationSource {
                task: 7,
                changes_version: "v1".into(),
                skipped: Vec::new(),
            },
            events: EventBatch {
                expected_events: 2,
                expected_bytes: 64,
                history_sha256: "9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0"
                    .into(),
                encoded_events: vec![
                    crate::codec::encode_event(&CourtEvent::Clock(1_700_000_000)),
                    crate::codec::encode_event(&applied),
                ],
            },
            targets: vec![TargetState {
                path: "docs/notes.md".into(),
                before: FileState::Absent,
                after: FileState::Present(
                    "3b1f0f4d2c5a6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7".into(),
                ),
            }],
        }
    }

    fn record_path(root: &Temp) -> PathBuf {
        root.0.join(".ziyuan/recovery.json")
    }

    /// Raw bytes of a valid record, for corruption and version tests.
    fn record_bytes(ws: &Workspace, state: RecordState) -> Vec<u8> {
        crate::recovery::encode_record(&record(ws, state)).unwrap()
    }

    /// The codec error a corrupted file must surface, without matching on message.
    enum Expect {
        Corrupt,
        TooLarge,
        Newer,
    }
    impl Expect {
        fn matches(&self, error: &RecoveryError) -> bool {
            match (self, error) {
                (Self::Corrupt, RecoveryError::Corrupt(_)) => true,
                (Self::TooLarge, RecoveryError::TooLarge { limit }) => {
                    *limit == crate::recovery::MAX_RECORD_BYTES
                }
                (Self::Newer, RecoveryError::Newer { found, supported }) => {
                    (*found, *supported) == (2, 1)
                }
                _ => false,
            }
        }
    }

    /// Replace the digest with one that matches the same bytes but altered content:
    /// keeps length checks happy while failing the checksum.
    fn tampered_digest(bytes: &[u8]) -> Vec<u8> {
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let start = text.find("\"sha256\":\"sha256:").unwrap() + "\"sha256\":\"sha256:".len();
        let mut text = text.into_bytes();
        let head = text[start];
        text[start] = if head == b'0' { b'1' } else { b'0' };
        text
    }

    #[test]
    fn absent_record_and_lock_creates_no_record_file() {
        let root = Temp::new();
        let ws = workspace(&root);
        assert!(ws.pending_record().unwrap().is_none());
        let mut guard = ws.lock(LockScope::Workspace).unwrap();
        assert!(ws.pending_record().unwrap().is_none());
        assert!(!record_path(&root).exists());
        assert_eq!(guard.reserve().unwrap(), 1);
        assert!(!record_path(&root).exists());
        guard.finish().unwrap();
        assert!(ws.pending_record().unwrap().is_none());
        assert!(!record_path(&root).exists());
    }

    #[test]
    fn publish_is_single_and_refuses_a_second_operation() {
        let root = Temp::new();
        let ws = workspace(&root);
        let mut guard = ws.lock(LockScope::Workspace).unwrap();
        // Only prepared and published records may be written at all.
        for state in [RecordState::Completed, RecordState::Abandoned] {
            assert!(matches!(
                guard.publish(&record(&ws, state)),
                Err(WorkspaceError::Unsafe(_))
            ));
        }
        assert!(ws.pending_record().unwrap().is_none());
        guard.publish(&record(&ws, RecordState::Prepared)).unwrap();
        let stored = record(&ws, RecordState::Prepared);
        assert_eq!(ws.pending_record().unwrap().unwrap(), stored);
        let mut other = record(&ws, RecordState::Published);
        other.operation_id = "0f1e2d3c4b5a69788796a5b4c3d2e1f0".into();
        assert!(matches!(
            guard.publish(&other),
            Err(WorkspaceError::RecordExists { .. })
        ));
        assert_eq!(ws.pending_record().unwrap().unwrap(), stored);
        // Republishing the same operation still needs complete/abandon, not a rewrite.
        assert!(matches!(
            guard.publish(&stored),
            Err(WorkspaceError::RecordExists { .. })
        ));
        assert_eq!(ws.pending_record().unwrap().unwrap(), stored);
        assert_no_temps(&root);
        // write.lock, write-protocol and recovery.json.
        assert_eq!(fs::read_dir(root.0.join(".ziyuan")).unwrap().count(), 3);
    }

    #[test]
    fn published_record_blocks_until_completed_finished_and_removed() {
        let root = Temp::new();
        let ws = workspace(&root);
        ws.ensure_writable().unwrap();
        let mut guard = ws.lock(LockScope::Workspace).unwrap();
        let operation = "a1b2c3d4e5f60718293a4b5c6d7e8f90";
        assert!(matches!(
            guard.complete(operation),
            Err(WorkspaceError::NoPendingRecord)
        ));
        assert!(matches!(
            guard.abandon(operation),
            Err(WorkspaceError::NoPendingRecord)
        ));
        // Prepare, publish, then reserve the odd generation (spec order).
        guard.publish(&record(&ws, RecordState::Prepared)).unwrap();
        ws.ensure_writable().unwrap();
        assert_eq!(guard.reserve().unwrap(), 1);
        // Bumping prepared to published goes through abandon + republish, not a rewrite.
        assert!(matches!(
            guard.publish(&record(&ws, RecordState::Published)),
            Err(WorkspaceError::RecordExists { .. })
        ));
        assert_eq!(
            ws.pending_record().unwrap().unwrap().state,
            RecordState::Prepared
        );
        guard.abandon(operation).unwrap();
        guard.remove_record().unwrap();
        guard.publish(&record(&ws, RecordState::Published)).unwrap();
        assert!(matches!(
            ws.ensure_writable(),
            Err(WorkspaceError::PendingRecovery { operation_id }) if operation_id == operation
        ));
        assert!(matches!(
            guard.complete("0f1e2d3c4b5a69788796a5b4c3d2e1f0"),
            Err(WorkspaceError::RecordMismatch { .. })
        ));
        assert!(matches!(
            guard.abandon(operation),
            Err(WorkspaceError::RecordMismatch { .. })
        ));
        let completed = guard.complete(operation).unwrap();
        assert_eq!(completed.state, RecordState::Completed);
        assert_eq!(completed.operation_id, operation);
        assert_eq!(guard.generation().unwrap(), 1, "complete must not advance");
        assert!(matches!(
            ws.ensure_writable(),
            Err(WorkspaceError::FinishPending { operation_id }) if operation_id == operation
        ));
        assert_eq!(guard.finish().unwrap(), 2);
        assert!(ws.ensure_writable().is_ok());
        assert!(ws.pending_record().unwrap().is_some());
        guard.remove_record().unwrap();
        assert!(ws.pending_record().unwrap().is_none());
        assert!(!record_path(&root).exists());
        assert_eq!(guard.generation().unwrap(), 2);
        assert!(ws.ensure_writable().is_ok());
        // And a new operation may publish again afterwards.
        guard.publish(&record(&ws, RecordState::Published)).unwrap();
        assert_eq!(guard.reserve().unwrap(), 3);
        assert!(matches!(
            ws.ensure_writable(),
            Err(WorkspaceError::PendingRecovery { .. })
        ));
        assert_no_temps(&root);
    }

    #[test]
    fn abandon_published_requires_the_callers_confirmation() {
        // A published record usually means files may already be written. Abandoning it is
        // only allowed when the caller states nothing happened; a bare refusal leaves the
        // record untouched so the workspace stays blocked.
        let root = Temp::new();
        let ws = workspace(&root);
        let operation = "a1b2c3d4e5f60718293a4b5c6d7e8f90";
        let mut guard = ws.lock(LockScope::Workspace).unwrap();
        guard.publish(&record(&ws, RecordState::Published)).unwrap();
        assert_eq!(guard.reserve().unwrap(), 1);

        assert!(matches!(
            guard.abandon_published(operation, Untouched::ruled_out()),
            Err(WorkspaceError::Unsafe(_))
        ));
        assert_eq!(
            ws.pending_record().unwrap().unwrap().state,
            RecordState::Published
        );
        assert!(ws.ensure_writable().is_err(), "still blocked");

        let abandoned = guard
            .abandon_published(operation, Untouched::confirmed())
            .unwrap();
        assert_eq!(abandoned.state, RecordState::Abandoned);
        assert_eq!(guard.generation().unwrap(), 1, "abandon must not advance");
    }

    #[test]
    fn abandon_published_refuses_a_prepared_record() {
        // The published-only entry point must not become a way around the prepared check.
        let root = Temp::new();
        let ws = workspace(&root);
        let operation = "a1b2c3d4e5f60718293a4b5c6d7e8f90";
        let mut guard = ws.lock(LockScope::Workspace).unwrap();
        guard.publish(&record(&ws, RecordState::Prepared)).unwrap();
        assert!(matches!(
            guard.abandon_published(operation, Untouched::confirmed()),
            Err(WorkspaceError::RecordMismatch { .. })
        ));
    }

    #[test]
    fn prepared_record_does_not_block_writes() {
        let root = Temp::new();
        let ws = workspace(&root);
        let mut guard = ws.lock(LockScope::Workspace).unwrap();
        // Private staging: prepared before the odd generation, and it blocks nothing
        // that a plain writer would need (the generation check still applies).
        guard.publish(&record(&ws, RecordState::Prepared)).unwrap();
        ws.ensure_writable().unwrap();
        assert_eq!(guard.reserve().unwrap(), 1);
        assert_eq!(guard.generation().unwrap(), 1);
        let abandoned: RecoveryRecord = guard.abandon("a1b2c3d4e5f60718293a4b5c6d7e8f90").unwrap();
        assert_eq!(abandoned.state, RecordState::Abandoned);
        assert_eq!(guard.generation().unwrap(), 1, "abandon must not advance");
        assert!(matches!(
            guard.abandon("a1b2c3d4e5f60718293a4b5c6d7e8f90"),
            Err(WorkspaceError::RecordMismatch { .. })
        ));
        assert!(matches!(
            guard.complete("a1b2c3d4e5f60718293a4b5c6d7e8f90"),
            Err(WorkspaceError::RecordMismatch { .. })
        ));
        // The terminal abandoned record on an odd generation asks for `finish`.
        assert!(matches!(
            ws.ensure_writable(),
            Err(WorkspaceError::FinishPending { .. })
        ));
        assert_eq!(guard.finish().unwrap(), 2);
        // A terminal record with the generation already even blocks nothing, so a
        // crashed cleanup can never wedge the workspace; it is still removable.
        ws.ensure_writable().unwrap();
        assert_eq!(guard.reserve().unwrap(), 3);
        guard.finish().unwrap();
        guard.remove_record().unwrap();
        assert_eq!(guard.generation().unwrap(), 4);
        assert_no_temps(&root);
    }

    #[test]
    fn odd_generation_blocks_writes_until_finished() {
        let root = Temp::new();
        let ws = workspace(&root);
        let mut guard = ws.lock(LockScope::Workspace).unwrap();
        guard.publish(&record(&ws, RecordState::Published)).unwrap();
        assert_eq!(guard.reserve().unwrap(), 1);
        assert!(matches!(
            ws.ensure_writable(),
            Err(WorkspaceError::PendingRecovery { .. })
        ));
        guard.complete("a1b2c3d4e5f60718293a4b5c6d7e8f90").unwrap();
        assert!(matches!(
            ws.ensure_writable(),
            Err(WorkspaceError::FinishPending { operation_id })
                if operation_id == "a1b2c3d4e5f60718293a4b5c6d7e8f90"
        ));
        assert_eq!(guard.finish().unwrap(), 2);
        ws.ensure_writable().unwrap();
        guard.remove_record().unwrap();
        assert_eq!(ws.generation().unwrap() % 2, 0);
        assert_no_temps(&root);
    }

    #[test]
    fn corrupt_and_newer_records_are_reported_and_never_overwritten() {
        let root = Temp::new();
        let ws = workspace(&root);
        let mut guard = ws.lock(LockScope::Workspace).unwrap();
        let path = record_path(&root);
        let valid = record_bytes(&ws, RecordState::Published);
        let mut bad_json = valid.clone();
        bad_json.truncate(valid.len() / 2);
        let mut oversized = valid.clone();
        oversized.resize(crate::recovery::MAX_RECORD_BYTES + 1, b' ');
        let newer = String::from_utf8(valid.clone())
            .unwrap()
            .replace("\"format_version\":1", "\"format_version\":2")
            .into_bytes();
        let cases: [(&[u8], Expect); 4] = [
            (&bad_json, Expect::Corrupt),
            (&tampered_digest(&valid), Expect::Corrupt),
            (&oversized, Expect::TooLarge),
            (&newer, Expect::Newer),
        ];
        for (bytes, expected) in cases {
            fs::write(&path, bytes).unwrap();
            let pending = ws.pending_record().unwrap_err();
            assert!(
                matches!(&pending, WorkspaceError::Recovery(e) if expected.matches(e)),
                "{}",
                pending
            );
            let blocked = ws.ensure_writable().unwrap_err();
            assert!(
                matches!(&blocked, WorkspaceError::Recovery(e) if expected.matches(e)),
                "{}",
                blocked
            );
            assert_eq!(fs::read(&path).unwrap(), bytes, "record must not be reset");
            // An unreadable record is never treated as absent, so the caller cannot
            // publish over it either way; a decoded-but-corrupt record is refused
            // outright and anything else fails before the write as well.
            assert!(guard.publish(&record(&ws, RecordState::Prepared)).is_err());
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        assert_no_temps(&root);
        fs::remove_file(&path).unwrap();
        assert!(matches!(
            guard.complete("a1b2c3d4e5f60718293a4b5c6d7e8f90"),
            Err(WorkspaceError::NoPendingRecord)
        ));
        assert!(matches!(
            guard.abandon("a1b2c3d4e5f60718293a4b5c6d7e8f90"),
            Err(WorkspaceError::NoPendingRecord)
        ));
        assert!(matches!(
            guard.remove_record(),
            Err(WorkspaceError::NoPendingRecord)
        ));
        assert_no_temps(&root);
    }

    #[test]
    fn record_path_must_be_a_regular_file() {
        let root = Temp::new();
        let ws = workspace(&root);
        let mut guard = ws.lock(LockScope::Workspace).unwrap();
        fs::create_dir(record_path(&root)).unwrap();
        assert!(matches!(
            ws.pending_record(),
            Err(WorkspaceError::Unsafe(_))
        ));
        assert!(matches!(
            ws.ensure_writable(),
            Err(WorkspaceError::Unsafe(_))
        ));
        assert!(matches!(
            guard.publish(&record(&ws, RecordState::Prepared)),
            Err(WorkspaceError::Unsafe(_))
        ));
        assert!(matches!(
            guard.remove_record(),
            Err(WorkspaceError::Unsafe(_))
        ));
        assert!(record_path(&root).is_dir());
        fs::remove_dir(record_path(&root)).unwrap();
        assert_no_temps(&root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_record_file_is_refused() {
        use std::os::unix::fs::symlink;
        let root = Temp::new();
        let target = Temp::new();
        let ws = workspace(&root);
        let mut guard = ws.lock(LockScope::Workspace).unwrap();
        let outside = target.0.join("outside.json");
        fs::write(&outside, b"sentinel").unwrap();
        symlink(&outside, record_path(&root)).unwrap();
        assert!(matches!(
            ws.pending_record(),
            Err(WorkspaceError::Unsafe(_))
        ));
        assert!(matches!(
            ws.ensure_writable(),
            Err(WorkspaceError::Unsafe(_))
        ));
        assert!(matches!(
            guard.publish(&record(&ws, RecordState::Prepared)),
            Err(WorkspaceError::Unsafe(_))
        ));
        assert!(matches!(
            guard.remove_record(),
            Err(WorkspaceError::Unsafe(_))
        ));
        fs::remove_file(record_path(&root)).unwrap();
        assert_eq!(fs::read(&outside).unwrap(), b"sentinel");
        assert_eq!(fs::read_dir(&target.0).unwrap().count(), 1);
        assert_no_temps(&root);
    }

    #[test]
    fn record_for_another_workspace_is_refused() {
        let root = Temp::new();
        let other = Temp::new();
        let ws = workspace(&root);
        let other_ws = workspace(&other);
        let mut guard = ws.lock(LockScope::Workspace).unwrap();
        for state in [RecordState::Prepared, RecordState::Published] {
            assert!(matches!(
                guard.publish(&record(&other_ws, state)),
                Err(WorkspaceError::Unsafe(_))
            ));
        }
        assert!(ws.pending_record().unwrap().is_none());
        // A relative workspace is rejected by the codec before the root comparison;
        // either way nothing is written.
        let mut relative = record(&ws, RecordState::Prepared);
        relative.workspace = PathBuf::from(".ziyuan");
        assert!(guard.publish(&relative).is_err());
        // The comparison is byte-for-byte on the stored path: recovery.rs does not
        // canonicalize, so a textually different root is refused even when it names
        // the same directory.
        let unresolved = root.0.join("..").join(root.0.file_name().unwrap());
        assert!(
            unresolved.is_dir(),
            "the path resolves to the same directory"
        );
        assert!(matches!(
            guard.publish(&build_record(&unresolved, RecordState::Prepared)),
            Err(WorkspaceError::Unsafe(_))
        ));
        assert!(ws.pending_record().unwrap().is_none());
        assert_no_temps(&root);
    }

    struct RecoveryChild(Child);
    impl RecoveryChild {
        fn publish(root: &Temp) -> Self {
            // The workspace must exist before the child opens it.
            drop(Workspace::open(&root.0).unwrap());
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "workspace::tests::child_publishes_a_recovery_record",
                    "--nocapture",
                ])
                .env("ZIYUAN_WORKSPACE_LOCK_TEST_ROOT", &root.0)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let mut child = Self(Child(child));
            let deadline = Instant::now() + Duration::from_secs(8);
            while !root.0.join("published").exists() {
                assert!(
                    child.0.0.try_wait().unwrap().is_none(),
                    "child exited before publishing"
                );
                assert!(Instant::now() < deadline, "child failed to publish");
                std::thread::sleep(Duration::from_millis(10));
            }
            child
        }
    }

    #[test]
    fn child_publishes_a_recovery_record() {
        let Some(root) = std::env::var_os("ZIYUAN_WORKSPACE_LOCK_TEST_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let ws = Workspace::open(&root).unwrap();
        let mut guard = ws.lock(LockScope::Workspace).unwrap();
        guard.publish(&record(&ws, RecordState::Published)).unwrap();
        guard.reserve().unwrap();
        fs::write(root.join("published"), "").unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while !root.join("release").exists() {
            assert!(Instant::now() < deadline, "parent never released child");
            std::thread::sleep(Duration::from_millis(10));
        }
        guard.complete("a1b2c3d4e5f60718293a4b5c6d7e8f90").unwrap();
        guard.finish().unwrap();
    }

    #[test]
    fn pending_recovery_from_another_process_blocks_until_finished() {
        let root = Temp::new();
        let ws = workspace(&root);
        let mut child = RecoveryChild::publish(&root);
        assert!(matches!(
            ws.ensure_writable(),
            Err(WorkspaceError::PendingRecovery { operation_id })
                if operation_id == "a1b2c3d4e5f60718293a4b5c6d7e8f90"
        ));
        fs::write(root.0.join("release"), "").unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = child.0.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "child failed to exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        fs::remove_file(root.0.join("published")).unwrap();
        fs::remove_file(root.0.join("release")).unwrap();
        assert_eq!(ws.generation().unwrap(), 2);
        ws.ensure_writable().unwrap();
        let mut guard = ws.lock(LockScope::Workspace).unwrap();
        assert_eq!(
            ws.pending_record().unwrap().unwrap().state,
            RecordState::Completed
        );
        guard.remove_record().unwrap();
        assert_eq!(guard.reserve().unwrap(), 3);
        assert_no_temps(&root);
    }
}
