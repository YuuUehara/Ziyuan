# 恢复记录格式与校验实现计划（Chunk 3a）

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为通用准奏提供独立版本化、8 MiB 限长、SHA-256 校验的恢复记录编解码，不提前接入尚不完整的恢复事务。

**Architecture:** `ziyuan-storage::recovery` 只处理内存记录的验证和字节编解码；不写磁盘，不读取目标，不推进世代，不暴露“完成／放弃”业务动作。记录只保存事件簿引用、确切事件编码和目标摘要，不复制呈报中的 FileChange 内容。后续 Chunk 3b 再在工作目录锁内持久化、接入事件簿和应用协调器。

**Tech Stack:** Rust、serde、serde_json、既有 workspace 依赖 ring（SHA-256）。

**计划审查：** 2026-10-31 已批准；codec 不构成恢复事务，提交前仍须对实现做规格与代码质量审查。

**边界与决策：**
- 本块不提供不受保护的保存接口；不改变 EventStore 或 Session。`Prepared` 只是可编码状态，不等于私有准备租约已经实现。
- 同时存储事件数、字节位置与此前历史的完整内容摘要，后续不能仅靠事件条数判断身份。字节位置针对完整已提交行。
- `generation_before` 为偶数，`generation_after = before + 2`，须 checked_add。状态转移及目标／事件证据由后续持锁协调器负责，codec 不自行证明业务已完成。
- 摘要是记录的规范化 typed JSON（固定字段顺序）之 SHA-256，不是文件原始排版的 SHA-256；摘要用于损坏检测，不是认证。
- 本块只支持通用准奏引用；材料暂存引用在材料事务接入前扩充格式。尚未发布的 1.2.x 实现可以继续补全 v1；一旦发布，未知新格式必须有新版本号。

## Chunk 1: 记录类型、验证与限长编解码

**Files:**
- Create: `crates/ziyuan-storage/src/recovery.rs`
- Modify: `crates/ziyuan-storage/src/lib.rs`（只新增 `pub mod recovery;`，暂不顶层重导出所有类型）
- Modify: `crates/ziyuan-storage/Cargo.toml`（新增 `ring.workspace = true`）
- Modify: `Cargo.lock`（只增加 storage 对已在锁文件内的 ring 的依赖关系，不更新版本）

### Task 1: 编码格式与损坏拒绝

- [ ] 写测试及最小可编译声明：fixture 含一个准奏记录、两个目标（创建与改写）、一条有效 `ChangesApplied` 编码；encode/decode stub 返回明确 Corrupt，见证往返断言失败再实现。
- [ ] 追加行为测试：损坏摘要、改变 payload 而未更新摘要、垃圾 JSON、未知状态／字段、格式 0、格式 2（即使 record 缺失也优先 Newer）、超出 8 MiB 的输入／输出、非法摘要、非法世代号、无效事件编码、重复目标／跳过段号及不安全相对路径。
- [ ] 执行命令：`cargo test -p ziyuan-storage --lib recovery::tests --locked`，分别保存 RED/GREEN 到 `D:/rust-project/ziyuan/target/recovery-codec-*.log`，确认行为失败非拼写编译错误。
- [ ] 实现以下数据模型及 codec；每个 public 类型有用途说明，沿用本 crate 中文文档密度。

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::PathBuf;

