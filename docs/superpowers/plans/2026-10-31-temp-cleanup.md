# 1.2.2 临时文件清理 Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让原子替换不再遗留需手工删除的 `.ziyuan-tmp-*`：正常写入只清理目标目录里可证明陈旧的临时文件，另给一条显式命令清理整个工作目录。

**Architecture:** 写入者在目标目录先建 `.ziyuan-lease-<id>` 租约、写内容并 `sync_all`、加锁、核验路径仍指向自己，再建临时文件；改名后关句柄并删租约。清理者从不等待租约锁：逐个非阻塞试锁，取得锁后读 `temp=` 认领候选，认领者唯一且目录里没有读不懂的租约才删除，且删除时仍持锁。受保护路径集合由调用方传入，核验器不读恢复记录。

**Tech Stack:** Rust 1.89（edition 2024，`File::try_lock` 自 1.89 稳定）、`std::hash::RandomState`、无新增依赖。

---

## 依据与边界

- 设计：[1.2.2 原子替换临时文件清理设计](../specs/2026-10-31-temp-recovery-design.md)（已获用户批准，含租约协议、状态机、锁顺序、失败降级）。
- 实验证据：[1.2.2 临时租约可行性验证报告](../../acceptance/1.2.2-temp-lease-feasibility.md)（34 项实验用例）。
- 只做 1.2.2 的清理修复。向量翻译记忆库属 1.3.0，本计划一律不碰。
- 不在本计划内：版本号登记、tag、推送、GitHub Release、crates.io 发布，均另行授权。

## 平台事实（写代码前必须知道）

1. Rust 的 `parse::<u32>()` **接受前导 `+`**（不接受前后空白），所以"是不是十进制数字"必须逐字节判断：`+1`、全角数字、空段都过不了字节判断，范围再交给 `parse`。两步缺一不可。
2. `u32`/`u128`/`u64` 的最大十进制位数是 **10/39/20**；`u128::MAX` 是 39 位，不是 38。
3. **Windows 上租约被锁住时，同一进程的另一个句柄读它会被拒绝**（字节区间锁冲突）。因此测试要读租约内容只能经持有者自己的句柄；清理者则必须**先取到锁、并用同一个句柄**完成核验与删除（设计"身份复核"：锁身份按文件对象，不按路径）。
4. 同进程、跨句柄的锁互斥**只在 Windows 上观测成立**；Unix 上可能共享。生产清理因此不承诺同进程保护，见设计"已知限制"，相关断言按平台门控。
5. 每个 chunk 结束时 `cargo clippy --all-targets -- -D warnings` 必须干净。Chunk 1–3 里 `Lease` 与决策函数还没有生产调用点，靠 Task 2 在文件顶部加的 `#![allow(dead_code)]` 兜住；**Task 4 用 `sweep_dir` 打通可达性后必须删掉这行**，删除后仍要通过 clippy，这才是它真的不再需要的证明。

## 文件结构

| 文件 | 责任 |
| --- | --- |
| `crates/ziyuan-runtime/src/tools/temp_cleanup.rs`（新建） | 临时文件命名与严格校验、租约载体、清理决策、目录与整树扫描、报告与原因名。**唯一**的命名与核验实现。 |
| `crates/ziyuan-runtime/src/tools/replace.rs`（修改） | `replace_target` 用租约覆盖 创建→写入→同步→关句柄→改名→删租约；临时文件名改由 `temp_cleanup` 生成。 |
| `crates/ziyuan-runtime/src/tools.rs`（修改） | 声明 `pub mod temp_cleanup;`；新增 `apply_changes_protected`，写入前清理目标目录并报诊断。 |
| `crates/ziyuan-cli/src/text_commands.rs`（修改） | 新增 `text recovery temps [--json] [--yes]`；从 `pending_record()` 构造受保护集合。 |
| `crates/ziyuan-app/src/edicts.rs`（修改） | `write_transaction` 改调 `apply_changes_protected`，把恢复记录目标一并作为受保护集合。 |
| `crates/ziyuan-app/src/write_txn_tests.rs`（修改） | 受管写入前真的清掉同目录陈旧临时文件的用例。 |
| `crates/ziyuan-cli/src/help.rs`（修改） | `text` 子命令补全里补上 `recovery`。 |
| `crates/ziyuan-cli/tests/contract.rs`（修改） | `text recovery temps --json` 黑盒契约。 |
| `docs/text-projects.md`、`docs/stability.md`、`CHANGELOG.md`、`docs/roadmap.md`、`docs/acceptance/1.2.2.md` | 用户文档、兼容性说明、验收记录。 |

## 关键约定

1. 临时文件名只有一处生成：`temp_cleanup::fresh_temp_name()`。
2. 清理者永不等待租约锁，只 `try_lock`；写入者等自己的锁，上限 300 ms。
3. 归属只能反查：候选名不含租约 `id`，必须列出租约、逐个试锁、读 `temp=` 比对。
4. 删除顺序：**持锁中**先删临时文件，再删租约，最后关句柄。
5. 核验不足一律保留；`--yes` 不是强制删除。**预览模式删除数为 0，且不删孤儿租约。**
6. 自动清理失败不阻断新写入，诊断走 stderr，不进 `--json` 的 stdout。
7. 扫描不进入 `.ziyuan`、`.git`、`.jj`、`target`、`node_modules`，不跟随符号链接与 junction。
8. 计划里的 Rust 片段为了可读性没有逐行折到 100 列以内，`format!`／`assert!` 之类的长行照抄会超限。**每次跑 `cargo fmt --all -- --check` 之前先跑一次 `cargo fmt --all`**，再 `--check` 复核；需要保留的换行在 review 时看 diff 判断，不要为了迁就片段而改断行。

## Chunk 1: 命名与租约载体（runtime）

### Task 1: 严格候选名与唯一命名处

**Files:**
- Create: `crates/ziyuan-runtime/src/tools/temp_cleanup.rs`
- Modify: `crates/ziyuan-runtime/src/tools.rs:23`
- Modify: `crates/ziyuan-runtime/src/tools/replace.rs:1-8`（导入）与 `:93-111`（`create_temp`）

- [ ] **Step 1: 声明模块、写唯一命名处、写失败测试。**

`crates/ziyuan-runtime/src/tools.rs` 第 23 行附近：

```rust
mod replace;
pub mod temp_cleanup;
```

新建 `temp_cleanup.rs`，**只写本步骤用到的东西**（多写的导入与常量会在非测试构建里被判死代码）：

```rust
//! 原子替换的临时文件与租约：命名、归属核验、清理。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 临时文件名前缀；清理器只认这一种命名。
pub(crate) const TEMP_PREFIX: &str = ".ziyuan-tmp-";

/// 本次写入的临时文件名：不含目标文件名，长度有界。
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
/// 空壳：Step 3 才写实现，先让 Step 2 真的失败。
pub fn is_candidate_name(name: &str) -> bool {
    let _ = name;
    false
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
        assert!(is_candidate_name(".ziyuan-tmp-4294967295-0-0"));
        assert!(is_candidate_name(
            ".ziyuan-tmp-0-340282366920938463463374607431768211455-0"
        ));
        assert!(is_candidate_name(
            ".ziyuan-tmp-0-0-18446744073709551615"
        ));
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
            // 超出各段最大值
            ".ziyuan-tmp-4294967296-1-1",
            ".ziyuan-tmp-1-340282366920938463463374607431768211456-1",
            ".ziyuan-tmp-1-1-18446744073709551616",
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
    }

    #[test]
    fn fresh_names_are_candidates_and_distinct() {
        let first = fresh_temp_name();
        let second = fresh_temp_name();
        assert!(is_candidate_name(&first), "{first}");
        assert!(is_candidate_name(&second), "{second}");
        assert_ne!(first, second);
    }
}
```

`replace.rs` 的 `create_temp` 改用唯一命名处：

```rust
fn create_temp(target: &Path) -> io::Result<(PathBuf, File)> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "目标路径缺少所在目录"))?;
    let temp = parent.join(super::temp_cleanup::fresh_temp_name());
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    Ok((temp, file))
}
```

`replace.rs` 顶部导入相应收窄：删掉 `SystemTime, UNIX_EPOCH`；**`AtomicU64`/`Ordering` 也从第 6 行删掉**，改为在 `mod tests` 里 `use std::sync::atomic::{AtomicU64, Ordering};`（否则非测试构建报未使用导入）。

- [ ] **Step 2: 运行，确认失败。**

```powershell
cargo test -p ziyuan-runtime --locked --lib tools::temp_cleanup
```

Expected: 2 failed（空壳返回 `false`，两个用例都断言失败）。

- [ ] **Step 3: 写真正实现。**

只替换 `is_candidate_name`：

```rust
pub fn is_candidate_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(TEMP_PREFIX) else {
        return false;
    };
    let parts: Vec<&str> = rest.split('-').collect();
    let [pid, nanos, counter] = parts.as_slice() else {
        return false;
    };
    // 逐字节判十进制（`parse` 会接受 `+1` 与空白），再用 parse 判范围。
    pid.parse::<u32>().is_ok()
        && nanos.parse::<u128>().is_ok()
        && counter.parse::<u64>().is_ok()
        && [pid, nanos, counter]
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}
```

两处判断缺一不可：`parse` 管范围，逐字节判断管"只接受 ASCII 十进制"。

- [ ] **Step 4: 运行，确认通过并做仓库标准校验。**

```powershell
cargo test -p ziyuan-runtime --locked --lib tools::temp_cleanup
cargo test -p ziyuan-runtime --locked --lib tools::replace::tests::
cargo fmt --all -- --check
cargo clippy -p ziyuan-runtime --all-targets --locked -- -D warnings
```

