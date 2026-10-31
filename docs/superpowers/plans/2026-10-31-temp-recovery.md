# 1.2.2 临时文件清理 Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 先用可重复的跨进程实验验证活跃保护的必要条件，补全经审阅的协议，再实施不会误删活跃文件的临时文件清理。

**Architecture:** 保持正常替换路径不变，在独立的 Rust 集成测试目标中验证锁、强杀、Windows 共享模式及创建／改名窗口。实验不能单凭锁可取得就判定文件陈旧。本计划可执行范围止于技术证据、设计补充及用户审阅，生产清理器须在该门槛通过后另写具体实施任务。

**Tech Stack:** Rust 1.89 / edition 2024，标准库 `File::try_lock`、`std::process` 与平台文件 API，Cargo，PowerShell 7，jj；本阶段不引入依赖、不使用 unsafe。

---

设计依据：[临时文件清理设计](../specs/2026-10-31-temp-recovery-design.md)。向量记忆库不在本计划实施范围内。

执行状态：Task 1 至 Task 3 完成（提交 c5310854、8aec5639、47a8cbff）；Task 4 第 1、3、5、6 步完成，第 2 步部分完成（1.89 缺失、Linux/macOS 证据未取得），第 4 步因证据不足未执行。修正与锁身份补验见 71d2e713、6ed31b39。停在用户审阅，下一步须另行批准。证据见[可行性报告](../../acceptance/1.2.2-temp-lease-feasibility.md)。

## Chunk 1: 活跃保护可行性与设计停止点

### 执行边界

用户已批准书面设计与实施计划编写，尚未授权执行本计划。本阶段不是“1.2.2 修复已完成”。

不能把尚未确定的租约载体、锁顺序或控制文件清理方式写成已经验证的生产方案。按以下任务先获得事实；若无法在现有依赖和边界内解决，提交实验结论并暂停，不自行增加依赖、扩大扫描范围或放宽删除条件。

所有命令在专用 worktree `D:/rust-project/ziyuan/.worktrees/v1.1.0` 执行：

```powershell
cd D:/rust-project/ziyuan/.worktrees/v1.1.0
$env:CARGO_TARGET_DIR = 'D:/rust-project/ziyuan/target'
```

后续新开的 PowerShell 进程均须重新设置工作目录与 `CARGO_TARGET_DIR`。原生 macOS／Linux 执行者使用自己的绝对 checkout 和 target 路径，不复制 Windows 路径。

### 文件职责

| 操作 | 路径 | 职责 |
| --- | --- | --- |
| 新建 | `crates/ziyuan-runtime/tests/temp_lease_probe.rs` | 独立集成测试入口、父进程断言和子进程测试入口；不暴露生产 API |
| 新建 | `crates/ziyuan-runtime/tests/support/temp_lease_probe.rs` | 仅本测试使用的临时目录、子进程守卫、有时限 IPC、锁／共享模式实验辅助 |
| 新建 | `docs/acceptance/1.2.2-temp-lease-feasibility.md` | 命令、平台、观察结果、未测项及协议必要条件，不冒充产品验收 |
| 修改 | `docs/superpowers/specs/2026-10-31-temp-recovery-design.md` | 仅在证据支持时补充一个具体保护协议；保留设计审阅门槛 |
| 修改 | `docs/superpowers/plans/2026-10-31-temp-recovery.md` | 勾选已验证步骤，登记停止点及后续计划位置 |
| 不改 | `crates/ziyuan-runtime/src/tools/replace.rs`、CLI、Cargo manifests、版本和发布工作流 | 本阶段不启用任何生产删除行为 |

`tests/` 当前不存在，可新建；`tests/support/` 不单独成为 Cargo 集成测试目标。用入口内的 `#[path = "support/temp_lease_probe.rs"] mod support;` 引入辅助模块。不得把整个 `replace.rs` 用 `#[path]` 编入新目标，以免复制它的测试和线程局部状态。

### Task 1: 固定基线与可重复的子进程测试框架

- [x] **Step 1: 检查工作副本并记录现有替换测试。**

