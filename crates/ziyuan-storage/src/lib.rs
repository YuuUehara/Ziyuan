//! 紫垣事件簿：事件存储接口与实现。文件实现为 append-only 文本日志（v1 格式），未来可换成 SQLite。
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use ziyuan_core::CourtEvent;

mod codec;
pub mod json_file;
pub mod recovery;
pub mod workspace;

pub use json_file::{JsonFile, JsonFileError, Stored};
pub use workspace::{LockScope, Workspace, WorkspaceError, WriteGuard};

#[derive(Debug)]
pub enum StorageError {
    Io(io::Error),
    /// 事件簿第 `line` 行（从 1 开始）无法解码。
    Corrupt {
        line: usize,
        reason: String,
    },
    /// 第 `line` 行由更新版本的紫垣写入（`what` 为其格式或事件类型），本版无法读取。
    Newer {
        line: usize,
        what: String,
    },
    /// 乐观并发冲突：事件簿已有 `actual` 条事件，而调用者以为是 `expected` 条。未写入任何内容。
    Conflict {
        expected: usize,
        actual: usize,
    },
}
impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "事件簿读写失败: {e}"),
            Self::Corrupt { line, reason } => write!(f, "日志第 {line} 行: {reason}"),
            Self::Newer { line, what } => write!(
                f,
                "事件簿第 {line} 行（{what}）由更新版本的紫垣写入，本版 v{} 读不了，请升级 ziyuan 后再打开",
                env!("CARGO_PKG_VERSION")
            ),
            Self::Conflict { expected, actual } => write!(
                f,
                "事件簿已被其他进程更新（预期 {expected} 条事件，实际 {actual} 条）"
            ),
        }
    }
}
impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Corrupt { .. } | Self::Newer { .. } | Self::Conflict { .. } => None,
        }
    }
}
impl From<io::Error> for StorageError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// 朝廷事件流的持久化接口。
pub trait EventStore {
    /// 按写入顺序读取全部已提交事件；尚无事件时返回空列表。
    fn load(&self) -> Result<Vec<CourtEvent>, StorageError>;
    /// 第 `known` 条之后的事件（调用者已有前 `known` 条）。事件簿不足 `known` 条时返回
    /// [`StorageError::Conflict`]。文件实现只读取新增的部分。
    fn load_since(&self, known: usize) -> Result<Vec<CourtEvent>, StorageError> {
        let mut events = self.load()?;
        if events.len() < known {
            return Err(StorageError::Conflict {
                expected: known,
                actual: events.len(),
            });
        }
        Ok(events.split_off(known))
    }
    /// 前 `upto` 条事件中 `keep` 选中的，按写入顺序。只为查几条事件时用：逐条解码、
    /// 只留选中的，不把整本事件簿留在内存里，也不动增量读取的位置。
    fn load_where(
        &self,
        upto: usize,
        keep: &dyn Fn(&CourtEvent) -> bool,
    ) -> Result<Vec<CourtEvent>, StorageError> {
        let mut events = self.load()?;
        events.truncate(upto);
        events.retain(|e| keep(e));
        Ok(events)
    }
    /// 仅当事件簿中恰好有 `expected` 条事件时追加 `events`（乐观并发），否则返回
    /// [`StorageError::Conflict`] 且不写入。调用者负责先校验事件（见 `ziyuan-app`）。
    fn append(&mut self, expected: usize, events: &[CourtEvent]) -> Result<(), StorageError>;
    /// 写入者是否记时辰（[`CourtEvent::Clock`]）。持久化的事件簿记；内存事件簿（测试）不记，
    /// 以免事件条数随时钟变化。
    fn stamps_time(&self) -> bool {
        false
    }
    /// 读会典（快照）：须由同一 `tag`（调用者的状态格式，如版本号）所修，且仍是本事件簿前若干条的。
    /// 没有会典时为 `Ok(None)`；有而用不得时返回原因，调用者应从头回放。
    /// 读到后，随后的 [`load_since`](Self::load_since)`(snapshot.events)` 只读会典之后的事件。
    fn load_snapshot(&self, _tag: &str) -> Result<Option<Snapshot>, String> {
        Ok(None)
    }
    /// 修会典：`write` 写出的状态涵盖事件簿的前 `events` 条。只在刚读到或刚写完第 `events` 条时
    /// 修得（否则不知其位置），修不得时返回 `Ok(false)`。会典随时可删，删了从头回放。
    fn save_snapshot(
        &mut self,
        _tag: &str,
        _events: usize,
        _write: &dyn Fn(&mut dyn Write) -> io::Result<()>,
    ) -> Result<bool, StorageError> {
        Ok(false)
    }
}

