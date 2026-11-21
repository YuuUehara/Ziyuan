# 路线图

各版实际交付与兼容性变更以 [CHANGELOG](../CHANGELOG.md) 与 [GitHub Releases](https://github.com/YuuUehara/Ziyuan/releases) 为准；兼容承诺见[稳定性约定](stability.md)。未完成议题不代表已承诺交付日期。

## 已完成里程碑

- **0.1 – 1.0（朝廷治理与受控执行基线）**：三种政体（丞相制、废相亲政、内阁制）、玉玺工具与路径权限、工部暂存与逐段准奏、户部度支、都察院复核、刑部与锦衣卫防提示注入、DAG 分办、Linux／macOS 号舍沙箱、GitHub 驿站、Web「奉天门」与 TUI「紫禁城」。
- **1.1 – 1.4（小说翻译闭环、参考与勘校）**：UTF-8 TXT／Markdown 预览（含显式 ruby 注音与安全显示）及确认导入；逐块初译、双路审校、逐段朱批、修订与御览定稿；受管写入与租约临时文件清理；本地译文记忆（`local`）与显式预览参考绑定；词证、六事逐段勘校、逐阶段用量账本与成稿 TAR 导出；syosetu 关注与新章节自动入队。
- **1.5 – 1.13（人工工作台与多格式回填）**：独立人工草稿（含注音／着重／粗斜体标记编辑器与浏览器暂存）；结构化人物／术语／称谓规则资料与段旁只读提示；三态自动版本快照与对比清理；DOCX／EPUB 导入与定稿文字回填；字幕（SRT）、本地翻译记忆与审校评论；Web 新建空项目；确定性检查、CSV／TBX 术语交换与双语检索。
- **1.14 – 1.15（连续翻译与全工作台整理）**：完整工作区备份与恢复、多格式原文追加与单章修订（TXT／Markdown／DOCX／EPUB）、跨章待查、连续双语通读与交稿预检、HTML 成稿保留完整标记、全文检索与草稿替换、纯文本 TMX 交换、终端人工译稿以及须按项目授权的远程嵌入。
- **1.34.0（Windows 原生号舍 W3 断网 stdio 外藩、Prompt Caching 与 TUI 安全回滚）**：`ziyuan-sandbox-windows` 与 `ziyuan-runtime` 落地 W3 双向匿名管道 AppContainer＋`PrivateDesktop`＋Job 长连接 stdio MCP 沙箱（断网＋独立只读项目快照＋私有 scratch，开放联网／域名白名单／`haoshe.write` 继续失败关闭）；新增 `ZIYUAN_LLM_PROMPT_CACHE`（默认 `auto` 自动为 Claude/Anthropic 注入最多 3 处 `cache_control` 断点）与跨服务商缓存命中解析，度支奏销、TUI 页头与 Web 透明展示缓存命中与写入，`USAGE` 零缓存保持旧编码、非零缓存追加两列（`readable_since = 1.34.0`）；TUI 诏令页新增 `Z` 安全回滚预览弹窗与 `y` 确认（冲突 fail-closed 阻断），与 CLI／Web 完整对齐。
- **1.33.0（辅助编程工作流闭环：符号起止行号、`read_file end_line` 与安全回滚）**：`file_outline` / `find_symbols` 为 6 类语言符号输出起止行号（`42-98`），`read_file` 新增与 `lines` 互斥的可选 `end_line` 与 `replace_text` 行范围无缝衔接；新增 `ziyuan revert <ID> [--dry-run] [--json]` 与 Web `GET`/`POST /api/tasks/{id}/changes/revert` 及「安全回滚」按钮，在工作区锁下逐字节核验磁盘后 fail-closed 回滚已落盘诏令（支持 `--skip` 还原与幂等跳过），零新增事件与玉玺代码。
- **1.32.0（辅助编程执行体验增强：主动取消、精准替换与长对话压缩）**：`ExecutionHooks::cancel_flag` / `CancelToken::flag` 与 `Tool::execute_with_cancel` 打通 `run_tests` 撤销／超时／度支停支的在途子进程主动终止；`replace_text` 支持单处与批量 `edits[]` 可选 `start_line`/`end_line` 行范围锚定及歧义／失配候选行号诊断；`agent_loop::compact_turns` 在长对话超限时确定性折叠较早完整轮次为围栏结构化工作摘要（完整保留最近 4 轮与工具配对），零新增事件与玉玺代码。
- **1.31.0（Windows 原生号舍 W2 正式接入生产）**：完成 `ziyuan-sandbox-windows` crates.io 首发与 Trusted Publishing 配置及维护者签核，`ziyuan-runtime::haoshe::run_shell` 在 Windows 默认 `auto`／`require` 下正式接入 0 能力 AppContainer＋`PrivateDesktop`＋Job Object 执行 `run_tests`（全断网、过滤 12 项宿主保留变量、有界尾部与失败扫描窗口、主动取消与整树清理），工作区维持 `unsafe_code = "forbid"`；`ZIYUAN_SANDBOX_ALLOW`、外部 `CARGO_TARGET_DIR` 与长连接 stdio 外藩继续显式失败关闭（留待 W3），零新增事件。
- **1.30.0（Windows 原生号舍 W2 工程加固与真实工具链实测）**：拆分 `ziyuan-sandbox-windows` 生产 FFI（`windows.rs`）与探针 CLI（`probe.rs`），落地动态 `user32.dll` 按任务独立隐藏桌面 `PrivateDesktop`、有界尾部输出读取与完整任务私有环境变量，原生 Windows 11 12/12 实测通过 PowerShell／`Program Files` Git 隔离执行与 `%USERPROFILE%` 工具链拒绝；`ziyuan-runtime` 补齐 `ShellRunSpec::cancel` 取消透传并明示 stdio MCP 留待 W3，零新增事件。
- **1.29.0（Windows 原生号舍 W2 预备与执行所有权边界）**：`ziyuan-sandbox-windows` 新增 `SandboxMode::AppContainerChildCapable` 与 `run_shell_sandboxed`（`cmd.exe /D /S /C`），在原生非管理员 Windows 11 实测通过子进程派生、全量文件隔离（Win32 5）、零网络能力（宿主监听零抵达）及超时／取消整树收尾；`ziyuan-runtime::haoshe` 新增拥有完整生命周期的 `run_shell` 并切换 `RunTests` 调用，Windows 生产继续按 ADR-0001 失败关闭直至完成独立 unsafe 审计与 crates.io 首发，零新增事件。
- **1.28.0（运行速度与体验补齐）**：同一模型回合内连续多条只读工具调用自动并发执行（上限 8 线程、结果与钩子保序，写/测/MCP 保持串行）；在 `ReadFile` 与根目录范围授权下自动读取并围栏注入 `AGENTS.md`/`CLAUDE.md`（4 KiB 截断 + 锦衣卫扫描）；TUI 补齐 `F`（jj 并行写分办）、`A`/`E`（运行中追加 DAG 子诏令与修改待办前置依赖）及 `V` 修订弹窗都察院批注段号提示，零新增事件。
- **1.27.0（搜索与导航深度）**：`list_files` 支持 `glob`；`search_code` 新增 `regex`、`glob`、`context_lines`（0–5）与暂存新建文件覆盖；随 `Capability::SearchCode` 新增跨文件整词引用查找工具 `find_references`（跳过注释与字符串、区分 `[定义]` 与 `[引用]`），零新增事件与玉玺代码。
- **1.26.0（命名检查白名单与编译诊断摘录）**：新增 `ZIYUAN_TEST_COMMANDS` 命名白名单（`run_tests` 可选 `preset`，仍不开放任意 shell），扩展 `rustc`/`tsc`/`mypy` 结构化编译诊断摘录，测试副本跳过 Python 缓存目录，零新增事件。
- **1.25.0（只读版本库上下文工具）**：随 `Capability::ReadFile` 新增 `vcs_status`、`vcs_diff`、`vcs_log`（支持 `jj` 与 `git`），严格只读、校验 `rev` 防注入、按玉玺范围与禁区二次过滤并设 32 KiB 上限，零新增事件。
- **1.24.0（「打回再改一轮」一键修订闭环）**：新增 `ziyuan edict --revise <诏令ID>`、Web `POST /api/tasks/{id}/revise` 与 TUI `V` 快捷键，自动携带上轮 diff、逐段驳回／都察院意见与皇帝批语下发工部修订诏令，零新增事件。
- **1.23.0（批量补丁替换与文件删除／改名暂存）**：`replace_text` 新增单次最多 50 处的原子 `edits` 批量补丁模式；随 `Capability::WriteFile` 新增 `delete_file` 与 `rename_file` 暂存工具（覆盖读／搜／符号／测试副本视图与逐段 `--skip` 准奏），`CHANGES` 以 `after = "-"` 编码删除（最低读取版本序号 57 `1.23.0`，不含删除时仍为 `0.3`），受管恢复记录开放 `Present -> Absent` 目标。
- **1.22.0（导入文件 JIS 支持与文档修订）**：新增基于 `encoding_rs` 的严格无损解码器 `decode_text_bytes` 与浏览器端同步解码，覆盖 TXT／Markdown 预览与直导、原文更新、SRT 字幕、CSV／TBX 术语表及 TMX 记忆的 `Shift_JIS`／`ISO-2022-JP`／`EUC-JP` 导入（按规范 UTF-8 归档、CLI 与 Web 预览指纹一致）；全面修正 README 与主文档中 `1.17`–`1.21` 迭代后的过时表述。零新增事件类型。
- **1.21.0（运行中调整 DAG）**：新增事件 `DAGADD`（`DagSubtaskAdded`）与 `DAGDEPS`（`DagDependenciesUpdated`），最低读取版本登记为序号 55（`1.21.0`，未使用时不抬高旧日志门槛）；支持在原诏令未结束前显式追加子诏令（严守单层 2–8 件上限、官员资格、幂等键唯一与无环校验）及修改尚未开始（`Pending`）子诏令的前置依赖（禁止改写已开始／已结束节点，上游失败或撤销后可改线解除下游阻塞）；补全 CLI `ziyuan dag add|deps`、Web `POST /api/tasks/{id}/dag/{add,deps}` 与页面交互控件。
- **1.20.0（并行写诏令 `jj workspace` 工作副本隔离）**：并发执行 ≥ 2 件写／测诏令且位于 `jj` 版本库时，自动为每件写诏令检出独立临时 `jj workspace`（`WorkspaceLease` 随执行线程析构自动 `jj workspace forget` 并清目录，启动前自动回收孤立副本，`ZIYUAN_JJ_WORKSPACE=auto|always|off` 可调）；`ziyuan split --parallel-writes` 与 Web `POST /api/tasks/{id}/split`（`parallel_writes: true`）支持在 `jj` 仓库下显式开启写子诏令并行（非 `jj` 仓库失败关闭），落盘仍沿用逐段准奏与原文防覆盖核验。零新增事件类型。
- **1.19.0（Windows 原生号舍 W1b–W1d 隔离验收与安全库接口）**：完善实验 crate `ziyuan-sandbox-windows`（仍保持 `publish = false`、不接入生产），补齐 W1b 大小写／Unicode／长路径／NTFS 重解析点边界、W1c 零网络能力与 TCP4／UDP4／IPv6 回环零抵达阻断、W1d 严格 LPAC 超时／取消／并行隔离及 Job 孙进程树收尾，并提炼 W2 就绪的安全 Rust 接口（`probe_capability`／`run_sandboxed`）。生产 `ziyuan-runtime` 在 Windows 上继续按 ADR-0001 方案 A 失败关闭，待独立审计与 W2 子进程能力评审后接入。
- **1.18.0（轻量代码符号工具与多框架测试失败摘录）**：`file_outline` 随 `read_file` 授予，`find_symbols` 随 `search_code` 授予，支持 Rust／JS／TS／Python／Go／C／C++／Markdown 的符号大纲与跨文件定义查找（跳过注释、字符串与代码块，暂存优先，先按玉玺范围与禁区过滤）；`run_tests` 失败摘录修复 Rust 1.89+ 线程 ID 匹配并扩展至 `pytest`、`node --test`、`go test`、`jest`/`vitest`。零新增事件类型。
- **1.17.0（都察院逐段审 diff 与轻量版条陈）**：修复 `replace_text` 验功失效遗漏，呈报 diff 包入隔离边界并由都察院逐段附注意见（CLI／Web／TUI 展示，绝不自动驳回）；内置只读「条陈」模板，支持 `--plan` 先出方案、`--from-plan`／Web／TUI `P` 键收窄范围授写权。
- **1.16.1（Web／TUI 画风排版与截图刷新）**：统一向导弹窗与资料编辑器三主题样式、改进 TUI 下诏弹窗焦点提示，并刷新 6 张指南截图。
- **1.16.0（工部按行读取、补丁替换与检索分页）**：`read_file` 可按行窗口读，窗口正文同样不超过 64 KiB；`replace_text` 随 `write_file` 替换不重叠计次的唯一旧文本，路径先检查；`search_code` 可限目录、忽略大小写并分页；`cargo test` 失败时另附测试名和断言；终端下诏可选范围和工具。不新增事件，不提高旧事件簿读取门槛。符号索引仍未做。
- **1.15.5（说明与命令摘要）**：更正 1.15.0 已发布的记录；准备资料后的下一步改到资料工具栏的「批准上下文」；确定性检查筛选与实际问题类型一致；网页记忆说明覆盖 `local`、`deterministic` 和已授权的 `remote`；`text` 补全补齐；连续翻译命令默认中文摘要，`--json` 才给完整 JSON。
- **1.15.1 – 1.15.4（译书工作台易用性，已发布）**：1.15.1 把「翰林院」设为独立入口（`/#translation`），工作台分为译稿、资料、通读交稿、项目管理，并用可读的章节与译稿组名称；整本 TXT 导入把网页请求放到 24 MiB JSON（原文仍是 8 MiB），预览可折叠长章和多条警告，TXT 可按非空行或空行分段，也可拆开过长段落。1.15.2 默认打开手工翻译，把三栏对照和模型译校收进下方，并合并译书文档。1.15.3 把人工送审的空段错误写成可读的章段位置。1.15.4 把注音和粗斜体工具放进译文栏，补上空白段跳转、`Ctrl+S`／`Cmd+S`、资料工具栏的「批准上下文」，并更新工作台截图。

## 后续待议方向（无时间承诺）

以下议题属于独立工程研究或长期评估方向，必须经过对应平台实测或人工盲评门槛后再决定是否并入生产流程：

1. **Windows 原生号舍 W3 后续扩展（按域名放行与 `haoshe.write`）**：W2 一次性断网 `run_tests` 已于 1.31.0 落地，W3 断网只读快照长连接 stdio 外藩（`mcp.rs`）已于 1.34.0 正式接入生产；Windows 下的 `ZIYUAN_SANDBOX_ALLOW` 门吏代理、`haoshe.write` 及外部 `CARGO_TARGET_DIR` 仍按需评估，此前继续显式失败关闭。
2. **持久化代码索引与混合检索**：轻量免索引的 `file_outline`／`find_symbols` 已在 1.18.0 落地；面向超大型仓库的持久化符号／语义混合检索与评测基线仍待后续评估。
3. **长篇翻译文学效果评估**：在同等模型、术语、预算与审批规则下开展双语盲评，详见[译文记忆评估步骤](memory.md#文学效果要怎样另行评估)。

## 不纳入范围

以下能力继续不作为产品承诺：自动抓书、自动出版、OCR、无审批无人值守批量改写，以及复杂版式／出版级排版的自动保证。

## 版本号规则

1.0 起遵循语义化版本约定：兼容新增升次版本号，兼容修复升修订号，破坏冻结接口的变更留待下一主版本。每次改动先记入 [CHANGELOG](../CHANGELOG.md) 的 `Unreleased`，发版时归档为正式版本号。
