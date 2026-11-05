//! 恢复记录：应用改动前写下的、带校验和的独立记录，供崩溃后核对项目文件。
//!
//! 记录只描述一次应用尝试的参照（任务、事件、目标文件前后哈希），不是已交付事务：
//! 写入方须自行处理落盘顺序与项目内的真实内容，本模块只负责编解码与格式校验。

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ziyuan_core::{CourtEvent, TranslationSource};

/// 本版默认写入的恢复记录格式版本。
pub const RECOVERY_FORMAT: u64 = 1;
/// 第二版恢复记录：显式区分模型与人工来源。本版只读，不默认写入。
pub const RECOVERY_FORMAT_V2: u64 = 2;
/// 恢复记录的字节上限（8 MiB）。
pub const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;

/// 记录所处的阶段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordState {
    Prepared,
    Published,
    Completed,
    Abandoned,
}

/// 单个文件的存在状态；存在的文件记下其内容哈希。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    content = "sha256",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum FileState {
    Absent,
    Present(String),
}

/// 一处目标文件：相对工作区根的路径，应用前后的状态。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetState {
    pub path: String,
    pub before: FileState,
    pub after: FileState,
}

/// 来源：此次应用对应的事件簿位置。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationSource {
    pub task: u64,
    pub changes_version: String,
    pub skipped: Vec<u32>,
}

/// 第二版模型来源：字段与第一版相同，但必须显式带 `kind: model`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelApplication {
    pub task: u64,
    pub changes_version: String,
    pub skipped: Vec<u32>,
}

/// 第二版人工来源。准奏操作身份以记录顶层的 `operation_id` 为唯一值。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanApplication {
    pub project: String,
    pub block: String,
    pub approval_request_id: String,
    pub approval_request_hash: String,
    pub previous: TranslationSource,
    pub target_format: String,
    pub logical_path: String,
    pub before: Option<String>,
    pub after: String,
}

/// 第二版来源分支：不猜、不许字段跨分支。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationV2 {
    Model(ModelApplication),
    Human(HumanApplication),
}

/// 第二版恢复记录：外壳与第一版相同，来源改为显式分支。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRecordV2 {
    pub operation_id: String,
    pub workspace: PathBuf,
    pub generation_before: u64,
    pub generation_after: u64,
    pub state: RecordState,
    pub application: ApplicationV2,
    pub events: EventBatch,
    pub targets: Vec<TargetState>,
}

/// 解码结果：调用方按来源分派，第一版含义不变。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodedRecovery {
    V1(RecoveryRecord),
    V2(RecoveryRecordV2),
}

/// 记录内直接保存的事件行，免去重建时另读事件簿。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventBatch {
    pub expected_events: u64,
    pub expected_bytes: u64,
    pub history_sha256: String,
    pub encoded_events: Vec<String>,
}

/// 一份恢复记录。
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

/// 恢复记录的编解码错误。只为固定的原因或版本号，不含记录内容。
#[derive(Debug)]
pub enum RecoveryError {
    /// 超过上限 `limit` 字节。
    TooLarge { limit: usize },
    /// 由更新版本的紫垣写入，本版只支持 `supported`。
    Newer { found: u64, supported: u64 },
    /// 格式或内容不合法。
    Corrupt(String),
}
impl fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { limit } => {
                write!(f, "恢复记录超过上限 {limit} 字节")
            }
            Self::Newer { found, supported } => write!(
                f,
                "恢复记录格式 {found} 由更新版本的紫垣写入，本版只支持 {supported}，请升级 ziyuan 后再打开"
            ),
            Self::Corrupt(reason) => write!(f, "恢复记录损坏: {reason}"),
        }
    }
}
impl std::error::Error for RecoveryError {}

/// 编码一份恢复记录：先校验内容，再整体加上格式版本与校验和。
pub fn encode_record(record: &RecoveryRecord) -> Result<Vec<u8>, RecoveryError> {
    validate_record(record)?;
    let record_bytes = to_bytes(record)?;
    let envelope = EnvelopeRef::<RecoveryRecord> {
        format_version: RECOVERY_FORMAT,
        sha256: &checksum(&record_bytes),
        record,
    };
    let bytes = to_bytes(&envelope)?;
    match bytes.len() > MAX_RECORD_BYTES {
        true => Err(RecoveryError::TooLarge {
            limit: MAX_RECORD_BYTES,
        }),
        false => Ok(bytes),
    }
}

/// 编码第二版恢复记录。本版命令行不写入第二版，也没有人工准奏调用点；
/// 导出它只为构造和核对协议样本，不改变第一版默认写入。
pub fn encode_record_v2(record: &RecoveryRecordV2) -> Result<Vec<u8>, RecoveryError> {
    validate_record_v2(record)?;
    let record_bytes = to_bytes(record)?;
    let envelope = EnvelopeRef::<RecoveryRecordV2> {
        format_version: RECOVERY_FORMAT_V2,
        sha256: &checksum(&record_bytes),
        record,
    };
    let bytes = to_bytes(&envelope)?;
    match bytes.len() > MAX_RECORD_BYTES {
        true => Err(RecoveryError::TooLarge {
            limit: MAX_RECORD_BYTES,
        }),
        false => Ok(bytes),
    }
}

/// 解码一份恢复记录：先看大小，再看格式版本，最后校验和与内容。
///
/// 校验和只表明字节未被改动，不是身份认证；字段顺序与空白可以重排，
/// 校验的是记录本身的固定序列化。
pub fn decode_record(bytes: &[u8]) -> Result<RecoveryRecord, RecoveryError> {
    let version = peek_version(bytes)?;
    if version != RECOVERY_FORMAT {
        return Err(RecoveryError::Newer {
            found: version,
            supported: RECOVERY_FORMAT,
        });
    }
    let record = decode_envelope::<RecoveryRecord>(bytes, version)?;
    validate_record(&record)?;
    Ok(record)
}

/// 第二版恢复记录也按完整信封读取；第一版默认写入路径不变。
pub fn decode_record_any(bytes: &[u8]) -> Result<DecodedRecovery, RecoveryError> {
    let version = peek_version(bytes)?;
    match version {
        RECOVERY_FORMAT => {
            let record = decode_envelope::<RecoveryRecord>(bytes, version)?;
            validate_record(&record)?;
            Ok(DecodedRecovery::V1(record))
        }
        RECOVERY_FORMAT_V2 => {
            let record = decode_envelope::<RecoveryRecordV2>(bytes, version)?;
            validate_record_v2(&record)?;
            Ok(DecodedRecovery::V2(record))
        }
        found => Err(RecoveryError::Newer {
            found,
            supported: RECOVERY_FORMAT_V2,
        }),
    }
}

/// 只看大小与版本号：其余字段不认识也能认出「由更新版本写入」。
fn peek_version(bytes: &[u8]) -> Result<u64, RecoveryError> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(RecoveryError::TooLarge {
            limit: MAX_RECORD_BYTES,
        });
    }
    let header: Header =
        serde_json::from_slice(bytes).map_err(|_| corrupt("不是合法的 JSON 对象"))?;
    if header.format_version == 0 {
        return Err(corrupt("不支持的格式版本"));
    }
    Ok(header.format_version)
}

/// 解析外壳，并按记录类型固定字段序核对校验和。
fn decode_envelope<T>(bytes: &[u8], version: u64) -> Result<T, RecoveryError>
where
    T: serde::de::DeserializeOwned + Serialize,
{
    // 外壳、记录及嵌套来源均严格解析，未知字段直接拒绝，
    // 不能先丢掉来源标签再用旧布局通过校验和。
    let envelope: Envelope<T> =
        serde_json::from_slice(bytes).map_err(|_| corrupt("字段缺失或含有未知字段"))?;
    if envelope.format_version != version {
        return Err(corrupt("格式版本不一致"));
    }
    if !envelope.sha256.starts_with("sha256:") || envelope.sha256.len() != 7 + 64 {
        return Err(corrupt("校验和格式不合法"));
    }
    let digest = envelope
        .sha256
        .get(7..)
        .ok_or_else(|| corrupt("校验和格式不合法"))?;
    if !is_sha256(digest) {
        return Err(corrupt("校验和格式不合法"));
    }
    // 按记录类型固定字段序重新序列化，与原文的空白、字段次序无关。
    let record_bytes = to_bytes(&envelope.record)?;
    if !checksum_matches(&record_bytes, &digest_bytes(digest)) {
        return Err(corrupt("校验和不符"));
    }
    Ok(envelope.record)
}