/// 会典（快照）：调用者序列化的状态，涵盖事件簿的前 `events` 条。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub events: usize,
    pub data: Vec<u8>,
}

/// 事件类型的代码（事件簿中的类型字段，小写），如 `edict`、`tool`、`usage`。
pub fn event_kind(event: &CourtEvent) -> String {
    codec::kind_of(event).to_lowercase()
}

impl<T: EventStore + ?Sized> EventStore for &mut T {
    fn stamps_time(&self) -> bool {
        (**self).stamps_time()
    }
    fn load(&self) -> Result<Vec<CourtEvent>, StorageError> {
        (**self).load()
    }
    fn load_since(&self, known: usize) -> Result<Vec<CourtEvent>, StorageError> {
        (**self).load_since(known)
    }
    fn load_snapshot(&self, tag: &str) -> Result<Option<Snapshot>, String> {
        (**self).load_snapshot(tag)
    }
    fn load_where(
        &self,
        upto: usize,
        keep: &dyn Fn(&CourtEvent) -> bool,
    ) -> Result<Vec<CourtEvent>, StorageError> {
        (**self).load_where(upto, keep)
    }
    fn save_snapshot(
        &mut self,
        tag: &str,
        events: usize,
        write: &dyn Fn(&mut dyn Write) -> io::Result<()>,
    ) -> Result<bool, StorageError> {
        (**self).save_snapshot(tag, events, write)
    }
    fn append(&mut self, expected: usize, events: &[CourtEvent]) -> Result<(), StorageError> {
        (**self).append(expected, events)
    }
}

/// `<root>/.ziyuan/court.log` 文本事件簿，可由多个进程共用。
///
/// 读取持共享文件锁，追加持独占文件锁并先核对事件数，因此并发写入者不会产生重复 ID 或交错的行。
/// 每行一个事件；末尾没有换行的残行来自中断的写入，读取时忽略，下次追加前截掉。
///
/// 事件簿只追加、不改写，因此记下「前若干条事件止于第几字节」的游标后，
/// 增量读取与追加前的核对都只需读新增的部分。
pub struct EventLog {
    path: PathBuf,
    cursor: Mutex<Cursor>,
}

/// 事件簿前 `events` 条事件恰好占前 `bytes` 字节（`bytes` 处是行首）。
#[derive(Clone, Copy, Default)]
struct Cursor {
    events: usize,
    bytes: u64,
    /// 游标所指的文件（Unix 下为设备号与 inode），用于发现事件簿被删除重建。
    file: Option<(u64, u64)>,
}

#[cfg(unix)]
fn identity(file: &File) -> io::Result<Option<(u64, u64)>> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata()?;
    Ok(Some((meta.dev(), meta.ino())))
}
#[cfg(not(unix))]
fn identity(_: &File) -> io::Result<Option<(u64, u64)>> {
    Ok(None)
}

impl EventLog {
    pub fn new(root: &Path) -> Self {
        Self {
            path: root.join(".ziyuan").join("court.log"),
            cursor: Mutex::new(Cursor::default()),
        }
    }

    /// 游标仍然适用于 `file`：是同一个文件，文件不短于游标，且游标前一字节是换行
    /// （或游标在开头）。文件被删除重建等情形下不适用，退回从头读取。
    fn usable(file: &mut File, cursor: Cursor) -> io::Result<bool> {
        if cursor.bytes == 0 {
            return Ok(true);
        }
        if cursor.file.is_some() && cursor.file != identity(file)? {
            return Ok(false);
        }
        if file.metadata()?.len() < cursor.bytes {
            return Ok(false);
        }
        let mut last = [0u8];
        file.seek(SeekFrom::Start(cursor.bytes - 1))?;
        file.read_exact(&mut last)?;
        Ok(last[0] == b'\n')
    }

