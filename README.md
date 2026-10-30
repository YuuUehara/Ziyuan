# 紫垣 ZiYuan

[![CI](https://github.com/YuuUehara/Ziyuan/actions/workflows/ci.yml/badge.svg)](https://github.com/YuuUehara/Ziyuan/actions/workflows/ci.yml) [![Release](https://img.shields.io/github/v/release/YuuUehara/Ziyuan)](https://github.com/YuuUehara/Ziyuan/releases/latest)

> 用 Rust 编写的多智能体协作工具：你提出任务、分配权限、审阅结果，再决定是否把改动写入文件。界面和角色借用了中国古代朝廷的称呼。

**当前稳定版为 v1.2.0。** 新增逐块小说翻译闭环（初译、审校、朱批、修订、御览、断点续做与 Web 审阅）、御笔札记，以及新章节提醒自动入队。 可以用命令行、Web 或终端界面管理任务，查看执行记录，并在批准前审阅文件差异。

- **先试用，不必先配置模型**：未设置 `ZIYUAN_LLM_MODEL` 时只演示派工，不会真的分析或修改文件。
- **接入模型后才执行真实工作**：支持 OpenAI 兼容接口，已有 DeepSeek 实测。默认只能读取项目；修改文件和运行测试要另行授权。
- **模型提出改动，你决定是否写入**：任务完成不等于文件已修改，要审阅后执行 `apply`。
- **逐块翻译小说**：导入日文或英文原文后，按块下诏初译、请只读审校、逐段朱批、修订并御览定稿；所有译稿仍须准奏才落盘，长篇文学质量须人审。另可记下自己的认可译法（御笔札记），不必先建朝廷。

本文中的“皇帝”指用户，“官员”指执行任务的智能体，“诏令”就是任务，“准奏”是批准写入文件。命令名称保持这些约定，不需要先熟悉官制才能使用。

![奉天门：紫垣的 Web 界面](docs/images/web-overview.png)

## 安装

可从 [GitHub Releases](https://github.com/YuuUehara/Ziyuan/releases/latest) 下载适合系统的压缩包，解压后运行其中的 `ziyuan`（Windows 为 `ziyuan.exe`）。Linux 预编译包需要 glibc 2.35 或更新版本。

也可以从源码安装，需要 Rust 1.89 或更新版本：

```bash
cargo install --git https://github.com/YuuUehara/Ziyuan ziyuan-cli --locked   # 安装 `ziyuan` 到 ~/.cargo/bin
cargo install --path crates/ziyuan-cli --locked                               # 或在本仓库中
```

正式发行版会通过标签触发的可信发布流程上传到 crates.io；首次发布须先手工完成，之后由发布作业自动跟进。是否可用、有哪些版本，以 crates.io 上显示的内容为准。若该版本尚未出现在 crates.io，请用上面的 `cargo install --git` 安装。

### 平台支持

| 平台 | 任务、审阅、GitHub 对接、Web/TUI | 测试命令的隔离环境（“号舍”） |
| --- | --- | --- |
| Linux | 支持 | bubblewrap（另装；按域名放行还要 socat） |
| macOS | 支持 | 系统自带的 Seatbelt |
| Windows | 支持 | 暂缺，默认拒绝运行需要隔离的命令。设 `ZIYUAN_SANDBOX=off` 会取消隔离，风险见 [ADR-0001](docs/adr/0001-windows-sandbox.md) |

## 快速开始

安装后，在一个独立目录试用。下面的命令可在 PowerShell 或常见 Linux/macOS shell 中逐行执行；使用已有目录时不要重新运行 `mkdir`。

```bash
mkdir ziyuan-demo
cd ziyuan-demo
ziyuan init
ziyuan court
ziyuan officials
ziyuan edict --dept works "概述当前目录的文件"
ziyuan tasks
ziyuan history
```

记录保存在当前目录的 `.ziyuan/`。未配置模型时，结果会注明只是演示；若已配置模型，上述任务会读取授权范围内的文件并产生模型费用。

想用图形界面，执行 `ziyuan serve`，再打开终端打印的本机地址。不要把带令牌的地址发给他人。终端界面用 `ziyuan tui` 启动。

接下来可按需要阅读：

- [接入真实模型](docs/guide.md#接入真实模型)：设置模型名称、服务地址和密钥。
- [让模型修改文件](docs/guide.md#工部动手受控写权限)：授权后先看 `ziyuan diff <任务ID>`，确认后再 `ziyuan apply <任务ID>`。将占位的任务 ID 替换为实际数字，不要直接输入尖括号。
- [权限与安全](SECURITY.md)：哪些文件会交给模型，哪些操作需要你批准。

## 任务如何分配？

三种制度决定任务经过哪些角色。这些规则由程序执行，不是只在提示中要求模型遵守：

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

## 使用前需要知道

1. **你决定权限和任免**：模型不能靠一句话把自己变成用户；改制、任免必须通过程序提供的操作入口。
2. **官员和官职不是一回事**：官员换职后编号不变，便于追查同一执行者的记录。
3. **改制不等于停止任务**：改制调整分配路线，执行机制仍然保留；停止或撤销任务要用相应操作。
4. **权限不能由模型自行扩大**：模型可以提出建议，但不能自行授予新的系统权限。
5. **演示后端不是代码分析**：`DemoBackend` 只验证调度线路，绝不声称已完成真实分析。
6. **记录保存在本地**：`.ziyuan/court.log` 保存任务和操作记录，多个进程的写入会协调。`.ziyuan/huidian` 只是加快启动的缓存，损坏时会按记录重建。新版可读旧记录；旧版未必认识新事件，先用 `ziyuan migrate` 检查最低版本要求。

1.0 起的兼容承诺见[稳定性约定](docs/stability.md)。ChatGPT 订阅与 HTTP MCP 仍为实验性；内部 Rust 接口和 Web 页面不在兼容承诺范围内。

支持 `recruit` 动态扩充官员；同部门多个郎中按任务量均衡派工。择优任用通过 `recommend` 给出建议，由皇帝决定是否任命。

## 开发与发布

源码分为十个 crate：`core` 定义任务与角色，`governance` 执行分配规则，`runtime` 执行模型与工具调用，`llm` 和 `github` 连接外部服务，`storage` 保存记录，`app` 提供各界面共用的功能，`cli`、`web`、`tui` 负责交互。完整说明见[体系结构](docs/architecture.md)。

在仓库根目录检查和构建：

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo test --workspace
cargo build --workspace
bash scripts/smoke.sh
```

最后一条需要 Bash 及脚本所需的运行环境。常规 CI 还检查 Rust 1.89、Linux glibc 2.35 基线，并在 macOS 和 Windows 上运行测试；具体流程见 `.github/workflows/ci.yml`。

推送与 `Cargo.toml` 版本一致的 `v*` 标签会触发发布：先建草稿，上传四个平台的包，再在对应平台重新下载验证，全部通过才公开；公开后另有一个作业按依赖顺序把十个 crate 发布到 crates.io（已存在的版本跳过）。说明取 CHANGELOG 中对应版本一节。crates.io 不能删除版本，只能 yank；可信发布须逐 crate 预先配置，首个版本只能手工发布，详见[发布指南](docs/releasing.md)。Linux GNU 包从 rc.4 起在 Ubuntu 22.04 构建；这不是所有 Linux 发行版都经过验证的承诺。旧 rc.3 包可能在旧 glibc 上提示 `GLIBC_2.39` 警告，请升级或在目标环境从源码构建。

## 文档

- [使用指南](docs/guide.md)：各项功能的详细用法。
  - 前端：[奉天门：Web 界面](docs/guide.md#奉天门web-界面)、[紫禁城：终端界面](docs/guide.md#紫禁城终端界面)、[命令行：JSON 与补全](docs/guide.md#命令行json-与补全)
  - 模型与执行：[接入真实模型](docs/guide.md#接入真实模型)、[任务队列与并发](docs/guide.md#任务队列与并发)、[常朝：常驻执行与自动复核](docs/guide.md#常朝常驻执行与自动复核)、[外藩朝贡：MCP 工具](docs/guide.md#外藩朝贡mcp-工具)
  - 权限与改动：[玉玺：限定官员的权限](docs/guide.md#玉玺限定官员的权限)、[工部动手：受控写权限](docs/guide.md#工部动手受控写权限)、[户部度支：预算与限额](docs/guide.md#户部度支预算与限额)、[刑部与锦衣卫：防提示注入](docs/guide.md#刑部与锦衣卫防提示注入)
  - 评定与监督：[吏部：评定、考绩与荐举](docs/guide.md#吏部评定考绩与荐举)、[都察院：复核回奏与证据核验](docs/guide.md#都察院复核回奏与证据核验)、[廷推与廷杖](docs/guide.md#廷推与廷杖)、[内阁、皇权与昏君模式](docs/guide.md#内阁皇权与昏君模式)
  - 协作与文书：[多官协作：分办与汇总](docs/guide.md#多官协作分办与汇总)、[人格、字号与廷议](docs/guide.md#人格字号与廷议)、[翰林院：诏令模板](docs/guide.md#翰林院诏令模板)、[礼部：科举与文书](docs/guide.md#礼部科举与文书)、[礼部修 CHANGELOG](docs/guide.md#礼部修-changelog)
  - 记录与对接：[起居注：按诏令、官员、类型、时间查](docs/guide.md#起居注按诏令官员类型时间查)、[翰林院修史：实录与前例](docs/guide.md#翰林院修史实录与前例)、[兵部：驿传与八百里加急](docs/guide.md#兵部驿传与八百里加急)、[兵部驿站 · GitHub](docs/guide.md#兵部驿站--github)、[版本控制：Jujutsu](docs/guide.md#版本控制jujutsu)
- [体系结构](docs/architecture.md)：分层、事件簿与设计取舍。
- [文档目录](docs/README.md)：全部文档的分类入口。
- [稳定性约定](docs/stability.md)：1.0 起冻结哪些接口。
- [CHANGELOG](CHANGELOG.md)：各版变化。
- [路线图](docs/roadmap.md)：下一版的规划。
- [整理原文与检查译稿](docs/text-projects.md)：导入原文、分块与检查候选译稿，1.1.0 起可用。
- [作品更新提醒](docs/subscriptions.md)：syosetu 关注列表、检查、Web 定时设置与 1.2.0 的自动入队，不下载正文。
- [小说翻译指南](docs/novel-translation.md)：1.2.0 的逐块初译、审校、朱批、修订与御览流程；1.0.0 的日／英短样本保留为历史试点，长篇文学质量仍须人审。
- [发布指南](docs/releasing.md)：标签发布、四平台验收、crates.io 可信发布的设置与注意事项。

## 许可

本项目采用 **MIT OR Apache-2.0** 双许可证，你可以任选其一使用：

- [MIT License](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)

除非另有明确说明，你有意提交并被本项目接纳的贡献也将按上述双许可证授权。

Web 界面内置的 [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono) 字体按 SIL Open Font License 1.1 随附，许可证见 [`crates/ziyuan-web/src/fonts/JetBrainsMono-OFL.txt`](crates/ziyuan-web/src/fonts/JetBrainsMono-OFL.txt)（发行包中为 `JetBrainsMono-OFL.txt`）。界面推荐的中文、日文字体也都是 OFL 1.1，不随程序附带，见[使用指南 · 字体](docs/guide.md#字体)。

软件的 MIT / Apache-2.0 许可不授权第三方文学原文、译文或书库品牌；模型上传、再分发与商业出版仍须逐项核对权利，见[小说翻译版权边界](docs/novel-translation.md#发布前版权核对2026-10-30)。
