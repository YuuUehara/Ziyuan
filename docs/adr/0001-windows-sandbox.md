# ADR-0001：Windows 号舍支持政策

- 状态：已切换（方案 B·W2 `run_tests` 与 W3 长连接 stdio 外藩、零网络能力命名管道门吏及快照内受控 `haoshe.write` 均已接入生产；开放联网、家目录写权与外部 `CARGO_TARGET_DIR` 维持显式失败关闭）
- 日期：2026-10-29
- 相关：[SECURITY.md](../../SECURITY.md)、[roadmap.md](../roadmap.md)「1.0 之后·号舍」

## 背景

号舍已在 Linux（bubblewrap）与 macOS（Seatbelt）落地：断网、只写测试副本、凭据目录不可读，可按域名放行（经门吏代理）。Windows 原生隔离（受限令牌、AppContainer）需要显式处理 unsafe/FFI 边界，维护与安全审计成本高。Job Objects 只约束进程组与资源，不能单独等同号舍；仅靠受限令牌也不应宣称达到 AppContainer 级的文件与网络隔离。把 `cmd /C` 直跑称作「沙箱」是失实陈述，任何文档与界面不得如此。

## 决策（方案 A：明确暂不支持）

v1.0 对 Windows 的承诺：

1. Windows 支持常规功能与只读工作流（CLI、TUI、Web、观政）。
2. 需要号舍隔离的受保护执行（`run_tests`、名册要求入号舍的外藩）在 Windows 上 **fail closed**：如实报「号舍不可用」，绝不悄悄退化为未隔离执行。默认的 `auto` 与 `require` 都是如此。
3. `ZIYUAN_SANDBOX=off` 仍是用户显式确认风险后的无隔离执行，各界面与测试结果如实标注。
4. 预留 `ziyuan-sandbox-windows` 扩展点：独立平台 crate，`SandboxCapability` 能力分级（`Unavailable` / `RestrictedProcess` / `FileSystemAndNetworkIsolated`），条件满足后实现。

## 落地

`auto` 已收紧（原先没有号舍时照常运行并注明）：无号舍时受保护操作一律拒绝，返回稳定错误 `ziyuan_runtime::haoshe::SandboxUnavailable { platform, operation, reason }`，仅显式 `off` 照常运行。Windows CI 跑全部测试：`run_tests` 用例先断言拒绝，再以 `off` 核对副本逻辑。

## 备选

- **方案 B：立即实现独立 Windows 号舍 crate。** 交付慢，需威胁模型、真实 Windows runner 上的隔离测试与 unsafe 审计；再评估条件满足后启动。
- **外部容器、虚拟机或 Windows Sandbox。** 部署与镜像成本高，作为后续研究，不入 v1.0。
- **显式允许一切无隔离执行。** 边界含糊，不采纳。

## 再评估条件

出现明确的 Windows 原生强隔离需求、可复现威胁模型、稳定的原生 Windows CI、安全测试与 unsafe 审计的维护资源时，按方案 B 启动 `ziyuan-sandbox-windows`；实现按能力等级如实报告，不笼统宣称「已隔离」。

2026-11-01 起按分阶段路线推进 Windows 原生号舍（W0 设计与启动条件、W1 隔离原型、W2 工具接入验收）。列入计划不等于再评估条件已经全部满足；W0 记录方案 B 的启动决定，W2 通过后才修改当前支持状态。此前方案 A 的失败关闭规则继续有效。

## W0 决定（2026-11-01）：进入独立原型，不接生产

