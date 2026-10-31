use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::BuildHasher;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const CONTINUE_TIMEOUT: Duration = Duration::from_millis(300);
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const MAX_MESSAGE: usize = 256;
const ENV: &str = "ZIYUAN_TEMP_LEASE_PROBE";
static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

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

struct Scratch {
    path: PathBuf,
    remove_on_drop: bool,
}

impl Scratch {
    fn create() -> io::Result<Self> {
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

pub fn run_child() -> io::Result<()> {
    // Validate every input before connecting or doing anything with a path.
    // Never include environment values in errors, and never create child paths.
    let case = env_text(ENV)?;
    if !matches!(case.as_str(), "handshake" | "wrong-id" | "continue-timeout") {
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
    let ready_id = if case == "wrong-id" { "wrong-id" } else { &id };
    send_message(
        &mut stream,
        &format!("READY {ready_id} connected"),
        Instant::now() + DEFAULT_TIMEOUT,
        "child READY",
    )?;
    let timeout = if case == "continue-timeout" {
        CONTINUE_TIMEOUT
    } else {
        DEFAULT_TIMEOUT
    };
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
    Ok(())
}