pub const RECOVERY_FORMAT: u64 = 1;
pub const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordState { Prepared, Published, Completed, Abandoned }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "sha256", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileState { Absent, Present(String) }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetState {
    pub path: String,
    pub before: FileState,
    pub after: FileState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationSource {
    pub task: u64,
    pub changes_version: String,
    pub skipped: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventBatch {
    pub expected_events: u64,
    pub expected_bytes: u64,
    pub history_sha256: String,
    /// 不含末尾 LF 的现行事件簿编码，包含当时固定的 Clock 事件。
    pub encoded_events: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRecord {
    pub operation_id: String,
    pub workspace: PathBuf,
    pub generation_before: u64,
    pub generation_after: u64,
    pub state: RecordState,
    pub application: ApplicationSource,
    pub events: EventBatch,
    pub targets: Vec<TargetState>,
}

#[derive(Debug)]
pub enum RecoveryError {
    TooLarge { limit: usize },
    Newer { found: u64, supported: u64 },
    Corrupt(String),
}
// 实现 Display 和 std::error::Error：不打印 record／目标内容／事件 payload；
// 消息仅含错误类别、格式号或固定验证原因。

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format_version: u64,
    sha256: String,
    record: RecoveryRecord,
}

fn hash(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let hex: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

fn valid_hash(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|s|
        s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

struct Limited { bytes: Vec<u8>, over: bool }
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_RECORD_BYTES.saturating_sub(self.bytes.len()) {
            self.over = true;
            return Err(io::Error::other("recovery record exceeds limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

fn json(value: &impl Serialize) -> Result<Vec<u8>, RecoveryError> {
    let mut out = Limited { bytes: Vec::new(), over: false };
    let result = serde_json::to_writer(&mut out, value);
    if out.over { return Err(RecoveryError::TooLarge { limit: MAX_RECORD_BYTES }); }
    result.map_err(|_| RecoveryError::Corrupt("记录无法编码".into()))?;
    Ok(out.bytes)
}

pub fn encode_record(record: &RecoveryRecord) -> Result<Vec<u8>, RecoveryError> {
    validate_record(record)?;
    let bytes = json(record)?;
    json(&Envelope {
        format_version: RECOVERY_FORMAT,
        sha256: hash(&bytes),
        record: record.clone(),
    })
}

pub fn decode_record(bytes: &[u8]) -> Result<RecoveryRecord, RecoveryError> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(RecoveryError::TooLarge { limit: MAX_RECORD_BYTES });
    }
    #[derive(Deserialize)]
    struct Header { format_version: u64 }
    let header: Header = serde_json::from_slice(bytes)
        .map_err(|_| RecoveryError::Corrupt("记录头无效".into()))?;
    if header.format_version > RECOVERY_FORMAT {
        return Err(RecoveryError::Newer { found: header.format_version, supported: RECOVERY_FORMAT });
    }
    if header.format_version != RECOVERY_FORMAT {
        return Err(RecoveryError::Corrupt("记录格式版本无效".into()));
    }
    let envelope: Envelope = serde_json::from_slice(bytes)
        .map_err(|_| RecoveryError::Corrupt("记录结构无效".into()))?;
    if !valid_hash(&envelope.sha256) || hash(&json(&envelope.record)?) != envelope.sha256 {
        return Err(RecoveryError::Corrupt("记录摘要不符".into()));
    }
    validate_record(&envelope.record)?;
    Ok(envelope.record)
}
```

`validate_record` 逐项返回固定原因的 `Corrupt`，不得打印 payload。实现细节：

1. ID 为 32 个小写 hex；workspace 为绝对路径（codec 不做 I/O 或 canonicalize，后续与 Workspace.root() 比较）；task 非零；`changes_version` 和 `history_sha256` 合法。
2. 世代前偶数、checked_add(2) 与后相等。测试 `u64::MAX-1` 不得溢出／重置。
3. skipped 无重复（原顺序保存，不擅自排序）；targets 精确字符串不重复。后续协调器负责按实际平台解析后的别名冲突，此处不假称彻底解决大小写／hard-link 别名。
4. path 为 `/` 分隔的非空相对路径，每段非空、不为 `.`／`..`，禁止反斜线、冒号、NUL；旧／新 Present 的 hash 合法。`Absent -> Absent` 拒绝。不读取目标，也不允许后续绕过领域路径权限校验。
5. 事件列表非空；每条不得含 CR／LF，须通过 `crate::codec::decode_event` 解码；至少且仅一条 `ChangesApplied`，其 task/skipped 必须与 application 一致；其他仅允许 `Clock`。这样不能伪装成任意业务提交批次。Clock 至多一条且必须在 ChangesApplied 前。
6. 通用准奏目前不支持删除：after 必须 Present；targets 可以为空（用户拒绝所有段落时，只记录 ChangesApplied）。目标摘要不能从记录凭空推导批准，后续必须核对事件簿中的原始 FileChange 和 changes_version。
7. EventBatch.expected_events／expected_bytes 是 u64 位置，不在 codec 中伪造与真实事件簿的一致性；后续必须验证完整历史摘要及其边界。

### Task 2: 最终检查与本地提交

- [ ] 对 fixture 的四种 RecordState 分别往返；证明无 FileChange.before／after 全文副本入记录（目标仅含摘要）。
- [ ] `cargo fmt --all -- --check`。
- [ ] `cargo clippy -p ziyuan-storage --all-targets --locked -- -D warnings`。
- [ ] `cargo test -p ziyuan-storage --locked`，再由控制线程运行全工作区检查。
- [ ] 规格审查后代码质量审查；有问题先修正并重跑相关检查。
- [ ] 本地 `jj commit`，摘要 `feat(storage): 增加准奏恢复记录格式与完整性校验`，含既定 Co-authored-by 行。不推送、不打标签、不发版。

**下一块（尚未实现）：** 工作目录锁内发布与更新清单、Prepared 租约、待恢复写入阻断、确切事件批次追加／前缀恢复、终态与世代收尾、Session 全入口接入。不能因 codec 完成就声称中断可恢复或译书并行写安全已经交付。
