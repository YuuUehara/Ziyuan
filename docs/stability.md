# 稳定性约定（1.0.0 已生效）

1.0 起，下列对外接口只增不改：可以加新的子命令、参数、字段与事件，但已有的不改名、不改义、不删除。确需破坏兼容时只在下一个主版本（2.0）进行，并在 CHANGELOG 的「兼容性」一节写明。实验性功能及内部实现的例外见下文。

## 冻结的接口

| 接口 | 约定 |
| --- | --- |
| 事件簿 `.ziyuan/court.log` | 格式 v1。新版总能读旧版写下的事件簿（有逐版样本的测试为证）；新增事件或行尾追加的可选字段不升格式版本。旧版读到新版写入的内容时提示升级，`ziyuan migrate` 指出至少要哪一版。 |
| 外藩名册 `.ziyuan/mcp.json` | `servers` 下每个服务写 `command`（可带 `args`、`env`、`haoshe`；`haoshe.network` 为布尔或域名列表）或 `url`（可带 `headers`），二者择一；值中的 `${VAR}` 取自环境。 |
| 令牌 `.ziyuan/web-token`、`.ziyuan/web-viewer-token` | 32 位十六进制，可带换行；内容不合格时重新生成。 |
| 考题 `.ziyuan/exam/*.txt` | 题目（可带「题：」前缀、可多行），其后一行「要点：甲；乙；丙」。 |
| CLI 子命令与参数 | 见 `ziyuan help`。「恢复默认」统一写 `default`，「一件不授」统一写 `none`；`all` 只用于票拟、批红、复核、复勘、回报等可逐件重做的批量操作，落盘、评定、撤销必须逐件进行。 |
| `--json` 输出 | `court`、`officials`、`tasks`、`history`、`records`、`migrate`、`flags`、`mcp`。字段只增不删、不改义；键的顺序不作保证。`court` 与 Web 的 `/api/state` 同构。 |
| 退出码 | 0 成功；1 运行中出错（如诏令不存在、事件簿读不了、模型调用失败）；2 用法错误（未知命令、参数不对）。管道提前关闭与 Ctrl-C 退朝视为成功。 |
| 环境变量 | `ZIYUAN_LLM_MODEL`、`ZIYUAN_LLM_BASE_URL`、`ZIYUAN_LLM_API_KEY`、`ZIYUAN_LLM_PROVIDER`、`ZIYUAN_LLM_PRICE`、`ZIYUAN_LLM_RETRIES`、`ZIYUAN_MAX_STEPS`、`ZIYUAN_CONTEXT_TOKENS`、`ZIYUAN_TEST_COMMAND`、`ZIYUAN_SANDBOX`、`ZIYUAN_SANDBOX_ALLOW`、`ZIYUAN_DELTA`、`ZIYUAN_TZ`、`ZIYUAN_GITHUB_REPO`、`ZIYUAN_GITHUB_TOKEN`、`ZIYUAN_CONFIG_DIR`。 |
| Web 驿站 `POST /api/courier` | 凭令牌（`Authorization: Bearer`）投递 `{ text, department?, station?, urgent? }`，只入队。 |

`ZIYUAN_CONTEXT_TOKENS` 为旧工具输出的节略触发阈值（正整数，默认 48000），不是模型硬窗口或度支预算；最近四份输出仍受保护。`ZIYUAN_DELTA` 指定可选 git-delta 可执行文件；未设置时查 PATH，不可用时回退内置预览，不改变逐段审批语义。

## 1.1.0 新增的文件

`text` 项目与 `follow` 关注列表独立存于 `.ziyuan/text-projects/` 和 `.ziyuan/subscriptions/`，各自的 JSON 文件带格式版本。它们不改变 `court.log` 的 v1 格式；含 DAG 的事件簿最低读取版本仍为 `1.0.0-rc.2`。

新工具遇到损坏或更新版本的文件会报错，不会重置为空。请勿手动编辑内部清单或改写原件副本。1.0 忽略这些文件不代表它能使用新功能；新命令及其输出见[文本工具](text-projects.md)和[关注教程](subscriptions.md)。`text` 与 `follow` 的命令、参数和退出码按上表约定只增不改；它们 `--json` 输出的字段和内部 JSON 文件格式暂不冻结，变化会写进 CHANGELOG。

## 1.2.0 新增