```powershell
jj status
cargo test -p ziyuan-runtime --locked tools::replace::tests:: -- --nocapture
```

预期：工作副本无无关改动，现有替换测试通过。失败则先按 @systematic-debugging 定位，不把基线失败算作新增测试的 RED。

- [x] **Step 2: 新建入口及辅助模块，先写 `probe_child_handshake_has_deadline` 测试。**

定义父子协议：父进程创建 loopback `TcpListener`，子进程接收地址及随机生成的本次测试标识并连接。只传测试目录内路径、阶段名和固定测试内容，不传用户文件或凭据。监听地址仅 `127.0.0.1`，不得暴露外网。协议逐行发送 `READY <id> <stage>`，父进程核对 id 后发 `CONTINUE`；所有读写、接受连接、子进程退出等待均有 10 秒截止时间。

先让握手辅助函数明确返回 `Unsupported`，测试断言成功，确保 RED 来自未实现行为。辅助实现用非阻塞 accept 加短暂让步及截止时间检查，不能无界等待或仅靠固定 sleep 猜就绪。

- [x] **Step 3: 运行 RED。**

```powershell
cargo test -p ziyuan-runtime --locked --test temp_lease_probe probe_child_handshake_has_deadline -- --exact --nocapture
```

预期：测试断言握手失败，不是编译错误或错误筛选导致的零测试。未匹配到测试名必须停止修正。

- [x] **Step 4: 实现最小握手与子进程守卫。**

子入口为普通 `#[test] fn probe_child_entry()`；未设置专属测试环境变量时立即返回，不执行实验。父进程使用以下参数启动当前测试二进制，子进程只运行该入口：

```rust
let child = std::process::Command::new(std::env::current_exe()?)
    .args(["--exact", "probe_child_entry", "--nocapture"])
    .env("ZIYUAN_TEMP_LEASE_PROBE", case)
    .env("ZIYUAN_TEMP_LEASE_PROBE_ADDR", address)
    .env("ZIYUAN_TEMP_LEASE_PROBE_ID", test_id)
    .env("ZIYUAN_TEMP_LEASE_PROBE_ROOT", root)
    .spawn()?;
```

辅助模块的 `ChildGuard` 必须在 `Drop` 中先 `kill` 再 `wait`，包含断言 panic 的路径；正常结束也须回收子进程。父进程自己的测试目录使用 PID、纳秒和进程内计数通过 `create_dir` 防碰撞，不先删除同名目录。清理守卫只删除本次成功创建的测试目录，绝不扫描用户工作目录。IPC 失败时给出阶段名及操作系统错误，不打印完整环境。

- [x] **Step 5: 重跑握手测试及 `probe_child_entry`，断言有实际测试执行并通过。**

用父进程不发 `CONTINUE` 的故障用例确认子进程等待也会在截止时间内退出；握手错误 ID 必须拒绝。测试通过后提交本任务的两个测试文件：

```powershell
jj diff --stat
jj diff
jj commit -m "test(runtime): 增加临时租约跨进程实验框架" -m "Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>"
```

每次提交前复核仅包含本任务文件；若用户插入了改动，不整批提交。

### Task 2: 验证 OS 锁、强杀释放与同进程行为

- [x] **Step 1: 写 `cross_process_lock_blocks_live_holder_and_releases_after_kill` 的失败测试。**

子进程创建独立测试文件，获取排他锁后发送 READY。父进程通过另一个句柄非阻塞试锁，必须得到 `WouldBlock`，不能把所有 I/O 错误都当作活跃。父进程强杀并回收子进程后，重新打开文件并在截止时间内取得锁。先让辅助函数错误地提前释放锁，确认测试能检出活跃期失守，再实现持续持有。

锁结果判断使用明确的错误分支：

```rust
match file.try_lock() {
    Ok(()) => ProbeLock::Acquired(file),
    Err(std::fs::TryLockError::WouldBlock) => ProbeLock::Busy,
    Err(std::fs::TryLockError::Error(error)) => return Err(error),
}
```

