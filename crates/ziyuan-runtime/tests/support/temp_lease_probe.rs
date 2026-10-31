use std::collections::hash_map::RandomState;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::hash::BuildHasher;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
pub const COMPLETE_NEW: &[u8] = b"complete-new";
const CONTINUE_TIMEOUT: Duration = Duration::from_millis(300);
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const MAX_MESSAGE: usize = 256;
const ENV: &str = "ZIYUAN_TEMP_LEASE_PROBE";
static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum ProbeLock {
    Acquired(File),
    Busy,
}

// This is an experimental observation, never a successful write in disguise.
#[derive(Debug, PartialEq, Eq)]
pub enum WriteObservation {
    Complete,
    Blocked { kind: io::ErrorKind, raw: i32 },
}

impl WriteObservation {
    fn message(&self) -> String {
        match self {
            Self::Complete => "WRITE_OK".to_owned(),
            Self::Blocked { kind, raw } => format!("WRITE_BLOCKED {raw} {kind:?}"),
        }
    }

    pub fn receive(stream: &mut TcpStream) -> io::Result<Self> {
        let message = read_message(
            stream,
            Instant::now() + DEFAULT_TIMEOUT,
            "parent write observation",
        )?;
        if message == "WRITE_OK" {
            return Ok(Self::Complete);
        }
        if let Some(raw) = message
            .strip_prefix("WRITE_BLOCKED ")
            .and_then(|rest| rest.split_once(' '))
            .and_then(|(raw, _)| raw.parse::<i32>().ok())
            .filter(|raw| cfg!(windows) && *raw == 33)
        {
            let observation = Self::Blocked {
                kind: io::Error::from_raw_os_error(raw).kind(),
                raw,
            };
            if observation.message() == message {
                return Ok(observation);
            }
        }
        Err(invalid("parent write observation: unexpected result"))
    }
}

fn create_temp(root: &Path, share_delete: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // std already defaults to read|write|delete sharing on Windows, so an
        // unrestricted case must be told apart by narrowing the other one.
        if share_delete {
            // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE.
            options.share_mode(1 | 2 | 4);
        } else {
            // FILE_SHARE_READ | FILE_SHARE_WRITE: no FILE_SHARE_DELETE.
            options.share_mode(1 | 2);
        }
    }
    #[cfg(not(windows))]
    assert!(!share_delete, "explicit share modes are Windows-only");
    options.open(root.join("temp"))
}

fn lock_new(file: File) -> io::Result<File> {
    match probe_lock(file)? {
        ProbeLock::Acquired(file) => Ok(file),
        ProbeLock::Busy => Err(io::Error::other("child new lock unexpectedly busy")),
    }
}

fn write_complete(file: &mut File) -> io::Result<()> {
    file.write_all(COMPLETE_NEW)?;
    file.sync_all()
}

struct ReplacementState {
    held: Option<File>,
    stage: &'static str,
    observation: Option<WriteObservation>,
}

fn prepare_replacement(case: &str, root: &Path) -> io::Result<ReplacementState> {
    let mut file = create_temp(root, case.starts_with("live-share-delete-"))?;
    let (stage, observation) = match case {
        "create-before-lock" => ("created-before-lock", None),
        "legacy-unleased" => {
            write_complete(&mut file)?;
            ("legacy-unleased", None)
        }
        _ if case.ends_with("-prewritten") || case == "close-prewritten-before-rename" => {
            write_complete(&mut file)?;
            file = lock_new(file)?;
            ("live-handle", Some(WriteObservation::Complete))
        }
        _ => {
            file = lock_new(file)?;
            let observation = match file.write_all(COMPLETE_NEW) {
                Ok(()) => WriteObservation::Complete,
                Err(e) if cfg!(windows) && e.raw_os_error() == Some(33) => {
                    WriteObservation::Blocked {
                        kind: e.kind(),
                        raw: 33,
                    }
                }
                Err(e) => return Err(error("child same-handle write", e)),
            };
            // sync_all must succeed, including after an observed refused write.
            file.sync_all()?;
            ("live-handle", Some(observation))
        }
    };
    if matches!(
        case,
        "close-before-rename" | "close-prewritten-before-rename"
    ) {
        // Preserve the actual lock-first observation; a refused write is never
        // renamed. The separately named prewritten case tests the same gap.
        drop(file);
        return Ok(ReplacementState {
            held: None,
            stage: "closed-before-rename",
            observation,
        });
    }
    Ok(ReplacementState {
        held: Some(file),
        stage,
        observation,
    })
}

