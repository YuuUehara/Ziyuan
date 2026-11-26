//! 原子替换的临时文件与租约：命名、归属核验、清理。

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

mod sweep;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
#[cfg(test)]
use sweep::walk;
pub use sweep::{Counts, MAX_DEPTH, Mode, Report, sweep_workspace};
pub(crate) use sweep::{normalize_relative, sweep_dir};

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
pub(crate) fn is_candidate_name(name: &str) -> bool {
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

thread_local! {
    /// 不便沿返回值上抛的失败：例如改名已成功却删不掉租约。按线程隔离，
    /// 由制造失败的调用方在同线程取走并报到 stderr。
    static PROBLEMS: RefCell<Vec<Problem>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn note_problem(path: PathBuf, message: String) {
    PROBLEMS.with(|problems| problems.borrow_mut().push(Problem { path, message }));
}

/// 取走当前线程累积的失败；空表示没有。
///
/// 只在 crate 内可用：诊断归属于制造它的那次调用，跨 crate 暴露会让别的
/// 调用方取走不是自己产生的诊断。
pub(crate) fn drain_problems() -> Vec<Problem> {
    PROBLEMS.with(|problems| std::mem::take(&mut *problems.borrow_mut()))
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
                tests_hooks::note_claim(temp_name);
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
pub(super) fn lease_id_of(name: &str) -> Option<&str> {
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
///
/// 一处有意的取舍：租约句柄拿不到（`PermissionDenied`，典型是并发清理者正在删它）时
/// 报 `LeaseBusy`，不报 I/O 失败。一份**永久**读不了的租约因此也呈现为 `LeaseBusy`——
/// 两种呈现都会进报告、都会保留候选，区别只在退出码：宁可把“读不了”说成“本轮占用”，
/// 也不让一份删不掉的租约把每一次干净的并发运行都变成失败。
pub(super) fn decide_candidate(
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
        // 删除中的候选报的是 `PermissionDenied` 而不是 `NotFound`（Windows 口径，
        // 句柄一关才彻底消失）。这一轮它等于已经不在：按“已不存在”收尾，不记失败——
        // 记失败会让一次干净的并发清理随机退 1（并发用例实测约每几十次出现一次）。
        // 代价：一份**永久**读不了元数据的候选也会落在这里，报成“已不存在”。
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
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
            // 删除中的租约同上：这一轮它已经不在清单的意义上了，跳过而不是记失败。
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => continue,
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
            // 拿不到句柄不等于“这份租约读不懂”：Windows 上删除中的文件会报
            // `PermissionDenied`，而锁支持缺失一类的错误也会落在这里。按“本轮不可用”
            // 处理——与“租约被占用”同样的后果，绝不当成 I/O 失败去拉高退出码。
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                return keep(path, Kind::Temp, Reason::LeaseBusy);
            }
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
    let injected = tests_hooks::before_remove(path);
    #[cfg(not(test))]
    let injected = false;
    // 注入的失败与真实的删除失败走完全同一条收尾：记一条诊断、保留候选、
    // 绝不上抛——自动清理失败不许阻断随后的写入。
    let removal = if injected {
        Err(io::Error::other("injected remove failure"))
    } else {
        fs::remove_file(path)
    };
    match removal {
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
///
/// 与 `decide_candidate` 同一处取舍：句柄拿不到（`PermissionDenied`）报 `LeaseBusy`，
/// 不报 I/O 失败。一份**永久**读不了的租约因此也呈现为 `LeaseBusy`（两种呈现都在报告里、
/// 都不删），换来的是并发清理的正常收尾不会把退出码拉成 1。
pub(super) fn decide_lease(
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
        // 与 `decide_candidate` 同一个删除中的窗口：别人正把这份租约删掉，这一轮它
        // 等于已经不在了。记成 I/O 失败会让干净的并发清理随机退 1。
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
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
        // 与 `decide_candidate` 同一条规则：删除中的租约在 Windows 上报
        // `PermissionDenied`，句柄拿不到只说明本轮用不上它，不说明它读不懂。
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            return kept(Reason::LeaseBusy);
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
pub(super) fn lease_entries(dir: &Path) -> io::Result<Vec<(PathBuf, String)>> {
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

/// 只在测试里存在的交错点，线程局部：测试二进制并行时不互相触发。
#[cfg(test)]
pub(crate) mod tests_hooks {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    enum Armed {
        /// 拿到锁之前把租约路径删掉：模拟孤儿回收抢先动手。
        Reclaim,
        /// 关句柄之后把租约换成目录：让删除必然失败（两个平台都会报错）。
        SwapToDir(PathBuf),
        /// 下一次 `release` 关句柄后换目录，不预先知道路径。
        SwapNextLeaseToDir,
        /// 删除候选之前先把它删掉：覆盖删除时的 `NotFound`。
        Vanish { path: PathBuf },
        /// 删除候选时注入失败：覆盖「删不掉只记诊断、不阻断后续写入」。
        FailRemove { path: PathBuf },
    }

    thread_local! {
        static ARMED: RefCell<Vec<Armed>> = const { RefCell::new(Vec::new()) };
        static CLAIMS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    /// 记下这份租约认领的临时文件名：写入者持锁期间，第二个句柄读不到内容，
    /// 用例只能从写入侧拿到它。
    pub(crate) fn note_claim(name: &str) {
        CLAIMS.with(|claims| claims.borrow_mut().push(name.to_owned()));
    }

    /// 取走已记录的认领，供用例与 `before_rename` 窗口比对。
    pub(crate) fn take_claims() -> Vec<String> {
        CLAIMS.with(|claims| std::mem::take(&mut *claims.borrow_mut()))
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

    /// 记下要「删不掉」的候选路径：真实的删除失败只在特定文件系统与权限下出现，
    /// 用例需要一条确定的注入路径来验证「清理失败不阻断写入」。
    pub(crate) fn arm_fail_remove(path: &Path) -> ArmedGuard {
        ARMED.with(|slot| {
            slot.borrow_mut().push(Armed::FailRemove {
                path: path.to_owned(),
            })
        });
        ArmedGuard(())
    }

    /// 布防句柄：析构时清掉本线程所有布防，用例 panic 也不会把状态漏给下一个用例。
    #[must_use]
    pub(crate) struct ArmedGuard(());

    impl Drop for ArmedGuard {
        fn drop(&mut self) {
            clear();
        }
    }

    /// 给 `replace.rs` 的用例用：租约路径由 `hold` 生成，事先不知道。
    pub(crate) fn arm_swap_next_lease_to_dir() -> ArmedGuard {
        ARMED.with(|slot| slot.borrow_mut().push(Armed::SwapNextLeaseToDir));
        ArmedGuard(())
    }

    pub(crate) fn clear() {
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

    /// 删候选之前调用；返回 `true` 表示本次删除要按失败收尾。
    pub(super) fn before_remove(path: &Path) -> bool {
        if take(|armed| matches!(armed, Armed::Vanish { path: target } if target == path)).is_some()
        {
            let _ = std::fs::remove_file(path);
        }
        take(|armed| matches!(armed, Armed::FailRemove { path: target } if target == path))
            .is_some()
    }

    pub(super) fn after_close(path: &Path) {
        // 先按路径匹配：`SwapToDir` 点名了要破坏哪份租约，信息更足；`SwapNextLeaseToDir`
        // 只是不知道名字的用例用的兜底，若先取它会吞掉同一事件上的定向布防。
        if let Some(Armed::SwapToDir(target)) =
            take(|armed| matches!(armed, Armed::SwapToDir(target) if target == path))
        {
            swap(&target);
            return;
        }
        // 没有定向布防时才轮到“下一次关句柄”：它不知道路径，所以不能按路径比较。
        if take(|armed| matches!(armed, Armed::SwapNextLeaseToDir)).is_some() {
            swap(path);
        }
    }
}

#[cfg(test)]
mod tests;