`ProbeLock::Acquired` 持有 `File`，不得返回 bool 后立即丢弃句柄。

- [x] **Step 2: 执行 RED，然后实现并执行 GREEN。**

```powershell
cargo test -p ziyuan-runtime --locked --test temp_lease_probe cross_process_lock_blocks_live_holder_and_releases_after_kill -- --exact --nocapture
```

每次恰有一项父测试执行；测试中的子入口不算第二个父测试。记录正常释放和强杀释放两种结果。

- [x] **Step 3: 添加 `same_process_independent_handles_are_observed`。**

测试同进程另开句柄和 `try_clone`，记录各平台实际排斥／共享行为。不要强加某个平台的观察为通用保证；测试只核对结果分类完整、无挂起且所有锁在守卫离开后可重新取得。报告中明确：无论观察如何，生产清理仍需同进程活跃集合或等效互斥，不能据此删除。

- [x] **Step 4: 重跑整个实验目标，提交锁实验。**

```powershell
cargo test -p ziyuan-runtime --locked --test temp_lease_probe -- --nocapture
jj diff
jj commit -m "test(runtime): 验证跨进程租约与强杀释放" -m "Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>"
```

### Task 3: 验证 Windows 共享语义与替换窗口

- [x] **Step 1: 写 `rename_with_live_handle_records_native_semantics` 和 `delete_with_live_handle_records_native_semantics`。**

全部使用自建测试目录和固定字节 `old` / `complete-new`。覆盖目标不存在和已存在；子进程持有、写入并同步临时文件，父进程观察临时文件能否打开／锁定／改名／删除。实验允许“操作被系统拒绝”，但必须记录错误码及目标最终内容；不能把拒绝改名伪装成成功。

Windows 测试比较标准打开方式与显式 `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`。仅测试模块使用安全标准库扩展：

```rust
#[cfg(windows)]
fn share_delete(options: &mut std::fs::OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    options.share_mode(0x0000_0001 | 0x0000_0002 | 0x0000_0004);
}
```

对持锁后写入、`sync_all`、保持租约时改名、成功后关闭句柄逐项断言。若结果只支持“独立锁载体而非临时文件句柄”，写为证据，不在本任务顺手新增生产控制文件。

- [x] **Step 2: 写 `close_before_rename_exposes_unprotected_interval`。**

用与现有 `fill` 一致的实验顺序：创建→文件锁→写入／同步→关闭句柄→在改名前 READY 暂停。父进程此时能取得临时文件锁，证明仅在 `fill` 中锁定不足。此测试断言“观察到窗口”，通过不意味着协议通过。

- [x] **Step 3: 写 `create_before_lock_exposes_unprotected_interval`。**

子进程在创建临时文件后、加锁前 READY 暂停，父进程取得锁。断言该窗口存在，记录连续保护必须从文件可见之前开始。实验只观察，不因拿到锁而删文件。

- [x] **Step 4: 写 `legacy_unleased_file_is_never_proven_stale_by_lock_alone`。**

子进程用旧版方式打开未加锁文件并暂停；父进程即使取得锁，也只返回测试状态 `OwnershipUnknown`。断言文件和内容不变。此用例把“锁可得”和“陈旧已证明”拆开，后续清理器必须继承此测试意图。

- [x] **Step 5: 完整运行并提交生命周期实验。**

```powershell
cargo test -p ziyuan-runtime --locked --test temp_lease_probe -- --nocapture
cargo test -p ziyuan-runtime --locked tools::replace::tests:: -- --nocapture
jj diff
jj commit -m "test(runtime): 记录临时文件锁与原子替换窗口" -m "Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>"
```

### Task 4: 平台证据、协议补充与停止点

- [x] **Step 1: 在原生 Windows 执行窄测、格式及 lints。**

```powershell
cargo fmt --all -- --check
cargo clippy -p ziyuan-runtime --all-targets --locked -- -D warnings
cargo test -p ziyuan-runtime --locked --test temp_lease_probe -- --nocapture
cargo test -p ziyuan-runtime --locked
```