// Ownership is intentional: success must not silently drop the locked handle.
pub fn probe_lock(file: File) -> io::Result<ProbeLock> {
    match file.try_lock() {
        Ok(()) => {
            eprintln!("native try_lock: Acquired, raw_os_error=None");
            Ok(ProbeLock::Acquired(file))
        }
        Err(TryLockError::WouldBlock) => {
            eprintln!("native try_lock: Busy (WouldBlock), raw_os_error=None");
            Ok(ProbeLock::Busy)
        }
        Err(TryLockError::Error(e)) => {
            eprintln!(
                "native try_lock: {e:?}, raw_os_error={:?}",
                e.raw_os_error()
            );
            Err(e)
        }
    }
}

pub fn open_probe(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(path)
}

pub fn acquire_before(path: &Path, timeout: Duration) -> io::Result<File> {
    let deadline = Instant::now() + timeout;
    loop {
        match probe_lock(open_probe(path)?)? {
            ProbeLock::Acquired(file) => return Ok(file),
            ProbeLock::Busy => {
                thread::sleep(POLL_INTERVAL.min(remaining(deadline, "lock acquire deadline")?));
            }
        }
    }
}

pub const LEASE_TAG: &str = "ziyuan-lease";
pub const LEASE_VERSION: &str = "v1";
const LEASE_WAIT: Duration = Duration::from_millis(300);

pub fn new_lease_id() -> String {
    format!(
        "{:016x}{:016x}",
        RandomState::new().hash_one(2_u8),
        RandomState::new().hash_one(3_u8)
    )
}

pub fn lease_file_name(id: &str) -> String {
    format!(".ziyuan-lease-{id}")
}

pub fn lease_line(temp_name: &str, id: &str) -> String {
    format!("{LEASE_TAG} {LEASE_VERSION} temp={temp_name} id={id}\n")
}

