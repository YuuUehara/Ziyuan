//! 紫垣事件簿：事件存储接口与实现。文件实现为简易 append-only 日志（v1 格式），未来可换成 SQLite。
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
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
}
impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "事件簿读写失败: {e}"),
            Self::Corrupt { line, reason } => write!(f, "日志第 {line} 行: {reason}"),
        }
    }
}
impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Corrupt { .. } => None,
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
    /// 按写入顺序读取全部事件；尚无事件时返回空列表。
    fn load(&self) -> Result<Vec<CourtEvent>, StorageError>;
    /// 追加一批事件。调用者负责先校验事件（见 `ziyuan-app`）。
    fn append(&mut self, events: &[CourtEvent]) -> Result<(), StorageError>;
}

/// `<root>/.ziyuan/court.log` 文本事件簿。单进程演示用：无写锁、无事务。
pub struct EventLog {
    path: PathBuf,
}
impl EventLog {
    pub fn new(root: &Path) -> Self {
        Self {
            path: root.join(".ziyuan").join("court.log"),
        }
    }
}
impl EventStore for EventLog {
    fn load(&self) -> Result<Vec<CourtEvent>, StorageError> {
        let data = match fs::read_to_string(&self.path) {
            Ok(data) => data,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        data.lines()
            .enumerate()
            .map(|(idx, line)| {
                codec::decode_event(line).map_err(|reason| StorageError::Corrupt {
                    line: idx + 1,
                    reason,
                })
            })
            .collect()
    }
    fn append(&mut self, events: &[CourtEvent]) -> Result<(), StorageError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut content = String::new();
        for event in events {
            content.push_str(&codec::encode_event(event));
            content.push('\n');
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(content.as_bytes())?;
        file.sync_data()?;
        Ok(())
    }
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
    fn append(&mut self, events: &[CourtEvent]) -> Result<(), StorageError> {
        self.events.extend_from_slice(events);
        Ok(())
    }
}