预期全部通过；“观察到危险窗口”的测试通过不能写成“删除协议已安全”。新增测试不依赖管理员权限、网络服务或第三方程序；loopback IPC 失败应如实失败，不静默跳过。

- [ ] **Step 2: 检查 MSRV，收集原生 Linux 与 macOS 结果。**（部分完成：`cargo +1.90 check` 通过；Rust 1.89 未安装，Linux/macOS 原生语义证据未取得，Windows 语义证据已取得）

```powershell
cargo +1.89 check -p ziyuan-runtime --all-targets --locked
```

Rust 1.89 未安装则报告环境阻塞，不自动安装或改变工具链。Linux/macOS 运行同一实验目标和现有替换测试。现有 CI 已在三个原生平台跑 workspace 测试，新集成测试会被发现，无需为回归发现修改工作流；但普通 CI 不带 `--nocapture`，只证明断言通过，不提供成功实验的行为输出。

具体平台语义证据须由有原生环境的执行者运行下列命令，保存退出码和完整输出至该平台自己的生成目录，再将不含用户数据的观察摘要录入报告：

```powershell
cargo test -p ziyuan-runtime --locked --test temp_lease_probe -- --nocapture
```

若只有普通 CI 绿灯，报告须区分“回归断言通过”和“原生语义输出未取得”，后者不能填为已经验证的观察。没有原生执行者时，继续标为“语义证据未取得”并保留停止点；另请用户授权增加保留 `--nocapture` 输出的定向 CI 步骤或提供原生环境后再收证据。本计划不隐含授权修改工作流或推送，不能为跑 CI 自行推送。

- [x] **Step 3: 新建可行性报告，分清事实和未决问题。**

报告逐项列出平台／文件系统／Rust 版本、命令、退出状态、测试数量、锁行为、Windows 共享模式、创建及关句柄窗口、强杀释放、未测项。还必须评估：稳定锁身份、同进程活跃集合、锁顺序及非阻塞清理、可信协议归属证据、旧版混用、控制文件自身强杀垃圾、业务恢复引用核验。

本实验不能证明网络文件系统可靠，不扩大到 SMB／NFS；支持范围待协议补充明确。任一关键维度只有推断没有证据时，结论是“尚未闭合”，不是“通过”。

- [ ] **Step 4: 仅在证据充分时补充一个具体协议。**（未执行：证据不足，按停止点转为下一轮有界验证，见可行性报告）

设计补充必须明确载体路径及版本、创建到改名／丢弃的状态机、何时登记活跃、如何证明归属、写入与清理的完整锁顺序、句柄生命周期、身份复核、控制文件遗留处置、失败降级和未来共享模块位置。必要的完整生命周期实验须在本目标内验证，不能只画状态图。

若实验仅证明单文件锁不足，先交付负面结果并暂停，请用户批准下一轮有界协议验证。不得在缺少证据时写下“将用某种租约解决”然后进入生产实现。

- [x] **Step 5: 独立审查实验结论及设计补充，复核差异后提交。**

使用 @requesting-code-review，并派一个只读 reviewer 检查本阶段变更；如发现问题先修复并重跑相关检查。提交前运行 `jj status`、`jj diff`，确认没有版本、依赖、CLI 或生产替换变更。

```powershell
jj commit -m "docs(runtime): 记录临时租约验证结果与实现门槛" -m "Co-authored-by: factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>"
```

- [x] **Step 6: 停止并交用户审阅。**

交付实验报告、具体协议（若已闭合）、测试证据及限制。等待用户批准设计补充后，使用 @writing-plans 编写生产清理实施计划，不能直接执行下面的后续范围。

## Chunk 2: 租约载体验证（2026-10-31 第二轮，用户已批准）

第一轮结论是"临时文件的锁不足以防误删"，且关闭句柄到改名之间必然失守。本轮只验证一个候选载体能否补上这段：**租约文件先于临时文件存在，且租约句柄一直持有到改名完成**，清理者靠"租约锁能否取得"判断写入者是否存活，靠租约内容判断它属于哪个临时文件。