Expected: 2 passed；`tools::replace::tests` 16 passed；fmt 与 clippy 通过。

- [ ] **Step 5: 提交。**

```powershell
jj commit -m "feat(runtime): 统一临时文件命名并严格校验候选名" -m "Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>"
```

### Task 2: 租约载体生命周期

**Files:**
- Modify: `crates/ziyuan-runtime/src/tools/temp_cleanup.rs`

- [ ] **Step 1: 写失败测试。**

追加到 `mod tests`（保留 Task 1 的两个用例）：

```rust
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
                path.file_name().is_some_and(|name| {
                    name.to_string_lossy().starts_with(super::LEASE_PREFIX)
                })
            })
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn lease_covers_creation_until_release() {
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
    fn release_reports_a_lease_it_could_not_delete() {
        let dir = scratch("release-fails");
        let name = fresh_temp_name();
        let lease = Lease::hold(&dir, &name).unwrap();
        let path = lease.path().to_path_buf();
        // 让删除必然失败：用一个"只读父目录"不跨平台，改用把目标换成目录——
        // 但要先让 release 关掉自己的句柄，所以经测试钩子换名。
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
        // 反斜杠、冒号、NUL 在任何平台上都不接受，免得同一份判断在 Unix 上放宽。
        for bad in [
            "", "..", ".", "sub/temp", "sub\\temp", "C:temp", "a:b", "a\nb", "a\rb", "a\0b",
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
```

- [ ] **Step 2: 运行，确认失败。**

```powershell
cargo test -p ziyuan-runtime --locked --lib tools::temp_cleanup
```

Expected: 编译失败，`cannot find type Lease`。

- [ ] **Step 3: 写实现。**

先在文件**最顶部**（`//!` 文档注释之后、`use` 之前）加一行临时脚手架：

```rust
// Chunk 1-3 还没有生产调用点；Task 4 用 sweep_dir 打通后必须删掉本行。
#![allow(dead_code)]
```

再在 `is_candidate_name` 之后追加（同时补上本任务才用到的导入）：

```rust
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};

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
    released: bool,
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
                        released: false,
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

    /// 租约文件路径：`replace.rs` 用它填“删不掉”的诊断。
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
        self.released = true;
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
        if !self.released {
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
    if name.is_empty()
        || name.contains(['\n', '\r', '\t', ' ', '\0', ':', '/', '\\'])
    {
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
        SwapToDir { path: Option<PathBuf> },
    }

    thread_local! {
        static ARMED: RefCell<Vec<Armed>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn arm_reclaim() {
        ARMED.with(|slot| slot.borrow_mut().push(Armed::Reclaim));
    }

    /// 记下要换名的租约路径；真正换名发生在 `release` 关句柄之后。
    pub(crate) fn arm_swap_to_dir(path: &Path) {
        ARMED.with(|slot| {
            slot.borrow_mut().push(Armed::SwapToDir {
                path: Some(path.to_owned()),
            })
        });
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

    pub(crate) fn before_lock(path: &Path) {
        if take(|armed| matches!(armed, Armed::Reclaim)).is_some() {
            let _ = std::fs::remove_file(path);
        }
    }

    pub(crate) fn after_close(path: &Path) {
        if let Some(Armed::SwapToDir { path: Some(target) }) =
            take(|armed| matches!(armed, Armed::SwapToDir { path: Some(target) } if target == path))
        {
            let _ = target;
            swap(path);
        }
    }
}
```

`contents` 用到 `Read`/`Seek`，但它们在非测试构建里没有调用点。所以模块顶部只留 `use std::io::{self, Write};`，把 `use std::io::{Read, Seek};` 放进 `contents` 内部；Task 3 的 `read_lease` 同理，内部写 `use std::io::Read;`。

- [ ] **Step 4: 运行，确认通过并做仓库标准校验。**

```powershell
cargo test -p ziyuan-runtime --locked --lib tools::temp_cleanup
cargo test -p ziyuan-runtime --locked --lib tools::replace::tests::
cargo test -p ziyuan-runtime --locked
cargo fmt --all -- --check
cargo clippy -p ziyuan-runtime --all-targets --locked -- -D warnings
```

Expected: `tools::temp_cleanup` 7 passed；`tools::replace::tests` 16 passed；库测试除既有两个并发敏感的 MCP 用例外通过（该问题见[验收报告](../../acceptance/1.2.2-temp-lease-feasibility.md)，与本次改动无关）；fmt 与 clippy 通过。

- [ ] **Step 5: 提交。**

```powershell
jj commit -m "feat(runtime): 加入覆盖整个替换过程的租约载体" -m "Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>"
```

## Chunk 2: 决策与报告（runtime）

### Task 3: 租约解析与单候选决策

**Files:**
- Modify: `crates/ziyuan-runtime/src/tools/temp_cleanup.rs`

- [ ] **Step 1: 写失败测试。**

追加到 `mod tests`。辅助函数先加：

```rust
    fn empty_protected() -> std::collections::BTreeSet<String> {
        std::collections::BTreeSet::new()
    }

    fn hex_id(seed: u64) -> String {
        format!("{seed:032x}")
    }

    /// 已死写入者留下的合法租约：内容对、没人持锁。
    fn dead_lease(dir: &std::path::Path, id: &str, temp: &str) {
        std::fs::write(dir.join(format!(".ziyuan-lease-{id}")), lease_line(temp, id)).unwrap();
    }

    fn write_temp(dir: &std::path::Path, name: &str) {
        std::fs::write(dir.join(name), b"partial").unwrap();
    }

    fn keep_reason(item: &Item) -> Reason {
        match item.outcome {
            Outcome::Keep(reason) => reason,
            Outcome::Remove => panic!("期望保留，实际可删除: {:?}", item.path),
        }
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
            (format!("ziyuan-lease v1 id={id} temp={name}\n"), Reason::LeaseUnparsable),
            (
                format!("ziyuan-lease v1 temp={name} id={id} extra=1\n"),
                Reason::LeaseUnparsable,
            ),
            (format!("ziyuan-lease v1 temp={name}\n"), Reason::LeaseUnparsable),
            (
                format!("ziyuan-lease v1 temp={name} id={id} id={id}\n"),
                Reason::LeaseUnparsable,
            ),
            (format!("ziyuan-lease v1 temp={name} id={id}"), Reason::LeaseUnparsable),
            // temp 必须是同目录裸文件名。
            (
                format!("ziyuan-lease v1 temp=sub/{name} id={id}\n"),
                Reason::LeaseUnparsable,
            ),
            // id 必须 32 位小写十六进制，且与文件名一致。
            (format!("ziyuan-lease v1 temp={name} id={id}\n"), Reason::LeaseIdMismatch),
            (
                format!("ziyuan-lease v1 temp={name} id=ABCDEFABCDEFABCDEFABCDEFABCDEFAB\n"),
                Reason::LeaseUnparsable,
            ),
            // 标签错。
            (format!("ziyuan-loan v1 temp={name} id={id}\n"), Reason::LeaseUnparsable),
        ];
        for (text, expected) in cases {
            // 最后一项故意传入与文件 id 不同的实参，验证 id= 与文件名比对。
            let file_id = if expected == Reason::LeaseIdMismatch {
                hex_id(9)
            } else {
                id.clone()
            };
            assert_eq!(parse_lease(&text, &file_id).unwrap_err(), expected, "{text:?}");
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
        let item = decide_candidate(&dir, &name, &name, &protected, true, &mut problems);
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
        let item = decide_candidate(&sub, &name, &relative, &protected, true, &mut problems);
        assert_eq!(keep_reason(&item), Reason::RecoveryReferenced);
        // 反例：集合里只有裸名时不该命中。
        let mut protected = empty_protected();
        protected.insert(name.clone());
        let item = decide_candidate(&sub, &name, &relative, &protected, true, &mut problems);
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
        let item = decide_candidate(&dir, &name, &name, &empty_protected(), true, &mut problems);
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
        let item = decide_candidate(&dir, &name, &name, &empty_protected(), true, &mut problems);
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
        let item = decide_candidate(&dir, &name, &name, &empty_protected(), true, &mut problems);
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
        let item = decide_candidate(&dir, &name, &name, &empty_protected(), false, &mut problems);
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
        let item = decide_candidate(&dir, &name, &name, &empty_protected(), true, &mut problems);
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
        std::fs::write(dir.join(format!(".ziyuan-lease-{}", hex_id(20))), b"garbage\n").unwrap();
        let mut problems = Vec::new();
        let item = decide_candidate(&dir, &name, &name, &empty_protected(), true, &mut problems);
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
        std::fs::write(dir.join(format!(".ziyuan-lease-{}", hex_id(23))), b"ziyuan-lease v3\n").unwrap();
        let mut problems = Vec::new();
        let item = decide_candidate(&dir, &name, &name, &empty_protected(), true, &mut problems);
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
        let item = decide_candidate(&dir, &name, &name, &empty_protected(), true, &mut problems);
        // 同进程跨句柄互斥只在 Windows 上观测成立，见设计“已知限制”。
        #[cfg(windows)]
        assert_eq!(keep_reason(&item), Reason::LeaseBusy);
        #[cfg(not(windows))]
        assert!(matches!(item.outcome, Outcome::Remove) || keep_reason(&item) == Reason::LeaseBusy);
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
        let item = decide_candidate(&dir, &name, &name, &empty_protected(), true, &mut problems);
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
        let preview = decide_lease(&dir, &dir.join(format!(".ziyuan-lease-{id}")), &id, false, &mut problems);
        assert_eq!(preview.outcome, Outcome::Remove);
        assert!(!preview.acted);
        assert_eq!(lease_paths(&dir).len(), 1, "预览不得删孤儿租约");
        let applied = decide_lease(&dir, &dir.join(format!(".ziyuan-lease-{id}")), &id, true, &mut problems);
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
        let item = decide_lease(&dir, &dir.join(format!(".ziyuan-lease-{id}")), &id, true, &mut problems);
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
        let item = decide_candidate(&dir, &name, &name, &empty_protected(), true, &mut problems);
        tests_hooks::clear();
        assert_eq!(keep_reason(&item), Reason::TempAlreadyAbsent);
        assert!(problems.is_empty(), "已不存在不是失败");
        assert_eq!(lease_paths(&dir).len(), 1, "候选没了就不动租约，留给孤儿路径");
        std::fs::remove_dir_all(dir).unwrap();
    }
```