fn is_lower_hex_id(text: &str) -> bool {
    text.len() == 32
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

// A lease may only name a plain file in the same directory. Checking for
// separators is not enough: on Windows `join("C:temp")` or `join("a:b")`
// replaces the base path entirely, so the name must be exactly one normal
// path component.
fn is_bare_file_name(name: &str) -> bool {
    if name.is_empty() || name.contains(['\n', '\r']) {
        return false;
    }
    let mut parts = Path::new(name).components();
    matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none()
}

// create_new is the conflict primitive: a second writer must never truncate
// or reuse an existing lease. Content is written before the writer locks it.
pub fn create_lease_file(root: &Path, id: &str, temp_name: &str) -> io::Result<File> {
    if !is_bare_file_name(temp_name) {
        return Err(invalid("lease temp= must be a bare file name"));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(root.join(lease_file_name(id)))?;
    file.write_all(lease_line(temp_name, id).as_bytes())?;
    file.sync_all()?;
    Ok(file)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseContent {
    Valid { temp: String },
    UnknownVersion,
    Unparsable,
    IdMismatch,
}

pub fn parse_lease(line: &str, expected_id: &str) -> LeaseContent {
    let Some(body) = line.strip_suffix('\n') else {
        return LeaseContent::Unparsable;
    };
    if body.contains(['\n', '\r']) {
        return LeaseContent::Unparsable;
    }
    let mut fields = body.split(' ');
    if fields.next() != Some(LEASE_TAG) {
        return LeaseContent::Unparsable;
    }
    match fields.next() {
        Some(LEASE_VERSION) => {}
        Some(_) => return LeaseContent::UnknownVersion,
        None => return LeaseContent::Unparsable,
    }
    let mut temp = None;
    let mut id = None;
    for field in fields {
        if let Some(value) = field.strip_prefix("temp=") {
            // Duplicates are ambiguous: a cleaner must not guess which wins.
            if temp.is_some() {
                return LeaseContent::Unparsable;
            }
            temp = Some(value.to_owned());
        } else if let Some(value) = field.strip_prefix("id=") {
            if id.is_some() {
                return LeaseContent::Unparsable;
            }
            id = Some(value.to_owned());
        } else {
            return LeaseContent::Unparsable;
        }
    }
    let (Some(temp), Some(id)) = (temp, id) else {
        return LeaseContent::Unparsable;
    };
    if !is_lower_hex_id(&id) || id != expected_id {
        return LeaseContent::IdMismatch;
    }
    if !is_bare_file_name(&temp) {
        return LeaseContent::Unparsable;
    }
    LeaseContent::Valid { temp }
}

// Test-only decision model. Every test asserts one of these values, so a
// cleaner rule that silently changed behaviour would fail the assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveReason {
    WriterDeadLeaseVerified,
    OrphanLease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepReason {
    LeaseBusy,
    TempWithoutLease,
    TempAlreadyAbsent,
    TempNotRegular,
    LeaseUnknownVersion,
    LeaseUnparsable,
    LeaseIdMismatch,
    LeaseContentMismatch,
    LeaseAlreadyAbsent,
    TempStillPresent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseDecision {
    Remove { reason: RemoveReason },
    Keep { reason: KeepReason },
}

impl LeaseDecision {
    pub fn remove(reason: RemoveReason) -> Self {
        Self::Remove { reason }
    }

    pub fn keep(reason: KeepReason) -> Self {
        Self::Keep { reason }
    }
}

/// Lease content is one short line. A larger or non-UTF-8 file is not a lease
/// we understand, so it must classify as Unparsable and be retained rather than
/// failing the whole cleanup with an I/O error.
const MAX_LEASE_BYTES: u64 = 4096;

fn read_lease_text(file: &File) -> io::Result<Result<String, KeepReason>> {
    let length = file.metadata()?.len();
    if length > MAX_LEASE_BYTES {
        return Ok(Err(KeepReason::LeaseUnparsable));
    }
    let mut bytes = Vec::with_capacity(length as usize);
    let mut handle = file;
    handle.read_to_end(&mut bytes)?;
    Ok(match String::from_utf8(bytes) {
        Ok(text) => Ok(text),
        Err(_) => Err(KeepReason::LeaseUnparsable),
    })
}

fn classify_line(line: &str, id: &str) -> Result<LeaseContent, KeepReason> {
    match parse_lease(line, id) {
        LeaseContent::Valid { temp } => Ok(LeaseContent::Valid { temp }),
        LeaseContent::UnknownVersion => Err(KeepReason::LeaseUnknownVersion),
        LeaseContent::Unparsable => Err(KeepReason::LeaseUnparsable),
        LeaseContent::IdMismatch => Err(KeepReason::LeaseIdMismatch),
    }
}

// Unreadable, oversized or non-UTF-8 content is a lease we do not understand:
// retain it instead of failing the whole cleanup with an I/O error.
fn classify_lease(file: &File, id: &str) -> Result<LeaseContent, KeepReason> {
    match read_lease_text(file) {
        Ok(Ok(text)) => classify_line(&text, id),
        Ok(Err(reason)) => Err(reason),
        // A read failure is not proof of anything, so retain rather than fail.
        Err(_) => Err(KeepReason::LeaseUnparsable),
    }
}

// Test-only interleaving point, keyed to one scratch directory: it fires
// immediately before the first deletion of that directory, which is after the
// lease lock is dropped in a buggy "verify, release, delete" order and while it
// is still held in the correct order. The key matters because the test binary
// runs its tests in parallel and every cleaner calls this path.
type DeleteHook = std::sync::Arc<dyn Fn() + Send + Sync>;
static DELETE_HOOK: std::sync::Mutex<Option<(PathBuf, DeleteHook)>> = std::sync::Mutex::new(None);

pub fn install_delete_hook(root: &Path, hook: DeleteHook) {
    *DELETE_HOOK.lock().unwrap() = Some((root.to_path_buf(), hook));
}

pub fn clear_delete_hook() {
    *DELETE_HOOK.lock().unwrap() = None;
}

fn run_delete_hook(root: &Path) {
    let hook = DELETE_HOOK
        .lock()
        .unwrap()
        .as_ref()
        .filter(|(scoped, _)| scoped == root)
        .map(|(_, hook)| std::sync::Arc::clone(hook));
    if let Some(hook) = hook {
        hook();
    }
}

// Temp-recovery entry point: never waits on a lease lock, only probes.
pub fn recover_temp(root: &Path, id: &str, temp_name: &str) -> io::Result<LeaseDecision> {
    let temp = root.join(temp_name);
    let lease_path = root.join(lease_file_name(id));
    let lease = match open_probe(&lease_path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(if temp.exists() {
                LeaseDecision::keep(KeepReason::TempWithoutLease)
            } else {
                LeaseDecision::keep(KeepReason::TempAlreadyAbsent)
            });
        }
        Err(e) => return Err(error("cleaner lease open", e)),
    };
    let held = match probe_lock(lease)? {
        ProbeLock::Busy => return Ok(LeaseDecision::keep(KeepReason::LeaseBusy)),
        ProbeLock::Acquired(file) => file,
    };
    let content = match classify_lease(&held, id) {
        Ok(content) => content,
        Err(reason) => return Ok(LeaseDecision::keep(reason)),
    };
    let LeaseContent::Valid { temp: recorded } = content else {
        return Err(invalid("cleaner lease: unexpected content shape"));
    };
    if recorded != temp_name {
        return Ok(LeaseDecision::keep(KeepReason::LeaseContentMismatch));
    }
    match std::fs::metadata(&temp) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(LeaseDecision::keep(KeepReason::TempAlreadyAbsent));
        }
        Err(e) => return Err(error("cleaner temp metadata", e)),
        Ok(metadata) if !metadata.is_file() => {
            return Ok(LeaseDecision::keep(KeepReason::TempNotRegular));
        }
        Ok(_) => {}
    }
    // Order is load-bearing: the temp file goes first, the lease last. The
    // lease handle stays locked until both are gone, so a second cleaner cannot
    // verify and delete concurrently; it observes Busy or a vanished lease.
    run_delete_hook(root);
    match std::fs::remove_file(&temp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(LeaseDecision::keep(KeepReason::TempAlreadyAbsent));
        }
        Err(e) => return Err(error("cleaner temp removal", e)),
    }
    std::fs::remove_file(&lease_path).map_err(|e| error("cleaner lease removal", e))?;
    drop(held);
    Ok(LeaseDecision::remove(RemoveReason::WriterDeadLeaseVerified))
}

