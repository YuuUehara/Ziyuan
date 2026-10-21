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

## 3. Event Sourcing

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

```rust,ignore
trait GovernancePolicy {
    fn route(&self, court: &Court, edict: &Edict) -> Result<Route, Error>;
    fn authorize(&self, principal: Principal, action: Action) -> Result<(), Error>;
}

trait CourtStorage {
    fn append_transaction(&mut self, events: &[CourtEvent]) -> Result<(), Error>;
    fn load_stream(&self) -> Result<Vec<CourtEvent>, Error>;
}
```

暂不实现这些抽象，以免 MVP 中只有一个实现且产生过度工程化。当前的 `Court` 和 `EventLog` 已是明确的重构边界。

## 7. 验收标准

1. 初始丞相制的路线包含丞相。
2. 改为 direct 后的同类任务路线不包含丞相。
3. 改为 cabinet 时若首辅空缺必须拒绝派工；任命后路由包含首辅。
4. 不允许在废相制度中任命丞相；不能双丞相。
5. 中文命令和换行内容能正确写入/解码事件簿。
6. `DemoBackend` 回奏明确包含 `[DEMO ONLY]`，以免误以为进行了代码检查。