`tempfile_target` 之类的外部 helper 不需要：`symlinked_candidate_is_never_followed` 里的 `outside` 直接写成

```rust
    #[cfg(unix)]
    fn tempfile_target(dir: &std::path::Path) -> std::path::PathBuf {
        let outside = dir.with_extension("outside");
        std::fs::write(&outside, b"keep me").unwrap();
        outside
    }
```

- [ ] **Step 2: 运行，确认失败。**

```powershell
cargo test -p ziyuan-runtime --locked --lib tools::temp_cleanup
```

Expected: 编译失败，`cannot find function decide_candidate`。

- [ ] **Step 3: 写实现。**

在 `Lease` 定义之前插入报告与原因类型。Task 2 已有 `use std::path::Path;`，这一行扩成
`use std::path::{Path, PathBuf};`，并新增 `use std::collections::BTreeSet;`。

```rust
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

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
    pub acted: bool,
}

/// 逐项 I/O 失败：进报告，不改变处置结论。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Problem {
    pub path: PathBuf,
    pub message: String,
}
```

在 `mod tests_hooks` **之前**插入解析与决策（也就是文件里除测试模块之外的最后一段；Rust 不在意项的顺序，写在这里不与 Task 2 已排好的 `struct Lease`／`is_bare_name` 冲突）：

```rust
/// 读取上限：超过即视为不可解析，不把整轮清理变成中止。
const MAX_LEASE_BYTES: u64 = 4096;

fn read_lease(file: &mut File) -> Result<String, Reason> {
    use std::io::Read;
    let mut buffer = Vec::new();
    file.by_ref()
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
fn decide_candidate(
    dir: &Path,
    name: &str,
    relative: &str,
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
            problems.push(Problem { path: path.clone(), message: e.to_string() });
            return keep(path, Kind::Temp, Reason::TempNotRegular);
        }
        Ok(meta) if !meta.file_type().is_file() => {
            return keep(path, Kind::Temp, Reason::TempNotRegular);
        }
        Ok(_) => {}
    }

    let entries = match lease_entries(dir) {
        Ok(entries) => entries,
        Err(e) => {
            problems.push(Problem { path: dir.to_path_buf(), message: e.to_string() });
            return keep(path, Kind::Temp, Reason::LeaseUnparsable);
        }
    };

    let mut claimants: Vec<Claim> = Vec::new();
    let mut first_bad: Option<Reason> = None;
    for (lease_path, id) in entries {
        let meta = match fs::symlink_metadata(&lease_path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                problems.push(Problem { path: lease_path, message: e.to_string() });
                first_bad.get_or_insert(Reason::LeaseUnparsable);
                continue;
            }
            Ok(meta) => meta,
        };
        if !meta.file_type().is_file() {
            first_bad.get_or_insert(Reason::LeaseNotRegular);
            continue;
        }
        let mut file = match OpenOptions::new().read(true).write(true).open(&lease_path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                problems.push(Problem { path: lease_path, message: e.to_string() });
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
                problems.push(Problem { path: lease_path, message: e.to_string() });
                first_bad.get_or_insert(Reason::LeaseUnparsable);
                continue;
            }
            Ok(()) => {}
        }
        // 锁身份按文件对象：加锁后再看名字是否还指向一个普通文件。名字被换掉时
        // 手里的锁证明不了任何事，本轮不动（与写入者的第 4 步核验同理）。
        match fs::symlink_metadata(&lease_path) {
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
        match parse_lease(&text, &id) {
            // `parse_lease` 已保证 `id=` 与文件名一致，认出归属就够了。
            Ok(temp) if temp == name => claimants.push(Claim {
                path: lease_path,
                _lock: file,
            }),
            // 不认领本候选：立刻放开，别让手里的锁妨碍别的决策。
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
        acted: outcome == Outcome::Remove,
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
            problems.push(Problem { path: path.to_path_buf(), message: e.to_string() });
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
            problems.push(Problem { path: lease_path.to_path_buf(), message: e.to_string() });
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
            problems.push(Problem { path: lease_path.to_path_buf(), message: e.to_string() });
            return kept(Reason::LeaseUnparsable);
        }
        Ok(file) => file,
    };
    match file.try_lock() {
        Err(std::fs::TryLockError::WouldBlock) => {
            return kept(Reason::LeaseBusy);
        }
        Err(std::fs::TryLockError::Error(e)) => {
            problems.push(Problem { path: lease_path.to_path_buf(), message: e.to_string() });
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
            problems.push(Problem { path: temp, message: e.to_string() });
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
            problems.push(Problem { path: lease_path.to_path_buf(), message: e.to_string() });
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

```

`decide_lease` 的临时路径一律用传入的 `dir`，不从租约路径反推，免得嵌套目录算错。

清理者侧**不再重新打开租约**：`decide_candidate` 把锁住的 `File` 放进 `Claim` 交给 `remove_as_owner`，删完临时文件与租约后随结构释放。这样“核验—删除”之间没有窗口，两个清理者不可能各自核验通过、各删一次。

**本任务要给 `tests_hooks` 加上 `Vanish` 分支**（`candidate_vanishing_before_delete_is_not_an_error` 用它覆盖删除时的 `NotFound`；`before_remove` 已在 `remove_as_owner` 里调用）：

```rust
    /// 删除候选之前先把它删掉：覆盖删除时的 `NotFound`。
    Vanish { path: PathBuf },
```

```rust
    pub(crate) fn arm_vanish(path: &Path) {
        ARMED.with(|slot| {
            slot.borrow_mut().push(Armed::Vanish {
                path: path.to_owned(),
            })
        });
    }

    pub(crate) fn before_remove(path: &Path) {
        if take(|armed| matches!(armed, Armed::Vanish { path: target } if target == path)).is_some() {
            let _ = std::fs::remove_file(path);
        }
    }
```

`contents` 用到的 `Seek`/`Read` 放在该方法内部 `use`，模块顶部只留 `use std::io::{self, Write};`——非测试构建里没有 `Read` 的调用点，放到顶部会被 `-D warnings` 判为未使用。

- [ ] **Step 4: 运行，确认通过并做仓库标准校验。**

```powershell
cargo test -p ziyuan-runtime --locked --lib tools::temp_cleanup
cargo test -p ziyuan-runtime --locked --lib tools::replace::tests::
cargo fmt --all -- --check
cargo clippy -p ziyuan-runtime --all-targets --locked -- -D warnings
```

Expected: `tools::temp_cleanup` 22 passed（Windows；Unix 另计入 `symlinked_candidate_is_never_followed`，为 23）；`tools::replace::tests` 16 passed；fmt 与 clippy 通过。

- [ ] **Step 5: 提交。**

```powershell
jj commit -m "feat(runtime): 按租约归属决策临时文件与孤儿租约的去留" -m "Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>"
```

## Chunk 3: 目录扫描与报告

### Task 4: 目录扫描与报告

**Files:**
- Modify: `crates/ziyuan-runtime/src/tools/temp_cleanup.rs`

- [ ] **Step 1: 写失败测试。**

```rust
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
        let (dir, name, id) = sweep_fixture("sweep-counts");
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
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sweep_keeps_a_protected_candidate() {
        let (dir, name, _id) = sweep_fixture("sweep-protected");
        let mut protected = empty_protected();
        protected.insert(name.clone());
        let report = sweep_dir(&dir, "", &protected, true);
        assert_eq!(keep_reason(temp_item(&report, &name)), Reason::RecoveryReferenced);
        assert!(dir.join(&name).is_file());
        assert_eq!(report.counts(Kind::Temp).removed, 0);
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
```

- [ ] **Step 2: 运行，确认失败。**

```powershell
cargo test -p ziyuan-runtime --locked --lib tools::temp_cleanup
```

Expected: 编译失败，`cannot find function sweep_dir`。

- [ ] **Step 3: 写实现。**

