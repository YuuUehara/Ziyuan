//! 原子替换的临时文件与租约：命名、归属核验、清理。

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 临时文件名前缀；清理器只认这一种命名。
pub(crate) const TEMP_PREFIX: &str = ".ziyuan-tmp-";

/// 本次写入的临时文件名：不含目标文件名，长度有界——目标名接近文件名上限时也能建出临时文件。
pub(crate) fn fresh_temp_name() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!(
        "{TEMP_PREFIX}{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// 严格匹配 `.ziyuan-tmp-<u32>-<u128>-<u64>`。
///
/// 这是删除的准入门槛，不是提示：放宽一点就会把用户的普通文件当成自己的残留删掉。
pub fn is_candidate_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(TEMP_PREFIX) else {
        return false;
    };
    let mut parts = rest.split('-');
    let (Some(pid), Some(nanos), Some(counter), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    // 先逐字节判十进制：`parse` 会接受 `+1`，也会接受全角数字之外的形式。
    // 再用 parse 判范围：逐字节判断管不了溢出。
    [pid, nanos, counter]
        .iter()
        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        && pid.parse::<u32>().is_ok()
        && nanos.parse::<u128>().is_ok()
        && counter.parse::<u64>().is_ok()
}

/// 租约文件名前缀。
const LEASE_PREFIX: &str = ".ziyuan-lease-";
const LEASE_TAG: &str = "ziyuan-lease";
const LEASE_VERSION: &str = "v1";
/// 写入者等待自己租约锁的上限；清理者从不等待。
const LEASE_LOCK_WAIT: Duration = Duration::from_millis(300);
const LEASE_LOCK_POLL: Duration = Duration::from_millis(5);

/// 保留原因：报告直接用这些名字，不另作映射。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    RecoveryReferenced,
    TempNotRegular,
    TempWithoutLease,
    TempAlreadyAbsent,
    TempAmbiguous,
    LeaseBusy,
    LeaseNotRegular,
    LeaseUnknownVersion,
    LeaseUnparsable,
    LeaseIdMismatch,
    LeaseAlreadyAbsent,
    TempStillPresent,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RecoveryReferenced => "RecoveryReferenced",
            Self::TempNotRegular => "TempNotRegular",
            Self::TempWithoutLease => "TempWithoutLease",
            Self::TempAlreadyAbsent => "TempAlreadyAbsent",
            Self::TempAmbiguous => "TempAmbiguous",
            Self::LeaseBusy => "LeaseBusy",
            Self::LeaseNotRegular => "LeaseNotRegular",
            Self::LeaseUnknownVersion => "LeaseUnknownVersion",
            Self::LeaseUnparsable => "LeaseUnparsable",
            Self::LeaseIdMismatch => "LeaseIdMismatch",
            Self::LeaseAlreadyAbsent => "LeaseAlreadyAbsent",
            Self::TempStillPresent => "TempStillPresent",
        }
    }
}

/// 一个候选或租约的处置。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// 可删除；`Item::acted` 区分“预览里本可删除”与“真的删掉了”。
    ///
    /// 删除真的失败时两条路径的收尾不一样，报告必须照各自的实情读：
    /// - 孤儿租约那一支留在 `Remove`，只是 `acted=false`（它确实可清理，本轮没删掉）；
    /// - 临时文件那一支改成 `Keep(TempStillPresent)`（它还占着盘，不能算可清理）。
    ///
    /// 两种情况都会记一条 `Problem`，所以失败本身从不静默。
    Remove,
    Keep(Reason),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Temp,
    Lease,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Item {
    /// 绝对路径，只给本进程用；报告一律经 `display_relative` 转成相对路径。
    pub path: PathBuf,
    pub kind: Kind,
    pub outcome: Outcome,
    /// 本轮真的删掉了它。`apply=false`、或删除失败时都是 `false`。
    ///
    /// 失败时 `outcome` 怎么走由各自的处置支路决定，两处都要连着 `acted` 一起看：
    /// 临时文件删不掉 → `Keep(TempStillPresent)` 且 `acted=false`；孤儿租约删不掉
    /// → 仍是 `Remove` 且 `acted=false`（于是 `Apply` 模式下“可删除数 > 已删除数”
    /// 只可能来自后者）。两种情况都会记一条 `Problem`，失败原因在那里。
    pub acted: bool,
}

impl Item {
    /// 报告里的原因名：保留原因原样给出；可删除项按设计补上两个删除原因之一。
    ///
    /// 删除原因不带字段：`Kind::Temp` 的可删除项只可能来自“持锁核验通过的租约”，
    /// `Kind::Lease` 的只可能来自“`temp=` 已不存在的合法租约”，一一对应。
    pub fn reason_name(&self) -> &'static str {
        match self.outcome {
            Outcome::Keep(reason) => reason.as_str(),
            Outcome::Remove => match self.kind {
                Kind::Temp => "WriterDeadLeaseVerified",
                Kind::Lease => "OrphanLease",
            },
        }
    }
}

/// 逐项 I/O 失败：进报告，不改变处置结论。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Problem {
    pub path: PathBuf,
    pub message: String,
}

/// 一次写入的租约：先于临时文件存在，覆盖到改名完成。
///
/// 清理者侧不需要它：那边只按路径取锁、按句柄核验。所以这里的方法都是给
/// 写入者用的，`pub` 是为了让它们成为 crate 之外可达的一部分协议实现。
pub struct Lease {
    path: std::path::PathBuf,
    file: Option<File>,
    /// 表示“`release()` 已经试过”，**不是**“租约已经不在了”：删除可能失败。
    release_attempted: bool,
}

