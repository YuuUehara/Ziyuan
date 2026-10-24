//! 紫垣事件簿：事件存储接口与实现。文件实现为 append-only 文本日志（v1 格式），未来可换成 SQLite。
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use ziyuan_core::CourtEvent;

mod codec;

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
                "事件簿第 {line} 行（{what}）由更新版本的紫垣写入，请升级 ziyuan 后再打开"
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
    /// 仅当事件簿中恰好有 `expected` 条事件时追加 `events`（乐观并发），否则返回
    /// [`StorageError::Conflict`] 且不写入。调用者负责先校验事件（见 `ziyuan-app`）。
    fn append(&mut self, expected: usize, events: &[CourtEvent]) -> Result<(), StorageError>;
}

impl<T: EventStore + ?Sized> EventStore for &mut T {
    fn load(&self) -> Result<Vec<CourtEvent>, StorageError> {
        (**self).load()
    }
    fn load_since(&self, known: usize) -> Result<Vec<CourtEvent>, StorageError> {
        (**self).load_since(known)
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

    /// 在已加锁的 `file` 上读取游标之后（不适用则从头）的已提交内容，返回起点游标、
    /// 新内容、其后残行的长度，以及游标是否因文件被替换而作废。
    fn read_tail(&self, file: &mut File) -> io::Result<(Cursor, Vec<u8>, usize, bool)> {
        let mut cursor = *self.cursor.lock().expect("锁未中毒");
        let replaced = !Self::usable(file, cursor)?;
        if replaced {
            cursor = Cursor::default();
        }
        cursor.file = identity(file)?;
        file.seek(SeekFrom::Start(cursor.bytes))?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        let committed = committed_len(&data);
        let torn = data.len() - committed;
        data.truncate(committed);
        Ok((cursor, data, torn, replaced))
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

/// 持有期间本进程是唯一的执行者；drop 时释放。
pub struct ExecutorLock {
    _file: File,
}

impl EventStore for EventLog {
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
        let (start, data, _, replaced) = self.read_tail(&mut file)?;
        // 游标超前于调用者（如本进程刚追加过）则从头读，跳过调用者已有的。
        let (start, data) = if start.events > known {
            self.set_cursor(Cursor::default());
            let (start, data, _, _) = self.read_tail(&mut file)?;
            (start, data)
        } else {
            (start, data)
        };
        let mut events = Vec::new();
        for (idx, line) in data.split_inclusive(|&b| b == b'\n').enumerate() {
            let number = start.events + idx;
            let corrupt = |reason| StorageError::Corrupt {
                line: number + 1,
                reason,
            };
            if number < known {
                continue;
            }
            let text =
                std::str::from_utf8(&line[..line.len() - 1]).map_err(|e| corrupt(e.to_string()))?;
            events.push(codec::decode_event(text).map_err(|e| match e {
                codec::DecodeError::Newer(what) => StorageError::Newer {
                    line: number + 1,
                    what,
                },
                codec::DecodeError::Malformed(reason) => corrupt(reason),
            })?);
        }
        let total = start.events + data.iter().filter(|&&b| b == b'\n').count();
        self.set_cursor(Cursor {
            events: total,
            bytes: start.bytes + data.len() as u64,
            file: start.file,
        });
        // 文件已被替换：调用者已有的前 `known` 条不再可信，让其从头读取。
        if total < known || (replaced && known > 0) {
            return Err(StorageError::Conflict {
                expected: known,
                actual: total,
            });
        }
        Ok(events)
    }

    fn append(&mut self, expected: usize, events: &[CourtEvent]) -> Result<(), StorageError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)?;
        file.lock()?;
        let (start, data, torn, _) = self.read_tail(&mut file)?;
        let committed = start.bytes + data.len() as u64;
        let actual = start.events + data.iter().filter(|&&b| b == b'\n').count();
        self.set_cursor(Cursor {
            events: actual,
            bytes: committed,
            file: start.file,
        });
        if actual != expected {
            return Err(StorageError::Conflict { expected, actual });
        }
        if torn > 0 {
            file.set_len(committed)?;
        }
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

/// 截至最后一个换行符的字节数：其后的内容是未完成的写入。
fn committed_len(data: &[u8]) -> usize {
    data.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1)
}

/// 内存事件簿：用于测试，或嵌入时不需要持久化的场景。
#[derive(Clone, Debug, Default)]
pub struct MemoryEventLog {
    events: Vec<CourtEvent>,
}
impl EventStore for MemoryEventLog {
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
    fn torn_tail_is_ignored_then_trimmed() {
        let root = temp_root("torn");
        let mut log = EventLog::new(&root);
        log.append(0, &[CourtEvent::Founded(GovernanceMode::Chancellor)])
            .unwrap();
        // 模拟写到一半时崩溃。
        let mut file = OpenOptions::new().append(true).open(&log.path).unwrap();
        file.write_all(b"v1\tREFORMED\tcab").unwrap();
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
    fn events_from_a_newer_version_ask_for_an_upgrade() {
        let root = temp_root("newer");
        let mut log = EventLog::new(&root);
        log.append(0, &[CourtEvent::Founded(GovernanceMode::Chancellor)])
            .unwrap();
        let mut file = OpenOptions::new().append(true).open(&log.path).unwrap();
        file.write_all(b"v1\tCOURT_BANQUET\t1\n").unwrap();
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
