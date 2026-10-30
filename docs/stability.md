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
