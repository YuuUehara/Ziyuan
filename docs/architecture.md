# ZiYuan 设计文档（v0.2.0）

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
                  └─ Runtime::Scheduler (Tokio：并发上限 / 超时 / 撤销)
                      └─ DemoBackend | AgentBackend (ziyuan-llm + 只读工具)
```

`Office` 和 `Agent` 是独立对象；官职任免修改 office map，但不更改 Agent ID。制度迁移自动清理协调层的旧职务，并保持六部任职关系。

### 玉玺（Yuxi）

工具权限由诏令携带的 `Seal { capabilities, scope }` 决定，而不是信任 Agent 声称的身份：

1. 皇帝下诏时给定玉玺（CLI `--tools` / `--scope`，默认全部只读工具、整个项目）。
2. `ziyuan-governance` 在 `new_edict` 与回放时校验：范围非空，且均为不含 `..` 的相对路径。手写的日志也不能绕过。
3. 玉玺随 `EdictIssued` 写入事件簿，与 `ToolInvoked` 一起构成审计记录。
4. `AgentBackend` 每件诏令调用 `tools::sealed_tools(root, &seal)`：只构造被授予的工具；范围 canonicalize 后必须在项目内。工具每次访问都再次 canonicalize 检查，`.ziyuan`、`.git`、`.jj` 与 `.env*` 始终拒绝。

尚未实现：按官职设定权限上限（目前各官职的工具相同）、皇帝的非 CLI 身份验证、写入类工具的审批。

## 3. 分层与 Event Sourcing

```text
ziyuan-cli ──┬──▶ ziyuan-web (axum, SSE) ──┐
             ├──▶ ziyuan-tui (ratatui) ───┤
             │                            ▼
             ├──────────────────────▶ ziyuan-app (Session) ──▶ ziyuan-governance (Court)
             │                            ├──▶ ziyuan-runtime (Scheduler / WorkerBackend)
             │                            └──▶ ziyuan-storage (EventStore)
             └──▶ ziyuan-llm ──▶ ziyuan-runtime (ModelProvider)
（所有 crate 依赖 ziyuan-core 领域类型；runtime 不依赖 governance）
```

- `ziyuan-core`：纯数据类型与 `ParseError`，无行为依赖。
- `ziyuan-governance`：`Court` 字段私有，只能通过 `Court::apply(&CourtEvent)` 改变；`apply` 与 `propose_*` 共用同一套规则，返回 `GovernanceError`。官制（`Office::all` / `exists_in` / `rank`）定义在 core，署理规则在 `Court::route`。`names::NameGenerator` 由种子生成姓名；种子由 `ziyuan-app` 随机给出，生成的姓名随 `AgentRegistered` 落盘，回放不依赖随机数。官职代码与 v0.1 兼容，旧事件簿可直接回放。`WorkerBackend::model_name` / `ModelProvider::model_name` 供各前端显示承办模型，`price()` 返回单价（`ziyuan_core::Price`）供估算花费；单价表与 `ZIYUAN_LLM_PRICE` 解析在 `ziyuan-llm::price_for`。工具调用轮次的前置文本与思维链记为 `Message::CallPrelude`，与其后的 `ToolCall` 编码为同一条 assistant 消息（DeepSeek 思考模式要求回传 `reasoning_content`）。内阁制下诏令须有 `EdictApproved`（批红）才进入 `Court::runnable_tasks`；为兼容旧事件簿，`apply(TaskAssigned)` 不检查批红，由 `Session::run` 跳过未批红的诏令。`OfficialPurged`（革职）后不得再任官，`propose_abolition` 一次提交「革去全部丞相 + 改为亲政」。改制时的史官记述（`reform_note`）只是呈现，不入事件簿。
- `ziyuan-storage`：`EventStore { load, append(expected, events) }`，`append` 只在事件数等于 `expected` 时写入（乐观并发）；`EventLog`（`.ziyuan/court.log`）与 `MemoryEventLog`。编码在私有 `codec` 模块，与文件 I/O 分离。
- `ziyuan-runtime`：`Scheduler` 并发执行已指派的 `Job`，经通道回报 `Progress`（工具调用与结局）；不了解治理规则。Adapter 错误统一为 `BoxError`。`AgentBackend` 运行 `agent_loop`，`tools` 提供只读工具。
- `ziyuan-llm`：`OpenAiCompatible` 实现 `ModelProvider`。HTTP/TLS 依赖只在此 crate，runtime 保持无网络依赖。
- `ziyuan-app`：`Session<S: EventStore>` 是唯一的写入路径。每个用例：治理层提议事件 → 在 `Court` 副本上 `apply` 校验 → `EventStore::append` → 替换内存状态。被拒绝的操作不留事件；写入的事件一定可以回放。
- `ziyuan-cli`：把参数解析为 `Command`，调用 `Session`，打印结果；`serve` 启动 Web 界面，`tui` 启动终端界面。
- `ziyuan-web`：紫禁城 Web 界面。内嵌单页（`index.html`，无构建步骤）+ JSON API + SSE。每个请求在阻塞线程中各自 `Session::open`，与 CLI 一样受乐观并发保护；`POST /api/run` 取得执行者锁后在后台执行全部未结束的诏令，`RunUpdate` 经 SSE（`text` / `finished` / `changed`）推送。仅监听 127.0.0.1，全部 API 需 `Authorization: Bearer <令牌>`（SSE 用 `?token=`），令牌由 `/dev/urandom` 生成并只打印在启动输出中。中文描述（`describe_event` 等）移到 `ziyuan-app`，各前端共用。
- `ziyuan-tui`：紫禁城终端界面（ratatui）。`App` 不依赖真实终端：按键经 `App::handle`、画面经 `App::draw`，测试用 `TestBackend` 驱动。执行在 Tokio 任务中调用 `Session::run_open`（与 Web 共用：反复执行未结束的诏令直到队列清空），进度经通道送回界面；每秒 `Session::reload` 以看到其他进程的变更。

初始化生成 `Founded`, `AgentRegistered`, `OfficialAppointed`。诏令的事件序列：

```text
EdictIssued ──▶ TaskAssigned ──▶ ToolInvoked* ──▶ TaskCompleted | TaskFailed | TaskCancelled
     └──────────────────────────────────────────▶ TaskCancelled（排队中撤销）
