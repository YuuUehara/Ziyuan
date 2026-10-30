# rc.2 继续验收记录（2026-10-30）

**状态：本地交接修复、新增入口与真实 GitHub 外发/恢复通过；不是正式版全量验收完成。** 已发布基线为 `v1.0.0-rc.2`，本次为其上的 Unreleased 工作区改动，未发布新版本。

## 同脚本真实模型对照

Linux orb、DeepSeek `deepseek-flash`、真实 `read_file` / `write_file` 与都察院复核。隔离 Git 仓库中原文 `VALUE=7`；子任务 #2 改为 11，#3 必须读取磁盘实值再加 5。前后使用同一脚本，仅替换二进制和新的隔离目录。

| 运行 | 依赖交接 | 最终值 | 输入/输出 tokens | 脚本命令总耗时 |
| --- | --- | --- | --- | --- |
| 发布基线 1 次 | #3 未等准奏即读到 7、提议 12；陈旧提案被拒 | 11（期望 16，失败） | 20383 / 4429 | 36.87s |
| 修改后第 1 次 | #3 保持待办且受 #2 阻；准奏后再 run，读到 11 | 16，通过 | 17363 / 4042 | 32.58s |
| 修改后第 2 次 | 同上 | 16，通过 | 21827 / 5225 | 42.56s |

三次均验证：未准奏磁盘保持 7；原文抢先改成 99 后 `apply` 拒绝且不覆盖；还原后准奏为 11；`apply --commit` 只提交 `src/value.rs`，保留脏的 `notes.txt`；重复提交保持同一提交 ID。重开及 `migrate --json` 可读，最低仍为 rc.2，事件格式未变。tokens 是整个朝廷累计用量，耗时包含命令/复核/本地 Git，不是模型速度基准，不据此宣称性能改善。

初次独立探索因未任命御史而失败；补招并任命后才跑复核，不能把那次失败计为模型验收通过。正式对照脚本均包含御史设置。样本数为基线 1 次、修改后 2 次，不是模型可靠性统计。

## 公共接口与 UI

- `ZIYUAN_DELTA=/home/user/.local/bin/delta cargo test --workspace`：175 passed，0 failed；含 CLI 实际二进制、Web HTTP、存储兼容、调度、预算与沙箱负向测试。额外真实 delta 测试覆盖三浅色主题及默认深色，均含统一/左右布局。`cargo fmt --all --check`、严格 clippy 与 `scripts/smoke.sh` 通过。
- CLI/Web 均验证创建排程、循环/自身/越界依赖拒绝、字段拼写拒绝、无部分写入、相同方案重复提交、冲突方案拒绝、重开后重试。只读 Web 令牌提交返回 403。写权限反向依赖按拓扑顺序串行，不因数组顺序补出环。
- Web 实跑人工表单：自依赖提交报错且草稿不丢；改成依赖节点 1 后成功创建，实际任务 #12 依赖 #11、显示受阻。标题/操作栏固定、正文独立滚动；320/390/768px 下输入无越界，四按钮均在框内，正文 scrollWidth 等于 clientWidth。
- 真实 git-delta 0.20.1 渲染三主题 × 两布局，DOM 中有 delta HTML，未回退；主题参与缓存键，词级强调、加减号、行号及逐段审批控件保留。截图仅示范基线留存提案的样式，不证明交接修复。

## 真实 GitHub 外发与恢复（用户授权后补验）