```rust
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

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Report {
    pub mode: Mode,
    /// 扫描范围：本次从哪个根开始。
    pub root: PathBuf,
    pub recursive: bool,
    pub items: Vec<Item>,
    pub problems: Vec<Problem>,
    /// 读不了的目录：既不算扫描成功，也不中止整轮。
    pub unscanned: Vec<PathBuf>,
    /// 主动不进入的目录名（只报名字，报告仍不含绝对路径）。
    pub skipped: Vec<String>,
}

impl Report {
    fn new(mode: Mode, root: &Path, recursive: bool) -> Self {
        Self {
            mode,
            root: root.to_path_buf(),
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
                std::path::Component::Normal(part) => {
                    Some(part.to_string_lossy().into_owned())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    /// 扫描范围：`root` 相对自身的路径，根本身记作 `.`。
    pub fn scope(&self) -> String {
        let relative = self.display_relative(&self.root);
        if relative.is_empty() {
            ".".to_owned()
        } else {
            relative
        }
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
                "reason": match item.outcome {
                    Outcome::Remove => serde_json::Value::Null,
                    Outcome::Keep(reason) => json!(reason.as_str()),
                },
                "acted": item.acted,
            })).collect::<Vec<_>>(),
            "problems": self.problems.iter().map(|problem| json!({
                "path": self.display_relative(&problem.path),
                "message": problem.message,
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
/// 相对路径；它不是被扫目录内部的位置。
pub fn sweep_dir(dir: &Path, relative_dir: &str, protected: &BTreeSet<String>, apply: bool) -> Report {
    let mut report = Report::new(if apply { Mode::Apply } else { Mode::Preview }, dir, false);
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
    let mut names: Vec<String> = Vec::new();
    match fs::read_dir(dir) {
        Err(e) => {
            report.unscanned.push(dir.to_path_buf());
            report.problems.push(Problem { path: dir.to_path_buf(), message: e.to_string() });
            return;
        }
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Err(e) => report.problems.push(Problem { path: dir.to_path_buf(), message: e.to_string() }),
                    Ok(entry) => {
                        if let Some(name) = entry.file_name().to_str() {
                            names.push(name.to_owned());
                        }
                    }
                }
            }
        }
    }
    names.sort();
    let candidates: Vec<String> = names.iter().filter(|name| is_candidate_name(name)).cloned().collect();
    for name in candidates {
        let relative = join_relative(relative_dir, &name);
        let item = decide_candidate(dir, &name, &relative, protected, apply, &mut report.problems);
        report.items.push(item);
    }
    // 租约清单在候选处理之后再取一次：随候选一起被删掉的租约不该再报一条“已不存在”。
    let leases = match lease_entries(dir) {
        Ok(leases) => leases,
        Err(e) => {
            report.problems.push(Problem { path: dir.to_path_buf(), message: e.to_string() });
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
```

**同时删掉 Task 2 加在文件顶部的那行 `#![allow(dead_code)]`**：`sweep_dir` 是 `pub`，它调用 `scan_into` → `decide_candidate`/`decide_lease` → 其余私有函数，整条链从 crate 外可达，不再需要脚手架。删完仍要通过下面的 clippy，这才是它确实不再需要的证明。若 clippy 仍报某项未使用，说明那项真没有调用点，按报错删掉或补上调用，不要恢复 allow。

- [ ] **Step 4: 运行，确认通过并做仓库标准校验。**

```powershell
cargo test -p ziyuan-runtime --locked --lib tools::temp_cleanup
cargo test -p ziyuan-runtime --locked --lib tools::replace::tests::
cargo fmt --all -- --check
cargo clippy -p ziyuan-runtime --all-targets --locked -- -D warnings
```

Expected: `tools::temp_cleanup` 27 passed（Windows；Unix 另计入 `symlinked_candidate_is_never_followed`，为 28）；`tools::replace::tests` 16 passed（Windows；Unix 14）；fmt 与 clippy 通过，且已无 `allow(dead_code)`。

- [ ] **Step 5: 提交。**

```powershell
jj commit -m "feat(runtime): 扫描目录并逐项报告可清理的临时文件与孤儿租约" -m "Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>"
```

## Chunk 4: 接入真实写入路径（runtime）

### Task 5: 替换过程由租约覆盖

**Files:**
- Modify: `crates/ziyuan-runtime/src/tools/temp_cleanup.rs`（诊断通道、`tests_hooks` 增 `SwapNextLeaseToDir`）
- Modify: `crates/ziyuan-runtime/src/tools/replace.rs:1-8` 导入、`:50-76` `replace_target`、`:93-111` 删除 `create_temp`

- [ ] **Step 1: 写失败测试。**

追加一个诊断用例到 `temp_cleanup` 的 `mod tests`（`Lease::path` 在 Task 2 就已是普通 `pub fn`，不用再改）：

```rust
    #[test]
    fn problems_recorded_once_are_drained_once() {
        let _ = drain_problems();
        let path = std::path::PathBuf::from(".ziyuan-lease-probe");
        note_problem(path.clone(), "删除失败".into());
        let drained = drain_problems();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].path, path);
        assert_eq!(drained[0].message, "删除失败");
        assert!(drain_problems().is_empty(), "取走后不得重复上报");
    }
```

再在 `replace.rs` 的 `mod tests` 追加。已有的 `names()` 辅助会排序并列出目录项，直接用它（不要定义同名局部变量）：

```rust
    fn lease_names(dir: &Path) -> Vec<String> {
        names(dir)
            .into_iter()
            .filter(|name| name.starts_with(".ziyuan-lease-"))
            .collect()
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
        let dir = scratch("rename-window");
        let target = dir.join("target.txt");
        std::fs::write(&target, OLD).unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let slot = seen.clone();
        // `before_rename` 在 `fill` 关闭临时文件之后、改名前调用：正是设计要求的窗口。
        let _guard = install(Faults {
            before_rename: Some(Box::new(move |target: &Path| {
                let leases = lease_names(target.parent().unwrap());
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
        super::super::temp_cleanup::tests_hooks::arm_swap_next_lease_to_dir();
        let result = replace_target(&target, NEW.as_bytes());
        super::super::temp_cleanup::tests_hooks::clear();
        assert!(result.is_ok(), "删租约失败不得报替换失败: {result:?}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), NEW);
        let problems = super::super::temp_cleanup::drain_problems();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].path.to_string_lossy().contains(".ziyuan-lease-"),
            "{problems:?}"
        );
        // 把换名成目录的租约清掉，供后面的用例复用目录无关的假设。
        for name in lease_names(&dir) {
            let _ = std::fs::remove_dir_all(dir.join(name));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
```

并改已有 `assert_fault_keeps_target`：在断言目录内容之外加一行 `assert!(lease_names(&dir).is_empty(), "失败路径不得留下租约");`。

- [ ] **Step 2: 运行，确认失败。**

```powershell
cargo test -p ziyuan-runtime --locked --lib tools::
```

Expected: 编译失败，`drain_problems` 与 `arm_swap_next_lease_to_dir` 不存在（后者要到 Step 3 的 `tests_hooks` 扩展里加）。这一步只跑一次编译，确认新用例确实还没被满足。

- [ ] **Step 3: 写实现。**

`temp_cleanup.rs`：在 `Problem` 定义之后加诊断通道，并给 `tests_hooks` 补 `SwapNextLeaseToDir`（`arm_swap_next_lease_to_dir` 只在 `replace.rs` 的用例里用；它不知道租约路径，所以要“下一次 `release` 关句柄后换目录”）：

```rust
    /// 下一次 `release` 关句柄后换目录，不预先知道路径。
    SwapNextLeaseToDir,
```

```rust
    /// 给 `replace.rs` 的用例用：租约路径由 `hold` 生成，事先不知道。
    pub(crate) fn arm_swap_next_lease_to_dir() {
        ARMED.with(|slot| slot.borrow_mut().push(Armed::SwapNextLeaseToDir));
    }
```

`after_close` 开头补一段优先匹配：

```rust
    pub(crate) fn after_close(path: &Path) {
        if take(|armed| matches!(armed, Armed::SwapNextLeaseToDir)).is_some() {
            swap(path);
            return;
        }
        if let Some(Armed::SwapToDir { path: Some(target) }) =
            take(|armed| matches!(armed, Armed::SwapToDir { path: Some(target) } if target == path))
        {
            let _ = target;
            swap(path);
        }
    }
```

```rust
use std::cell::RefCell;

thread_local! {
    /// 不便沿返回值上抛的失败：例如改名已成功却删不掉租约。按线程隔离，
    /// 由调用方（`apply_changes_protected`、CLI）取走并报到 stderr。
    static PROBLEMS: RefCell<Vec<Problem>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn note_problem(path: PathBuf, message: String) {
    PROBLEMS.with(|problems| problems.borrow_mut().push(Problem { path, message }));
}

/// 取走当前线程累积的失败；空表示没有。
pub fn drain_problems() -> Vec<Problem> {
    PROBLEMS.with(|problems| std::mem::take(&mut *problems.borrow_mut()))
}
```

`replace.rs` 顶部：删掉 `use std::sync::atomic::{AtomicU64, Ordering};` 与 `SystemTime, UNIX_EPOCH`（`AtomicU64`/`Ordering` 只在测试里用，改到 `mod tests` 里的 `use std::sync::atomic::{AtomicU64, Ordering};`）；时间只留 `Duration, Instant`。

`replace_target` 改成：

```rust
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
    let file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
    {
        Ok(file) => file,
        Err(e) => {
            release_quietly(lease);
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
            release_quietly(lease);
            Ok(())
        }
        Err(primary) => {
            let error = discard(&temp, primary);
            release_quietly(lease);
            Err(error)
        }
    }
}

/// 放弃租约但不上抛它的错误：此刻真正要报告的是原始失败或“替换已完成”。
fn release_quietly(lease: super::temp_cleanup::Lease) {
    let path = lease.path().to_path_buf();
    if let Err(e) = lease.release() {
        super::temp_cleanup::note_problem(path, e.to_string());
    }
}
```

删除整个 `create_temp`（命名与创建都并入上面的流程）。

- [ ] **Step 4: 运行，确认通过并做仓库标准校验。**

```powershell
cargo test -p ziyuan-runtime --locked --lib tools::
cargo test -p ziyuan-runtime --locked --lib
cargo fmt --all -- --check
cargo clippy -p ziyuan-runtime --all-targets --locked -- -D warnings
```

Expected: `tools::replace::tests` 19 passed（16 + 3；Unix 17）；`tools::temp_cleanup` 28 passed（27 + 1；Unix 29）；fmt 与 clippy 通过。

- [ ] **Step 5: 提交。**