    /// 在已加锁的 `file` 上逐行读取游标之后（不适用则从头）的已提交内容，每行（不含换行符）
    /// 连同其序号（从 0 起）交给 `line`。边读边交，不把整段内容留在内存里：
    /// 事件簿有几百 MB 时，一次读完再解码会让内存峰值翻倍。
    fn read_tail(
        &self,
        file: &mut File,
        mut line: impl FnMut(usize, &[u8]) -> Result<(), StorageError>,
    ) -> Result<Tail, StorageError> {
        let mut start = *self.cursor.lock().expect("锁未中毒");
        let replaced = !Self::usable(file, start)?;
        if replaced {
            start = Cursor::default();
        }
        start.file = identity(file)?;
        file.seek(SeekFrom::Start(start.bytes))?;
        let mut reader = io::BufReader::with_capacity(1 << 16, &mut *file);
        let mut end = start;
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let n = reader.read_until(b'\n', &mut buf)?;
            if n == 0 || buf.last() != Some(&b'\n') {
                return Ok(Tail {
                    start,
                    end,
                    torn: n > 0,
                    replaced,
                });
            }
            line(end.events, &buf[..n - 1])?;
            end.events += 1;
            end.bytes += n as u64;
        }
    }

    fn set_cursor(&self, cursor: Cursor) {
        *self.cursor.lock().expect("锁未中毒") = cursor;
    }

    /// 执行者锁：同一朝廷同一时刻只允许一个进程执行诏令，这样未结束的已指派诏令只可能来自中断的进程，
    /// 可以安全地重新指派。已被占用时返回 `None`。进程退出（含崩溃）时由操作系统释放。
    pub fn try_lock_executor(&self) -> Result<Option<ExecutorLock>, StorageError> {
        let dir = self.path.parent().expect("事件簿路径含目录");
        fs::create_dir_all(dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("executor.lock"))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(ExecutorLock { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(e.into()),
        }
    }
}

/// [`EventLog::read_tail`] 读到的范围：起止游标、末尾有无残行、游标是否因文件被替换而作废。
struct Tail {
    start: Cursor,
    end: Cursor,
    torn: bool,
    replaced: bool,
}

/// 持有期间本进程是唯一的执行者；drop 时释放。
pub struct ExecutorLock {
    _file: File,
}

impl EventStore for EventLog {
    fn stamps_time(&self) -> bool {
        true
    }

    fn load(&self) -> Result<Vec<CourtEvent>, StorageError> {
        self.set_cursor(Cursor::default());
        self.load_since(0)
    }