- 私有测试仓库 `YuuUehara/ziyuan-sandbox`，独立 [issue #5](https://github.com/YuuUehara/ziyuan-sandbox/issues/5) → 诏令 #1 → DeepSeek 承办 → 都察院背书 → 人工采纳 → `apply --commit` → [回报评论](https://github.com/YuuUehara/ziyuan-sandbox/issues/5#issuecomment-6517322406) → [草稿 PR #6](https://github.com/YuuUehara/ziyuan-sandbox/pull/6)。未准奏时磁盘无文件，`github pr 1` 返回退出码 1「没有已准奏的改动」，远端无新 PR。
- 只提交 `rc2-acceptance-T2765.md` 一件文件，93 字节 Unicode 内容与独立期望逐字节一致；本地重复 `apply --commit` 未产生第二个提交。远端提交有 `Ziyuan-Edict: 1` 与 `Ziyuan-Op`，PR 正文有对应隐藏标记；独立 GitHub API 核对内容、文件列表、draft 状态。
- 重复 `github reply 1` / `github pr 1` 均以退出码 1 拒绝。各自在隔离事件簿中仅移除一条 `REPLIED`、保留原 `DISPATCHED`，然后用新 CLI 进程重试：找回原评论 ID `6517322406`、PR #6 和[原提交](https://github.com/YuuUehara/ziyuan-sandbox/commit/764dc3f72ae8b86ee2f442f7b97af4066481d577)，只补记、不重发、不新增提交。此为明确的“回执丢失”故障注入，不是真实断电或对任意并发竞争的证明。
- 采用已有且当时无打开来件的 `documentation` 标签隔离。新建后首次 CLI 拉取报告 0 件；随后独立 API 已能查到 #5，续跑同一幂等拉取才入队，未重建 issue；具体可见性延迟原因未定位。此后重复拉取均不增任务。普通评论和紫垣回帖不入队；成员 `/ziyuan` 评论单独生成 #2，重复拉取保持总计 2 件；只读 #2 也经真实模型完成。
- 全程累计输入/输出 10548 / 3420 tokens，脚本命令总耗时 59.01s（不含人工查看、暂停续跑和最终关闭操作）。御史给背书，但没有代码行引用或执行测试，`verified=false`；不以模型背书替代上述独立文件/API 核对。本轮是 Markdown 文件验收，不是代码测试验收。
- 已关闭本轮 issue #5 和草稿 PR #6，未合并；issue 最终恰有 3 条评论（普通备注、唯一回帖、成员指令）。远端默认分支 SHA、既有 issue #2/#3、PR #4 及 `ziyuan/task-3` 均未改变。本轮 `ziyuan/task-1` 分支保留供核查，未擅自删除远端分支。

证据在本线程 `.amp/in/artifacts/rc2-github-acceptance/`：固定案例、执行脚本、逐条命令、移除回执前的事件簿、前后状态、`result.json` 与 `cleanup.json`。记录包含初次拉取未就绪的失败，不覆盖为一次全绿运行。

## CRLF、多文件取舍与进程中断补验

- 真实 DeepSeek 承办、本地 CLI 与 Git：三个文件共四段；`a.txt` 两段、`b.txt` 一段、拟新建 `c.txt` 一段。驳回第 1、4 段，仅保留 `a.txt` 第二处与 `b.txt`，`c.txt` 不创建。磁盘和 Git blob 按独立期望逐字节核对，CRLF 保留；只把 CRLF 改成 LF 的并发编辑也会使整批准奏拒绝，其他文件不被写入。
- 在隔离仓库注入“首文件已写、次文件未写”状态，用新 CLI 续作，已写文件的修改时间不变；次文件若另有改动则拒绝，复原后续完。随后移除本地 `APPLIED` 回执，保留实际文件及已有 Git 提交，重试仍只补记，提交 ID 不变。无关且已暂存的文件继续留在暂存区，不被带进诏令提交。这是文件间中断/回执丢失的状态注入，不是写盘瞬间断电实验。
- 已准奏后带 `--skip` 再提交会拒绝重新作决定；不带 `--skip` 的 `apply --commit` 重试复用原提交及原取舍。初版验收脚本错误地期望前者成功，已保留失败记录并按已有 CLI 契约修正，不为测试改变产品行为。最终真实模型完整跑通一次（输入/输出 8712 / 2921 tokens）。
- 三组真实进程 `SIGKILL` / 新 CLI 重开：只在本机模型 HTTP 边界挂起，不请求真实模型或外部副作用；派工持久化后杀进程。次数上限 1 时重开记失败、不再派工；上限 2 时可手动再 `run` 完成并放行下游；中断跨过期限时记失败、下游继续受阻。重开后重复提交原排程均不追加事件。期限实验跨过数秒的真实边界，不冒充多日停机或全系统断电验证。初版脚本误用不存在的 JSON 字段/失败退出码，纠正后的三分支均通过，失败日志保留。
- 再跑 `cargo fmt --all -- --check`、`ZIYUAN_DELTA=/home/user/.local/bin/delta cargo clippy --workspace --all-targets --locked -- -D warnings`、同环境 `scripts/smoke.sh`：175 tests passed，smoke PASS。`cargo +1.89.0 check --workspace --all-targets --locked` 通过。`cargo audit` 退出 0，无已知漏洞；现有间接依赖 `bincode 1.3.3` 有停止维护警告 RUSTSEC-2025-0141，未忽略或改动依赖来掩盖。

补验证据在本线程 `.amp/in/artifacts/rc2-final-acceptance/`：`reproduce.py`、`interruption.py`、逐条命令、结果 JSON、fmt/clippy/test/smoke、MSRV 与审计日志。脚本只在隔离本地目录执行，不创建 GitHub 资源。

## 仍需完成

正式发布前仍须对应新提交的跨平台 CI；本地 MSRV/审计与上述边界补验已通过。多日停机、任意并发竞争或系统断电不在本轮证据范围。本轮 GitHub 外发是单件写任务，DAG 依赖交接是上节独立场景，不宣称已支持将多件子任务改动自动合并为一份 PR。TUI 深色渲染通过真实 delta 测试，但本轮未单独录制 TUI；不以 Web 截图代替其视觉证据。release Linux 二进制在 orb glibc 2.36 可运行，但出现弱 GLIBC_2.39 符号警告，跨旧发行版适用范围尚未验证。

本线程的 `.amp/in/artifacts/rc2-acceptance/` 保存前后命令、状态、结果 JSON 与复现脚本，`dag-schedule-flow.mp4` 保存最终表单交互。脚本读取环境中的模型凭据，不把凭据写入记录。小说翻译目前只给出工作流与只读示例，未做文学质量验收。
