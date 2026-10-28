# ZiYuan 设计文档（v0.6.0）

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
- `ziyuan-governance`：`Court` 字段私有，只能通过 `Court::apply(&CourtEvent)` 改变；`apply` 与 `propose_*` 共用同一套规则，返回 `GovernanceError`。官制（`Office::all` / `exists_in` / `rank`）定义在 core，署理规则在 `Court::route`。`names::NameGenerator` 由种子生成姓名；种子由 `ziyuan-app` 随机给出，生成的姓名随 `AgentRegistered` 落盘，回放不依赖随机数。官职代码与 v0.1 兼容，旧事件簿可直接回放。`WorkerBackend::model_name` / `ModelProvider::model_name` 供各前端显示承办模型，`price()` 返回单价（`ziyuan_core::Price`）供估算花费；单价表与 `ZIYUAN_LLM_PRICE` 解析在 `ziyuan-llm::price_for`。官职配置由 `OfficeModelSet` / `OfficeCeilingSet` 事件记录；`Session::run` 在指派时以 `Court::effective_seal`（`Seal::within`）求出受上限约束的玉玺、以 `Court::model_of` 选模型，写入 `Job` 与 `TaskAssigned.model`；`WorkerBackend::execute(&Job, hooks)`，`AgentBackend` 经 `ModelCatalog`（`ziyuan-llm::Catalog`）按名构造模型并查单价，`ziyuan-app::estimate_cost` 按各诏令实际模型汇总花费。多官协作：`EdictSplit{task,subtasks,note,model,usage}` 由 `Court::propose_split` 校验（待办、未分办、非子诏令、2–8 件、承办者为本部在任官员），子诏令以 `Task.parent` / `assignee` 记录，原诏令记 `Task.split`。`runnable_tasks` 让子诏令先行（须原诏令已批红），原诏令待子诏令全部结束后由 `route_task`（`Pick::Head`）派给尚书；`Session::run` 以 `summary_prompt` 替换汇总作业的内容并收回写权。`Session::plan_split` 经 `WorkerBackend::complete` 请尚书拆分，`split` 为御定；`overheads` 汇总票拟、拆分、复核三种额外开销供计价。都察院：`Office::Censor` 不属任何制度（`mode()` 为 `None`），不入 `route`；`CensorReported{task,censor,verdict,note,evidence,model,usage}` 由 `Court::propose_censure_check` 校验（已完成、未复核、现任御史、回避经手者），存于 `Task.censure`，用量并入 `Task.usage`。`Session::censor` 先用 `check_evidence`（引用解析见 `citations`，测试取最近一次 `run_tests` 的 `ToolInvoked.ok`）得出 `Evidence`，再经 `WorkerBackend::complete` 请御史下结论，失败时按 `Evidence::verified` 兜底。`ServiceRecord` 新增 `endorsed` / `impeached` / `verified`，荐举按 `merit()` 排序；`kaocheng_report` 只是呈现，不入事件簿。工部写权限：`Capability::WriteFile` / `RunTests` 只能盖在工部诏令上（`validate_edict_seal`）；`tools::sealed_tools` 返回 `Sealed`，`write_file` 写入其内存暂存区，执行成功后 `AgentBackend` 经 `ExecutionHooks::changes_proposed` 呈报，`Session::run` 在 `TaskCompleted` 之前记 `ChangesProposed`（`Task.changes`，重新指派时清空）。`Session::apply_changes` 先调 `tools::apply_changes`（核对原文、全有或全无）再记 `ChangesApplied`，`discard_changes` 记 `ChangesDiscarded`；`render_diff` 生成供各前端显示的统一 diff。工具调用轮次的前置文本与思维链记为 `Message::CallPrelude`，与其后的 `ToolCall` 编码为同一条 assistant 消息（DeepSeek 思考模式要求回传 `reasoning_content`）。内阁制下诏令须有 `EdictApproved`（批红）才进入 `Court::runnable_tasks`。票拟记为 `EdictDrafted{task,worker,note,model,usage}`（`Court::propose_draft` 校验内阁制、未批红且拟派者是 `draft_candidates` 中的本部在任官员），存于 `Task.draft`，用量并入 `Task.usage`；`Session::draft` 经 `WorkerBackend::complete`（单轮、无工具）请首辅模型拟票，失败时按旧例拟派并注明；改票 `Session::redraft` 也是一条 `EdictDrafted`。`Session::run` 用 `Court::route_task` 派工：有票拟按票拟，否则（中旨或拟派者已离任）按 `route` 的旧例；为兼容旧事件簿，`apply(TaskAssigned)` 不检查批红，由 `Session::run` 跳过未批红的诏令。`OfficialPurged`（革职）后不得再任官，`propose_abolition` 一次提交「革去全部丞相 + 改为亲政」。改制时的史官记述（`reform_note`）只是呈现，不入事件簿。昏君模式：`EmperorRetreat(bool)`（编码 `RETREAT 1/0`）由 `Court::propose_retreat` 校验；修道期间 `Session::approve` / `review` 返回 `InRetreat`，`Session::run` 先调 `Court::proxy_approvals` 代为批红，完成的回奏追加批语为 `PROXY_REVIEW_NOTE` 的 `TaskReviewed`，两者都把 `Task.proxied` 置真；写盘（`apply_changes`）仍须亲自准奏。`retreat_note` 只是呈现。户部度支：`BudgetSet{scope,tokens}`（`BudgetScope::Court` / `Office` / `Task`，编码 `BUDGET court|office:<官职>|task:<ID> [tokens]`）由 `Court::propose_budget` 校验（正数、诏令未结束）。`Court::task_limit` 取本件限额与承办者官职每件限额中较小者，`check_treasury` / `check_task_funds` / `check_funds` 计入尚未记账的用量。`Session::run` 有可执行诏令而国库告罄时返回 `TreasuryExhausted`；指派时本件已用尽则记 `TaskFailed`；`Job.budget` 为本件余额，`Job.treasury` 为本次执行各件共用的国库余额（`Arc<AtomicI64>`）。`TaskHooks::model_usage` 累计用量，用尽即撤销该件，`run_job` 把因此中断（或办成但已超支）的结局记为度支超限；国库已尽时尚未开始的作业为 `NotStarted`。票拟、复核、分办、廷议调用模型前先 `check_funds`。`duzhi_report` 只是呈现。刑部与锦衣卫：`ziyuan_runtime::guard` 提供 `scan`（按 `Threat` 的关键词检出，每类至多一处）、`fence` / `warning` / `nonce`（外来文书边界）。`agent_loop::run` 接收暗号，把每份工具输出 `fence` 后交给模型，并对其 `scan(Hijack)`，经 `ExecutionHooks::suspicion` → `Progress::Suspicion` 上报；`AgentBackend::execute` 每次生成新暗号并把 `warning` 写进系统提示。`Session::run` 把锦衣卫的检出记为 `SuspicionRaised{task,source:"tool:<工具>",threat,excerpt}`（只记执行中的诏令），完成时对回奏 `scan(全部类别)` 记为 `source:"report"`；有检出的诏令修道期间不追加代批的 `TaskReviewed`。`Session::censor` 与 `summary_prompt` 把回奏包在外来文书边界内，复核提示附上检出。检出不改变玉玺与派工。人格与廷议：`ziyuan_core::Persona::of(AgentId)` 由 ID 经 splitmix64 选定性格与文风，不入事件簿；`prompt()` 附在承办（`AgentBackend::execute`）、票拟、拆分、复核与廷议的系统提示末尾，只影响措辞。`CourtDebated{task,speeches}`（`Speech{speaker,office,text,model,usage}`）由 `Court::propose_debate` 校验（诏令待办、发言者现任所记官职），存于 `Task.debates`，用量并入 `Task.usage`，`overheads` 逐段计价。`Session::debate` 经 `WorkerBackend::complete` 逐人逐轮发言（默认人选 `Court::debate_candidates`），中途失败则记下已有发言并返回原因；`debate_minutes` 把最近一次纪要附在票拟的用户提示与承办作业的诏令内容之后，不改变 `route_task` 与玉玺。 礼部：`ExamTaken{agent,exam,score,answer,model,usage}`（`Court::propose_exam` 校验在册未革职、分数 ≤100）、`PalaceExamHeld{question,answers}`（答卷者现任所记官职）、`ZhuangyuanNamed{agent}`（须为最近一次殿试的答卷者、该科未点）；用量经 `exam_usage` 计入 `total_usage`，受国库约束。`ServiceRecord` 新增 `exams` / `exams_passed` / `zhuangyuan` 并计入 `merit()`。`ziyuan-app::ExamPaper` 解析 `.ziyuan/exam/*.txt`，`grade` 按要点命中比例判分（代码判，不靠模型）；`Session::exam` / `palace_exam` 经 `WorkerBackend::complete` 作答。回奏体例 `ziyuan_core::REPORT_SECTIONS` / `missing_sections` 只认行首标题；`AgentBackend::execute` 的系统提示要求三段，不合时追加「礼部发回」再跑至多 2 步，仍不合则照收原奏，由各前端标注。 兵部：`CourierDelivered{task,station}`（`Court::propose_courier`：诏令未结束、未记来处，来处 1–40 字单行）与 `EdictExpedited{task}`（`propose_expedite`：未结束、未加急；已分办的连同未结子诏令）存于 `Task.courier` / `Task.urgent`，子诏令继承原诏令的加急。`Session::post` 一次提交 `EdictIssued` + `CourierDelivered`（+ `EdictExpedited`），并以 `guard::scan` 把驿传内容的检出记为 `SuspicionRaised{source:"courier"}`（`Suspicion::COURIER`，唯一可记在待办诏令上的来源）；`proxy_approvals` 跳过有密报的诏令。`runnable_tasks` 稳定排序，加急在前；`Job.urgent` 让调度器的时限加倍。`ziyuan_app::EXECUTOR_BUSY` 是执行者锁被占时各前端的提示。 Agent Loop：`agent_loop::run` 以 `Metered` 包住执行钩子，记下每次调用服务端报告的用量，按「上次输入 + 输出 + 新增消息的估算 × 1.25」预判下一次输入（首次用 `estimate_tokens` × 1.25）；与 `ExecutionHooks::token_allowance`（`TaskHooks` 取本件度支与国库余额的较小者）相比，不足 `MIN_OUTPUT` 则返回 `LoopError::Budget`，否则经 `ExecutionHooks::max_output` 把余量交给 Adapter，`OpenAiCompatible` 在余量低于 8192 时传输出上限（`output_cap_field`：主机为 `api.openai.com` 用 `max_completion_tokens`，其余用 `max_tokens`）。思维链取 `reasoning_content`（DeepSeek）或 `reasoning`（Ollama），正文开头的 `<think>…</think>` 由 `split_think` 移入思维链；回奏交出前 `guard::strip_fences` 去掉照抄的本次暗号边界行。每次调用前 `abridge` 节略较早的工具输出（`guard::unfence` 取原文，再 `fence` 包回）。工具执行前 `check_args` 以 `validate_args`（JSON Schema 子集）校验参数，不合者作为工具错误退回。`max_steps` 用尽后不带工具再调用一次，回奏加 `STEP_LIMIT_NOTE`。`OpenAiCompatible::next_turn` 按 `Retry`（默认 3 次，1/2/4 秒，`Retry-After` 优先，上限 30 秒）重试连接错误、429 与 5xx，只在收到成功响应之前重试；每次调用的 trace ID 以 `X-Client-Request-Id` 发出，并写进错误信息。 翰林院：`ziyuan_core::Template{department,text,seal,criteria}`；`TemplateSaved{name,template}` / `TemplateRemoved{name}` 由 `Court::propose_template`（名 1–20 字无空白、内容非空、`validate_edict_seal`、标准 1–8 条单行）/ `propose_template_removal`（只能撤自定的）校验，存于 `Court.templates`；`Court::template` / `templates` 合并 `builtin_templates()`（自定的覆盖同名内置的）。`TemplateApplied{task,name,criteria}` 由 `propose_template_use` 生成（诏令待办、未用过模板），快照标准存于 `Task.template`；`Session::issue_templated` 与 `EdictIssued` 一并提交。`criteria_note` 附在承办作业的诏令内容之后，并写进 `Session::censor` 的提示，系统提示要求逐条核验，批语上限放宽到 1200 字。 增量呈报：`FileChange::hunks` / `select` 在 core 中按行（`split_inclusive`，保留行尾）求最长公共子序列，相隔不超过 2×3 行的改动并为一段，`select(keep)` 按段取舍后重建文件（新建文件整段驳回为 `None`）。`ChangesApplied{task,skipped}`（编码 `APPLIED <ID> [段号,…]`，无段号与 v0.3 相同）由 `Court::propose_application` 校验（段号 1..=总段数、跨文件连续编号、不能全部驳回），存于 `Changes.skipped`；`Session::apply_changes(task, root, skipped)` 只把取舍后与原文不同的文件交给 `tools::apply_changes`。`render_diffs` 生成带段号的 diff。`tools::audit_args` 把 `write_file` 审计参数中的 `content` 换成行数与字节数摘要。 CLI：`quiet_broken_pipe` 在 panic hook 中识别写 stdout 遇到断开的管道并以 0 退出（工作区禁用 unsafe，不能恢复 SIGPIPE）；`--json` 复用 `ziyuan_web::court_json`；`completions` 由 `SUBCOMMANDS` 与 core 的代码表生成脚本，测试保证 `SUBCOMMANDS` 都能被 `parse` 识别。 常朝：`Session::watch(scheduler, backend, cancel, &WatchOptions{poll,censor}, on_update)` 循环调用 `run_open`（每轮先重载事件簿，因此其他进程入队、批红的诏令都能接走），本轮完成的非子诏令在 `censor` 为 `Some(root)` 时逐件调用 `Session::censor`（阻塞到模型返回）；`GovernanceError::TreasuryExhausted` 与复核失败化为 `WatchUpdate::Notice`，不中断。轮间以 `tokio::select!` 等待 `poll` 或 `CancelToken::cancelled`。待评定的已完成原诏令达到 `NEGLECT_THRESHOLD` 时发一次 `WatchUpdate::Neglect`（降回阈值以下后可再发）。CLI `run --watch` 全程持有执行者锁。 小模型适配：`agent_loop::run` 在有工具可用、对话中尚无工具输出时，首次 `ModelTurn::Final` 不收，追加该回答与 `UNCHECKED_SEND_BACK` 再跑（只发回一次，之后照收）；礼部体例发回重写时不给工具，因此不会触发。`AgentBackend::execute` 的系统提示要求先调用工具查阅再作结论。 起居注：`CourtEvent::Clock(u64)`（编码 `CLOCK <unix秒>`）不改变状态；`EventStore::stamps_time` 为真（`EventLog`）时，`Session::commit` 在分钟变化时把一条时辰置于本批之前，`found` 把它放在 `Founded` 之后（内存事件簿不记，测试不受时钟影响）。`history_entries(history, &HistoryFilter{task,agent,kind,since,until})` 把各事件归到之前最近的时辰（`Founded` 取紧随其后的时辰），`event_tasks` / `event_agents` 给出事件涉及的诏令与官员，类型取 `ziyuan_storage::event_kind`（编码中的类型字段，小写）。`parse_time` / `format_time` 按 `tz_offset`（`ZIYUAN_TZ`）换算。trace：`ExecutionHooks::model_trace` 在每次调用前报告 trace ID，`TaskHooks` 把它附在 `Progress::Usage` 上，记入 `ModelUsage.trace`（编码为 `USAGE` 行尾可选字段）；票拟、复核等 `complete` 调用不记。 廷推与廷杖：`CourtNominated{office,votes}`（`Nomination{voter,candidate,note,model,usage}`）由 `Court::propose_nomination` 校验（`nomination_ballot`：官职出缺、吏部尚书在任、投票者为 `nine_ministers` 且不自举、候选人取自 `nomination_candidates`），任命该官职后作罢；用量经 `nomination_usage` 计入 `total_usage`。`Session::nominate` 经 `WorkerBackend::complete` 逐人推举，回答不合或失败时按考绩代推；`Session::appoint` 返回 `appointment_note`，`appointment_events` 对现任他职者先罢后任；`draw_lots` 只是在得票者中随机任命。`OfficialCaned{agent}` / `OfficialPardoned{agent}` 由 `propose_caning`（在任、未革职、自上次受杖以来弹劾 ≥ `IMPEACHMENTS`）/ `propose_pardon` 校验；`Court::demotion` 由受杖以来的采纳件数（`REDEMPTION`）推出，`effective_seal` 在降级期间去掉写权。`ServiceRecord.nominated` / `caned` 不计入 `merit()`。 翰林院修史：`ziyuan_core::Record{task,department,edict,worker,handler,summary,verdict}` 由 `Court::records` 从已完成的原诏令推出（不另记事件，事件簿只增不改），verdict 合并 `Task.review` 与 `Task.censure`；`search_records` 按共有检索词数排序（ASCII 词小写、其余字符取相邻二字组，同分新者在前）；只为查询求检索词，实录原文以 `record_has` 直接查找（二字组即子串，ASCII 词按整词不分大小写，单字须单独成串），结果与对原文求词表再取交集相同，5 万件时由 1.4 秒降到 0.01–0.12 秒。`Session::run` 开工前取一次实录放进 `Job.records`（`Arc<Vec<Record>>`），非空时 `AgentBackend::execute` 追加只读工具 `tools::record_search`（不读文件，不受玉玺所限，输出照常 `fence`）。`Session::draft` 在用户提示末尾附上与诏令相关的至多 3 件实录，`Session::nominate` 在每位候选人后附其最近经办的一件。 刑部复勘：`SuspicionReviewed{task,index,reviewer,upheld,note,model,usage}`（编码 `RECHECKED`）由 `Court::propose_recheck` 校验（检出存在、未复勘未平反、复勘者现任刑部官职且未经手），存于 `Suspicion.review`，用量并入 `Task.usage`，`overheads` 计为「复勘」；`Court::recheck_official` 依尚书、侍郎、郎中次序取未经手者。`Session::recheck` 经 `WorkerBackend::complete` 逐处请其判断（可疑文字与回奏包在外来文书边界内），`parse_recheck` 认第一行的「维持」「误报」，批语只留理由。`SuspicionAcquitted{task,index}`（`ACQUITTED`）由 `propose_acquittal` 校验（未平反、皇帝在朝），置 `Suspicion.acquitted`；`Task::standing_suspicions` 只含未平反的，`proxy_approvals` 与完成时的代批判断都改用它。`suspicion_status` 供各前端显示复勘与平反情况。 兵部 · GitHub：新 crate `ziyuan-github`（只依赖 core、ureq、serde_json）独占 GitHub 凭据，`GitHub::from_env` 读 `ZIYUAN_GITHUB_REPO` / `ZIYUAN_GITHUB_TOKEN`；`intake` 把 issue/PR 及成员（`author_association` 为 OWNER/MEMBER/COLLABORATOR）的 `/ziyuan` 评论转为 `Dispatch{station,text}`，CLI 以 `Task.courier` 去重后经 `Session::post` 入队（驿站名上限放宽到 80 字），玉玺由调用者给定：`github pull --tools/--scope` 所授，其余入口为默认只读（`validate_edict_seal` 照常校验）。`CourierReplied{task,pull_request,url}`（编码 `REPLIED`）由 `Court::propose_courier_reply` 校验（经驿传入队；评论须已完成、已评定、无 `standing_suspicions`、未回报；PR 须改动 `Applied`、未开过），存于 `Task.replied` / `Task.pull_request`。`ziyuan-app` 的 `courier` 模块提供 `Session::github_pull` / `github_reply` / `github_pull_request`（先 `check_reply` 再调 GitHub，成功后 `record_reply`；GitHub 错误为 `AppError::External`），CLI 与 Web 共用。`reply_text` 生成评论正文；`selected_files`（`apply_changes` 也用）求逐段取舍后的文件，`GitHub::draft_pull_request` 先按 `before` 核对远端原文，再经 Git Data API（trees → commits → refs → pulls）开草稿 PR。 礼部修 CHANGELOG：`validate_edict_seal` 对礼部诏令放行 `WriteFile`，条件是范围恰为 `[ziyuan_core::CHANGELOG]`（`RunTests` 仍唯工部）；`Sandbox::new` 只对这一路径允许范围尚不存在（经 `contained` 解析）。`Session::draft_changelog(version, commits)` 以 `changelog_text`（体例、提交、最近 20 件实录）`issue` 一件礼部诏令；`git log` 由 `ziyuan_app::changelog_commits(root, since)` 调用（CLI 与 Web 共用；`Session` 本身不运行 git）。其后与工部改动同路：`ChangesProposed` → `apply_changes`。 CI 与发版：`.github/workflows/ci.yml`（fmt、clippy `-D warnings`、`scripts/smoke.sh`；`msrv` 以 1.89 `cargo check --locked`；macOS 与 Windows 均 `cargo test`）；`release.yml` 在 `v*` 标签上先核对标签与 `Cargo.toml` 版本、以 `CHANGELOG.md` 本版一节（或自动生成）建 Release，再按目标三元组构建 `ziyuan-cli` 并上传压缩包。 外藩 MCP：`Seal.mcp: BTreeSet<String>`（`*`、`服务`、`服务/工具`，`valid_mcp_grant` 校验，`validate_seal` 拒绝不合法者）；编码时以 `mcp:` 前缀并入玉玺的工具列表。`Seal::within` 对 MCP 授权取「被上限涵盖则保留，否则收窄为上限中被它涵盖的项」；`effective_seal` 在降级期间清空。`ziyuan_runtime::mcp` 读 `.ziyuan/mcp.json`，`Server` 以换行分隔的 JSON-RPC 与子进程通信（读线程 + `recv_timeout` 60 秒，略过通知，回绝反向请求，`Drop` 时杀进程），`sealed_tools` 由 `AgentBackend::execute` 调用，每件工具包成 `McpTool`（`服务__工具`），与本件其他工具一同析构。审计与外来文书边界沿用 `agent_loop`。CLI 与 Web 设官职上限时未给 `--mcp` 即为 `*`；v0.5 之前记下的上限没有 MCP 授权，因而不许外藩。
- `ziyuan-storage`：`EventStore { load, load_since(known), append(expected, events) }`，`append` 只在事件数等于 `expected` 时写入（乐观并发）；`EventLog`（`.ziyuan/court.log`）与 `MemoryEventLog`。编码在私有 `codec` 模块，与文件 I/O 分离。
- `ziyuan-runtime`：`Scheduler` 并发执行已指派的 `Job`，经通道回报 `Progress`（工具调用与结局）；不了解治理规则。Adapter 错误统一为 `BoxError`。`AgentBackend` 运行 `agent_loop`，`tools` 提供只读工具。
- `ziyuan-llm`：`OpenAiCompatible` 实现 `ModelProvider`。HTTP/TLS 依赖只在此 crate，runtime 保持无网络依赖。
- `ziyuan-app`：`Session<S: EventStore>` 是唯一的写入路径。每个用例：治理层提议事件 → 在 `Court` 副本上 `apply` 校验 → `EventStore::append` → 替换内存状态。被拒绝的操作不留事件；写入的事件一定可以回放。
- `ziyuan-cli`：把参数解析为 `Command`，调用 `Session`，打印结果；`serve` 启动 Web 界面，`tui` 启动终端界面。
- `ziyuan-web`：奉天门 Web 界面。内嵌单页（`index.html`，无构建步骤）+ JSON API + SSE。请求在阻塞线程中取用缓存的 `Session`（`reload` 增量刷新，并发请求时另开），与 CLI 一样受乐观并发保护；`POST /api/run` 取得执行者锁后在后台执行全部未结束的诏令，`RunUpdate` 经 SSE（`text` / `finished` / `changed`）推送；`{watch:true}` 改调 `Session::watch`（常朝，`WatchUpdate` 的提示经 `notice` 事件推送），`/api/state` 的 `watching` 标出。`GET /api/records?q=`（`ziyuan_core::find_records`，CLI `records` 同此）、`POST /api/changelog`、`POST /api/github/pull`、`/api/tasks/{id}/github/{reply,pr}` 与 CLI 走同一组 `Session` 方法；`Config.github` 为 `None` 时后三者返回 400。默认监听 127.0.0.1（`--bind` 可改）。全部 API 需 `Authorization: Bearer <令牌>` 或 HttpOnly、SameSite=Strict 的 `ziyuan_token` Cookie（`GET /?token=` 或 `POST /login` 种下，API 不再接受查询参数中的令牌）；令牌由 `load_or_create_token` 存于 `.ziyuan/web-token`（0600），`--new-token` 重新生成。中文描述（`describe_event` 等）移到 `ziyuan-app`，各前端共用。
- `ziyuan-tui`：紫禁城终端界面（ratatui）。`App` 不依赖真实终端：按键经 `App::handle`、画面经 `App::draw`，测试用 `TestBackend` 驱动。执行在 Tokio 任务中调用 `Session::run_open`（与 Web 共用：反复执行未结束的诏令直到队列清空），进度经通道送回界面；每秒 `Session::reload` 以看到其他进程的变更。复勘（`x`，后台 `Session::recheck`）、平反（`X`，`Session::acquit(task, None)`）与实录（`R`，`find_records`）与 CLI 同用 app 层方法。

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
- **Schema 兼容**：新增事件类型或在行尾追加可选字段时格式版本仍为 v1；v0.4 起读到更高版本或未知事件类型返回 `StorageError::Newer`（提示升级，并报本版版本号），而不是 `Corrupt`；v0.3 及更早的版本只报解码失败，追加的可选字段在各旧版中都按「字段数量不正确」处理。已发布的版本改不了，因此兼容由新版负责：`codec::readable_since(line)` 给出能读这一行的最早发布版本，依据是逐一用 v0.1–v0.5 发布时的解码器试读 `crates/ziyuan-storage/src/compat.tsv` 中的样本（每种事件类型、可选字段、后加的官职与玉玺代码各至少一条）；测试保证本版读得回全部样本且版本与之相符，`readable_since` 不用通配，新增事件类型时编译器会提醒补上。`EventLog::inspect` 逐行求出 `LogReport`（事件数、残行、最早可读版本及最先要求它的一行、第一处读不了的行），`ziyuan migrate` 不经 `Session` 打印它。不另加首行头：每行已有格式版本，首行头会让 v0.4/v0.5 拒读全部新事件簿。只有改变既有字段含义时才升级版本，由读取方转换旧行，届时 `migrate` 才真正改写。
- **快照**：实测 24 万条事件（64 MB）冷启动约 0.25 秒（其中解码约 0.09 秒，已改为查表解 hex），常驻进程刷新为微秒级，暂不做快照。v0.6 按每件诏令 16 条事件（6 次工具调用与用量、约 1.6 KB 回奏）再测：5 万件（85 万条、230 MB）时 `EventLog::load` 约 0.8 秒、`Court::replay` 约 0.18 秒，解码中回奏全文的 hex 约占一半，内存：回奏全文在 `CourtEvent::TaskCompleted`、`TaskStatus::Completed` 与 `Record` 中都是 `Arc<str>`，事件簿与朝廷状态共用一份；`EdictIssued` 与 `TemplateSaved` 的载荷装箱，`CourtEvent` 由 152 字节降到 104 字节（工具调用、用量等小事件占绝大多数）；`EventLog::read_tail` / `inspect` 以 `BufReader` 逐行读、边读边解码，不把整段文件留在内存里；CLI 的 `serve` / `tui` 不再先开一个用不上的会话（只核对 `court.log` 非空），前端各自开会话；`history_entries` 只为返回的各条求事件类型。5 万件时 `ziyuan court` 峰值由 500 MB 降到 312 MB（0.97 → 0.75 秒），`serve` 常驻由 648 MB 降到 335 MB，`migrate` 由 239 MB 降到 4 MB。余下的大头是事件簿本身（约 225 MB：85 万条事件与回奏全文）。快照仍须解码回奏全文，最多省下回放与工具、用量行的解码，且须缓存 `Court` 的全部私有状态并在规则变化时失效，复杂度不值得；若冷启动成为瓶颈，再考虑迁移 SQLite。前端的代价随诏令数增长更快，因此 `court_json(court, backend, recent)` 在 `recent` 为 `Some(n)` 时只列待理会的诏令（未结束、待评定、改动待准奏，连同分办的原诏令与子诏令）与最近 n 件，另给 `task_total` / `open_total`（Web 默认 50，`?all=1` 为全部；CLI `--json` 为全部）；`HistoryFilter.limit` 只为相符者中最后若干条生成描述（Web 默认 500，多取一条以给出 `truncated`；TUI 1000）；`event_kind` 改由 `codec::kind_of` 按变体给出，不再编码整条事件，`compat.tsv` 的测试保证二者一致。

