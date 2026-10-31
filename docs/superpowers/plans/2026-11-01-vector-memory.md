# 1.3.0 向量翻译记忆库实施计划

日期：2026-11-01。依据[向量翻译记忆库开发设计](../specs/2026-10-31-vector-translation-memory-design.md)（已获批准）。

## 范围裁决

设计的阶段一门槛要求「预先登记质量阈值、延迟／磁盘／费用预算和人工评测办法，确认选型」。这些需要人工双语评测，本计划**不代做，也不编造数值**。因此本计划只交付**确定性机制**，把质量结论留给阶段一评审：

| 做 | 不做（本计划明确排除） |
| --- | --- |
| 记录契约：身份、版本、来源定位、摘要、嵌入签名、水位、失效态 | 选定远程嵌入模型与服务商 |
| 资格判定：只收当前 `Finalized` 且版本核验通过的逐段对 | 冻结相似度阈值（接口只回分数，不设门槛） |
| 嵌入接口 + 确定性本地提供方（零外发） | 实现任何远程嵌入提供方 |
| 可重建索引：完整性标记、格式／维度校验、先建后切换 | 训练／微调模型 |
| 范围先行的混合检索 | 声称文学收益或一致性指标 |
| 独立提示适配器（标为「参考译法」，不获工具权限） | 自动采纳参考、跨用户共享库 |

阈值与后端在阶段一评审时确定，本计划不写死。检索接口**返回分数但不按分数过滤**：过滤条件只有「授权范围」与「资格」，两者都是确定的。

## 技术事实（写代码前必须知道）

1. **段落对来源**：`Translations::project(id)` → `Project.chapters[].blocks[]`（`PromptBlock { schema_version, block_id, paragraphs: Vec<BlockParagraph { id, text }> }`，`crates/ziyuan-app/src/text_project/model.rs:41-57`）。`TextProjects::show` 每次重算哈希，**每个项目每轮只调一次**。
2. **资格唯一可信来源**是 `block_states`（`translation/status.rs:388-399`），`Finalized` 只在 `:340-346` 产生，且其前 `:312-339` 的陈旧门为空。**不得**用 `Court::translation_signoffs` 抄近路——它不做陈旧校验。
3. `BlockState::Finalized { task, translation_hash }` 的 `translation_hash` 是已定稿的 `zh.json` 内容摘要；磁盘可能被手改，因此收录前必须核对 `sha256_hex(disk_bytes) == translation_hash`（`StaleReason::TranslationEdited` 正是这条被触发的结果）。
4. **不得按下标 zip**：`validate_ids`（`text_project/validate.rs:132-189`）已覆盖缺失／重复／陌生／乱序／空译文，配对必须在它通过后按 ID 查表。
5. **水位**：`CourtEvent::TranslationFinalized` 无序号，位置即 `Vec` 下标。可复核水位 = `{events: Session::event_count(), history_sha256: EventStore::history_sha256_at_most(events), history_bytes: EventStore::history_bytes()}`，形状参照 `RecoveryRecord::EventBatch`（`ziyuan-storage/src/recovery.rs:58-66`）。`Session.store` 私有，需要新增公开方法。
6. **世代**：只在 `WriteGuard::begin/finish` 推进。索引发布**不得**调用 `begin/finish`，只读 `Workspace::generation()` 并记进水位。
7. `.ziyuan/` 已被 `DENIED_DIRS`、MCP `hidden`、`SandboxRules.hidden`、`.gitignore` 排除，索引放在 `.ziyuan/memory/` 内即可沿用这套保护。
8. 聊天模型只读 `ZIYUAN_LLM_*`，`from_env()` 在缺 `ZIYUAN_LLM_BASE_URL` 时回落到远程 URL。嵌入必须用**独立前缀**并镜像「缺席即不可用」：`ZIYUAN_MEMORY_*`，未设置即可用性为「不可用」，绝不回落到任何默认端点。
9. 无 `toml` 依赖、无配置文件。配置一律走命令行参数 + 环境变量，且必须显式。
10. `sha256_hex` 已有 4 份重复，新代码**不得**再复制第 5 份：`ziyuan-storage::content_sha256` 已公开可用；`ziyuan-memory` 用 `ring` 直接算（storage 的封装不跨进程语义，引入依赖更差）。
11. CLI 加子命令要动 10 处：`TEXT_HELP`、`TextAction`、用法串、`parse`、`run`、`help.rs` 主帮助、bash/zsh 词表、fish 词表、`JSON_COMMANDS`、测试。
12. 当前无跨项目 scope 概念：每条译书命令只收一个位置参数项目 ID。记忆库必须引入**显式授权项目集合**，默认只有当前项目。

