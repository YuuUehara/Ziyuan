# 紫垣 ZiYuan

> **实验性 Rust Multi-Agent 框架**：以中国古代朝廷为角色模型，支持可变治理结构、官职任免、诏令派工、事件日志。

**当前为 v0.2 开发中。** CLI 默认使用明确标记的 `DemoBackend`，只模拟 Worker 收到任务。设置 `ZIYUAN_LLM_MODEL` 后，诏令由真实模型（OpenAI 兼容 API）执行，官员只能用只读工具 `read_file`、`search_code` 查阅当前目录；仍不能修改文件或运行命令。

## 组件命名

| 名称 | 职能 | 当前状态 |
| --- | --- | --- |
| 紫垣 ZiYuan | 项目和整体框架 | Rust Workspace |
| 廷枢 Tingshu | Agent Runtime / Scheduler | `ziyuan-runtime`：Tokio 调度器（并发上限、超时、撤销）+ Tool Calling |
| 玉玺 Yuxi | 职权、审批、审计 | `Seal`：每件诏令的工具与查阅范围，随事件簿审计 |
| 诏令 ZhaoLing | 任务入口 | `Edict` 数据模型 + CLI |
| 吏部 LiBu | 考绩、升迁 | `review` 评定、`examine` 考绩、`recommend` 荐举（只采信皇帝评定） |
| 紫禁城 Forbidden City | 可视化与角色扮演 UI | 尚未实现 |

## 项目结构

```text
crates/
  ziyuan-core/        官员、官职、部门、诏令、事件等领域类型
  ziyuan-governance/  朝廷状态重放、改制、任免与任务路由（规则唯一来源）
  ziyuan-runtime/     Scheduler、WorkerBackend（Demo / Agent）、Tool Calling Loop、只读工具
  ziyuan-llm/         OpenAI 兼容 Chat Completions Adapter（ureq + serde_json）
  ziyuan-storage/     EventStore 接口；文件事件簿与内存事件簿
  ziyuan-app/         Session 用例层：提议 → 校验 → 持久化 → 更新状态
  ziyuan-cli/         `ziyuan` 命令行（仅解析参数与呈现结果）

docs/                 架构决策与扩展路线
```

## 快速开始