```powershell
jj commit -m "feat(runtime): 用租约覆盖整个替换过程并在删不掉时留下诊断" -m "Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>"
```

### Task 6: 写入前自动清理目标目录

**Files:**
- Modify: `crates/ziyuan-runtime/src/tools.rs:24`（`pub mod temp_cleanup;` 已在 Task 1 加）、`:220-239`（`apply_changes`）

- [ ] **Step 1: 写失败测试。**

追加到 `tools.rs` 的 `mod tests`：

```rust
    /// 造一份“已死写入者”的租约与它认领的临时文件。
    fn stale_temp(dir: &Path) -> PathBuf {
        let id = "0123456789abcdef0123456789abcdef";
        let temp = dir.join(".ziyuan-tmp-1-1-1");
        fs::write(&temp, b"partial").unwrap();
        fs::write(
            dir.join(format!(".ziyuan-lease-{id}")),
            format!("ziyuan-lease v1 temp={} id={id}\n", temp.file_name().unwrap().to_string_lossy()),
        )
        .unwrap();
        temp
    }

    fn change(path: &str, after: &str) -> FileChange {
        FileChange {
            path: path.into(),
            before: None,
            after: after.into(),
        }
    }

    #[test]
    fn apply_changes_sweeps_a_stale_temp_next_to_the_target() {
        let dir = workspace("sweep-stale");
        let project = dir.join("project");
        let stale = stale_temp(&project.join("src"));
        let problems = apply_changes_protected(
            &project,
            &[change("src/new.rs", "fn main() {}\n")],
            &BTreeSet::new(),
        )
        .unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert!(!stale.exists(), "写入前应清掉同目录的陈旧临时文件");
        assert_eq!(
            fs::read_to_string(project.join("src/new.rs")).unwrap(),
            "fn main() {}\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn apply_changes_keeps_a_protected_temp() {
        let dir = workspace("sweep-protected");
        let project = dir.join("project");
        let stale = stale_temp(&project.join("src"));
        let name = stale.file_name().unwrap().to_string_lossy().into_owned();
        let protected: BTreeSet<String> = [format!("src/{name}")].into_iter().collect();
        apply_changes_protected(&project, &[change("src/new.rs", "x\n")], &protected).unwrap();
        assert!(stale.exists(), "受保护路径不得删除");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn apply_changes_leaves_other_directories_alone() {
        let dir = workspace("sweep-other");
        let project = dir.join("project");
        let elsewhere = stale_temp(&project.join("docs"));
        apply_changes_protected(&project, &[change("src/new.rs", "x\n")], &BTreeSet::new())
            .unwrap();
        assert!(elsewhere.exists(), "自动清理只碰目标所在目录");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn apply_changes_still_writes_when_the_sweep_finds_nothing() {
        let dir = workspace("sweep-clean");
        let project = dir.join("project");
        let problems =
            apply_changes_protected(&project, &[change("src/a.rs", "a\n")], &BTreeSet::new())
                .unwrap();
        assert!(problems.is_empty());
        assert_eq!(fs::read_to_string(project.join("src/a.rs")).unwrap(), "a\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn plain_apply_changes_never_sweeps() {
        // 不掌握受保护集合的调用方不该顺手清理：宁可不删。
        let dir = workspace("sweep-plain");
        let project = dir.join("project");
        let stale = stale_temp(&project.join("src"));
        apply_changes(&project, &[change("src/new.rs", "x\n")]).unwrap();
        assert!(stale.exists(), "apply_changes 不做清理");
        assert!(project.join("src/new.rs").is_file());
        fs::remove_dir_all(dir).unwrap();
    }
```

- [ ] **Step 2: 运行，确认失败。**

```powershell
cargo test -p ziyuan-runtime --locked --lib tools::tests::apply_changes_sweeps
```

Expected: 编译失败，`cannot find function apply_changes_protected`。

- [ ] **Step 3: 写实现。**

`tools.rs` 顶部加 `use std::collections::BTreeSet;`（若已通过 `use` 引入则跳过），并改 `apply_changes`：

```rust
/// 把改动写入项目，**不做**临时文件清理。
///
/// 需要“写入前清掉同目录陈旧临时文件”的调用方一律用 `apply_changes_protected`：
/// 只有它能拿到调用方从恢复记录构造的受保护集合。这里保持无清理行为，是为了不让
/// 不知道受保护集合的调用方（包括测试与将来的调用者）在有待恢复记录的工作区里
/// 扫掉不该扫的东西——设计明确“集合无法构造时整轮不删”。
pub fn apply_changes(root: &Path, changes: &[FileChange]) -> Result<(), BoxError> {
    let mut problems = Vec::new();
    write_all(root, changes, None, &mut problems)
}

/// 同 `apply_changes`，另在写每个目标之前清掉该目录里可证明陈旧的临时文件。
///
/// `protected` 是调用方从恢复记录构造的工作区相对路径集合；集合构造失败时调用方
/// 不应调用本函数（宁可不清理），传空集合等于“没有受保护项”。
/// 自动清理绝不阻断写入：任何清理失败都只进返回的诊断，写入照旧。
pub fn apply_changes_protected(
    root: &Path,
    changes: &[FileChange],
    protected: &BTreeSet<String>,
) -> Result<Vec<temp_cleanup::Problem>, BoxError> {
    // 先取走上次遗留的诊断：线程局部状态不能跨调用漂移，否则失败会被算到别人头上。
    let mut problems = temp_cleanup::drain_problems();
    let outcome = write_all(root, changes, Some(protected), &mut problems);
    // 无论成功还是提前返回，本线程累积的失败都要交出去，不能留在 thread_local 里。
    problems.extend(temp_cleanup::drain_problems());
    outcome.map(|()| problems)
}

/// `protected` 为 `None` 时整轮不清理（给不掌握受保护集合的调用方用）。
fn write_all(
    root: &Path,
    changes: &[FileChange],
    protected: Option<&BTreeSet<String>>,
    problems: &mut Vec<temp_cleanup::Problem>,
) -> Result<(), BoxError> {
    let root = root.canonicalize()?;
    let mut targets = Vec::new();
    for change in changes {
        let (state, target) = base_state_in(&root, change)?;
        match state {
            BaseState::Written => {}
            BaseState::Original => targets.push((target, &change.after)),
            BaseState::Changed => {
                return Err(format!("{} 自呈报以来已被改动，拒绝落盘", change.path).into());
            }
        }
    }
    for (target, after) in targets {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
            if let Some(protected) = protected {
                // 只清本次写入目标所在目录；不取工作目录锁，调用方本就持锁。
                let relative_parent = relative_dir(&root, parent);
                let report = temp_cleanup::sweep_dir(parent, &relative_parent, protected, true);
                problems.extend(report.problems);
            }
        }
        replace::replace_target(&target, after.as_bytes())?;
    }
    Ok(())
}

/// 工作区相对目录路径（`/` 分隔）；根目录是空串，与受保护集合同一形式。
fn relative_dir(root: &Path, dir: &Path) -> String {
    dir.strip_prefix(root)
        .map(|rel| {
            rel.components()
                .filter_map(|component| match component {
                    Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_default()
}
```

- [ ] **Step 4: 运行，确认通过并做仓库标准校验。**

```powershell
cargo test -p ziyuan-runtime --locked --lib tools::
cargo test -p ziyuan-runtime --locked
cargo fmt --all -- --check
cargo clippy -p ziyuan-runtime --all-targets --locked -- -D warnings
```

Expected: `tools::tests` 新增 5 个通过，`tools::replace::tests` 19 passed（Unix 17），`tools::temp_cleanup` 28 passed（Unix 29）；fmt 与 clippy 通过。注意 `plain_apply_changes_never_sweeps` 是反向保护：它证明 `apply_changes` 保持无清理行为，不是走个过场。

- [ ] **Step 5: 提交。**

```powershell
jj commit -m "feat(runtime): 每次落盘前清理目标目录里可证明陈旧的临时文件" -m "Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>"
```

## Chunk 5: 整树扫描与两条入口

### Task 7: 递归扫描工作目录

**Files:**
- Modify: `crates/ziyuan-runtime/src/tools/temp_cleanup.rs`

- [ ] **Step 1: 写失败测试。**

