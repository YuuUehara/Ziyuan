# 稳定性约定（1.0 起生效）

1.0 起，下列对外接口只增不改：可以加新的子命令、参数、字段与事件，但已有的不改名、不改义、不删除。确需破坏兼容时只在下一个主版本（2.0）进行，并在 CHANGELOG 的「兼容性」一节写明。1.0 之前仍可调整，但每次都写进 CHANGELOG。

## 冻结的接口

| 接口 | 约定 |
| --- | --- |
| 事件簿 `.ziyuan/court.log` | 格式 v1。新版总能读旧版写下的事件簿（有逐版样本的测试为证）；新增事件或行尾追加的可选字段不升格式版本。旧版读到新版写入的内容时提示升级，`ziyuan migrate` 指出至少要哪一版。 |
| 外藩名册 `.ziyuan/mcp.json` | `servers` 下每个服务写 `command`（可带 `args`、`env`）或 `url`（可带 `headers`），二者择一；值中的 `${VAR}` 取自环境。 |
| 令牌 `.ziyuan/web-token`、`.ziyuan/web-viewer-token` | 32 位十六进制，可带换行；内容不合格时重新生成。 |
| 考题 `.ziyuan/exam/*.txt` | 题目（可带「题：」前缀、可多行），其后一行「要点：甲；乙；丙」。 |
| CLI 子命令与参数 | 见 `ziyuan help`。「恢复默认」统一写 `default`，「一件不授」统一写 `none`；`all` 只用于票拟、批红、复核、复勘、回报等可逐件重做的批量操作，落盘、评定、撤销必须逐件进行。 |
| `--json` 输出 | `court`、`officials`、`tasks`、`history`、`records`、`migrate`、`flags`、`mcp`。字段只增不删、不改义；键的顺序不作保证。`court` 与 Web 的 `/api/state` 同构。 |
| 退出码 | 0 成功；1 运行中出错（如诏令不存在、事件簿读不了、模型调用失败）；2 用法错误（未知命令、参数不对）。管道提前关闭与 Ctrl-C 退朝视为成功。 |
| 环境变量 | `ZIYUAN_LLM_MODEL`、`ZIYUAN_LLM_BASE_URL`、`ZIYUAN_LLM_API_KEY`、`ZIYUAN_LLM_PROVIDER`、`ZIYUAN_LLM_PRICE`、`ZIYUAN_LLM_RETRIES`、`ZIYUAN_MAX_STEPS`、`ZIYUAN_TEST_COMMAND`、`ZIYUAN_TZ`、`ZIYUAN_GITHUB_REPO`、`ZIYUAN_GITHUB_TOKEN`、`ZIYUAN_CONFIG_DIR`。 |
| Web 驿站 `POST /api/courier` | 凭令牌（`Authorization: Bearer`）投递 `{ text, department?, station?, urgent? }`，只入队。 |

## 不在冻结范围内

- **库 crate 的 Rust API**：`ziyuan-core`、`ziyuan-app` 等十个 crate 是 `ziyuan` 命令行的内部实现，版本号随整体走，但不按 semver 承诺 API。
- **给人看的文字输出**：措辞、排版、史官记随时可改；脚本请用 `--json`。
- **缓存与内部文件**：《大明会典》`.ziyuan/huidian`（对不上即重修，可随时删去）、执行者锁 `.ziyuan/executor.lock`、用户配置目录中的 ChatGPT 凭据与锁。
- **Web 页面与其余 `/api/*`**：供内嵌页面使用，可随页面改动。
- **测试用的环境变量**：如 `ZIYUAN_CHATGPT_ISSUER`。
