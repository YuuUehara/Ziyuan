//! 落盘时对单个文件的整体替换。

use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 改名遇到短暂占用时的重试时限与单次等待上限。
const RENAME_RETRY_BUDGET: Duration = Duration::from_millis(250);
const RENAME_RETRY_STEP: Duration = Duration::from_millis(10);

/// 删除临时文件失败时的错误：保留原本的失败原因，并给出残留临时文件的路径。
#[derive(Debug)]
pub(crate) struct CleanupFailed {
    pub(crate) primary: io::Error,
    pub(crate) cleanup: io::Error,
    pub(crate) temp: PathBuf,
}

impl std::fmt::Display for CleanupFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}；且未能删除临时文件 {}：{}",
            self.primary,
            self.temp.display(),
            self.cleanup
        )
    }
}

impl std::error::Error for CleanupFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.primary)
    }
}

#[derive(Clone, Copy, Debug)]
enum Point {
    Permissions,
    Sync,
    BeforeRename,
}

/// 用 `bytes` 替换 `target` 的全部内容：先写同目录的临时文件并 `sync_all`，再改名覆盖目标。
/// 任一步失败时目标保持原样（原文或不存在），只删除自己建的临时文件。
/// 已有目标须可写（只读一律拒绝，不清除属性），其普通权限位复制到新文件；
/// 属主、ACL、扩展属性与硬链接身份不保留。只保证单个文件的替换，不防外部同时写入。
pub(super) fn replace_target(target: &Path, bytes: &[u8]) -> io::Result<()> {
    #[cfg(test)]
    faults::begin();
    let permissions = existing_permissions(target)?;
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "目标路径缺少所在目录"))?;
    let temp_name = super::temp_cleanup::fresh_temp_name();
    // 租约先于临时文件存在，覆盖创建、写入、关句柄与改名整段：它同时也是清理者
    // 唯一的归属证据。加锁后 `hold` 会再核验路径仍指向自己。
    let lease = super::temp_cleanup::Lease::hold(parent, &temp_name)?;
    let temp = parent.join(&temp_name);
    let file = match OpenOptions::new().write(true).create_new(true).open(&temp) {
        Ok(file) => file,
        Err(e) => {
            release_recording_failure(lease);
            return Err(e);
        }
    };
    let result = fill(file, &temp, bytes, permissions).and_then(|()| {
        #[cfg(test)]
        faults::before_rename(target);
        inject(Point::BeforeRename)?;
        let started = Instant::now();
        retry_transient(
            || rename(&temp, target),
            || started.elapsed(),
            std::thread::sleep,
        )
    });
    match result {
        Ok(()) => {
            sync_parent(target);
            // 改名已成功：删租约失败不回滚、不报替换失败，只留下一诊断。
            release_recording_failure(lease);
            Ok(())
        }
        Err(primary) => {
            let error = discard(&temp, primary);
            release_recording_failure(lease);
            Err(error)
        }
    }
}

/// 放弃租约但不上抛它的错误：此刻真正要报告的是原始失败或“替换已完成”。
///
/// 错误不丢弃，只是改道：记到诊断通道，由同线程的调用方取走并报到 stderr。
fn release_recording_failure(lease: super::temp_cleanup::Lease) {
    let path = lease.path().to_path_buf();
    if let Err(e) = lease.release() {
        super::temp_cleanup::note_problem(path, e.to_string());
    }
}

/// 以不截断的写方式打开已有目标，沿用旧实现的写权限检查；目标不存在时返回 `None`。
fn existing_permissions(target: &Path) -> io::Result<Option<Permissions>> {
    let file = match OpenOptions::new().write(true).open(target) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let permissions = file.metadata()?.permissions();
    // root 在 Unix 上能以写方式打开只读文件，这里另行拒绝，各平台结果一致。
    if permissions.readonly() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} 是只读文件，拒绝改写", target.display()),
        ));
    }
    Ok(Some(permissions))
}