impl Lease {
    /// 建租约、写内容、刷盘、加锁、核验路径仍指向自己。
    ///
    /// 核验不是保险：内容写好到加锁之间是唯一还能被清理器回收的窗口，而删除
    /// 名字之后手里的句柄加锁仍然会成功。不核验就会带着一个已被删除的租约
    /// 继续建临时文件，留下没有归属证据、谁也证明不了陈旧的残留。
    pub fn hold(parent: &Path, temp_name: &str) -> io::Result<Self> {
        if !is_bare_name(temp_name) {
            return Err(invalid("租约只能指向同目录的单个文件名"));
        }
        for _ in 0..3 {
            let id = fresh_lease_id();
            let path = parent.join(lease_file_name(&id));
            let mut file = match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            };
            let result = (|| {
                file.write_all(lease_line(temp_name, &id).as_bytes())?;
                file.sync_all()?;
                #[cfg(test)]
                tests_hooks::before_lock(&path);
                acquire(&file, LEASE_LOCK_WAIT)?;
                match fs::symlink_metadata(&path) {
                    Ok(meta) if meta.file_type().is_file() => Ok(()),
                    Ok(_) => Err(invalid("租约路径已不是普通文件")),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "租约在加锁前被回收，放弃本次写入",
                    )),
                    Err(e) => Err(e),
                }
            })();
            match result {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                        release_attempted: false,
                    });
                }
                Err(e) => {
                    // 只删自己刚建的那份：先关句柄再删。
                    drop(file);
                    let _ = fs::remove_file(&path);
                    return Err(e);
                }
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "连续三次都撞上同名租约",
        ))
    }

    /// 租约文件路径：接入替换流程后由调用方填“删不掉”的诊断。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 经持有者自己的句柄读内容：Windows 上锁内不能被第二句柄读。
    #[cfg(test)]
    pub(crate) fn contents(&mut self) -> io::Result<String> {
        use std::io::{Read, Seek};
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| invalid("租约句柄已关闭"))?;
        let mut text = String::new();
        file.seek(std::io::SeekFrom::Start(0))?;
        file.read_to_string(&mut text)?;
        Ok(text)
    }

    /// 关句柄、删租约，并把删除失败如实交回调用方。
    ///
    /// 改名已成功时删除失败**不能**回滚，也不能说替换失败：调用方改用诊断
    /// 通道单列一条，残留租约交给后续清理按孤儿处理。
    pub fn release(mut self) -> io::Result<()> {
        self.release_attempted = true;
        drop(self.file.take());
        #[cfg(test)]
        tests_hooks::after_close(&self.path);
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        // Windows 上持锁的句柄会让删除失败，所以先关句柄。
        drop(self.file.take());
        // `release` 已经试过一次：失败原因（句柄被占用、路径被换成目录）通常仍在，
        // 重试只会吞掉第二次失败，而 `Drop` 无处报告。残留交给清理器按孤儿处理。
        if !self.release_attempted {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// 与临时文件同目录、只含单个普通文件名。
///
/// 只排除分隔符不够：Windows 上 `join("C:temp")` 会替换整个基准路径；反斜杠、
/// 冒号与 NUL 在任何平台上都不接受。空白与制表符也必须拒绝：租约行按空格切字段，
/// 名字里带空格会写出一份自家解析器读不懂的租约，变成永久残留。
fn is_bare_name(name: &str) -> bool {
    if name.is_empty() || name.contains(['\n', '\r', '\t', ' ', '\0', ':', '/', '\\']) {
        return false;
    }
    let mut parts = Path::new(name).components();
    matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none()
}

fn lease_file_name(id: &str) -> String {
    format!("{LEASE_PREFIX}{id}")
}

fn lease_line(temp_name: &str, id: &str) -> String {
    format!("{LEASE_TAG} {LEASE_VERSION} temp={temp_name} id={id}\n")
}

/// 32 位小写十六进制，与工程里其它随机 ID 同法。
fn fresh_lease_id() -> String {
    use std::hash::{BuildHasher, RandomState};
    let high = RandomState::new().hash_one(0x6c65_6173_6500_0001_u64);
    let low = RandomState::new().hash_one(0x6c65_6173_6500_0002_u64);
    format!("{high:016x}{low:016x}")
}

/// 非阻塞优先、有上限的取锁；清理者一侧从不调用它。
fn acquire(file: &File, timeout: Duration) -> io::Result<()> {
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => {
                let left = timeout.saturating_sub(started.elapsed());
                if left.is_zero() {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "等待租约锁超时"));
                }
                std::thread::sleep(left.min(LEASE_LOCK_POLL));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// 读取上限：超过即视为不可解析，不把整轮清理变成中止。
const MAX_LEASE_BYTES: u64 = 4096;

fn read_lease(file: &mut File) -> Result<String, Reason> {
    use std::io::Read;
    let mut buffer = Vec::new();
    // 显式限定：模块顶部同时导入了 `Write`，两个 trait 都有 `by_ref`，简写会歧义。
    // 借一层再取，句柄留给调用方继续用（`Claim` 要接着持有它）。
    io::Read::by_ref(file)
        .take(MAX_LEASE_BYTES + 1)
        .read_to_end(&mut buffer)
        .map_err(|_| Reason::LeaseUnparsable)?;
    if buffer.len() as u64 > MAX_LEASE_BYTES {
        return Err(Reason::LeaseUnparsable);
    }
    String::from_utf8(buffer).map_err(|_| Reason::LeaseUnparsable)
}

/// 解析租约内容，返回它认领的临时文件名。`file_id` 是文件名里的 id，必须与内容一致。
fn parse_lease<'a>(text: &'a str, file_id: &str) -> Result<&'a str, Reason> {
    // 单行、以换行结尾、字段顺序固定：多一个少一个都不可解析。
    let Some(line) = text.strip_suffix('\n') else {
        return Err(Reason::LeaseUnparsable);
    };
    if line.contains('\n') {
        return Err(Reason::LeaseUnparsable);
    }
    let fields: Vec<&str> = line.split(' ').collect();
    let [tag, version, temp, id] = fields.as_slice() else {
        return Err(Reason::LeaseUnparsable);
    };
    if *tag != LEASE_TAG {
        return Err(Reason::LeaseUnparsable);
    }
    if *version != LEASE_VERSION {
        return Err(Reason::LeaseUnknownVersion);
    }
    let Some(temp) = temp.strip_prefix("temp=") else {
        return Err(Reason::LeaseUnparsable);
    };
    let Some(id) = id.strip_prefix("id=") else {
        return Err(Reason::LeaseUnparsable);
    };
    if !is_bare_name(temp) || !is_lease_id(id) {
        return Err(Reason::LeaseUnparsable);
    }
    if id != file_id {
        return Err(Reason::LeaseIdMismatch);
    }
    Ok(temp)
}

/// 32 位小写十六进制。
fn is_lease_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 文件名里的租约 id；名字不合格式即不是租约。
fn lease_id_of(name: &str) -> Option<&str> {
    let id = name.strip_prefix(LEASE_PREFIX)?;
    is_lease_id(id).then_some(id)
}

/// 决策一个候选；`apply=false` 只报不删。
///
/// 顺序照设计：受保护判断（不开句柄）→ 类型核验（不跟随链接）→ 逐租约非阻塞试锁
/// → 归属判定 → 持锁删除。
///
/// `leases` 由调用方按目录列一次传进来（设计第 2 步）：列不出来是“这个目录看不懂”，
/// 属于调用方的扫描失败，不是某个候选或某份租约的原因，所以本函数不自己 `read_dir`。
///
/// 由此得到一条不变量：没有租约列举就没有任何删除。凡是要判归属的路径都从 `leases`
/// 出发，列不出来时调用方连候选都不该送进来。
fn decide_candidate(
    dir: &Path,
    name: &str,
    relative: &str,
    leases: &[(PathBuf, String)],
    protected: &BTreeSet<String>,
    apply: bool,
    problems: &mut Vec<Problem>,
) -> Item {
    let path = dir.join(name);
    if protected.contains(relative) {
        return keep(path, Kind::Temp, Reason::RecoveryReferenced);
    }
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return keep(path, Kind::Temp, Reason::TempAlreadyAbsent);
        }
        Err(e) => {
            problems.push(Problem {
                path: path.clone(),
                message: e.to_string(),
            });
            return keep(path, Kind::Temp, Reason::TempNotRegular);
        }
        Ok(meta) if !meta.file_type().is_file() => {
            return keep(path, Kind::Temp, Reason::TempNotRegular);
        }
        Ok(_) => {}
    }

    let mut claimants: Vec<Claim> = Vec::new();
    let mut first_bad: Option<Reason> = None;
    // 列举是调用方的目录级动作；这里只消费结果，失败不再落到某个候选头上。
    for (lease_path, id) in leases {
        let meta = match fs::symlink_metadata(lease_path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                problems.push(Problem {
                    path: lease_path.clone(),
                    message: e.to_string(),
                });
                first_bad.get_or_insert(Reason::LeaseUnparsable);
                continue;
            }
            Ok(meta) => meta,
        };
        if !meta.file_type().is_file() {
            first_bad.get_or_insert(Reason::LeaseNotRegular);
            continue;
        }
        let mut file = match OpenOptions::new().read(true).write(true).open(lease_path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                problems.push(Problem {
                    path: lease_path.clone(),
                    message: e.to_string(),
                });
                first_bad.get_or_insert(Reason::LeaseUnparsable);
                continue;
            }
            Ok(file) => file,
        };
        // 永不等待：任何一个租约被占用，整个目录本轮不动。
        match file.try_lock() {
            Err(std::fs::TryLockError::WouldBlock) => {
                return keep(path, Kind::Temp, Reason::LeaseBusy);
            }
            Err(std::fs::TryLockError::Error(e)) => {
                problems.push(Problem {
                    path: lease_path.clone(),
                    message: e.to_string(),
                });
                first_bad.get_or_insert(Reason::LeaseUnparsable);
                continue;
            }
            Ok(()) => {}
        }
        // 锁身份按文件对象：加锁后再看名字是否还指向一个普通文件。名字被换掉时
        // 手里的锁证明不了任何事，本轮不动（与写入者的第 4 步核验同理）。
        match fs::symlink_metadata(lease_path) {
            Ok(meta) if meta.file_type().is_file() => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            _ => {
                first_bad.get_or_insert(Reason::LeaseNotRegular);
                continue;
            }
        }
        let text = match read_lease(&mut file) {
            Ok(text) => text,
            Err(reason) => {
                first_bad.get_or_insert(reason);
                continue;
            }
        };
        match parse_lease(&text, id) {
            // `parse_lease` 已保证 `id=` 与文件名一致，认出归属就够了。
            Ok(temp) if temp == name => claimants.push(Claim {
                path: lease_path.clone(),
                _lock: file,
            }),
            // 不认领本候选：提前放开。句柄析构本来也会释放锁，这里提前只是为了
            // 让并发清理者不必等本轮这个句柄析构。
            Ok(_) => {
                let _ = file.unlock();
            }
            Err(reason) => {
                first_bad.get_or_insert(reason);
            }
        }
    }

    // 多于一个认领者绝不猜主人，即使同时存在读不懂的租约。
    let outcome = if claimants.len() > 1 {
        Outcome::Keep(Reason::TempAmbiguous)
    } else if let Some(reason) = first_bad {
        Outcome::Keep(reason)
    } else if let Some(claim) = claimants.pop() {
        if apply {
            remove_as_owner(&claim, &path, problems)
        } else {
            Outcome::Remove
        }
    } else {
        Outcome::Keep(Reason::TempWithoutLease)
    };
    Item {
        path,
        kind: Kind::Temp,
        // 必须同时看 `apply`：预览里一个认领者也会得出 `Outcome::Remove`，
        // 光比处置会把“本可删除”报成“已删除”，报告就会撒谎。
        acted: apply && outcome == Outcome::Remove,
        outcome,
    }
}

