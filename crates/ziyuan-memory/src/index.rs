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
//! 兜底）。排序**是格式的一部分**：同一组记录无论以什么顺序交进来，都必须编出同一串字节；
//! 因此解码也按同一规范重算摘要——顺序不对的文件摘要对不上，按损坏拒收，而不是被悄悄接受。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::MemoryError;
use crate::embed::ModelSignature;
use crate::record::{Identity, MemoryRecord, Watermark, sha256_hex};

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
    /// 编成索引文件字节：外层美观排版并带末尾换行（仓库惯例），记录数组紧凑。
    ///
    /// 记录在这里排序，摘要按排序后的字节算。记录或信封签名不合法即拒绝编码——
    /// 宁可编不出来，也不写出会被自己拒收的文件。
    pub fn encode(&self) -> Result<Vec<u8>, MemoryError> {
        self.signature.validate()?;
        for record in &self.records {
            record.validate_format()?;
        }
        let records = sorted_records(&self.records);
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
    pub fn decode(bytes: &[u8]) -> Result<Self, MemoryError> {
        let raw: RawEnvelope = serde_json::from_slice(bytes)
            .map_err(|e| MemoryError::Corrupt(format!("无法解码: {e}")))?;
        if raw.format_version > FORMAT_VERSION {
            return Err(MemoryError::Newer {
                found: raw.format_version,
                supported: FORMAT_VERSION,
            });
        }
        if let Err(MemoryError::Invalid(reason)) = raw.signature.validate() {
            return Err(MemoryError::Corrupt(format!("信封签名不合规: {reason}")));
        }
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
    /// 目录不存在就建；写入走 [`DirLock`]（`<root>/.ziyuan/memory/.lock`，超时 5 秒），
    /// 再「同目录临时文件 → 刷盘 → 改名」。绝不写到 `.ziyuan/memory/` 之外。
    pub fn publish(&self, root: &Path) -> Result<PathBuf, MemoryError> {
        let dir = memory_dir(root);
        fs::create_dir_all(&dir)?;
        let bytes = self.encode()?;
        let path = dir.join(INDEX_FILE);
        let _lock = DirLock::acquire(&dir)?;
        write_atomic(&path, &bytes)?;
        Ok(path)
    }

    /// 读 `<root>/.ziyuan/memory/index.json`；不存在即 `Ok(None)`。
    ///
    /// 存在但损坏是**错误**，绝不是 `None`：把损坏当成「还没建过」就等于悄悄重置。
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
/// 保证同一组记录无论输入顺序都编出同一串字节。
fn sorted_records(records: &[MemoryRecord]) -> Vec<MemoryRecord> {
    let mut sorted = records.to_vec();
    sorted.sort_by(|left, right| sort_key(&left.identity).cmp(&sort_key(&right.identity)));
    sorted
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

/// 记录的规范编码：先排序，再 `serde_json::to_vec`。`integrity.sha256` 就是这个摘要。
fn canonical_record_bytes(records: &[MemoryRecord]) -> Result<Vec<u8>, MemoryError> {
    serde_json::to_vec(&sorted_records(records))
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
fn corrupt_record(label: &str, what: &str, cause: &MemoryError) -> MemoryError {
    match cause {
        MemoryError::Invalid(reason) => {
            MemoryError::Corrupt(format!("记录 {label} {what}: {reason}"))
        }
        other => other.clone(),
    }
}

/// 写普通文件：先写同目录临时文件、刷盘，再改名覆盖。失败时删掉临时文件。
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), MemoryError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = path
        .parent()
        .ok_or_else(|| MemoryError::Invalid("索引路径没有上级目录".into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| MemoryError::Invalid("索引路径没有文件名".into()))?;
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
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e.into())
        }
    }
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
                return Err(MemoryError::Io {
                    reason: format!(
                        "{} 被占用超过 {} 秒",
                        path.display(),
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
}
