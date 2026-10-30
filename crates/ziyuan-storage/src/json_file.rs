//! 带锁、有版本的 JSON 状态文件：供文本工程、作品订阅等需要整体读改写的小型本地状态使用。
//!
//! 文件内容是一个信封：
//!
//! ```json
//! { "schema_version": 1, "revision": 3, "data": { ... } }
//! ```
//!
//! - `schema_version`：调用者的数据格式版本（从 1 起）。比本版新的拒绝读取，提示升级；
//!   不比本版新的交给 `T` 反序列化，下次保存时写成本版。
//! - `revision`：每次保存加一（从 1 起），调用者可据此判断状态是否被别人改过。
//!
//! 并发：同目录下固定的 `<文件名>.lock` 是锁文件，只加锁、从不替换或删除。
//! [`JsonFile::load`] 持共享锁读取；[`JsonFile::update`] 持独占锁，在锁内重新读取、交给闭包修改、
//! 再保存，因此不会有基于过期内容的写入。没有不加锁的保存接口。等锁有上限
//! （[`DEFAULT_LOCK_TIMEOUT`]，可用 [`JsonFile::with_lock_timeout`] 调整），超时返回
//! [`JsonFileError::LockTimeout`]。
//!
//! 保存：先在内存中序列化并核对字节上限，再写入同目录的唯一临时文件、刷盘，最后改名替换旧文件
//! （Windows 上同样直接覆盖，不先删旧文件）。序列化失败、超限或写入失败时旧文件原样保留。
//! 读取与写入用同一字节上限；文件损坏时报错，绝不当作空状态重置。文件不存在才视为尚未初始化。
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 默认最长等锁时间。
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const LOCK_POLL: Duration = Duration::from_millis(10);

#[derive(Debug)]
pub enum JsonFileError {
    Io(io::Error),
    /// 在 `waited` 内没有等到锁文件 `lock` 上的锁。
    LockTimeout {
        lock: PathBuf,
        waited: Duration,
    },
    /// 文件或待保存的内容超过 `limit` 字节。
    TooLarge {
        limit: u64,
    },
    /// 文件内容不是有效的状态信封，或数据与 `T` 不符。
    Corrupt(String),
    /// 文件由更新版本写入（`found` 大于本版支持的 `supported`）。
    Newer {
        found: u64,
        supported: u32,
    },
    /// 待保存的值无法序列化。
    Serialize(String),
}
impl fmt::Display for JsonFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "状态文件读写失败: {e}"),
            Self::LockTimeout { lock, waited } => write!(
                f,
                "等待状态文件锁 {} 超时（{} 毫秒），可能有其他进程正在使用",
                lock.display(),
                waited.as_millis()
            ),
            Self::TooLarge { limit } => write!(f, "状态文件超过 {limit} 字节上限"),
            Self::Corrupt(reason) => write!(f, "状态文件已损坏: {reason}"),
            Self::Newer { found, supported } => write!(
                f,
                "状态文件格式 v{found} 由更新版本的紫垣写入，本版只支持到 v{supported}，请升级 ziyuan 后再打开"
            ),
            Self::Serialize(reason) => write!(f, "状态无法序列化: {reason}"),
        }
    }
}
impl std::error::Error for JsonFileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}
impl From<io::Error> for JsonFileError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// 读到或刚保存的状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored<T> {
    /// 文件中的格式版本；刚保存的总是本版。
    pub schema_version: u32,
    /// 保存次数，从 1 起。
    pub revision: u64,
    pub value: T,
}

/// 一个 JSON 状态文件的句柄。只记路径与参数，不持有打开的文件，可在多个线程、进程中各建一个。
pub struct JsonFile<T> {
    path: PathBuf,
    lock_path: PathBuf,
    schema_version: u32,
    max_bytes: u64,
    lock_timeout: Duration,
    _value: PhantomData<fn() -> T>,
    #[cfg(test)]
    fault: Option<Fault>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    CreateTemp,
    WriteTemp,
    Replace,
}