/// 已锁定的认领租约。句柄就是核验过的那个对象，删除也基于它，不再按路径重新打开。
struct Claim {
    path: PathBuf,
    /// 只靠存在性持有锁：删完临时文件与租约后随本结构一起释放。
    /// 下划线开头是因为从不需要读它（否则 `dead_code` 会报“字段从未读取”）。
    _lock: File,
}

/// 持锁删临时文件，再删租约；句柄一直握到两者都处理完。
///
/// 不再按路径重新打开：释放再取锁就丢掉了“核验—删除”的原子性，两个清理者会
/// 各自核验通过、各删一次（设计“身份复核”与锁顺序）。
fn remove_as_owner(claim: &Claim, path: &Path, problems: &mut Vec<Problem>) -> Outcome {
    #[cfg(test)]
    tests_hooks::before_remove(path);
    match fs::remove_file(path) {
        Ok(()) => {}
        // 已不存在不视为失败；租约仍在，下一轮按孤儿处理。
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Outcome::Keep(Reason::TempAlreadyAbsent);
        }
        Err(e) => {
            problems.push(Problem {
                path: path.to_path_buf(),
                message: e.to_string(),
            });
            return Outcome::Keep(Reason::TempStillPresent);
        }
    }
    // 租约删不掉不影响“临时文件已清掉”的事实，记 I/O 失败供人工跟进。
    // 已经不存在不算失败：另一个清理者先删掉也是成功收尾。
    match fs::remove_file(&claim.path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => problems.push(Problem {
            path: claim.path.clone(),
            message: e.to_string(),
        }),
    }
    Outcome::Remove
}

/// 孤儿租约：内容合法、`temp=` 已不存在才删；`apply=false` 只报。
///
/// 与候选一样，锁住之后只认这个句柄：读到内容、删的也是它自己。
fn decide_lease(
    dir: &Path,
    lease_path: &Path,
    id: &str,
    apply: bool,
    problems: &mut Vec<Problem>,
) -> Item {
    let kind = Kind::Lease;
    let kept = |reason| keep(lease_path.to_path_buf(), kind, reason);
    let meta = match fs::symlink_metadata(lease_path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return kept(Reason::LeaseAlreadyAbsent);
        }
        Err(e) => {
            problems.push(Problem {
                path: lease_path.to_path_buf(),
                message: e.to_string(),
            });
            return kept(Reason::LeaseUnparsable);
        }
        Ok(meta) => meta,
    };
    if !meta.file_type().is_file() {
        return kept(Reason::LeaseNotRegular);
    }
    let mut file = match OpenOptions::new().read(true).write(true).open(lease_path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return kept(Reason::LeaseAlreadyAbsent);
        }
        Err(e) => {
            problems.push(Problem {
                path: lease_path.to_path_buf(),
                message: e.to_string(),
            });
            return kept(Reason::LeaseUnparsable);
        }
        Ok(file) => file,
    };
    match file.try_lock() {
        Err(std::fs::TryLockError::WouldBlock) => {
            return kept(Reason::LeaseBusy);
        }
        Err(std::fs::TryLockError::Error(e)) => {
            problems.push(Problem {
                path: lease_path.to_path_buf(),
                message: e.to_string(),
            });
            return kept(Reason::LeaseUnparsable);
        }
        Ok(()) => {}
    }
    let text = match read_lease(&mut file) {
        Ok(text) => text,
        Err(reason) => return kept(reason),
    };
    let temp_name = match parse_lease(&text, id) {
        Ok(temp) => temp,
        Err(reason) => return kept(reason),
    };
    let temp = dir.join(temp_name);
    match fs::symlink_metadata(&temp) {
        Ok(_) => return kept(Reason::TempStillPresent),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        // 判断不了 `temp=` 是否还在，就按“还在”保留，并记一条 I/O 失败。
        Err(e) => {
            problems.push(Problem {
                path: temp,
                message: e.to_string(),
            });
            return kept(Reason::TempStillPresent);
        }
    }
    if !apply {
        return Item {
            path: lease_path.to_path_buf(),
            kind,
            outcome: Outcome::Remove,
            acted: false,
        };
    }
    match fs::remove_file(lease_path) {
        Ok(()) => Item {
            path: lease_path.to_path_buf(),
            kind,
            outcome: Outcome::Remove,
            acted: true,
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => kept(Reason::LeaseAlreadyAbsent),
        // 删除失败：判定仍是“可清理”，只是这一轮没删成。报 `TempStillPresent` 会撒谎——
        // 那份租约的 `temp=` 已被证明不存在。I/O 失败照记，下次再试。
        Err(e) => {
            problems.push(Problem {
                path: lease_path.to_path_buf(),
                message: e.to_string(),
            });
            Item {
                path: lease_path.to_path_buf(),
                kind,
                outcome: Outcome::Remove,
                acted: false,
            }
        }
    }
}

fn keep(path: PathBuf, kind: Kind, reason: Reason) -> Item {
    Item {
        path,
        kind,
        outcome: Outcome::Keep(reason),
        acted: false,
    }
}

/// 同目录的租约文件名与 id，按名字排序保证报告顺序稳定。
///
/// 不按类型预过滤：目录或链接冒充的租约也要报 `LeaseNotRegular`，不能被当成
/// “没有租约”而误删候选。
fn lease_entries(dir: &Path) -> io::Result<Vec<(PathBuf, String)>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(id) = lease_id_of(name) {
            entries.push((entry.path(), id.to_owned()));
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(entries)
}

/// 两种模式：预览只报“本可删除”，删数为 0。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Preview,
    Apply,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Apply => "apply",
        }
    }
}

/// 某一类对象的计数：可清理数与实际删除数分列，预览才说得清。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Counts {
    pub candidates: usize,
    pub removable: usize,
    pub removed: usize,
    pub kept: usize,
}

/// 一轮扫描的报告：计数与逐项明细都出自同一次判定，不再二次统计。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Report {
    pub mode: Mode,
    /// 扫描范围：调用方给的相对标签，工作区根是空串。报告里的路径也相对同一个根。
    pub root: PathBuf,
    /// 报告里的“范围”：`root` 的标签，空标签按 `.` 呈现。
    scope: String,
    pub recursive: bool,
    pub items: Vec<Item>,
    pub problems: Vec<Problem>,
    /// 读不了的目录：既不算扫描成功，也不中止整轮。
    pub unscanned: Vec<PathBuf>,
    /// 主动不进入的目录名（只报名字，报告仍不含绝对路径）。
    pub skipped: Vec<String>,
}

impl Report {
    fn new(mode: Mode, root: &Path, scope: &str, recursive: bool) -> Self {
        Self {
            mode,
            root: root.to_path_buf(),
            scope: scope.to_owned(),
            recursive,
            items: Vec::new(),
            problems: Vec::new(),
            unscanned: Vec::new(),
            skipped: Vec::new(),
        }
    }

