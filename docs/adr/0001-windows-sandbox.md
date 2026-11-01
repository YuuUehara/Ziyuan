# ADR-0001：Windows 号舍支持政策

- 状态：已采纳（方案 A）
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

2026-11-01：Windows 原生号舍已纳入[正式开发计划](../code-task-next-batch.md#五windows-原生号舍先立边界再接执行)，分 W0 设计与启动条件、W1 隔离原型、W2 工具接入验收。列入计划不等于再评估条件已经全部满足；W0 记录方案 B 的启动决定，W2 通过后才修改当前支持状态。此前方案 A 的失败关闭规则继续有效。

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

**W0 结果：** 已完成源码、现有原生 CI 和官方接口约束的设计核对，选定有边界的 LPAC＋Job 原型及上述验收顺序。尚未实现平台 crate、运行隔离探针或完成 unsafe／独立安全审阅；这些是 W1／W2 的继续条件，不虚构为已通过。W1 有条件推进，R0 本地代码索引契约可按正式计划独立安排，不因 Windows 生产接入未完成而无限搁置。

## 参考

- [Microsoft：Launch an AppContainer／LPAC](https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer)：双主体权限、profile、SECURITY_CAPABILITIES 与 LPAC 创建属性。
- [Microsoft：UpdateProcThreadAttribute](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute)：句柄白名单的继承要求与各属性支持边界。
- [Microsoft：Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)：后代关联、breakaway、嵌套 Job、kill-on-close 与通知限制。