    fn load_since(&self, known: usize) -> Result<Vec<CourtEvent>, StorageError> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.set_cursor(Cursor::default());
                return match known {
                    0 => Ok(Vec::new()),
                    _ => Err(StorageError::Conflict {
                        expected: known,
                        actual: 0,
                    }),
                };
            }
            Err(e) => return Err(e.into()),
        };
        file.lock_shared()?;
        // 游标超前于调用者（如本进程刚追加过）则从头读，跳过调用者已有的。
        let cursor = *self.cursor.lock().expect("锁未中毒");
        if cursor.events > known && Self::usable(&mut file, cursor)? {
            self.set_cursor(Cursor::default());
        }
        let mut events = Vec::new();
        let tail = self.read_tail(&mut file, |number, line| {
            if number >= known {
                events.push(decode_line(number, line)?);
            }
            Ok(())
        })?;
        self.set_cursor(tail.end);
        let total = tail.end.events;
        // 文件已被替换：调用者已有的前 `known` 条不再可信，让其从头读取。
        if total < known || (tail.replaced && known > 0) {
            return Err(StorageError::Conflict {
                expected: known,
                actual: total,
            });
        }
        Ok(events)
    }

    fn load_where(
        &self,
        upto: usize,
        keep: &dyn Fn(&CourtEvent) -> bool,
    ) -> Result<Vec<CourtEvent>, StorageError> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        file.lock_shared()?;
        let mut reader = io::BufReader::with_capacity(1 << 16, file);
        let (mut events, mut line) = (Vec::new(), Vec::new());
        for number in 0..upto {
            line.clear();
            let n = reader.read_until(b'\n', &mut line)?;
            if n == 0 || line.last() != Some(&b'\n') {
                break;
            }
            let event = decode_line(number, &line[..n - 1])?;
            if keep(&event) {
                events.push(event);
            }
        }
        Ok(events)
    }

    fn load_snapshot(&self, tag: &str) -> Result<Option<Snapshot>, String> {
        let file = match File::open(self.snapshot_path()) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("读不了会典：{e}")),
        };
        let mut reader = io::BufReader::with_capacity(1 << 16, file);
        let mut header = String::new();
        reader
            .read_line(&mut header)
            .map_err(|e| format!("会典首行读不出：{e}"))?;
        let fields: Vec<&str> = header.trim_end_matches('\n').split('\t').collect();
        let [HUIDIAN, HUIDIAN_FORMAT, made_by, events, bytes, hash] = fields.as_slice() else {
            return Err("会典格式不认得".into());
        };
        if *made_by != tag {
            return Err(format!("会典为 {made_by} 所修，本版为 {tag}"));
        }
        let parse = |v: &str| v.parse::<u64>().map_err(|_| "会典首行数字不对".to_owned());
        let (events, bytes) = (parse(events)? as usize, parse(bytes)?);
        let hash = u64::from_str_radix(hash, 16).map_err(|_| "会典首行摘要不对")?;
        let mut file = File::open(&self.path).map_err(|e| format!("打不开事件簿：{e}"))?;
        file.lock_shared().map_err(|e| e.to_string())?;
        let cursor = Cursor {
            events,
            bytes,
            file: identity(&file).map_err(|e| e.to_string())?,
        };
        let matches = events > 0
            && Self::usable(&mut file, cursor).map_err(|e| e.to_string())?
            && Self::fingerprint(&mut file, bytes).map_err(|e| e.to_string())? == hash;
        if !matches {
            return Err("会典与事件簿对不上".into());
        }
        // 按文件大小一次分配，免得边读边扩容时占到两倍。
        let size = reader.get_ref().metadata().map_or(0, |m| m.len()) as usize;
        let mut data = Vec::with_capacity(size.saturating_sub(header.len()));
        reader
            .read_to_end(&mut data)
            .map_err(|e| format!("读不了会典：{e}"))?;
        self.set_cursor(cursor);
        Ok(Some(Snapshot { events, data }))
    }

    fn save_snapshot(
        &mut self,
        tag: &str,
        events: usize,
        write: &dyn Fn(&mut dyn Write) -> io::Result<()>,
    ) -> Result<bool, StorageError> {
        let cursor = *self.cursor.lock().expect("锁未中毒");
        if events == 0 || cursor.events != events || tag.contains(['\t', '\n']) {
            return Ok(false);
        }
        let mut file = File::open(&self.path)?;
        file.lock_shared()?;
        if !Self::usable(&mut file, cursor)? {
            return Ok(false);
        }
        let hash = Self::fingerprint(&mut file, cursor.bytes)?;
        drop(file);
        let path = self.snapshot_path();
        // 各进程各写各的临时文件，改名是原子的：读者只见旧会典或新会典。
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        let mut out = io::BufWriter::new(File::create(&tmp)?);
        writeln!(
            out,
            "{HUIDIAN}\t{HUIDIAN_FORMAT}\t{tag}\t{events}\t{}\t{hash:016x}",
            cursor.bytes
        )?;
        if let Err(e) = write(&mut out) {
            drop(out);
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        out.into_inner().map_err(|e| e.into_error())?.sync_data()?;
        fs::rename(&tmp, &path)?;
        Ok(true)
    }

    fn append(&mut self, expected: usize, events: &[CourtEvent]) -> Result<(), StorageError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        // 不用 append 模式：Windows 上追加句柄没有截断残行所需的写权限（set_len 报拒绝访问）。
        // 持独占锁后定位到已提交的末尾再写，效果相同。
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.path)?;
        file.lock()?;
        let tail = self.read_tail(&mut file, |_, _| Ok(()))?;
        let (committed, actual, start) = (tail.end.bytes, tail.end.events, tail.start);
        self.set_cursor(tail.end);
        if actual != expected {
            return Err(StorageError::Conflict { expected, actual });
        }
        if tail.torn {
            file.set_len(committed)?;
        }
        file.seek(SeekFrom::Start(committed))?;
        let mut content = String::new();
        for event in events {
            content.push_str(&codec::encode_event(event));
            content.push('\n');
        }
        file.write_all(content.as_bytes())?;
        file.sync_data()?;
        self.set_cursor(Cursor {
            events: actual + events.len(),
            bytes: committed + content.len() as u64,
            file: start.file,
        });
        Ok(())
    }
}