    /// 报告里的路径形式：相对 `root`、`/` 分隔。绝对路径不外泄。
    ///
    /// 不在 `root` 之下时只保留 `Normal` 组件（盘符、根与前导 `..` 都被滤掉），
    /// 所以无论哪种情况都不会输出绝对路径。
    pub fn display_relative(&self, path: &Path) -> String {
        let relative = path.strip_prefix(&self.root).unwrap_or(path);
        relative
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    /// 扫描范围：调用方给的相对标签（`sweep_dir` 的 `relative_dir`），根记作 `.`。
    ///
    /// 返回 `.` 而不是空串：报告是给人看的，`scope: ""` 会被读成“没扫描任何地方”。
    /// 标签与报告里的路径始终对得上：路径一律相对 `root`，标签说的也是 `root`。
    pub fn scope(&self) -> String {
        if self.scope.is_empty() {
            ".".to_owned()
        } else {
            self.scope.clone()
        }
    }

    /// 报告里不出现绝对外部路径：消息里若嵌了扫描根，换成 `.`。
    ///
    /// std 的 `fs` 错误目前不带路径，所以这条今天多半是空操作；但只要有一处错误
    /// 带上路径，整份报告的契约就破了，所以在出口处兜住，而不是指望每个调用点自觉。
    fn scrub(&self, message: &str) -> String {
        let root = self.root.display().to_string();
        if root.is_empty() {
            return message.to_owned();
        }
        message.replace(&root, ".")
    }

    pub fn counts(&self, kind: Kind) -> Counts {
        let mut counts = Counts::default();
        for item in self.items.iter().filter(|item| item.kind == kind) {
            counts.candidates += 1;
            match item.outcome {
                Outcome::Remove => {
                    counts.removable += 1;
                    if item.acted {
                        counts.removed += 1;
                    }
                }
                Outcome::Keep(_) => counts.kept += 1,
            }
        }
        counts
    }

    /// 本次实际删掉的对象数（候选与租约都算）。
    pub fn removed_total(&self) -> usize {
        self.items.iter().filter(|item| item.acted).count()
    }

    /// 按设计的退出码判定：只有扫描／删除的 I/O 失败、或读不了的目录才算“需要处理”。
    ///
    /// 预览里“本可删除但没删”**不是**失败：报告已经逐项标出可清理数，脚本据此决定
    /// 是否加 `--yes`；设计明确“仅保留活跃或未知项不算执行失败”。
    pub fn needs_attention(&self) -> bool {
        !self.problems.is_empty() || !self.unscanned.is_empty()
    }

    /// 报告只列原因名与相对路径，不带内部细节。
    ///
    /// `reason` 恒为设计中的 14 个名字之一（12 个保留 + 2 个删除），从不为 `null`：
    /// 可删除项的删除原因由 `Item::reason_name` 按类型补齐。`problems` 的消息也过
    /// `scrub`，所以整份 JSON 不含绝对外部路径。
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::json;
        let describe = |kind: Kind| {
            let counts = self.counts(kind);
            json!({
                "candidates": counts.candidates,
                "removable": counts.removable,
                "removed": counts.removed,
                "kept": counts.kept,
            })
        };
        json!({
            "mode": self.mode.as_str(),
            "scope": self.scope(),
            "recursive": self.recursive,
            "temps": describe(Kind::Temp),
            "leases": describe(Kind::Lease),
            "removed_total": self.removed_total(),
            "skipped": self.skipped,
            "items": self.items.iter().map(|item| json!({
                "path": self.display_relative(&item.path),
                "kind": match item.kind { Kind::Temp => "temp", Kind::Lease => "lease" },
                "outcome": match item.outcome {
                    Outcome::Remove => "remove",
                    Outcome::Keep(_) => "keep",
                },
                "reason": item.reason_name(),
                "acted": item.acted,
            })).collect::<Vec<_>>(),
            "problems": self.problems.iter().map(|problem| json!({
                "path": self.display_relative(&problem.path),
                "message": self.scrub(&problem.message),
            })).collect::<Vec<_>>(),
            "unscanned": self.unscanned
                .iter()
                .map(|path| self.display_relative(path))
                .collect::<Vec<_>>(),
        })
    }
}

/// 扫一个目录（不递归）：候选项逐项决策，再逐个处理孤儿租约。
///
/// `relative_dir` 是相对工作区根的目录路径（根目录传空串），用来拼受保护集合里的
/// 相对路径，同时作为报告的扫描范围标签；它不是被扫目录内部的位置。
pub fn sweep_dir(
    dir: &Path,
    relative_dir: &str,
    protected: &BTreeSet<String>,
    apply: bool,
) -> Report {
    let mut report = Report::new(
        if apply { Mode::Apply } else { Mode::Preview },
        dir,
        relative_dir,
        false,
    );
    scan_into(&mut report, dir, relative_dir, protected, apply);
    report
}

fn scan_into(
    report: &mut Report,
    dir: &Path,
    relative_dir: &str,
    protected: &BTreeSet<String>,
    apply: bool,
) {
    // 第一次列举：候选名与租约清单出自同一次 `read_dir`，两者一起决定本轮删不删。
    // 读不出来就连候选都不判、直接放弃这个目录：`decide_candidate` 的前提是“租约列全了”，
    // 不列租约就送候选进去，会把每一个都判成 `TempWithoutLease`——看着安全，实则让这个
    // 目录永远清不掉，也永远不知道原因。
    // 单条 entry 出错也走同一条路：不完整的列举证明不了“只有一个认领者”，一个有两个
    // 活跃租约的临时文件会被读成单认领 `Outcome::Remove` 而遭误删，所以必须与“列不出来”
    // 一样按失败关闭处理。
    let mut names: Vec<String> = Vec::new();
    let mut leases: Vec<(PathBuf, String)> = Vec::new();
    match fs::read_dir(dir) {
        Err(e) => {
            report.unscanned.push(dir.to_path_buf());
            report.problems.push(Problem {
                path: dir.to_path_buf(),
                message: e.to_string(),
            });
            return;
        }
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Err(e) => {
                        report.problems.push(Problem {
                            path: dir.to_path_buf(),
                            message: e.to_string(),
                        });
                        report.unscanned.push(dir.to_path_buf());
                        return;
                    }
                    Ok(entry) => {
                        let file_name = entry.file_name();
                        let Some(name) = file_name.to_str() else {
                            continue;
                        };
                        names.push(name.to_owned());
                        if let Some(id) = lease_id_of(name) {
                            leases.push((entry.path(), id.to_owned()));
                        }
                    }
                }
            }
        }
    }
    names.sort();
    // 租约顺序也固定：报告要能逐字比较，不能跟着文件系统的返回次序走。
    leases.sort_by(|left, right| left.0.cmp(&right.0));
    let candidates: Vec<String> = names
        .into_iter()
        .filter(|name| is_candidate_name(name))
        .collect();
    for name in candidates {
        let relative = join_relative(relative_dir, &name);
        let item = decide_candidate(
            dir,
            &name,
            &relative,
            &leases,
            protected,
            apply,
            &mut report.problems,
        );
        report.items.push(item);
    }
    // 租约清单在候选处理之后再取一次，不能复用第一次的清单：随候选一起被删掉的租约
    // 不该再报一条“已不存在”。复用旧清单会让报告凭空多出与候选同样多的假问题项。
    let leases = match lease_entries(dir) {
        Ok(leases) => leases,
        Err(e) => {
            // 候选那一段确实走完了，但孤儿租约那一段没走成：所以这个目录算“没扫全”，
            // 不能只记一条 I/O 失败就算收尾——否则 JSON 里分不出“没有孤儿租约”
            // 与“孤儿那趟根本没跑”，报告会被读成干净。
            report.unscanned.push(dir.to_path_buf());
            report.problems.push(Problem {
                path: dir.to_path_buf(),
                message: e.to_string(),
            });
            return;
        }
    };
    for (path, id) in leases {
        // 不按类型预过滤：目录或链接冒充的租约要交给 decide_lease 报
        // LeaseNotRegular，不能被当成“没有租约”。
        let item = decide_lease(dir, &path, &id, apply, &mut report.problems);
        report.items.push(item);
    }
}

/// 相对路径一律用 `/` 分隔，与恢复记录同一形式；根目录本身是空串。
fn join_relative(relative_dir: &str, name: &str) -> String {
    if relative_dir.is_empty() {
        name.to_owned()
    } else {
        format!("{relative_dir}/{name}")
    }
}

/// 只在测试里存在的交错点，线程局部：测试二进制并行时不互相触发。
///
/// 只放已经用得到的那个：`Vanish` 随 Task 3 的删除路径加入，
/// `SwapNextLeaseToDir` 留到 Task 5，免得在更早的 chunk 里留下没人用的死代码。
#[cfg(test)]
pub(crate) mod tests_hooks {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    enum Armed {
        /// 拿到锁之前把租约路径删掉：模拟孤儿回收抢先动手。
        Reclaim,
        /// 关句柄之后把租约换成目录：让删除必然失败（两个平台都会报错）。
        SwapToDir(PathBuf),
        /// 删除候选之前先把它删掉：覆盖删除时的 `NotFound`。
        Vanish { path: PathBuf },
    }