```rust
    #[test]
    fn workspace_sweep_reaches_nested_directories() {
        let dir = scratch("tree");
        let nested = dir.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        let target = fresh_temp_name();
        write_temp(&nested, &target);
        let id = hex_id(41);
        dead_lease(&nested, &id, &target);
        let report = sweep_workspace(&dir, &empty_protected(), true);
        assert!(report.recursive);
        assert_eq!(report.counts(Kind::Temp).removed, 1, "{report:?}");
        assert!(!nested.join(&target).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn workspace_sweep_protects_nested_relative_paths() {
        let dir = scratch("tree-protected");
        let nested = dir.join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        let name = fresh_temp_name();
        write_temp(&nested, &name);
        let mut protected = empty_protected();
        protected.insert(format!("sub/{name}"));
        let report = sweep_workspace(&dir, &protected, true);
        assert_eq!(keep_reason(temp_item(&report, &name)), Reason::RecoveryReferenced);
        assert!(nested.join(&name).is_file());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn workspace_sweep_skips_control_and_build_directories() {
        let dir = scratch("tree-skip");
        for skipped in [".ziyuan", ".git", ".jj", "target", "node_modules"] {
            let sub = dir.join(skipped);
            std::fs::create_dir_all(&sub).unwrap();
            let name = fresh_temp_name();
            write_temp(&sub, &name);
            dead_lease(&sub, &hex_id(42), &name);
        }
        let report = sweep_workspace(&dir, &empty_protected(), true);
        assert!(report.items.is_empty(), "{report:?}");
        for skipped in [".ziyuan", ".git", ".jj", "target", "node_modules"] {
            assert_eq!(std::fs::read_dir(dir.join(skipped)).unwrap().count(), 2);
        }
        // 跳过不等于“没发生”：报告必须写明跳过了哪些，不能声称扫全了。
        assert_eq!(
            report.skipped,
            vec![
                ".git".to_string(),
                ".jj".to_string(),
                ".ziyuan".to_string(),
                "node_modules".to_string(),
                "target".to_string(),
            ],
            "{report:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn two_cleaners_never_double_count_a_single_candidate() {
        let (dir, name, _id) = sweep_fixture("tree-race");
        // 两个清理者并发扫同一棵树：谁先删都一样，合计只能记一次删除。
        let root = std::sync::Arc::new(dir.clone());
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let root = root.clone();
                std::thread::spawn(move || sweep_workspace(&root, &empty_protected(), true))
            })
            .collect();
        let reports: Vec<Report> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let temps_removed: usize = reports
            .iter()
            .map(|report| report.counts(Kind::Temp).removed)
            .sum();
        assert_eq!(temps_removed, 1, "同一候选只能被记一次删除");
        assert!(!dir.join(&name).exists());
        assert!(lease_paths(&dir).is_empty());
        for report in &reports {
            // 落败的一方把“已被别人删掉”当成功收尾，不报失败。
            assert!(report.problems.is_empty(), "{report:?}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_reused_pid_without_a_lease_is_never_removed() {
        let dir = scratch("tree-reuse");
        // 旧版遗留、无租约：即使名字格式合法也不动（PID 只作诊断）。
        let name = ".ziyuan-tmp-4242-1-1";
        write_temp(&dir, name);
        let report = sweep_workspace(&dir, &empty_protected(), true);
        assert_eq!(keep_reason(temp_item(&report, name)), Reason::TempWithoutLease);
        assert!(dir.join(name).is_file(), "无归属证据的文件绝不删除");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn workspace_sweep_never_follows_a_linked_directory() {
        let dir = scratch("tree-link");
        let outside = dir.with_extension("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let name = fresh_temp_name();
        write_temp(&outside, &name);
        dead_lease(&outside, &hex_id(43), &name);
        std::os::unix::fs::symlink(&outside, dir.join("link")).unwrap();
        let report = sweep_workspace(&dir, &empty_protected(), true);
        assert!(report.items.is_empty(), "{report:?}");
        assert!(outside.join(&name).is_file(), "链接目标不得被清理");
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }
```

- [ ] **Step 2: 运行，确认失败。**

```powershell
cargo test -p ziyuan-runtime --locked --lib tools::temp_cleanup::tests::workspace_sweep
```

Expected: 编译失败，`cannot find function sweep_workspace`。

- [ ] **Step 3: 写实现。**

```rust
/// 不进入的目录：控制目录与常见构建产物。跳过的不止 `.ziyuan`，所以必须记进
/// `report.skipped` 并在两种输出里列出，否则“已扫全”就是假话。
const SKIPPED_DIRS: [&str; 5] = [".ziyuan", ".git", ".jj", "target", "node_modules"];

/// 手工入口用：从工作区根开始递归清理。
pub fn sweep_workspace(root: &Path, protected: &BTreeSet<String>, apply: bool) -> Report {
    let mut report = Report::new(if apply { Mode::Apply } else { Mode::Preview }, root, true);
    walk(&mut report, root, "", protected, apply);
    report.skipped.sort();
    report.skipped.dedup();
    report
}

/// 深度优先；子目录用不跟随链接的元数据判断，链接与 junction 一律不进入。
fn walk(
    report: &mut Report,
    dir: &Path,
    relative_dir: &str,
    protected: &BTreeSet<String>,
    apply: bool,
) {
    scan_into(report, dir, relative_dir, protected, apply);
    let Ok(entries) = fs::read_dir(dir) else {
        // `scan_into` 已把该目录记进 `unscanned`。
        return;
    };
    let mut children = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            // 名字不是 UTF-8：读不进受保护集合的比较，也拼不出相对路径。
            // 静默跳过会让报告声称扫全了，所以显式记一条。
            report.problems.push(Problem {
                path: entry.path(),
                message: "目录项名称不是 UTF-8，未能纳入扫描".into(),
            });
            continue;
        };
        if SKIPPED_DIRS.contains(&name) {
            report.skipped.push(name.to_owned());
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            report.problems.push(Problem {
                path: entry.path(),
                message: "无法读取目录项元数据".into(),
            });
            continue;
        };
        // `is_dir()` 对符号链接返回 false（跟随后才为真），junction/reparse 同理
        // 不满足普通目录判定；再显式排除链接，双保险。
        let file_type = meta.file_type();
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        children.push((entry.path(), join_relative(relative_dir, name)));
    }
    children.sort();
    for (path, relative) in children {
        walk(report, &path, &relative, protected, apply);
    }
}
```

`skipped` 里排的是名字、不是路径，排序结果与遍历顺序无关；这也是用例能断言固定顺序的原因。

- [ ] **Step 4: 运行并做仓库标准校验。**

```powershell
cargo test -p ziyuan-runtime --locked --lib tools::temp_cleanup
cargo fmt --all -- --check
cargo clippy -p ziyuan-runtime --all-targets --locked -- -D warnings
```

Expected: `tools::temp_cleanup` 33 passed（Windows；Unix 另计入 `workspace_sweep_never_follows_a_linked_directory`，为 34）；fmt 与 clippy 通过。

- [ ] **Step 5: 提交。**

```powershell
jj commit -m "feat(runtime): 从工作区根递归扫描并跳过控制目录与链接" -m "Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>"
```

### Task 8: CLI `text recovery temps`

**Files:**
- Modify: `crates/ziyuan-cli/src/text_commands.rs:21-40`（`TEXT_HELP`）、`:61`（用法串）、`:64-70`（枚举）、`:177-194`（解析）、`:349`（`run_recovery` 分支）
- Modify: `crates/ziyuan-cli/src/help.rs:306` 与 `:342`（两处 `text` 子命令补全）

- [ ] **Step 1: 改解析、用法与帮助文本，先写出期望（编译期即失败）。**

用法串改成：

```rust
const RECOVERY_USAGE: &str = "用法: ziyuan text recovery status [--json] | ziyuan text recovery finish <操作ID> --yes | ziyuan text recovery abandon <操作ID> --yes | ziyuan text recovery temps [--json] [--yes]";
```

枚举加：

```rust
    /// 清理遗留的 `.ziyuan-tmp-*`：默认只预览，`--yes` 才删。
    Temps { apply: bool },
```

`parse_recovery` 加两条分支（`--json` 已由 `main` 全局提取，不会出现在这里）：

```rust
        [action] if action == "temps" => RecoveryAction::Temps { apply: false },
        [action, yes] if action == "temps" && yes == "--yes" => RecoveryAction::Temps { apply: true },
```

`TEXT_HELP` 在三条 `recovery` 说明之后补一段（保持缩进与既有条目一致）：

```text
  ziyuan text recovery temps [--yes]
                                       列出工作目录里遗留的 .ziyuan-tmp-* 与其租约；
                                       默认只预览，加 --yes 才删除可证明陈旧的项
```

`help.rs` 有两处 `text` 子命令列表，都要补 `recovery`（第 306 行的 bash/zsh 列表与第 342 行的 fish 列表；只补一处会让一种 shell 的补全缺项）：

```rust
// 第 306 行附近
"text") words="init import show check recovery translate review revise zhupi finalize reopen context approve status resume help"
// 第 342 行附近
                format!("{} -a 'init import show check recovery translate review revise zhupi finalize reopen context approve status resume help'", seen("text")),
```

动手前先打开 `crates/ziyuan-cli/src/help.rs:300-345` 核对这两处的实际写法（可能是 `words=`、可能是 `-a` 列表），按既有格式补词，不要照抄这里的示例。

- [ ] **Step 2: 运行，确认失败。**

```powershell
cargo check -p ziyuan-cli --locked --all-targets
```

Expected: 编译失败，`run_recovery` 的 `match` 未覆盖 `Temps`。

- [ ] **Step 3: 写实现。**

`run_recovery` 开头加分支：

```rust
        RecoveryAction::Temps { apply } => return run_temps(&workspace, apply, json),
```

新增（签名写成单行：多行会被 `cargo fmt` 折回，导致 `--check` 失败）：