仍是实验，不是实现。不改 `replace.rs`、不加 CLI、不加依赖、不写版本。

### 本轮假设与格式

租约文件与临时文件同目录，命名为 `.ziyuan-lease-<id>`，`<id>` 为 32 位小写十六进制随机值，由 `create_new` 创建。内容为单行：

```
ziyuan-lease v1 temp=<temp-file-name> id=<32 hex>
```

`temp=` 记录它保护的临时文件名（不含目录）。持有顺序固定为：建租约 → 租约加锁 → 建临时文件 → 写 → 同步 → 改名 → 关闭租约句柄 → 删租约。

### 文件职责

| 操作 | 路径 | 职责 |
| --- | --- | --- |
| 修改 | `crates/ziyuan-runtime/tests/support/temp_lease_probe.rs` | 增加租约读写、加锁、非阻塞探测、安全清理的测试辅助 |
| 修改 | `crates/ziyuan-runtime/tests/temp_lease_probe.rs` | 新增租约生命周期、竞争、强杀残留、旧文件与损坏租约的用例 |
| 修改 | `docs/acceptance/1.2.2-temp-lease-feasibility.md` | 记录本轮证据、未决项与结论 |
| 不改 | `crates/ziyuan-runtime/src/**`、CLI、manifests、工作流、版本 | 生产行为一律不动 |

### Task 5: 租约载体生命周期

- [x] **Step 1: 写 `lease_covers_close_before_rename_window` 的失败测试。**

子进程按上述顺序建租约、加锁、建临时文件、写入、`sync_all`、**关闭临时文件句柄**，然后在改名前 READY。父进程观察两点：临时文件本身可以取锁（证明第一轮的窗口仍在），但**租约**是 Busy。断言必须能失败：先让辅助在关闭临时句柄时一并释放租约，确认测试报错，再改为持续持有。

- [x] **Step 2: 写 `cleaner_keeps_temp_while_lease_is_held`。**

父进程只做非阻塞探测，不得等待。断言租约为 Busy 时决策为保留，临时文件与租约都不变；子进程 CONTINUE 后改名成功，父进程随后可取得租约锁。

- [x] **Step 3: 写 `killed_writer_lease_is_reclaimable`。**

子进程持租约与临时文件时被强杀。父进程取得租约锁后，必须核验：租约内容版本为 v1、`temp=` 与实际文件名一致、临时文件是普通文件。三者齐全才删除临时文件、再删租约，并断言目录内无其他文件被改动。任一核验失败则保留并报告，测试要覆盖"内容对不上"和"临时文件已不在"两种保留分支。

- [x] **Step 4: 写 `killed_before_temp_creation_leaves_only_lease`。**

子进程建好并锁住租约、尚未建临时文件时被强杀。父进程应只删租约，目录清空，不得因"没有临时文件"去猜别的东西。

- [x] **Step 5: 写 `temp_without_lease_and_foreign_leases_are_retained`。**

覆盖三种保留：符合命名但没有租约的旧临时文件、租约为未知版本或无法解析、租约 `id` 与文件名不匹配。三者都必须原样保留并给出可区分的原因；`--yes` 之类的强制语义不在本轮实现。

- [x] **Step 6: 写 `second_creator_conflicts_instead_of_overwriting`。**

同名租约已存在时，`create_new` 必须返回 `AlreadyExists`，第二个写入者据此报冲突并退出，不得截断或复用别人的租约。断言原租约内容未变。

- [x] **Step 7: 写 `lease_lock_wait_has_deadline`。**

写入者等待已被占用的租约锁时必须有时限并超时返回，不能无限等待；清理者一侧只允许非阻塞探测。用 300 毫秒量级的上限验证，并断言超时后双方仍能各自收尾，不留下未回收的子进程。

- [x] **Step 8: 全量运行并提交。**

```powershell
cargo test -p ziyuan-runtime --locked --test temp_lease_probe -- --nocapture
cargo test -p ziyuan-runtime --locked --lib tools::replace::tests::
cargo fmt --all -- --check
cargo clippy -p ziyuan-runtime --all-targets --locked -- -D warnings
```