fn checksum_matches(bytes: &[u8], expected: &[u8]) -> bool {
    // 上限只有 8 MiB，一次算完；先算再比，避免分块拼接的边界错误。
    digest(bytes).as_ref() == expected
}

/// 固定布局的校验和外壳；`sha256` 覆盖 `record` 按 `RecoveryRecord` 字段序序列化的字节。
/// 编码时借入记录（不另存副本），解码时取回记录。
#[derive(Serialize)]
struct EnvelopeRef<'a, T> {
    format_version: u64,
    sha256: &'a str,
    record: &'a T,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, bound(deserialize = "T: Deserialize<'de>"))]
struct Envelope<T> {
    /// 只作严格解析的字段之一：版本已由 [`Header`] 判定，这里再核对它存在、不重复且一致。
    format_version: u64,
    sha256: String,
    record: T,
}

/// 只读版本号的头部：旧版记录即便其余字段不认识，也能认出「由更新版本写入」。
#[derive(Deserialize)]
struct Header {
    format_version: u64,
}

fn corrupt(reason: &str) -> RecoveryError {
    RecoveryError::Corrupt(reason.into())
}

/// 以 8 MiB 为上限序列化：写出过程中越界即停，不产出超限的缓冲。
fn to_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, RecoveryError> {
    to_bounded(value, MAX_RECORD_BYTES)
}

fn to_bounded<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>, RecoveryError> {
    let mut out = BoundedWriter {
        buf: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut out, value).map_err(|e| {
        if e.io_error_kind() == Some(std::io::ErrorKind::FileTooLarge) {
            RecoveryError::TooLarge { limit }
        } else {
            corrupt("无法序列化为 JSON")
        }
    })?;
    if out.buf.len() > limit {
        return Err(RecoveryError::TooLarge { limit });
    }
    Ok(out.buf)
}

/// 写满 `limit` 即拒绝继续写，并丢弃已写的部分。
struct BoundedWriter {
    buf: Vec<u8>,
    limit: usize,
}
impl std::io::Write for BoundedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.buf.len().saturating_add(buf.len()) > self.limit {
            self.buf.clear();
            self.buf.shrink_to(0);
            return Err(std::io::ErrorKind::FileTooLarge.into());
        }
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `sha256:` 加 64 位小写十六进制。
fn checksum(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    push_hex(&mut out, digest(bytes).as_ref());
    out
}

fn digest(bytes: &[u8]) -> ring::digest::Digest {
    ring::digest::digest(&ring::digest::SHA256, bytes)
}

fn push_hex(out: &mut String, bytes: &[u8]) {
    const HEX: &[u8] = b"0123456789abcdef";
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 15) as usize] as char);
    }
}

/// 把 64 位小写十六进制读成字节。
fn digest_bytes(hex: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    for pair in hex.as_bytes().as_chunks::<2>().0 {
        let nibble = |b: u8| match b {
            b'0'..=b'9' => b - b'0',
            _ => b - b'a' + 10,
        };
        out.push(nibble(pair[0]) << 4 | nibble(pair[1]));
    }
    out
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(is_lower_hex)
}

fn is_operation_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(is_lower_hex)
}

fn is_lower_hex(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

/// 第二版解码后的内容校验；分支字段不得混用，人工批次的日志位置仍由调用方核对。
fn validate_record_v2(record: &RecoveryRecordV2) -> Result<(), RecoveryError> {
    match &record.application {
        ApplicationV2::Model(app) => {
            validate_bounds(&record.events, &record.targets, app.skipped.len())?;
            validate_common(
                &record.operation_id,
                &record.workspace,
                record.generation_before,
                record.generation_after,
                &record.events,
                &record.targets,
            )?;
            validate_model_application(app.task, &app.changes_version, &app.skipped, &record.events)
        }
        ApplicationV2::Human(app) => {
            validate_bounds(&record.events, &record.targets, 0)?;
            validate_common(
                &record.operation_id,
                &record.workspace,
                record.generation_before,
                record.generation_after,
                &record.events,
                &record.targets,
            )?;
            validate_human_application(app)?;
            validate_human_events(&record.events, app, &record.operation_id)?;
            validate_human_targets(app, &record.targets)
        }
    }
}

/// 现场判据：只描述事件簿在记录批次附近的样子，不判断该收尾还是继续。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneEvidence {
    /// 前缀完整，批次一个字节也没追加。
    CleanAbsent,
    /// 前缀完整，批次正好从固定位置开始（其后可有其他事件）。
    ExactAtSlot,
    /// 批次只写了一半，保留现场。
    Partial,
    /// 前缀、条数或批次位置对不上。
    Diverged,
}

/// 核对记录声明的批次窗口。调用方须读入至少
/// `expected_bytes + 批次字节数 + 1` 字节；窗口短于前缀即视为分歧。
pub fn classify_scene(
    record: &RecoveryRecordV2,
    window: &[u8],
) -> Result<SceneEvidence, RecoveryError> {
    let events = &record.events;
    let batch_len = events
        .encoded_events
        .iter()
        .try_fold(0usize, |sum, line| sum.checked_add(line.len() + 1));
    let batch_len = batch_len.ok_or_else(|| corrupt("事件批过大"))?;
    let prefix_events = events
        .expected_events
        .checked_sub(events.encoded_events.len() as u64)
        .ok_or_else(|| corrupt("事件批条数超过预期"))?;
    let prefix_bytes = usize::try_from(events.expected_bytes).map_err(|_| corrupt("前缀过长"))?;
    if window.len() < prefix_bytes {
        return Ok(SceneEvidence::Diverged);
    }
    let prefix = &window[..prefix_bytes];
    if !checksum_matches(prefix, &digest_bytes(&events.history_sha256))
        || prefix.last().is_some_and(|b| *b != b'\n')
        || prefix.iter().filter(|b| **b == b'\n').count() as u64 != prefix_events
    {
        return Ok(SceneEvidence::Diverged);
    }
    let rest = &window[prefix_bytes..];
    if rest.is_empty() {
        return Ok(SceneEvidence::CleanAbsent);
    }
    let mut batch = Vec::with_capacity(batch_len);
    for line in &events.encoded_events {
        batch.extend_from_slice(line.as_bytes());
        batch.push(b'\n');
    }
    if rest.starts_with(&batch) {
        Ok(SceneEvidence::ExactAtSlot)
    } else if batch.starts_with(rest) {
        Ok(SceneEvidence::Partial)
    } else {
        Ok(SceneEvidence::Diverged)
    }
}

/// 校验记录内容；只返回固定的原因，不回显记录里的值。
///
/// 解析到真实文件的各种别名与权限须由写入方在读取工作区根之后另行核对，
/// 本函数只保证记录本身自洽。
fn validate_record(record: &RecoveryRecord) -> Result<(), RecoveryError> {
    validate_bounds(
        &record.events,
        &record.targets,
        record.application.skipped.len(),
    )?;
    validate_common(
        &record.operation_id,
        &record.workspace,
        record.generation_before,
        record.generation_after,
        &record.events,
        &record.targets,
    )?;
    validate_model_application(
        record.application.task,
        &record.application.changes_version,
        &record.application.skipped,
        &record.events,
    )
}

fn validate_common(
    operation_id: &str,
    workspace: &std::path::Path,
    generation_before: u64,
    generation_after: u64,
    events: &EventBatch,
    targets: &[TargetState],
) -> Result<(), RecoveryError> {
    if !is_operation_id(operation_id) {
        return Err(corrupt("操作编号须为 32 位小写十六进制"));
    }
    if !is_absolute(workspace) {
        return Err(corrupt("工作区须为绝对路径"));
    }
    if !is_sha256(&events.history_sha256) {
        return Err(corrupt("事件簿校验和不合法"));
    }
    let even_and_next = generation_before.is_multiple_of(2)
        && generation_before.checked_add(2) == Some(generation_after);
    if !even_and_next {
        return Err(corrupt("代次须为偶数且前进 2"));
    }
    validate_targets(targets)
}