## 文件结构

| 文件 | 责任 |
| --- | --- |
| `crates/ziyuan-memory/src/lib.rs` | crate 根、模块声明、错误类型 |
| `crates/ziyuan-memory/src/record.rs` | 记录契约、身份、来源定位、摘要、嵌入签名、水位、失效态 |
| `crates/ziyuan-memory/src/eligibility.rs` | 纯函数：给定块状态与磁盘事实，判定是否收录 |
| `crates/ziyuan-memory/src/pair.rs` | 纯函数：按 ID 配对原文／译文段落，结构不合格即整块不收 |
| `crates/ziyuan-memory/src/embed.rs` | `EmbeddingProvider` trait、`Embedding`、`ModelSignature`、`Unavailable` |
| `crates/ziyuan-memory/src/local.rs` | 确定性本地提供方（零外发） |
| `crates/ziyuan-memory/src/index.rs` | 索引格式、编解码、完整性标记、维度／格式校验、先建后切换 |
| `crates/ziyuan-memory/src/search.rs` | 范围先行的混合检索 |
| `crates/ziyuan-memory/src/watermark.rs` | 覆盖水位的编码与相等判断 |
| `crates/ziyuan-app/src/memory.rs` | 把 app 的权威状态接进 memory：`Session::memory_pairs` 等 |
| `crates/ziyuan-cli/src/memory_commands.rs` | `ziyuan text memory …` |

## 不变量（每个任务都要守）

- 索引是**可重建派生数据**：删掉不损失任何权威资料；损坏、格式更新、维度变化一律拒绝使用并提示重建，绝不重置为空后假装最新。
- 收录只来自**当前有效**的 `Finalized`；退回、版本失效、不确定、磁盘手改一律不具资格。
- 检索**先按授权范围过滤，再做向量召回**；不允许「先全库召回再隐藏」。
- 嵌入**默认零外发**；无可用提供方时明确报不可用，不静默降级为「没有参考也不说」。
- 上报数字必须可复核：`events` 与 `history_sha256` 同时记录，读取前后不一致即拒绝发布。
- 日志与报告不含原文、译文、查询正文或密钥。
- 新增内容只增不改：不动既有子命令、参数、退出码与 `--json` 字段。

## 任务

### Task 1：`ziyuan-memory` crate 与记录契约

**Files:** `Cargo.toml`（workspace members + 依赖）、`crates/ziyuan-memory/Cargo.toml`、`src/lib.rs`、`src/record.rs`。

记录必须至少含设计中「数据契约」列出的每一项。落成结构：

```rust
pub struct MemoryRecord {
    pub identity: Identity,          // workspace_id, project, chapter, block, paragraph
    pub source_language: String,
    pub target_language: String,
    pub source: Text,                // text + sha256
    pub target: Text,
    pub digests: Digests,            // block_source_sha256, block_target_sha256
    pub origin: Origin,              // finalized_events, translation_hash, context_version, zhupi_version
    pub embedding: EmbeddingSignature, // provider, model, revision, dim, metric, input_sha256
    pub watermark: Watermark,
}
```

- `Identity` 的 `workspace_id` 来自工作区根路径的规范化文本摘要（**不存绝对路径**），迁移后须重新绑定：记录它以便发现不匹配，而不是认定相符。
- 所有结构 `deny_unknown_fields`，`serde(rename_all = "snake_case")`。
- 校验函数 `MemoryRecord::validate()`：拒绝空字段、非法摘要格式（64 位小写十六进制）、`source.text` 空、维度为 0。
- 不写 I/O，不读事件簿。

**验收**：往返序列化、`deny_unknown_fields`、各字段缺失或非法时拒绝、`workspace_id` 不含绝对路径。

### Task 2：配对与资格判定（纯函数）

**Files:** `src/pair.rs`、`src/eligibility.rs`。

```rust
pub fn pair_paragraphs(
    expected: &PromptBlock,
    candidate: &PromptBlock,
) -> Result<Vec<ParagraphPair>, PairingErrors>;

pub enum Eligibility { Eligible, Ineligible(IneligibleReason) }
pub fn eligibility(state: &BlockState, disk_sha256: Option<&str>) -> Eligibility;
```

