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

## 参考

- 《ZiYuan_开发路线图与Diff优化建议_含Windows沙箱决策_2026-10-29.md》§3（含 ADR 草案 §3.5）
- Microsoft Learn：AppContainer isolation、Restricted Tokens、CreateProcessInSandbox（实验性 API）