// Lease-garbage entry point: only an orphan lease (its referenced temp is
// gone) is removed, and only after the same version/id/content checks.
pub fn reclaim_orphan_lease(root: &Path, id: &str) -> io::Result<LeaseDecision> {
    let lease_path = root.join(lease_file_name(id));
    let lease = match open_probe(&lease_path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(LeaseDecision::keep(KeepReason::LeaseAlreadyAbsent));
        }
        Err(e) => return Err(error("orphan lease open", e)),
    };
    let held = match probe_lock(lease)? {
        ProbeLock::Busy => return Ok(LeaseDecision::keep(KeepReason::LeaseBusy)),
        ProbeLock::Acquired(file) => file,
    };
    let content = match classify_lease(&held, id) {
        Ok(content) => content,
        Err(reason) => return Ok(LeaseDecision::keep(reason)),
    };
    let LeaseContent::Valid { temp } = content else {
        return Err(invalid("orphan lease: unexpected content shape"));
    };
    if root.join(&temp).exists() {
        return Ok(LeaseDecision::keep(KeepReason::TempStillPresent));
    }
    std::fs::remove_file(&lease_path).map_err(|e| error("orphan lease removal", e))?;
    drop(held);
    Ok(LeaseDecision::remove(RemoveReason::OrphanLease))
}

fn error(stage: &str, error: io::Error) -> io::Error {
    // Windows reports socket read timeouts as TimedOut; Unix may use WouldBlock.
    let kind = match error.kind() {
        io::ErrorKind::WouldBlock => io::ErrorKind::TimedOut,
        kind => kind,
    };
    io::Error::new(kind, format!("{stage}: {error}"))
}