- **事件簿**：仍为格式 v1，新增三种事件 `TranslationLinked`、`TranslationFinalized`、`TranslationReopened`，`readable_since` 为 1.2.0（独立发布序号 17）。1.1.0 及更早的二进制打开含这些事件的事件簿时**拒绝打开并提示升级**，与此前新增事件的规则相同；`ziyuan migrate` 会指出最先要求 1.2.0 的行。旧事件簿照常可读，本版不要求迁移。
- **CLI**：新增 `ziyuan note` 子命令（札记），以及 `text translate`、`text review`、`text revise`、`text zhupi`、`text finalize`、`text reopen`、`text context approve`、`text status`、`text resume` 与 `follow queue`。全部是新增，没有改名、改义或删去已有子命令与参数。`--json` 的冻结清单在原有子命令上不变，另支持 `note`（以及已有的 `text`、`follow`）。
- **`--json` 字段**：冻结清单仍是上表列出的那些命令；`text`、`note`（以及 1.1.0 的 `follow`）的 `--json` 字段不冻结，变化会写进 CHANGELOG。`court`、`tasks` 等既有命令的字段只增不改，译书关联只表现为任务上新增的 `translation` 字段。
- **文件**：`translations/<项目>/` 是给用户阅读、备份的材料（上下文草稿、导出的原文与上下文、译稿），不属于内部格式承诺；`.ziyuan/notes.json` 与 `.ziyuan/text-projects/`（上下文快照、朱批、项目清单）是内部状态，格式暂不冻结，新版遇到更新版本写入的文件会报错而不会重置为空。
- **旧版不能使用这些功能**：旧版本不认识 `note` / `text` 新子命令，也读不了 `translations/` 的译稿状态与朱批。不要把“旧版不会动这些文件”说成兼容；它只是用不了这些功能。升级后用 `ziyuan migrate` 检查事件簿即可。

## 1.2.1 新增

- **事件簿**：仍为格式 v1，**没有新增事件类型**，`readable_since` 不变（最高仍为 1.2.0）。旧事件簿照常可读，无需迁移；1.2.0 及更早的二进制也不会因为本版新增的内容而拒绝读取事件簿。
- **CLI**：新增 `ziyuan text recovery status|finish|abandon`。全部是新增，没有改名、改义或删去已有子命令与参数。`status` 在仍有未收尾的受管写入时以退出码 1 结束（状态本身读取成功），便于脚本判断是否还能写入。
- **新增内部文件**：`.ziyuan/write.lock`（写锁）、`.ziyuan/write.generation`（世代号，十进制加换行，偶数为空闲）、`.ziyuan/write-protocol`（写入协议版本，当前为 `1`）、`.ziyuan/recovery.json`（恢复记录，带格式版本与 SHA-256 校验）。四个都**不在冻结范围内**，格式暂不冻结，变化写进 CHANGELOG。损坏或由更新版本写入时会报错，不会重置为空。
- **旧版不能安全使用这些工作目录**：1.2.0 及更早的版本不认识这些文件，也不会据恢复记录阻断写入。**不要用旧版本写入由 1.2.1 管理过的工作目录**，否则恢复记录可能与实际状态对不上；确有需要时先把该目录的 `.ziyuan/recovery.json` 核对清楚。

## 1.2.2 新增

- **事件簿**：仍为格式 v1，**没有新增事件类型**，`readable_since` 不变（最高仍为 1.2.0）。旧事件簿照常可读，无需迁移；1.2.0 及更早的二进制也不会因为本版新增的内容而拒绝读取事件簿。
- **CLI**：新增 `ziyuan text recovery temps [--json] [--yes]`，默认只预览。全部是新增，没有改名、改义或删去已有子命令与参数；既有命令的 `--json` 字段一个也不变。
- **新增控制文件**：`.ziyuan-lease-<id>`，建在目标文件所在目录（不在 `.ziyuan/` 内），`<id>` 为 32 位小写十六进制，内容一行 `ziyuan-lease v1 temp=<临时文件名> id=<id>`，由写入者随用随建、改名完成后删除。它可以随时重建，不是用户数据，**不在冻结范围内**；留下或丢失只影响清理判断，不影响译稿、恢复记录与事件簿。
- **准奏顺带清理**：`apply` 在每个目标所在目录内先清可证明陈旧的临时文件，再落盘。行为与输出字段的其余部分不变，清理诊断走 stderr（命令行）；TUI 占着备用屏，写 stderr 只会被下一次重绘抹掉，改为在状态栏那一行追加条数「（N 项临时文件未能清理）」，路径与原因只在 `text recovery temps` 的完整报告里给。
- **只删能证明陈旧的项**：租约被活着的写入者持有、符号链接与 junction、目录、命名不符者与恢复记录引用项一律保留；`--yes` 只授权删除通过全部核对的项，不是强制删除，也不按文件年龄或 PID 猜测。
- **退出码**：沿用冻结约定——跑完为 0（包括预览里列出可清理项），扫描／删除的 I/O 失败与读不了的目录为 1，用法错误为 2。这里有意与 `text recovery status` 不同：`status` 的 1 表示「状态读到了，但还有未收尾的写入」，清理的 1 只表示「这一轮没跑完」。