/// 复制权限、写入内容并落到磁盘；返回前关闭文件。
fn fill(
    mut file: File,
    temp: &Path,
    bytes: &[u8],
    permissions: Option<Permissions>,
) -> io::Result<()> {
    // 先收紧权限再写内容：私有文件（如 0600）的内容不能经默认权限的临时文件外露。
    inject(Point::Permissions)?;
    if let Some(permissions) = permissions {
        file.set_permissions(permissions)?;
        #[cfg(test)]
        faults::note_permissions_copied();
    }
    #[cfg(test)]
    faults::before_first_write(temp);
    #[cfg(not(test))]
    let _ = temp;
    write_payload(&mut file, bytes)?;
    inject(Point::Sync)?;
    file.sync_all()
}

fn rename(temp: &Path, target: &Path) -> io::Result<()> {
    #[cfg(test)]
    if faults::take_transient() {
        return Err(io::Error::from_raw_os_error(32));
    }
    fs::rename(temp, target)
}

/// Windows 上杀毒软件、索引服务等短暂占用文件会让改名报拒绝访问或共享冲突，在时限内重试。
fn retry_transient(
    mut attempt: impl FnMut() -> io::Result<()>,
    mut elapsed: impl FnMut() -> Duration,
    mut sleep: impl FnMut(Duration),
) -> io::Result<()> {
    loop {
        match attempt() {
            Err(e) if is_transient(&e) => {
                let spent = elapsed();
                if spent >= RENAME_RETRY_BUDGET {
                    return Err(e);
                }
                sleep((RENAME_RETRY_BUDGET - spent).min(RENAME_RETRY_STEP));
            }
            result => return result,
        }
    }
}

fn is_transient(e: &io::Error) -> bool {
    // ERROR_ACCESS_DENIED、ERROR_SHARING_VIOLATION、ERROR_LOCK_VIOLATION
    cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 33))
}

/// 替换已完成，目录同步失败不改变结果。
fn sync_parent(target: &Path) {
    #[cfg(unix)]
    if let Some(dir) = target.parent().and_then(|p| File::open(p).ok()) {
        let _ = dir.sync_all();
    }
    #[cfg(not(unix))]
    let _ = target;
}

fn discard(temp: &Path, primary: io::Error) -> io::Error {
    match remove_temp(temp) {
        Ok(()) => primary,
        Err(e) if e.kind() == io::ErrorKind::NotFound => primary,
        Err(cleanup) => io::Error::new(
            primary.kind(),
            CleanupFailed {
                primary,
                cleanup,
                temp: temp.to_owned(),
            },
        ),
    }
}

fn remove_temp(temp: &Path) -> io::Result<()> {
    #[cfg(test)]
    if faults::armed(|f| f.fail_cleanup) {
        return Err(io::Error::other("injected cleanup failure"));
    }
    fs::remove_file(temp)
}
fn write_payload(file: &mut File, bytes: &[u8]) -> io::Result<()> {
    #[cfg(test)]
    if faults::armed(|f| f.partial_write) {
        file.write_all(&bytes[..bytes.len() / 2])?;
        return Err(io::Error::other("injected partial write"));
    }
    file.write_all(bytes)
}

fn inject(point: Point) -> io::Result<()> {
    #[cfg(test)]
    if faults::armed(|f| match point {
        Point::Permissions => f.fail_permissions,
        Point::Sync => f.fail_sync,
        Point::BeforeRename => f.fail_before_rename,
    }) {
        return Err(io::Error::other(format!("injected {point:?} failure")));
    }
    let _ = point;
    Ok(())
}

/// 测试用的故障注入：只作用于安装它的线程，守卫析构（含 panic 展开）时清除。
#[cfg(test)]
pub(crate) mod faults {
    use std::cell::RefCell;
    use std::marker::PhantomData;
    use std::path::Path;

    pub(crate) type Hook = Box<dyn FnMut(&Path)>;