4A 提交 [4e78433](https://github.com/YuuUehara/Ziyuan/commit/4e784334ce4bfcaf952812ee9fb9a8de1072baf6) 的[六项 CI](https://github.com/YuuUehara/Ziyuan/actions/runs/41332688102)全部通过。按既定顺序完成本节设计核对，**方案 B 仅进入 W1 实验原型阶段**；不是宣布 Windows 隔离已可用，也不是通过安全审计。生产仍执行方案 A，`auto`／`require` 不回退为无隔离运行。

### 威胁模型与首批范围

- 将用户配置的测试命令、项目构建脚本及其后代进程视为可能恶意。保护宿主源码、`.ziyuan`、Git／jj 元数据、模拟凭据、宿主服务及其他任务；命令本身在副本内写坏资料不算逃逸，副本不自动落盘。
- 信任 Windows 内核、安全策略、紫垣父进程及受审的平台边界；不承诺抵抗内核漏洞、管理员、调试器或另一个同用户恶意宿主进程，不把进程隔离等同虚拟机。宿主路径在检查后被外部恶意替换的边界必须明示，不以字符串前缀或 `canonicalize` 一次就宣称解决。
- 首批仅支持默认断网、任务私有副本／临时目录／AppContainer profile，以及明确准入的只读运行依赖。profile 本身可写是 Windows 的资源模型，必须按任务独立、纳入清理并显式列为可写范围，不能承诺“系统中只存在一个可写目录”。
- 不授网络 capability、不添加 loopback exemption，不修改宿主防火墙或全局注册表策略，不改凭据目录、源码或工具链的 ACL；本任务 profile 的系统登记与清理由创建协议管理。不以管理员权限、WSL、容器或用户关闭号舍作为强隔离通过条件。外部 `CARGO_TARGET_DIR` 与广域可写目录不直接继承，首批不支持就明确拒绝。
- 网络按域名放行、本地外藩的显式联网及任意广域读写授权不在 W1 范围。W2 只接已验证的能力；未知或尚不支持的 `Network::Only`／`Open` 请求须明示拒绝，不静默改写用户策略，更不能放开全部网络凑兼容。

### 方案选择与能力矩阵

微软文档确认：AppContainer 的访问同时受用户与容器 SID 的权限约束；普通 AppContainer 有环境授予的额外访问，LPAC 通过退出 All Application Packages 权限进一步收窄。Job 管理进程组和资源，不提供文件、网络隔离；默认后代关联也不能代替对 WMI／COM 等代理创建路径的限制和探针。

| 候选 | 能提供什么 | 缺什么／决定 |
| --- | --- | --- |
| `cmd /C` 或仅限制环境变量 | 执行兼容与部分秘密环境过滤 | 无文件／网络隔离；永不标为号舍 |
| 受限令牌、低完整性级别、Job | 部分权限／写入限制、进程与资源收尾 | 不能单独证明宿主资料不可读或默认断网；至多 `RestrictedProcess` |
| LPAC＋独立资源权限＋Job | 文件／对象访问收窄、无网络 capability、进程树管理的候选组合 | 选作 W1；必须逐项实测，不从 API 名称直接升级能力 |
| 实验性 CreateProcessInSandbox | 另一种平台路线 | 首批不依赖实验性 API；稳定性及发布条件另评 |

`Unavailable`／`RestrictedProcess`／`FileSystemAndNetworkIsolated` 仍是拟议能力契约，不是已有类型。API 存在、编译成功、令牌含 AppContainer 标记、Job 关闭成功，均不足以单独给出最高等级。

### 平台边界与启动协议（拟议接口，未实现）

1. `ziyuan-sandbox-windows` 独立拥有 Windows 句柄、SID、属性列表及进程生命周期，对上层提供创建、等待、终止和结果查询的安全 Rust 接口；不向 runtime 泄漏裸句柄或要求调用者自行补策略。调用输入为已解析的绝对程序路径、参数、工作目录、准入资源、显式环境与标准流，不接受不受控的宿主句柄列表。
2. 当前 `haoshe::Confined` 只携带 `std::process::Command`，`run_tests` 与 MCP 各自 `spawn`／等待；不能塞一个临时 Job 句柄后在函数返回时销毁。W1 先独立验证生命周期，4B 再梳理共享等待／取消契约，W2 才以可保留资源所有权的执行接口接入两处调用者。Windows shell 也必须独立构造，不能沿用当前 Unix `/bin/sh -c` 路径。
3. 每次创建独立 profile 与最小资源权限，先验证所有准入路径，拒绝越界重解析点、链接和不支持的路径形状。必要系统加载资源与依赖逐项登记，优先私有只读副本；LPAC 运行失败时不得通过给整个用户目录或磁盘加容器权限解决。并发任务不得复用可写 profile。
4. 用 `STARTUPINFOEX`／`SECURITY_CAPABILITIES` 和 LPAC 的 `ALL_APPLICATION_PACKAGES_POLICY` 创建进程；配置 Job 的 kill-on-close，禁用 breakaway。首批采用挂起创建、加入 Job、核对身份／策略、最后恢复线程的顺序；任一步失败，未受保护的用户命令不得先执行。无法嵌套进入 CI 已有 Job 时报告不可用，不带 breakaway 重试。
5. 标准流只准入本任务的必要句柄。使用 `HANDLE_LIST` 时遵守 Win32 要求设置 `bInheritHandles=TRUE`，但不得因此继承其他句柄；Job、秘密文件、父进程等句柄不准传入。清空环境后显式提供必要系统变量，`HOME`／`USERPROFILE`／`APPDATA`／`TEMP` 及缓存指向私有资料，不直接复用宿主用户配置。shell 路径、Unicode／引号和启动目录分别验证。
6. 正常完成、超时、取消、父进程退出及启动中途失败都由同一资源所有者收尾。终止整个本任务 Job，确认后代退出后才删除副本／profile、释放执行许可。Job 通知可能遗漏，不能仅凭没有通知或父进程已退出判断整树已清空；结合明确的等待与进程计数。清理失败保留可识别诊断，不按名字杀进程、不删除别的任务目录、不复用未清理资源。

### 原生平台与 unsafe 审阅条件

- 已核对[最近 Windows 作业日志](https://github.com/YuuUehara/Ziyuan/actions/runs/41332688102/job/124925921335)：Windows Server 2025 Datacenter，`10.0.26100`；镜像 `windows-2025-vs2026`，版本 `20261021.278.1`。**这是功能基线，不是隔离探针结果**，`windows-latest` 将来可能变更。
- W1 先针对原生 x86_64 MSVC、Server 2025 的 `26100` 基线验证，每份证据记录实际 OS／镜像／权限与父 Job 状态。CI 账户名或镜像不能证明非管理员可用，须检查实际令牌并补普通用户对照。Windows 11 24H2 同代桌面系统列为后续原生补验目标；没有桌面实测不外推支持，更旧 Windows、ARM64、网络盘与共享 profile 不在首批承诺。系统支持某 API 不等于该版本已验收。
- 根工作区保持 `unsafe_code = "forbid"`。必要 FFI 只能在独立平台 crate 的私有模块显式授权，不能给 runtime 或整个工作区降级 lint；具体 Cargo 配置与 Windows bindings 依赖在 W1 提交中审阅。上层保持 safe API。
- 每处 unsafe 必须说明指针／长度／对齐、缓冲区与 SID 寿命、句柄唯一所有权、分配与释放 API 配对、错误分支和线程语义。原型由本批实现者逐处记录与审查；目前没有已指定的长期平台维护人或独立安全签核，不能代替用户宣称其已具备。W2 生产接入前必须完成独立的平台边界审阅与维护责任确认；未满足则保持实验级及失败关闭。

### W1 分批探针与停止条件

原型测试只用临时哨兵、模拟凭据与自建服务，不拿真实秘密试探。每项有宿主允许对照、容器内实际运行确认和明确访问结果；单看非零退出、超时或没有输出不算拒绝证据。

| 小批 | 允许对照 | 必须拒绝或准确收尾的对照 |
| --- | --- | --- |
| W1a · 启动与进程归属 | 原生探针写启动标记，记录令牌／LPAC 身份和 Job 归属 | 策略／入 Job 失败时用户命令无启动标记；无关可继承秘密句柄不可用 |
| W1b · 文件与依赖 | 私有副本、临时目录与 profile 可写；准入依赖可读／执行 | 宿主源码、`.ziyuan`、模拟 `.ssh`、版本库及另一任务读写被拒；只读依赖不可改；覆盖大小写、空格、Unicode、长路径、重解析点／硬链接与启动后换链边界 |
| W1c · 网络与代理路径 | 宿主访问自建 TCP／UDP 服务成功，容器内允许文件探针成功 | IPv4／IPv6 loopback 与非 loopback 目标的访问被拒，核对服务端记录；检查未授网络能力、无 loopback 豁免；WMI／COM 等代理启动路径不得越出容器策略。只有本机服务探针时，不报告公网出站已实测 |
| W1d · 生命周期 | 正常命令及后代退出，退出状态与输出正确 | 超时、主动取消、父进程异常退出、孙进程持有输出管道、启动故障均有界收尾；无后代、句柄、目录／profile 残留或明确清理失败；并行任务互不终止 |

W1a 不得标为文件／网络隔离通过；W1b／W1c 未全通过时，不升级最高能力、不接入生产。探针若依赖管理员放宽策略、宿主广域 ACL、网络豁免或静默去掉 LPAC 才能跑，应停止并记录兼容性缺项，而非降低保护门槛。W1d 的取消机制与 4B 分别记录实现和实测，不能借平台 API 文档替代实际后代退出证据。

### 阶段实施记录（W0 – W2）

工作区根目录始终保持 `unsafe_code = "forbid"`，所有 Win32 FFI 仅封装在 `ziyuan-sandbox-windows` 私有模块 `windows.rs` 内（与独立验证探针 `probe.rs` 解耦），生产 `ziyuan-runtime` 仅经安全 Rust 接口调用。

- **W0（已完成）**：完成威胁模型、LPAC＋Job 方案选型及边界协议；选定以独立平台 crate 推进，不改生产 `runtime`／`MCP` 行为。
- **W1a 启动与句柄探针（Server 2025 CI 基线已通过）**：独立探针以 LPAC 属性、`SystemRoot`＋任务私有 `LOCALAPPDATA`、挂起创建＋kill-on-close Job 及显式标准流句柄白名单运行；在 Windows Server 2025 Datacenter（`10.0.26100`）基线上通过普通 AppContainer、LPAC（`AAP_ACCESS=0`）与恢复前故障清理三组对照。
- **W1b 文件、依赖与路径边界探针（原生 Windows 11 非管理员桌面与 CI 已通过）**：合成矩阵确认 LPAC 子进程可读写自身准入副本，对宿主源码、`.ziyuan`、模拟 `.ssh`/`.git` 与 peer 哨兵返回 `ERROR_ACCESS_DENIED`（5）；只读依赖目录（禁继承保护 DACL、显式低完整性标签 `S-1-16-4096` 与 `FileIdInfo` 句柄身份核验）全部通过；补齐大小写变体（`SOURCE.RS`、`.GIT/CONFIG`）拒绝、Unicode／空格路径、`>=160` 字符嵌套长路径读写以及 NTFS 目录联接（junction）重解析点拒绝（`PermissionDenied`）。
- **W1c 网络阻断探针（原生 Windows 11 非管理员桌面已通过）**：在宿主本机建立 IPv4 TCP、IPv4 UDP 与 IPv6 TCP 回环监听并先通过宿主正向连通对照后归零计数；LPAC 子进程核验 `TokenCapabilities == 0`，在 Windows 11 上调用 `WSAStartup` 即被内核拒绝（`WSASYSCALLFAILURE` 10107），且三类宿主监听器均确认零连接／零数据包抵达（`HOST_NET_ZERO_HITS=1`）。（仅测本机回环监听，不据此宣称公网出站已实测。）
- **W1d 生命周期、并发隔离与安全库接口（原生 Windows 11 非管理员桌面已通过）**：`ziyuan-sandbox-windows` 对外暴露无裸句柄的安全 Rust API（`SandboxCapability`、`LaunchSpec`、`ExitOutcome`、`RunResult`、`probe_capability`、`run_sandboxed`），所有 Win32 FFI 仍封装在私有 `windows` 模块。实测通过：严格 LPAC（`AAP_ACCESS=0`、`TokenCapabilities=0`）超时与主动取消有界收尾（`ActiveProcesses == 0` 且 profile 清零）、三路并行独立 LPAC 任务互不干扰、保留环境变量覆盖与重解析点拒绝；另在普通 0 能力 AppContainer＋`KILL_ON_JOB_CLOSE` Job 对照中验证超时、主动取消与父退孙留（孙进程持有继承 stdout 句柄）均由 `TerminateJobObject`＋进程数归零干净收尾。实测同时记录一项 Windows 11 LPAC 关键约束：零能力严格 LPAC 内直接调用 `CreateProcessW` 会被内核以 `ERROR_ACCESS_DENIED`（5）拒绝（触发 `Security-LessPrivilegedAppContainer` EventID 1），因此 W2 接入需派生子进程（如 `cargo test` 调用 `rustc`）的构建工具前，须另行评审最小非网络 capability 或代理进程方案。
- **W2 预备与执行所有权边界（1.29.0，原生 Windows 11 非管理员桌面已实测）**：
  - `ziyuan-sandbox-windows` 新增 `SandboxMode`（`StrictLpac` / `AppContainerChildCapable`）、`run_sandboxed_with_mode`、`ShellSpec` 与 `run_shell_sandboxed`（`%SystemRoot%\System32\cmd.exe /D /S /C`，禁 AutoRun、拒重解析点与控制字符）。原生非管理员 Windows 11 实测证实：0 能力 `AppContainerChildCapable`（`AAP_ACCESS=1`、`TokenCapabilities=0`）＋ `KILL_ON_JOB_CLOSE` Job 在允许 `cmd.exe` 派生子进程的同时，完整保留了 W1b 文件隔离（宿主源码、`.ziyuan`、模拟 `.ssh`、`.git` 与 peer 目录读写全部返回 Win32 `ERROR_ACCESS_DENIED (5)`）、W1c 零网络隔离（`NET_CAPS=0`，IPv4/IPv6 回环 TCP 与 IPv4 UDP 经 WFP 阻断，宿主三类监听均 `HOST_NET_ZERO_HITS=1`）以及超时／取消整树收尾（`active_processes_after == 0`、`cleanup_warnings == []`）。
  - `ziyuan-runtime::haoshe` 完成 §平台边界 #2 的执行所有权重构：新增拥有完整生命周期的 `haoshe::run_shell`（`ShellRunSpec` / `ShellRunOutput`），`tools.rs::RunTests` 已切至委托调用，并在 Windows `mode != Mode::Off` 分支显式校验并拒绝 `Network::Only`/`Open` 与外部 `CARGO_TARGET_DIR`。
- **W2 工程加固、模块拆分与真实工具链实测（1.30.0，原生 Windows 11 非管理员桌面已实测）**：
  - 将 W1a–W1d/W2 探针 CLI 拆入独立 `probe.rs`，生产核心 `windows.rs` 仅保留最小 Win32 FFI（零 WinSock、零探针代码）；`LaunchSpec`/`ShellSpec` 新增有界尾部读取 `max_output_bytes`（默认 16 MiB，`RunResult` 返回 `output_truncated`/`total_output_bytes`）；`build_environment` 补齐任务私有 `USERPROFILE`/`HOME`/`APPDATA`/`TEMP`/`TMP` 及 `WINDIR`/`ComSpec`/`SystemDrive`/`PATHEXT`（12 项保留变量禁覆盖）。
  - `AppContainerChildCapable` 落地按任务独立隐藏桌面 `PrivateDesktop`（Alternate Desktop 模式，动态加载 `System32\user32.dll`，仅授本任务 SID `0x000200C3`、禁切桌面与钩子、Job 排空后销毁），使静态链接 `user32.dll` 的真实工具链在非默认／无头桌面上不再以 `0xC0000142` 崩溃，同时保持严格 LPAC 与 crate 零静态 `user32.dll` 导入。
  - 原生非管理员 Windows 11 12/12 实测证实：真实 Windows PowerShell 与带标准 `ALL APPLICATION PACKAGES:(RX)` 继承的 `Program Files` Git 可在零能力 AppContainer＋`PrivateDesktop` 内正常运行且宿主秘密文件与回环监听均遭阻断（`hits == 0`）；同时证实 `%USERPROFILE%` 下工具链（如默认 `cargo.exe`）与剥离 `ALL APPLICATION PACKAGES` ACE 的 MSI 安装目录（如部分 Node.js MSI）均被内核拒绝访问，W2 绝不放宽宿主用户目录或系统 ACL。
  - `ziyuan-runtime` 补齐 `haoshe::ShellRunSpec::cancel` 主动取消透传，并在 `mcp.rs` 明示 W2 首批仅覆盖一次性断网命令（`run_tests`），长连接 stdio 外藩留待 W3。
## W2 生产切换决定（1.31.0，2026-11-21）

1. **治理与发布门槛闭环**：
   - 唯一维护者（`YuuUehara`）确认对精简后的 `crates/ziyuan-sandbox-windows/src/windows.rs` 私有 FFI 边界完成维护者审阅并承担 Windows 平台长期维护责任；
   - `ziyuan-sandbox-windows` 已完成 crates.io 首次手工发布（`1.30.0`）与 GitHub OIDC Trusted Publishing（`release.yml`）配置，并排入 `release.yml` 十二个 crate 发布顺序（位于 `ziyuan-runtime` 之前）。
2. **W2 生产正式接入范围（`run_tests`）**：
   - `ziyuan-runtime` 在 `target.'cfg(windows)'` 下接入 `ziyuan-sandbox-windows`，保持自身 `unsafe_code = "forbid"`；
   - `haoshe::run_shell` 在 Windows `Mode::Auto`／`Mode::Require` 且 `Network::Off`、无外部 `CARGO_TARGET_DIR` 时，经 `probe_capability()` 核验 `FileSystemAndNetworkIsolated` 后调用 `run_shell_sandboxed`（0 能力 `AppContainerChildCapable`＋`PrivateDesktop`＋`KILL_ON_JOB_CLOSE` Job Object），过滤 12 项宿主保留环境变量、按 `max_output` 与 `max_failure_scan` 有界采集输出、支持主动取消与整树排空清理，结果如实标注 `号舍（Windows AppContainer＋Job）`。
3. **W3 首批落地（长连接断网 stdio 外藩）**：
   - `ziyuan-sandbox-windows` 新增 `StdioSpec`、`SandboxedStdio`（`Send`）与 `spawn_stdio_sandboxed`，通过 Win32 匿名双向管道（`CreatePipe`）仅把子端 `[child_stdin_read, child_stdout_write, stderr_nul]` 纳入 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`、父端保持非继承且恢复前核验父端不可见；复用 0 能力 `AppContainerChildCapable`＋`PrivateDesktop`＋`KILL_ON_JOB_CLOSE` Job Object，在 `shutdown()`/`Drop` 时先排空整棵 Job 进程树再回收目录与 profile；
   - `ziyuan-runtime::haoshe::spawn_stdio_windows` 与 `mcp.rs` 接入 Windows W3：对名册写了 `"haoshe": { "network": false }` 且 `write` 为空的本地 stdio 外藩，将项目复制到任务私有只读快照（`readonly_dirs`，保护 DACL＋低 IL，自动跳过 `.ziyuan`、`.git`、`.jj`、`.env*`、构建产物与重解析点；绝不改写宿主 ACL），仅准入任务私有 `scratch` 目录（同步注入 `TMPDIR`）与容器私有 `TEMP`/`TMP`/`HOME` 可写；解析 `PATH` 时自动跳过 `WindowsApps` 等重解析点／应用别名。
4. **W3 后续收官（1.38.0，2026-11-22）：零网络能力命名管道门吏与受控快照子目录可写**：
   - **按域名白名单出站（`ZIYUAN_SANDBOX_ALLOW` 与 MCP `haoshe.network: [...]`）**：实测证实零网络能力（`TokenCapabilities == 0`、零 loopback 豁免）AppContainer 可打开由宿主以保护 SDDL（仅授 `SY`、宿主用户与本任务容器 SID 读写权＋低 IL SACL）创建的命名管道（`\\.\pipe\ziyuan-door-…`），且同 SID 容器内进程间可通过本机回环互通，而 WFP 仍完全阻断容器直连宿主端口及不同 SID 的 peer AppContainer 访问该管道与回环端口。据此 `ziyuan-sandbox-windows` 新增 `PipeDoorStream`、`DoorSpec`、`run_shell_sandboxed_with_door` 与 `spawn_stdio_sandboxed_with_door`，在同 SID 容器与同 Job Object 内拉起纯 `.NET BCL` 异步 TCP↔NamedPipe 桥接并注入 `HTTP_PROXY`／`HTTPS_PROXY`／`ALL_PROXY` 与 `ZIYUAN_HAOSHE_DOOR_PIPE`，由宿主机 `haoshe::proxy` 门吏统一执行域名白名单与阻断审计，Job 排空时桥接原子退出。
   - **受控快照内 `haoshe.write`**：`admit_directories` 按路径深度优先准入外层只读父目录再准入内层可写子目录，使 `haoshe.write` 经严格校验的项目相对子目录仅在任务私有快照内可写，快照父目录／兄弟目录保持 `FRFX` 只读，宿主项目目录零写入且零 ACL 变更。
5. **永久或仍保持失败关闭的边界**：
   - 开放联网 `Network::Open`（含 MCP `"haoshe": true`）显式拒绝（`SandboxUnavailable`），始终坚持零网络能力边界；
   - 宿主外部 `CARGO_TARGET_DIR` 及指向绝对路径、盘符／UNC、家目录（`~`）、`..`、敏感目录（`.ziyuan`/`.git`/`.jj`/`.env*`/`target`/`node_modules`）或重解析点的 `haoshe.write` 显式拒绝（`SandboxUnavailable`）；
   - 宿主 `%USERPROFILE%` 下程序／工具链（如默认 `cargo.exe` 或安装于 `AppData` 的解释器）未经准入仍被内核拒绝，绝不放宽宿主用户目录或系统 ACL；用户可通过 `ZIYUAN_SANDBOX_TOOLCHAIN` 为 `run_tests` 显式声明只读工具链目录（经路径与敏感段严格校验、拒绝重解析点与 `%USERPROFILE%`/系统根目录、按文件数与总字节有界复制到独立任务私有只读快照并前置到容器 `PATH`，结束即毁、零宿主 ACL 变更）。

## 参考

- [Microsoft：Launch an AppContainer／LPAC](https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer)：双主体权限、profile、SECURITY_CAPABILITIES 与 LPAC 创建属性。
- [Microsoft：UpdateProcThreadAttribute](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute)：句柄白名单的继承要求与各属性支持边界。
- [Microsoft：Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)：后代关联、breakaway、嵌套 Job、kill-on-close 与通知限制。
- [Microsoft：OWNER RIGHTS](https://learn.microsoft.com/en-us/windows-server/identity/ad-ds/manage/understand-security-identifiers)：带该 SID 的 ACE 会取代所有者隐含的 `READ_CONTROL`／`WRITE_DAC` 权限。
- [Microsoft：Mandatory Integrity Control](https://learn.microsoft.com/en-us/windows/win32/secauthz/mandatory-integrity-control)：完整性检查先于 DACL；只读权限实验必须排除不同完整性级别带来的拒绝。
