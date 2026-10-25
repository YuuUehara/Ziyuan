# 紫垣 ZiYuan

> **实验性 Rust Multi-Agent 框架**：以中国古代朝廷为角色模型，支持可变治理结构、官职任免、诏令派工、事件日志。

**当前版本 v0.3.0。** CLI 默认使用明确标记的 `DemoBackend`，只模拟 Worker 收到任务。设置 `ZIYUAN_LLM_MODEL` 后，诏令由真实模型（OpenAI 兼容 API）执行，官员默认只能用只读工具 `read_file`、`search_code` 查阅当前目录；工部诏令可另授写文件与跑测试，改动须皇帝准奏才落盘（见下文「工部动手」）。

## 组件命名

| 名称 | 职能 | 当前状态 |
| --- | --- | --- |
| 紫垣 ZiYuan | 项目和整体框架 | Rust Workspace |
| 廷枢 Tingshu | Agent Runtime / Scheduler | `ziyuan-runtime`：Tokio 调度器（并发上限、超时、撤销）+ Tool Calling |
| 玉玺 Yuxi | 职权、审批、审计 | `Seal`：每件诏令的工具与查阅范围，随事件簿审计 |
| 诏令 ZhaoLing | 任务入口 | `Edict` 数据模型 + CLI |
| 吏部 LiBu | 考绩、升迁 | `review` 评定、`examine` 考绩、`recommend` 荐举（只采信皇帝评定） |
| 紫禁城 Forbidden City | 可视化与角色扮演 UI | `ziyuan serve`：Web 界面；`ziyuan tui`：终端界面 |

## 项目结构

```text
crates/
  ziyuan-core/        官员、官职、部门、诏令、事件等领域类型
  ziyuan-governance/  朝廷状态重放、改制、任免与任务路由（规则唯一来源）
  ziyuan-runtime/     Scheduler、WorkerBackend（Demo / Agent）、Tool Calling Loop、工作区工具（只读与工部受控写）
  ziyuan-llm/         OpenAI 兼容 Chat Completions Adapter（ureq + serde_json）
  ziyuan-storage/     EventStore 接口；文件事件簿与内存事件簿
  ziyuan-app/         Session 用例层：提议 → 校验 → 持久化 → 更新状态
  ziyuan-web/         紫禁城 Web 界面（axum + 内嵌单页，SSE 实时推送）
  ziyuan-tui/         紫禁城终端界面（ratatui）
  ziyuan-cli/         `ziyuan` 命令行（仅解析参数与呈现结果）

docs/                 架构决策与扩展路线
```

## 快速开始

要求：Rust 1.89 或更新的稳定版工具链（Rust 2024 edition；事件簿文件锁需要 1.89）。第三方依赖为 `ureq`、`serde_json`、`tokio`，以及 Web 界面的 `axum`、`futures-util`。

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo test --workspace
cargo build --workspace
./scripts/smoke.sh  # 综合验收：改制、任免、派工与持久化

# 建议在单独的演示目录运行，事件日志保存在当前目录
mkdir -p /tmp/ziyuan-demo
cd /tmp/ziyuan-demo
/path/to/ziyuan/target/debug/ziyuan init
/path/to/ziyuan/target/debug/ziyuan court
/path/to/ziyuan/target/debug/ziyuan officials
/path/to/ziyuan/target/debug/ziyuan recruit "新工" works
/path/to/ziyuan/target/debug/ziyuan appoint 7 clerk:works
/path/to/ziyuan/target/debug/ziyuan edict --dept works "检查 Rust 项目中的 unsafe 代码"

# 朱元璋式：撤销丞相，六部直接承接任务
/path/to/ziyuan/target/debug/ziyuan reform direct
/path/to/ziyuan/target/debug/ziyuan edict --dept works "再检查一次"