/// 第一版与第二版的 model 分支共用：真实任务、旧改动版本字面量与事件批。
fn validate_model_application(
    task: u64,
    changes_version: &str,
    skipped: &[u32],
    events: &EventBatch,
) -> Result<(), RecoveryError> {
    if task == 0 {
        return Err(corrupt("任务编号不得为 0"));
    }
    if !matches!(changes_version, "v1") {
        return Err(corrupt("改动版本不合法"));
    }
    let mut sorted = skipped.to_vec();
    sorted.sort_unstable();
    if sorted.windows(2).any(|w| w[0] == w[1]) {
        return Err(corrupt("驳回段号重复"));
    }
    validate_model_events(events, task, skipped)
}

/// 先看规模再细查：8 MiB 的字节里装不下这么多事件或目标，故按字段声明的上限
/// 预分配不会放大内存；超出者一律损坏。
fn validate_bounds(
    events: &EventBatch,
    targets: &[TargetState],
    skipped_len: usize,
) -> Result<(), RecoveryError> {
    const MAX_EVENTS: usize = 1_048_576;
    if events.encoded_events.len() > MAX_EVENTS {
        return Err(corrupt("事件过多"));
    }
    if targets.len() > (MAX_RECORD_BYTES / 5) * 2 {
        return Err(corrupt("目标过多"));
    }
    if skipped_len > MAX_RECORD_BYTES / 2 {
        return Err(corrupt("驳回段号过多"));
    }
    Ok(())
}

/// 绝对路径：以 `/` 或盘符 `X:/`、`X:\` 开头。按原始字节判断，不做有损转换，
/// 也不触碰文件系统（真实根由调用方另行比对）。
fn is_absolute(path: &std::path::Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    match bytes.first() {
        Some(b'/') => bytes.len() > 1,
        Some(first) if first.is_ascii_alphabetic() => {
            matches!(bytes.get(1..3), Some([b':', b'/' | b'\\']))
        }
        _ => false,
    }
}

fn validate_targets(targets: &[TargetState]) -> Result<(), RecoveryError> {
    let mut seen = std::collections::HashSet::with_capacity(targets.len());
    for target in targets {
        if !is_portable_relative(&target.path) {
            return Err(corrupt("目标路径须为安全的相对路径"));
        }
        if !seen.insert(target.path.as_str()) {
            return Err(corrupt("目标路径重复"));
        }
        for state in [&target.before, &target.after] {
            if matches!(state, FileState::Present(hash) if !is_sha256(hash)) {
                return Err(corrupt("目标哈希不合法"));
            }
        }
        // 本版只支持新建与改写：删除（存在变为不存在）留待后续版本。
        if target.before == target.after {
            return Err(corrupt("目标前后状态须不同"));
        }
        if target.after == FileState::Absent {
            return Err(corrupt("目标须由存在变为存在"));
        }
    }
    Ok(())
}

/// 以 `/` 分隔的相对路径：无空段、`.`、`..`，无反斜杠、盘符冒号与 NUL。
/// 按字节判断，全角斜杠之类不会在平台上被当作分隔符，故照常接受。
fn is_portable_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.bytes().any(|b| matches!(b, b'\\' | b':' | 0))
        && path
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | ".."))
}

fn validate_model_events(
    events: &EventBatch,
    task: u64,
    skipped: &[u32],
) -> Result<(), RecoveryError> {
    if events.encoded_events.is_empty() {
        return Err(corrupt("恢复记录须含事件"));
    }
    let mut applied_seen = false;
    let mut clocks = 0usize;
    for line in &events.encoded_events {
        if line.contains(['\r', '\n']) {
            return Err(corrupt("事件行不得含换行"));
        }
        let event = crate::codec::decode_event(line).map_err(|_| corrupt("事件无法解码"))?;
        match event {
            CourtEvent::Clock(_) => {
                if applied_seen {
                    return Err(corrupt("时辰须在应用事件之前"));
                }
                clocks += 1;
            }
            CourtEvent::ChangesApplied {
                task: applied_task,
                skipped: applied_skipped,
            } => {
                let matches_source = applied_task.0 == task && applied_skipped == skipped;
                if !matches_source || applied_seen {
                    return Err(corrupt("须恰有与来源一致的应用事件"));
                }
                applied_seen = true;
            }
            _ => return Err(corrupt("事件只可为时辰或应用事件")),
        }
    }
    if clocks > 1 {
        return Err(corrupt("时辰至多一条"));
    }
    if !applied_seen {
        return Err(corrupt("须恰有与来源一致的应用事件"));
    }
    Ok(())
}

/// 人工来源的形状；准奏事件、绑定前缀与盘上字节由调用方另行核对。
fn validate_human_application(app: &HumanApplication) -> Result<(), RecoveryError> {
    if !is_operation_id(&app.project) {
        return Err(corrupt("项目 ID 不合法"));
    }
    if !is_block_id(&app.block) {
        return Err(corrupt("块 ID 不合法"));
    }
    if !is_request_id(&app.approval_request_id) {
        return Err(corrupt("准奏请求标识不合法"));
    }
    if !is_sha256(&app.approval_request_hash) {
        return Err(corrupt("准奏请求摘要不合法"));
    }
    validate_translation_source(&app.previous)?;
    if !is_target_format(&app.target_format) {
        return Err(corrupt("目标格式不合法"));
    }
    let expected = format!("translations/{}/{}/zh.json", app.project, app.block);
    if app.logical_path != expected {
        return Err(corrupt("目标路径不合法"));
    }
    if let Some(before) = &app.before
        && !is_sha256(before)
    {
        return Err(corrupt("准奏前目标摘要不合法"));
    }
    if !is_sha256(&app.after) {
        return Err(corrupt("准奏后目标摘要不合法"));
    }
    Ok(())
}

fn validate_translation_source(source: &TranslationSource) -> Result<(), RecoveryError> {
    match source {
        TranslationSource::None {} => Ok(()),
        TranslationSource::Model {
            task,
            changes_version,
            event,
        } => {
            if task.0 == 0 {
                return Err(corrupt("模型来源须为真实诏令"));
            }
            let digest = changes_version
                .strip_prefix("sha256:")
                .ok_or_else(|| corrupt("模型改动版本须沿用 sha256: 前缀"))?;
            if !is_sha256(digest) || !is_sha256(&event.hash) {
                return Err(corrupt("模型来源摘要不合法"));
            }
            Ok(())
        }
        TranslationSource::Human {
            operation_id,
            event,
        } => {
            if !is_operation_id(operation_id) || !is_sha256(&event.hash) {
                return Err(corrupt("人工来源摘要不合法"));
            }
            Ok(())
        }
    }
}

/// 人工批次只能是一条前置 Clock 与唯一一条准奏事件，且必须与来源字段一致；
/// 所引用的送审绑定必须落在本笔事务追加前的固定前缀里。
fn validate_human_events(
    events: &EventBatch,
    app: &HumanApplication,
    operation_id: &str,
) -> Result<(), RecoveryError> {
    let prefix_events = events
        .expected_events
        .checked_sub(events.encoded_events.len() as u64)
        .ok_or_else(|| corrupt("事件批条数超过预期"))?;
    let mut clock_seen = false;
    let mut approved_seen = false;
    for line in &events.encoded_events {
        if line.contains(['\r', '\n']) {
            return Err(corrupt("事件行不得含换行"));
        }
        let event = crate::codec::decode_event(line).map_err(|_| corrupt("事件无法解码"))?;
        match event {
            CourtEvent::Clock(_) if !clock_seen && !approved_seen => clock_seen = true,
            CourtEvent::Clock(_) => return Err(corrupt("时辰至多一条且须在前")),
            CourtEvent::HumanTranslationApproved { approval } if !approved_seen => {
                let request = &approval.request;
                let matches_source = approval.operation_id == operation_id
                    && request.project == app.project
                    && request.block == app.block
                    && request.request_id == app.approval_request_id
                    && approval.request_hash == app.approval_request_hash
                    && request.previous == app.previous
                    && request.target.format == app.target_format
                    && request.target.path == app.logical_path
                    && request.target.before == app.before
                    && request.target.after == app.after;
                if !matches_source {
                    return Err(corrupt("人工准奏事件与来源不一致"));
                }
                if request.binding.index >= prefix_events {
                    return Err(corrupt("送审绑定须位于本笔事务的前缀之内"));
                }
                approved_seen = true;
            }
            _ => return Err(corrupt("事件只可为时辰或人工准奏事件")),
        }
    }
    if !approved_seen {
        return Err(corrupt("须恰有一条人工准奏事件"));
    }
    Ok(())
}

