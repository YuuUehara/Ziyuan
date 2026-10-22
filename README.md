# 紫垣 ZiYuan

> **实验性 Rust Multi-Agent 框架**：以中国古代朝廷为角色模型，支持可变治理结构、官职任免、诏令派工、事件日志。

**当前为 v0.2 开发中。** CLI 默认使用明确标记的 `DemoBackend`，只模拟 Worker 收到任务。设置 `ZIYUAN_LLM_MODEL` 后，诏令由真实模型（OpenAI 兼容 API）执行，官员只能用只读工具 `read_file`、`search_code` 查阅当前目录；仍不能修改文件或运行命令。

## 组件命名

| 名称 | 职能 | 当前状态 |
| --- | --- | --- |
| 紫垣 ZiYuan | 项目和整体框架 | Rust Workspace |
| 廷枢 Tingshu | Agent Runtime / Scheduler | `ziyuan-runtime`：同步演示调度 + Tool Calling 接口 |
| 玉玺 Yuxi | 职权、审批、审计 | `ziyuan-governance` 的显式任免规则，后续拆分 |
| 诏令 ZhaoLing | 任务入口 | `Edict` 数据模型 + CLI |
| 吏部 LiBu | 考绩、升迁 | `examine` 完成数，后续实现真正评估 |
| 紫禁城 Forbidden City | 可视化与角色扮演 UI | 尚未实现 |

## 项目结构

```text
crates/
  ziyuan-core/        官员、官职、部门、诏令、事件等领域类型
  ziyuan-governance/  朝廷状态重放、改制、任免与任务路由（规则唯一来源）
  ziyuan-runtime/     WorkerBackend（Demo / Agent）、Tool Calling Loop、只读工具
  ziyuan-llm/         OpenAI 兼容 Chat Completions Adapter（ureq + serde_json）
  ziyuan-storage/     EventStore 接口；文件事件簿与内存事件簿
  ziyuan-app/         Session 用例层：提议 → 校验 → 持久化 → 更新状态
  ziyuan-cli/         `ziyuan` 命令行（仅解析参数与呈现结果）

docs/                 架构决策与扩展路线
```

## 快速开始

要求：Rust 1.85 或更新的稳定版工具链（Rust 2024 edition）。第三方依赖仅限 `ziyuan-llm` 使用的 `ureq` 与 `serde_json`。

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

模型调用或工具循环失败时，诏令被拒绝，不写入事件簿。单件诏令最多 12 轮模型调用。

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
6. **日志是 MVP 格式**：`.ziyuan/court.log` 是版本化文本事件日志；尚不支持多进程写锁、事务式追加或损坏恢复，不应直接用于生产。

v0.1 支持 `recruit` 动态扩充官员；同部门多个属官目前按已记录任务数量进行简单均衡派工，**尚不具备“择优任用”能力**。

## 下一阶段（v0.2）

- ~~为 ModelProvider 实现真实 LLM API Adapter（Tool Calling）~~ 已完成；流式输出与使用量统计待做。
- ~~将 `WorkerBackend` 切换为实际 `AgentLoop`，可配置只读工具 `search_code`、`read_file`~~ 已完成。
- 为运行时加 Tokio 调度、任务队列、取消、超时、并发上限与审计链路。
- 让任命/罢免使用原子事件事务，并引入 `Yuxi` 权限令牌与非 CLI 身份验证。
- 实现可验证的 `LiBu` 考绩与候选人推荐；升迁仍由皇帝批准。
- 做「紫禁城」TUI/Web UI；人格层只影响措辞与辩论，不得越权。

详见 [docs/architecture.md](docs/architecture.md)。

## 许可

本项目采用 **MIT OR Apache-2.0** 双许可证，你可以任选其一使用：

- [MIT License](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)

除非另有明确说明，你有意提交并被本项目接纳的贡献也将按上述双许可证授权。
