//! Workspace-wide managed write exclusion and generation tracking.
//!
//! Control files are replaced atomically; this does not promise power-loss safety.
//! Static symlinks/reparse points are refused. Concurrent malicious path replacement
//! is outside this API's threat model.
use std::collections::HashSet;
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_WRITE_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
pub const WRITE_PROTOCOL: u64 = 1;

#[derive(Debug)]
pub enum WorkspaceError {
    Io(io::Error),
    Timeout { waited: Duration },
    Unsafe(String),
    Corrupt(String),
    Newer { found: u64 },
    Unfinished { generation: u64 },
    NotStarted { generation: u64 },
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
    fn from(error: io::Error) -> Self {
        Self::Io(error)
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
}