/// 人工准奏至多一个物理目标，且必须与该块逻辑输出一致；
/// 零目标只表示同字节批准，逻辑输出的前后摘要必须相同。
fn validate_human_targets(
    app: &HumanApplication,
    targets: &[TargetState],
) -> Result<(), RecoveryError> {
    if targets.len() > 1 {
        return Err(corrupt("人工准奏至多一个目标"));
    }
    if targets.is_empty() && app.before.as_deref() != Some(app.after.as_str()) {
        return Err(corrupt("零目标只用于同字节批准：准奏前后摘要须相同"));
    }
    if let Some(target) = targets.first() {
        if target.path != app.logical_path {
            return Err(corrupt("人工目标路径与来源不符"));
        }
        let before_matches = match (&target.before, &app.before) {
            (FileState::Absent, None) => true,
            (FileState::Present(hash), Some(expected)) => hash == expected,
            _ => false,
        };
        if !before_matches {
            return Err(corrupt("人工目标前状与来源不符"));
        }
        if target.after != FileState::Present(app.after.clone()) {
            return Err(corrupt("人工目标后状与来源不符"));
        }
    }
    Ok(())
}

fn is_block_id(block: &str) -> bool {
    let Some((chapter, index)) = block
        .strip_prefix('c')
        .and_then(|rest| rest.split_once("-b"))
    else {
        return false;
    };
    let digits =
        |value: &str| (4..=9).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit());
    digits(chapter) && digits(index)
}