/// 解码第 `number` 行（从 0 起）。
fn decode_line(number: usize, line: &[u8]) -> Result<CourtEvent, StorageError> {
    let corrupt = |reason| StorageError::Corrupt {
        line: number + 1,
        reason,
    };
    let text = std::str::from_utf8(line).map_err(|e| corrupt(e.to_string()))?;
    codec::decode_event(text).map_err(|e| match e {
        codec::DecodeError::Newer(what) => StorageError::Newer {
            line: number + 1,
            what,
        },
        codec::DecodeError::Malformed(reason) => corrupt(reason),
    })
}

/// 会典文件的首行标记与格式。
const HUIDIAN: &str = "ZIYUAN-HUIDIAN";
const HUIDIAN_FORMAT: &str = "1";
/// 核对会典与事件簿时，取事件簿开头与会典止处之前各这么多字节求摘要。
const FINGERPRINT_SPAN: u64 = 4096;

impl EventLog {
    fn snapshot_path(&self) -> PathBuf {
        self.path.with_file_name("huidian")
    }

    /// 事件簿前 `bytes` 字节的指纹：开头与末尾各一段的 FNV-1a 摘要。开头含建朝的种子，
    /// 换了一本事件簿便对不上；末尾一段核对会典止处之前没有被改动。
    fn fingerprint(file: &mut File, bytes: u64) -> io::Result<u64> {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut feed = |file: &mut File, start: u64, len: u64| -> io::Result<()> {
            let mut buf = vec![0; len as usize];
            file.seek(SeekFrom::Start(start))?;
            file.read_exact(&mut buf)?;
            for b in buf {
                hash = (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
            }
            Ok(())
        };
        let span = bytes.min(FINGERPRINT_SPAN);
        feed(file, 0, span)?;
        feed(file, bytes - span, span)?;
        Ok(hash)
    }

    /// 会典的状况（`ziyuan migrate` 所用）：涵盖的事件条数与所修的 `tag`；没有会典时为 `None`。
    pub fn snapshot_header(&self) -> Option<(usize, String)> {
        let file = File::open(self.snapshot_path()).ok()?;
        let mut line = String::new();
        io::BufReader::new(file).read_line(&mut line).ok()?;
        let fields: Vec<&str> = line.trim_end().split('\t').collect();
        match fields.as_slice() {
            [HUIDIAN, HUIDIAN_FORMAT, tag, events, ..] => {
                Some((events.parse().ok()?, (*tag).to_owned()))
            }
            _ => None,
        }
    }
}

/// 各发布版本，按序号（[`LogReport::readable_since`] 所用，从 1 起）排列。
pub const RELEASES: [&str; 17] = [
    "0.1.0",
    "0.2.0",
    "0.3.0",
    "0.4.0",
    "0.5.0",
    "0.6.0",
    "0.7.0",
    "0.8.0",
    "0.9.0",
    "0.10.0",
    "1.0.0-rc.1",
    "1.0.0-rc.2",
    "1.0.0-rc.3",
    "1.0.0-rc.4",
    "1.0.0",
    "1.1.0",
    "1.2.0",
];

/// 序号对应的发布版本号；超出所知的报本版。
pub fn release_name(ordinal: u32) -> &'static str {
    ordinal
        .checked_sub(1)
        .and_then(|i| RELEASES.get(i as usize))
        .copied()
        .unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// [`EventLog::inspect`] 的结果（`ziyuan migrate` 所用）。
#[derive(Debug)]
pub struct LogReport {
    /// 完整的事件行数（不含末尾未写完的残行）。
    pub events: usize,
    /// 末尾有未写完的残行（下次追加前截掉）。
    pub torn_tail: bool,
    /// 打开整本事件簿至少要哪个发布版本：[`RELEASES`] 中的序号（从 1 起，见 [`release_name`]）；
    /// 空事件簿为 1。
    pub readable_since: u32,
    /// 最先要求 `readable_since` 的一行：行号与事件类型（小写）。
    pub first_needing: Option<(usize, String)>,
    /// 本版读不了的第一行（损坏或由更新版本写入）；有则其后不再检查。
    pub problem: Option<StorageError>,
}

impl EventLog {
    /// 逐行检查事件簿：本版能否读、至少要哪个发布版本才能打开。只读，不取执行者锁。
    pub fn inspect(&self) -> Result<LogReport, StorageError> {
        let file = File::open(&self.path)?;
        file.lock_shared()?;
        let mut report = LogReport {
            events: 0,
            torn_tail: false,
            readable_since: 1,
            first_needing: None,
            problem: None,
        };
        // 逐行读，不把整本事件簿读进内存。
        let mut reader = io::BufReader::with_capacity(1 << 16, file);
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = reader.read_until(b'\n', &mut line)?;
            if n == 0 || line.last() != Some(&b'\n') {
                report.torn_tail = n > 0;
                break;
            }
            let number = report.events + 1;
            report.events = number;
            let since = std::str::from_utf8(&line[..n - 1])
                .map_err(|e| codec::DecodeError::Malformed(e.to_string()))
                .and_then(|text| Ok((codec::readable_since(text)?, text)));
            match since {
                Ok((minor, text)) if minor > report.readable_since => {
                    report.readable_since = minor;
                    let kind = text.split('\t').nth(1).unwrap_or_default().to_lowercase();
                    report.first_needing = Some((number, kind));
                }
                Ok(_) => {}
                Err(codec::DecodeError::Newer(what)) => {
                    report.problem = Some(StorageError::Newer { line: number, what });
                    break;
                }
                Err(codec::DecodeError::Malformed(reason)) => {
                    report.problem = Some(StorageError::Corrupt {
                        line: number,
                        reason,
                    });
                    break;
                }
            }
        }
        Ok(report)
    }
}