impl<T> JsonFile<T> {
    /// `path` 须含文件名；`schema_version` 从 1 起；读写都不得超过 `max_bytes` 字节。
    ///
    /// # Panics
    /// `path` 没有文件名或 `schema_version` 为 0 时。
    pub fn new(path: impl Into<PathBuf>, schema_version: u32, max_bytes: u64) -> Self {
        let path = path.into();
        let name = path.file_name().expect("状态文件路径须含文件名");
        assert!(schema_version >= 1, "格式版本从 1 起");
        let mut lock_name = name.to_os_string();
        lock_name.push(".lock");
        Self {
            lock_path: path.with_file_name(lock_name),
            path,
            schema_version,
            max_bytes,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
            _value: PhantomData,
            #[cfg(test)]
            fault: None,
        }
    }

    /// 最长等锁时间。
    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 固定的锁文件：`<文件名>.lock`，与状态文件同目录。
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    fn dir(&self) -> &Path {
        match self.path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        }
    }

    fn acquire(&self, shared: bool) -> Result<File, JsonFileError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.lock_path)?;
        let started = Instant::now();
        loop {
            let attempt = if shared {
                file.try_lock_shared()
            } else {
                file.try_lock()
            };
            match attempt {
                Ok(()) => return Ok(file),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(e)) => return Err(e.into()),
            }
            let waited = started.elapsed();
            if waited >= self.lock_timeout {
                return Err(JsonFileError::LockTimeout {
                    lock: self.lock_path.clone(),
                    waited,
                });
            }
            std::thread::sleep(LOCK_POLL.min(self.lock_timeout - waited));
        }
    }

    #[cfg(test)]
    fn inject(&self, at: Fault) -> io::Result<()> {
        match self.fault {
            Some(fault) if fault == at => Err(io::Error::other(format!("注入故障 {at:?}"))),
            _ => Ok(()),
        }
    }

    /// 写入同目录的唯一临时文件、刷盘，再改名覆盖状态文件。失败时删掉临时文件，旧文件不动。
    fn replace(&self, bytes: &[u8]) -> Result<(), JsonFileError> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = self.dir();
        let name = self.path.file_name().expect("状态文件路径含文件名");
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let (tmp, mut file) = loop {
            let mut tmp_name = std::ffi::OsString::from(".");
            tmp_name.push(name);
            tmp_name.push(format!(
                ".{}.{nanos}.{}.tmp",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let tmp = dir.join(tmp_name);
            #[cfg(test)]
            self.inject(Fault::CreateTemp)?;
            match OpenOptions::new().write(true).create_new(true).open(&tmp) {
                Ok(file) => break (tmp, file),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        };
        let written = (|| {
            #[cfg(test)]
            self.inject(Fault::WriteTemp)?;
            file.write_all(bytes)?;
            file.sync_all()
        })();
        drop(file);
        let replaced = written.and_then(|()| {
            #[cfg(test)]
            self.inject(Fault::Replace)?;
            // std 在 Windows 上以覆盖方式改名（MOVEFILE_REPLACE_EXISTING / POSIX 语义），不先删目标。
            fs::rename(&tmp, &self.path)
        });
        if let Err(e) = replaced {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        sync_dir(dir);
        Ok(())
    }
}

/// 改名后刷目录项，使替换在断电后也持久。只是尽力而为：此时新内容已就位，报错会让调用者误以为没有保存。
/// Windows 的 std 打不开目录句柄，跳过。
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(dir) = File::open(dir) {
        let _ = dir.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[derive(Deserialize)]
struct Header {
    schema_version: u64,
    revision: u64,
}

#[derive(Deserialize)]
struct Body<T> {
    data: T,
}

#[derive(Serialize)]
struct Envelope<'a, T> {
    schema_version: u32,
    revision: u64,
    data: &'a T,
}

/// 写满 `limit` 字节后再写即报错的缓冲区，使超大的值不必整个序列化进内存。
struct Limited {
    buf: Vec<u8>,
    limit: u64,
    over: bool,
}
impl Write for Limited {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.buf.len() as u64 + data.len() as u64 > self.limit {
            self.over = true;
            return Err(io::Error::other("超过字节上限"));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<T: Serialize + DeserializeOwned> JsonFile<T> {
    /// 持共享锁读取当前状态；文件（或其目录）不存在时为 `Ok(None)`。不建目录，不改动状态文件。
    pub fn load(&self) -> Result<Option<Stored<T>>, JsonFileError> {
        match fs::metadata(self.dir()) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
        let _lock = self.acquire(true)?;
        self.read_locked()
    }

    /// 持独占锁读改写：在锁内重新读取当前状态（不存在时为 `None`）交给 `f`，`f` 返回新值与
    /// 任意结果 `R`；新值以 `revision + 1`、本版格式保存后，连同 `R` 一并返回。
    ///
    /// `f` 返回错误、读取失败（损坏、过新、超限）或保存失败时不写入任何内容，旧文件原样保留。
    /// `f` 在持锁期间运行，应当只做内存中的计算。
    pub fn update<R, E>(
        &self,
        f: impl FnOnce(Option<Stored<T>>) -> Result<(T, R), E>,
    ) -> Result<(Stored<T>, R), E>
    where
        E: From<JsonFileError>,
    {
        fs::create_dir_all(self.dir()).map_err(JsonFileError::from)?;
        let _lock = self.acquire(false)?;
        let current = self.read_locked()?;
        let revision = match &current {
            None => 1,
            Some(stored) => stored
                .revision
                .checked_add(1)
                .ok_or_else(|| JsonFileError::Corrupt("revision 已到上限".into()))?,
        };
        let (value, out) = f(current)?;
        let bytes = self.encode(revision, &value)?;
        self.replace(&bytes)?;
        Ok((
            Stored {
                schema_version: self.schema_version,
                revision,
                value,
            },
            out,
        ))
    }

    fn read_locked(&self) -> Result<Option<Stored<T>>, JsonFileError> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut bytes = Vec::new();
        file.take(self.max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > self.max_bytes {
            return Err(JsonFileError::TooLarge {
                limit: self.max_bytes,
            });
        }
        self.decode(&bytes).map(Some)
    }

    fn decode(&self, bytes: &[u8]) -> Result<Stored<T>, JsonFileError> {
        let corrupt = |e: serde_json::Error| JsonFileError::Corrupt(e.to_string());
        // 先只读版本：过新的文件报「请升级」，而不是因数据结构不同报损坏。
        let header: Header = serde_json::from_slice(bytes).map_err(corrupt)?;
        if header.schema_version == 0 {
            return Err(JsonFileError::Corrupt("schema_version 须从 1 起".into()));
        }
        if header.schema_version > u64::from(self.schema_version) {
            return Err(JsonFileError::Newer {
                found: header.schema_version,
                supported: self.schema_version,
            });
        }
        if header.revision == 0 {
            return Err(JsonFileError::Corrupt("revision 须从 1 起".into()));
        }
        let body: Body<T> = serde_json::from_slice(bytes).map_err(corrupt)?;
        Ok(Stored {
            schema_version: header.schema_version as u32,
            revision: header.revision,
            value: body.data,
        })
    }

    fn encode(&self, revision: u64, value: &T) -> Result<Vec<u8>, JsonFileError> {
        let mut out = Limited {
            buf: Vec::new(),
            limit: self.max_bytes,
            over: false,
        };
        let envelope = Envelope {
            schema_version: self.schema_version,
            revision,
            data: value,
        };
        let written = serde_json::to_writer_pretty(&mut out, &envelope).map_err(|e| e.to_string());
        let written = written.and_then(|()| out.write_all(b"\n").map_err(|e| e.to_string()));
        match written {
            Ok(()) => Ok(out.buf),
            Err(_) if out.over => Err(JsonFileError::TooLarge {
                limit: self.max_bytes,
            }),
            Err(reason) => Err(JsonFileError::Serialize(reason)),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serializer};
    use std::fs::{self, File, OpenOptions};
    use std::time::Instant;

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    struct Counter {
        n: u64,
        note: String,
    }

    const LIMIT: u64 = 1 << 16;
    const CHILD_ENV: &str = "ZIYUAN_JSON_FILE_CHILD";
    const CHILD_ROUNDS: u64 = 20;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ziyuan-json-file-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn counter_file(dir: &Path) -> JsonFile<Counter> {
        JsonFile::new(dir.join("state.json"), 1, LIMIT)
    }

    fn bump(file: &JsonFile<Counter>) -> Result<Stored<Counter>, JsonFileError> {
        file.update(|current| {
            let mut value = current.map(|s| s.value).unwrap_or_default();
            value.n += 1;
            Ok::<_, JsonFileError>((value, ()))
        })
        .map(|(stored, ())| stored)
    }

    fn set_note(file: &JsonFile<Counter>, note: &str) -> Result<Stored<Counter>, JsonFileError> {
        file.update(|current| {
            let mut value = current.map(|s| s.value).unwrap_or_default();
            value.note = note.to_owned();
            Ok::<_, JsonFileError>((value, ()))
        })
        .map(|(stored, ())| stored)
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect()
    }

    #[test]
    fn missing_file_initializes_then_reopens_and_updates() {
        let dir = temp_dir("create");
        let file = counter_file(&dir);
        assert_eq!(file.load().unwrap(), None);
        assert!(!dir.exists(), "纯读取不应建目录");

        let first = bump(&file).unwrap();
        assert_eq!(
            first,
            Stored {
                schema_version: 1,
                revision: 1,
                value: Counter {
                    n: 1,
                    note: String::new()
                }
            }
        );
        let reopened = counter_file(&dir);
        assert_eq!(reopened.load().unwrap(), Some(first.clone()));

        let (second, seen) = reopened
            .update(|current| {
                let current = current.unwrap();
                let mut value = current.value.clone();
                value.n += 10;
                Ok::<_, JsonFileError>((value, current.revision))
            })
            .unwrap();
        assert_eq!(seen, 1);
        assert_eq!(second.revision, 2);
        assert_eq!(second.value.n, 11);
        assert_eq!(file.load().unwrap(), Some(second));
        let text = fs::read_to_string(file.path()).unwrap();
        assert!(text.contains("\"schema_version\": 1"), "{text}");
        assert!(text.contains("\"revision\": 2"), "{text}");
        assert!(leftovers(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn closure_error_writes_nothing() {
        let dir = temp_dir("abort");
        let file = counter_file(&dir);
        let err = file
            .update(|_| Err::<(Counter, ()), _>(JsonFileError::Corrupt("abort".into())))
            .unwrap_err();
        assert!(matches!(err, JsonFileError::Corrupt(_)));
        assert!(!file.path().exists());
        bump(&file).unwrap();
        let before = fs::read(file.path()).unwrap();
        let _ = file.update(|_| Err::<(Counter, ()), _>(JsonFileError::Corrupt("abort".into())));
        assert_eq!(fs::read(file.path()).unwrap(), before);
        assert_eq!(file.load().unwrap().unwrap().revision, 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_files_are_errors_and_never_reset() {
        let dir = temp_dir("corrupt");
        let file = counter_file(&dir);
        fs::create_dir_all(&dir).unwrap();
        let cases = [
            "",
            "{not json",
            "[]",
            r#"{"revision":1,"data":{"n":1,"note":""}}"#,
            r#"{"schema_version":"1","revision":1,"data":{"n":1,"note":""}}"#,
            r#"{"schema_version":0,"revision":1,"data":{"n":1,"note":""}}"#,
            r#"{"schema_version":1,"revision":0,"data":{"n":1,"note":""}}"#,
            r#"{"schema_version":1,"revision":-1,"data":{"n":1,"note":""}}"#,
            r#"{"schema_version":1,"revision":1}"#,
            r#"{"schema_version":1,"revision":1,"data":{"n":"x","note":""}}"#,
            r#"{"schema_version":1,"revision":18446744073709551615,"data":{"n":1,"note":""}}"#,
        ];
        for (i, case) in cases.iter().enumerate() {
            fs::write(file.path(), case).unwrap();
            if i + 1 < cases.len() {
                let err = file.load().unwrap_err();
                assert!(matches!(err, JsonFileError::Corrupt(_)), "{case}: {err:?}");
            }
            let err = bump(&file).unwrap_err();
            assert!(matches!(err, JsonFileError::Corrupt(_)), "{case}: {err:?}");
            assert_eq!(fs::read_to_string(file.path()).unwrap(), *case);
        }
        assert!(leftovers(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn newer_schema_is_rejected_and_older_is_upgraded_on_save() {
        let dir = temp_dir("schema");
        fs::create_dir_all(&dir).unwrap();
        let file: JsonFile<Counter> = JsonFile::new(dir.join("state.json"), 2, LIMIT);
        let newer = r#"{"schema_version":3,"revision":4,"data":{"n":1,"note":""}}"#;
        fs::write(file.path(), newer).unwrap();
        assert!(matches!(
            file.load().unwrap_err(),
            JsonFileError::Newer {
                found: 3,
                supported: 2
            }
        ));
        let err = bump(&file).unwrap_err();
        assert!(matches!(err, JsonFileError::Newer { .. }), "{err:?}");
        assert!(err.to_string().contains("升级"), "{err}");
        assert_eq!(fs::read_to_string(file.path()).unwrap(), newer);

        let older = r#"{"schema_version":1,"revision":4,"data":{"n":1,"note":""}}"#;
        fs::write(file.path(), older).unwrap();
        assert_eq!(file.load().unwrap().unwrap().schema_version, 1);
        let saved = bump(&file).unwrap();
        assert_eq!(
            (saved.schema_version, saved.revision, saved.value.n),
            (2, 5, 2)
        );
        assert_eq!(file.load().unwrap(), Some(saved));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn byte_limit_applies_equally_to_reads_and_writes() {
        let dir = temp_dir("limit");
        let roomy = counter_file(&dir);
        let note = "x".repeat(100);
        set_note(&roomy, &note).unwrap();
        let exact = fs::metadata(roomy.path()).unwrap().len();

        // 恰好等于上限：写得进，重开读得出。
        let tight: JsonFile<Counter> = JsonFile::new(roomy.path(), 1, exact);
        let saved = set_note(&tight, &note).unwrap();
        assert_eq!(fs::metadata(tight.path()).unwrap().len(), exact);
        let reopened: JsonFile<Counter> = JsonFile::new(roomy.path(), 1, exact);
        assert_eq!(reopened.load().unwrap(), Some(saved.clone()));

        // 多一个字节：写入被拒，旧状态原样保留。
        let before = fs::read(tight.path()).unwrap();
        let err = set_note(&tight, &format!("{note}y")).unwrap_err();
        assert!(
            matches!(err, JsonFileError::TooLarge { limit } if limit == exact),
            "{err:?}"
        );
        assert_eq!(fs::read(tight.path()).unwrap(), before);
        assert_eq!(tight.load().unwrap(), Some(saved));
        assert!(leftovers(&dir).is_empty());

        // 文件比上限多一个字节（尾随空白仍是合法 JSON）：读取与更新都被拒，不改动文件。
        let mut grown = before.clone();
        grown.push(b' ');
        fs::write(tight.path(), &grown).unwrap();
        assert!(matches!(
            reopened.load().unwrap_err(),
            JsonFileError::TooLarge { .. }
        ));
        assert!(matches!(
            bump(&tight).unwrap_err(),
            JsonFileError::TooLarge { .. }
        ));
        assert_eq!(fs::read(tight.path()).unwrap(), grown);
        assert_eq!(roomy.load().unwrap().unwrap().value.note, note);
        fs::remove_dir_all(dir).unwrap();
    }

    #[derive(Debug, Deserialize)]
    struct Picky {
        fail: bool,
    }
    impl Serialize for Picky {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            if self.fail {
                return Err(serde::ser::Error::custom("refused"));
            }
            #[derive(Serialize)]
            struct Plain {
                fail: bool,
            }
            Plain { fail: false }.serialize(s)
        }
    }

    #[test]
    fn serialization_failure_preserves_old_state() {
        let dir = temp_dir("serialize");
        let file: JsonFile<Picky> = JsonFile::new(dir.join("state.json"), 1, LIMIT);
        file.update(|_| Ok::<_, JsonFileError>((Picky { fail: false }, ())))
            .unwrap();
        let before = fs::read(file.path()).unwrap();
        let err = file
            .update(|_| Ok::<_, JsonFileError>((Picky { fail: true }, ())))
            .unwrap_err();
        assert!(matches!(err, JsonFileError::Serialize(_)), "{err:?}");
        assert_eq!(fs::read(file.path()).unwrap(), before);
        assert_eq!(file.load().unwrap().unwrap().revision, 1);
        assert!(leftovers(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failures_at_temp_and_replace_preserve_original() {
        let dir = temp_dir("faults");
        let mut file = counter_file(&dir);
        bump(&file).unwrap();
        let before = fs::read(file.path()).unwrap();
        for fault in [Fault::CreateTemp, Fault::WriteTemp, Fault::Replace] {
            file.fault = Some(fault);
            let err = bump(&file).unwrap_err();
            assert!(matches!(err, JsonFileError::Io(_)), "{fault:?}: {err:?}");
            assert_eq!(fs::read(file.path()).unwrap(), before, "{fault:?}");
            assert!(
                leftovers(&dir).is_empty(),
                "{fault:?}: {:?}",
                leftovers(&dir)
            );
        }
        file.fault = None;
        assert_eq!(bump(&file).unwrap().revision, 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lock_file_is_fixed_and_separate_from_data() {
        let dir = temp_dir("lockfile");
        let file = counter_file(&dir);
        assert_eq!(file.lock_path(), dir.join("state.json.lock"));
        bump(&file).unwrap();
        let lock = File::open(file.lock_path()).unwrap();
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            lock.metadata().unwrap().ino()
        };
        for _ in 0..3 {
            bump(&file).unwrap();
        }
        assert_eq!(fs::metadata(file.lock_path()).unwrap().len(), 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(fs::metadata(file.lock_path()).unwrap().ino(), inode);
        }
        drop(lock);
        assert_eq!(file.load().unwrap().unwrap().revision, 4);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lock_wait_is_bounded() {
        let dir = temp_dir("timeout");
        let file = counter_file(&dir).with_lock_timeout(Duration::from_millis(150));
        bump(&file).unwrap();
        let before = fs::read(file.path()).unwrap();

        let holder = OpenOptions::new()
            .write(true)
            .open(file.lock_path())
            .unwrap();
        holder.lock().unwrap();
        let started = Instant::now();
        let err = bump(&file).unwrap_err();
        assert!(matches!(err, JsonFileError::LockTimeout { .. }), "{err:?}");
        assert!(matches!(
            file.load().unwrap_err(),
            JsonFileError::LockTimeout { .. }
        ));
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(300), "{waited:?}");
        assert!(waited < Duration::from_secs(5), "{waited:?}");
        holder.unlock().unwrap();

        // 共享锁之间不互斥：别人在读时照样能读，但不能写。
        holder.lock_shared().unwrap();
        assert_eq!(file.load().unwrap().unwrap().revision, 1);
        assert!(matches!(
            bump(&file).unwrap_err(),
            JsonFileError::LockTimeout { .. }
        ));
        drop(holder);
        assert_eq!(fs::read(file.path()).unwrap(), before);
        assert_eq!(bump(&file).unwrap().revision, 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concurrent_handles_do_not_lose_updates() {
        let dir = temp_dir("threads");
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    let file = counter_file(&dir).with_lock_timeout(Duration::from_secs(30));
                    for _ in 0..25 {
                        bump(&file).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let stored = counter_file(&dir).load().unwrap().unwrap();
        assert_eq!((stored.revision, stored.value.n), (100, 100));
        assert!(leftovers(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    /// 由 [`concurrent_processes_do_not_lose_updates`] 以子进程运行；直接运行时什么都不做。
    #[test]
    fn child_process_increments() {
        let Some(dir) = std::env::var_os(CHILD_ENV) else {
            return;
        };
        let file = counter_file(Path::new(&dir)).with_lock_timeout(Duration::from_secs(60));
        for _ in 0..CHILD_ROUNDS {
            bump(&file).unwrap();
        }
    }

    #[test]
    fn concurrent_processes_do_not_lose_updates() {
        let dir = temp_dir("processes");
        let exe = std::env::current_exe().unwrap();
        let children: Vec<_> = (0..3)
            .map(|_| {
                std::process::Command::new(&exe)
                    .args([
                        "--exact",
                        "json_file::tests::child_process_increments",
                        "--test-threads=1",
                    ])
                    .env(CHILD_ENV, &dir)
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        let file = counter_file(&dir).with_lock_timeout(Duration::from_secs(60));
        for _ in 0..CHILD_ROUNDS {
            bump(&file).unwrap();
        }
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }
        let stored = file.load().unwrap().unwrap();
        assert_eq!(stored.revision, 4 * CHILD_ROUNDS);
        assert_eq!(stored.value.n, 4 * CHILD_ROUNDS);
        assert!(leftovers(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