```rust
/// 手工清理遗留临时文件。默认预览；`--yes` 才真删。
///
/// 不取工作目录锁：那会把正在写入的进程挡到超时，而非阻塞试探已经足够给出可信判定。
fn run_temps(workspace: &Workspace, apply: bool, json: bool) -> Result<(), Box<dyn Error>> {
    use ziyuan_runtime::tools::temp_cleanup::{Kind, Outcome};
    // 集合构造失败就整轮不清理：宁可什么都不做，也不要误删恢复记录引用的项。
    let protected = protected_targets(workspace)?;
    let report = ziyuan_runtime::tools::temp_cleanup::sweep_workspace(
        workspace.root(),
        &protected,
        apply,
    );
    if json {
        print_json(report.to_json())?;
    } else {
        let temps = report.counts(Kind::Temp);
        let leases = report.counts(Kind::Lease);
        println!("范围：{}（含子目录）", report.scope());
        println!("模式：{}", if apply { "删除" } else { "预览，加 --yes 才删" });
        println!(
            "临时文件：候选 {}，可清理 {}，已删除 {}，保留 {}",
            temps.candidates, temps.removable, temps.removed, temps.kept
        );
        println!(
            "租约：候选 {}，可清理 {}，已删除 {}，保留 {}",
            leases.candidates, leases.removable, leases.removed, leases.kept
        );
        if !report.skipped.is_empty() {
            println!("未进入的目录（按名字跳过）：{}", report.skipped.join("、"));
        }
        for item in &report.items {
            let why = match item.outcome {
                Outcome::Remove => {
                    if item.acted { "已删除" } else { "可清理" }
                }
                Outcome::Keep(reason) => reason.as_str(),
            };
            // 报告只出相对路径：绝对路径不外泄。
            println!("  {why:<22} {}", report.display_relative(&item.path));
        }
        for problem in &report.problems {
            println!(
                "  失败：{}：{}",
                report.display_relative(&problem.path),
                problem.message
            );
        }
        for dir in &report.unscanned {
            println!("  读不了的目录：{}", report.display_relative(dir));
        }
    }
    // 只有扫描/删除的 I/O 失败与读不了的目录才退 1；预览发现可清理项仍算成功，
    // 报告里的“可清理 N”就是待办提示（设计：仅保留活跃或未知项不算执行失败）。
    if report.needs_attention() {
        return Err("临时文件清理未能完成；见上面的逐项报告".into());
    }
    Ok(())
}

/// 受保护集合：恢复记录里的全部目标路径（`/` 分隔的工作区相对路径）。
fn protected_targets(
    workspace: &Workspace,
) -> Result<std::collections::BTreeSet<String>, Box<dyn Error>> {
    let mut protected = std::collections::BTreeSet::new();
    if let Some(record) = workspace.pending_record()? {
        for target in &record.targets {
            protected.insert(target.path.clone());
        }
    }
    Ok(protected)
}
```

`Workspace` 与 `pending_record` 已在本文件用到，无需新导入；`ziyuan_runtime` 亦已是依赖。

- [ ] **Step 4: 手工验证并做仓库标准校验。**

```powershell
$repo = $PWD.Path
$tmp = Join-Path $env:TEMP "ziyuan-temps-manual"
Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $tmp | Out-Null
Set-Content -Path (Join-Path $tmp ".ziyuan-tmp-1-1-1") -Value "partial"
# 必须 -NoNewline 且自己写 `n：Set-Content 默认补 CRLF，租约行会变成 "…<id>\r\n"，
# 解析时 id 带上 \r 就不再是 32 位十六进制，结果成了 LeaseUnparsable。
$lease = "ziyuan-lease v1 temp=.ziyuan-tmp-1-1-1 id=0123456789abcdef0123456789abcdef`n"
Set-Content -NoNewline -Path (Join-Path $tmp ".ziyuan-lease-0123456789abcdef0123456789abcdef") -Value $lease
Push-Location $tmp
# cwd 已经离开仓库，必须显式 --manifest-path，否则找不到 Cargo.toml。
cargo run -q -p ziyuan-cli --locked --manifest-path (Join-Path $repo "Cargo.toml") -- text recovery temps --json
"预览退出码: $LASTEXITCODE"
cargo run -q -p ziyuan-cli --locked --manifest-path (Join-Path $repo "Cargo.toml") -- text recovery temps --yes --json
"删除退出码: $LASTEXITCODE"
Get-ChildItem -Force $tmp
Pop-Location
Remove-Item -Recurse -Force $tmp
cargo fmt --all
cargo fmt --all -- --check
cargo clippy -p ziyuan-cli --all-targets --locked -- -D warnings
```

Expected: 预览报 `mode=preview`、`temps.removable=1`、`temps.removed=0`、**退出码 0**（预览不是失败），两个文件都还在；`--yes` 后 `removed=1`、退出码 0、目录为空。用 `cargo run` 而不是裸 `ziyuan`：PATH 上可能是旧的 `cargo install` 版本，验出来的行为会不对应本仓库代码。

- [ ] **Step 5: 提交。**

```powershell
jj commit -m "feat(cli): 新增 text recovery temps 预览与清理遗留临时文件" -m "Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>"
```

### Task 9: 受管写入把恢复记录目标交给清理器

**Files:**
- Modify: `crates/ziyuan-app/src/edicts.rs:794-796`
- Modify: `crates/ziyuan-app/src/write_txn_tests.rs`（追加一个用例；该模块由 `lib.rs:131` 声明）

- [ ] **Step 1: 写失败测试。**

追加到 `crates/ziyuan-app/src/write_txn_tests.rs`。那里已有 `Root`、`prepared()`、`apply_reviewed` 与 `Root::temps()`，直接用：

```rust
#[tokio::test]
async fn managed_write_sweeps_a_stale_temp_next_to_the_target() {
    let root = Root::new("sweep-stale");
    // 已死写入者的残留：一份合法租约，加上它认领的临时文件，与目标同目录。
    let stale = root.path().join(".ziyuan-tmp-1-1-1");
    std::fs::write(&stale, "partial").unwrap();
    std::fs::write(
        root.path()
            .join(".ziyuan-lease-0123456789abcdef0123456789abcdef"),
        "ziyuan-lease v1 temp=.ziyuan-tmp-1-1-1 \
         id=0123456789abcdef0123456789abcdef\n",
    )
    .unwrap();

    let (mut session, task) = prepared().await;
    session
        .apply_reviewed(task, root.path(), Vec::new(), None)
        .unwrap();

    assert_eq!(root.read(), CHANGED);
    assert!(!stale.exists(), "写入前应清掉目标目录里的陈旧临时文件");
    assert!(root.temps().is_empty(), "{:?}", root.temps());
    let leases: Vec<String> = std::fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !leases.iter().any(|name| name.starts_with(".ziyuan-lease-")),
        "{leases:?}"
    );
}
```

`Root::temps()` 只找 `.ziyuan-tmp-` 与 `.tmp`，租约要单独断言。

- [ ] **Step 2: 运行，确认失败。**

```powershell
cargo test -p ziyuan-app --locked managed_write_sweeps_a_stale_temp
```

Expected: 1 failed，`写入前应清掉目标目录里的陈旧临时文件`——`apply_changes` 还没做清理。过滤词必须与用例名一致，否则 cargo 会以 0 passed 退出，把 RED 步骤伪装成通过。

- [ ] **Step 3: 写实现。**

把

```rust
        ziyuan_runtime::tools::apply_changes(root, selected)
            .map_err(|e| AppError::Apply(e.to_string()))?;
```

换成

```rust
        // 受保护集合就是本次记录写下的全部目标：清理器据此跳过仍被记录引用的项。
        let protected: std::collections::BTreeSet<String> =
            record.targets.iter().map(|t| t.path.clone()).collect();
        let problems =
            ziyuan_runtime::tools::apply_changes_protected(root, selected, &protected)
                .map_err(|e| AppError::Apply(e.to_string()))?;
        // 清理失败不阻断写入：落盘结果照旧，诊断只走 stderr。
        for problem in problems {
            eprintln!(
                "警告：未能清理临时文件 {}：{}",
                problem.path.display(),
                problem.message
            );
        }