/// 内存事件簿：用于测试，或嵌入时不需要持久化的场景。
#[derive(Clone, Debug, Default)]
pub struct MemoryEventLog {
    events: Vec<CourtEvent>,
    /// 会典：(tag, 涵盖的事件条数, 内容)。
    snapshot: Option<(String, usize, Vec<u8>)>,
}
impl EventStore for MemoryEventLog {
    fn load_snapshot(&self, tag: &str) -> Result<Option<Snapshot>, String> {
        match &self.snapshot {
            None => Ok(None),
            Some((made_by, ..)) if made_by != tag => Err(format!("会典为 {made_by} 所修")),
            Some((_, events, _)) if *events > self.events.len() => Err("会典与事件簿对不上".into()),
            Some((_, events, data)) => Ok(Some(Snapshot {
                events: *events,
                data: data.clone(),
            })),
        }
    }
    fn save_snapshot(
        &mut self,
        tag: &str,
        events: usize,
        write: &dyn Fn(&mut dyn Write) -> io::Result<()>,
    ) -> Result<bool, StorageError> {
        if events == 0 || events > self.events.len() {
            return Ok(false);
        }
        let mut data = Vec::new();
        write(&mut data)?;
        self.snapshot = Some((tag.to_owned(), events, data));
        Ok(true)
    }
    fn load(&self) -> Result<Vec<CourtEvent>, StorageError> {
        Ok(self.events.clone())
    }
    fn append(&mut self, expected: usize, events: &[CourtEvent]) -> Result<(), StorageError> {
        if self.events.len() != expected {
            return Err(StorageError::Conflict {
                expected,
                actual: self.events.len(),
            });
        }
        self.events.extend_from_slice(events);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ziyuan_core::GovernanceMode;

    fn temp_root(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ziyuan-storage-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn reform(mode: GovernanceMode) -> CourtEvent {
        CourtEvent::InstitutionReformed(mode)
    }

    #[test]
    fn stale_writer_gets_conflict_and_writes_nothing() {
        let root = temp_root("conflict");
        let (mut a, mut b) = (EventLog::new(&root), EventLog::new(&root));
        a.append(0, &[CourtEvent::Founded(GovernanceMode::Chancellor)])
            .unwrap();
        let err = b.append(0, &[reform(GovernanceMode::Cabinet)]).unwrap_err();
        assert!(matches!(
            err,
            StorageError::Conflict {
                expected: 0,
                actual: 1
            }
        ));
        b.append(1, &[reform(GovernanceMode::Cabinet)]).unwrap();
        assert_eq!(a.load().unwrap().len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn load_where_keeps_matches_within_upto() {
        let root = temp_root("where");
        let mut log = EventLog::new(&root);
        let tool = |task, ok| CourtEvent::ToolInvoked {
            task: ziyuan_core::TaskId(task),
            tool: "read_file".into(),
            args_json: "{}".into(),
            ok,
        };
        log.append(
            0,
            &[
                CourtEvent::Founded(GovernanceMode::Chancellor),
                tool(1, true),
                tool(2, true),
                tool(1, false),
                tool(1, true),
            ],
        )
        .unwrap();
        let keep =
            |e: &CourtEvent| matches!(e, CourtEvent::ToolInvoked { task, .. } if task.0 == 1);
        // 第 5 条在 `upto` 之外，不取。
        assert_eq!(
            log.load_where(4, &keep).unwrap(),
            [tool(1, true), tool(1, false)]
        );
        assert_eq!(MemoryEventLog::default().load_where(9, &keep).unwrap(), []);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn torn_tail_is_ignored_then_trimmed() {
        let root = temp_root("torn");
        let mut log = EventLog::new(&root);
        log.append(0, &[CourtEvent::Founded(GovernanceMode::Chancellor)])
            .unwrap();
        // 模拟写到一半时崩溃。
        let mut file = OpenOptions::new().append(true).open(&log.path).unwrap();
        file.write_all(b"v1\tREFORMED\tcab").unwrap();
        drop(file); // Windows 删不掉仍开着的文件。
        assert_eq!(log.load().unwrap().len(), 1);
        log.append(1, &[reform(GovernanceMode::DirectRule)])
            .unwrap();
        assert_eq!(
            log.load().unwrap(),
            [
                CourtEvent::Founded(GovernanceMode::Chancellor),
                reform(GovernanceMode::DirectRule)
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incremental_reads_see_other_writers_and_survive_replacement() {
        use GovernanceMode::*;
        let root = temp_root("incremental");
        let (mut a, mut b) = (EventLog::new(&root), EventLog::new(&root));
        assert_eq!(a.load_since(0).unwrap(), []);
        a.append(0, &[CourtEvent::Founded(Chancellor)]).unwrap();
        assert_eq!(a.load_since(1).unwrap(), []);
        // b 追加两条：a 只读到新增的两条；a 的游标随后据此核对追加。
        b.append(1, &[reform(Cabinet), reform(DirectRule)]).unwrap();
        assert_eq!(
            a.load_since(1).unwrap(),
            [reform(Cabinet), reform(DirectRule)]
        );
        assert!(matches!(
            a.append(1, &[reform(Chancellor)]),
            Err(StorageError::Conflict {
                expected: 1,
                actual: 3
            })
        ));
        a.append(3, &[reform(Chancellor)]).unwrap();
        // 游标超前于调用者：从头读，跳过已有的。
        assert_eq!(
            a.load_since(2).unwrap(),
            [reform(DirectRule), reform(Chancellor)]
        );
        assert!(matches!(
            b.load_since(9),
            Err(StorageError::Conflict {
                expected: 9,
                actual: 4
            })
        ));
        // 坏行的行号按全文计。
        let mut file = OpenOptions::new().append(true).open(&a.path).unwrap();
        file.write_all(b"v1\tREFORMED\tnope\n").unwrap();
        drop(file);
        assert!(matches!(
            a.load_since(4),
            Err(StorageError::Corrupt { line: 5, .. })
        ));
        // 文件被删除重建（更短、内容不同）：游标作废，从头读。
        fs::remove_file(&a.path).unwrap();
        b.append(0, &[CourtEvent::Founded(Cabinet)]).unwrap();
        assert!(matches!(
            a.load_since(4),
            Err(StorageError::Conflict { actual: 1, .. })
        ));
        assert_eq!(a.load().unwrap(), [CourtEvent::Founded(Cabinet)]);
        // 重建得更长、游标处不在行首：同样从头读。
        fs::remove_file(&a.path).unwrap();
        let long = CourtEvent::EmperorRetreat(true);
        b.append(0, &[reform(Cabinet), long.clone(), long.clone()])
            .unwrap();
        // 已有的一条不再可信：报冲突，调用者从头读。
        assert!(matches!(
            a.load_since(1),
            Err(StorageError::Conflict { actual: 3, .. })
        ));
        assert_eq!(a.load_since(1).unwrap(), [long.clone(), long]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inspect_reports_the_oldest_release_that_can_open_the_log() {
        let root = temp_root("inspect");
        let log = EventLog::new(&root);
        let path = root.join(".ziyuan/court.log");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // v0.1 写下的事件簿：v0.1 即可打开。
        fs::write(
            &path,
            "v1\tFOUNDED\tchancellor\nv1\tEDICT\t1\tworks\te69fa5\n",
        )
        .unwrap();
        let r = log.inspect().unwrap();
        assert_eq!((r.events, r.readable_since, r.first_needing), (2, 1, None));
        // 第 3 行复核（v0.3 起）、第 4 行授外藩的诏令（v0.5 起）、末尾残行。
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"v1\tRETREAT\t1\nv1\tEDICT\t2\tworks\t78\tmcp:docs\t2e\nv1\tAPPRO")
            .unwrap();
        let r = log.inspect().unwrap();
        assert_eq!((r.events, r.torn_tail), (4, true));
        assert_eq!(
            (r.readable_since, r.first_needing),
            (5, Some((4, "edict".into())))
        );
        assert!(r.problem.is_none());
        // 更新版本写入的一行：指出行号，其后不查。
        fs::write(
            &path,
            "v1\tFOUNDED\tchancellor\nv1\tCOURT_BANQUET\t1\nv1\tRETREAT\t1\n",
        )
        .unwrap();
        let r = log.inspect().unwrap();
        assert!(
            matches!(r.problem, Some(StorageError::Newer { line: 2, .. })),
            "{r:?}"
        );
        assert_eq!((r.events, r.readable_since), (2, 1));
    }

    #[test]
    fn translation_events_need_release_1_2_0() {
        let root = temp_root("inspect-translation");
        let log = EventLog::new(&root);
        let path = root.join(".ziyuan/court.log");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let reopen = codec::encode_event(&CourtEvent::TranslationReopened {
            project: "0123456789abcdef0123456789abcdef".into(),
            block: "c0001-b0001".into(),
            reason: "重译".into(),
        });
        fs::write(&path, format!("v1\tFOUNDED\tchancellor\n{reopen}\n")).unwrap();
        let r = log.inspect().unwrap();
        assert!(r.problem.is_none(), "{r:?}");
        assert_eq!(
            (r.readable_since, r.first_needing),
            (17, Some((2, "transreopen".into())))
        );
        assert_eq!(release_name(r.readable_since), "1.2.0");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn events_from_a_newer_version_ask_for_an_upgrade() {
        let root = temp_root("newer");
        let mut log = EventLog::new(&root);
        log.append(0, &[CourtEvent::Founded(GovernanceMode::Chancellor)])
            .unwrap();
        let mut file = OpenOptions::new().append(true).open(&log.path).unwrap();
        file.write_all(b"v1\tCOURT_BANQUET\t1\n").unwrap();
        drop(file);
        let err = log.load_since(1).unwrap_err();
        assert!(matches!(err, StorageError::Newer { line: 2, .. }), "{err}");
        assert!(err.to_string().contains("请升级"), "{err}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn executor_lock_is_exclusive_until_dropped() {
        let root = temp_root("executor");
        let log = EventLog::new(&root);
        let held = log.try_lock_executor().unwrap();
        assert!(held.is_some());
        assert!(EventLog::new(&root).try_lock_executor().unwrap().is_none());
        drop(held);
        assert!(log.try_lock_executor().unwrap().is_some());
        fs::remove_dir_all(root).unwrap();
    }
}
