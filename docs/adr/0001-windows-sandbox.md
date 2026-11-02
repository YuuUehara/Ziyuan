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

**W0 当时结果：** 已完成源码、现有原生 CI 和官方接口约束的设计核对，选定有边界的 LPAC＋Job 原型及上述验收顺序。W0 时尚未实现平台 crate、运行隔离探针或完成 unsafe／独立安全审阅；这些是 W1／W2 的继续条件，不虚构为已通过。W1 有条件推进，R0 本地代码索引契约可按正式计划独立安排，不因 Windows 生产接入未完成而无限搁置。

### W1a 实现：独立启动探针，Server 2025 CI 基线已通过

新增未发布的 `ziyuan-sandbox-windows` 工作区成员，仅含独立探针可执行文件；不是通用命令启动 API，不接受用户测试命令，也没有 runtime／MCP 调用者。生产十一 crate 和发布流程不变。根 `unsafe_code = "forbid"` 不动；该实验 crate 使用 `deny`，只有私有 Win32 模块显式允许 FFI，逐处记录指针、所有权、对齐与释放约束。这是实现者自查，不是独立安全签核。

- 每次新建 profile，把自身受信探针复制进去；不复用可写 profile，不给宿主目录加 ACL。显式 Unicode 环境只选择 `SystemRoot` 与指向本任务 profile 的 `LOCALAPPDATA`，按变量名排序；系统根变量缺失则拒绝，不回退继承宿主环境，也不传入宿主 AppData 路径。不授 capability，设置 LPAC 属性，挂起创建、加入 kill-on-close Job 后核对身份与**本任务** Job 归属，再恢复线程。
- 身份以 `TokenIsAppContainer` 检查，AAP opt-out 改用真实令牌的 `AccessCheck` 对照，不再依赖 class 46 的用户态支持。固定合成 DACL 向 Everyone 与 `ALL_APPLICATION_PACKAGES` 授予同一访问位；宿主及单独创建的普通 AppContainer 必须允许，LPAC 必须拒绝，挂起阶段与恢复后的子进程分别检查。访问检查 API 失败绝不当作拒绝通过。普通容器只运行本探针作为显式正对照，不是 LPAC 失败后的回退。
- 白名单只含本任务标准流；另建一个宿主可用、故意设为可继承的匿名事件哨兵，但不放入白名单。宿主用 `GetHandleInformation` 核对哨兵有效且可继承；在线程恢复前，从父进程用 `DuplicateHandle` 查询子进程句柄表：白名单输出句柄必须可复制，哨兵必须明确返回无效句柄，其他错误均失败。复制品只留在父进程，立即关闭，不关闭源句柄。输出 `CHILD_OUTPUT_HANDLE=1 CHILD_SENTINEL_ABSENT=1` 并只读记录严格句柄策略；不让子进程故意引用无效句柄，不关闭保护策略。子进程入口先写出并刷新 `CHILD_ENTERED=1`，再检查身份。LPAC 子进程必须实际写出 `APP=1 AAP_ACCESS=0 JOB=1 ENV=1`，普通容器对应 `AAP_ACCESS=1`；不再声称子进程自行测得 `SECRET=0`，也不输出容易被误认为通用身份查询的 `LPAC=1`。哨兵是模拟秘密句柄，不是真实秘密文件。`ENV` 检查系统根变量存在、LocalAppData 值位于本任务 profile、宿主注入的模拟秘密变量未传入。这是环境值检查，不是路径防换链或文件隔离证明。
- 故障对照在所有挂起身份检查成功后注入恢复前拒绝，走同一终止／等待／清理路径，检查子进程输出为空；不能把入 Job 本身失败误当故障对照通过。等待有时限，无法确认结束时保留 profile 并诊断，不删除或复用不确定资源。
- 关闭句柄、删除自建文件后删除 profile，同时检查存储路径已消失，不把删除 API 返回成功等同无残留。完整进程树、父进程崩溃、超时取消与并发资源生命周期仍属 W1d；当前只运行自己的单进程探针。
- 原生 Windows 测试运行普通容器、LPAC 与恢复前拒绝三个公开可执行入口。CI 固定 `windows-2025`，记录 OS、镜像、宿主提升状态与父 Job 状态；非 Windows 跳过原生测试不是隔离证据，手动运行探针会明确失败，不伪装成功。普通用户及桌面系统对照仍待补验。