fn invalid(stage: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, stage)
}

fn remaining(deadline: Instant, stage: &str) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, stage))
}

// Reapply the remaining absolute deadline on every operation: neither a slow
// trickle of bytes nor a missing newline can extend the wait indefinitely.
pub fn read_message(stream: &mut TcpStream, deadline: Instant, stage: &str) -> io::Result<String> {
    let mut message = Vec::new();
    loop {
        stream
            .set_read_timeout(Some(remaining(deadline, stage)?))
            .map_err(|e| error(stage, e))?;
        let mut byte = [0];
        match stream.read(&mut byte) {
            Ok(0) => return Err(error(stage, io::ErrorKind::UnexpectedEof.into())),
            Ok(_) if byte[0] == b'\n' => {
                return String::from_utf8(message)
                    .map_err(|_| error(stage, io::ErrorKind::InvalidData.into()));
            }
            Ok(_) if message.len() < MAX_MESSAGE => message.push(byte[0]),
            Ok(_) => return Err(error(stage, io::ErrorKind::InvalidData.into())),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(error(stage, e)),
        }
    }
}

pub fn send_message(
    stream: &mut TcpStream,
    message: &str,
    deadline: Instant,
    stage: &str,
) -> io::Result<()> {
    if message.len() > MAX_MESSAGE || message.contains(['\n', '\r']) {
        return Err(invalid(stage));
    }
    let framed = format!("{message}\n");
    let mut bytes = framed.as_bytes();
    while !bytes.is_empty() {
        stream
            .set_write_timeout(Some(remaining(deadline, stage)?))
            .map_err(|e| error(stage, e))?;
        match stream.write(bytes) {
            Ok(0) => return Err(error(stage, io::ErrorKind::WriteZero.into())),
            Ok(count) => bytes = &bytes[count..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(error(stage, e)),
        }
    }
    Ok(())
}

pub fn accept_ready(
    listener: &TcpListener,
    id: &str,
    stage: &str,
    timeout: Duration,
) -> io::Result<TcpStream> {
    let deadline = Instant::now() + timeout;
    listener
        .set_nonblocking(true)
        .map_err(|e| error("parent accept setup", e))?;
    let mut stream = loop {
        let left = remaining(deadline, "parent accept deadline")?;
        match listener.accept() {
            Ok((stream, peer)) if peer.ip() == Ipv4Addr::LOCALHOST => break stream,
            Ok(_) => return Err(invalid("parent accept: non-loopback peer")),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_INTERVAL.min(left));
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(error("parent accept", e)),
        }
    };
    // Winsock accepted sockets inherit the listener's nonblocking mode.
    stream
        .set_nonblocking(false)
        .map_err(|e| error("parent READY socket setup", e))?;
    let message = read_message(&mut stream, deadline, "parent READY read")?;
    if message != format!("READY {id} {stage}") {
        return Err(error(
            "parent READY mismatch",
            io::ErrorKind::InvalidData.into(),
        ));
    }
    Ok(stream)
}

pub struct Scratch {
    path: PathBuf,
    remove_on_drop: bool,
}

impl Scratch {
    pub fn create() -> io::Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| invalid("scratch clock"))?
            .as_nanos();
        for _ in 0..32 {
            let counter = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ziyuan-temp-lease-probe-{}-{nanos}-{counter}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        remove_on_drop: true,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(error("scratch create", e)),
            }
        }
        Err(error(
            "scratch collision limit",
            io::ErrorKind::AlreadyExists.into(),
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if self.remove_on_drop
            && let Err(e) = fs::remove_dir_all(&self.path)
        {
            let _ = writeln!(io::stderr().lock(), "scratch cleanup: {e}");
        }
    }
}

// The child is always killed and reaped before Scratch's destructor runs,
// including while unwinding. No output pipes can fill and deadlock the child.
pub struct ChildGuard {
    child: Child,
    listener: TcpListener,
    id: String,
    scratch: Scratch,
}

