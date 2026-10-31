//! 索引格式：信封、完整性标记、逐条一致性校验与「先写临时文件再改名」的发布。
//!
//! 索引是**可重建的派生数据**：删掉不损失任何权威资料。因此损坏、格式更新、维度变化
//! 一律拒绝使用并提示重建，**绝不**重置为空后假装最新；也绝不「尽力使用」其中一部分记录。
//!
//! 文件形态（`.ziyuan/memory/index.json`）：
//!
//! ```text
//! {
//!   "format_version": 1,
//!   "signature": { "provider": …, "model": …, "revision": …, "dim": …, "metric": … },
//!   "watermark":  { "events": …, "history_sha256": …, "history_bytes": …, "generation": … },
//!   "records": [ … ],
//!   "integrity": { "count": …, "sha256": … }
//! }
//! ```
//!
//! `integrity.sha256` 是**记录数组规范编码**的摘要：`serde_json::to_vec` 的结果，且记录
//! 必须先按身份排序（`project`、`chapter`、`block`、`paragraph`，全同则按 `workspace_id`
//! 兜底，再全同则按记录的规范编码兜底，使排序成为全序）。排序**是格式的一部分**：同一组
//! 记录无论以什么顺序交进来，都必须编出同一串字节。
//!
//! 摘要**看不见顺序**——它算在排好序的字节上，乱序的文件摘要照样对得上。因此解码时另有
//! 一道显式的顺序核对：文件的记录顺序必须等于规范顺序，否则按损坏拒收（见 [`MemoryIndex::decode`]）。
//! 只靠摘要就会把乱序文件当成合格索引读出来，而读出的对象不再与 `encode` 的产出逐字节相同。
//!
//! 身份的**唯一性**同样是格式的一部分：同一段落只能有一条记录（重新定稿是**换掉**现行版本，
//! 不是并排堆两条）。编码前发现重复即拒收，绝不让「同一身份、不同内容」的两条记录靠输入
//! 顺序决定谁先谁后。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::MemoryError;
use crate::embed::ModelSignature;
use crate::record::{Identity, MemoryRecord, Watermark, sha256_hex};
use crate::watermark::{WatermarkReader, require_publishable};

/// 本版认识的索引格式版本。
pub const FORMAT_VERSION: u64 = 1;

/// `.ziyuan/` 下的记忆库目录（相对工作区根）。
const MEMORY_DIR: [&str; 2] = [".ziyuan", "memory"];
/// 索引文件名。
const INDEX_FILE: &str = "index.json";
/// 排他锁文件名，与 `translation::DirLock` 同名同义。
const LOCK_FILE: &str = ".lock";
const LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// 完整性标记：记录条数与记录数组规范编码的摘要。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct Integrity {
    count: usize,
    sha256: String,
}

/// 写出去的整个对象。
#[derive(Serialize)]
struct Envelope<'a> {
    format_version: u64,
    signature: &'a ModelSignature,
    watermark: &'a Watermark,
    records: &'a [MemoryRecord],
    integrity: Integrity,
}

/// 读进来的整个对象。字段形状与写出去的相同，`deny_unknown_fields` 防止夹带。
#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct RawEnvelope {
    format_version: u64,
    signature: ModelSignature,
    watermark: Watermark,
    records: Vec<MemoryRecord>,
    integrity: Integrity,
}

/// 一份索引：信封签名与水位，加记录。
///
/// 不存向量：向量由提供方在检索时现算，索引里只留签名与输入摘要。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryIndex {
    pub format_version: u64,
    pub signature: ModelSignature,
    pub watermark: Watermark,
    pub records: Vec<MemoryRecord>,
}

