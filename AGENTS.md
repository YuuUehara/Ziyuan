# 紫垣 ZiYuan 仓库开发约定（AGENTS.md）

本文件面向参与紫垣代码库审阅与修缮的智能体与协作者，概述分层边界、兼容承诺、画风规范与验证流程。

## 1. 分层与红线约束

- **十二个工作区 crate**：
  - `ziyuan-core`：纯领域类型与解析，零 I/O、零网络依赖。
  - `ziyuan-governance`：朝廷状态机（`Court`），所有变更必须经 `Court::apply(&CourtEvent)` 回放且与 `propose_*` 规则同源。
  - `ziyuan-sandbox-windows`：唯一承载最小私有 Win32 FFI 的平台隔离层（AppContainer + `PrivateDesktop` + Job Object）。
  - `ziyuan-runtime`：调度器、工具循环（`agent_loop`）、受控工具集、锦衣卫围栏与跨平台号舍编排；不依赖 `governance`、无 HTTP/TLS 依赖。
  - `ziyuan-llm` / `ziyuan-github`：外部协议与鉴权边界。
  - `ziyuan-storage`：事件簿（`.ziyuan/court.log`）、会典快照、受管写锁与恢复记录。
  - `ziyuan-memory`：纯派生译文记忆索引与检索，不充当权威批准依据。
  - `ziyuan-app`：`Session` 用例编排、翰林院译书项目（`text_project` / `translation`）、作品关注与 `doctor` 只读诊断。
  - `ziyuan-cli` / `ziyuan-web` / `ziyuan-tui`：三端交互层，共用 `ziyuan-app` 业务语义。
- **安全与权限边界**：
  - 根工作区与业务 crate 维持 `unsafe_code = "forbid"`。
  - 写文件与跑测试仅在获准范围内暂存或入号舍执行，须经人工审阅与 `apply` 准奏方才落盘；观政令牌（viewer）对一切写接口必须返回 `403`。
  - 严禁回显或持久化明文密钥；路径访问须规范化并拒绝 `.ziyuan`、`.git`、`.jj`、`.env*` 与越界重解析点。
- **1.0+ 稳定性约定（见 `docs/stability.md`）**：
  - 冻结的 CLI 子命令、参数、`--json` 字段、退出码（`0`/`1`/`2`）与 `court.log` v1 格式只增不改。
  - 优先采用不新增朝廷事件类型、不新增玉玺能力代码的设计，避免无故抬高旧事件簿 `readable_since` 读取门槛。

## 2. 界面画风与文档体例

- **Web「奉天门」视觉一致性**：
  - 严守「宋韵五境」（`tianqing` 汝窑天青、`mo` 墨分五色、`qinglv` 青绿山水、`xiangzhi` 宣和缃帙、夜间模式 `ye` 玄夜秉烛）以及「宋刻乌丝栏」（`body.neo-wusilan`）和疏密行距偏好（`body.neo-compact`）。
  - 新增样式一律复用 `--bg`、`--sheet`、`--well`、`--ink`、`--muted`、`--line`、`--seal`、`--cinnabar`、`--gold`、`--ok`、`--warn`、`--bad` 等语义变量，禁止硬编码仅适配浅色底的十六进制色值。
  - 新增前端逻辑优先抽为经 `/assets/*.mjs` 加载的独立 ES 模块，采用纯 DOM API（`createElement` / `textContent`）防 XSS，并配齐同名 `*.test.mjs` 单测。
- **文档与文案体例**：
  - 保持朝廷典章称谓（诏令、票拟、批红、准奏、御览定稿、观政、翰林院、垂拱枢机）与平实直白的操作说明并重；如实写明只读、草稿、实验性与人工把关边界，不夸大自动化或文学质量保证。

## 3. 验证与构建清理

每次完成代码与文档改动后，按需执行：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
node --test crates/ziyuan-web/src/*.test.mjs
```

验证完毕后务必执行 `cargo clean` 清理 `target/` 目录，避免残留大体积构建产物。
