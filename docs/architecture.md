# ZiYuan 设计文档（v0.1）

## 1. 核心目标

既能作为普通 Agent Runtime 使用，又允许用户变更治理结构（丞相制 / 废相亲政 / 内阁制）。制度切换应改变执行路径、职权和审批流程，而不只是改写 System Prompt。

## 2. 数据与权限边界

```text
Emperor (human / authenticated control plane)
    ├─ institution reform (user-only)
    ├─ appointment / dismissal (user-only in v0.1)
    └─ edict
       └─ Governance::route(edict)
          ├─ Chancellor Coordinator OR
          ├─ Direct to Department OR
          └─ Cabinet First Secretary
              └─ Minister → Clerk (Worker)
                  └─ Runtime::dispatch(WorkerBackend)
                      └─ DemoBackend | RealLLMBackend (future)
```

`Office` 和 `Agent` 是独立对象；官职任免修改 office map，但不更改 Agent ID。制度迁移自动清理协调层的旧职务，并保持六部任职关系。

工具的实际访问权限还没有在 v0.1 实现；未来应从签发者权限、官职与任务上下文中取交集（least privilege），而不是信任 Agent 声称的身份。

## 3. 分层与 Event Sourcing

```text
ziyuan-cli ──▶ ziyuan-app (Session) ──▶ ziyuan-governance (Court)
                    │        │                    ▲
                    │        └──▶ ziyuan-runtime ─┘ (dispatch / WorkerBackend)
                    └──▶ ziyuan-storage (EventStore)
            （所有 crate 依赖 ziyuan-core 领域类型）
```

- `ziyuan-core`：纯数据类型与 `ParseError`，无行为依赖。
- `ziyuan-governance`：`Court` 字段私有，只能通过 `Court::apply(&CourtEvent)` 改变；`apply` 与 `propose_*` 共用同一套规则，返回 `GovernanceError`。
- `ziyuan-storage`：`EventStore { load, append }`；`EventLog`（`.ziyuan/court.log`）与 `MemoryEventLog`。编码在私有 `codec` 模块，与文件 I/O 分离。
- `ziyuan-runtime`：`dispatch` 按 `Court::route` 调用 `WorkerBackend`；Adapter 错误统一为 `BoxError`。
- `ziyuan-app`：`Session<S: EventStore>` 是唯一的写入路径。每个用例：治理层提议事件 → 在 `Court` 副本上 `apply` 校验 → `EventStore::append` → 替换内存状态。被拒绝的操作不留事件；写入的事件一定可以回放。
- `ziyuan-cli`：把参数解析为 `Command`，调用 `Session`，打印结果。未来的 TUI/Web 复用 `Session`。

初始化生成 `Founded`, `AgentRegistered`, `OfficialAppointed`。运行时追加 `EdictIssued`, `TaskCompleted`。`Court::replay` 根据完整日志恢复当前朝廷状态；`InstitutionReformed` 在回放时清理最高协调职务。

注意：v0.1 的 EventLog 只适合单进程演示。后续迁移 SQLite 的单事务批次写入；另需锁、Schema 版本、幂等键、快照与事件去重。

## 4. Runtime 与 LLM

`ziyuan-runtime::WorkerBackend` 是实际执行者接口；`DemoBackend` 不执行任务。

`ziyuan-runtime::agent_loop` 提供独立的 `ModelProvider`, `Tool`, `ModelTurn`, `Message` 等抽象，并实现最小 Tool Calling 循环。真实 LLM Provider 尚未实现，当前同步执行，后续可以改为 async。

此 Loop **还不是生产级别**：未实现 API retry policy、token budget、并行工具、工具输入 JSON Schema 验证、沙箱、模型响应结构检验、prompt injection 隔离、trace IDs、上下文压缩。

## 5. 吏部与官员升迁（后续）

推荐：
- 以任务验证通过率、回归测试、质量与成本为基础的绩效指标。
- 官员可以提交任命建议，但不得直接授予自己系统权限。
- 准备 `AppointmentProposal` 与 `HumanApproval` 独立事件类型。
- 不应把人格、模型生成的赞赏或互相投票当成唯一评价依据。

## 6. 建议的下一步接口

`EventStore` 已落地（文件与内存两种实现）；SQLite 实现只需再实现该 trait，并让 `append` 成为单事务。

```rust,ignore
trait GovernancePolicy {
    fn route(&self, court: &Court, edict: &Edict) -> Result<Route, GovernanceError>;
    fn authorize(&self, principal: Principal, action: Action) -> Result<(), GovernanceError>;
}
```

`GovernancePolicy` 暂不实现：目前三种制度由 `GovernanceMode` 枚举确定性路由，只有一个实现时抽象没有收益。授权（`Principal`/`Action`）应放在 `Session` 入口处检查。

## 7. 验收标准

1. 初始丞相制的路线包含丞相。
2. 改为 direct 后的同类任务路线不包含丞相。
3. 改为 cabinet 时若首辅空缺必须拒绝派工；任命后路由包含首辅。
4. 不允许在废相制度中任命丞相；不能双丞相。
5. 中文命令和换行内容能正确写入/解码事件簿。
6. `DemoBackend` 回奏明确包含 `[DEMO ONLY]`，以免误以为进行了代码检查。