impl MemoryIndex {
    /// 编成索引文件字节：外层美观排版并带末尾换行（仓库惯例）。
    ///
    /// **排版只在外层**：记录数组也是美观排版的一部分，逐条多行；紧凑编码只用在
    /// `integrity.sha256` 的计算上（`serde_json::to_vec`），与文件里的排版无关。
    ///
    /// 记录在这里先查身份唯一性、再排序，摘要按排序后的字节算。记录或信封签名不合法
    /// 即拒绝编码——宁可编不出来，也不写出会被自己拒收的文件。
    pub fn encode(&self) -> Result<Vec<u8>, MemoryError> {
        self.signature.validate().map_err(MemoryError::Invalid)?;
        for record in &self.records {
            record.validate_format()?;
        }
        // 先查重再排序：排序是全序，重复身份也能排出确定顺序，但那会让「同一身份、不同内容」
        // 的两条记录悄悄共存，且谁先谁后由输入顺序决定。设计中重新定稿是换掉现行版本，
        // 因此重复就是调用方的错，宁可编不出来。
        require_unique_identities(&self.records)?;
        let records = sorted_records(&self.records)?;
        let integrity = Integrity {
            count: records.len(),
            sha256: sha256_hex(&canonical_record_bytes(&records)?),
        };
        let envelope = Envelope {
            format_version: self.format_version,
            signature: &self.signature,
            watermark: &self.watermark,
            records: &records,
            integrity,
        };
        let mut bytes = serde_json::to_vec_pretty(&envelope)
            .map_err(|e| MemoryError::Corrupt(format!("索引无法序列化: {e}")))?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// 解码并校验整份索引；任一处不合格即整份拒收，**绝不**返回部分索引。
    ///
    /// 除摘要之外还显式核对记录顺序：摘要算在规范顺序的字节上，乱序文件的摘要照样对得上，
    /// 只比摘要就会读出一份与 `encode` 产出不再逐字节相同的对象。
    pub fn decode(bytes: &[u8]) -> Result<Self, MemoryError> {
        let raw: RawEnvelope = serde_json::from_slice(bytes)
            .map_err(|e| MemoryError::Corrupt(format!("无法解码: {e}")))?;
        if raw.format_version == 0 {
            return Err(MemoryError::Corrupt("索引格式 0 不受支持".into()));
        }
        if raw.format_version > FORMAT_VERSION {
            return Err(MemoryError::Newer {
                found: raw.format_version,
                supported: FORMAT_VERSION,
            });
        }
        raw.signature
            .validate()
            .map_err(|reason| MemoryError::Corrupt(format!("信封签名不合规: {reason}")))?;
        if raw.integrity.count != raw.records.len() {
            return Err(MemoryError::Corrupt(format!(
                "记录条数不符: integrity.count 为 {}，实有 {} 条",
                raw.integrity.count,
                raw.records.len()
            )));
        }
        let canonical = canonical_record_bytes(&raw.records)?;
        if raw.integrity.sha256 != sha256_hex(&canonical) {
            return Err(MemoryError::Corrupt(
                "integrity.sha256 与记录内容不符".into(),
            ));
        }
        // 摘要看不见顺序，所以这里单独核对。经过这一关，`raw.records` 就是规范顺序，
        // 读出的对象与 `encode` 的产出逐字节相同。
        if raw.records != sorted_records(&raw.records)? {
            return Err(MemoryError::Corrupt(
                "记录未按规范顺序排列；索引损坏可删去，重建即可".into(),
            ));
        }
        for record in &raw.records {
            let label = record_label(&record.identity);
            record
                .validate_format()
                .map_err(|e| corrupt_record(&label, "格式不合规", &e))?;
            // 记录里的签名／水位是冗余副本：一旦与信封走散，检索就会拿着与构建时不同的
            // 签名或水位去算，而这种错配没有别的地方能发现。逐条比对，任一条不符即整份拒收。
            if !raw.signature.matches(&record.embedding) {
                return Err(MemoryError::Corrupt(format!(
                    "记录 {label} 的嵌入签名与索引信封不一致"
                )));
            }
            if record.watermark != raw.watermark {
                return Err(MemoryError::Corrupt(format!(
                    "记录 {label} 的水位与索引信封不一致"
                )));
            }
        }
        Ok(Self {
            format_version: raw.format_version,
            signature: raw.signature,
            watermark: raw.watermark,
            records: raw.records,
        })
    }

    /// 发布到 `<root>/.ziyuan/memory/index.json`，返回写下的路径。
    ///
    /// 先编码、后建目录：这份索引因记录不合规而编不出来时，不该在磁盘上留下一个空的
    /// `memory/`，让人误以为「建过但没内容」。目录不存在就建；写入走 [`DirLock`]
    /// （`<root>/.ziyuan/memory/.lock`，超时 5 秒），再「同目录临时文件 → 刷盘 → 改名」。
    /// 绝不写到 `.ziyuan/memory/` 之外。
    ///
    /// 只覆盖 `index.json`：它存在但是坏的，发布就把它整个换掉（索引是派生数据，
    /// 覆盖坏文件正是想要的结果）。
    pub fn publish(&self, root: &Path) -> Result<PathBuf, MemoryError> {
        let bytes = self.encode()?;
        let dir = memory_dir(root);
        fs::create_dir_all(&dir)?;
        let path = dir.join(INDEX_FILE);
        let _lock = DirLock::acquire(&dir)?;
        write_atomic(&path, &bytes)?;
        Ok(path)
    }

    /// 发布，但先重读一次水位：与索引里记的不一致就不写。
    ///
    /// 「记的水位」是构建开始时拍下的（[`Self::watermark`]），`read` 是发布前这一刻的实况。
    /// 两者对不上（资料变了，或工作区还有未收尾的受管写入）就整批放弃，磁盘上原有的索引
    /// 一字不动——留着旧索引好过发布一份说不清覆盖到哪一步的新索引。
    ///
    /// `read` 由调用方注入：生产里是去读事件簿与世代，测试里可以给一个反例闭包，
    /// 从而不必造真实并发就能测「构建期间事件簿变了」。
    pub fn publish_checked(
        &self,
        root: &Path,
        read: WatermarkReader<'_>,
    ) -> Result<PathBuf, MemoryError> {
        let after = read()?;
        require_publishable(&self.watermark, &after)?;
        self.publish(root)
    }

    /// 读 `<root>/.ziyuan/memory/index.json`；不存在即 `Ok(None)`。
    ///
    /// 存在但损坏是**错误**，绝不是 `None`：把损坏当成「还没建过」就等于悄悄重置。
    ///
    /// 这里**不取共享锁**：读只碰一个路径，与 [`Self::publish`] 的改名是原子的，没必要
    /// 为此付出锁的文件句柄。代价是读的这一刻可能正好撞上改名——此时要么读到旧的完整
    /// 内容，要么在极窄的窗口里失败。索引是派生数据，这种瞬时失败可以接受：调用方重试
    /// 一次，或直接重建即可。
    pub fn load(root: &Path) -> Result<Option<Self>, MemoryError> {
        let path = index_path(root);
        match fs::read(&path) {
            Ok(bytes) => Self::decode(&bytes).map(Some),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// `<root>/.ziyuan/memory/index.json`。
pub fn index_path(root: &Path) -> PathBuf {
    memory_dir(root).join(INDEX_FILE)
}

fn memory_dir(root: &Path) -> PathBuf {
    MEMORY_DIR
        .iter()
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

/// 按身份排序：`(project, chapter, block, paragraph)`，全同则按 `workspace_id` 兜底，
/// 再全同则按记录自身的规范编码兜底。前者保证同一组记录无论输入顺序都编出同一串字节；
/// 后者让排序成为**全序**——将来 `Identity` 若加了字段而这里没跟上，也不会退化成
/// 「同身份记录按输入顺序排列」（`sort_by` 是稳定排序，稳定正是问题：它对「谁先谁后」
/// 不吭声，输入顺序就这么渗进了输出字节）。
fn sorted_records(records: &[MemoryRecord]) -> Result<Vec<MemoryRecord>, MemoryError> {
    // 每条记录的规范编码只算一次：既是兜底排序键，也与摘要用的是同一串字节。
    let mut keyed: Vec<(Vec<u8>, &MemoryRecord)> = Vec::with_capacity(records.len());
    for record in records {
        keyed.push((record_bytes(record)?, record));
    }
    keyed.sort_by(|(left_bytes, left), (right_bytes, right)| {
        sort_key(&left.identity)
            .cmp(&sort_key(&right.identity))
            .then_with(|| left_bytes.cmp(right_bytes))
    });
    Ok(keyed
        .into_iter()
        .map(|(_, record)| record.clone())
        .collect())
}

fn sort_key(identity: &Identity) -> (&str, &str, &str, &str, &str) {
    (
        &identity.project,
        &identity.chapter,
        &identity.block,
        &identity.paragraph,
        &identity.workspace_id,
    )
}

/// 一条记录的紧凑编码。
fn record_bytes(record: &MemoryRecord) -> Result<Vec<u8>, MemoryError> {
    serde_json::to_vec(record).map_err(|e| MemoryError::Corrupt(format!("记录无法序列化: {e}")))
}

/// 同一身份只能有一条记录：重新定稿是**替换**现行版本，不是并排堆两条。
///
/// 少了这一关，两条同身份不同内容的记录会被稳定排序按输入顺序编出去，输入顺序一变字节就变，
/// 而旧版本还能与新的那条共存——检索时命中哪一条全看排序位置。
fn require_unique_identities(records: &[MemoryRecord]) -> Result<(), MemoryError> {
    let mut seen = std::collections::BTreeSet::new();
    for record in records {
        if !seen.insert(sort_key(&record.identity)) {
            return Err(MemoryError::Invalid(format!(
                "同一段落只能有一条记录: {}",
                record_label(&record.identity)
            )));
        }
    }
    Ok(())
}

/// 记录的规范编码：先排序，再 `serde_json::to_vec`。`integrity.sha256` 就是这个摘要。
fn canonical_record_bytes(records: &[MemoryRecord]) -> Result<Vec<u8>, MemoryError> {
    serde_json::to_vec(&sorted_records(records)?)
        .map_err(|e| MemoryError::Corrupt(format!("记录无法序列化: {e}")))
}

/// 记录的身份标签：只含 ID，不含原文、译文或路径。
fn record_label(identity: &Identity) -> String {
    format!(
        "{}/{}/{}/{}",
        identity.project, identity.chapter, identity.block, identity.paragraph
    )
}

/// 把记录级的错误包成整份索引的 [`MemoryError::Corrupt`]。
///
/// 逐项列出而不是留一个 `other => other.clone()` 的兜底：新增错误种类时编译器会在这里
/// 提醒「想清楚它该不该被包成损坏」，而不是默默透传过去、让调用方以为记录坏了。
fn corrupt_record(label: &str, what: &str, cause: &MemoryError) -> MemoryError {
    match cause {
        MemoryError::Invalid(reason) => {
            MemoryError::Corrupt(format!("记录 {label} {what}: {reason}"))
        }
        // 记录级校验只报 `Invalid`；其余种类说明这个包装器用错了地方。原样透传，
        // 让真正的种类（配置、IO、新版本、签名不符、资料漂移、待收尾）不被伪装成损坏。
        other @ (MemoryError::Unavailable
        | MemoryError::Config(_)
        | MemoryError::Corrupt(_)
        | MemoryError::Newer { .. }
        | MemoryError::SignatureMismatch { .. }
        | MemoryError::Stale { .. }
        | MemoryError::RecoveryPending { .. }
        | MemoryError::Io { .. }) => other.clone(),
    }
}

/// 写普通文件：先写同目录临时文件、刷盘，再改名覆盖，最后刷目录项。失败时删掉临时文件。
///
/// 刷目录项是**尽力而为**：此时新内容已经就位，报错只会让调用方以为没写成。对索引这种
/// 可重建的派生数据，断电后丢了目录项也不过是 `load` 读不到（`Ok(None)`）后重建一次；
/// 因此这里不返回目录刷盘的错误，比照 `ziyuan-storage` 的 `json_file::sync_dir`。
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), MemoryError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().ok_or_else(|| MemoryError::Io {
        reason: "索引路径没有上级目录".into(),
    })?;
    let name = path.file_name().ok_or_else(|| MemoryError::Io {
        reason: "索引路径没有文件名".into(),
    })?;
    let (tmp, mut file) = loop {
        let mut tmp_name = std::ffi::OsString::from(".");
        tmp_name.push(name);
        tmp_name.push(format!(
            ".{}.{}.tmp",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let tmp = dir.join(tmp_name);
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(file) => break (tmp, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    };
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    match written.and_then(|()| fs::rename(&tmp, path)) {
        Ok(()) => {
            sync_dir(dir);
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e.into())
        }
    }
}

/// 改名后刷目录项，使替换在断电后也持久。只是尽力而为，见 [`write_atomic`]。
/// Windows 的 std 打不开目录句柄，跳过。
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(dir) = File::open(dir) {
        let _ = dir.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// 记忆库目录的排他锁：读改写期间不让另一个进程同时写。模式照 `translation::DirLock`。
struct DirLock {
    _file: File,
}

impl DirLock {
    /// 锁 `dir/.lock`；锁文件本身留在目录里（无内容）。
    fn acquire(dir: &Path) -> Result<Self, MemoryError> {
        let path = dir.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        let started = std::time::Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(fs::TryLockError::WouldBlock) => {}
                Err(fs::TryLockError::Error(e)) => return Err(e.into()),
            }
            let waited = started.elapsed();
            if waited >= LOCK_TIMEOUT {
                // 消息不带路径：本 crate 的消息只含固定原因、字段名与版本号，
                // 绝对路径既会泄露目录布局，也不便贴进问题单。
                return Err(MemoryError::Io {
                    reason: format!(
                        "记忆库被另一个紫垣进程占用超过 {} 秒；请稍后重试",
                        LOCK_TIMEOUT.as_secs()
                    ),
                });
            }
            std::thread::sleep(LOCK_POLL.min(LOCK_TIMEOUT - waited));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::EmbeddingProvider as _;
    use crate::local::LocalProvider;
    use crate::record::{
        Digests, EmbeddingSignature, Origin, Text, workspace_id_from_canonical_text,
    };

    fn signature() -> ModelSignature {
        LocalProvider::new().signature().clone()
    }

    fn watermark() -> Watermark {
        Watermark {
            events: 12,
            history_sha256: sha256_hex(b"history"),
            history_bytes: 4096,
            generation: 8,
        }
    }

    fn record(project: &str, paragraph: &str) -> MemoryRecord {
        let signature = signature();
        MemoryRecord {
            identity: Identity {
                workspace_id: workspace_id_from_canonical_text("D:/novels/demo").unwrap(),
                project: project.into(),
                chapter: "c0001".into(),
                block: "c0001-b0001".into(),
                paragraph: paragraph.into(),
            },
            source_language: "ja".into(),
            target_language: "zh".into(),
            content_type: "paragraph".into(),
            source: Text::new(format!("原文 {project} {paragraph}")),
            target: Text::new(format!("译文 {project} {paragraph}")),
            digests: Digests {
                block_source_sha256: sha256_hex(b"block-source"),
                block_target_sha256: sha256_hex(b"block-target"),
            },
            origin: Origin {
                finalized_events: vec![3],
                translation_hash: sha256_hex(b"zh.json"),
                context_version: 2,
                zhupi_version: 1,
            },
            embedding: EmbeddingSignature {
                provider: signature.provider,
                model: signature.model,
                revision: signature.revision,
                dim: signature.dim,
                metric: signature.metric,
                input_sha256: sha256_hex(b"input"),
            },
            watermark: watermark(),
        }
    }

    fn index() -> MemoryIndex {
        MemoryIndex {
            format_version: FORMAT_VERSION,
            signature: signature(),
            watermark: watermark(),
            records: vec![
                record("p0001", "c0001-p0001"),
                record("p0001", "c0001-p0002"),
            ],
        }
    }

    fn good_integrity(records: &[MemoryRecord]) -> Integrity {
        Integrity {
            count: records.len(),
            sha256: sha256_hex(&canonical_record_bytes(records).unwrap()),
        }
    }

    /// 按任意字段拼一份信封字节：用来造「摘要自洽但内容不对」的索引。
    fn envelope_bytes(
        format_version: u64,
        signature: &ModelSignature,
        watermark: &Watermark,
        records: &[MemoryRecord],
        integrity: Integrity,
    ) -> Vec<u8> {
        serde_json::to_vec(&Envelope {
            format_version,
            signature,
            watermark,
            records,
            integrity,
        })
        .unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ziyuan-memory-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn index_round_trips_through_encode_and_decode() {
        let index = index();
        let bytes = index.encode().unwrap();
        let decoded = MemoryIndex::decode(&bytes).unwrap();
        assert_eq!(decoded, index);
        // 末尾换行与美观排版是仓库惯例，便于人读与 diff。
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert!(
            bytes.starts_with(b"{\n"),
            "{}",
            String::from_utf8_lossy(&bytes)
        );
    }

    #[test]
    fn decode_rejects_a_newer_format_version() {
        let index = index();
        let bytes = envelope_bytes(
            FORMAT_VERSION + 1,
            &index.signature,
            &index.watermark,
            &index.records,
            good_integrity(&index.records),
        );
        assert_eq!(
            MemoryIndex::decode(&bytes),
            Err(MemoryError::Newer {
                found: FORMAT_VERSION + 1,
                supported: FORMAT_VERSION
            })
        );
    }

    #[test]
    fn decode_rejects_integrity_count_mismatch() {
        let index = index();
        let mut integrity = good_integrity(&index.records);
        integrity.count += 1;
        let bytes = envelope_bytes(
            FORMAT_VERSION,
            &index.signature,
            &index.watermark,
            &index.records,
            integrity,
        );
        match MemoryIndex::decode(&bytes) {
            Err(MemoryError::Corrupt(reason)) => assert!(reason.contains("count"), "{reason}"),
            other => panic!("预期条数不符，得到 {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_integrity_digest_mismatch() {
        let index = index();
        let mut records = index.records.clone();
        // 摘要算好之后再改一条记录的内容：字节变了，摘要却还是旧的。
        let integrity = good_integrity(&records);
        records[0].source.text.push('。');
        let bytes = envelope_bytes(
            FORMAT_VERSION,
            &index.signature,
            &index.watermark,
            &records,
            integrity,
        );
        match MemoryIndex::decode(&bytes) {
            Err(MemoryError::Corrupt(reason)) => {
                assert!(reason.contains("integrity.sha256"), "{reason}");
            }
            other => panic!("预期摘要不符，得到 {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_a_record_whose_signature_differs_from_the_envelope() {
        let index = index();
        let mut records = index.records.clone();
        records[1].embedding.provider = "remote".into();
        let bytes = envelope_bytes(
            FORMAT_VERSION,
            &index.signature,
            &index.watermark,
            &records,
            good_integrity(&records),
        );
        match MemoryIndex::decode(&bytes) {
            Err(MemoryError::Corrupt(reason)) => {
                // 须点名是哪一条记录，且不回显任何正文。
                assert!(
                    reason.contains("p0001/c0001/c0001-b0001/c0001-p0002"),
                    "{reason}"
                );
                assert!(reason.contains("嵌入签名"), "{reason}");
                assert!(!reason.contains("原文"), "{reason}");
            }
            other => panic!("预期签名不符，得到 {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_a_record_whose_watermark_differs_from_the_envelope() {
        let index = index();
        let mut records = index.records.clone();
        records[0].watermark.generation += 2;
        let bytes = envelope_bytes(
            FORMAT_VERSION,
            &index.signature,
            &index.watermark,
            &records,
            good_integrity(&records),
        );
        match MemoryIndex::decode(&bytes) {
            Err(MemoryError::Corrupt(reason)) => {
                assert!(reason.contains("c0001-p0001"), "{reason}");
                assert!(reason.contains("水位"), "{reason}");
            }
            other => panic!("预期水位不符，得到 {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_a_record_failing_validation() {
        let index = index();
        let mut records = index.records.clone();
        records[0].embedding.dim = 0;
        let bytes = envelope_bytes(
            FORMAT_VERSION,
            &index.signature,
            &index.watermark,
            &records,
            good_integrity(&records),
        );
        match MemoryIndex::decode(&bytes) {
            Err(MemoryError::Corrupt(reason)) => {
                assert!(reason.contains("格式不合规"), "{reason}");
                assert!(reason.contains("embedding.dim"), "{reason}");
            }
            other => panic!("预期记录不合格式，得到 {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_an_envelope_signature_without_a_model() {
        let index = index();
        let mut signature = index.signature.clone();
        signature.dim = 0;
        let bytes = envelope_bytes(
            FORMAT_VERSION,
            &signature,
            &index.watermark,
            &[],
            Integrity {
                count: 0,
                sha256: sha256_hex(&canonical_record_bytes(&[]).unwrap()),
            },
        );
        match MemoryIndex::decode(&bytes) {
            Err(MemoryError::Corrupt(reason)) => assert!(reason.contains("dim"), "{reason}"),
            other => panic!("预期信封签名不合法，得到 {other:?}"),
        }
    }

    /// 排序是格式的一部分：乱序交进来，编出的字节按身份升序，解码保持该顺序。
    #[test]
    fn encode_sorts_records_by_identity() {
        let shuffled = MemoryIndex {
            records: vec![
                record("p0003", "c0001-p0001"),
                record("p0001", "c0001-p0002"),
                record("p0002", "c0001-p0001"),
                record("p0001", "c0001-p0001"),
            ],
            ..index()
        };
        let sorted = MemoryIndex {
            records: vec![
                record("p0001", "c0001-p0001"),
                record("p0001", "c0001-p0002"),
                record("p0002", "c0001-p0001"),
                record("p0003", "c0001-p0001"),
            ],
            ..index()
        };
        // 与交入顺序无关：两串字节完全相同。
        assert_eq!(shuffled.encode().unwrap(), sorted.encode().unwrap());

        let value: serde_json::Value = serde_json::from_slice(&shuffled.encode().unwrap()).unwrap();
        let projects: Vec<&str> = value["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| record["identity"]["project"].as_str().unwrap())
            .collect();
        assert_eq!(projects, ["p0001", "p0001", "p0002", "p0003"]);

        let decoded = MemoryIndex::decode(&shuffled.encode().unwrap()).unwrap();
        let ids: Vec<(&str, &str)> = decoded
            .records
            .iter()
            .map(|record| {
                (
                    record.identity.project.as_str(),
                    record.identity.paragraph.as_str(),
                )
            })
            .collect();
        assert_eq!(
            ids,
            [
                ("p0001", "c0001-p0001"),
                ("p0001", "c0001-p0002"),
                ("p0002", "c0001-p0001"),
                ("p0003", "c0001-p0001"),
            ]
        );
    }

    #[test]
    fn load_returns_none_when_absent() {
        let dir = temp_dir("absent");
        assert_eq!(MemoryIndex::load(&dir).unwrap(), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn publish_then_load_round_trips() {
        let dir = temp_dir("publish");
        let index = index();
        let path = index.publish(&dir).unwrap();
        assert_eq!(path, index_path(&dir));
        assert!(path.ends_with(Path::new(".ziyuan").join("memory").join("index.json")));
        assert_eq!(MemoryIndex::load(&dir).unwrap(), Some(index));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn publish_leaves_no_temp_files() {
        let dir = temp_dir("no-temp");
        let index = index();
        let path = index.publish(&dir).unwrap();
        // 再发布一次：覆盖成功，也不该留临时文件。
        index.publish(&dir).unwrap();
        let mut names: Vec<String> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, [LOCK_FILE.to_string(), INDEX_FILE.to_string()]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_file_on_disk_makes_load_error_rather_than_reset() {
        let dir = temp_dir("corrupt");
        let path = index_path(&dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let garbage = b"{\"format_version\": 1, \"records\": [";
        fs::write(&path, garbage).unwrap();
        match MemoryIndex::load(&dir) {
            Err(MemoryError::Corrupt(reason)) => assert!(reason.contains("无法解码"), "{reason}"),
            other => panic!("预期损坏报错，得到 {other:?}"),
        }
        // 绝不「重置为空后假装最新」：文件原样留在原处。
        assert_eq!(fs::read(&path).unwrap(), garbage);
        fs::remove_dir_all(dir).unwrap();
    }

    /// 摘要算在记录数组的紧凑编码上，与信封的美观排版无关。
    #[test]
    fn integrity_digest_covers_the_compact_record_array() {
        let index = index();
        let text = String::from_utf8(index.encode().unwrap()).unwrap();
        let expected = sha256_hex(&canonical_record_bytes(&index.records).unwrap());
        assert!(
            text.contains(&format!("\"sha256\": \"{expected}\"")),
            "{text}"
        );
        assert!(text.contains(&format!("\"count\": {}", index.records.len())));
    }

    /// 能够真正考验「顺序」这一关的只有一种文件：记录乱序，但 `integrity.sha256`
    /// 按规范顺序重算过。这样摘要对得上，只剩顺序本身可查。
    ///
    /// 摘要算在排序后的字节上，所以它**看不见**顺序；把顺序核对删掉，这条用例就会
    /// 读到一份乱序的对象而不是报损坏。
    #[test]
    fn decode_rejects_records_out_of_canonical_order_even_when_the_digest_matches() {
        let index = index();
        let mut records = index.records.clone();
        records.reverse();
        // 摘要仍按排序后的字节算：文件乱序，摘要却自洽。
        let bytes = envelope_bytes(
            FORMAT_VERSION,
            &index.signature,
            &index.watermark,
            &records,
            good_integrity(&records),
        );
        match MemoryIndex::decode(&bytes) {
            Err(MemoryError::Corrupt(reason)) => {
                assert!(reason.contains("未按规范顺序排列"), "{reason}");
            }
            other => panic!("预期乱序被拒，得到 {other:?}"),
        }
    }

    /// 乱序且摘要按「没排序的字节」算过：这一份死在摘要上，报的是摘要不符而不是顺序。
    ///
    /// 摘要若按规范顺序重算就与顺序无关了（见上一条用例），所以这里刻意用错误写法
    /// ——直接哈希文件里那串乱序数组——来造一份「摘要自洽但内容不对」的文件。
    #[test]
    fn decode_rejects_a_shuffle_with_a_stale_digest() {
        let index = index();
        let mut records = index.records.clone();
        records.reverse();
        let naive = Integrity {
            count: records.len(),
            sha256: sha256_hex(&serde_json::to_vec(&records).unwrap()),
        };
        let bytes = envelope_bytes(
            FORMAT_VERSION,
            &index.signature,
            &index.watermark,
            &records,
            naive,
        );
        match MemoryIndex::decode(&bytes) {
            Err(MemoryError::Corrupt(reason)) => {
                assert!(reason.contains("integrity.sha256"), "{reason}");
            }
            other => panic!("预期摘要不符，得到 {other:?}"),
        }
    }

    /// 同一身份两条记录：内容不同，靠稳定排序就会按输入顺序编出去。重新定稿是**换掉**
    /// 现行版本，所以这种输入本身就是错的，编码阶段就该拒收。
    #[test]
    fn encode_rejects_duplicate_identities() {
        let mut restated = record("p0001", "c0001-p0001");
        restated.target = Text::new("重新定稿后的另一版译文");
        let duplicate = MemoryIndex {
            records: vec![record("p0001", "c0001-p0001"), restated],
            ..index()
        };
        match duplicate.encode() {
            Err(MemoryError::Invalid(reason)) => {
                assert!(reason.contains("同一段落只能有一条"), "{reason}");
                // 点名是哪一段，且不回显任何正文。
                assert!(
                    reason.contains("p0001/c0001/c0001-b0001/c0001-p0001"),
                    "{reason}"
                );
                assert!(!reason.contains("译文"), "{reason}");
            }
            other => panic!("预期重复身份被拒，得到 {other:?}"),
        }
    }

    /// 格式 0 从来不存在：显式拒收，而不是让它顺着「不比 1 新」被放进来。
    #[test]
    fn decode_rejects_format_version_zero() {
        let index = MemoryIndex {
            records: Vec::new(),
            ..index()
        };
        let bytes = envelope_bytes(
            0,
            &index.signature,
            &index.watermark,
            &[],
            good_integrity(&[]),
        );
        match MemoryIndex::decode(&bytes) {
            Err(MemoryError::Corrupt(reason)) => {
                assert!(reason.contains("索引格式 0 不受支持"), "{reason}");
            }
            other => panic!("预期格式 0 被拒，得到 {other:?}"),
        }
    }

    /// 空索引是合法的：签名有效、`records: []`、`count: 0`。刚建库、还没有任何定稿时就是这个形态，
    /// 不能把它当成损坏。
    #[test]
    fn decode_accepts_a_valid_empty_index() {
        let index = MemoryIndex {
            records: Vec::new(),
            ..index()
        };
        let bytes = index.encode().unwrap();
        let decoded = MemoryIndex::decode(&bytes).unwrap();
        assert_eq!(decoded, index);
        assert!(decoded.records.is_empty());
    }

    /// 字节同一性：解码再编码必须逐字节相同。这条钉住「读出来的对象就是规范形态」——
    /// 任何一处顺序、排版或字段序列化不唯一，都会在这里露出来。
    ///
    /// **注意**：本用例隐含「`MemoryRecord` 里没有浮点字段」。`serde_json` 对浮点的往返
    /// 不保证逐字节相同（`1.0` 与 `1`、精度截断、`-0.0` 都是坑）。Task 5 若要给
    /// `MemoryRecord` 加浮点字段，必须先重审这条用例，而不是把它改成 `unwrap` 了就算。
    /// 向量不属于记录（向量由提供方现算），因此暂时没有这个风险。
    #[test]
    fn encode_is_byte_identical_after_a_decode_round_trip() {
        let index = MemoryIndex {
            records: vec![
                record("p0002", "c0001-p0001"),
                record("p0001", "c0001-p0002"),
                record("p0001", "c0001-p0001"),
            ],
            ..index()
        };
        let first = index.encode().unwrap();
        let second = MemoryIndex::decode(&first).unwrap().encode().unwrap();
        assert_eq!(first, second);
        // 解码出来的对象是规范顺序；与交入顺序无关，编码结果仍是同一串字节。
        assert_eq!(
            MemoryIndex::decode(&second).unwrap().encode().unwrap(),
            first
        );
    }

    /// 覆盖一份已经损坏的 `index.json` 是**约定的行为**：索引是派生数据，
    /// 重建就该把坏文件整个换掉，而不是让 `status` 永远卡在「损坏」。
    #[test]
    fn publish_over_a_corrupt_index_overwrites_it() {
        let dir = temp_dir("overwrite-corrupt");
        let path = index_path(&dir);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"{\"format_version\": 1, \"records\": [").unwrap();
        assert!(MemoryIndex::load(&dir).is_err());

        let index = index();
        index.publish(&dir).unwrap();
        assert_eq!(MemoryIndex::load(&dir).unwrap(), Some(index));
        fs::remove_dir_all(dir).unwrap();
    }

    /// 编码失败（记录不合规）时不该留下一个空的 `memory/`：那会让人以为「建过，只是空的」。
    #[test]
    fn publish_creates_no_directory_when_encoding_fails() {
        let dir = temp_dir("no-dir-on-failure");
        let mut broken = record("p0001", "c0001-p0001");
        broken.source = Text::new("");
        let index = MemoryIndex {
            records: vec![broken],
            ..index()
        };
        assert!(index.publish(&dir).is_err());
        assert!(!memory_dir(&dir).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    /// 水位一动没动：照常发布，且读取器确实被调用了一次。
    #[test]
    fn publish_checked_publishes_when_the_watermark_did_not_move() {
        let dir = temp_dir("publish-checked-ok");
        let index = index();
        let calls = std::cell::Cell::new(0);
        let expected = index.watermark.clone();
        let reader = || {
            calls.set(calls.get() + 1);
            Ok(expected.clone())
        };
        let path = index.publish_checked(&dir, &reader).unwrap();
        assert_eq!(path, index_path(&dir));
        assert_eq!(calls.get(), 1, "注入的读取器必须真的被调用");
        assert_eq!(MemoryIndex::load(&dir).unwrap(), Some(index));
        fs::remove_dir_all(dir).unwrap();
    }

    /// 构建开始时记的是 12 条事件，发布前重读变成 13 条：`Stale`，
    /// 且磁盘上原有的索引一字不动。
    #[test]
    fn publish_checked_refuses_when_events_drifted_and_leaves_the_old_index() {
        let dir = temp_dir("publish-checked-events");
        let old = index();
        old.publish(&dir).unwrap();
        let before = fs::read(index_path(&dir)).unwrap();

        // 建库期间事件簿又多了事件：索引里记的仍是 12。
        let mut moved = old.watermark.clone();
        moved.events += 1;
        let reader = || Ok(moved.clone());

        let newer = MemoryIndex {
            records: vec![record("p0009", "c0001-p0009")],
            ..index()
        };
        match newer.publish_checked(&dir, &reader) {
            Err(MemoryError::Stale { reason }) => {
                assert!(reason.contains("events 12→13"), "{reason}");
            }
            other => panic!("预期资料漂移被拒，得到 {other:?}"),
        }
        assert_eq!(
            fs::read(index_path(&dir)).unwrap(),
            before,
            "旧索引不该被动过"
        );
        assert_eq!(MemoryIndex::load(&dir).unwrap(), Some(old));
        fs::remove_dir_all(dir).unwrap();
    }

    /// 事件条数与字节数都没变、只有历史摘要变了（同一位置换了内容）：同样 `Stale`。
    #[test]
    fn publish_checked_refuses_when_the_history_digest_drifted() {
        let dir = temp_dir("publish-checked-digest");
        let index = index();
        let mut drifted = index.watermark.clone();
        drifted.history_sha256 = sha256_hex(b"rewritten history");
        let reader = || Ok(drifted.clone());

        match index.publish_checked(&dir, &reader) {
            Err(MemoryError::Stale { reason }) => {
                assert!(reason.contains("history_sha256 由 "), "{reason}");
                assert!(
                    !reason.contains(&index.watermark.history_sha256),
                    "{reason}"
                );
            }
            other => panic!("预期摘要漂移被拒，得到 {other:?}"),
        }
        assert!(!index_path(&dir).exists(), "拒绝发布就该一个字节都不写");
        fs::remove_dir_all(dir).unwrap();
    }

    /// 两端任一端有未收尾的受管写入：报 `RecoveryPending` 而不是 `Stale`，
    /// 并带上世代号，好让人直接去 `text recovery status` 对着看。
    #[test]
    fn publish_checked_reports_unfinished_writes_at_either_end() {
        let dir = temp_dir("publish-checked-generation");

        let mut odd_before = MemoryIndex {
            records: vec![record("p0001", "c0001-p0001")],
            ..index()
        };
        odd_before.watermark.generation = 9;
        for record in &mut odd_before.records {
            record.watermark = odd_before.watermark.clone();
        }
        let steady = odd_before.watermark.clone();
        let reader = || Ok(steady.clone());
        assert_eq!(
            odd_before.publish_checked(&dir, &reader),
            Err(MemoryError::RecoveryPending { generation: 9 })
        );

        let even = MemoryIndex {
            records: vec![record("p0001", "c0001-p0001")],
            ..index()
        };
        let mut odd_after = even.watermark.clone();
        odd_after.generation = 11;
        let reader = || Ok(odd_after.clone());
        assert_eq!(
            even.publish_checked(&dir, &reader),
            Err(MemoryError::RecoveryPending { generation: 11 })
        );

        assert!(!index_path(&dir).exists(), "拒绝发布就该一个字节都不写");
        fs::remove_dir_all(dir).unwrap();
    }

    /// 消息里不能出现路径。逐个走**真实的构造点**，而不是拿手写的字符串比对：
    /// 锁超时一支要真等到超时，`From<io::Error>` 一支要真去碰一个不存在的路径。
    #[test]
    fn lock_timeout_and_io_errors_name_no_path() {
        fn assert_no_separators(message: &str) {
            assert!(!message.contains('/'), "{message}");
            assert!(!message.contains('\\'), "{message}");
        }

        let dir = temp_dir("no-path");
        // 一支：同目录再进一次，必须等满超时才失败（Windows 与 unix 的锁都是按句柄冲突）。
        let held = DirLock::acquire(&dir).unwrap();
        match DirLock::acquire(&dir) {
            Err(MemoryError::Io { reason }) => {
                assert_no_separators(&reason);
                assert!(reason.contains("被另一个紫垣进程占用"), "{reason}");
                assert!(
                    reason.contains(&LOCK_TIMEOUT.as_secs().to_string()),
                    "{reason}"
                );
            }
            Ok(_) => panic!("预期锁超时，却在另一支还持有锁时又拿到了锁"),
            Err(other) => panic!("预期锁超时，得到错误 {other}"),
        }
        drop(held);

        // 另一支：`From<io::Error>`——读一个不存在的目录下的索引文件。
        let missing =
            MemoryError::from(fs::read(dir.join("missing-dir").join(INDEX_FILE)).unwrap_err());
        assert!(matches!(missing, MemoryError::Io { .. }), "{missing:?}");
        assert_no_separators(&missing.to_string());
        fs::remove_dir_all(dir).unwrap();
    }
}