本地 Windows MSVC 交叉目标 Clippy 与 Rust 1.89 检查通过；原生结果以以下验收记录为准。合成 DACL 检查是目标版本上的 AAP 授权行为验证，不是微软承诺的通用 LPAC 检测 API，也未证明宿主文件不可访问、网络被拒或后代进程全部正确收尾。以下失败轮次保留定位依据，不代表当前仍在等待同一批验收。

**首轮原生失败记录：** [f0e0c21 的 CI](https://github.com/YuuUehara/Ziyuan/actions/runs/41347088703)五项通过，Windows 两条探针各自及证据步骤重试均报 Win32 203；没有拿到子进程身份／Job／句柄通过证据。实际平台是 Server 2025 `26100`，镜像 `win25-vs2026`／`20261021.278.1`，宿主 `HOST_ELEVATED=1 HOST_JOB=1`。原始空环境块是启动兼容性假设，不将错误号直接解释为 LPAC／Job 不可用。随后仅选择 `SystemRoot` 并增加错误调用位置与环境哨兵，原生结果见下。

**第二轮定位：** [3837fa3 的 CI](https://github.com/YuuUehara/Ziyuan/actions/runs/41347867878)仍是五项通过、Windows 两条失败；位置诊断确认 `CreateProcessW` 返回 203，说明仅补 `SystemRoot` 不足。外部 AppContainer 的[逐变量对照](https://github.com/janhq/jan/issues/9101)指向缺失 `LOCALAPPDATA`，但不代替本仓库的 LPAC 实测。本次只新增指向本任务 profile 的 `LOCALAPPDATA`，并核对子进程实际环境；失败时保留受信探针输出用于诊断。原生 CI 确认前仍不结项，不放宽身份、Job 或句柄检查，也不据提升权限宿主外推普通用户支持。

**第三轮进展与接口修正：** [defbff4 的 CI](https://github.com/YuuUehara/Ziyuan/actions/runs/41348504203)仍有 Windows 失败，但已越过创建与入 Job，错误转为 `GetTokenInformation` 返回 87，尚未恢复线程。`LOCALAPPDATA` 修正已使该基线不再出现启动 203，不等于子进程已运行通过。[微软 Q&A 的版本实测与文档核对](https://learn.microsoft.com/en-us/answers/questions/6011619/gettokeninformation-supported-contract-for-tokenis)指出 class 46 没有完整 Win32 支持契约，SDK 枚举存在不能当作可查询保证。当前改验上述 AAP 授权行为，新增普通容器正对照，仍待本仓库原生 CI；不使用保留属性、原生内核接口或将查询失败改成 `true` 来冒充身份确认。

**第四轮分段结果：** [67e96e1 的 CI](https://github.com/YuuUehara/Ziyuan/actions/runs/41349478196)中恢复前拒绝对照已通过，含挂起 LPAC 的身份、AAP 拒绝、Job 与零输出清理检查。普通容器的挂起检查亦已通过，但恢复后以 `0xc0000008`（无效句柄）退出，尚无入口输出，后续 LPAC 正常运行对照未执行。入口原本先等待故意缺失的哨兵，与异常码吻合但尚无调用栈证明；现改用句柄信息查询，并增加首行启动标记，仍需原生 CI 确认原因和正常运行结果，不将该异常算作句柄隔离成功。

**第五轮入口定位：** [149f6ca 的 CI](https://github.com/YuuUehara/Ziyuan/actions/runs/41350205175)确实收到 `CHILD_ENTERED=1`，随后仍以 `0xc0000008` 退出，恢复前拒绝对照通过；把等待改成 `GetHandleInformation` 并未解决异常。微软[严格句柄策略文档](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-process_mitigation_strict_handle_check_policy)说明无效句柄引用可触发异常；本轮尚无该策略标志或调用栈实测，不把它定为已证明的原因。当前改为上述父进程挂起查询，增加正对照并记录策略，正常普通容器与 LPAC 的结果仍待原生 CI。

**2026-11-01 原生基线验收：** 实现提交 [922af58](https://github.com/YuuUehara/Ziyuan/commit/922af58b985395c722b96bd809bcfa24f08b3f99) 的[六项 CI](https://github.com/YuuUehara/Ziyuan/actions/runs/41350809351)全部通过。[Windows 原生日志](https://github.com/YuuUehara/Ziyuan/actions/runs/41350809351/job/124980335319)在工作区测试和单独证据步骤各运行两项测试，均为 `2 passed; 0 failed`；后者实际输出如下：

| 对照 | 实测结果 |
| --- | --- |
| 普通 AppContainer | `CHILD_ENTERED=1`、`APP=1 AAP_ACCESS=1 JOB=1 ENV=1`、`PROFILE_CLEANED=1` |
| LPAC | `CHILD_ENTERED=1`、`APP=1 AAP_ACCESS=0 JOB=1 ENV=1`、`PROFILE_CLEANED=1` |
| 恢复前拒绝 | `FAULT_BEFORE_RESUME=1 CHILD_BYTES=0 PROFILE_CLEANED=1`，无子进程入口标记 |
| 三个对照共同的挂起检查 | `CHILD_OUTPUT_HANDLE=1 CHILD_SENTINEL_ABSENT=1`、`CHILD_STRICT_HANDLE_FLAGS=3` |

实际平台为 Server 2025 Datacenter `10.0.26100`，镜像 `win25-vs2026`／`20261021.278.1`，宿主 `HOST_ELEVATED=1 HOST_JOB=1`。严格句柄标志证实本基线启用了无效句柄异常且永久生效；移除子进程内的无效句柄引用后，正常探针已通过，不声称获得了旧崩溃的调用栈。W1a 按此原生 CI 基线结项，允许继续 W1b；普通用户与桌面 Windows 补验、文件／网络隔离及后代生命周期仍未完成。实验 crate 仍不发布、不接生产，Windows 受保护执行继续失败关闭，未升级支持等级。

### W1b 首批实现：合成文件访问矩阵，限定原生 CI 已通过

独立可执行入口 `--file-probe` 仍只重执行受信自身，沿用 W1a 的 LPAC、Job、环境与句柄检查，不授 capability、不改 ACL。安全 Rust 文件模块独立于允许 unsafe 的私有 FFI 模块。

- 父进程在独占新建的临时目录建立合成源码、`.ziyuan`、模拟 `.ssh` 与 `.git` 哨兵，另在第二个新建 profile 建立 peer 哨兵。全部先验证宿主写入、读回和写打开成功，随后关闭文件句柄；不读取真实仓库、凭据或其他用户文件。临时目录名含空格与 Unicode；这不是完整路径边界验收。
- LPAC 子进程先读回并修改自己的已准入合成副本，另建私有输出和临时文件，逐一读回内容；再对五类受保护哨兵分别请求读取和写打开。两者均须明确返回 Win32 `ERROR_ACCESS_DENIED`（5），NotFound、共享冲突、异常、超时或无输出均失败。拒绝探针不截断、不写入受保护文件，即使意外授予访问也只报告失败。
- 子进程结束后，父进程独立核对实际私有写入结果、五份受保护哨兵未变，再删除三棵自建目录及两个 profile，确认路径消失。无法确认子进程结束时保留全部 profile／夹具，不让析构删除不确定资源。新增公开可执行测试与原有启动对照一同进入 Windows 原生 CI；交叉编译通过不算原生隔离通过。

实现提交 [c9af11b](https://github.com/YuuUehara/Ziyuan/commit/c9af11b97ef02c248851cd5faa1d4e1fbdf5e1a1) 的[六项 CI](https://github.com/YuuUehara/Ziyuan/actions/runs/41354168120)全部通过。[Windows 原生证据](https://github.com/YuuUehara/Ziyuan/actions/runs/41354168120/job/124990548526)为 `3 passed; 0 failed`：实际私有读写成功，五类读取与写打开均返回 5，父进程核对内容、夹具及两个 profile 清理通过。宿主仍为提升权限且已有父 Job 的 CI 基线。

第二个 profile 没有运行任务，不能称为完整双任务并发隔离验证。大小写／长路径、重解析点／硬链接、启动后换链、普通用户及桌面系统仍待后续批次；网络和后代生命周期仍分别属于 W1c／W1d。本矩阵不结项整个 W1b、不接生产、不升级能力等级。只读依赖的下一批进度单列如下。

### W1b 只读依赖小批：实现已加入，原生 CI 待验

新增 `--readonly-dependency`，沿用并重跑首批私有／宿主／peer 对照。另独占新建依赖目录，复制自身受信可执行文件和非映像数据哨兵；不使用真实项目依赖、不启动任意命令、不增加 capability。LPAC 直接从该目录启动同一受信探针，不额外派生执行器，以实际入口及规范路径核对读取／执行允许。

- 只修改这个自建依赖目录的 DACL，禁止父级权限继承；SYSTEM 与宿主用户保留完整权限，本任务 AppContainer SID 只获可继承的读取／执行。`OWNER RIGHTS` ACE 限制隐含所有者 `WRITE_DAC`，防止同用户身份改回写权限；不设置 DOS 只读属性，不改其他宿主目录或原有拒绝夹具的 ACL。
- 同时建立独立可写对照子目录及未映射的映像副本，仅该对照向本任务 SID 授完整访问。两组目录／文件均显式设低完整性标签；父子分别查询对象标签，子进程还核对自身令牌完整性，均须为 `4096`。同一 LPAC 必须先取得可写对照的全部逐项权限，并实际完成写入、读回、改名、新建和删除；随后才接受只读组拒绝，不能把 medium／low 完整性差异当作 DACL 生效。
- 父进程在策略生效后、子进程启动前，用相同文件 API 验证目录及两文件的写入、追加、扩展属性、属性、删除、改 DACL、改所有者权限均可取得；目录另验 `DELETE_CHILD`。句柄全部立即关闭。子进程对这些权限逐项请求，必须明确返回 5，不能只用一次 `GENERIC_WRITE` 的部分拒绝冒充全部写权限被拒。
- 实际删除／改名使用未映射的数据文件，另试新建文件；均须明确拒绝，避免把运行中 EXE 的映像锁当成只读策略证据。父进程在退出后逐字节核对依赖映像、数据和目录条目，再清理并检查路径消失；无法确认子进程终止时，依赖目录与其他夹具一并保留。

**首轮原生失败：** [bb8a148 的 CI](https://github.com/YuuUehara/Ziyuan/actions/runs/41371781992)五项通过、Windows 失败。子进程实际进入、身份／私有读写／五类拒绝均通过，随后新增依赖检查返回错误 5；尚无完整依赖允许／拒绝证据，原日志不足以区分具体失败调用。当前将只读 ACE 从通用 `GRGX` 改为文件专用 `FRFX`，不增加写权限，同时为文件允许对照和完整性标签查询补足错误位置／阶段；该调整仍待原生对照验证，不将掩码假设写成已确认原因，不跳过失败检查。

本批尚待原生 CI，不凭交叉编译宣称 DACL 生效。保护依赖目录父级的换链／替换、重解析点、硬链接、真实工具链依赖和普通用户兼容性不在本批验收内，仍不构成完整 W1b 或生产安全签核。

## 参考

- [Microsoft：Launch an AppContainer／LPAC](https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer)：双主体权限、profile、SECURITY_CAPABILITIES 与 LPAC 创建属性。
- [Microsoft：UpdateProcThreadAttribute](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute)：句柄白名单的继承要求与各属性支持边界。
- [Microsoft：Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)：后代关联、breakaway、嵌套 Job、kill-on-close 与通知限制。
- [Microsoft：OWNER RIGHTS](https://learn.microsoft.com/en-us/windows-server/identity/ad-ds/manage/understand-security-identifiers)：带该 SID 的 ACE 会取代所有者隐含的 `READ_CONTROL`／`WRITE_DAC` 权限。
- [Microsoft：Mandatory Integrity Control](https://learn.microsoft.com/en-us/windows/win32/secauthz/mandatory-integrity-control)：完整性检查先于 DACL；只读权限实验必须排除不同完整性级别带来的拒绝。