- `pair_paragraphs` 的检查必须与 `validate_ids` 等价（缺失／重复／陌生／乱序／空译文／块 ID 不符／schema 版本），但返回按 ID 配好的对；**不得按下标 zip**。
- `eligibility` 只认 `BlockState::Finalized { translation_hash, .. }` **且** `disk_sha256 == Some(translation_hash)`。其它一切状态（含 `Stale`、`Uncertain`、`Applied`）返回带原因的非资格。
- `ziyuan-memory` 不能依赖 `ziyuan-app` 的类型。因此 `pair_paragraphs` 接受**本 crate 自己的**最小视图类型（`ParaId`/`ParaText`），由 app 侧适配；`eligibility` 接受 `(state_kind, translation_hash)` 的扁平参数而不是 `BlockState`。若这样会丢失语义，改为在 app 侧实现资格映射、memory 只收已判定合格的输入——**选后者**，保持 memory 无 app 依赖。

**验收**：下标错位但 ID 正确时配对仍正确（正例）；缺段／重复／乱序／陌生／空译文各拒（负例）；资格判定对 10 种 `BlockState` 的映射在 app 侧逐项有测试。

### Task 3：嵌入接口与确定性本地提供方

**Files:** `src/embed.rs`、`src/local.rs`。

```rust
pub struct Embedding { pub vector: Vec<f32> }
pub struct ModelSignature { provider, model, revision, dim, metric }
pub trait EmbeddingProvider {
    fn signature(&self) -> &ModelSignature;
    fn embed(&self, texts: &[&str]) -> Result<Vec<Embedding>, MemoryError>;
}
pub struct Unavailable;  // 默认：任何调用都返回 MemoryError::Unavailable
```

- `Unavailable` 是默认值，不是错误状态：`from_env()` 读 `ZIYUAN_MEMORY_PROVIDER`，未设置或为 `none` 即 `Unavailable`；**不得**回落到任何 URL。
- 确定性本地提供方：`provider = "local"`, `model = "char-hash-v1"`, `dim = 256`, `metric = "cosine"`。算法固定并写进文档，使不同进程、不同平台得到同一向量：对每条文本按字符（`char`，不按字节）取 `blake2`-free 的确定性哈希——用 `ring` 的 SHA-256 对「字符 + 位置」串流，桶号为前 4 字节取模 256，符号由第 5 字节决定，最后 L2 归一化。空文本报错。
- 归一化后余弦相似度 = 点积，检索不得再假设范数为 1 之外的任何东西。

**验收**：同输入两次得同一向量（跨进程也一致，用固定期望值断言，不只断言两次相等）；不同文本不同向量；空文本报错；`Unavailable` 的每次调用都返回不可用且不触网（用例断言无 `ZIYUAN_*` 环境变量时 `from_env()` 为 `Unavailable`）。

### Task 4：索引格式与完整性

**Files:** `src/index.rs`、`src/watermark.rs`。

文件形态：单文件 JSON，`{format_version, signature, watermark, records[], integrity{count, sha256_of_records}}} `，`records` 按身份排序以保证可复现；写入用「同目录临时文件 → 刷盘 → 改名」。

- `FORMAT_VERSION = 1`。
- 解码时校验：格式版本更高 → 明确报「索引由更新版本写入」；`signature` 与当前提供方不一致（provider/model/revision/dim/metric 任一不同）→ 报「签名不符，须重建」；`integrity` 不符 → 报「索引损坏」；`records.len() != integrity.count` → 损坏。
- **绝不**在签名不符或损坏时「尽力使用」部分记录。
- 路径 `.ziyuan/memory/index.json`；写入走 `DirLock`（crate 内自实现，`File::try_lock` + 超时，模式照 `translation::DirLock`，超时 5 秒）。
- 发布前比对水位：构建开始时记下的 `history_sha256`／`events`／`generation`，与发布前重读的一致才切换；不一致即放弃本次构建并报「资料在构建期间有变」。

**验收**：往返一致；更高版本／签名不符／校验和不符／条数不符各报对应错误；构建期间事件簿变化则拒绝切换（用可注入的水位读取器模拟，不靠真实并发）；中断留下的临时文件不影响下次构建。

### Task 5：范围先行的混合检索

**Files:** `src/search.rs`。

```rust
pub struct SearchRequest<'a> {
    pub authorized: &'a BTreeSet<String>,   // 允许出现的项目
    pub project: &'a str,                   // 当前项目（必在本集合内）
    pub query: &'a str,
    pub limit: usize,
}
pub struct Hit { pub record: MemoryRecord, pub score: f32, pub lexical: f32, pub vector: f32 }
```

- 第一步过滤：`record.identity.project ∈ authorized`。**在算任何分数之前**。
- 关键词分：查询与原文的字符二元组重合率（Jaccard），确定性、无阈值。
- 向量分：余弦。
- 融合用 reciprocal rank fusion（`1/(60+rank)`，常数写进代码并注明「结构常数，不是质量阈值」），两种分数分别排名后相加。
- 不按分数过滤；只按 `limit` 截断。`limit` 上限 50，默认取 CLI 参数（不设魔数门槛）。
- 返回项带来源定位与版本（`record.identity`、`record.origin`），供提示适配器展示。