    thread_local! {
        static ARMED: RefCell<Vec<Armed>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn arm_reclaim() {
        ARMED.with(|slot| slot.borrow_mut().push(Armed::Reclaim));
    }

    /// 记下要抢先删掉的候选路径；真正的删除发生在 `remove_as_owner` 动手之前。
    pub(super) fn arm_vanish(path: &Path) {
        ARMED.with(|slot| {
            slot.borrow_mut().push(Armed::Vanish {
                path: path.to_owned(),
            })
        });
    }

    /// 记下要换名的租约路径；真正换名发生在 `release` 关句柄之后。
    pub(super) fn arm_swap_to_dir(path: &Path) {
        ARMED.with(|slot| slot.borrow_mut().push(Armed::SwapToDir(path.to_owned())));
    }

    pub(super) fn clear() {
        ARMED.with(|slot| slot.borrow_mut().clear());
    }

    fn take(matches: impl Fn(&Armed) -> bool) -> Option<Armed> {
        ARMED.with(|slot| {
            let mut slot = slot.borrow_mut();
            let index = slot.iter().position(matches)?;
            Some(slot.remove(index))
        })
    }

    fn swap(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::create_dir(path);
    }

    pub(super) fn before_lock(path: &Path) {
        if take(|armed| matches!(armed, Armed::Reclaim)).is_some() {
            let _ = std::fs::remove_file(path);
        }
    }

    pub(super) fn before_remove(path: &Path) {
        if take(|armed| matches!(armed, Armed::Vanish { path: target } if target == path)).is_some()
        {
            let _ = std::fs::remove_file(path);
        }
    }

    pub(super) fn after_close(path: &Path) {
        if let Some(Armed::SwapToDir(target)) =
            take(|armed| matches!(armed, Armed::SwapToDir(target) if target == path))
        {
            swap(&target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_names_are_strict() {
        assert!(is_candidate_name(&format!(
            "{TEMP_PREFIX}{}-1-1",
            std::process::id()
        )));
        assert!(is_candidate_name(".ziyuan-tmp-0-0-0"));
        // 前导零合法：协议只要求十进制可解析。
        assert!(is_candidate_name(".ziyuan-tmp-007-0-0"));
        // 各段类型自身的上界必须被接受。
        let maxima = format!("{TEMP_PREFIX}{}-{}-{}", u32::MAX, u128::MAX, u64::MAX);
        assert!(is_candidate_name(&maxima), "{maxima}");
        for bad in [
            // 段数
            ".ziyuan-tmp-1-1",
            ".ziyuan-tmp-1-1-1-1",
            ".ziyuan-tmp-",
            // 空段
            ".ziyuan-tmp--1-1",
            ".ziyuan-tmp-1--1",
            ".ziyuan-tmp-1-1-",
            // 符号与空白：`parse::<u32>()` 会接受 `+1`
            ".ziyuan-tmp-+1-1-1",
            ".ziyuan-tmp-1-+1-1",
            ".ziyuan-tmp- 1-1-1",
            ".ziyuan-tmp-1-1- 1",
            // 非 ASCII 数字
            ".ziyuan-tmp-１-1-1",
            // 额外后缀与空白
            ".ziyuan-tmp-1-1-1.bak",
            ".ziyuan-tmp-1-1-1 ",
            // 前缀相似
            ".ziyuan-tmpx-1-1-1",
            "ziyuan-tmp-1-1-1",
            ".ziyuan-lease-0123456789abcdef0123456789abcdef",
        ] {
            assert!(!is_candidate_name(bad), "{bad:?}");
        }
        // 各段上界再 +1 必须被拒绝。u128 无法在类型内加一，故直接写 `u128::MAX + 1` 的字面量。
        for overflow in [
            format!("{TEMP_PREFIX}{}-1-1", u32::MAX as u64 + 1),
            format!(
                "{TEMP_PREFIX}1-{}-1",
                "340282366920938463463374607431768211456"
            ),
            format!("{TEMP_PREFIX}1-1-{}", u64::MAX as u128 + 1),
        ] {
            assert!(!is_candidate_name(&overflow), "{overflow}");
        }
    }

    #[test]
    fn fresh_names_are_candidates_and_distinct() {
        let first = fresh_temp_name();
        let second = fresh_temp_name();
        assert!(is_candidate_name(&first), "{first}");
        assert!(is_candidate_name(&second), "{second}");
        assert_ne!(first, second);
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ziyuan-temp-cleanup-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn lease_paths(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(super::LEASE_PREFIX))
            })
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn drop_removes_an_unreleased_lease() {
        let dir = scratch("lease");
        let name = fresh_temp_name();
        let path = {
            let mut lease = Lease::hold(&dir, &name).unwrap();
            let paths = lease_paths(&dir);
            assert_eq!(paths.len(), 1, "{paths:?}");
            assert_eq!(paths[0], lease.path());
            // Windows 上锁内不能被第二句柄读，所以经持有者自己的句柄读。
            let text = lease.contents().unwrap();
            assert!(text.ends_with('\n'), "{text:?}");
            assert!(text.starts_with("ziyuan-lease v1 "), "{text:?}");
            assert!(text.contains(&format!("temp={name}")), "{text:?}");
            // 第二句柄取不到锁（Windows 上跨句柄互斥成立）。
            #[cfg(windows)]
            {
                let probe = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&paths[0])
                    .unwrap();
                assert!(
                    matches!(probe.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
                    "写入者必须持有租约锁"
                );
            }
            paths[0].clone()
        };
        assert!(!path.exists(), "释放后租约必须消失");
        assert!(lease_paths(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn release_removes_the_lease_and_reports_success() {
        let dir = scratch("release-ok");
        let name = fresh_temp_name();
        let lease = Lease::hold(&dir, &name).unwrap();
        let path = lease.path().to_path_buf();
        lease.release().unwrap();
        assert!(!path.exists(), "release 成功后租约必须消失");
        assert!(lease_paths(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn release_tolerates_a_lease_already_gone() {
        let dir = scratch("release-gone");
        let lease = Lease::hold(&dir, &fresh_temp_name()).unwrap();
        std::fs::remove_file(lease.path()).unwrap();
        assert!(lease.release().is_ok(), "已被删掉的租约不算失败");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn release_reports_a_lease_it_could_not_delete() {
        let dir = scratch("release-fails");
        let name = fresh_temp_name();
        let lease = Lease::hold(&dir, &name).unwrap();
        let path = lease.path().to_path_buf();
        // 让删除必然失败：只读父目录不跨平台，所以经测试钩子在关句柄之后把租约换成目录。
        tests_hooks::arm_swap_to_dir(&path);
        let result = lease.release();
        tests_hooks::clear();
        assert!(result.is_err(), "租约删不掉时必须能报告出来");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hold_aborts_when_its_name_was_reclaimed_before_the_lock() {
        let dir = scratch("reclaimed");
        let name = fresh_temp_name();
        // 在"写完内容"与"加锁"之间插入一次删除，模拟孤儿租约路径抢先回收。
        tests_hooks::arm_reclaim();
        let result = Lease::hold(&dir, &name);
        tests_hooks::clear();
        let error = result.err().expect("租约被回收后必须放弃本次写入");
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted, "{error}");
        assert!(lease_paths(&dir).is_empty());
        assert!(!dir.join(&name).exists(), "放弃时不得建临时文件");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hold_rejects_names_that_are_not_bare() {
        let dir = scratch("bare");
        // 反斜杠、冒号、NUL、空白在任何平台上都不接受，免得同一份判断在 Unix 上放宽。
        // 空格与制表符也必须拒绝：租约行按空格切字段，名字里带空格会写出自家解析器读不懂的租约。
        for bad in [
            "",
            "..",
            ".",
            "sub/temp",
            "sub\\temp",
            "C:temp",
            "a:b",
            "a b",
            "a\tb",
            "a\nb",
            "a\rb",
            "a\0b",
        ] {
            assert!(Lease::hold(&dir, bad).is_err(), "{bad:?}");
        }
        assert!(lease_paths(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_second_creator_never_truncates_an_existing_lease() {
        let dir = scratch("conflict");
        let name = fresh_temp_name();
        let mut first = Lease::hold(&dir, &name).unwrap();
        let path = first.path().to_path_buf();
        let before = first.contents().unwrap();
        // `create_new` 是冲突原语：同名第二次创建必须失败，且不改原内容。
        assert_eq!(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(first.contents().unwrap(), before);
        drop(first);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn empty_protected() -> std::collections::BTreeSet<String> {
        std::collections::BTreeSet::new()
    }

    fn hex_id(seed: u64) -> String {
        format!("{seed:032x}")
    }

    /// 已死写入者留下的合法租约：内容对、没人持锁。
    fn dead_lease(dir: &std::path::Path, id: &str, temp: &str) {
        std::fs::write(
            dir.join(format!(".ziyuan-lease-{id}")),
            lease_line(temp, id),
        )
        .unwrap();
    }

    fn write_temp(dir: &std::path::Path, name: &str) {
        std::fs::write(dir.join(name), b"partial").unwrap();
    }

    /// 设计第 2 步的“列出租约”，测试里由调用方代劳。
    fn leases(dir: &std::path::Path) -> Vec<(std::path::PathBuf, String)> {
        lease_entries(dir).unwrap()
    }

    fn keep_reason(item: &Item) -> Reason {
        match item.outcome {
            Outcome::Keep(reason) => reason,
            Outcome::Remove => panic!("期望保留，实际可删除: {:?}", item.path),
        }
    }

    #[cfg(unix)]
    fn tempfile_target(dir: &std::path::Path) -> std::path::PathBuf {
        let outside = dir.with_extension("outside");
        std::fs::write(&outside, b"keep me").unwrap();
        outside
    }

    #[test]
    fn parse_accepts_the_protocol_line_and_nothing_else() {
        let id = hex_id(7);
        let name = fresh_temp_name();
        // 返回值借用入参，所以先绑到变量上，不能借临时 `String`。
        let line = lease_line(&name, &id);
        let parsed = parse_lease(&line, &id).unwrap();
        assert_eq!(parsed, name);

        let cases = [
            // 版本不是 v1：单独一类原因，不是笼统的不可解析。
            (
                format!("ziyuan-lease v2 temp={name} id={id}\n"),
                Reason::LeaseUnknownVersion,
            ),
            // 顺序错、字段多一个、少一个、重复、缺少行尾换行。
            (
                format!("ziyuan-lease v1 id={id} temp={name}\n"),
                Reason::LeaseUnparsable,
            ),
            (
                format!("ziyuan-lease v1 temp={name} id={id} extra=1\n"),
                Reason::LeaseUnparsable,
            ),
            (
                format!("ziyuan-lease v1 temp={name}\n"),
                Reason::LeaseUnparsable,
            ),
            (
                format!("ziyuan-lease v1 temp={name} id={id} id={id}\n"),
                Reason::LeaseUnparsable,
            ),
            (
                format!("ziyuan-lease v1 temp={name} id={id}"),
                Reason::LeaseUnparsable,
            ),
            // temp 必须是同目录裸文件名。
            (
                format!("ziyuan-lease v1 temp=sub/{name} id={id}\n"),
                Reason::LeaseUnparsable,
            ),
            // id 必须 32 位小写十六进制，且与文件名一致。
            (
                format!("ziyuan-lease v1 temp={name} id={id}\n"),
                Reason::LeaseIdMismatch,
            ),
            (
                format!("ziyuan-lease v1 temp={name} id=ABCDEFABCDEFABCDEFABCDEFABCDEFAB\n"),
                Reason::LeaseUnparsable,
            ),
            // 标签错。
            (
                format!("ziyuan-loan v1 temp={name} id={id}\n"),
                Reason::LeaseUnparsable,
            ),
        ];
        for (text, expected) in cases {
            // `LeaseIdMismatch` 那一行传的实参故意与内容里的 `id=` 不同，验证比对的是文件名 id。
            let file_id = if expected == Reason::LeaseIdMismatch {
                hex_id(9)
            } else {
                id.clone()
            };
            assert_eq!(
                parse_lease(&text, &file_id).unwrap_err(),
                expected,
                "{text:?}"
            );
        }
    }

    #[test]
    fn read_lease_rejects_oversized_and_non_utf8() {
        let dir = scratch("read");
        let path = dir.join(".ziyuan-lease-probe");
        std::fs::write(&path, vec![b'a'; 4097]).unwrap();
        let mut file = std::fs::File::open(&path).unwrap();
        assert_eq!(read_lease(&mut file).unwrap_err(), Reason::LeaseUnparsable);
        // 恰好上限的内容不因超限被拒：读取成功，随后由解析判为不可解析。
        std::fs::write(&path, vec![b'a'; 4096]).unwrap();
        let mut file = std::fs::File::open(&path).unwrap();
        let text = read_lease(&mut file).unwrap();
        assert_eq!(text.len(), 4096);
        assert_eq!(parse_lease(&text, &hex_id(1)), Err(Reason::LeaseUnparsable));
        // 非 UTF-8。
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        let mut file = std::fs::File::open(&path).unwrap();
        assert_eq!(read_lease(&mut file).unwrap_err(), Reason::LeaseUnparsable);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn absent_orphan_lease_is_reported() {
        let dir = scratch("lease-absent");
        let id = hex_id(28);
        let mut problems = Vec::new();
        let item = decide_lease(
            &dir,
            &dir.join(format!(".ziyuan-lease-{id}")),
            &id,
            true,
            &mut problems,
        );
        assert_eq!(keep_reason(&item), Reason::LeaseAlreadyAbsent);
        assert!(problems.is_empty(), "{problems:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn protected_relative_path_wins_before_any_io() {
        let dir = scratch("protected");
        let name = fresh_temp_name();
        write_temp(&dir, &name);
        // 受保护集合命中时，即使目录里没有任何租约也报 RecoveryReferenced 而非
        // TempWithoutLease —— 说明判断发生在打开任何句柄之前。
        let mut protected = empty_protected();
        protected.insert(name.clone());
        let mut problems = Vec::new();
        let item = decide_candidate(
            &dir,
            &name,
            &name,
            &leases(&dir),
            &protected,
            true,
            &mut problems,
        );
        assert_eq!(keep_reason(&item), Reason::RecoveryReferenced);
        assert!(dir.join(&name).exists(), "受保护项绝不能被删");
        assert!(problems.is_empty());

        // 只比文件名会把嵌套候选当成普通候选：这里断言正确形式 `sub/<name>` 生效。
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        write_temp(&sub, &name);
        let relative = format!("sub/{name}");
        let mut protected = empty_protected();
        protected.insert(relative.clone());
        let item = decide_candidate(
            &sub,
            &name,
            &relative,
            &leases(&sub),
            &protected,
            true,
            &mut problems,
        );
        assert_eq!(keep_reason(&item), Reason::RecoveryReferenced);
        // 反例：集合里只有裸名时不该命中。
        let mut protected = empty_protected();
        protected.insert(name.clone());
        let item = decide_candidate(
            &sub,
            &name,
            &relative,
            &leases(&sub),
            &protected,
            true,
            &mut problems,
        );
        assert_eq!(keep_reason(&item), Reason::TempWithoutLease);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn candidate_type_is_checked_before_leases() {
        let dir = scratch("type");
        let name = fresh_temp_name();
        // 目录形态的候选：不是普通文件，且此时已有一份合法认领租约。
        std::fs::create_dir(dir.join(&name)).unwrap();
        let id = hex_id(11);
        dead_lease(&dir, &id, &name);
        let mut problems = Vec::new();
        let item = decide_candidate(
            &dir,
            &name,
            &name,
            &leases(&dir),
            &empty_protected(),
            true,
            &mut problems,
        );
        assert_eq!(keep_reason(&item), Reason::TempNotRegular);
        assert!(dir.join(&name).is_dir(), "非普通文件绝不能被删");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_candidate_is_never_followed() {
        let dir = scratch("symlink");
        let name = fresh_temp_name();
        let outside = tempfile_target(&dir);
        std::os::unix::fs::symlink(&outside, dir.join(&name)).unwrap();
        let mut problems = Vec::new();
        let item = decide_candidate(
            &dir,
            &name,
            &name,
            &leases(&dir),
            &empty_protected(),
            true,
            &mut problems,
        );
        assert_eq!(keep_reason(&item), Reason::TempNotRegular);
        assert!(outside.exists(), "链接目标不得被删");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn one_claimant_is_removed_with_its_lease() {
        let dir = scratch("claimant");
        let name = fresh_temp_name();
        write_temp(&dir, &name);
        let id = hex_id(13);
        dead_lease(&dir, &id, &name);
        let mut problems = Vec::new();
        let item = decide_candidate(
            &dir,
            &name,
            &name,
            &leases(&dir),
            &empty_protected(),
            true,
            &mut problems,
        );
        assert_eq!(item.outcome, Outcome::Remove);
        assert!(item.acted, "真删掉的项必须标记 acted");
        assert!(problems.is_empty(), "{problems:?}");
        assert!(!dir.join(&name).exists());
        assert!(lease_paths(&dir).is_empty(), "删除后不得留下租约");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn preview_reports_removable_without_deleting() {
        let dir = scratch("preview");
        let name = fresh_temp_name();
        write_temp(&dir, &name);
        let id = hex_id(15);
        dead_lease(&dir, &id, &name);
        let mut problems = Vec::new();
        let item = decide_candidate(
            &dir,
            &name,
            &name,
            &leases(&dir),
            &empty_protected(),
            false,
            &mut problems,
        );
        assert_eq!(item.outcome, Outcome::Remove);
        assert!(!item.acted, "预览不得删除任何东西");
        assert!(dir.join(&name).is_file());
        assert_eq!(lease_paths(&dir).len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn two_claimants_are_ambiguous() {
        let dir = scratch("ambiguous");
        let name = fresh_temp_name();
        write_temp(&dir, &name);
        dead_lease(&dir, &hex_id(17), &name);
        dead_lease(&dir, &hex_id(18), &name);
        let mut problems = Vec::new();
        let item = decide_candidate(
            &dir,
            &name,
            &name,
            &leases(&dir),
            &empty_protected(),
            true,
            &mut problems,
        );
        assert_eq!(keep_reason(&item), Reason::TempAmbiguous);
        assert!(dir.join(&name).is_file(), "歧义时绝不猜主人");
        assert_eq!(lease_paths(&dir).len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bad_lease_blocks_deletion_of_a_claimed_candidate() {
        let dir = scratch("bad-lease");
        let name = fresh_temp_name();
        write_temp(&dir, &name);
        dead_lease(&dir, &hex_id(19), &name);
        // 同一目录里另有一份读不懂的租约：本轮不删，报它的原因。
        std::fs::write(
            dir.join(format!(".ziyuan-lease-{}", hex_id(20))),
            b"garbage\n",
        )
        .unwrap();
        let mut problems = Vec::new();
        let item = decide_candidate(
            &dir,
            &name,
            &name,
            &leases(&dir),
            &empty_protected(),
            true,
            &mut problems,
        );
        assert_eq!(keep_reason(&item), Reason::LeaseUnparsable);
        assert!(dir.join(&name).is_file());
        assert_eq!(lease_paths(&dir).len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn two_claimants_with_a_bad_lease_report_ambiguity() {
        let dir = scratch("ambiguous-bad");
        let name = fresh_temp_name();
        write_temp(&dir, &name);
        dead_lease(&dir, &hex_id(21), &name);
        dead_lease(&dir, &hex_id(22), &name);
        std::fs::write(
            dir.join(format!(".ziyuan-lease-{}", hex_id(23))),
            b"ziyuan-lease v3\n",
        )
        .unwrap();
        let mut problems = Vec::new();
        let item = decide_candidate(
            &dir,
            &name,
            &name,
            &leases(&dir),
            &empty_protected(),
            true,
            &mut problems,
        );
        assert_eq!(keep_reason(&item), Reason::TempAmbiguous);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_busy_lease_keeps_the_candidate() {
        let dir = scratch("busy");
        let name = fresh_temp_name();
        write_temp(&dir, &name);
        let holder = Lease::hold(&dir, &name).unwrap();
        let mut problems = Vec::new();
        let item = decide_candidate(
            &dir,
            &name,
            &name,
            &leases(&dir),
            &empty_protected(),
            true,
            &mut problems,
        );
        // 两个平台都会跨句柄报出冲突：Windows 靠共享/锁机制，Unix 上 `File::try_lock`
        // 是 `flock(2)`、锁属于打开文件描述，同进程两次独立 `open(2)` 也互斥，第二次拿
        // `EWOULDBLOCK`。Windows 做不到的是阻止不配合的进程重命名或删除已加锁的文件，
        // 那是设计的“已知限制”，与这条断言无关。
        assert_eq!(keep_reason(&item), Reason::LeaseBusy);
        drop(holder);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lease_that_is_not_a_file_is_never_followed() {
        let dir = scratch("lease-dir");
        let name = fresh_temp_name();
        write_temp(&dir, &name);
        let id = hex_id(24);
        // 目录冒充租约：必须报 LeaseNotRegular 而不是当作“没有租约”去删候选。
        std::fs::create_dir(dir.join(format!(".ziyuan-lease-{id}"))).unwrap();
        let mut problems = Vec::new();
        let item = decide_candidate(
            &dir,
            &name,
            &name,
            &leases(&dir),
            &empty_protected(),
            true,
            &mut problems,
        );
        assert_eq!(keep_reason(&item), Reason::LeaseNotRegular);
        assert!(dir.join(&name).is_file(), "读不懂的归属证据必须保护候选");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn orphan_lease_is_reclaimed_only_when_applying() {
        let dir = scratch("orphan");
        let gone = fresh_temp_name();
        let id = hex_id(25);
        dead_lease(&dir, &id, &gone);
        let mut problems = Vec::new();
        let preview = decide_lease(
            &dir,
            &dir.join(format!(".ziyuan-lease-{id}")),
            &id,
            false,
            &mut problems,
        );
        assert_eq!(preview.outcome, Outcome::Remove);
        assert!(!preview.acted);
        assert_eq!(lease_paths(&dir).len(), 1, "预览不得删孤儿租约");
        let applied = decide_lease(
            &dir,
            &dir.join(format!(".ziyuan-lease-{id}")),
            &id,
            true,
            &mut problems,
        );
        assert_eq!(applied.outcome, Outcome::Remove);
        assert!(applied.acted);
        assert!(lease_paths(&dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn orphan_lease_with_a_live_temp_is_kept() {
        let dir = scratch("orphan-live");
        let name = fresh_temp_name();
        write_temp(&dir, &name);
        let id = hex_id(26);
        dead_lease(&dir, &id, &name);
        let mut problems = Vec::new();
        let item = decide_lease(
            &dir,
            &dir.join(format!(".ziyuan-lease-{id}")),
            &id,
            true,
            &mut problems,
        );
        assert_eq!(keep_reason(&item), Reason::TempStillPresent);
        assert_eq!(lease_paths(&dir).len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn candidate_vanishing_before_delete_is_not_an_error() {
        let dir = scratch("vanish");
        let name = fresh_temp_name();
        write_temp(&dir, &name);
        let id = hex_id(27);
        dead_lease(&dir, &id, &name);
        tests_hooks::arm_vanish(&dir.join(&name));
        let mut problems = Vec::new();
        let item = decide_candidate(
            &dir,
            &name,
            &name,
            &leases(&dir),
            &empty_protected(),
            true,
            &mut problems,
        );
        tests_hooks::clear();
        assert_eq!(keep_reason(&item), Reason::TempAlreadyAbsent);
        assert!(problems.is_empty(), "已不存在不是失败");
        assert_eq!(
            lease_paths(&dir).len(),
            1,
            "候选没了就不动租约，留给孤儿路径"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn temp_item<'a>(report: &'a Report, name: &str) -> &'a Item {
        report
            .items
            .iter()
            .find(|item| {
                item.kind == Kind::Temp
                    && item
                        .path
                        .file_name()
                        .is_some_and(|file| file.to_string_lossy() == name)
            })
            .unwrap_or_else(|| panic!("没有 {name} 的报告项: {report:?}"))
    }

    fn sweep_fixture(tag: &str) -> (std::path::PathBuf, String, String) {
        let dir = scratch(tag);
        let name = fresh_temp_name();
        write_temp(&dir, &name);
        let id = hex_id(31);
        dead_lease(&dir, &id, &name);
        (dir, name, id)
    }

    #[test]
    fn sweep_reports_candidates_and_leases_separately() {
        let (dir, name, _id) = sweep_fixture("sweep-counts");
        // 一个普通文件冒充候选形状但不符合严格命名。
        std::fs::write(dir.join(".ziyuan-tmp-not-a-candidate"), b"x").unwrap();
        let report = sweep_dir(&dir, "", &empty_protected(), true);
        let temps = report.counts(Kind::Temp);
        let leases = report.counts(Kind::Lease);
        assert_eq!(temps.candidates, 1, "{report:?}");
        assert_eq!(temps.removable, 1, "{report:?}");
        assert_eq!(temps.removed, 1, "{report:?}");
        assert_eq!(temps.kept, 0, "{report:?}");
        assert_eq!(leases.candidates, 0, "{report:?}");
        // 候选先被删掉，租约随之消失，孤儿路径看不到它。
        assert_eq!(report.removed_total(), 1);
        assert!(report.problems.is_empty(), "{report:?}");
        assert!(!dir.join(&name).exists());
        assert!(lease_paths(&dir).is_empty());
        assert_eq!(report.mode, Mode::Apply);
        assert_eq!(report.root, dir.clone());
        assert_eq!(report.scope(), ".");
        // 删掉的候选在报告里必须带上两个删除原因之一，不能留空。
        let removed = temp_item(&report, &name);
        assert_eq!(removed.outcome, Outcome::Remove, "{report:?}");
        assert_eq!(removed.reason_name(), "WriterDeadLeaseVerified");
        let json = report.to_json();
        let items = json["items"].as_array().unwrap();
        assert!(!items.is_empty(), "{json}");
        assert!(
            items.iter().all(|item| !item["reason"].is_null()),
            "报告里的原因不得为空: {json}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sweep_preview_deletes_nothing_and_reports_removable() {
        let (dir, name, _id) = sweep_fixture("sweep-preview");
        let gone = fresh_temp_name();
        let orphan = hex_id(32);
        dead_lease(&dir, &orphan, &gone);
        let before = lease_paths(&dir).len();
        let report = sweep_dir(&dir, "", &empty_protected(), false);
        assert_eq!(report.mode, Mode::Preview);
        assert_eq!(report.counts(Kind::Temp).removable, 1, "{report:?}");
        assert_eq!(report.counts(Kind::Temp).removed, 0, "{report:?}");
        assert_eq!(report.removed_total(), 0);
        assert!(dir.join(&name).is_file(), "预览不得删候选");
        assert_eq!(lease_paths(&dir).len(), before, "预览不得删孤儿租约");
        // 孤儿租约必须真的被报成可清理项：上面只证明“没删”，一个从不报告孤儿
        // 租约的回归也能通过。库里两份租约：一份认领仍活着的候选（保留），一份孤儿。
        let leases = report.counts(Kind::Lease);
        assert_eq!(leases.candidates, 2, "{report:?}");
        assert_eq!(leases.removable, 1, "{report:?}");
        assert_eq!(leases.removed, 0, "{report:?}");
        assert_eq!(leases.kept, 1, "{report:?}");
        let orphan_item = report
            .items
            .iter()
            .find(|item| item.kind == Kind::Lease && item.outcome == Outcome::Remove)
            .unwrap_or_else(|| panic!("没有可清理的租约报告项: {report:?}"));
        assert_eq!(orphan_item.reason_name(), "OrphanLease");
        assert!(!orphan_item.acted, "预览不得删孤儿租约: {report:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sweep_keeps_a_protected_candidate() {
        let (dir, name, _id) = sweep_fixture("sweep-protected");
        let mut protected = empty_protected();
        protected.insert(name.clone());
        let report = sweep_dir(&dir, "", &protected, true);
        assert_eq!(
            keep_reason(temp_item(&report, &name)),
            Reason::RecoveryReferenced
        );
        assert!(dir.join(&name).is_file());
        assert_eq!(report.counts(Kind::Temp).removed, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sweep_keeps_a_nested_protected_candidate() {
        let root = scratch("sweep-nested-protected");
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let name = fresh_temp_name();
        write_temp(&sub, &name);
        // 受保护集合存的是相对工作区根的路径，所以只有 `join_relative` 把
        // `sub` 拼在前面，这条才能命中。
        let mut protected = empty_protected();
        protected.insert(format!("sub/{name}"));
        let report = sweep_dir(&sub, "sub", &protected, true);
        assert_eq!(report.scope(), "sub", "{report:?}");
        assert_eq!(
            keep_reason(temp_item(&report, &name)),
            Reason::RecoveryReferenced
        );
        assert!(sub.join(&name).is_file(), "受保护的嵌套候选不得被删");
        assert_eq!(report.counts(Kind::Temp).removed, 0, "{report:?}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn needs_attention_distinguishes_preview_from_failure() {
        // 预览里有可清理项不是失败：报告已逐项标出可清理数，由脚本决定是否加 `--yes`。
        let (dir, _name, _id) = sweep_fixture("sweep-attention-preview");
        let preview = sweep_dir(&dir, "", &empty_protected(), false);
        assert_eq!(preview.counts(Kind::Temp).removable, 1, "{preview:?}");
        assert!(!preview.needs_attention(), "{preview:?}");
        std::fs::remove_dir_all(dir).unwrap();

        // 目录没扫全就是失败，哪怕一个项都没判出来。
        let root = scratch("sweep-attention-unscanned");
        let not_a_dir = root.join("regular.txt");
        std::fs::write(&not_a_dir, b"x").unwrap();
        let unscanned = sweep_dir(&not_a_dir, "", &empty_protected(), true);
        assert!(!unscanned.unscanned.is_empty(), "{unscanned:?}");
        assert!(unscanned.needs_attention(), "{unscanned:?}");
        std::fs::remove_dir_all(root).unwrap();

        // 空目录扫得干干净净，也不是失败。
        let empty = scratch("sweep-attention-empty");
        let clean = sweep_dir(&empty, "", &empty_protected(), true);
        assert!(clean.items.is_empty(), "{clean:?}");
        assert!(!clean.needs_attention(), "{clean:?}");
        std::fs::remove_dir_all(empty).unwrap();
    }

    #[test]
    fn report_never_serializes_an_absolute_path() {
        let dir = scratch("sweep-absolute");
        let mut report = Report::new(Mode::Preview, &dir, "", true);
        // std 的 `fs` 错误今天不带路径，所以这里直接造一条带绝对路径的消息：
        // 契约要在出口处成立，而不是指望恰好没有错误带路径。
        report.problems.push(Problem {
            path: dir.join("leak.txt"),
            message: format!("删除失败：{}", dir.display()),
        });
        let json = report.to_json().to_string();
        let root = dir.display().to_string();
        // 两种形态都要断言：JSON 文本里反斜杠是转义的，只查单反斜杠形态在 Windows
        // 上永远为真（等于没查）。
        assert!(!json.contains(&root), "绝对根漏进报告: {json}");
        let escaped_root = root.replace('\\', r"\\");
        assert!(!json.contains(&escaped_root), "绝对根漏进报告: {json}");
        // JSON 文本里的 `\\` 是一个反斜杠：路径一律 `/` 分隔，连转义形态都不该有。
        assert!(!json.contains(r"\\"), "报告里不该有反斜杠路径: {json}");
        assert!(!json.contains("//"), "报告里不该有 `//` 前缀: {json}");
        // 盘符形态（`X:`）也不该出现。
        assert!(
            !json
                .as_bytes()
                .windows(2)
                .any(|window| window[0].is_ascii_alphabetic() && window[1] == b':'),
            "报告里出现了盘符形态: {json}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sweep_records_a_directory_it_cannot_list() {
        let dir = scratch("sweep-unscanned");
        let not_a_dir = dir.join("regular.txt");
        std::fs::write(&not_a_dir, b"x").unwrap();
        let report = sweep_dir(&not_a_dir, "", &empty_protected(), true);
        assert_eq!(report.unscanned, vec![not_a_dir.clone()], "{report:?}");
        assert!(report.items.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sweep_never_reports_an_empty_directory_as_failure() {
        let dir = scratch("sweep-empty");
        let report = sweep_dir(&dir, "", &empty_protected(), true);
        assert!(report.items.is_empty());
        assert!(report.problems.is_empty());
        assert!(report.unscanned.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
