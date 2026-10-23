//! 紫垣事件簿：事件存储接口与实现。文件实现为 append-only 文本日志（v1 格式），未来可换成 SQLite。
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
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
            Self::Corrupt { .. } | Self::Conflict { .. } => None,
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
    /// 仅当事件簿中恰好有 `expected` 条事件时追加 `events`（乐观并发），否则返回
    /// [`StorageError::Conflict`] 且不写入。调用者负责先校验事件（见 `ziyuan-app`）。
    fn append(&mut self, expected: usize, events: &[CourtEvent]) -> Result<(), StorageError>;
}

impl<T: EventStore + ?Sized> EventStore for &mut T {
    fn load(&self) -> Result<Vec<CourtEvent>, StorageError> {
        (**self).load()
    }
    fn append(&mut self, expected: usize, events: &[CourtEvent]) -> Result<(), StorageError> {
        (**self).append(expected, events)
    }
}

/// `<root>/.ziyuan/court.log` 文本事件簿，可由多个进程共用。
///
/// 读取持共享文件锁，追加持独占文件锁并先核对事件数，因此并发写入者不会产生重复 ID 或交错的行。
/// 每行一个事件；末尾没有换行的残行来自中断的写入，读取时忽略，下次追加前截掉。
pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    pub fn new(root: &Path) -> Self {
        Self {
            path: root.join(".ziyuan").join("court.log"),
        }
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
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        file.lock_shared()?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        data[..committed_len(&data)]
            .split_inclusive(|&b| b == b'\n')
            .enumerate()
            .map(|(idx, line)| {
                let corrupt = |reason| StorageError::Corrupt {
                    line: idx + 1,
                    reason,
                };
                let text = std::str::from_utf8(&line[..line.len() - 1])
                    .map_err(|e| corrupt(e.to_string()))?;
                codec::decode_event(text).map_err(corrupt)
            })
            .collect()
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
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        let committed = committed_len(&data);
        let actual = data[..committed].iter().filter(|&&b| b == b'\n').count();
        if actual != expected {
            return Err(StorageError::Conflict { expected, actual });
        }
        if committed < data.len() {
            file.set_len(committed as u64)?;
        }
        let mut content = String::new();
        for event in events {
            content.push_str(&codec::encode_event(event));
            content.push('\n');
        }
        file.write_all(content.as_bytes())?;
        file.sync_data()?;
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