## 4. Runtime 与 LLM

`ziyuan-runtime::WorkerBackend` 是实际执行者接口（同步，`Send + Sync`）；`DemoBackend` 不执行任务。

用量的路径：`OpenAiCompatible` 请求 `stream_options.include_usage`，从流的最后一块（或普通 JSON 的 `usage`）读出 `prompt_tokens`/`completion_tokens` → `ExecutionHooks::model_usage` → `Progress::Usage` → `Session::run` 提交 `ModelUsage { task, usage }`。指派后任何时刻都可记录（超时后才返回的调用也计入）；`Task::usage`、`ServiceRecord::usage` 与 `Court::total_usage` 由回放累加。

流式文本的路径：`text_delta` → `Progress::TextDelta` → `RunUpdate::Text` → CLI（stderr，按诏令 ID 分行）。它只用于显示，不进入事件簿。

`Scheduler` 基于 Tokio：信号量限制并发，每件任务在阻塞线程池中执行；许可随执行线程释放，因此超时后仍在收尾的线程也计入并发数。取消是协作式的：超时或 `CancelToken` 触发时调度器立即给出结局，并通过 `ExecutionHooks::is_cancelled` 通知后端；`agent_loop` 在每轮模型调用前与执行工具前检查，`OpenAiCompatible` 在流式响应的每一行检查并中止请求。撤销时尚未开始的任务报告为 `NotStarted`，保持未结束。CLI 退出时不等待仍阻塞的 HTTP 请求。