```

`Session::issue` 只写 `EdictIssued`（路由不通则拒绝）。`Session::run` 按当前制度逐件写 `TaskAssigned`，交给 `Scheduler`，再把每条 `Progress` 作为事件提交。`Court::apply` 校验生命周期：结束后的诏令不能再变更；`ToolInvoked` 只能出现在已指派的诏令上；完成者必须是被指派者。未结束且已指派的诏令（进程中断）可重新指派。v0.1 日志中没有 `TaskAssigned` 的完成记录仍可回放。`Court::replay` 根据完整日志恢复当前朝廷状态；`InstitutionReformed` 在回放时清理最高协调职务。

### 多进程

- **读写锁**：`EventLog::load` 持共享文件锁，`append` 持独占文件锁，并在锁内核对事件数后整批写入、`fsync`。读者不会看到写了一半的批次。
- **乐观并发**：`Session::commit` 接收一个基于 `Court` 生成事件的闭包。若事件簿已被其他进程更新（`Conflict`），重新加载、重放并再次调用闭包，因此诏令/官员 ID 与规则检查总以最新状态为准，不会产生重复 ID。
- **残行**：末尾没有换行符的行来自崩溃中断的写入，读取时忽略，下次追加前截掉。多事件批次（仅 `init`）若在写入中途崩溃，可能留下完整的前几行。
- **执行者锁**：`.ziyuan/executor.lock`（`try_lock`，进程退出即释放）。同一时刻只有一个进程执行诏令，因此“已指派但未结束”的诏令只可能来自中断的进程，`run` 可以安全地重新指派。其他进程仍可入队、撤销、任免；`run` 在每次提交时都会看到这些变化：被撤销的诏令跳过，执行中被撤销的结果作废。
- **代价**：每次追加会重读全文（O(n)）。后续迁移 SQLite 时用单事务实现同一 `append(expected, events)` 语义，并补充快照与 Schema 版本。

## 4. Runtime 与 LLM

`ziyuan-runtime::WorkerBackend` 是实际执行者接口（同步，`Send + Sync`）；`DemoBackend` 不执行任务。

用量的路径：`OpenAiCompatible` 请求 `stream_options.include_usage`，从流的最后一块（或普通 JSON 的 `usage`）读出 `prompt_tokens`/`completion_tokens` → `ExecutionHooks::model_usage` → `Progress::Usage` → `Session::run` 提交 `ModelUsage { task, usage }`。指派后任何时刻都可记录（超时后才返回的调用也计入）；`Task::usage`、`ServiceRecord::usage` 与 `Court::total_usage` 由回放累加。

流式文本的路径：`text_delta` → `Progress::TextDelta` → `RunUpdate::Text` → CLI（stderr，按诏令 ID 分行）。它只用于显示，不进入事件簿。

`Scheduler` 基于 Tokio：信号量限制并发，每件任务在阻塞线程池中执行；许可随执行线程释放，因此超时后仍在收尾的线程也计入并发数。取消是协作式的：超时或 `CancelToken` 触发时调度器立即给出结局，并通过 `ExecutionHooks::is_cancelled` 通知后端；`agent_loop` 在每轮模型调用前与执行工具前检查，`OpenAiCompatible` 在流式响应的每一行检查并中止请求。撤销时尚未开始的任务报告为 `NotStarted`，保持未结束。CLI 退出时不等待仍阻塞的 HTTP 请求。

`ziyuan-runtime::agent_loop` 提供独立的 `ModelProvider`, `Tool`, `ModelTurn`, `Message` 等抽象，并实现最小 Tool Calling 循环。一轮内的全部工具调用先于其输出写入消息（与 Chat API 的顺序一致）；任一调用未注册则整轮拒绝，不执行任何工具。

`ziyuan-llm::OpenAiCompatible` 对接 OpenAI 兼容的 `/chat/completions`（同步，`stream: true`；SSE 中的文本经 `ExecutionHooks::text_delta` 实时上报，工具调用按 `index` 拼接；服务若忽略 `stream` 而返回普通 JSON 也能处理），由 `ZIYUAN_LLM_MODEL` / `ZIYUAN_LLM_BASE_URL` / `ZIYUAN_LLM_API_KEY` 配置。

`ziyuan-runtime::tools::read_only_tools(root)` 提供 `read_file`（64 KiB 截断）与 `search_code`（字面量、最多 50 条）。路径先 canonicalize 再检查是否在根目录内，因此 `..`、绝对路径与指向外部的符号链接都被拒绝；搜索跳过 `.git`、`.jj`、`.ziyuan`、`target`。

此 Loop **还不是生产级别**：未实现 API retry policy、token budget、并行工具、工具输入 JSON Schema 验证、沙箱、模型响应结构检验、prompt injection 隔离、trace IDs、上下文压缩。

## 5. 吏部与官员升迁

- **评定**：`TaskReviewed { task, verdict, note }`，只能针对已完成的诏令，每件一次；`Court::apply` 在回放时同样检查。目前唯一的评定者是 CLI 皇帝。
- **考绩**：`Court::service_record(agent)` 完全由事件簿计算：采纳、驳回、待评定、失败（`TaskStatus::Failed` 记录了失败时的承办者）、工具调用与失败次数。撤销不计；未评定的回奏不计入评价。
- **荐举**：`Court::recommend(office)` 列出专长与现行制度都符合的官员，按 `采纳 − 驳回` 降序，再按失败、工具失败升序。它不产生事件，任命仍走 `propose_appointment`。
- 不把人格、模型生成的赞赏、完成次数或互相投票当成评价依据。

尚未实现：自动验证（回归测试、检查回奏引用的文件与行号是否属实）、成本与 token 统计、官员提交任命建议的 `AppointmentProposal` 事件。

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

1. 初始丞相制的路线包含中书右丞相；右丞相空缺时由左丞相署理，尚书空缺时由侍郎署理。
2. 改为 direct 后的同类任务路线不包含丞相。
3. 改为 cabinet 时若首辅空缺必须拒绝派工；任命后路由包含首辅。
4. 不允许在废相制度中任命丞相或内阁官职；不能双丞相。改制只撤销旧协调机构的官职，六部官员保留、品级随制度变化。
5. 中文命令和换行内容能正确写入/解码事件簿。
6. `DemoBackend` 回奏明确包含 `[DEMO ONLY]`，以免误以为进行了代码检查。