要求：Rust 1.89 或更新的稳定版工具链（Rust 2024 edition；事件簿文件锁需要 1.89）。第三方依赖为 `ureq`、`serde_json` 与 `tokio`。

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
export ZIYUAN_LLM_MODEL=deepseek-chat
export ZIYUAN_LLM_BASE_URL=https://api.deepseek.com/v1   # 默认 https://api.openai.com/v1
export ZIYUAN_LLM_API_KEY=...                            # 本地 Ollama 可不设
cd /path/to/your/project && ziyuan init
ziyuan edict --dept works "找出所有 unsafe 代码并说明用途"
```

单件诏令最多 12 轮模型调用。模型调用失败、超时或被撤销都会记入事件簿（`TaskFailed` / `TaskCancelled`），每次工具调用也记为 `ToolInvoked`（参数与成败，不含输出），可用 `ziyuan history` 审计。

### 玉玺：限定官员的权限

每件诏令都盖有玉玺（`Seal`），规定承办官员可用的工具与可查阅的路径，并随 `EdictIssued` 记入事件簿：

```bash
ziyuan edict --scope crates/ziyuan-storage --tools search_code "检查 storage 的 unsafe"
ziyuan edict --tools none "只凭诏令文本回答"
```

- 默认：全部只读工具，整个项目。`--scope` 可重复；`--tools` 为逗号列表或 `none`。
- 绝对路径、`..` 与不存在的路径在下诏时拒绝；运行时按 canonical 路径检查，符号链接也无法越界。
- 无论玉玺如何，`.ziyuan`（事件簿）、`.git`、`.jj` 与 `.env*` 一律不可读。
- 运行时按玉玺构造工具：模型看不到未授予的工具，越界的读取记为失败的 `ToolInvoked`。

### 吏部：评定、考绩与荐举

```bash
ziyuan review 3 accept "结论属实"      # 评定已完成的诏令（每件一次）
ziyuan review 4 reject "漏查 storage"
ziyuan examine 7                      # 采纳/驳回/失败/待评定，工具失败次数
ziyuan recommend minister:works       # 按履历排列可任工部尚书者
```

- 考绩只采信皇帝的评定（`TaskReviewed`）。未评定的回奏单独列出、不计入；模型的自我评价与完成次数都不是政绩。
- 失败（含超时）计入承办官员；撤销不计。越出玉玺的工具调用计为工具失败。
- 荐举按「采纳 − 驳回」降序，再按失败数、工具失败数升序。它只是建议：不写入事件簿，任命仍须 `appoint`。

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
- 并发派工时，已派出但未完成的任务也计入属官负载。
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

- **丞相制**：皇帝 → 丞相 → 对应部尚书 → 属官；
- **废相亲政**：皇帝 → 对应部尚书 → 属官；
- **内阁制**：皇帝 → 首辅 → 对应部尚书 → 属官。

改制会撤销先前的丞相/首辅官职，防止旧身份在改制后继续自动保有最高权力；新制度需要明确任命协调者。演示日志可观察三种不同路线。

## 关键边界

1. **用户是皇帝，官员是 Agent**：皇帝并非一个可由 Prompt 假扮的 Worker，`reform` / `appoint` / `dismiss` 只接受用户 CLI 发出的显式操作。
2. **身份与官职分开**：官员 ID 在升迁/罢免后不变；Office 表示角色，不是人格，也不是 Runtime 本体。
3. **治理与执行分开**：`ziyuan-governance` 给出路由，`ziyuan-runtime` 执行。废相不会关闭 Scheduler。
4. **系统权限不由模型授予**：LLM 以后可以建言，但授权变化必须通过受控接口。
5. **演示后端不是代码分析**：`DemoBackend` 只验证调度线路，绝不声称已完成真实分析。
6. **日志是 MVP 格式**：`.ziyuan/court.log` 是版本化文本事件日志，多进程读写由文件锁与乐观并发保护，崩溃留下的残行会被忽略并截掉；但每次追加都会重读全文，尚无快照与 Schema 迁移，不应直接用于生产。

支持 `recruit` 动态扩充官员；同部门多个属官按任务量均衡派工。择优任用通过 `recommend` 给出建议，由皇帝决定是否任命。

## 下一阶段（v0.2）

- ~~为 ModelProvider 实现真实 LLM API Adapter（Tool Calling、流式输出）~~ 已完成；token 使用量统计已完成（成本估算待做）。
- ~~将 `WorkerBackend` 切换为实际 `AgentLoop`，可配置只读工具 `search_code`、`read_file`~~ 已完成。
- ~~为运行时加 Tokio 调度、任务队列、取消、超时、并发上限与审计链路~~ 已完成（工具调用审计）。
- ~~事件簿写锁与原子追加~~ 已完成（文件锁 + 乐观并发 + 执行者锁）；~~`Yuxi` 权限令牌~~ 已完成（诏令级玉玺）；按官职设定权限上限与非 CLI 身份验证待做。
- ~~实现可验证的 `LiBu` 考绩与候选人推荐~~ 已完成（基于皇帝评定）；自动验证（如回归测试）待做，升迁仍由皇帝批准。
- 做「紫禁城」TUI/Web UI；人格层只影响措辞与辩论，不得越权。

详见 [docs/architecture.md](docs/architecture.md)。

## 许可

本项目采用 **MIT OR Apache-2.0** 双许可证，你可以任选其一使用：

- [MIT License](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)

除非另有明确说明，你有意提交并被本项目接纳的贡献也将按上述双许可证授权。
