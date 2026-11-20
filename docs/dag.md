# DAG 排程速查

本文记录 DAG 分办排程的设计与已交付边界，供创建带依赖的子诏令时查阅；该事件（`DAG`）自 `1.0.0-rc.2` 引入，现行边界与 1.0 兼容约定一致。

## 改动概览

- `EdictDag` 是追加事件，与 `EdictSplit` 在同一提议中写入；旧事件不改。
- 写子诏令默认按稳定拓扑顺序自动串行（同样可办时按提交顺序）；在可用 `jj` 版本库中可显式启用并行写（`--parallel-writes` / Web 勾选「并行写」），各写子诏令执行期自动获独立临时 `jj workspace` 隔离；只读子诏令始终可并行。
- 依赖须已完成且无改动或改动已准奏落盘，不能把暂存回奏当作已可读的产物。上游失败、撤销或驳回改动会使下游保持受阻，不自动重试或降级；皇帝可撤销受阻子诏令，让尚书按各件结局汇总。
- 节点可携尝试次数上限、期限、度支、约定产出、另行批红要求与幂等键。到期或次数用尽时记失败；约定产出写入承办与汇总提示。
- CLI `--json`、Web `/api/state` 与 TUI 展示依赖及受阻状态。

## 主要落点

| 区域 | 改动 |
| --- | --- |
| `ziyuan-core` | `DagNode`、`CourtEvent::EdictDag`、`DagSubtaskAdded`、`DagDependenciesUpdated` |
| `ziyuan-governance` | DAG 校验、自动串行与显式并行写提议、运行中追加与改依赖、指派门控、批红与 `blocked_by` |
| `ziyuan-storage` | `DAG`（序号 12，`1.0.0-rc.2`）与 `DAGADD` / `DAGDEPS`（序号 55，`1.21.0`）编码/回放与兼容样本 |
| `ziyuan-app` | `ScheduledSubtask`、`AddedSubtask`、`Session::split_scheduled` / `dag_add_subtask` / `dag_update_deps`、并发写 `jj workspace` 隔离与运行时排程检查 |
| CLI / Web | `split --plan` / `--parallel-writes`、`dag add|deps`、Web「御定排程」／「运行中追加子诏令」／「改依赖」与排程查询 |
| TUI | 依赖、受阻展示；自定义创建与运行中调整仍用 CLI / Web |
| 测试 | 校验拒绝情形、调度与失败阻塞、提示注入、API JSON、TUI 状态徽章 |

## 使用与边界

日常仍用既有分办流程：

```bash
ziyuan split 3
ziyuan split 3 4:改 storage 7:改 runtime
ziyuan split 3 --plan docs/examples/dag-plan.json
ziyuan split 3 --parallel-writes --plan docs/examples/dag-plan.json
ziyuan run
```

`--plan` 读 JSON 数组，示例见 [examples/dag-plan.json](examples/dag-plan.json)；不要与自动分办或 `官员:内容` 混用。Web 选择待办、尚未分办的父诏令，点「御定排程」，提交 2–8 个节点。两者都直接进入同一治理校验；循环、重复/越界/自身依赖、不合法官员、零次数/预算与空/重复键均拒绝，不留下部分子任务。输入字段拼写错误会报错，不会当作模型分办。

字段：`worker` 为本部在任非尚书官员 ID，`text` 为任务内容，`dependencies` 是**当前数组中从 1 开始的节点序号，不是诏令 ID**；可向后引用，但不可成环。可选字段为 `attempts`（正整数次数上限）、`deadline`（Unix 秒；Web 输入本地时间后转换）、`budget`（正整数 tokens 软限额）、`artifact`（约定产出提示，不是文件存在性保证）、`approval_required`（内阁制下另须批红）及 `idempotency_key`（父任务内唯一的非空节点键）。写权限计划会按拓扑顺序补齐串行安全边，不强制数组正向依赖；只读计划可并行。`court --json`、`tasks --json` 和 Web `/api/state` 可查询保存结果。

API：`POST /api/tasks/{id}/split` 的正文为 `{ "schedule": [...], "parallel_writes"?: bool }`，返回 `{ "subtasks": [实际诏令ID, ...] }`。空正文或 `{}` 仍是既有尚书分办。只读令牌不可提交。