清理的已知限制如下，按实际情况写明，不声称已覆盖：

1. 极窄窗口：`create_new` 之后、整行内容写完之前被强杀，会留下内容不可解析、未加锁、也不指向任何临时文件的租约残片。清理器无法证明归属，按 `LeaseUnparsable` 永久保留并报告，须手工删除。这里有意选择泄漏而非误删：残片只有几十字节，误删会让活跃写入失去归属证据。
2. 同一目录里只要有一份读不懂或版本未知的租约，该目录本轮的临时文件候选就都不会删除（`LeaseUnknownVersion`／`LeaseUnparsable`／`LeaseIdMismatch`）。宁可不动也不误删的代价是：一份坏租约会让该目录暂时不能自动清理，直到手工处理掉它。
3. 1.2.1 及更早留下的临时文件没有租约，一律报 `TempWithoutLease`，永不自动清理；早期版本写下的残留只能手工确认后删除。
4. 临时文件名回收再用：租约只记录文件名，不记录文件对象身份，「删除临时文件后同名重建」的竞争未验证，本版不处理。
5. 同一进程内的活跃写入不在本协议覆盖范围内：租约只解决跨进程归属判断。
6. Windows 上持有锁并不阻止别的进程改名或删除（std 默认共享模式允许），协议是协作式的，不承诺抵御不受信任进程的并发替换。
7. 符号链接与 junction 形态的候选／租约只用「不是普通文件」这一条覆盖，本机只验证了目录形态；非符号链接的目录重解析点（如云占位符）仍能通过 `is_dir()`，类型判断依赖 `symlink_metadata`。
8. 硬链接数异常既没有定义为保留原因，也没有检查，本版不声称覆盖。
9. **文件系统不支持文件锁时，这个工作目录里写不进去。** 租约靠 `File::try_lock` 加锁；锁不可用（`try_lock` 报错而非“被别人占着”）时 `Lease::hold` 直接失败，于是**每一次受管写入都会失败**，包括与清理无关的准奏落盘——租约在写入路径上，不只是清理路径。部分网络文件系统属于这种情况。这是有意的取舍：没有锁就没有归属证据，宁可拒绝写入，也不留下谁也证明不了陈旧的临时文件。

本项目自身的两处残留，一并写明：

- 恢复记录读不出来时，**自动**清理在这一轮整轮跳过（失败关闭），报告为路径 `.` 的一条 `Problem`——它绝不在猜的状态下删除任何东西；手工命令此时直接以退出码 1 结束，并在错误里说明「一份都没删」。
- 恢复记录里因为「目标内容已等于新内容」而被略过的目标，不在该轮的受保护集合内。这类目标没有待写入的字节，但理论上仍少了一层保护。

## 实验性

下列功能可用，但只用模拟服务测过，1.0 起不承诺兼容：行为、参数与凭据格式都可能调整，变化写进 CHANGELOG。用真实服务实测通过后转为冻结。

- **ChatGPT 订阅**：`ZIYUAN_LLM_PROVIDER=chatgpt`、`ziyuan login` / `logout` 及其凭据（变量名本身冻结，`chatgpt` 这一取值的行为不冻结）。
- **HTTP 外藩**：`.ziyuan/mcp.json` 中以 `url` 登记的 Streamable HTTP 服务（以 `command` 登记的本地服务已冻结）。

## 不在冻结范围内

- **库 crate 的 Rust API**：`ziyuan-core`、`ziyuan-app` 等十个 crate 是 `ziyuan` 命令行的内部实现，版本号随整体走，但不按 semver 承诺 API。
- **给人看的文字输出**：措辞、排版、史官记随时可改；脚本请用 `--json`。
- **缓存与内部文件**：《大明会典》`.ziyuan/huidian`（对不上即重修，可随时删去）、执行者锁 `.ziyuan/executor.lock`、用户配置目录中的 ChatGPT 凭据与锁。
- **Web 页面与其余 `/api/*`**：供内嵌页面使用，可随页面改动。
- **测试用的环境变量**：如 `ZIYUAN_CHATGPT_ISSUER`。
