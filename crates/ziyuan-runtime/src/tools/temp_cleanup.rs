//! 原子替换的临时文件与租约：命名、归属核验、清理。

// Chunk 1-3 还没有生产调用点；Task 4 用 sweep_dir 打通后必须删掉本行。
#![allow(dead_code)]

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
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

/// 只在测试里存在的交错点，线程局部：测试二进制并行时不互相触发。
///
/// 只放本任务用到的两个：`Vanish` 在 Task 3、`SwapNextLeaseToDir` 在 Task 5
/// 加入，避免在更早的 chunk 里留下没人用的死代码。
#[cfg(test)]
pub(crate) mod tests_hooks {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    enum Armed {
        /// 拿到锁之前把租约路径删掉：模拟孤儿回收抢先动手。
        Reclaim,
        /// 关句柄之后把租约换成目录：让删除必然失败（两个平台都会报错）。
        SwapToDir(PathBuf),
    }

    thread_local! {
        static ARMED: RefCell<Vec<Armed>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn arm_reclaim() {
        ARMED.with(|slot| slot.borrow_mut().push(Armed::Reclaim));
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
}