**并行写与 `jj workspace` 隔离（`1.20.0`）**：当同一批 `ziyuan run` 并发执行 ≥ 2 件含写／测权限的诏令且项目位于 `jj` 版本库中时，调度前自动为每件写诏令经 `jj workspace add --name ziyuan-ws-* -r @ <临时目录>` 检出独立工作副本，工具与 `run_tests` 均在各自副本内运行，执行线程彻底退出后随租约（`WorkspaceLease`）自动 `jj workspace forget` 并清理临时目录（同时自动回收历史异常中断残留的孤立工作副本）。可通过环境变量 `ZIYUAN_JJ_WORKSPACE=auto|always|off` 控制（默认 `auto`）。在 `split` 中显式启用 `--parallel-writes` / `parallel_writes: true` 时，若项目不在可用 `jj` 版本库中将直接失败关闭；即使并行写执行成功，落盘仍须皇帝逐件准奏，互不重叠的文件各自落盘，改到同一文件时后准奏者因 `FileChange.before` 原文已变而被安全拒绝、不会静默覆盖。

**幂等重试与运行中调整（`1.21.0`）**：相同父任务重复提交同一 `split --plan` 有效方案，仍返回原子任务 ID、不追加事件（内容、官员或有效元数据变化则拒绝，不能借重试改写原分办）。自 `1.21.0` 起，在**原诏令尚未结束（Pending）**的前提下，支持两项显式运行中调整（通过新事件 `DAGADD` / `DAGDEPS` 记入事件簿，完整可重放，仍严守单层 2–8 件子诏令上限与环路校验）：
- **追加子诏令**：`ziyuan dag add <父诏令ID> <官员ID:子诏令> [--deps <ID,ID>] [--artifact <产出>] [--attempts <N>] [--budget <N>] [--deadline <Unix秒>] [--approval] [--key <标识>]`，或 Web `POST /api/tasks/{id}/dag/add`（总子诏令数不得超过 8 件，依赖填同父子诏令的实际 ID，内阁制下遵守 `approval_required`）。
- **调整待办子诏令依赖**：`ziyuan dag deps <子诏令ID> (--deps <ID,ID> | --clear)`，或 Web `POST /api/tasks/{id}/dag/deps` `{ "dependencies": [...] }`。**仅允许修改尚未开始（`Pending`）的子诏令**，已指派、已完成、已失败或已撤销的子诏令不可改写；当上游子诏令失败或被撤销导致下游受阻时，皇帝可显式改线或清除断裂依赖，使下游恢复可调度而无需放弃已完成的前置成果。

节点键不在不同父任务之间去重，也不保证模型调用、工具调用或任意外部副作用恰好一次。GitHub 回报/PR 与本地提交的专用幂等机制仍独立工作。

准奏上游后再次 `ziyuan run`，后件才能读取落盘产物；绝不自动应用或共享未经批准的暂存改动。逐段驳回后实际落盘的仍只是获准部分，不保证满足下游的语义前提，须人审。期限、次数上限不是自动重试策略，产物名与节点键也不是译稿缓存或文件锁。

## 版本策略

`DAG` 自 `1.0.0-rc.2` 发布，最低读取版本登记为序号 12（`release_name(12) = 1.0.0-rc.2`）。`1.21.0` 新增 `DAGADD` 与 `DAGDEPS` 事件，最低读取版本登记为序号 55（`release_name(55) = 1.21.0`）；未使用运行中调整的旧事件簿不提高读取门槛。

## 已采用的决定

1. **运行中调整边界（1.21.0）**：允许在原诏令未结束前显式追加子诏令（仍守单层 2–8 上限）与修改尚未开始（`Pending`）子诏令的前置依赖（严禁成环与改写已开始／已结束节点），由 `DAGADD`/`DAGDEPS` 独立事件审计回放。
2. **失败处置**：上游失败或改动驳回不会自动重试，也不会让下游降级运行；下游继续受阻，皇帝可通过 `ziyuan dag deps`／Web「改依赖」改线或清除断裂依赖，也可撤销或另行分办。
3. **审批交接**：有改动时，以准奏后的磁盘为交接点，安全优先于无人值守的一次跑完。

## 验证

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- 真实模型复验与未验收边界见 [rc.2 验收记录](acceptance/1.0.0-rc.2.md)。