准奏进版本库：`ziyuan_app::vcs`。`Session::commit_applied` 对已 `Applied` 的改动取 `selected_files` 的路径，以 `commit_message`（首行「紫垣诏令 #n：…」，附承办者、驳回段号与 `Ziyuan-Edict: n` 尾注）调 `commit_files`：`jj root` 成功则 `jj commit -m … -- file:"路径"…`（只把这些文件分出，余下留在新的工作副本），否则 `git add` 后 `git commit --only`。提交不入事件簿。`GitHub::draft_pull_request` 以同一 `commit_message` 作 PR 中提交的说明，标题取其首行。

会典（快照）：`Court` 及其引用的 core 类型实现 serde 的 `Serialize`/`Deserialize`（core 仍不做 IO）。`EventStore::load_snapshot(tag)` / `save_snapshot(tag, events, write)` 存取不透明的状态；`EventLog` 写 `.ziyuan/huidian`（首行 `ZIYUAN-HUIDIAN 1 <tag> <事件数> <字节数> <摘要>`，其后是状态），读时核对 tag、事件簿长度、止处为行首，以及事件簿开头与止处之前各 4 KB 的 FNV-1a 摘要，对上了就把游标设到止处，随后的 `load_since` 只解码其后的行。`Session::open` 以 `huidian_tag()`（版本号）读会典并解析（`read_huidian` 从缓冲区解析：axum 开了 serde_json 的 `raw_value`，流式解析慢近一倍），用不得则从头回放并重修；`revise_huidian` 在新增 `HUIDIAN_EVERY.max(上部/10)` 条后于打开或提交时流式写出。`Session` 只记事件条数与最近一条时辰；`history()` 首次调用时才读整本事件簿（`OnceLock`，之后随提交与重载追加），起居注经它取用。复核（`check_evidence`、`tool_calls`）经 `Session::task_events` 只取该件诏令的 `TaskAssigned` 与 `ToolInvoked`：整本事件簿已在手就从中筛，否则 `EventStore::load_where(len, keep)` 逐条解码、只留选中的，且不动增量读取的游标。史官记经 `take_chronicle` 交前端显示，不入事件簿。