fn is_request_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn is_target_format(format: &str) -> bool {
    (1..=64).contains(&format.len())
        && format
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'/' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ziyuan_core::{CourtEvent, TaskId};

    const WORKSPACE: &str = r"D:\projects\ziyuan";
    const CREATED_SHA: &str = "3b1f0f4d2c5a6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7";
    const REWRITTEN_BEFORE_SHA: &str =
        "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
    const REWRITTEN_AFTER_SHA: &str =
        "5a4b3c2d1e0f9a8b7c6d5e4f3a2b1c0d5a4b3c2d1e0f9a8b7c6d5e4f3a2b1c0d";
    const HISTORY_SHA: &str = "9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0";
    const CHANGES_VERSION: &str = "v1";
    // 由独立的 Python json.dumps（固定字段序、紧凑分隔）与 hashlib.sha256 核对，
    // 不是调用待测编码器生成；文件行尾不属于恢复信封的编码字节。
    const FIRST_FORMAT_GOLDEN: &str = include_str!("recovery-first-format.json");

    fn fixture() -> RecoveryRecord {
        let applied = CourtEvent::ChangesApplied {
            task: TaskId(7),
            skipped: Vec::new(),
        };
        RecoveryRecord {
            operation_id: "a1b2c3d4e5f60718293a4b5c6d7e8f90".into(),
            workspace: PathBuf::from(WORKSPACE),
            generation_before: 4,
            generation_after: 6,
            state: RecordState::Prepared,
            application: ApplicationSource {
                task: 7,
                changes_version: CHANGES_VERSION.into(),
                skipped: Vec::new(),
            },
            events: EventBatch {
                expected_events: 12,
                expected_bytes: 4096,
                history_sha256: HISTORY_SHA.into(),
                encoded_events: vec![
                    crate::codec::encode_event(&CourtEvent::Clock(1_700_000_000)),
                    crate::codec::encode_event(&applied),
                ],
            },
            targets: vec![
                TargetState {
                    path: "docs/notes.md".into(),
                    before: FileState::Absent,
                    after: FileState::Present(CREATED_SHA.into()),
                },
                TargetState {
                    path: "src/lib.rs".into(),
                    before: FileState::Present(REWRITTEN_BEFORE_SHA.into()),
                    after: FileState::Present(REWRITTEN_AFTER_SHA.into()),
                },
            ],
        }
    }

    #[test]
    fn fixture_record_round_trips() {
        let record = fixture();
        let bytes = encode_record(&record).expect("编码");
        assert!(bytes.len() <= MAX_RECORD_BYTES);
        let text = String::from_utf8(bytes.clone()).expect("UTF-8");
        assert!(text.contains("\"format_version\":1"), "{text}");
        assert_eq!(decode_record(&bytes).expect("解码"), record);
    }

    #[test]
    fn first_format_matches_independent_golden() {
        let mut expected = fixture();
        expected.application.skipped = vec![2, 9];
        expected.events.encoded_events[1] = "v1\tAPPLIED\t7\t2,9".into();
        let golden = FIRST_FORMAT_GOLDEN
            .trim_end_matches(['\r', '\n'])
            .as_bytes();
        assert_eq!(decode_record(golden).expect("读取第一版定本"), expected);
        assert_eq!(encode_record(&expected).expect("编码第一版"), golden);
    }

    #[test]
    fn every_state_round_trips() {
        for state in [
            RecordState::Prepared,
            RecordState::Published,
            RecordState::Completed,
            RecordState::Abandoned,
        ] {
            let mut record = fixture();
            record.state = state;
            let bytes = encode_record(&record).expect("编码");
            assert_eq!(decode_record(&bytes).expect("解码"), record);
        }
    }

    #[test]
    fn record_with_every_hunk_skipped_has_no_targets() {
        let mut record = fixture();
        record.targets.clear();
        record.application.skipped = vec![1, 2, 3];
        record.events.encoded_events =
            vec![crate::codec::encode_event(&CourtEvent::ChangesApplied {
                task: TaskId(7),
                skipped: vec![1, 2, 3],
            })];
        let bytes = encode_record(&record).expect("编码");
        assert_eq!(decode_record(&bytes).expect("解码"), record);
    }

    #[test]
    fn whitespace_reformat_keeps_checksum_valid() {
        let record = fixture();
        let bytes = encode_record(&record).expect("编码");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        let pretty = serde_json::to_vec_pretty(&value).expect("格式化");
        assert_ne!(pretty, bytes);
        assert_eq!(decode_record(&pretty).expect("解码"), record);
    }

    fn corrupt_reason(result: Result<RecoveryRecord, RecoveryError>) -> String {
        match result {
            Err(RecoveryError::Corrupt(reason)) => reason,
            other => panic!("期望 Corrupt，却是 {other:?}"),
        }
    }

    fn assert_rejected(record: &RecoveryRecord, reason: &str) {
        match encode_record(record) {
            Ok(_) => panic!("不应通过校验: {reason}"),
            Err(RecoveryError::Corrupt(found)) => assert_eq!(found, reason),
            Err(other) => panic!("期望 Corrupt，却是 {other}"),
        }
    }

    #[test]
    fn tampered_digest_is_rejected() {
        let bytes = encode_record(&fixture()).expect("编码");
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        value["sha256"] = serde_json::json!(format!("sha256:{}", "0".repeat(64)));
        let tampered = serde_json::to_vec(&value).expect("JSON");
        assert_eq!(corrupt_reason(decode_record(&tampered)), "校验和不符");
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let bytes = encode_record(&fixture()).expect("编码");
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        value["record"]["application"]["task"] = serde_json::json!(8);
        let tampered = serde_json::to_vec(&value).expect("JSON");
        assert_eq!(corrupt_reason(decode_record(&tampered)), "校验和不符");
    }

    #[test]
    fn digest_formats_are_rejected() {
        let bytes = encode_record(&fixture()).expect("编码");
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        let good: String = serde_json::from_value(value["sha256"].clone()).expect("字符串");
        let digest = good.trim_start_matches("sha256:");
        for bad in [
            digest.to_owned(),
            format!("sha256:{}", digest.to_uppercase()),
            format!("sha256:{}", &digest[..63]),
            format!("md5:{}", digest[..32].to_owned()),
        ] {
            value["sha256"] = serde_json::json!(bad);
            let tampered = serde_json::to_vec(&value).expect("JSON");
            assert!(matches!(
                decode_record(&tampered),
                Err(RecoveryError::Corrupt(_))
            ));
        }
    }

    #[test]
    fn malformed_json_is_rejected() {
        for bad in [
            &b""[..],
            &b"not json"[..],
            &b"[]"[..],
            &b"null"[..],
            &b"{"[..],
        ] {
            assert!(matches!(decode_record(bad), Err(RecoveryError::Corrupt(_))));
        }
    }

    #[test]
    fn unknown_fields_and_states_are_rejected() {
        let bytes = encode_record(&fixture()).expect("编码");
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        value["extra"] = serde_json::json!(1);
        let extra = serde_json::to_vec(&value).expect("JSON");
        assert!(matches!(
            decode_record(&extra),
            Err(RecoveryError::Corrupt(_))
        ));

        let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        value["record"]["state"] = serde_json::json!("rolled_back");
        let unknown_state = serde_json::to_vec(&value).expect("JSON");
        assert!(matches!(
            decode_record(&unknown_state),
            Err(RecoveryError::Corrupt(_))
        ));

        let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        value["record"]["targets"][0]["note"] = serde_json::json!("x");
        let extra_target = serde_json::to_vec(&value).expect("JSON");
        assert!(matches!(
            decode_record(&extra_target),
            Err(RecoveryError::Corrupt(_))
        ));
    }

    #[test]
    fn first_format_does_not_discard_source_tags() {
        for kind in ["model", "human"] {
            let mut value: serde_json::Value =
                serde_json::from_str(FIRST_FORMAT_GOLDEN).expect("定本 JSON");
            value["record"]["application"]["kind"] = serde_json::json!(kind);
            // 保留旧校验和：若错误地丢掉未知 kind，旧记录的校验和仍会匹配，
            // 因而这里必须由严格的来源字段解析拒绝，而非碰巧因摘要不同失败。
            let bytes = serde_json::to_vec(&value).expect("JSON");
            assert!(matches!(
                decode_record(&bytes),
                Err(RecoveryError::Corrupt(_))
            ));
        }
    }

    #[test]
    fn source_validation_is_required_even_with_a_matching_checksum() {
        for invalid_task in [true, false] {
            let mut record = fixture();
            if invalid_task {
                record.application.task = 0;
                record.events.encoded_events[1] = "v1\tAPPLIED\t0".into();
            } else {
                // 恢复协议版本不是模型提案的内容摘要，不能互换。
                record.application.changes_version = format!("sha256:{HISTORY_SHA}");
            }
            // 绕过编码器的预检，构造摘要确实自洽、只有来源语义不合法的输入。
            let payload = serde_json::to_vec(&record).expect("记录 JSON");
            let value = serde_json::json!({
                "format_version": 1,
                "sha256": format!("sha256:{}", crate::content_sha256(&payload)),
                "record": record,
            });
            let bytes = serde_json::to_vec(&value).expect("信封 JSON");
            assert!(matches!(
                decode_record(&bytes),
                Err(RecoveryError::Corrupt(_))
            ));
        }
    }

    #[test]
    fn malformed_version_headers_are_not_guessed() {
        for header in [
            "",
            r#""format_version":null,"#,
            r#""format_version":"1","#,
            r#""format_version":1.0,"#,
            r#""format_version":-1,"#,
            r#""format_version":true,"#,
            r#""format_version":1,"format_version":2,"#,
            r#""format_version":2,"format_version":1,"#,
        ] {
            // 其余内容全部有效，避免因缺正文而让错误的版本推断也通过测试。
            let bytes = FIRST_FORMAT_GOLDEN.replacen(r#""format_version":1,"#, header, 1);
            assert!(matches!(
                decode_record(bytes.as_bytes()),
                Err(RecoveryError::Corrupt(_))
            ));
        }
    }

    fn version_only_record(version: u64) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({ "format_version": version })).expect("JSON")
    }

    #[test]
    fn unsupported_versions_are_told_apart() {
        for bytes in [
            version_only_record(2),
            serde_json::to_vec(&serde_json::json!({
                "format_version": 2,
                "sha256": "future",
                "record": { "unknown_layout": true }
            }))
            .expect("JSON"),
        ] {
            match decode_record(&bytes) {
                Err(RecoveryError::Newer { found, supported }) => {
                    assert_eq!((found, supported), (2, RECOVERY_FORMAT));
                }
                other => panic!("期望 Newer，却是 {other:?}"),
            }
        }
        for version in [0, RECOVERY_FORMAT] {
            assert!(matches!(
                decode_record(&version_only_record(version)),
                Err(RecoveryError::Corrupt(_))
            ));
        }
        // 即使后半段是完整可读的旧模型布局，也不能忽略新格式号后回退读取。
        for version in [2, 3] {
            let mut value: serde_json::Value =
                serde_json::from_str(FIRST_FORMAT_GOLDEN).expect("定本 JSON");
            value["format_version"] = serde_json::json!(version);
            let bytes = serde_json::to_vec(&value).expect("JSON");
            assert!(matches!(
                decode_record(&bytes),
                Err(RecoveryError::Newer { found, supported: 1 }) if found == version
            ));
        }
    }

    fn more_targets_than_limit_allows(record: &mut RecoveryRecord) {
        // 每个目标约 1 KiB 有效路径；一万个足以越过 8 MiB。
        for i in 0..10_000u32 {
            record.targets.push(TargetState {
                path: format!("docs/{i:05}-{}.md", "a".repeat(1000)),
                before: FileState::Absent,
                after: FileState::Present(CREATED_SHA.into()),
            });
        }
    }

    #[test]
    fn oversized_valid_record_is_rejected_without_encoding() {
        let mut record = fixture();
        more_targets_than_limit_allows(&mut record);
        match encode_record(&record) {
            Err(RecoveryError::TooLarge { limit }) => assert_eq!(limit, MAX_RECORD_BYTES),
            Ok(bytes) => panic!("不应编码成功，却得到 {} 字节", bytes.len()),
            Err(other) => panic!("期望 TooLarge，却是 {other}"),
        }
    }

    #[test]
    fn double_ended_record_is_rejected() {
        let bytes = encode_record(&fixture()).expect("编码");
        let mut extra = bytes.clone();
        extra.extend_from_slice(b"{}");
        assert!(matches!(
            decode_record(&extra),
            Err(RecoveryError::Corrupt(_))
        ));
    }

    #[test]
    fn bounded_serialization_stops_at_the_limit() {
        // 直接压小上限，验证「写出过程中越界即停」不依赖 8 MiB 的大样本。
        let record = fixture();
        let full = serde_json::to_vec(&record).expect("JSON");
        assert!(to_bounded(&record, full.len()).is_ok());
        for limit in [0, 1, full.len() - 1] {
            match to_bounded(&record, limit) {
                Err(RecoveryError::TooLarge { limit: found }) => assert_eq!(found, limit),
                other => panic!("上限 {limit} 时应为 TooLarge，却是 {other:?}"),
            }
        }
    }

    #[test]
    fn oversized_input_is_rejected_first() {
        let bytes = encode_record(&fixture()).expect("编码");
        let mut oversized = bytes.clone();
        oversized.resize(MAX_RECORD_BYTES + 1, b' ');
        match decode_record(&oversized) {
            Err(RecoveryError::TooLarge { limit }) => assert_eq!(limit, MAX_RECORD_BYTES),
            other => panic!("期望 TooLarge，却是 {other:?}"),
        }
        // 即便前面是「更新版本」，超出上限也先报 TooLarge。
        let mut newer = version_only_record(9);
        newer.resize(MAX_RECORD_BYTES + 1, b' ');
        assert!(matches!(
            decode_record(&newer),
            Err(RecoveryError::TooLarge { .. })
        ));
    }

    #[test]
    fn operation_id_must_be_32_lowercase_hex() {
        let mut record = fixture();
        for bad in [
            "A1B2C3D4E5F60718293A4B5C6D7E8F90",
            "a1b2c3d4e5f60718293a4b5c6d7e8f9",
            "a1b2c3d4e5f60718293a4b5c6d7e8f9g",
            "a1b2c3d4-e5f6-0718-293a-4b5c6d7e8f90",
        ] {
            record.operation_id = bad.into();
            assert_rejected(&record, "操作编号须为 32 位小写十六进制");
        }
        record.operation_id = "a1b2c3d4e5f60718293a4b5c6d7e8f90".into();
        assert!(encode_record(&record).is_ok());
    }

    #[test]
    fn workspace_must_be_absolute() {
        let mut record = fixture();
        for bad in ["projects\\ziyuan", "", "."] {
            record.workspace = PathBuf::from(bad);
            assert_rejected(&record, "工作区须为绝对路径");
        }
        record.workspace = PathBuf::from("/tmp/ziyuan");
        assert!(encode_record(&record).is_ok());
    }

    #[test]
    fn application_task_must_be_nonzero() {
        let mut record = fixture();
        record.application.task = 0;
        assert_rejected(&record, "任务编号不得为 0");
    }

    #[test]
    fn application_hashes_must_be_valid() {
        let mut record = fixture();
        record.application.changes_version = String::new();
        assert_rejected(&record, "改动版本不合法");
        record.application.changes_version = CHANGES_VERSION.into();
        record.events.history_sha256 = "not-a-hash".into();
        assert_rejected(&record, "事件簿校验和不合法");
        record.events.history_sha256 = HISTORY_SHA.to_uppercase();
        assert_rejected(&record, "事件簿校验和不合法");
    }

    #[test]
    fn generation_must_advance_by_two() {
        let mut record = fixture();
        record.generation_after = 5;
        assert_rejected(&record, "代次须为偶数且前进 2");
        record.generation_before = 5;
        record.generation_after = 7;
        assert_rejected(&record, "代次须为偶数且前进 2");
        record.generation_before = 4;
        record.generation_after = 8;
        assert_rejected(&record, "代次须为偶数且前进 2");
        record.generation_before = u64::MAX - 1;
        record.generation_after = u64::MAX;
        assert_rejected(&record, "代次须为偶数且前进 2");
        record.generation_before = 0;
        record.generation_after = 2;
        assert!(encode_record(&record).is_ok());
    }

    #[test]
    fn skipped_segments_are_unique() {
        let mut record = fixture();
        record.application.skipped = vec![2, 1, 2];
        record.events.encoded_events =
            vec![crate::codec::encode_event(&CourtEvent::ChangesApplied {
                task: TaskId(7),
                skipped: vec![2, 1, 2],
            })];
        assert_rejected(&record, "驳回段号重复");
    }

    #[test]
    fn target_paths_must_be_unique_portable_relative() {
        let mut record = fixture();
        let base = record.targets[0].clone();
        record.targets = vec![base.clone(), base.clone()];
        assert_rejected(&record, "目标路径重复");
        for bad in [
            "",
            "/abs/path",
            "dir//file",
            "dir/./file",
            "dir/../file",
            "../escape",
            "dir\\file",
            "C:/file",
            "a\0b",
            "trailing/",
        ] {
            let mut target = base.clone();
            target.path = bad.into();
            record.targets = vec![target];
            assert_rejected(&record, "目标路径须为安全的相对路径");
        }
        // 全角斜杠不是平台分隔符，不会被拆成上级目录，可作文件名的一部分。
        let mut target = base.clone();
        target.path = "docs/a\u{ff0f}b.md".into();
        record.targets = vec![target];
        assert!(encode_record(&record).is_ok());
        let mut target = base.clone();
        target.path = "目录/文件.md".into();
        record.targets = vec![target];
        assert!(encode_record(&record).is_ok());
    }

    #[test]
    fn workspace_must_be_absolute_without_lossy_text() {
        let mut record = fixture();
        for bad in ["projects", "a/b", "", ".", "C:", "C:ziyuan", "/"] {
            record.workspace = PathBuf::from(bad);
            assert_rejected(&record, "工作区须为绝对路径");
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            // 非 UTF-8 的绝对路径确实「绝对」，但记录以 JSON 存 `PathBuf`，
            // serde 要求能转成 UTF-8：这种路径**编不出记录**，而不是被有损转换成别的路径。
            // 这里钉住的是后者——宁可编码失败，也绝不悄悄改写路径。
            record.workspace = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/\xff"));
            assert!(
                encode_record(&record).is_err(),
                "非 UTF-8 路径不能靠有损转换编码成记录"
            );
        }
        for good in ["C:/ziyuan", "c:\\work\\ziyuan", "/tmp/ziyuan"] {
            record.workspace = PathBuf::from(good);
            assert!(encode_record(&record).is_ok(), "{good}");
        }
    }

    #[test]
    fn changes_version_must_be_a_known_tag() {
        let mut record = fixture();
        for bad in ["", "v", "v01", "v1.0", "1", "v2 ", "vx"] {
            record.application.changes_version = bad.into();
            assert_rejected(&record, "改动版本不合法");
        }
    }

    #[test]
    fn target_states_must_record_their_own_change() {
        let mut record = fixture();
        let mut target = record.targets[0].clone();
        target.before = FileState::Absent;
        target.after = FileState::Absent;
        record.targets = vec![target];
        assert_rejected(&record, "目标前后状态须不同");

        // 本版不做删除：存在变为不存在不被接受（留待 v1.3）。
        let mut target = record.targets[0].clone();
        target.before = FileState::Present(CREATED_SHA.into());
        target.after = FileState::Absent;
        record.targets = vec![target];
        assert_rejected(&record, "目标须由存在变为存在");

        let mut target = record.targets[0].clone();
        target.after = FileState::Present("bad".into());
        record.targets = vec![target];
        assert_rejected(&record, "目标哈希不合法");
    }

    #[test]
    fn rewritten_into_same_bytes_is_rejected() {
        let mut record = fixture();
        let mut target = record.targets[1].clone();
        target.before = FileState::Present(REWRITTEN_AFTER_SHA.into());
        record.targets[1] = target;
        assert_rejected(&record, "目标前后状态须不同");
    }

    fn single_event(record: &mut RecoveryRecord, event: CourtEvent) {
        record.events.encoded_events = vec![crate::codec::encode_event(&event)];
        record.targets.truncate(1);
    }

    #[test]
    fn event_batch_must_not_be_empty() {
        let mut record = fixture();
        record.events.encoded_events.clear();
        assert_rejected(&record, "恢复记录须含事件");
    }

    #[test]
    fn events_must_not_contain_line_breaks() {
        let mut record = fixture();
        for bad in [
            "v1\tAPPLIED\t7\n",
            "v1\tAPPLIED\t7\r",
            "v1\tAPPLIED\t7\t1\n2",
        ] {
            record.events.encoded_events = vec![bad.into()];
            assert_rejected(&record, "事件行不得含换行");
        }
    }

    #[test]
    fn events_must_decode() {
        let mut record = fixture();
        record.events.encoded_events = vec!["v1\tNOT_A_KIND".into()];
        assert_rejected(&record, "事件无法解码");
    }

    #[test]
    fn changes_applied_must_match_the_application() {
        let mut record = fixture();
        let event = CourtEvent::ChangesApplied {
            task: TaskId(7),
            skipped: vec![1],
        };
        single_event(&mut record, event);
        assert_rejected(&record, "须恰有与来源一致的应用事件");

        let event = CourtEvent::ChangesApplied {
            task: TaskId(8),
            skipped: Vec::new(),
        };
        single_event(&mut record, event);
        assert_rejected(&record, "须恰有与来源一致的应用事件");

        let event = court_event::discarded();
        single_event(&mut record, event);
        assert_rejected(&record, "事件只可为时辰或应用事件");
    }

    #[test]
    fn only_one_application_event_is_allowed() {
        let mut record = fixture();
        let event = CourtEvent::ChangesApplied {
            task: TaskId(7),
            skipped: Vec::new(),
        };
        record.events.encoded_events = vec![
            crate::codec::encode_event(&event),
            crate::codec::encode_event(&event),
        ];
        assert_rejected(&record, "须恰有与来源一致的应用事件");
    }

    #[test]
    fn misplaced_or_repeated_clock_is_rejected() {
        let applied = crate::codec::encode_event(&CourtEvent::ChangesApplied {
            task: TaskId(7),
            skipped: Vec::new(),
        });
        let at = |n| crate::codec::encode_event(&CourtEvent::Clock(n));
        for (lines, expected) in [
            (vec![applied.clone(), at(1)], "时辰须在应用事件之前"),
            (vec![at(1), applied.clone(), at(2)], "时辰须在应用事件之前"),
            (vec![at(1), at(2), applied.clone()], "时辰至多一条"),
        ] {
            let mut record = fixture();
            record.events.encoded_events = lines;
            assert_rejected(&record, expected);
        }
    }

    mod court_event {
        use super::*;
        pub fn discarded() -> CourtEvent {
            CourtEvent::ChangesDiscarded { task: TaskId(7) }
        }
    }

    #[test]
    fn only_clock_and_changes_applied_may_appear() {
        let applied = CourtEvent::ChangesApplied {
            task: TaskId(7),
            skipped: Vec::new(),
        };
        for business in [
            CourtEvent::Founded(ziyuan_core::GovernanceMode::Cabinet),
            CourtEvent::ChangesDiscarded { task: TaskId(7) },
        ] {
            let mut record = fixture();
            record.events.encoded_events = vec![
                crate::codec::encode_event(&business),
                crate::codec::encode_event(&applied),
            ];
            assert_rejected(&record, "事件只可为时辰或应用事件");
        }
    }

    #[test]
    fn clock_may_only_lead_the_application_event() {
        let applied = CourtEvent::ChangesApplied {
            task: TaskId(7),
            skipped: Vec::new(),
        };
        let clock = CourtEvent::Clock(1_700_000_000);

        let mut record = fixture();
        record.events.encoded_events = vec![crate::codec::encode_event(&applied)];
        assert!(encode_record(&record).is_ok());

        let mut record = fixture();
        record.events.encoded_events = vec![
            crate::codec::encode_event(&clock),
            crate::codec::encode_event(&applied),
        ];
        assert!(encode_record(&record).is_ok());
    }

    #[test]
    fn display_messages_are_fixed_and_leak_nothing() {
        let record = fixture();
        let mut bad = record.clone();
        bad.workspace = PathBuf::from("secret-relative-root");
        let err = encode_record(&bad).expect_err("拒绝");
        let text = err.to_string();
        assert!(text.contains("恢复记录损坏"), "{text}");
        assert!(!text.contains("secret-relative-root"), "{text}");

        let bytes = encode_record(&record).expect("编码");
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        value["record"]["operation_id"] = serde_json::json!("泄露内容");
        let tampered = serde_json::to_vec(&value).expect("JSON");
        let text = decode_record(&tampered).expect_err("拒绝").to_string();
        assert!(!text.contains("泄露内容"), "{text}");
        assert!(!text.contains("docs/notes.md"), "{text}");

        assert_eq!(
            RecoveryError::TooLarge {
                limit: MAX_RECORD_BYTES
            }
            .to_string(),
            "恢复记录超过上限 8388608 字节"
        );
        let newer = RecoveryError::Newer {
            found: 3,
            supported: RECOVERY_FORMAT,
        };
        assert!(newer.to_string().contains("请升级"), "{newer}");
        assert!(std::error::Error::source(&newer).is_none());
    }

    #[test]
    fn record_carries_no_file_text() {
        let record = fixture();
        let bytes = encode_record(&record).expect("编码");
        let text = String::from_utf8(bytes).expect("UTF-8");
        for field in ["\"before\"", "\"after\"", "\"content\"", "\"text\""] {
            let count = text.matches(field).count();
            let allowed = match field {
                // 目标的前后状态各出现一次/目标。
                "\"before\"" | "\"after\"" => record.targets.len(),
                _ => 0,
            };
            assert_eq!(count, allowed, "{field}: {text}");
        }
    }

    #[test]
    fn envelope_is_keyed_by_fixed_field_order() {
        let bytes = encode_record(&fixture()).expect("编码");
        let text = String::from_utf8(bytes).expect("UTF-8");
        let order = [
            "format_version",
            "sha256",
            "record",
            "operation_id",
            "workspace",
        ];
        let mut last = 0;
        for key in order {
            let at = text.find(&format!("\"{key}\"")).expect(key);
            assert!(at >= last, "{key} 字段顺序错乱");
            last = at;
        }
    }

    fn human_record() -> RecoveryRecordV2 {
        use ziyuan_core::{
            EventReference, HumanActor, HumanApproval, HumanApprovalRequest, HumanApprovalTarget,
        };
        let project = "0123456789abcdef0123456789abcdef";
        let block = "c0001-b0001";
        let path = format!("translations/{project}/{block}/zh.json");
        let request = HumanApprovalRequest {
            project: project.into(),
            block: block.into(),
            request_id: "approve-1".into(),
            actor: HumanActor::Local,
            submission_id: "bind-1".into(),
            // 前缀 3 条（5 − 批次 2），绑定须落在其中。
            binding: EventReference {
                index: 2,
                hash: "b".repeat(64),
            },
            previous: TranslationSource::None {},
            target: HumanApprovalTarget {
                format: "zh-json/1".into(),
                path: path.clone(),
                before: None,
                after: "f".repeat(64),
            },
        };
        let approval = HumanApproval {
            request,
            request_hash: "c".repeat(64),
            operation_id: "a1b2c3d4e5f60718293a4b5c6d7e8f90".into(),
        };
        RecoveryRecordV2 {
            operation_id: "a1b2c3d4e5f60718293a4b5c6d7e8f90".into(),
            workspace: PathBuf::from(WORKSPACE),
            generation_before: 4,
            generation_after: 6,
            state: RecordState::Published,
            application: ApplicationV2::Human(HumanApplication {
                project: project.into(),
                block: block.into(),
                approval_request_id: "approve-1".into(),
                approval_request_hash: "c".repeat(64),
                previous: TranslationSource::None {},
                target_format: "zh-json/1".into(),
                logical_path: path.clone(),
                before: None,
                after: "f".repeat(64),
            }),
            events: EventBatch {
                expected_events: 5,
                expected_bytes: 512,
                history_sha256: HISTORY_SHA.into(),
                encoded_events: vec![
                    crate::codec::encode_event(&CourtEvent::Clock(1_700_000_000)),
                    crate::codec::encode_event(&CourtEvent::HumanTranslationApproved {
                        approval: Box::new(approval),
                    }),
                ],
            },
            targets: vec![TargetState {
                path,
                before: FileState::Absent,
                after: FileState::Present("f".repeat(64)),
            }],
        }
    }

    #[test]
    fn second_format_human_round_trips_without_changing_v1_readers() {
        let record = human_record();
        let bytes = encode_record_v2(&record).expect("编码第二版");
        match decode_record_any(&bytes).expect("解码第二版") {
            DecodedRecovery::V2(found) => assert_eq!(found, record),
            other => panic!("应为第二版，却是 {other:?}"),
        }
        assert!(matches!(
            decode_record(&bytes),
            Err(RecoveryError::Newer {
                found: 2,
                supported: 1
            })
        ));
        let v1 = encode_record(&fixture()).expect("编码第一版");
        assert!(matches!(decode_record_any(&v1), Ok(DecodedRecovery::V1(_))));

        let mut future: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        future["format_version"] = 3.into();
        let future = serde_json::to_vec(&future).expect("JSON");
        assert!(matches!(
            decode_record_any(&future),
            Err(RecoveryError::Newer {
                found: 3,
                supported: 2
            })
        ));
    }

    #[test]
    fn second_format_model_branch_keeps_the_old_application_event() {
        let applied = CourtEvent::ChangesApplied {
            task: TaskId(7),
            skipped: Vec::new(),
        };
        let record = RecoveryRecordV2 {
            operation_id: "a1b2c3d4e5f60718293a4b5c6d7e8f90".into(),
            workspace: PathBuf::from(WORKSPACE),
            generation_before: 4,
            generation_after: 6,
            state: RecordState::Prepared,
            application: ApplicationV2::Model(ModelApplication {
                task: 7,
                changes_version: CHANGES_VERSION.into(),
                skipped: Vec::new(),
            }),
            events: EventBatch {
                expected_events: 12,
                expected_bytes: 4096,
                history_sha256: HISTORY_SHA.into(),
                encoded_events: vec![crate::codec::encode_event(&applied)],
            },
            targets: vec![TargetState {
                path: "docs/notes.md".into(),
                before: FileState::Absent,
                after: FileState::Present(CREATED_SHA.into()),
            }],
        };
        let bytes = encode_record_v2(&record).expect("编码第二版模型来源");
        assert_eq!(
            decode_record_any(&bytes).expect("解码"),
            DecodedRecovery::V2(record)
        );
    }

    #[test]
    fn second_format_human_rejects_mismatch_and_cross_branch_fields() {
        assert!(encode_record_v2(&human_record()).is_ok());

        let mut wrong_hash = human_record();
        if let ApplicationV2::Human(app) = &mut wrong_hash.application {
            app.approval_request_hash = "d".repeat(64);
        }
        assert!(encode_record_v2(&wrong_hash).is_err());

        let mut wrong_target = human_record();
        if let ApplicationV2::Human(app) = &mut wrong_target.application {
            app.after = "e".repeat(64);
        }
        assert!(encode_record_v2(&wrong_target).is_err());

        let mut two_targets = human_record();
        two_targets.targets.push(TargetState {
            path: "docs/other.md".into(),
            before: FileState::Absent,
            after: FileState::Present("e".repeat(64)),
        });
        assert!(encode_record_v2(&two_targets).is_err());

        let mut model_event = human_record();
        model_event.events.encoded_events =
            vec![crate::codec::encode_event(&CourtEvent::ChangesApplied {
                task: TaskId(7),
                skipped: Vec::new(),
            })];
        assert!(encode_record_v2(&model_event).is_err());

        for bad in [
            serde_json::json!({"kind": "human", "task": 7, "changes_version": "v1", "skipped": []}),
            serde_json::json!({"kind": "model", "task": 7, "changes_version": "v1", "skipped": [], "project": "x"}),
            serde_json::json!({"kind": "alien"}),
        ] {
            assert!(
                serde_json::from_value::<ApplicationV2>(bad).is_err(),
                "跨分支或未知种类必须拒绝"
            );
        }
    }

    #[test]
    fn scene_classification_separates_absent_exact_partial_and_diverged() {
        let mut record = human_record();
        record.events.expected_events = record.events.encoded_events.len() as u64;
        record.events.expected_bytes = 0;
        record.events.history_sha256 =
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into();
        let batch: Vec<u8> = record
            .events
            .encoded_events
            .iter()
            .flat_map(|line| {
                let mut bytes = line.as_bytes().to_vec();
                bytes.push(b'\n');
                bytes
            })
            .collect();
        assert_eq!(
            classify_scene(&record, b"").unwrap(),
            SceneEvidence::CleanAbsent
        );
        assert_eq!(
            classify_scene(&record, &batch).unwrap(),
            SceneEvidence::ExactAtSlot
        );
        assert_eq!(
            classify_scene(&record, &batch[..batch.len() / 2]).unwrap(),
            SceneEvidence::Partial
        );
        assert_eq!(
            classify_scene(&record, b"v1\tCLOCK\t1\n").unwrap(),
            SceneEvidence::Diverged
        );
        let mut changed = record.clone();
        changed.events.history_sha256 = "0".repeat(64);
        assert_eq!(
            classify_scene(&changed, b"").unwrap(),
            SceneEvidence::Diverged
        );
    }

    fn assert_v2_rejected(record: &RecoveryRecordV2, reason: &str) {
        match encode_record_v2(record) {
            Ok(_) => panic!("不应通过校验: {reason}"),
            Err(RecoveryError::Corrupt(found)) => assert_eq!(found, reason),
            Err(other) => panic!("期望 Corrupt，却是 {other}"),
        }
    }

    /// 同时改写来源与批次里的准奏事件，使两者仍一致，只留下待测的那一处问题。
    fn edit_human(
        record: &mut RecoveryRecordV2,
        edit: impl FnOnce(&mut HumanApplication, &mut ziyuan_core::HumanApproval),
    ) {
        let ApplicationV2::Human(app) = &mut record.application else {
            panic!("应为人工来源");
        };
        let slot = record
            .events
            .encoded_events
            .iter_mut()
            .find(|line| line.contains("HUMANAPPROVED"))
            .expect("批次含准奏事件");
        let CourtEvent::HumanTranslationApproved { mut approval } =
            crate::codec::decode_event(slot).expect("解码准奏事件")
        else {
            panic!("应为准奏事件");
        };
        edit(app, &mut approval);
        *slot = crate::codec::encode_event(&CourtEvent::HumanTranslationApproved { approval });
    }

    #[test]
    fn human_binding_must_sit_inside_the_fixed_prefix() {
        assert!(encode_record_v2(&human_record()).is_ok());
        // 前缀 3 条：索引 3 起都落在本批或更后，不能当作已入簿的绑定。
        for index in [3, 4, u64::MAX] {
            let mut record = human_record();
            edit_human(&mut record, |_, approval| {
                approval.request.binding.index = index;
            });
            assert_v2_rejected(&record, "送审绑定须位于本笔事务的前缀之内");
        }
        let mut no_prefix = human_record();
        no_prefix.events.expected_events = 2;
        assert_v2_rejected(&no_prefix, "送审绑定须位于本笔事务的前缀之内");
        let mut underflow = human_record();
        underflow.events.expected_events = 1;
        assert_v2_rejected(&underflow, "事件批条数超过预期");
    }

    #[test]
    fn zero_targets_are_only_for_same_byte_approval() {
        let mut created = human_record();
        created.targets.clear();
        assert_v2_rejected(&created, "零目标只用于同字节批准：准奏前后摘要须相同");

        let mut rewritten = human_record();
        rewritten.targets.clear();
        edit_human(&mut rewritten, |app, approval| {
            app.before = Some("e".repeat(64));
            approval.request.target.before = Some("e".repeat(64));
        });
        assert_v2_rejected(&rewritten, "零目标只用于同字节批准：准奏前后摘要须相同");

        let mut same = human_record();
        same.targets.clear();
        edit_human(&mut same, |app, approval| {
            app.before = Some(app.after.clone());
            approval.request.target.before = Some(app.after.clone());
        });
        let bytes = encode_record_v2(&same).expect("同字节批准可编码");
        assert_eq!(
            decode_record_any(&bytes).expect("解码"),
            DecodedRecovery::V2(same)
        );
    }

    #[test]
    fn second_format_limit_counts_escaped_utf8_envelope_bytes() {
        let with_workspace = |suffix: &str| {
            let mut record = human_record();
            record.workspace = PathBuf::from(format!("/tmp/{suffix}"));
            record
        };
        let base = encode_record_v2(&with_workspace("")).expect("编码").len();
        let room = MAX_RECORD_BYTES - base;
        // 每单元原文 5 字节：3 字节汉字、反斜杠（转义成 2 字节）、U+0001（转义成 6 字节），共 11 字节。
        let unit = "紫\\\u{1}";
        let units = room / 11 - 1;
        let fill = format!("{}{}", unit.repeat(units), "a".repeat(room - units * 11));
        assert!(fill.len() < room, "原文字节须少于信封实际字节");

        let at_limit = with_workspace(&fill);
        let bytes = encode_record_v2(&at_limit).expect("恰好上限可编码");
        assert_eq!(bytes.len(), MAX_RECORD_BYTES);
        assert_eq!(
            decode_record_any(&bytes).expect("恰好上限可解码"),
            DecodedRecovery::V2(at_limit)
        );

        for over in [format!("{fill}a"), fill.replacen('a', "\u{1}", 1)] {
            match encode_record_v2(&with_workspace(&over)) {
                Err(RecoveryError::TooLarge { limit }) => assert_eq!(limit, MAX_RECORD_BYTES),
                other => panic!("超限应为 TooLarge，却是 {other:?}"),
            }
        }
        let mut padded = bytes;
        padded.push(b' ');
        assert!(matches!(
            decode_record_any(&padded),
            Err(RecoveryError::TooLarge { .. })
        ));
    }
}