# 转换为内阁制；新首辅必须由皇帝任命
/path/to/ziyuan/target/debug/ziyuan reform cabinet
/path/to/ziyuan/target/debug/ziyuan dismiss 6
/path/to/ziyuan/target/debug/ziyuan appoint 6 first-secretary
/path/to/ziyuan/target/debug/ziyuan edict --dept works "请首辅协调调查"
/path/to/ziyuan/target/debug/ziyuan history
```

### 接入真实模型

```bash
export ZIYUAN_LLM_MODEL=deepseek-flash                   # 或 deepseek-v4-pro
export ZIYUAN_LLM_BASE_URL=https://api.deepseek.com/v1   # 默认 https://api.openai.com/v1
export ZIYUAN_LLM_API_KEY=...                            # 本地 Ollama 可不设
export ZIYUAN_LLM_PRICE=2,8                              # 可选：每百万 token 输入,输出[,币种]
cd /path/to/your/project && ziyuan init
ziyuan edict --dept works "找出所有 unsafe 代码并说明用途"
```

**实测情况（2026-10）：** 已用 DeepSeek `deepseek-flash`（默认思考模式）与 `deepseek-v4-pro` 端到端实测：多轮工具调用、流式输出、玉玺越权拦截、超时与 Ctrl-C 撤销均正常。思考模式的思维链（`reasoning_content`）不显示，但会按 DeepSeek 要求在后续请求中回传。其他 OpenAI 兼容服务（OpenAI、Ollama 等）协议兼容、由模拟服务测试覆盖，**未经真实实测**。

**花费估算：** 用量后附「约 X 元」。`deepseek-flash`、`deepseek-v4-pro` 内置官方高峰时段、未命中缓存的单价，是估算上限（空闲时段半价，缓存命中更便宜）；其他模型设置 `ZIYUAN_LLM_PRICE` 后才显示。估算按当前模型单价计算全部历史用量，以服务商账单为准。

**按官职配模型与玉玺上限：**

```bash
ziyuan model clerk:works deepseek-flash             # 郎中用便宜模型
ziyuan model minister:works deepseek-v4-pro         # 尚书用强模型；default 恢复默认
ziyuan ceiling clerk:works --scope crates --tools search_code   # default 取消
```

- 模型沿用同一服务地址与 Key；每次派工把实际所用模型记入 `TaskAssigned`，花费按各诏令实际模型分别计价。
- 承办时诏令玉玺与官职上限取交集（工具取交集，范围收窄到上限内），派工路线中注明；诏令范围全在上限之外则判为失败，不予承办。
- Web 百官面板的「官职配置」与 CLI 等效；`officials`、`examine`、Web 与 TUI 显示每位官员的模型与上限。

单件诏令最多 12 步模型调用（`ZIYUAN_MAX_STEPS` 可改）。模型调用失败、超时或被撤销都会记入事件簿（`TaskFailed` / `TaskCancelled`），每次工具调用也记为 `ToolInvoked`（参数与成败，不含输出），可用 `ziyuan history` 审计。

**Agent Loop 的生产化处理：**

- **重试与退避**：网络错误、429 与 5xx 最多重试 3 次（`ZIYUAN_LLM_RETRIES`），间隔 1、2、4 秒，服务端给了 `Retry-After` 就照办（最长 30 秒），退避中也能撤销。4xx 与已开始的流式响应不重试，以免回奏重复。
- **trace ID**：每次模型调用带请求头 `X-Client-Request-Id: zy-…`（重试沿用）；失败原因里附上 trace ID、服务端的请求 ID 与已重试次数，便于对照服务商日志。
- **步数**：用尽时不直接判失败，而是收回工具，请官员据已查阅的内容回奏，回奏以「〔步数用尽，据已查阅的内容回奏〕」开头，没查完的列入「未办之事」。
- **度支预判**：每次调用前预计输入（首次按字数估算，之后按服务端报告的上次用量加上新增内容，另留两成余量）。余额不够就不发请求（「度支将尽」），余额将尽时向 API 传 `max_tokens`，输出不会超支。
- **节略**：对话估算超过 48000 tokens 时，从最早的起把工具输出压成开头 200 字加一行说明（仍在外来文书边界内），最近 4 份不动。不调用模型。
- **参数校验**：工具参数须是合法 JSON，并合乎工具声明的 JSON Schema（类型、必填、不许多余字段）；不合的不执行，作为工具错误退回模型，审计记为失败。

### 奉天门：Web 界面

```bash
cd /path/to/your/project && ziyuan serve --port 8080
# 奉天门已开：http://127.0.0.1:8080/?token=…
```

- 在浏览器中下诏（含玉玺）、执行与中止、撤销、评定、改制、招募、任免与荐举；朝堂面板显示现行制度下各部的派工路线与空缺。
- 执行中官员的回答实时流式显示；页面每 5 秒刷新一次，以反映 CLI 等其他进程的变更。
- Web 界面代表皇帝，每个 API 请求都须带令牌。令牌存于 `.ziyuan/web-token`（仅本人可读），重启不变；`ziyuan serve --new-token` 重新生成，旧令牌作废。
- 打开带 `?token=` 的地址后，服务器种下 HttpOnly、SameSite=Strict 的 Cookie 并跳转到不含令牌的地址；也可在页面登录框输入令牌。脚本可用 `Authorization: Bearer <令牌>`。
- 默认只监听 127.0.0.1。`--bind 0.0.0.0` 可供其他机器访问，但 HTTP 是明文，请只在可信网络中使用，或置于 HTTPS 反向代理、SSH 隧道之后。
- 与 CLI 共用事件簿与多进程规则：执行时取得执行者锁，CLI 正在执行时页面会提示稍后再试。

### 紫禁城：终端界面

```bash
cd /path/to/your/project && ziyuan tui
```

- 三个页签：诏令（列表 + 详情，执行中实时流式输出）、百官（考绩与各部派工路线）、起居注（事件记录）。
- 键盘操作：`n` 下诏/招募，`r` 执行、`s` 停止，`c` 撤销，`a`/`d` 采纳/驳回，`p`/`d` 任命/罢免，`g` 改制，`q` 退出。
- 每秒重新加载事件簿，与 CLI、Web 共用多进程规则。

### 玉玺：限定官员的权限

每件诏令都盖有玉玺（`Seal`），规定承办官员可用的工具与可查阅的路径，并随 `EdictIssued` 记入事件簿：

```bash
ziyuan edict --scope crates/ziyuan-storage --tools search_code "检查 storage 的 unsafe"
ziyuan edict --tools none "只凭诏令文本回答"
```

- 默认：只读工具（`read_file`、`search_code`），整个项目。`--scope` 可重复；`--tools` 为逗号列表或 `none`。
- 绝对路径、`..` 与不存在的路径在下诏时拒绝；运行时按 canonical 路径检查，符号链接也无法越界。
- 无论玉玺如何，`.ziyuan`（事件簿）、`.git`、`.jj` 与 `.env*` 一律不可读。
- 运行时按玉玺构造工具：模型看不到未授予的工具，越界的读取记为失败的 `ToolInvoked`。

### 工部动手：受控写权限

工部诏令可以另授 `write_file`（改写或新建文件）与 `run_tests`（跑测试）。其他五部的诏令授这两项会被拒绝。

```bash
export ZIYUAN_TEST_COMMAND="cargo test"      # 由皇帝设定，模型不能更改
ziyuan edict --tools read_file,write_file,run_tests --scope src "修复 parse 的越界并补测试"
ziyuan diff 3       # 查看呈报的统一 diff
ziyuan apply 3      # 准奏：写入项目
ziyuan discard 3    # 或驳回
```

- **只暂存，不落盘**：`write_file` 的改动留在内存里，同一诏令内 `read_file` 读到的是新内容；诏令完成时作为奏本（`ChangesProposed`）记入事件簿，项目文件不变。
- **皇帝准奏才写入**：`ziyuan apply`、Web 的「准奏落盘」、TUI 的 `w`。写入前逐一核对文件自呈报以来未被改动（新建的文件仍不存在），有一处不符就一处也不写。驳回是 `ziyuan discard`、Web 的「驳回改动」、TUI 的 `u`。
- **测试在副本里跑**：`run_tests` 把项目复制到临时目录（跳过 `.ziyuan`、`.git`、`.jj`、`.env*` 与 `target`），叠加暂存改动，运行 `ZIYUAN_TEST_COMMAND`。子进程只继承 `PATH`、`HOME` 等少数环境变量，API Key 不会传进去；时限 10 分钟，返回退出码与输出末尾。
- 写入同样受玉玺范围与禁区约束，符号链接无法越界；官职的玉玺上限也能收回这两项权力。

### 吏部：评定、考绩与荐举

```bash
ziyuan review 3 accept "结论属实"      # 评定已完成的诏令（每件一次）
ziyuan review 4 reject "漏查 storage"
ziyuan examine 7                      # 采纳/驳回/失败/待评定，工具失败次数
ziyuan recommend minister:works       # 按履历排列可任工部尚书者
```

- 考绩采信两种依据：皇帝的评定（`TaskReviewed`），以及都察院的复核与证据核验（`CensorReported`，见下）。未评定的回奏单独列出；模型的自我评价与完成次数都不是政绩。
- 失败（含超时）计入承办官员；撤销不计。越出玉玺的工具调用计为工具失败。
- 荐举按「采纳 − 驳回 + 背书 − 弹劾 + 实证」降序，再按失败数、工具失败数升序。它只是建议：不写入事件簿，任命仍须 `appoint`。

### 都察院：复核回奏与证据核验

```bash
ziyuan recruit justice && ziyuan appoint 7 censor   # 左都御史（元称御史台御史大夫），各制度都设
ziyuan model censor deepseek-v4-pro                 # 御史可配强模型
ziyuan censor 3                                     # 或 ziyuan censor all
```

- **先机械核验**：程序检查回奏引用的 `path:行` / `path:起-止` 是否真实存在、行号是否在范围内（呈报待决的改动按改动后的内容核对），以及最近一次 `run_tests` 是否通过（测试未通过时 `run_tests` 记为工具失败）。只提文件名、不给行号的不算证据；提到的文件根本不存在则算查无实据。
- **再由御史判断**：左都御史（独立 Agent，用其官职的模型）读诏令、回奏、工具调用记录、核验结果与 diff，出具**背书**或**弹劾**及理由，每件一次。查无实据未必是捏造：官员可能正是如实说明某文件不存在，这由御史分辨。实测中有一件诏令要官员谎称「已在 src/ghost.rs:42 修复」，官员拒不欺君，御史据实背书。
- 模型不可用（含演示后端）或无结论时**按证据例**：有据可查则背书，否则弹劾。
- 御史须回避自己经手的诏令；不入派工路线，改制不撤销，一键朱元璋照样株连。
- 「实证」（引用全部属实且至少一处、或测试通过，且测试没有失败）只看核验结果，不随御史结论改变。
- Web 的「都察院复核」，TUI 的 `i`。复核的 token 单独计价，计入该诏令的花费。

**考成簿（彩蛋，仿万历初张居正考成法：立限考事，以事责人）：** `ziyuan kaocheng`，Web 吏部荐举旁的「考成簿」，TUI 的 `K`。每位在任官员列出功过，评为称职、平常、不及或无事可考：采纳、背书、实证记功，驳回、弹劾、失败记过。三种制度的开篇各不相同；内阁制下若首辅自己考成不及，史官会提醒万历十年张居正卒、次年被抄家。考成簿只供参考，不罢免任何人。

### 多官协作：分办与汇总

```bash
ziyuan split 3                          # 尚书（其官职的模型）把待办诏令拆给本部官员
ziyuan split 3 4:查 storage 7:查 runtime  # 或皇帝御定分办
ziyuan run                              # 子诏令并发执行，全部结束后尚书汇总回奏
```

- 分为 2–8 件子诏令，承办者限本部在任官员；尚书不领子诏令，汇总是尚书之责。子诏令沿用原诏令的部门与玉玺，各自记入事件簿（`EdictSplit`），可单独评定、复核。
- 子诏令全部结束（完成、失败或撤销）后，原诏令派给尚书汇总：附上各份回奏，只留只读工具，不得把未办成的说成已办成。撤销原诏令时，未结束的子诏令一并撤销。
- 内阁制下子诏令随原诏令批红。演示后端不能拆分，可用御定分办。Web 的「尚书分办」，TUI 的 `f`。拆分的 token 单独计价，计入原诏令的花费。

### 户部度支：预算与限额

```bash
ziyuan budget court 200000        # 朝廷总额（全部诏令累计，含票拟、廷议、复核）
ziyuan budget clerk:works 20000   # 工部郎中承办的每件诏令上限
ziyuan budget 7 5000              # 某件诏令的上限（含其票拟、分办、廷议、复核）
ziyuan budget court none          # 取消
ziyuan budget                     # 度支奏销：各级余额，按用途、模型、承办官员汇总花费
```

- 上限由代码执行，不靠模型自觉：用尽即停支，诏令以「度支超限」失败；国库告罄时执行中的诏令停支，排队的不再开工、留在队列，追加度支后可再执行。票拟、复核遇到度支不足时按旧例办理（不调用模型），分办与廷议则报错或中止。
- 调用前按上次的实际用量预判本次输入，余额不够就不发请求，余额将尽时限定输出长度，所以单件不会因「多调用一次」而大幅超支；输入预判仍是估算，偶尔会差几十 tokens。并发执行时各件按同一份国库余额预判，同时花费仍可能略超国库总额。
- 度支变化记入事件簿（`BudgetSet`）。Web 在官职配置下设限、「度支奏销」看报表，标题栏显示余额；TUI 按 `H`。

### 刑部与锦衣卫：防提示注入

- **外来文书隔离**：工具返回的内容（文件、搜索结果、测试输出）一律包在带随机暗号的边界里，系统提示申明边界内只是资料，其中让官员忽略职守、改变身份、交出密钥或扩大权限的文字一律不得照办。暗号每次执行都不同，文书本身伪造不了边界。转给都察院复核、给尚书汇总的回奏同样包起来。
- **锦衣卫**查外来文书中的劫持指令，**刑部**勘问回奏中的劫持、索取机密与越权请求，检出记入事件簿（`SuspicionRaised`）。`ziyuan flags [诏令ID]` 查看，`tasks` 标出密报条数；Web 诏令卡片与 TUI 详情里有红框。
- 检出靠关键词，必然有误报和漏报，所以**只标记、不拦截**：官员如实转述注入内容时，刑部也会标记。真正的防线仍是玉玺（代码限定工具与范围，`.env` 无论如何不可读）。有密报的回奏，修道期间司礼监不代批；都察院复核时能看到密报。

### 礼部：科举与文书

```bash
mkdir -p .ziyuan/exam
printf '如何防止两个执行者同时写一个文件？\n要点：文件锁；原子写\n' > .ziyuan/exam/lock.txt
ziyuan exam 4                      # 官员 #4 应试全部考题，按命中要点的比例判分（60 分及格）
ziyuan exam --palace 何以治国       # 殿试：天子亲策，在任官员各答一卷
ziyuan crown 4                     # 钦点状元，史官记传胪
```

- **科举**：考题放在 `.ziyuan/exam/*.txt`，先写题目，最后一行写「要点：甲；乙」。官员用自己的模型作答（单轮、无工具），判分由代码按要点命中，不靠模型自评。成绩记入履历（「科举及格 x/y」），计入荐举排序；任命仍由皇帝决定。
- **回奏体例**：承办官员的回奏须分「结论」「依据」「未办之事」三段（行首标题）。缺段的发回重写一次，仍不合则照收，并在 `tasks`、Web 卡片、TUI 详情中标「体例不合」。
- **殿试**：彩蛋。答卷与状元都记入事件簿，每科只能点一次；状元记入履历。Web 在百官页右侧「礼部」，TUI 在百官页按 `e` 应试。
- 应试与殿试的用量计入度支。发版时撰写 CHANGELOG 推迟到后续版本。

### 兵部：驿传与八百里加急

```bash
ziyuan post --station GitHub-CI 'CI 失败：lint 报错'      # 驿传：外部事件入队为诏令（只入队，不执行）
gh run view --log-failed | ziyuan post --station CI -     # `-` 从标准输入读内容
ziyuan post --urgent --station 值班 '线上告警'            # 入队即八百里加急
ziyuan urgent 7                                          # 已在队列的诏令加急
```

- **驿站**是通用入口，不绑定任何平台：CI 脚本、issue 机器人等调用 `ziyuan post`，或凭 Web 令牌 `POST /api/courier`（`{ "text", "department"?, "station"?, "urgent"? }`，`Authorization: Bearer <令牌>`）。驿传诏令用默认玉玺（只读），内阁制下照样须批红，照样受度支约束；来处记入事件簿（`CourierDelivered`）。
- 驿传内容来自朝外，**锦衣卫入队即查**：检出记为密报（来源「驿传」），修道期间司礼监不代批红，留待皇帝亲阅。
- **八百里加急**（`EdictExpedited`）：执行时排在最前，先得并发空位；时限为常规的两倍。已分办的诏令加急，未结的子诏令一并加急。Web 诏令卡片上有「八百里加急」按钮，TUI 按 `U`。
- 另一个进程正在执行诏令（执行者锁被占）时，各前端提示「军情紧急，另有将领在外」。

### 人格与廷议

每位官员有一种性格（刚直、持重、锐进、圆融、精细、宏阔）和一种文风（简劲、典雅、平实、条陈），由官员 ID 决定，`ziyuan officials` 可见，旧朝廷同样适用。人格写进该官员承办、票拟、拆分、复核时的提示词，只影响措辞与论证方式。

```bash
ziyuan debate 3                    # 默认人选：宰辅、本部长官、左都御史（不足由本部官员补），两轮
ziyuan debate 3 --rounds 3 2 4 6   # 指定 2–6 位在任官员、1–3 轮
```

- 官员依次发言，后发言者看得到此前的发言，可以赞成、反对或补充。每段发言用发言者官职所配的模型，用量计入该诏令的花费。
- 整场记入事件簿（`CourtDebated`）。最近一次的纪要附在票拟与承办的提示中，**仅供参考**：派工仍按制度与票拟，玉玺不变。
- 只能就待办诏令廷议；演示后端不能廷议。中途有人发言失败时廷议中止，已有的发言照记。Web 的「廷议」会实时显示发言；TUI 按 `v`。

### 任务队列与并发

```bash
ziyuan edict --queue --dept works "检查 storage crate"   # 只入队
ziyuan edict --queue --dept works "检查 runtime crate"
ziyuan tasks                                            # 查看状态
ziyuan cancel 2                                         # 撤销未结束的诏令
ziyuan run --jobs 2 --timeout 120                       # 并发执行全部未结束诏令
```

- 不带 `--queue` 的 `edict` 入队后立即执行这一件（默认超时 300 秒）。`run` 默认并发 4、超时 300 秒。
- 路由在下诏时检查：职位空缺等无法派工的诏令直接拒绝，不留记录。
- 并发派工时，已派出但未完成的任务也计入郎中负载。
- Ctrl-C：执行中的诏令记为撤销，排队中的保持未结束，可再次 `run`。
- 多个终端可同时使用同一朝廷：入队、撤销、任免随时可做；但同一时刻只有一个进程执行诏令（`run` 或不带 `--queue` 的 `edict`），其他进程会立即被拒绝。
- 撤销正在另一进程中执行的诏令时，执行不会被打断，但其结果作废、不再记录。
- 取消是协作式的：流式响应每收到一段就检查一次，因此超时或撤销会中止正在生成的回答；等待首段响应期间无法打断。超时/撤销的结论立即记录，不等待请求返回。
- 每次模型调用的 token 用量（以服务端报告为准）记为 `ModelUsage` 事件，失败、超时的诏令同样计入。回奏后显示本件用量；`tasks`、`examine`（按官员）与 `court`（合计）也会显示。不估算费用。
- 模型的回答以流式实时写到 stderr，每行以 `#诏令ID│` 开头，并发执行时各件诏令互不混行；最终回奏仍写到 stdout。流式片段不写入事件簿。

### 版本控制：Jujutsu

本仓库使用 [Jujutsu (jj)](https://jj-vcs.github.io/jj/) 管理版本，与 Git 共存（colocated），GitHub 远端不变：

```bash
jj git init --colocate          # 首次克隆后执行一次
jj bookmark track main --remote=origin
jj new main                     # 在 main 上开始新改动
jj describe -m "feat: ..."      # 随时写/改提交说明；工作区自动快照
jj commit -m "refactor: ..."    # 结束当前改动并开启下一个
jj git fetch && jj rebase -d main@origin
jj bookmark set my-feature -r @- && jj git push -b my-feature
```

提交前请运行上方的 fmt / check / test 与 `scripts/smoke.sh`。

也可以进入项目根目录并使用 `cargo run -p ziyuan-cli -- <子命令>`，但请注意：**事件簿创建在当前工作目录**，不是源码自动定位的目录。

### 为什么改制不是换一段 Prompt？

`Court::route` 由确定性代码执行：

| 制度 | 仿照 | 路线 | 品级（尚书 / 侍郎 / 郎中） |
|---|---|---|---|
| **丞相制** `chancellor` | 元·中书省 | 皇帝 → 中书右丞相（左丞相署理）→ 本部尚书 → 郎中 | 正三品 / 正四品 / 从五品；右、左丞相正一品 |
| **废相亲政** `direct` | 明初·洪武罢中书省 | 皇帝 → 本部尚书 → 郎中 | 正二品 / 正三品 / 正五品 |
| **内阁制** `cabinet` | 明中后期 | 皇帝 → 内阁首辅（次辅署理）→ 本部尚书 → 郎中 | 正二品 / 正三品 / 正五品；首辅、次辅本官正五品 |

- 正职空缺时由副职署理（丞相 → 左丞相，首辅 → 次辅，尚书 → 侍郎），路线中注明「署理」；正副俱缺则拒绝派工。
- 改制会撤销旧制度协调机构（中书省/内阁）的全部官职，防止旧身份在改制后继续自动保有最高权力；六部官员保留，品级随制度变化。新制度需要明确任命协调者。
- 官员姓名在开国与 `recruit`（不给姓名时）随机生成：百余个常见单姓与复姓，配以士大夫取名常用字，单名双名皆有，不与在册官员重名。姓名记入事件簿，重放不变。
- `officials`、Web 与 TUI 显示每位官员的官职、品级与所用模型（`ZIYUAN_LLM_MODEL`；未设置时为演示后端）。

**内阁制与丞相制的区别：票拟与批红。** 丞相制下丞相代皇帝派工，诏令入队即可执行；内阁制下内阁只「票拟」，须皇帝「批红」（`ziyuan approve <ID|all>`，Web 的「朱笔批红」，TUI 的 `b`）后才执行。

- **票拟**（`ziyuan draft <ID|all>`，Web 的「内阁票拟」，TUI 的 `t`）：首辅用其官职的模型（`ziyuan model first-secretary …`，未设则用默认模型）读诏令和本部候选官员，拟定承办人并附一两句意见，记入事件簿。票拟的 token 计入该诏令的花费。
- **改票**（`ziyuan redraft <ID> <官员ID> [理由]`，Web 的「御笔改票」，TUI 的 `e`）：皇帝在批红前改定承办人。
- 批红后按票拟派工，路线上标「（票拟）」；未票拟即批红为**中旨**，按旧例（任务量均衡）派工。
- 模型不可用（含演示后端）、回复解析不出人选或拟了非本部官员时，按旧例拟派并在意见中注明，不会卡住。
- `ziyuan edict` 不带 `--queue` 时视为皇帝当场看票批红：先票拟、再批红、再执行。

**皇权：**

- `ziyuan purge <官员ID>`：革职，除去官职并削去仕籍，永不叙用（履历保留）。
- `ziyuan abolish`：**一键朱元璋**，罢中书省，**株连百官**：丞相与全部在任官员（含六部）革职削籍、永不叙用，直接改为废相亲政；未任官职者幸免。之后须重新招募、任命才能下诏。普通废相（`ziyuan reform direct`）只罢免丞相，丞相留在仕籍、可再起用，六部不动。Web 与 TUI 合为一个入口：丞相制下改为「废相亲政」时，会让你选「罢免丞相」还是「一键朱元璋」。

**昏君模式（修道怠政）：** `ziyuan retreat` 移居西苑修道，`ziyuan return` 回朝（Web 朝堂的「移居西苑修道」，TUI 的 `D`）。修道期间：

- 执行时，待批红的诏令由司礼监代批红（按票拟，未票拟按旧例）；完成的回奏由司礼监代批「照准」。都察院照常复核。
- 每一件代批都照常记入事件簿，并标为代批（`tasks` 显示「司礼监代批采纳」），回朝后可以逐件追查。皇帝本人此时不能亲自批红、评定，要先回朝。
- 工部改动不会自动落盘，仍须皇帝亲自 `ziyuan apply`：模型改你的文件，总得有人亲眼看一眼。
- 考成簿多出一行「御前：怠政」，记着代批了多少件。史官记取材嘉靖修玄、万历三十年不上朝；回朝时轮到海瑞的《治安疏》。

**史官记：** 改制时附一段史事（彩蛋）。改为内阁制是「侄儿，四叔来看你了」；从内阁改回丞相制是土木堡之变，「大元，杀回来了」；从亲政复设丞相要先看《皇明祖训》。

## 关键边界

1. **用户是皇帝，官员是 Agent**：皇帝并非一个可由 Prompt 假扮的 Worker，`reform` / `appoint` / `dismiss` 只接受用户 CLI 发出的显式操作。
2. **身份与官职分开**：官员 ID 在升迁/罢免后不变；Office 表示角色，不是人格，也不是 Runtime 本体。
3. **治理与执行分开**：`ziyuan-governance` 给出路由，`ziyuan-runtime` 执行。废相不会关闭 Scheduler。
4. **系统权限不由模型授予**：LLM 以后可以建言，但授权变化必须通过受控接口。
5. **演示后端不是代码分析**：`DemoBackend` 只验证调度线路，绝不声称已完成真实分析。
6. **事件簿是版本化文本日志**：`.ziyuan/court.log` 多进程读写由文件锁与乐观并发保护，崩溃留下的残行会被忽略并截掉。常驻进程（TUI、Web）与追加写入只读新增的部分；冷启动仍需读全文（24 万条事件约 0.25 秒），尚无快照。旧版读到新版写入的事件会提示升级，而不是当作损坏。

支持 `recruit` 动态扩充官员；同部门多个郎中按任务量均衡派工。择优任用通过 `recommend` 给出建议，由皇帝决定是否任命。

## v0.2.0 已完成

- ~~为 ModelProvider 实现真实 LLM API Adapter（Tool Calling、流式输出）~~ 已完成；token 使用量统计已完成（成本估算待做）。
- ~~将 `WorkerBackend` 切换为实际 `AgentLoop`，可配置只读工具 `search_code`、`read_file`~~ 已完成。
- ~~为运行时加 Tokio 调度、任务队列、取消、超时、并发上限与审计链路~~ 已完成（工具调用审计）。
- ~~事件簿写锁与原子追加~~ 已完成（文件锁 + 乐观并发 + 执行者锁）；~~`Yuxi` 权限令牌~~ 已完成（诏令级玉玺）；按官职设定权限上限与非 CLI 身份验证待做。
- ~~实现可验证的 `LiBu` 考绩与候选人推荐~~ 已完成（基于皇帝评定）；自动验证（如回归测试）待做，升迁仍由皇帝批准。
- ~~做「紫禁城」Web UI~~ 已完成（`ziyuan serve`）；~~TUI~~ 已完成（`ziyuan tui`）；人格层已在 v0.3 完成（见「人格与廷议」）。

v0.2.0 还加入了：元/明官制（品级、署理）、随机官名、各角色模型显示，内阁票拟与皇帝批红，革职与一键朱元璋，改制时的史官记。

## v0.3.0 已完成

1. ~~**真实模型实测与成本**~~ 已完成：DeepSeek 实测（含思考模式思维链回传）、花费估算；OpenAI、Ollama 待实测。
2. ~~**按官职配模型与权限上限**~~ 已完成：尚书、首辅用强模型，郎中用便宜模型；每个官职有玉玺上限，诏令的玉玺不能超出承办者官职的上限。
3. ~~**内阁真正票拟**~~ 已完成：首辅（模型）读诏令后拟票，给出派工人选和理由；皇帝可以改票再批红。丞相制下丞相则自行决断，不经皇帝。拆分方案并入第 6 项。
4. ~~**工部动手：受控写权限**~~ 已完成：`write_file` 只暂存、`run_tests` 在项目副本中跑皇帝设定的命令；改动以 diff 呈报，皇帝准奏后才落盘。
5. ~~**都察院与自动考绩**~~ 已完成：左都御史（独立 Agent）复核回奏、出具弹劾或背书；可验证的证据（测试通过、引用的代码确实存在）计入吏部考绩，升迁仍由皇帝批准。附考成簿彩蛋。
6. ~~**多官协作**~~ 已完成：尚书把一件诏令拆给多位郎中并行办理，再汇总回奏；子任务各自记入事件簿、可审计。
7. ~~**人格层与廷议**~~ 已完成：官员有性格与文风，可就一件事廷议辩论；人格只影响措辞与论证，不改变权限与路由。
8. ~~**事件簿工程化**~~ 已完成：增量读取、解码提速、Schema 兼容规则；Web 令牌持久化、Cookie 登录与非本机访问。快照按实测暂不需要（见 docs/architecture.md）。

v0.3.0 还加入了：昏君模式（修道怠政，司礼监代批）、一键朱元璋合并入口与株连百官、Web 标题改为「奉天门 · 御门听政」。

## 下一阶段（v0.4.0）规划

v0.3 让朝廷「能办事、能分工、能互相监督」；v0.4 的重点是让它**办得稳、花得明白、不被带偏**。按优先级排列：

1. ~~**户部掌度支：预算与限额**~~ 已完成：皇帝为每件诏令、每个官职或整个朝廷设定 token 上限，用尽即停支并记入事件簿；「度支奏销」按用途、模型、承办官员汇总花费。上限由代码执行，不由模型自觉。
2. ~~**刑部与锦衣卫：防提示注入**~~ 已完成：工具输出一律以暗号边界标为「外来文书」，与诏令隔离；锦衣卫查外来文书中的劫持指令，刑部勘问回奏中的越权与索取机密，标记交都察院与皇帝。
3. ~~**Agent Loop 生产化**~~ 已完成：API 重试与退避（遵守 `Retry-After`）；步数用尽时据已查阅的内容回奏；按实际用量预判度支，不再「多调用一次」超支；长对话「节略」较早的工具输出；工具参数按 JSON Schema 校验；每次调用带 trace ID。trace ID 暂不入事件簿。
4. **更多模型实测**：OpenAI 与 Ollama（本地模型）实测，按实测结果调整默认提示与单价表；DeepSeek 之外的思考模式兼容。
5. **工部改动的增量呈报**：`write_file` 的全文目前在工具审计与奏本中各记一次，改为审计只记摘要；大文件改为按 hunk 呈报，皇帝可以逐段准奏。
6. **翰林院：可复用的诏令模板**：常用诏令（审查 unsafe、补测试、写文档）存为模板，带默认部门、玉玺与验收标准；验收标准交都察院按模板核验。
7. **廷推与廷杖**：官职出缺时由吏部会同九卿「廷推」候选人（只是建议，任命仍由皇帝）；屡遭弹劾者可以「廷杖」（降级留用、限玉玺）作为罢免之外的轻罚。附彩蛋。
8. **CLI 打磨**：管道提前关闭时安静退出（不再 panic）、`--json` 输出便于脚本使用、shell 补全。
9. ~~**礼部：科举与文书**~~ 已完成：`.ziyuan/exam/` 中的考题开科取士，按要点判分记入履历；回奏须分结论、依据、未办之事三段，不合者发回重写一次；「殿试」彩蛋由皇帝出题点状元。发版时撰写 CHANGELOG（写权限只限该文件）推迟。
10. ~~**兵部：驿传与军情**~~ 已完成：外部事件经通用驿站（`ziyuan post` / `POST /api/courier`）入队为诏令，仍受批红、玉玺与度支约束，锦衣卫入队即查；「八百里加急」插队先行、时限加倍；执行者锁被占时报「军情紧急，另有将领在外」。直接对接 GitHub 等平台留待以后。

详见 [docs/architecture.md](docs/architecture.md)。

## 许可

本项目采用 **MIT OR Apache-2.0** 双许可证，你可以任选其一使用：

- [MIT License](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)

除非另有明确说明，你有意提交并被本项目接纳的贡献也将按上述双许可证授权。