`ziyuan-runtime::agent_loop` 提供独立的 `ModelProvider`, `Tool`, `ModelTurn`, `Message` 等抽象，并实现最小 Tool Calling 循环。一轮内的全部工具调用先于其输出写入消息（与 Chat API 的顺序一致）；任一调用未注册则整轮拒绝，不执行任何工具。

`ziyuan-llm::OpenAiCompatible` 对接 OpenAI 兼容的 `/chat/completions`（同步，`stream: true`；SSE 中的文本经 `ExecutionHooks::text_delta` 实时上报，工具调用按 `index` 拼接；服务若忽略 `stream` 而返回普通 JSON 也能处理），由 `ZIYUAN_LLM_MODEL` / `ZIYUAN_LLM_BASE_URL` / `ZIYUAN_LLM_API_KEY` 配置。

ChatGPT 订阅：`ZIYUAN_LLM_PROVIDER=chatgpt` 时凭据为 `Auth::Plan(Arc<chatgpt::Account>)`，请求改发 `/responses`（`responses::body`：系统消息并入 `instructions`，工具归入 namespace `ziyuan`，`store:false`、`stream:true`，不传输出上限），流由 `responses::read_events` 解析，只认 `response.completed` 为成功；错误码经 `plan_error` 译为中文。401 时 `Account::refresh(Some(旧令牌))` 续期后重发一次。`chatgpt::Account` 管 OAuth（授权码 + PKCE、首次以 `dynamic_agent_client` 注册、`ext_agent_host_id` 持久化、ring 校验 ID token 的 RS256 签名），凭据存于用户配置目录（`ZIYUAN_CONFIG_DIR`），续期持 `chatgpt.lock` 文件锁。`Catalog::price` 在订阅模式下返回 `None`。

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