**验收**：越权项目的记录即使在向量上更近也绝不出现（用同文本不同项目的两条记录构造）；`limit` 生效；空查询报用法错误；同一输入两次结果次序相同（确定性）。

### Task 6：app 侧接入

**Files:** `crates/ziyuan-app/src/memory.rs`、`Cargo.toml`、`lib.rs`。

```rust
pub struct FinalizedPair { pub identity: ..., pub source: String, pub target: String, ... }
impl<S: EventStore> Session<S> {
    /// 当前有效定稿的逐段原译文对；结构不合格的块整块不收，并给出原因。
    pub fn memory_pairs(&self, root: &Path, project: &str) -> Result<MemoryPairs, AppError>;
    /// 可复核水位：事件条数、历史摘要、字节数、稳定偶数世代。
    pub fn memory_watermark(&self, root: &Path) -> Result<Watermark, AppError>;
}
```

- 复用 `Translations::project`（每项目一次）、`block_states`（唯一可信资格来源）、`Translations::read_translation` 读磁盘字节。
- 资格判定在 app 侧完成；`ziyuan-memory` 只收已判定合格的输入（Task 2 的裁决）。
- 配对用 Task 2 的纯函数；ID 不合格的块整块不收，理由进报告，**不部分收录**。
- 世代要求偶数：`Workspace::ensure_writable()` 通过才发布；否则报「工作区有待恢复写入」。

**验收**：`Finalized` 的块产生对；`Stale`（原文变、上下文变、磁盘手改）不产生；`Applied`／`AwaitingYulan` 不产生；残缺块整块不收且理由指名块；`memory_watermark` 的 `events` 与 `Session::event_count()` 一致。

### Task 7：CLI

**Files:** `crates/ziyuan-cli/src/memory_commands.rs`、`text_commands.rs`、`help.rs`、`main.rs`（若需注册）、`tests/`。

```
ziyuan text memory status <项目ID> [--json]
ziyuan text memory build <项目ID> [--project <其他项目ID>]... [--json]
ziyuan text memory search <项目ID> --query <文本> [--project <其他项目ID>]... [--limit N] [--json]
```

- 默认授权集合只含本命令的位置参数项目；跨项目必须在 `--project` 里逐个写明，**不提供 `--all`**。
- `search` 在无可用嵌入提供方时退 1 并说明「记忆库不可用」，**不**静默返回空结果当作「没有相似内容」。
- 输出与既有约定一致：路径只用工作区相对路径；`--json` 字段只增不改；退出码 0／1／2。
- 报告不含原文、译文、查询正文（`--json` 里 `hits[].source/target` 是**用户自己要的结果**，允许；日志与错误消息不得含）。

**验收**：`status` 在无索引时退 0 并说明未建；`build` 建后 `status` 显示条数与水位；`search` 越权项目不出现；缺 `--query` 为用法错误（退 2）；无提供方时 `search` 退 1 且消息指明原因；补全词表含 `memory`。

### Task 8：文档、稳定性与验收

**Files:** `docs/novel-translation.md`、`docs/stability.md`、`CHANGELOG.md`、`docs/roadmap.md`、`docs/acceptance/1.3.0.md`。

- 用户文档：怎么建、怎么查、默认零外发、什么叫「参考译法」、什么时候不可用。
- **选型评测门槛**文档（不是本版结论）：必须写明阶段一需要人工评测的维度（同义改写、人名、术语、对白、近似但不适用、无匹配）、须预先登记的指标与预算，并明确「本版不声称任何文学收益」。
- 稳定性：新增 `.ziyuan/memory/`（可删、可重建、非冻结）、新增子命令、无事件簿变更、退出码沿用、已知限制（本地提供方不是语义模型、阈值待定、无远程提供方、跨项目须显式授权）。
- 验收记录：验证矩阵逐项对上真实用例名。

## 完成标准

- `cargo test --workspace --locked` 全绿；`cargo clippy --workspace --all-targets --locked -- -D warnings` 干净；`cargo fmt --all -- --check` 干净。
- 索引可删可重建；损坏／版本不符／签名不符／维度不符一律拒绝使用并提示重建。
- 越权项目在任何检索结果里都不出现，且过滤发生在打分之前。
- 无可用提供方时明确报不可用，绝不外发、绝不伪装成「无相似内容」。
- 文档写明本版不声称文学收益，阈值与后端留待阶段一评审。