impl ChildGuard {
    pub fn spawn(case: &str) -> io::Result<Self> {
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| error("parent bind", e))?;
        let address = listener
            .local_addr()
            .map_err(|e| error("parent address", e))?;
        let scratch = Scratch::create()?;
        // Randomly seeded hashers avoid a predictable PID/time-only ID.
        let id = format!(
            "{:016x}{:016x}",
            RandomState::new().hash_one(0_u8),
            RandomState::new().hash_one(1_u8)
        );
        let child =
            Command::new(std::env::current_exe().map_err(|e| error("child executable", e))?)
                .args(["--exact", "probe_child_entry", "--nocapture"])
                .env(ENV, case)
                .env(format!("{ENV}_ADDR"), address.to_string())
                .env(format!("{ENV}_ID"), &id)
                .env(format!("{ENV}_ROOT"), &scratch.path)
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|e| error("child spawn", e))?;
        Ok(Self {
            child,
            listener,
            id,
            scratch,
        })
    }

    pub fn ready(&mut self, stage: &str, timeout: Duration) -> io::Result<TcpStream> {
        accept_ready(&self.listener, &self.id, stage, timeout)
    }

    pub fn root(&self) -> &Path {
        &self.scratch.path
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn finish(&mut self, timeout: Duration) -> io::Result<()> {
        if self.reap(timeout)?.success() {
            Ok(())
        } else {
            Err(io::Error::other("child finish: unsuccessful exit"))
        }
    }

    pub fn reap(&mut self, timeout: Duration) -> io::Result<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().map_err(|e| error("child reap", e))? {
                return Ok(status);
            }
            thread::sleep(POLL_INTERVAL.min(remaining(deadline, "child exit deadline")?));
        }
    }

    pub fn kill(&mut self) -> io::Result<()> {
        if self
            .child
            .try_wait()
            .map_err(|e| error("child kill status", e))?
            .is_none()
        {
            self.child.kill().map_err(|e| error("child kill", e))?;
        }
        Ok(())
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Err(e) = self
            .kill()
            .and_then(|()| self.reap(DEFAULT_TIMEOUT).map(|_| ()))
        {
            // If the OS cannot confirm termination, retain the owned directory
            // rather than deleting paths that a possibly-live child still uses.
            self.scratch.remove_on_drop = false;
            let _ = writeln!(
                io::stderr().lock(),
                "child guard cleanup (scratch retained): {e}"
            );
        }
    }
}

pub fn handshake(timeout: Duration) -> io::Result<()> {
    let mut child = ChildGuard::spawn("handshake")?;
    let mut stream = child.ready("connected", timeout)?;
    send_message(
        &mut stream,
        "CONTINUE",
        Instant::now() + timeout,
        "parent CONTINUE",
    )?;
    child.finish(timeout)
}

fn env_text(name: &str) -> io::Result<String> {
    std::env::var(name)
        .map_err(|_| invalid(&format!("child configuration: missing or invalid {name}")))
}

// Each variant models one kill point of the writer lifecycle. The lease
// handle (and temp handle) is owned until the flow says otherwise.
enum LeaseCase {
    None,
    CloseBeforeRename { lease: Option<File> },
    Held { _lease: File, _temp: File },
    OnlyLease { _lease: File },
    Conflict { path: PathBuf },
    LockWait { path: PathBuf },
}

impl LeaseCase {
    fn stage(&self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::CloseBeforeRename { .. } => Some("lease-held-closed-before-rename"),
            Self::Held { .. } => Some("lease-held"),
            Self::OnlyLease { .. } => Some("lease-only"),
            Self::Conflict { .. } => Some("lease-conflict"),
            Self::LockWait { .. } => Some("lease-wait"),
        }
    }
}