- [x] **Step 9: 更新可行性报告并停止。**

报告新增一节记录：租约格式、生命周期、强杀后各阶段的可回收性、保留分支、锁等待行为，以及仍未验证的项。结论若支持，就写成"候选协议可闭合，待用户审阅后写入设计"；若不支持，如实写"仍不闭合"并说明卡在哪一步。不得在报告里直接宣布 1.2.2 已可实施。

### 本轮执行记录

Task 5 九步已完成，另补一项计划外但必需的安全修正：

- 初版辅助在"核验后、删除前"释放租约锁，使"核验 + 删除"不再原子，两个清理者可能各删一次。已改为租约句柄保持加锁直到临时文件与租约都被删除，并新增 `two_cleaners_never_double_delete`。
- 该用例经过两轮修正才真正有效：最初两线程同时启动，看似并发，但在缺陷顺序下 15 次全过（第二个清理者常先拿到 Busy）；改为受控交错（先让第一个停在核验后，再启动第二个，300 毫秒后放行）后，缺陷顺序 **10 次全部失败**，正确顺序 5 次全部通过。
- 提交：`fdb56882`（Task 5 主体）；顺序修正与报告更新见本轮最后一个提交。
- 全量：`temp_lease_probe` 26 passed、`tools::replace::tests` 16 passed、fmt 与 clippy 通过、`cargo +1.90 check` 通过。

### 本轮停止点

以上完成即停。把租约协议写进设计文档、以及生产清理实施计划，都需要用户另行批准；本轮不进入 `replace.rs`。

## 门槛通过后的生产计划输入（非可执行任务）

下一份计划须固定以下文件责任和逐项 RED/GREEN 命令，不允许跳过本章安全条件：

1. **共用核验器与协议实现**：根据已验证的载体和依赖方向选择唯一公共模块位置；runtime 和 CLI 共用，不能产生 runtime↔storage 环依赖。名称严格解析，普通文件／硬链接检查，不跟随 symlink、junction、reparse point。锁冲突、未知及身份变化一律保留。
2. **正常替换接入**：修改 `crates/ziyuan-runtime/src/tools/replace.rs`，使连续保护覆盖创建至丢弃；写前只扫描目标父目录，不阻断新写入。保留权限收紧、同步、原子改名、Windows 重试和原始错误语义。确定清理报告如何从 runtime 进入既有诊断通道，不污染 JSON 或把替换成功误报为失败。
3. **显式命令**：修改 `crates/ziyuan-cli/src/text_commands.rs`，新增 `temps`，默认预览只读，`--yes` 逐项复核后删除；确认全局 `--json` 解析位置及选项顺序。冻结报告字段、跳过原因及 0/1/2 退出码，并在 `crates/ziyuan-cli/tests/contract.rs` 加黑盒契约测试。
4. **恢复兼容**：明确如何从 runtime 局部入口获得可信的工作目录身份及恢复引用信息，或在无法获取时安全保留；恢复状态不可读不得猜测。清理不推进世代、不删除恢复引用材料，不以删除临时文件解锁业务恢复。
5. **完整安全验收**：独立进程强杀后清理、全生命周期活跃文件、同进程与双清理者、旧版未知所有权、相似名称、链接／junction／硬链接、扫描及删除失败、业务恢复引用、Windows 原生共享冲突及目标 old/new 完整性。
6. **产品文档与回归**：更新 `docs/text-projects.md`、必要的 `docs/stability.md` 新增契约、`CHANGELOG.md` 的 Unreleased 与 `docs/roadmap.md`。先跑受影响 crate、CLI 契约，再按现有 CI 跑 fmt、workspace clippy、MSRV、workspace 测试和各原生平台验收。1.2.2 版本登记、tag、推送、GitHub Release 与 crates.io 均另行授权。

该清单明确后续交付范围，不代表生产实施计划已经可执行，也不把向量记忆库夹带进入 1.2.2。
