# 紫垣 ZiYuan

[![CI](https://github.com/YuuUehara/Ziyuan/actions/workflows/ci.yml/badge.svg)](https://github.com/YuuUehara/Ziyuan/actions/workflows/ci.yml) [![Release](https://img.shields.io/github/v/release/YuuUehara/Ziyuan)](https://github.com/YuuUehara/Ziyuan/releases/latest)

> **实验性 Rust Multi-Agent 框架**：以中国古代朝廷为角色模型，支持可变治理结构、官职任免、诏令派工、事件日志。

**当前版本 v0.8.0。** CLI 默认使用明确标记的 `DemoBackend`，只模拟 Worker 收到任务。设置 `ZIYUAN_LLM_MODEL` 后，诏令由真实模型（OpenAI 兼容 API）执行，官员默认只能用只读工具 `read_file`、`list_files`、`search_code` 查阅当前目录；工部诏令可另授写文件与跑测试，改动须皇帝准奏才落盘（见下文「工部动手」）。

![奉天门：紫垣的 Web 界面](docs/images/web-overview.png)

## 组件命名

| 名称 | 职能 | 当前状态 |
| --- | --- | --- |
| 紫垣 ZiYuan | 项目和整体框架 | Rust Workspace |
| 廷枢 Tingshu | Agent Runtime / Scheduler | `ziyuan-runtime`：Tokio 调度器（并发上限、超时、撤销）+ Tool Calling |
| 玉玺 Yuxi | 职权、审批、审计 | `Seal`：每件诏令的工具与查阅范围，随事件簿审计 |
| 诏令 ZhaoLing | 任务入口 | `Edict` 数据模型 + CLI |
| 吏部 LiBu | 考绩、升迁 | `review` 评定、`examine` 考绩、`recommend` 荐举（只采信皇帝评定） |
| 紫禁城 Forbidden City | 可视化与角色扮演 UI | `ziyuan serve`：奉天门 Web 界面；`ziyuan tui`：紫禁城终端界面 |

## 项目结构

```text
crates/
  ziyuan-core/        官员、官职、部门、诏令、事件等领域类型
  ziyuan-governance/  朝廷状态重放、改制、任免与任务路由（规则唯一来源）
  ziyuan-runtime/     Scheduler、WorkerBackend（Demo / Agent）、Tool Calling Loop、工作区工具（只读与工部受控写）
  ziyuan-llm/         OpenAI 兼容 Chat Completions Adapter（ureq + serde_json）
  ziyuan-github/      兵部驿站 · GitHub：issue/PR 入队、回奏写回评论、草稿 PR（凭据只在此使用）
  ziyuan-storage/     EventStore 接口；文件事件簿与内存事件簿
  ziyuan-app/         Session 用例层：提议 → 校验 → 持久化 → 更新状态
  ziyuan-web/         奉天门 Web 界面（axum + 内嵌单页，SSE 实时推送）
  ziyuan-tui/         紫禁城终端界面（ratatui）
  ziyuan-cli/         `ziyuan` 命令行（仅解析参数与呈现结果）

docs/                 架构决策与扩展路线
```

## 安装

```bash
cargo install --git https://github.com/YuuUehara/Ziyuan ziyuan-cli --locked   # 安装 `ziyuan` 到 ~/.cargo/bin
cargo install --path crates/ziyuan-cli --locked                               # 或在本仓库中
```

发版（推送 `v*` 标签）时，GitHub Actions 会在 Release 上附预编译的 `ziyuan`：Linux x86_64、macOS x86_64 与 Apple Silicon、Windows x86_64。Release 说明取 `CHANGELOG.md` 中本版一节（可用 `ziyuan changelog` 起草），没有则自动生成；标签须与 `Cargo.toml` 的版本一致。尚未发布到 crates.io。

CI（`.github/workflows/ci.yml`）在每次推送 main 与 PR 时跑 fmt、clippy、`scripts/smoke.sh`（含全部测试），另查 Rust 1.89 能否编译，并在 macOS 与 Windows 上跑全部测试。

## 快速开始

要求：Rust 1.89 或更新的稳定版工具链（Rust 2024 edition；事件簿文件锁需要 1.89）。主要第三方依赖：`ureq`、`serde`/`serde_json`、`tokio`，Web 界面的 `axum`、`futures-util`，终端界面的 `ratatui`，ChatGPT 订阅登录校验签名的 `ring`。

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
/path/to/ziyuan/target/debug/ziyuan appoint 11 clerk:works   # 开国已有 #1–#10，新人为 #11
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

## 为什么改制不是换一段 Prompt？

`Court::route` 由确定性代码执行：

| 制度 | 仿照 | 路线 | 品级（尚书 / 侍郎 / 郎中） |
|---|---|---|---|
| **丞相制** `chancellor` | 元·中书省 | 皇帝 → 中书右丞相（左丞相署理）→ 本部尚书 → 郎中 | 正三品 / 正四品 / 从五品；右、左丞相正一品 |
| **废相亲政** `direct` | 明初·洪武罢中书省 | 皇帝 → 本部尚书 → 郎中 | 正二品 / 正三品 / 正五品 |
| **内阁制** `cabinet` | 明中后期 | 皇帝 → 内阁首辅（次辅署理）→ 本部尚书 → 郎中 | 正二品 / 正三品 / 正五品；首辅、次辅本官正五品 |

- 正职空缺时由副职署理（丞相 → 左丞相，首辅 → 次辅，尚书 → 侍郎），路线中注明「署理」；正副俱缺则拒绝派工。
- 改制会撤销旧制度协调机构（中书省/内阁）的全部官职，防止旧身份在改制后继续自动保有最高权力；六部官员保留，品级随制度变化。新制度需要明确任命协调者。
- 开国时（#1–#10）设丞相一人，吏部、工部各有尚书与郎中，户、礼、兵、刑四部各有尚书（本部无郎中时尚书亲办），六部诏令都能承接；左都御史不设（设了常朝会自动复核、多花 token）。`init` 与 `court` 会列出缺位的官职及其影响（如左都御史空缺则无人复核），并由吏部荐举一位闲居、专长相符、考绩最优的官员；任命仍须皇帝确认（Web 朝堂可一键任命，经确认框）。
- 官员姓名在开国与 `recruit`（不给姓名时）随机生成：百余个常见单姓与复姓，配以士大夫取名常用字，单名双名皆有，不与在册官员重名。姓名记入事件簿，重放不变。
- `officials`、Web 与 TUI 显示每位官员的官职、品级与所用模型（`ZIYUAN_LLM_MODEL`；未设置时为演示后端）。

内阁制下诏令须先「票拟」再由皇帝「批红」，丞相制下丞相自行派工；详见[内阁、皇权与昏君模式](docs/guide.md#内阁皇权与昏君模式)。

## 关键边界

1. **用户是皇帝，官员是 Agent**：皇帝并非一个可由 Prompt 假扮的 Worker，`reform` / `appoint` / `dismiss` 只接受用户 CLI 发出的显式操作。
2. **身份与官职分开**：官员 ID 在升迁/罢免后不变；Office 表示角色，不是人格，也不是 Runtime 本体。
3. **治理与执行分开**：`ziyuan-governance` 给出路由，`ziyuan-runtime` 执行。废相不会关闭 Scheduler。
4. **系统权限不由模型授予**：LLM 以后可以建言，但授权变化必须通过受控接口。
5. **演示后端不是代码分析**：`DemoBackend` 只验证调度线路，绝不声称已完成真实分析。
6. **事件簿是版本化文本日志**：`.ziyuan/court.log` 多进程读写由文件锁与乐观并发保护，崩溃留下的残行会被忽略并截掉。常驻进程（TUI、Web）与追加写入只读新增的部分；冷启动从《大明会典》快照（`.ziyuan/huidian`）接着回放其后的事件，整本事件簿只在看起居注等需要时读入；会典只是缓存，对不上就以事件簿为准重修。新版总能读旧事件簿；旧版读到新版写入的事件时，v0.4 起提示升级，更早的版本只报解码失败（已发布的版本改不了），`ziyuan migrate` 由新版查明一本事件簿至少要哪个版本才能打开。

支持 `recruit` 动态扩充官员；同部门多个郎中按任务量均衡派工。择优任用通过 `recommend` 给出建议，由皇帝决定是否任命。

## 文档

- [使用指南](docs/guide.md)：各项功能的详细用法。
  - 前端：[奉天门：Web 界面](docs/guide.md#奉天门web-界面)、[紫禁城：终端界面](docs/guide.md#紫禁城终端界面)、[命令行：JSON 与补全](docs/guide.md#命令行json-与补全)
  - 模型与执行：[接入真实模型](docs/guide.md#接入真实模型)、[任务队列与并发](docs/guide.md#任务队列与并发)、[常朝：常驻执行与自动复核](docs/guide.md#常朝常驻执行与自动复核)、[外藩朝贡：MCP 工具](docs/guide.md#外藩朝贡mcp-工具)
  - 权限与改动：[玉玺：限定官员的权限](docs/guide.md#玉玺限定官员的权限)、[工部动手：受控写权限](docs/guide.md#工部动手受控写权限)、[户部度支：预算与限额](docs/guide.md#户部度支预算与限额)、[刑部与锦衣卫：防提示注入](docs/guide.md#刑部与锦衣卫防提示注入)
  - 评定与监督：[吏部：评定、考绩与荐举](docs/guide.md#吏部评定考绩与荐举)、[都察院：复核回奏与证据核验](docs/guide.md#都察院复核回奏与证据核验)、[廷推与廷杖](docs/guide.md#廷推与廷杖)、[内阁、皇权与昏君模式](docs/guide.md#内阁皇权与昏君模式)
  - 协作与文书：[多官协作：分办与汇总](docs/guide.md#多官协作分办与汇总)、[人格、字号与廷议](docs/guide.md#人格字号与廷议)、[翰林院：诏令模板](docs/guide.md#翰林院诏令模板)、[礼部：科举与文书](docs/guide.md#礼部科举与文书)、[礼部修 CHANGELOG](docs/guide.md#礼部修-changelog)
  - 记录与对接：[起居注：按诏令、官员、类型、时间查](docs/guide.md#起居注按诏令官员类型时间查)、[翰林院修史：实录与前例](docs/guide.md#翰林院修史实录与前例)、[兵部：驿传与八百里加急](docs/guide.md#兵部驿传与八百里加急)、[兵部驿站 · GitHub](docs/guide.md#兵部驿站--github)、[版本控制：Jujutsu](docs/guide.md#版本控制jujutsu)
- [体系结构](docs/architecture.md)：分层、事件簿与设计取舍。
- [稳定性约定](docs/stability.md)：1.0 起冻结哪些接口。
- [CHANGELOG](CHANGELOG.md)：各版变化。
- [路线图](docs/roadmap.md)：下一版的规划。

## 许可

本项目采用 **MIT OR Apache-2.0** 双许可证，你可以任选其一使用：

- [MIT License](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)

除非另有明确说明，你有意提交并被本项目接纳的贡献也将按上述双许可证授权。