// Writer order under test: create lease -> lock lease -> create temp -> write
// -> sync -> rename -> close lease handle -> delete lease.
fn prepare_lease_case(case: &str, root: &Path, id: &str) -> io::Result<LeaseCase> {
    let path = root.join(lease_file_name(id));
    match case {
        "lease-close-before-rename" => {
            let lease = lock_new(create_lease_file(root, id, "temp")?)?;
            let mut temp = create_temp(root, false)?;
            write_complete(&mut temp)?;
            // The temp handle closes here; the first-round window opens.
            drop(temp);
            // The lease handle stays held across READY, CONTINUE and rename.
            let lease = Some(lease);
            Ok(LeaseCase::CloseBeforeRename { lease })
        }
        "lease-held-kill" => {
            let lease = lock_new(create_lease_file(root, id, "temp")?)?;
            let mut temp = create_temp(root, false)?;
            write_complete(&mut temp)?;
            Ok(LeaseCase::Held {
                _lease: lease,
                _temp: temp,
            })
        }
        "lease-mismatched-kill" => {
            // Deliberately records a different temp name.
            let lease = lock_new(create_lease_file(root, id, "other-temp")?)?;
            let mut temp = create_temp(root, false)?;
            write_complete(&mut temp)?;
            Ok(LeaseCase::Held {
                _lease: lease,
                _temp: temp,
            })
        }
        "lease-only-kill" => Ok(LeaseCase::OnlyLease {
            _lease: lock_new(create_lease_file(root, id, "temp")?)?,
        }),
        "lease-conflict" => Ok(LeaseCase::Conflict { path }),
        "lease-wait" => Ok(LeaseCase::LockWait { path }),
        _ => Err(invalid("child lease: unknown case")),
    }
}