```

- [ ] **Step 4: 运行并做仓库标准校验。**

```powershell
cargo test -p ziyuan-app --locked
cargo fmt --all -- --check
cargo clippy -p ziyuan-app --all-targets --locked -- -D warnings
```

Expected: 全部通过（新用例由失败转通过）。

- [ ] **Step 5: 提交。**

```powershell
jj commit -m "feat(app): 受管写入把恢复记录目标作为受保护集合交给清理器" -m "Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>"
```

## Chunk 6: 契约与文档

### Task 10: 黑盒契约测试

**Files:**
- Modify: `crates/ziyuan-cli/tests/contract.rs`

- [ ] **Step 1: 写测试。**

```rust
/// 造一个“写入者已死”的工作目录：临时文件加一份认领它的租约。
fn stale_temp_workspace(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("ziyuan-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let temp = dir.join(".ziyuan-tmp-4242-1-1");
    std::fs::write(&temp, b"partial").unwrap();
    std::fs::write(
        dir.join(".ziyuan-lease-0123456789abcdef0123456789abcdef"),
        "ziyuan-lease v1 temp=.ziyuan-tmp-4242-1-1 id=0123456789abcdef0123456789abcdef\n",
    )
    .unwrap();
    (dir, temp)
}

#[test]
fn text_recovery_temps_previews_then_deletes() {
    let (dir, temp) = stale_temp_workspace("recovery-temps");
    let preview = ziyuan(&dir, &["text", "recovery", "temps", "--json"]);
    // 预览不是失败：设计写明“仅保留活跃或未知项不算执行失败”，可清理数写在报告里。
    assert_eq!(preview.status.code(), Some(0), "{:?}", preview.stdout);
    let report: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(report["mode"], "preview");
    assert_eq!(report["scope"], ".");
    assert_eq!(report["temps"]["removable"], 1);
    assert_eq!(report["temps"]["removed"], 0);
    // 报告里只出现相对路径，不泄露绝对路径。
    assert_eq!(report["items"][0]["path"], ".ziyuan-tmp-4242-1-1");
    assert!(
        !String::from_utf8_lossy(&preview.stdout).contains(r"\\?\"),
        "报告不得包含绝对路径"
    );
    assert!(temp.is_file(), "预览不得删除");

    let applied = ziyuan(&dir, &["text", "recovery", "temps", "--yes", "--json"]);
    assert_eq!(applied.status.code(), Some(0), "{:?}", applied.stdout);
    let report: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(report["mode"], "apply");
    assert_eq!(report["temps"]["removed"], 1);
    assert!(!temp.exists(), "确认后应删除");
    assert!(!dir.join(".ziyuan-lease-0123456789abcdef0123456789abcdef").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn text_recovery_temps_keeps_a_temp_without_a_lease() {
    // 旧版遗留或 PID 复用：名字合法但没有租约，没有归属证据，一律保留。
    let dir = std::env::temp_dir().join(format!("ziyuan-cli-temps-reuse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let orphan = dir.join(".ziyuan-tmp-4242-1-1");
    std::fs::write(&orphan, b"partial").unwrap();
    let out = ziyuan(&dir, &["text", "recovery", "temps", "--yes", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{:?}", out.stdout);
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["temps"]["kept"], 1);
    assert_eq!(report["temps"]["removed"], 0);
    assert_eq!(report["items"][0]["reason"], "TempWithoutLease");
    assert!(orphan.is_file(), "无归属证据的文件绝不删除");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn text_recovery_temps_is_clean_on_an_empty_workspace() {
    let dir = std::env::temp_dir().join(format!("ziyuan-cli-temps-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = ziyuan(&dir, &["text", "recovery", "temps", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["temps"]["candidates"], 0);
    assert_eq!(report["leases"]["candidates"], 0);
    assert_eq!(report["removed_total"], 0);
    std::fs::remove_dir_all(dir).unwrap();
}
```

若 `contract.rs` 未依赖 `serde_json`：该 crate 已在 `Cargo.toml` 里作为依赖（`print_json` 用到），测试里可直接 `use serde_json`。

`stale_temp_workspace` 造的临时名必须让报告顺序可预测：目录里只有一个候选与一个租约，`items[0]` 必是候选（`scan_into` 先处理候选、再处理租约）。若实现改了顺序，这里的断言会直接指出。

- [ ] **Step 2: 运行并确认它们不是空转。**

```powershell
cargo test -p ziyuan-cli --locked --test contract text_recovery_temps
```

Expected: 三条都通过（Task 8 已经交付实现，这里没有新的 RED 可言）。它们的价值在于能抓住旧行为的两种回归：预览若退 1 立刻红（`preview.status.code()`），报告若给绝对路径也立刻红（`items[0].path` 与 `\\?\` 检查）。真红了就改实现，不要改断言。

- [ ] **Step 3: 跑全量契约测试。**

```powershell
cargo test -p ziyuan-cli --locked --test contract
```

Expected: 全部通过，原有用例不受新增子命令影响。

- [ ] **Step 4: 提交。**

```powershell
jj commit -m "test(cli): 补齐 text recovery temps 的黑盒契约" -m "Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>"
```

### Task 11: 用户文档、稳定性说明与验收记录

**Files:**
- Modify: `docs/text-projects.md`
- Modify: `docs/stability.md`
- Modify: `CHANGELOG.md`
- Modify: `docs/roadmap.md`
- Create: `docs/acceptance/1.2.2.md`

- [ ] **Step 1: 写文档。**

`docs/text-projects.md` 增加一节“遗留临时文件的清理”，说明：

- 原子替换会在目标目录短暂出现 `.ziyuan-tmp-<pid>-<nanos>-<count>` 与配套的 `.ziyuan-lease-<id>`；正常结束时会自行删除。
- 进程被强杀后可能留下这两类文件；正常写入时只清理本次目标所在目录里可证明陈旧的临时文件。
- 手工入口：`ziyuan text recovery temps`（只预览）与 `ziyuan text recovery temps --yes`（真删）；`--json` 输出机器可读报告。
- 退出码：**0 表示本轮完成**（包括预览里发现可清理项——那不是失败，报告会写明 `可清理 N`）；1 表示有扫描／删除的 I/O 失败或读不了的目录；2 是用法错误。
- 扫描范围是工作区根及其子目录，但**按名字跳过** `.ziyuan`、`.git`、`.jj`、`target`、`node_modules`，并在报告里列出，不声称扫全。
- 报告里的路径一律是工作区相对路径，不含绝对路径。
- 什么不会被删：活跃写入者的（租约被占用）、符号链接与 junction、目录、命名不合格式的、恢复记录引用的、以及租约读不懂的目录里的候选。

`docs/stability.md` 增加 1.2.2 条目：

- 新增控制文件 `.ziyuan-lease-<id>`（可与临时文件一起重建，不属用户数据）；`translate` 等已有命令的行为与输出字段不变。
- 清理只删可证明陈旧的临时文件；`--yes` 不是强制删除。
- `text recovery temps` 的退出码沿用既有约定：完成即 0，I/O 失败 1，用法错误 2；预览发现可清理项仍为 0（与 `text recovery status` 那种“仍有未收尾写入”不同）。
- 已知限制按设计“已知限制”逐条写入，至少覆盖：
  1. 强杀在租约内容写完整之前 → 残片按 `LeaseUnparsable` **永久保留**，需手工删除；
  2. 目录里有一份读不懂／版本未知的租约 → 该目录的候选本轮都不删，直到手工处理；
  3. 旧版无租约的临时文件一律 `TempWithoutLease`，不会自动清理；
  4. 临时文件名回收再用（同名重建）未验证，协议只记录文件名、不记录文件对象标识；
  5. 同进程活跃集合不在本协议内；
  6. Windows 上持锁不阻止他人改名／删除，协议是协作式的；
  7. 符号链接与 junction 形态只用“不是普通文件”这一条覆盖，实测样本只有目录形态；
  8. 硬链接数异常既未定义原因值，也未给检查方式与用例，**不声称已覆盖**。

`CHANGELOG.md` 顶部加 1.2.2 段：修复强杀后遗留 `.ziyuan-tmp-*` 需手工删除的问题；新增 `text recovery temps`；说明 `apply_changes` 行为变化（写入前清理同目录陈旧临时文件）与新增控制文件 `.ziyuan-lease-<id>`。

`docs/roadmap.md` 把 1.2.2 标为完成，链接到设计与验收。

`docs/acceptance/1.2.2.md` 记录：需求、设计链接、实施计划链接、验证矩阵、平台说明、已知限制全表。验证矩阵必须能逐项对上真实用例，至少含：

| 场景 | 对应用例 | 结果 |
| --- | --- | --- |
| 严格命名：合法上界被接受、符号／溢出／额外后缀被拒 | `candidate_names_are_strict` | 待填 |
| 租约覆盖创建到释放 | `lease_covers_creation_until_release` | 待填 |
| 加锁前被回收即放弃、不建临时文件 | `hold_aborts_when_its_name_was_reclaimed_before_the_lock` | 待填 |
| 改名窗口内租约仍在 | `lease_is_held_after_the_temp_handle_closes_and_before_rename` | 待填 |
| 改名成功但删租约失败 → 替换仍成功 | `lease_delete_failure_does_not_fail_the_replacement` | 待填 |
| 受保护相对路径优先于任何 I/O | `protected_relative_path_wins_before_any_io` | 待填 |
| 非普通文件／链接绝不被删 | `candidate_type_is_checked_before_leases`、`symlinked_candidate_is_never_followed` | 待填 |
| 一认领者删、两认领者歧义、坏租约挡删 | `one_claimant_is_removed_with_its_lease`、`two_claimants_are_ambiguous`、`bad_lease_blocks_deletion_of_a_claimed_candidate` | 待填 |
| 预览不删（含不删孤儿租约） | `preview_reports_removable_without_deleting`、`orphan_lease_is_reclaimed_only_when_applying`、`sweep_preview_deletes_nothing_and_reports_removable` | 待填 |
| 双清理者不重复删除 | `two_cleaners_never_double_count_a_single_candidate` | 待填 |
| 旧版无租约／PID 复用一律保留 | `a_reused_pid_without_a_lease_is_never_removed`、`text_recovery_temps_keeps_a_temp_without_a_lease` | 待填 |
| 自动清理只碰目标目录且不阻断写入 | `apply_changes_sweeps_a_stale_temp_next_to_the_target`、`apply_changes_leaves_other_directories_alone` | 待填 |

表中的“结果”按实际运行填 `通过／跳过（平台）`，**不照抄本计划的预期计数**；平台说明写清哪些断言按 `#[cfg(windows)]` 门控、Linux/macOS 只由 CI 覆盖。

- [ ] **Step 2: 校验文档。**

```powershell
Select-String -Path docs/text-projects.md,docs/stability.md,CHANGELOG.md,docs/roadmap.md,docs/acceptance/1.2.2.md -Pattern " +$"
```

Expected: 无输出（无行尾空白）。另人工核对：代码围栏成对、相对链接可达、中文引号成对，代码块内的 ASCII 引号保持原样。

- [ ] **Step 3: 跑本轮全量校验。**

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Expected: fmt 与 clippy 通过；测试全绿。若库测试仍出现既有的两个 MCP `initialize` 逾时，用 `-- --test-threads=4` 复核并在验收记录里如实写明该问题与本次改动无关。

- [ ] **Step 4: 提交。**

```powershell
jj commit -m "docs: 记录 1.2.2 临时文件清理的行为、限制与验收" -m "Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>"
```

## 完成标准

- `cargo test --workspace --locked` 全绿；`cargo clippy --workspace --all-targets --locked -- -D warnings` 无告警；`cargo fmt --all -- --check` 干净；代码里不再有 `allow(dead_code)` 脚手架。
- 强杀写入者后，下一次写入会同目录清掉可证明陈旧的临时文件与租约；`text recovery temps --yes` 能清掉其余可判定项。
- 受保护集合命中、活跃租约、非普通文件、命名不合格式、读不懂的租约，全部保留并在报告里给出原因。
- 两个清理者并发扫同一棵树时，同一候选只被记一次删除，且双方都不报失败。
- 报告只出现相对路径；退出码按设计：完成 0、I/O 失败 1、用法错误 2。
- 文档写明 `--yes` 不是强制删除，以及设计"已知限制"的全部 8 条（含旧版无租约遗留文件只能保留、坏租约挡住该目录、硬链接未覆盖）。