    #[derive(Default)]
    pub(crate) struct Faults {
        /// 前若干次替换不注入故障。
        pub(crate) skip_calls: u32,
        pub(crate) partial_write: bool,
        pub(crate) fail_permissions: bool,
        pub(crate) fail_sync: bool,
        pub(crate) fail_before_rename: bool,
        pub(crate) fail_cleanup: bool,
        /// 改名前先报这么多次 Windows 共享冲突（错误码 32）。
        pub(crate) transient_renames: u32,
        pub(crate) before_rename: Option<Hook>,
        /// 临时文件写入首个字节之前调用，参数为临时文件路径。
        pub(crate) before_first_write: Option<Hook>,
    }

    struct State {
        faults: Faults,
        armed: bool,
        permissions_copied: bool,
    }

    thread_local! {
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    }

    #[must_use]
    pub(crate) struct Guard(PhantomData<*const ()>);

    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = STATE.try_with(|s| s.borrow_mut().take());
        }
    }

    pub(crate) fn install(faults: Faults) -> Guard {
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                faults,
                armed: false,
                permissions_copied: false,
            })
        });
        Guard(PhantomData)
    }

    pub(crate) fn installed() -> bool {
        STATE.with(|s| s.borrow().is_some())
    }

    pub(super) fn begin() {
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.armed = state.faults.skip_calls == 0;
                state.permissions_copied = false;
                state.faults.skip_calls = state.faults.skip_calls.saturating_sub(1);
            }
        });
    }

    pub(super) fn armed(pick: impl FnOnce(&Faults) -> bool) -> bool {
        STATE.with(|s| {
            s.borrow()
                .as_ref()
                .is_some_and(|state| state.armed && pick(&state.faults))
        })
    }

    pub(super) fn take_transient() -> bool {
        STATE.with(|s| match s.borrow_mut().as_mut() {
            Some(state) if state.armed && state.faults.transient_renames > 0 => {
                state.faults.transient_renames -= 1;
                true
            }
            _ => false,
        })
    }

    pub(super) fn note_permissions_copied() {
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.permissions_copied = true;
            }
        });
    }

    /// 本次替换是否已把已有目标的权限复制到临时文件。
    pub(crate) fn permissions_copied() -> bool {
        STATE.with(|s| s.borrow().as_ref().is_some_and(|st| st.permissions_copied))
    }

    pub(super) fn before_rename(target: &Path) {
        run_hook(target, |f| &mut f.before_rename);
    }

    pub(super) fn before_first_write(temp: &Path) {
        run_hook(temp, |f| &mut f.before_first_write);
    }

    /// 调用钩子时不持有线程局部状态的借用，钩子可查询状态。
    fn run_hook(path: &Path, slot: fn(&mut Faults) -> &mut Option<Hook>) {
        let hook = STATE.with(|s| match s.borrow_mut().as_mut() {
            Some(state) if state.armed => slot(&mut state.faults).take(),
            _ => None,
        });
        if let Some(mut hook) = hook {
            hook(path);
            STATE.with(|s| {
                if let Some(state) = s.borrow_mut().as_mut() {
                    *slot(&mut state.faults) = Some(hook);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::faults::{Faults, install, installed};
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};

    const OLD: &str = "old bytes that must survive";
    const NEW: &str = "brand new content written atomically";

    fn scratch(name: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ziyuan-replace-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("keep.txt"), "unrelated").unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn lease_names(dir: &Path) -> Vec<String> {
        names(dir)
            .into_iter()
            .filter(|name| name.starts_with(".ziyuan-lease-"))
            .collect()
    }

    /// 注入 `faults` 后替换失败：目标保持原样（原文或不存在），不留自己的临时文件，旁边的文件不动。
    fn assert_fault_keeps_target(name: &str, existing: bool, faults: Faults) {
        let dir = scratch(name);
        let target = dir.join("target.txt");
        if existing {
            fs::write(&target, OLD).unwrap();
        }
        let err = {
            let _guard = install(faults);
            replace_target(&target, NEW.as_bytes()).unwrap_err()
        };
        assert!(err.to_string().contains("injected"), "{name}: {err}");
        assert!(
            err.get_ref().is_none_or(|e| !e.is::<CleanupFailed>()),
            "{name}: {err}"
        );
        if existing {
            assert_eq!(fs::read_to_string(&target).unwrap(), OLD, "{name}");
            assert_eq!(names(&dir), ["keep.txt", "target.txt"], "{name}");
        } else {
            assert!(!target.exists(), "{name}");
            assert_eq!(names(&dir), ["keep.txt"], "{name}");
        }
        assert_eq!(
            fs::read_to_string(dir.join("keep.txt")).unwrap(),
            "unrelated"
        );
        assert!(lease_names(&dir).is_empty(), "失败路径不得留下租约");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn partial_write_keeps_existing_target_bytes() {
        let faults = Faults {
            partial_write: true,
            ..Faults::default()
        };
        assert_fault_keeps_target("partial-existing", true, faults);
    }

    #[test]
    fn partial_write_leaves_absent_target_absent() {
        let faults = Faults {
            partial_write: true,
            ..Faults::default()
        };
        assert_fault_keeps_target("partial-absent", false, faults);
    }

    #[test]
    fn permission_copy_failure_keeps_target() {
        for existing in [true, false] {
            let faults = Faults {
                fail_permissions: true,
                ..Faults::default()
            };
            assert_fault_keeps_target("permissions", existing, faults);
        }
    }

    #[test]
    fn file_sync_failure_keeps_target() {
        for existing in [true, false] {
            let faults = Faults {
                fail_sync: true,
                ..Faults::default()
            };
            assert_fault_keeps_target("sync", existing, faults);
        }
    }

    #[test]
    fn pre_rename_failure_keeps_target() {
        for existing in [true, false] {
            let faults = Faults {
                fail_before_rename: true,
                ..Faults::default()
            };
            assert_fault_keeps_target("rename", existing, faults);
        }
    }

    #[test]
    fn cleanup_failure_reports_primary_error_and_temp_path() {
        let dir = scratch("cleanup");
        let target = dir.join("target.txt");
        fs::write(&target, OLD).unwrap();
        let err = {
            let _guard = install(Faults {
                fail_before_rename: true,
                fail_cleanup: true,
                ..Faults::default()
            });
            replace_target(&target, NEW.as_bytes()).unwrap_err()
        };
        let failed = err
            .get_ref()
            .and_then(|e| e.downcast_ref::<CleanupFailed>())
            .unwrap_or_else(|| panic!("缺少清理失败信息: {err}"));
        assert!(failed.primary.to_string().contains("BeforeRename"), "{err}");
        assert!(
            failed.cleanup.to_string().contains("injected cleanup"),
            "{err}"
        );
        assert_eq!(err.kind(), failed.primary.kind());
        let source = std::error::Error::source(failed).unwrap().to_string();
        assert_eq!(source, failed.primary.to_string());
        assert_eq!(failed.temp.parent(), Some(dir.as_path()));
        assert!(failed.temp.is_file(), "{}", failed.temp.display());
        assert!(err.to_string().contains(&failed.temp.display().to_string()));
        assert_eq!(fs::read_to_string(&target).unwrap(), OLD);
        fs::remove_file(&failed.temp).unwrap();
        assert_eq!(names(&dir), ["keep.txt", "target.txt"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fault_guard_resets_on_panic() {
        let unwound = std::panic::catch_unwind(|| {
            let _guard = install(Faults {
                fail_sync: true,
                ..Faults::default()
            });
            assert!(installed());
            panic!("故意 panic");
        });
        assert!(unwound.is_err());
        assert!(!installed());
        let dir = scratch("reset");
        let target = dir.join("target.txt");
        replace_target(&target, NEW.as_bytes()).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), NEW);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replaces_existing_and_creates_new_without_leftovers() {
        let dir = scratch("success");
        let existing = dir.join("existing.txt");
        fs::write(&existing, OLD).unwrap();
        replace_target(&existing, NEW.as_bytes()).unwrap();
        replace_target(&dir.join("fresh.txt"), b"fresh").unwrap();
        assert_eq!(fs::read_to_string(&existing).unwrap(), NEW);
        assert_eq!(fs::read_to_string(dir.join("fresh.txt")).unwrap(), "fresh");
        assert_eq!(names(&dir), ["existing.txt", "fresh.txt", "keep.txt"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replacement_leaves_a_clean_directory() {
        let dir = scratch("clean");
        let target = dir.join("target.txt");
        std::fs::write(&target, OLD).unwrap();
        replace_target(&target, NEW.as_bytes()).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), NEW);
        // `scratch` 本来就会放一个 keep.txt：它必须原样在，别的都不许留。
        assert_eq!(
            names(&dir),
            vec!["keep.txt".to_string(), "target.txt".to_string()],
            "成功替换后不留临时文件或租约"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lease_is_held_after_the_temp_handle_closes_and_before_rename() {
        use super::super::temp_cleanup::tests_hooks::take_claims;
        // 同线程上一个用例留下的认领会污染下面的比对：先丢掉。
        let _ = take_claims();
        let dir = scratch("rename-window");
        let target = dir.join("target.txt");
        std::fs::write(&target, OLD).unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let slot = seen.clone();
        let temp_seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let temp_slot = temp_seen.clone();
        // `before_first_write` 的参数就是临时文件路径：租约认领的名字只能从写入侧拿到，
        // 写入者持锁期间第二个句柄读不到租约内容。
        let _guard = install(Faults {
            before_first_write: Some(Box::new(move |temp: &Path| {
                *temp_slot.lock().unwrap() = Some(temp.to_path_buf());
            })),
            // `before_rename` 在 `fill` 关闭临时文件之后、改名前调用：正是设计要求的窗口。
            before_rename: Some(Box::new(move |target: &Path| {
                let temp = temp_seen
                    .lock()
                    .unwrap()
                    .take()
                    .expect("before_first_write 未被调用");
                let claims = take_claims();
                assert_eq!(
                    claims,
                    vec![temp.file_name().unwrap().to_string_lossy().into_owned()],
                    "租约必须认领本次写入的临时文件，而不只是存在一份租约"
                );
                let parent = target.parent().unwrap();
                let leases = lease_names(parent);
                // 光有租约文件不够：句柄若在改名前被关掉或丢掉，文件还在、锁却没了，
                // 清理者立刻就能把这次写入判成陈旧。改名窗口内必须**仍持有**锁。
                assert_eq!(leases.len(), 1, "改名窗口内必须恰好有一份租约: {leases:?}");
                #[cfg(windows)]
                {
                    // 同进程跨句柄互斥只在 Windows 上成立（见设计记录的平台事实）；
                    // Unix 上 `flock(2)` 的语义不同，这里只断言 Windows 观察到的事实，
                    // 不在 Unix 上假装验证了同一件事。
                    let lease_path = parent.join(&leases[0]);
                    let probe = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&lease_path)
                        .unwrap_or_else(|e| panic!("改名窗口内租约必须仍然存在: {e}"));
                    assert!(
                        matches!(probe.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
                        "改名前租约必须仍被持有"
                    );
                }
                *slot.lock().unwrap() = Some(leases);
            })),
            ..Faults::default()
        });
        replace_target(&target, NEW.as_bytes()).unwrap();
        let leases = seen.lock().unwrap().take().expect("before_rename 未被调用");
        assert_eq!(leases.len(), 1, "改名窗口内必须恰好有一份租约: {leases:?}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), NEW);
        assert!(lease_names(&dir).is_empty(), "成功路径必须清掉租约");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lease_delete_failure_does_not_fail_the_replacement() {
        let _ = super::super::temp_cleanup::drain_problems();
        let dir = scratch("lease-stuck");
        let target = dir.join("target.txt");
        std::fs::write(&target, OLD).unwrap();
        let _armed = super::super::temp_cleanup::tests_hooks::arm_swap_next_lease_to_dir();
        let result = replace_target(&target, NEW.as_bytes());
        assert!(result.is_ok(), "删租约失败不得报替换失败: {result:?}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), NEW);
        let problems = super::super::temp_cleanup::drain_problems();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(
            problems[0].path.parent(),
            Some(dir.as_path()),
            "诊断必须指向本次写入所在目录的那份租约: {problems:?}"
        );
        assert!(
            problems[0]
                .path
                .to_string_lossy()
                .contains(".ziyuan-lease-"),
            "{problems:?}"
        );
        // 把换名成目录的租约清掉，供后面的用例复用目录无关的假设。
        for name in lease_names(&dir) {
            let _ = std::fs::remove_dir_all(dir.join(name));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reader_sees_old_content_until_rename_then_new_content() {
        let dir = scratch("reader");
        let target = dir.join("target.txt");
        fs::write(&target, OLD).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let reader = std::thread::spawn({
            let (barrier, target) = (barrier.clone(), target.clone());
            move || {
                barrier.wait();
                let before = fs::read_to_string(&target).unwrap();
                barrier.wait();
                barrier.wait();
                (before, fs::read_to_string(&target).unwrap())
            }
        });
        {
            let hook_barrier = barrier.clone();
            let _guard = install(Faults {
                before_rename: Some(Box::new(move |_| {
                    hook_barrier.wait();
                    hook_barrier.wait();
                })),
                ..Faults::default()
            });
            replace_target(&target, NEW.as_bytes()).unwrap();
        }
        barrier.wait();
        let (before, after) = reader.join().unwrap();
        assert_eq!((before.as_str(), after.as_str()), (OLD, NEW));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn readonly_target_is_rejected_and_kept() {
        let dir = scratch("readonly");
        let target = dir.join("target.txt");
        fs::write(&target, OLD).unwrap();
        let mut permissions = fs::metadata(&target).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&target, permissions).unwrap();
        let err = replace_target(&target, NEW.as_bytes()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
        assert_eq!(fs::read_to_string(&target).unwrap(), OLD);
        let mut permissions = fs::metadata(&target).unwrap().permissions();
        assert!(permissions.readonly(), "只读属性不得被清除");
        assert_eq!(names(&dir), ["keep.txt", "target.txt"]);
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(&target, permissions).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unix_mode_bits_are_preserved() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode");
        for mode in [0o600, 0o755] {
            let target = dir.join(format!("target-{mode:o}"));
            fs::write(&target, OLD).unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(mode)).unwrap();
            replace_target(&target, NEW.as_bytes()).unwrap();
            assert_eq!(fs::read_to_string(&target).unwrap(), NEW);
            let actual = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(actual, mode);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn transient_rename_conflicts_are_retried() {
        let dir = scratch("transient");
        let target = dir.join("target.txt");
        fs::write(&target, OLD).unwrap();
        {
            let _guard = install(Faults {
                transient_renames: 3,
                ..Faults::default()
            });
            replace_target(&target, NEW.as_bytes()).unwrap();
        }
        assert_eq!(fs::read_to_string(&target).unwrap(), NEW);
        assert_eq!(names(&dir), ["keep.txt", "target.txt"]);
        fs::remove_dir_all(dir).unwrap();
    }

    fn retry_with_clock(
        start: Duration,
        mut outcome: impl FnMut(u32) -> io::Result<()>,
    ) -> (io::Result<()>, u32, Vec<Duration>) {
        let clock = std::cell::Cell::new(start);
        let (mut attempts, mut slept) = (0, Vec::new());
        let result = retry_transient(
            || {
                attempts += 1;
                outcome(attempts)
            },
            || clock.get(),
            |d| {
                slept.push(d);
                clock.set(clock.get() + d);
            },
        );
        (result, attempts, slept)
    }

    #[cfg(windows)]
    #[test]
    fn transient_rename_errors_succeed_after_retries() {
        let (result, attempts, slept) = retry_with_clock(Duration::ZERO, |n| match n {
            1 => Err(io::Error::from_raw_os_error(5)),
            2 => Err(io::Error::from_raw_os_error(32)),
            3 => Err(io::Error::from_raw_os_error(33)),
            _ => Ok(()),
        });
        result.unwrap();
        assert_eq!(attempts, 4);
        assert_eq!(slept, [RENAME_RETRY_STEP; 3]);
    }

    #[cfg(windows)]
    #[test]
    fn transient_rename_errors_give_up_after_the_budget() {
        let (result, attempts, slept) =
            retry_with_clock(Duration::ZERO, |_| Err(io::Error::from_raw_os_error(32)));
        assert_eq!(result.unwrap_err().raw_os_error(), Some(32));
        assert_eq!(slept.iter().sum::<Duration>(), RENAME_RETRY_BUDGET);
        assert!(slept.iter().all(|d| *d <= RENAME_RETRY_STEP));
        assert_eq!(attempts as usize, slept.len() + 1);
        // 剩余时间不足一步时只睡剩余的部分。
        let (result, attempts, slept) = retry_with_clock(Duration::from_millis(246), |_| {
            Err(io::Error::from_raw_os_error(33))
        });
        assert!(result.is_err());
        assert_eq!((attempts, slept), (2, vec![Duration::from_millis(4)]));
    }

    #[test]
    fn other_rename_errors_are_not_retried() {
        let mut codes = vec![2];
        if cfg!(not(windows)) {
            codes.extend([5, 32, 33]);
        }
        for code in codes {
            let (result, attempts, slept) =
                retry_with_clock(Duration::ZERO, |_| Err(io::Error::from_raw_os_error(code)));
            assert_eq!(result.unwrap_err().raw_os_error(), Some(code));
            assert_eq!((attempts, slept.len()), (1, 0), "{code}");
        }
    }

    #[test]
    fn permissions_are_copied_before_any_content_is_written() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let dir = scratch("private");
        let target = dir.join("target.txt");
        fs::write(&target, OLD).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let seen = Rc::new(RefCell::new(None));
        {
            let seen = seen.clone();
            let _guard = install(Faults {
                before_first_write: Some(Box::new(move |temp| {
                    let metadata = fs::metadata(temp).unwrap();
                    #[cfg(unix)]
                    let mode = {
                        use std::os::unix::fs::PermissionsExt;
                        metadata.permissions().mode() & 0o777
                    };
                    #[cfg(not(unix))]
                    let mode = 0o600;
                    *seen.borrow_mut() = Some((faults::permissions_copied(), metadata.len(), mode));
                })),
                ..Faults::default()
            });
            replace_target(&target, NEW.as_bytes()).unwrap();
        }
        assert_eq!(
            *seen.borrow(),
            Some((true, 0, 0o600)),
            "写入首个字节前须已复制权限"
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), NEW);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn long_target_names_can_be_replaced_and_created() {
        let dir = scratch("long");
        let existing = format!("{}.txt", "e".repeat(236));
        let absent = format!("{}.txt", "a".repeat(236));
        assert_eq!((existing.len(), absent.len()), (240, 240));
        fs::write(dir.join(&existing), OLD).unwrap();
        replace_target(&dir.join(&existing), NEW.as_bytes()).unwrap();
        replace_target(&dir.join(&absent), b"fresh").unwrap();
        assert_eq!(fs::read_to_string(dir.join(&existing)).unwrap(), NEW);
        assert_eq!(fs::read_to_string(dir.join(&absent)).unwrap(), "fresh");
        let mut expected = vec![absent, existing, "keep.txt".to_owned()];
        expected.sort();
        assert_eq!(names(&dir), expected);
        fs::remove_dir_all(dir).unwrap();
    }
}