pub fn run_child() -> io::Result<()> {
    // Validate every input before connecting or doing anything with a path.
    // Never include environment values in errors or mutate paths before validation.
    let case = env_text(ENV)?;
    if !matches!(
        case.as_str(),
        "handshake"
            | "wrong-id"
            | "continue-timeout"
            | "lock-holder"
            | "close-before-rename"
            | "close-prewritten-before-rename"
            | "create-before-lock"
            | "legacy-unleased"
            | "live-restricted-share-lock-first"
            | "live-restricted-share-prewritten"
            | "live-share-delete-lock-first"
            | "live-share-delete-prewritten"
            | "lease-close-before-rename"
            | "lease-held-kill"
            | "lease-mismatched-kill"
            | "lease-only-kill"
            | "lease-conflict"
            | "lease-wait"
    ) {
        return Err(invalid("child configuration: unknown case"));
    }
    let address: SocketAddr = env_text(&format!("{ENV}_ADDR"))?
        .parse()
        .map_err(|_| invalid("child configuration: invalid address"))?;
    if address.ip() != Ipv4Addr::LOCALHOST || address.port() == 0 {
        return Err(invalid("child configuration: address must be 127.0.0.1"));
    }
    let id = env_text(&format!("{ENV}_ID"))?;
    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("child configuration: invalid ID"));
    }
    let root = PathBuf::from(
        std::env::var_os(format!("{ENV}_ROOT"))
            .ok_or_else(|| invalid("child configuration: missing root"))?,
    );
    if !root.is_absolute() || !root.is_dir() {
        return Err(invalid(
            "child configuration: root must be an existing absolute directory",
        ));
    }

    let mut stream = TcpStream::connect_timeout(&address, DEFAULT_TIMEOUT)
        .map_err(|e| error("child connect", e))?;
    let replacement = if case.starts_with("live-")
        || matches!(
            case.as_str(),
            "close-before-rename"
                | "close-prewritten-before-rename"
                | "create-before-lock"
                | "legacy-unleased"
        ) {
        Some(prepare_replacement(&case, &root)?)
    } else {
        None
    };
    let held = if case == "lock-holder" {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(root.join("held.lock"))?;
        match probe_lock(file)? {
            ProbeLock::Acquired(file) => Some(file),
            ProbeLock::Busy => return Err(io::Error::other("child new lock unexpectedly busy")),
        }
    } else {
        None
    };
    let lease_case = if matches!(
        case.as_str(),
        "lease-close-before-rename"
            | "lease-held-kill"
            | "lease-mismatched-kill"
            | "lease-only-kill"
            | "lease-conflict"
            | "lease-wait"
    ) {
        prepare_lease_case(&case, &root, &id)?
    } else {
        LeaseCase::None
    };
    let ready_id = if case == "wrong-id" { "wrong-id" } else { &id };
    let stage = if let Some(state) = &replacement {
        state.stage
    } else if let Some(stage) = lease_case.stage() {
        stage
    } else if case == "lock-holder" {
        "locked"
    } else {
        "connected"
    };
    send_message(
        &mut stream,
        &format!("READY {ready_id} {stage}"),
        Instant::now() + DEFAULT_TIMEOUT,
        "child READY",
    )?;
    if let Some(observation) = replacement
        .as_ref()
        .and_then(|state| state.observation.as_ref())
    {
        send_message(
            &mut stream,
            &observation.message(),
            Instant::now() + DEFAULT_TIMEOUT,
            "child write observation",
        )?;
    }
    let timeout = if case == "continue-timeout" {
        CONTINUE_TIMEOUT
    } else {
        DEFAULT_TIMEOUT
    };
    match &lease_case {
        LeaseCase::Conflict { path } => {
            let go = read_message(
                &mut stream,
                Instant::now() + DEFAULT_TIMEOUT,
                "child lease GO",
            )?;
            if go != "GO" {
                return Err(invalid("child lease GO mismatch"));
            }
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(path)
            {
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => send_message(
                    &mut stream,
                    "LEASE_CONFLICT",
                    Instant::now() + DEFAULT_TIMEOUT,
                    "child lease conflict",
                )?,
                Ok(_) => {
                    return Err(invalid(
                        "child lease conflict: create_new unexpectedly succeeded",
                    ));
                }
                Err(e) => return Err(error("child lease conflict", e)),
            }
            // The second writer reports the conflict and exits; it must never
            // fall through to the shared write path or touch the lease.
            return Ok(());
        }
        LeaseCase::LockWait { path } => {
            let go = read_message(
                &mut stream,
                Instant::now() + DEFAULT_TIMEOUT,
                "child lease GO",
            )?;
            if go != "GO" {
                return Err(invalid("child lease GO mismatch"));
            }
            let started = Instant::now();
            let message = match acquire_before(path, LEASE_WAIT) {
                Ok(file) => {
                    drop(file);
                    "WAIT_ACQUIRED".to_owned()
                }
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                    format!("WAIT_TIMEOUT {}", started.elapsed().as_millis())
                }
                Err(e) => return Err(error("child lease wait", e)),
            };
            send_message(
                &mut stream,
                &message,
                Instant::now() + DEFAULT_TIMEOUT,
                "child lease wait result",
            )?;
        }
        _ => {}
    }
    let response = read_message(&mut stream, Instant::now() + timeout, "child CONTINUE");
    if case == "continue-timeout" {
        return match response {
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(()),
            _ => Err(invalid("child CONTINUE: expected deadline expiry")),
        };
    }
    if response? != "CONTINUE" {
        return Err(error(
            "child CONTINUE mismatch",
            io::ErrorKind::InvalidData.into(),
        ));
    }
    // Keep the owned lock alive across READY and the entire CONTINUE wait.
    drop(held);
    let rename_after_close = matches!(
        case.as_str(),
        "close-before-rename" | "close-prewritten-before-rename"
    ) && replacement
        .as_ref()
        .is_some_and(|state| state.observation == Some(WriteObservation::Complete));
    if case == "create-before-lock" {
        let mut file = replacement
            .and_then(|state| state.held)
            .ok_or_else(|| invalid("child create-before-lock: missing handle"))?;
        // Evidence window only. After CONTINUE use the explicitly distinct
        // prewrite-then-lock ordering, not a claim of protected writing.
        write_complete(&mut file)?;
        let file = lock_new(file)?;
        drop(file);
    } else {
        drop(replacement);
    }
    if rename_after_close {
        fs::rename(root.join("temp"), root.join("target"))?;
    }
    if let LeaseCase::CloseBeforeRename { lease } = lease_case
        && let Some(lease) = lease
    {
        // Lock still held while the temp file is renamed, then release the
        // lease handle and only afterwards delete the lease file.
        fs::rename(root.join("temp"), root.join("target"))?;
        drop(lease);
        fs::remove_file(root.join(lease_file_name(&id)))?;
    }
    Ok(())
}
