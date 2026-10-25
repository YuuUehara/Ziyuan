# ZiYuan 设计文档（v0.3.0）

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
- `ziyuan-governance`：`Court` 字段私有，只能通过 `Court::apply(&CourtEvent)` 改变；`apply` 与 `propose_*` 共用同一套规则，返回 `GovernanceError`。官制（`Office::all` / `exists_in` / `rank`）定义在 core，署理规则在 `Court::route`。`names::NameGenerator` 由种子生成姓名；种子由 `ziyuan-app` 随机给出，生成的姓名随 `AgentRegistered` 落盘，回放不依赖随机数。官职代码与 v0.1 兼容，旧事件簿可直接回放。`WorkerBackend::model_name` / `ModelProvider::model_name` 供各前端显示承办模型，`price()` 返回单价（`ziyuan_core::Price`）供估算花费；单价表与 `ZIYUAN_LLM_PRICE` 解析在 `ziyuan-llm::price_for`。官职配置由 `OfficeModelSet` / `OfficeCeilingSet` 事件记录；`Session::run` 在指派时以 `Court::effective_seal`（`Seal::within`）求出受上限约束的玉玺、以 `Court::model_of` 选模型，写入 `Job` 与 `TaskAssigned.model`；`WorkerBackend::execute(&Job, hooks)`，`AgentBackend` 经 `ModelCatalog`（`ziyuan-llm::Catalog`）按名构造模型并查单价，`ziyuan-app::estimate_cost` 按各诏令实际模型汇总花费。多官协作：`EdictSplit{task,subtasks,note,model,usage}` 由 `Court::propose_split` 校验（待办、未分办、非子诏令、2–8 件、承办者为本部在任官员），子诏令以 `Task.parent` / `assignee` 记录，原诏令记 `Task.split`。`runnable_tasks` 让子诏令先行（须原诏令已批红），原诏令待子诏令全部结束后由 `route_task`（`Pick::Head`）派给尚书；`Session::run` 以 `summary_prompt` 替换汇总作业的内容并收回写权。`Session::plan_split` 经 `WorkerBackend::complete` 请尚书拆分，`split` 为御定；`overheads` 汇总票拟、拆分、复核三种额外开销供计价。都察院：`Office::Censor` 不属任何制度（`mode()` 为 `None`），不入 `route`；`CensorReported{task,censor,verdict,note,evidence,model,usage}` 由 `Court::propose_censure_check` 校验（已完成、未复核、现任御史、回避经手者），存于 `Task.censure`，用量并入 `Task.usage`。`Session::censor` 先用 `check_evidence`（引用解析见 `citations`，测试取最近一次 `run_tests` 的 `ToolInvoked.ok`）得出 `Evidence`，再经 `WorkerBackend::complete` 请御史下结论，失败时按 `Evidence::verified` 兜底。`ServiceRecord` 新增 `endorsed` / `impeached` / `verified`，荐举按 `merit()` 排序；`kaocheng_report` 只是呈现，不入事件簿。工部写权限：`Capability::WriteFile` / `RunTests` 只能盖在工部诏令上（`validate_edict_seal`）；`tools::sealed_tools` 返回 `Sealed`，`write_file` 写入其内存暂存区，执行成功后 `AgentBackend` 经 `ExecutionHooks::changes_proposed` 呈报，`Session::run` 在 `TaskCompleted` 之前记 `ChangesProposed`（`Task.changes`，重新指派时清空）。`Session::apply_changes` 先调 `tools::apply_changes`（核对原文、全有或全无）再记 `ChangesApplied`，`discard_changes` 记 `ChangesDiscarded`；`render_diff` 生成供各前端显示的统一 diff。工具调用轮次的前置文本与思维链记为 `Message::CallPrelude`，与其后的 `ToolCall` 编码为同一条 assistant 消息（DeepSeek 思考模式要求回传 `reasoning_content`）。内阁制下诏令须有 `EdictApproved`（批红）才进入 `Court::runnable_tasks`。票拟记为 `EdictDrafted{task,worker,note,model,usage}`（`Court::propose_draft` 校验内阁制、未批红且拟派者是 `draft_candidates` 中的本部在任官员），存于 `Task.draft`，用量并入 `Task.usage`；`Session::draft` 经 `WorkerBackend::complete`（单轮、无工具）请首辅模型拟票，失败时按旧例拟派并注明；改票 `Session::redraft` 也是一条 `EdictDrafted`。`Session::run` 用 `Court::route_task` 派工：有票拟按票拟，否则（中旨或拟派者已离任）按 `route` 的旧例；为兼容旧事件簿，`apply(TaskAssigned)` 不检查批红，由 `Session::run` 跳过未批红的诏令。`OfficialPurged`（革职）后不得再任官，`propose_abolition` 一次提交「革去全部丞相 + 改为亲政」。改制时的史官记述（`reform_note`）只是呈现，不入事件簿。昏君模式：`EmperorRetreat(bool)`（编码 `RETREAT 1/0`）由 `Court::propose_retreat` 校验；修道期间 `Session::approve` / `review` 返回 `InRetreat`，`Session::run` 先调 `Court::proxy_approvals` 代为批红，完成的回奏追加批语为 `PROXY_REVIEW_NOTE` 的 `TaskReviewed`，两者都把 `Task.proxied` 置真；写盘（`apply_changes`）仍须亲自准奏。`retreat_note` 只是呈现。户部度支：`BudgetSet{scope,tokens}`（`BudgetScope::Court` / `Office` / `Task`，编码 `BUDGET court|office:<官职>|task:<ID> [tokens]`）由 `Court::propose_budget` 校验（正数、诏令未结束）。`Court::task_limit` 取本件限额与承办者官职每件限额中较小者，`check_treasury` / `check_task_funds` / `check_funds` 计入尚未记账的用量。`Session::run` 有可执行诏令而国库告罄时返回 `TreasuryExhausted`；指派时本件已用尽则记 `TaskFailed`；`Job.budget` 为本件余额，`Job.treasury` 为本次执行各件共用的国库余额（`Arc<AtomicI64>`）。`TaskHooks::model_usage` 累计用量，用尽即撤销该件，`run_job` 把因此中断（或办成但已超支）的结局记为度支超限；国库已尽时尚未开始的作业为 `NotStarted`。票拟、复核、分办、廷议调用模型前先 `check_funds`。`duzhi_report` 只是呈现。刑部与锦衣卫：`ziyuan_runtime::guard` 提供 `scan`（按 `Threat` 的关键词检出，每类至多一处）、`fence` / `warning` / `nonce`（外来文书边界）。`agent_loop::run` 接收暗号，把每份工具输出 `fence` 后交给模型，并对其 `scan(Hijack)`，经 `ExecutionHooks::suspicion` → `Progress::Suspicion` 上报；`AgentBackend::execute` 每次生成新暗号并把 `warning` 写进系统提示。`Session::run` 把锦衣卫的检出记为 `SuspicionRaised{task,source:"tool:<工具>",threat,excerpt}`（只记执行中的诏令），完成时对回奏 `scan(全部类别)` 记为 `source:"report"`；有检出的诏令修道期间不追加代批的 `TaskReviewed`。`Session::censor` 与 `summary_prompt` 把回奏包在外来文书边界内，复核提示附上检出。检出不改变玉玺与派工。人格与廷议：`ziyuan_core::Persona::of(AgentId)` 由 ID 经 splitmix64 选定性格与文风，不入事件簿；`prompt()` 附在承办（`AgentBackend::execute`）、票拟、拆分、复核与廷议的系统提示末尾，只影响措辞。`CourtDebated{task,speeches}`（`Speech{speaker,office,text,model,usage}`）由 `Court::propose_debate` 校验（诏令待办、发言者现任所记官职），存于 `Task.debates`，用量并入 `Task.usage`，`overheads` 逐段计价。`Session::debate` 经 `WorkerBackend::complete` 逐人逐轮发言（默认人选 `Court::debate_candidates`），中途失败则记下已有发言并返回原因；`debate_minutes` 把最近一次纪要附在票拟的用户提示与承办作业的诏令内容之后，不改变 `route_task` 与玉玺。 礼部：`ExamTaken{agent,exam,score,answer,model,usage}`（`Court::propose_exam` 校验在册未革职、分数 ≤100）、`PalaceExamHeld{question,answers}`（答卷者现任所记官职）、`ZhuangyuanNamed{agent}`（须为最近一次殿试的答卷者、该科未点）；用量经 `exam_usage` 计入 `total_usage`，受国库约束。`ServiceRecord` 新增 `exams` / `exams_passed` / `zhuangyuan` 并计入 `merit()`。`ziyuan-app::ExamPaper` 解析 `.ziyuan/exam/*.txt`，`grade` 按要点命中比例判分（代码判，不靠模型）；`Session::exam` / `palace_exam` 经 `WorkerBackend::complete` 作答。回奏体例 `ziyuan_core::REPORT_SECTIONS` / `missing_sections` 只认行首标题；`AgentBackend::execute` 的系统提示要求三段，不合时追加「礼部发回」再跑至多 2 步，仍不合则照收原奏，由各前端标注。 兵部：`CourierDelivered{task,station}`（`Court::propose_courier`：诏令未结束、未记来处，来处 1–40 字单行）与 `EdictExpedited{task}`（`propose_expedite`：未结束、未加急；已分办的连同未结子诏令）存于 `Task.courier` / `Task.urgent`，子诏令继承原诏令的加急。`Session::post` 一次提交 `EdictIssued` + `CourierDelivered`（+ `EdictExpedited`），并以 `guard::scan` 把驿传内容的检出记为 `SuspicionRaised{source:"courier"}`（`Suspicion::COURIER`，唯一可记在待办诏令上的来源）；`proxy_approvals` 跳过有密报的诏令。`runnable_tasks` 稳定排序，加急在前；`Job.urgent` 让调度器的时限加倍。`ziyuan_app::EXECUTOR_BUSY` 是执行者锁被占时各前端的提示。 Agent Loop：`agent_loop::run` 以 `Metered` 包住执行钩子，记下每次调用服务端报告的用量，按「上次输入 + 输出 + 新增消息的估算 × 1.25」预判下一次输入（首次用 `estimate_tokens` × 1.25）；与 `ExecutionHooks::token_allowance`（`TaskHooks` 取本件度支与国库余额的较小者）相比，不足 `MIN_OUTPUT` 则返回 `LoopError::Budget`，否则经 `ExecutionHooks::max_output` 把余量交给 Adapter，`OpenAiCompatible` 在余量低于 8192 时传 `max_tokens`。每次调用前 `abridge` 节略较早的工具输出（`guard::unfence` 取原文，再 `fence` 包回）。工具执行前 `check_args` 以 `validate_args`（JSON Schema 子集）校验参数，不合者作为工具错误退回。`max_steps` 用尽后不带工具再调用一次，回奏加 `STEP_LIMIT_NOTE`。`OpenAiCompatible::next_turn` 按 `Retry`（默认 3 次，1/2/4 秒，`Retry-After` 优先，上限 30 秒）重试连接错误、429 与 5xx，只在收到成功响应之前重试；每次调用的 trace ID 以 `X-Client-Request-Id` 发出，并写进错误信息。
- `ziyuan-storage`：`EventStore { load, load_since(known), append(expected, events) }`，`append` 只在事件数等于 `expected` 时写入（乐观并发）；`EventLog`（`.ziyuan/court.log`）与 `MemoryEventLog`。编码在私有 `codec` 模块，与文件 I/O 分离。
- `ziyuan-runtime`：`Scheduler` 并发执行已指派的 `Job`，经通道回报 `Progress`（工具调用与结局）；不了解治理规则。Adapter 错误统一为 `BoxError`。`AgentBackend` 运行 `agent_loop`，`tools` 提供只读工具。
- `ziyuan-llm`：`OpenAiCompatible` 实现 `ModelProvider`。HTTP/TLS 依赖只在此 crate，runtime 保持无网络依赖。
- `ziyuan-app`：`Session<S: EventStore>` 是唯一的写入路径。每个用例：治理层提议事件 → 在 `Court` 副本上 `apply` 校验 → `EventStore::append` → 替换内存状态。被拒绝的操作不留事件；写入的事件一定可以回放。
- `ziyuan-cli`：把参数解析为 `Command`，调用 `Session`，打印结果；`serve` 启动 Web 界面，`tui` 启动终端界面。
- `ziyuan-web`：奉天门 Web 界面。内嵌单页（`index.html`，无构建步骤）+ JSON API + SSE。请求在阻塞线程中取用缓存的 `Session`（`reload` 增量刷新，并发请求时另开），与 CLI 一样受乐观并发保护；`POST /api/run` 取得执行者锁后在后台执行全部未结束的诏令，`RunUpdate` 经 SSE（`text` / `finished` / `changed`）推送。默认监听 127.0.0.1（`--bind` 可改）。全部 API 需 `Authorization: Bearer <令牌>` 或 HttpOnly、SameSite=Strict 的 `ziyuan_token` Cookie（`GET /?token=` 或 `POST /login` 种下，API 不再接受查询参数中的令牌）；令牌由 `load_or_create_token` 存于 `.ziyuan/web-token`（0600），`--new-token` 重新生成。中文描述（`describe_event` 等）移到 `ziyuan-app`，各前端共用。
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
- **增量读取**：`EventLog` 记住游标（前若干条事件止于第几字节，以及文件的设备号与 inode）。`load_since(known)` 与 `append` 的事件数核对只读游标之后的内容；文件被删除重建、变短或游标处不在行首时游标作废、从头读取，此时若调用者已有事件则报 `Conflict`，`Session::reload` 据此从头重放。`Session::reload` 只把新事件应用到 `Court` 副本上。
- **Schema 兼容**：新增事件类型或在行尾追加可选字段时格式版本仍为 v1；旧版读到更高版本或未知事件类型返回 `StorageError::Newer`（提示升级），而不是 `Corrupt`。只有改变既有字段含义时才升级版本，由读取方转换旧行。
- **快照**：实测 24 万条事件（64 MB）冷启动约 0.25 秒（其中解码约 0.09 秒，已改为查表解 hex），常驻进程刷新为微秒级，暂不做快照。快照须同时缓存 `Court` 的全部私有状态，并在规则变化时失效，复杂度不值得；若冷启动成为瓶颈，再考虑迁移 SQLite。

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
