//! `ziyuan text recovery` 的端到端检查：用真实的工作目录锁与恢复记录，
//! 走真实的命令行二进制，不模拟文件系统。

use std::path::Path;
use std::process::Command;
use ziyuan_storage::recovery::{
    ApplicationSource, EventBatch, FileState, RecordState, RecoveryRecord, TargetState,
};
use ziyuan_storage::{LockScope, Workspace, WriteGuard};

const OPERATION: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90";

struct Temp(std::path::PathBuf);
impl Temp {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ziyuan-recovery-cli-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("建临时目录");
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 记录里的工作区只能用 `X:\` 或 `/` 开头，规范化后的 Windows 根带 `\\?\`，故去掉前缀。
///
/// 必须传入**已规范化**的根（`ws.root()`），不要传临时目录的原始路径：CI 的
/// `%TEMP%` 是 8.3 短名（`RUNNER~1`），与规范化后的长名（`runneradmin`）对不上，
/// 记录会被判成「不是这个工作目录」。
fn record_root(canonical: &Path) -> std::path::PathBuf {
    let text = canonical.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) => std::path::PathBuf::from(rest),
        None => canonical.to_path_buf(),
    }
}

fn record(workspace: &Path, state: RecordState) -> RecoveryRecord {
    use ziyuan_core::{CourtEvent, TaskId};
    let applied = CourtEvent::ChangesApplied {
        task: TaskId(7),
        skipped: Vec::new(),
    };
    RecoveryRecord {
        operation_id: OPERATION.into(),
        workspace: workspace.to_path_buf(),
        generation_before: 0,
        generation_after: 2,
        state,
        application: ApplicationSource {
            task: 7,
            changes_version: "v1".into(),
            skipped: Vec::new(),
        },
        events: EventBatch {
            expected_events: 1,
            expected_bytes: 0,
            // 空事件簿的摘要：记录预告本批写在开头，故摘要必须是对得上的一段真实历史。
            history_sha256: empty_history_sha256(),
            encoded_events: vec![ziyuan_storage::encode_event_line(&applied)],
        },
        targets: vec![TargetState {
            path: "docs/notes.md".into(),
            before: FileState::Absent,
            after: FileState::Present(ziyuan_storage::content_sha256(TARGET)),
        }],
    }
}

/// 记录里声明的新内容；测试把它真的写到目标位置，收尾时的摘要核对才能通过。
const TARGET: &[u8] = b"new bytes";

/// 空事件簿的 SHA-256：没有任何行时摘要就是空输入的摘要。
fn empty_history_sha256() -> String {
    // SHA-256 对空输入的公开常量。写成字面量免得测试引入额外依赖。
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into()
}

fn publish(ws: &Workspace, guard: &mut WriteGuard, state: RecordState) {
    let root = record_root(ws.root());
    guard.reserve().expect("占下奇数世代");
    // 只能先发布 Prepared/Published，再到完成态。
    let start = match state {
        RecordState::Completed | RecordState::Published => RecordState::Published,
        RecordState::Prepared | RecordState::Abandoned => RecordState::Prepared,
    };
    guard.publish(&record(&root, start)).expect("写恢复记录");
    // Abandoned 在测试里保持 Prepared 起：放弃只能由命令行在 Prepared 上执行。
    if state == RecordState::Completed {
        guard.complete(OPERATION).expect("标记完成");
    }
}

/// 让目标文件真的处于记录里的「新内容」状态：收尾要核对摘要，凭空指向不存在的文件会被拒。
fn materialize_target(workspace: &Path) {
    let dir = workspace.join("docs");
    std::fs::create_dir_all(&dir).expect("建目标目录");
    std::fs::write(dir.join("notes.md"), TARGET).expect("写目标文件");
}

fn run(workspace: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ziyuan"))
        .args(args)
        .current_dir(workspace)
        .output()
        .expect("运行 ziyuan");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn status_reports_idle_on_a_clean_workspace() {
    let temp = Temp::new("idle");
    let (code, stdout, _) = run(temp.path(), &["text", "recovery", "status"]);
    assert_eq!(code, 0, "干净工作目录应为退出码 0");
    assert!(stdout.contains("世代号：0"), "实际输出：{stdout}");
    assert!(stdout.contains("idle"), "实际输出：{stdout}");
}

#[test]
fn a_published_record_blocks_writes_and_status_says_so() {
    let temp = Temp::new("published");
    let ws = Workspace::open(temp.path()).expect("打开工作目录");
    let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
    publish(&ws, &mut guard, RecordState::Published);
    drop(guard);

    let reopened = Workspace::open(temp.path()).expect("重新打开");
    assert!(
        reopened.ensure_writable().is_err(),
        "已发布的记录必须阻断写入"
    );

    let (code, stdout, _) = run(temp.path(), &["text", "recovery", "status"]);
    assert_eq!(code, 1, "未收尾时状态命令应以退出码 1 结束");
    assert!(stdout.contains(OPERATION), "应指出操作 ID：{stdout}");
    assert!(stdout.contains("1"), "应报告非零世代号：{stdout}");
}

#[test]
fn finish_requires_the_matching_operation_and_then_unblocks() {
    for upgraded in [false, true] {
        let temp = Temp::new("finish");
        materialize_target(temp.path());
        let ws = Workspace::open(temp.path()).expect("打开工作目录");
        let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
        if upgraded {
            guard.upgrade_write_protocol().expect("显式升级写入闸门");
        }
        publish(&ws, &mut guard, RecordState::Completed);
        drop(guard);

        // 操作 ID 不符时拒绝，且不改动磁盘。
        let (code, _, stderr) = run(
            temp.path(),
            &[
                "text",
                "recovery",
                "finish",
                "ffffffffffffffffffffffffffffffff",
                "--yes",
            ],
        );
        assert_eq!(code, 1, "ID 不符应失败：{stderr}");
        assert!(
            stderr.contains("不符") || stderr.contains("不符"),
            "{stderr}"
        );

        // 不给 --yes 视为用法错误。
        let (code, _, _) = run(temp.path(), &["text", "recovery", "finish", OPERATION]);
        assert_eq!(code, 2, "缺少 --yes 应为用法错误");

        let (code, stdout, stderr) = run(
            temp.path(),
            &["text", "recovery", "finish", OPERATION, "--yes"],
        );
        assert_eq!(code, 0, "收尾应成功：{stderr}");
        assert!(stdout.contains("已收尾"), "{stdout}");

        let reopened = Workspace::open(temp.path()).expect("重新打开");
        reopened.ensure_writable().expect("收尾后应可写入");
        assert_eq!(reopened.generation().expect("读世代号"), 2);
        assert!(
            reopened.pending_record().expect("读记录").is_none(),
            "收尾后恢复记录应已清理"
        );
        assert_eq!(
            reopened.write_protocol().unwrap(),
            Some(if upgraded { 2 } else { 1 })
        );
        assert_eq!(
            std::fs::read(temp.path().join("docs/notes.md")).unwrap(),
            TARGET
        );
    }
}

#[test]
fn finish_refuses_when_a_target_no_longer_matches_the_record() {
    let temp = Temp::new("finish-drift");
    materialize_target(temp.path());
    let ws = Workspace::open(temp.path()).expect("打开工作目录");
    let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
    publish(&ws, &mut guard, RecordState::Completed);
    drop(guard);

    // 收尾前有人把目标改成了第三种内容：既不是旧内容也不是新内容。
    std::fs::write(temp.path().join("docs/notes.md"), b"hand edited").expect("手改目标");

    let (code, _, stderr) = run(
        temp.path(),
        &["text", "recovery", "finish", OPERATION, "--yes"],
    );
    assert_eq!(code, 1, "目标被改过时不应收尾：{stderr}");
    assert!(
        stderr.contains("docs/notes.md") || stderr.contains("docs\\notes.md"),
        "应指出哪个目标对不上：{stderr}"
    );

    // 未收尾：世代号仍是奇数，记录还在。
    let reopened = Workspace::open(temp.path()).expect("重新打开");
    assert_eq!(reopened.generation().expect("读世代号"), 1);
    assert!(reopened.pending_record().expect("读记录").is_some());
}

#[test]
fn finish_allows_a_published_record_whose_files_were_already_written() {
    // 真实崩溃形态：文件已按新内容落盘，事件却没记上，记录停在「已发布」。
    let temp = Temp::new("published-written");
    materialize_target(temp.path());
    let ws = Workspace::open(temp.path()).expect("打开工作目录");
    let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
    publish(&ws, &mut guard, RecordState::Published);
    drop(guard);

    // 第一版 finish 只收尾：即使本批事件不在簿中，也不重放业务或截断残行。
    let log_path = temp.path().join(".ziyuan/court.log");
    let log_before = b"v1\tCLOCK\t120\nunfinished-tail";
    std::fs::write(&log_path, log_before).expect("写入带残行的事件簿");
    let (code, _, stderr) = run(
        temp.path(),
        &["text", "recovery", "finish", OPERATION, "--yes"],
    );
    assert_eq!(code, 0, "文件已是新内容时应允许收尾：{stderr}");

    let reopened = Workspace::open(temp.path()).expect("重新打开");
    reopened.ensure_writable().expect("收尾后应可写入");
    assert_eq!(reopened.generation().expect("读代次"), 2);
    assert!(reopened.pending_record().expect("读记录").is_none());
    assert_eq!(std::fs::read(&log_path).expect("读事件簿"), log_before);
    assert_eq!(
        std::fs::read(temp.path().join("docs/notes.md")).expect("读目标"),
        TARGET
    );
}

#[test]
fn finish_refuses_a_published_record_whose_files_were_never_written() {
    // 另一半形态：记录发布了，但一个文件都还没写到，目标仍是旧内容。
    // 这正是「已发布就崩了」的常态，不能当作完成来收尾——那样等于把用户已批准的改动悄悄丢掉。
    let temp = Temp::new("published-unwritten");
    let ws = Workspace::open(temp.path()).expect("打开工作目录");
    let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
    publish(&ws, &mut guard, RecordState::Published);
    drop(guard);

    let (code, stdout, stderr) = run(
        temp.path(),
        &["text", "recovery", "finish", OPERATION, "--yes"],
    );
    assert_ne!(code, 0, "什么都没写时不应报成功收尾：{stdout}");
    assert!(stderr.contains("没有完成"), "{stderr}");
    assert!(stderr.contains("abandon"), "应指出放弃的去路：{stderr}");

    // 记录与奇数世代都必须原样留着：拒绝收尾不能顺手把状态改掉。
    let reopened = Workspace::open(temp.path()).expect("重新打开");
    assert!(reopened.pending_record().expect("读记录").is_some());
    assert!(reopened.ensure_writable().is_err(), "仍应拒绝写入");
}

#[test]
fn a_published_record_with_nothing_written_can_be_abandoned() {
    // 拒绝收尾之后，用户仍要有一条明确的路：确认放弃。
    let temp = Temp::new("published-abandon");
    let ws = Workspace::open(temp.path()).expect("打开工作目录");
    let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
    publish(&ws, &mut guard, RecordState::Published);
    drop(guard);

    let (code, _, stderr) = run(
        temp.path(),
        &["text", "recovery", "abandon", OPERATION, "--yes"],
    );
    assert_eq!(code, 0, "目标全部未改动时应允许放弃：{stderr}");

    let reopened = Workspace::open(temp.path()).expect("重新打开");
    reopened.ensure_writable().expect("放弃后应可写入");
}

#[test]
fn a_published_record_whose_targets_were_written_cannot_be_abandoned() {
    // 目标已是新内容时放弃，等于把已落盘的结果从账上抹掉，必须拒绝。
    let temp = Temp::new("published-written-abandon");
    materialize_target(temp.path());
    let ws = Workspace::open(temp.path()).expect("打开工作目录");
    let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
    publish(&ws, &mut guard, RecordState::Published);
    drop(guard);

    let (code, stdout, stderr) = run(
        temp.path(),
        &["text", "recovery", "abandon", OPERATION, "--yes"],
    );
    assert_ne!(code, 0, "目标已改写时不应允许放弃：{stdout}");
    assert!(stderr.contains("已不是记录中的旧内容"), "{stderr}");
    assert!(stderr.contains("finish"), "应指出收尾的去路：{stderr}");

    let reopened = Workspace::open(temp.path()).expect("重新打开");
    assert!(reopened.pending_record().expect("读记录").is_some());
}

#[test]
fn a_lookalike_event_from_another_operation_does_not_count_as_done() {
    // 回归：曾经只比对事件簿「最近 N 条」，于是另一笔内容相同的准奏写出的逐字相同的一行，
    // 会被误当成「本批已追记」，让一次根本没写的准奏被报成收尾成功。现在要求
    // 本批之前的历史摘要与记录相符、且本批紧接着它。
    let temp = Temp::new("lookalike");
    let ws = Workspace::open(temp.path()).expect("打开工作目录");
    let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
    publish(&ws, &mut guard, RecordState::Published);
    drop(guard);

    // 外部进程追记一条编码完全相同的事件（同一事件、同一内容）。
    let mut log = ziyuan_storage::EventLog::new(temp.path());
    let applied = ziyuan_core::CourtEvent::ChangesApplied {
        task: ziyuan_core::TaskId(7),
        skipped: Vec::new(),
    };
    ziyuan_storage::EventStore::append(&mut log, 0, &[applied]).expect("追记外部事件");

    // 目标仍未写：不能因为事件簿里有那一行就报成功。
    let (code, stdout, stderr) = run(
        temp.path(),
        &["text", "recovery", "finish", OPERATION, "--yes"],
    );
    assert_ne!(code, 0, "目标未写时不应报收尾成功：{stdout}");
    assert!(stderr.contains("没有完成"), "{stderr}");

    // 放弃同样不能盲目通过：事件簿里已经有那一行，宁可拒绝让人核对，也不擅自抹掉。
    let (code, _, stderr) = run(
        temp.path(),
        &["text", "recovery", "abandon", OPERATION, "--yes"],
    );
    assert_ne!(code, 0, "事件簿已有同类事件时不应擅自放弃：{stderr}");
    assert!(stderr.contains("已经在事件簿中"), "{stderr}");
}

#[test]
fn finish_accepts_crlf_content_written_verbatim() {
    // 落盘按字节原样写，不做行尾规范化；记录里的摘要是对写后的字节算的，
    // 因此含 CRLF 的目标也应能顺利收尾。这里把这一点钉住，免得日后再猜。
    let temp = Temp::new("crlf");
    let crlf = b"line one\r\nline two\r\n";
    let ws = Workspace::open(temp.path()).expect("打开工作目录");
    let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
    let mut rec = record(&record_root(ws.root()), RecordState::Published);
    rec.targets[0].after = FileState::Present(ziyuan_storage::content_sha256(crlf));
    guard.reserve().expect("占下奇数世代");
    guard.publish(&rec).expect("写恢复记录");
    drop(guard);
    std::fs::create_dir_all(temp.path().join("docs")).expect("建目标目录");
    std::fs::write(temp.path().join("docs/notes.md"), crlf).expect("写目标文件");

    let (code, _, stderr) = run(
        temp.path(),
        &["text", "recovery", "finish", OPERATION, "--yes"],
    );
    assert_eq!(code, 0, "CRLF 内容应能收尾：{stderr}");
}

#[test]
fn abandon_unblocks_without_touching_targets() {
    let temp = Temp::new("abandon");
    let ws = Workspace::open(temp.path()).expect("打开工作目录");
    let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
    publish(&ws, &mut guard, RecordState::Abandoned);
    drop(guard);

    // 放弃前写入必须被拒绝（记录仍处于 Prepared 或已放弃但世代为奇数）。
    let reopened = Workspace::open(temp.path()).expect("重新打开");
    assert!(reopened.ensure_writable().is_err(), "未收尾时应拒绝写入");
    drop(reopened);

    let (code, stdout, stderr) = run(
        temp.path(),
        &["text", "recovery", "abandon", OPERATION, "--yes"],
    );
    assert_eq!(code, 0, "放弃应成功：{stderr}");
    assert!(stdout.contains("已放弃"), "{stdout}");

    let reopened = Workspace::open(temp.path()).expect("重新打开");
    reopened.ensure_writable().expect("放弃后应可写入");
    assert_eq!(reopened.generation().expect("读世代号"), 2);
}

#[test]
fn status_json_carries_the_operation_id() {
    let temp = Temp::new("json");
    let ws = Workspace::open(temp.path()).expect("打开工作目录");
    let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
    publish(&ws, &mut guard, RecordState::Published);
    drop(guard);

    let record_path = temp.path().join(".ziyuan/recovery.json");
    let before = std::fs::read(&record_path).expect("读恢复记录");
    let (code, stdout, _) = run(temp.path(), &["text", "recovery", "status", "--json"]);
    assert_eq!(code, 1);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("输出应为 JSON");
    assert_eq!(value["state"], "published");
    assert_eq!(value["operation_id"], OPERATION);
    assert_eq!(value["task"], 7);
    let next = value["next"].as_str().expect("next");
    assert!(
        next.contains(&format!("ziyuan text recovery finish {OPERATION} --yes")),
        "{next}"
    );
    assert_eq!(std::fs::read(&record_path).expect("重读恢复记录"), before);
    assert_eq!(ws.generation().expect("读代次"), 1);
}

#[test]
fn unsupported_recovery_sources_are_refused_without_touching_evidence() {
    for (version, kind) in [
        (2, None),
        (2, Some("human")),
        (3, Some("model")),
        (1, Some("human")),
    ] {
        let temp = Temp::new("unsupported-source");
        materialize_target(temp.path());
        let ws = Workspace::open(temp.path()).expect("打开工作目录");
        let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
        publish(&ws, &mut guard, RecordState::Published);
        drop(guard);

        let record_path = temp.path().join(".ziyuan/recovery.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&record_path).expect("读恢复记录"))
                .expect("记录 JSON");
        value["format_version"] = serde_json::json!(version);
        if let Some(kind) = kind {
            value["record"]["application"]["kind"] = serde_json::json!(kind);
        }
        std::fs::write(&record_path, serde_json::to_vec(&value).expect("信封 JSON"))
            .expect("放入不支持的恢复记录");
        std::fs::write(
            temp.path().join(".ziyuan/court.log"),
            b"v1\tCLOCK\t120\nunfinished-tail",
        )
        .expect("写入带残行的事件簿");

        let protected = [
            ".ziyuan/recovery.json",
            ".ziyuan/write.generation",
            ".ziyuan/write-protocol",
            ".ziyuan/court.log",
            "docs/notes.md",
        ];
        let before = protected.map(|path| std::fs::read(temp.path().join(path)).expect("读证据"));
        for args in [
            vec!["text", "recovery", "status", "--json"],
            vec!["text", "recovery", "finish", OPERATION, "--yes"],
            vec!["text", "recovery", "abandon", OPERATION, "--yes"],
        ] {
            let (code, stdout, stderr) = run(temp.path(), &args);
            assert_ne!(code, 0, "不支持的记录不能报成功：{stdout}");
            let expected = if version > 2 {
                "更新版本"
            } else {
                "恢复记录损坏"
            };
            assert!(stderr.contains(expected), "{args:?}: {stderr}");
            for (path, bytes) in protected.iter().zip(&before) {
                assert_eq!(
                    std::fs::read(temp.path().join(path)).expect("重读证据"),
                    *bytes,
                    "{args:?} 不得改写 {path}"
                );
            }
            assert_eq!(ws.generation().expect("读代次"), 1);
            assert!(ws.ensure_writable().is_err(), "拒绝后仍须阻断写入");
        }
    }
}

const PROJECT: &str = "0123456789abcdef0123456789abcdef";
const BLOCK: &str = "c0001-b0001";
const TARGET_BYTES: &[u8] = b"new chapter";

fn logical_path() -> String {
    format!("translations/{PROJECT}/{BLOCK}/zh.json")
}

/// 人工准奏所引用的送审绑定：真实入簿在事件簿第 0 条，构成本笔事务的固定前缀。
fn binding_event() -> ziyuan_core::CourtEvent {
    use ziyuan_core::{
        CourtEvent, DraftBasis, HumanActor, HumanBindingRequest, HumanSubmissionBinding,
        TranslationSource,
    };
    let candidate = "{}\n";
    let request = HumanBindingRequest {
        project: PROJECT.into(),
        block: BLOCK.into(),
        request_id: "bind-1".into(),
        actor: HumanActor::Local,
        submission_id: "submit-1".into(),
        revision: "d".repeat(32),
        basis: DraftBasis {
            project_hash: "a".repeat(64),
            source_hash: "b".repeat(64),
            context_version: 1,
            zhupi_hash: "c".repeat(64),
            translation_hash: None,
        },
        candidate: candidate.into(),
        candidate_hash: ziyuan_storage::content_sha256(candidate.as_bytes()),
        previous: TranslationSource::None {},
    };
    CourtEvent::HumanSubmissionBound {
        binding: Box::new(HumanSubmissionBinding {
            request_hash: ziyuan_governance::human_binding_request_hash(&request),
            request,
        }),
    }
}

/// 记录写下时事件簿里的原始前缀：只有那条送审绑定。
fn prefix() -> String {
    format!("{}\n", ziyuan_storage::encode_event_line(&binding_event()))
}

fn lines(record: &ziyuan_storage::recovery::RecoveryRecordV2) -> String {
    let mut out = String::new();
    for line in &record.events.encoded_events {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// 第二版人工记录：前缀含真实绑定，批次为一条 Clock 加一条准奏，目标为一次新建。
fn human_record(workspace: &Path, after: &str) -> ziyuan_storage::recovery::RecoveryRecordV2 {
    use ziyuan_core::{
        CourtEvent, HumanActor, HumanApproval, HumanApprovalRequest, HumanApprovalTarget,
        TranslationSource,
    };
    use ziyuan_storage::recovery::{ApplicationV2, HumanApplication, RecoveryRecordV2};
    let logical = logical_path();
    let request = HumanApprovalRequest {
        project: PROJECT.into(),
        block: BLOCK.into(),
        request_id: "approve-1".into(),
        actor: HumanActor::Local,
        submission_id: "bind-1".into(),
        binding: ziyuan_storage::event_reference(0, &binding_event()),
        previous: TranslationSource::None {},
        target: HumanApprovalTarget {
            format: "zh-json/1".into(),
            path: logical.clone(),
            before: None,
            after: after.into(),
        },
    };
    let request_hash = ziyuan_governance::human_approval_request_hash(&request);
    let prefix = prefix();
    RecoveryRecordV2 {
        operation_id: OPERATION.into(),
        workspace: workspace.to_path_buf(),
        generation_before: 0,
        generation_after: 2,
        state: RecordState::Published,
        application: ApplicationV2::Human(HumanApplication {
            project: PROJECT.into(),
            block: BLOCK.into(),
            approval_request_id: "approve-1".into(),
            approval_request_hash: request_hash.clone(),
            previous: TranslationSource::None {},
            target_format: "zh-json/1".into(),
            logical_path: logical.clone(),
            before: None,
            after: after.into(),
        }),
        events: EventBatch {
            expected_events: 3,
            expected_bytes: prefix.len() as u64,
            history_sha256: ziyuan_storage::content_sha256(prefix.as_bytes()),
            encoded_events: vec![
                ziyuan_storage::encode_event_line(&CourtEvent::Clock(1_700_000_000)),
                ziyuan_storage::encode_event_line(&CourtEvent::HumanTranslationApproved {
                    approval: Box::new(HumanApproval {
                        request,
                        request_hash,
                        operation_id: OPERATION.into(),
                    }),
                }),
            ],
        },
        targets: vec![TargetState {
            path: logical,
            before: FileState::Absent,
            after: FileState::Present(after.into()),
        }],
    }
}

/// 现场：占下奇数代次，写入记录与给定的事件簿；`write_target` 时目标为新内容。
fn scene(
    name: &str,
    edit: impl FnOnce(&mut ziyuan_storage::recovery::RecoveryRecordV2),
    log: impl FnOnce(&ziyuan_storage::recovery::RecoveryRecordV2) -> Option<String>,
    write_target: bool,
) -> (Temp, ziyuan_storage::recovery::RecoveryRecordV2) {
    let temp = Temp::new(name);
    let ws = Workspace::open(temp.path()).expect("打开工作目录");
    let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
    guard.reserve().expect("占下奇数世代");
    let after = ziyuan_storage::content_sha256(TARGET_BYTES);
    let mut record = human_record(&record_root(ws.root()), &after);
    edit(&mut record);
    if write_target {
        let full = temp.path().join(logical_path());
        std::fs::create_dir_all(full.parent().unwrap()).expect("建目标目录");
        std::fs::write(&full, TARGET_BYTES).expect("写目标");
    }
    if let Some(log) = log(&record) {
        std::fs::write(temp.path().join(".ziyuan/court.log"), log).expect("写事件簿");
    }
    let bytes = ziyuan_storage::recovery::encode_record_v2(&record).expect("编码");
    std::fs::write(temp.path().join(".ziyuan/recovery.json"), &bytes).expect("写记录");
    drop(guard);
    (temp, record)
}

fn read_log(temp: &Temp) -> String {
    std::fs::read_to_string(temp.path().join(".ziyuan/court.log")).expect("读事件簿")
}

fn generation(temp: &Temp) -> u64 {
    Workspace::open(temp.path())
        .expect("重新打开")
        .generation()
        .expect("读代次")
}

/// 记录、代次、事件簿与逻辑目标的逐字节快照；缺失的文件记为 None。
fn evidence(temp: &Temp) -> Vec<(String, Option<Vec<u8>>)> {
    [
        ".ziyuan/recovery.json".to_string(),
        ".ziyuan/write.generation".to_string(),
        ".ziyuan/court.log".to_string(),
        logical_path(),
    ]
    .into_iter()
    .map(|path| {
        let bytes = std::fs::read(temp.path().join(&path)).ok();
        (path, bytes)
    })
    .collect()
}

fn assert_untouched(temp: &Temp, before: &[(String, Option<Vec<u8>>)], what: &str) {
    assert_eq!(evidence(temp), before, "{what} 不得改盘");
}

fn set_generation(temp: &Temp, generation: u64) {
    std::fs::write(
        temp.path().join(".ziyuan/write.generation"),
        format!("{generation}\n"),
    )
    .expect("写代次");
}

fn finish(temp: &Temp) -> (i32, String, String) {
    run(
        temp.path(),
        &["text", "recovery", "finish", OPERATION, "--yes"],
    )
}

fn abandon(temp: &Temp) -> (i32, String, String) {
    run(
        temp.path(),
        &["text", "recovery", "abandon", OPERATION, "--yes"],
    )
}

fn confirmed(temp: &Temp, action: &str) -> (i32, String, String) {
    run(
        temp.path(),
        &[
            "text",
            "recovery",
            action,
            OPERATION,
            "--yes",
            "--confirm-prepared",
        ],
    )
}

#[test]
fn second_format_status_is_read_only_and_never_finished() {
    let (temp, _record) = scene("second-format", |_| {}, |_| Some(prefix()), false);
    let before = evidence(&temp);
    let (code, stdout, stderr) = run(temp.path(), &["text", "recovery", "status", "--json"]);
    assert_eq!(code, 1, "{stdout}{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("status JSON");
    assert_eq!(value["format"], 2);
    assert_eq!(value["kind"], "human");
    assert_eq!(value["project"], PROJECT);
    assert_eq!(value["block"], BLOCK);
    assert_eq!(value["operation_id"], OPERATION);
    let next = value["next"].as_str().unwrap();
    assert!(next.contains("第二版"), "{next}");
    assert!(
        next.contains(&format!("ziyuan text recovery finish {OPERATION} --yes")),
        "{next}"
    );
    assert_untouched(&temp, &before, "status");

    let (code, _stdout, stderr) = finish(&temp);
    assert_ne!(code, 0, "未追加且目标未写时不能 finish");
    assert!(stderr.contains("没有完成"), "{stderr}");
    assert!(
        stderr.contains(&format!("ziyuan text recovery abandon {OPERATION} --yes")),
        "{stderr}"
    );
    assert!(
        !stderr.contains("\n                     "),
        "第二行不应缩进：{stderr}"
    );
    assert_untouched(&temp, &before, "finish");
}

#[test]
fn second_format_status_next_step_follows_the_state() {
    for (state, must, must_not) in [
        (RecordState::Prepared, "--confirm-prepared", "未追加时可用"),
        (RecordState::Published, "abandon", "--confirm-prepared"),
        (RecordState::Completed, "finish", "abandon"),
        (RecordState::Abandoned, "abandon", "finish"),
    ] {
        let (temp, _record) = scene("status-next", |r| r.state = state, |_| None, false);
        let (code, stdout, _) = run(temp.path(), &["text", "recovery", "status", "--json"]);
        assert_eq!(code, 1);
        let value: serde_json::Value = serde_json::from_str(&stdout).expect("status JSON");
        let next = value["next"].as_str().unwrap();
        assert!(next.contains(OPERATION), "{state:?}: {next}");
        assert!(next.contains(must), "{state:?}: {next}");
        assert!(!next.contains(must_not), "{state:?}: {next}");
    }
}

#[test]
fn second_format_published_record_is_finished_without_replaying_business() {
    let (temp, record) = scene(
        "second-format-finish",
        |_| {},
        |r| Some(prefix() + &lines(r)),
        true,
    );
    let log = read_log(&temp);
    let (code, stdout, stderr) = finish(&temp);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("已收尾"), "{stdout}");
    assert_eq!(generation(&temp), 2);
    assert!(!temp.path().join(".ziyuan/recovery.json").exists());
    assert_eq!(
        std::fs::read(temp.path().join(logical_path())).expect("目标仍在"),
        TARGET_BYTES
    );
    assert_eq!(read_log(&temp), log);
    assert_eq!(log, prefix() + &lines(&record));
}

#[test]
fn second_format_synthetic_human_batch_is_not_appended_without_admission() {
    // 记录自称的人工准奏在本工作区无从复核（项目不存在）：不能凭记录补记。
    let (temp, _record) = scene("second-format-continue", |_| {}, |_| Some(prefix()), true);
    let before = evidence(&temp);
    let (code, _stdout, stderr) = finish(&temp);
    assert_ne!(code, 0, "{stderr}");
    assert!(stderr.contains("补记前复核未通过"), "{stderr}");
    assert!(
        stderr.contains(&format!("ziyuan text recovery abandon {OPERATION} --yes")),
        "{stderr}"
    );
    assert!(
        !stderr.contains("\n                     "),
        "第二行不应缩进：{stderr}"
    );
    assert_untouched(&temp, &before, "finish");
}

#[test]
fn second_format_synthetic_same_byte_record_is_refused_without_touching() {
    let edit = |r: &mut ziyuan_storage::recovery::RecoveryRecordV2| {
        use ziyuan_core::CourtEvent;
        use ziyuan_storage::recovery::ApplicationV2;
        let after = ziyuan_storage::content_sha256(TARGET_BYTES);
        let ApplicationV2::Human(app) = &mut r.application else {
            unreachable!()
        };
        app.before = Some(after.clone());
        let Ok(CourtEvent::HumanTranslationApproved { mut approval }) =
            ziyuan_storage::decode_event_line(&r.events.encoded_events[1])
        else {
            unreachable!()
        };
        approval.request.target.before = Some(after);
        approval.request_hash = ziyuan_governance::human_approval_request_hash(&approval.request);
        app.approval_request_hash = approval.request_hash.clone();
        r.events.encoded_events[1] =
            ziyuan_storage::encode_event_line(&CourtEvent::HumanTranslationApproved { approval });
        r.targets.clear();
    };
    // 零目标同样要复核；逻辑输出缺失时更不能收尾。
    for write_target in [true, false] {
        let (temp, _record) = scene("same-bytes", edit, |_| Some(prefix()), write_target);
        let before = evidence(&temp);
        let (code, _stdout, stderr) = finish(&temp);
        assert_ne!(code, 0, "{stderr}");
        assert_untouched(&temp, &before, "finish");
    }
}
#[test]
fn second_format_absent_batch_and_old_targets_can_be_abandoned() {
    let (temp, _record) = scene("second-format-abandon", |_| {}, |_| Some(prefix()), false);
    let (code, stdout, stderr) = abandon(&temp);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stdout.contains("已放弃"), "{stdout}");
    assert_eq!(generation(&temp), 2);
    assert!(!temp.path().join(".ziyuan/recovery.json").exists());
    assert_eq!(read_log(&temp), prefix(), "放弃不得追加事件");
    assert!(!temp.path().join(logical_path()).exists());
}

fn model_record(workspace: &Path) -> ziyuan_storage::recovery::RecoveryRecordV2 {
    use ziyuan_core::{CourtEvent, TaskId};
    use ziyuan_storage::recovery::{ApplicationV2, ModelApplication, RecoveryRecordV2};
    let applied = CourtEvent::ChangesApplied {
        task: TaskId(7),
        skipped: Vec::new(),
    };
    RecoveryRecordV2 {
        operation_id: OPERATION.into(),
        workspace: workspace.to_path_buf(),
        generation_before: 0,
        generation_after: 2,
        state: RecordState::Published,
        application: ApplicationV2::Model(ModelApplication {
            task: 7,
            changes_version: "v1".into(),
            skipped: Vec::new(),
        }),
        events: EventBatch {
            expected_events: 1,
            expected_bytes: 0,
            history_sha256: empty_history_sha256(),
            encoded_events: vec![ziyuan_storage::encode_event_line(&applied)],
        },
        targets: Vec::new(),
    }
}

#[test]
fn second_format_completed_record_finishes_only_with_exact_batch() {
    let (temp, record) = scene(
        "second-format-completed",
        |r| r.state = RecordState::Completed,
        |r| Some(prefix() + &lines(r)),
        true,
    );
    let (code, stdout, stderr) = finish(&temp);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(generation(&temp), 2);
    assert!(!temp.path().join(".ziyuan/recovery.json").exists());
    assert_eq!(read_log(&temp), prefix() + &lines(&record));
    assert_eq!(
        std::fs::read(temp.path().join(logical_path())).expect("目标仍在"),
        TARGET_BYTES
    );

    // 已完成却看不到批次：与记录矛盾，保留现场。
    let (temp, _record) = scene(
        "second-format-completed-absent",
        |r| r.state = RecordState::Completed,
        |_| Some(prefix()),
        true,
    );
    let before = evidence(&temp);
    let (code, _stdout, stderr) = finish(&temp);
    assert_ne!(code, 0);
    assert!(stderr.contains("与记录不符"), "{stderr}");
    assert!(stderr.contains("ziyuan text recovery status"), "{stderr}");
    assert_untouched(&temp, &before, "finish");
}

#[test]
fn second_format_abandoned_record_can_finish_cleanup() {
    let (temp, _record) = scene(
        "second-format-abandoned",
        |r| r.state = RecordState::Abandoned,
        |_| Some(prefix()),
        false,
    );
    let (code, stdout, stderr) = abandon(&temp);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(generation(&temp), 2);
    assert!(!temp.path().join(".ziyuan/recovery.json").exists());
    assert_eq!(read_log(&temp), prefix());
    assert!(!temp.path().join(logical_path()).exists());
}

#[test]
fn second_format_nonempty_prefix_scenes_are_refused_without_touching() {
    type Edit = fn(&mut ziyuan_storage::recovery::RecoveryRecordV2);
    type Log = fn(&ziyuan_storage::recovery::RecoveryRecordV2) -> Option<String>;
    let cases: [(&str, Edit, Log, &str); 6] = [
        (
            "torn-tail",
            |_| {},
            |_| Some(prefix() + "v1\tCLOCK\t9"),
            "前缀或位置不符",
        ),
        (
            "count-mismatch",
            |r| r.events.expected_events += 1,
            |_| Some(prefix()),
            "前缀或位置不符",
        ),
        (
            "short-window",
            |_| {},
            |_| Some(prefix()[..prefix().len() / 2].to_string()),
            "前缀或位置不符",
        ),
        (
            "slot-taken",
            |_| {},
            |r| Some(prefix() + "v1\tCLOCK\t5\n" + &lines(r)),
            "前缀或位置不符",
        ),
        (
            "clock-only",
            |_| {},
            |r| Some(format!("{}{}\n", prefix(), r.events.encoded_events[0])),
            "批次只写了一半",
        ),
        (
            "approval-without-lf",
            |_| {},
            |r| Some(prefix() + lines(r).trim_end_matches('\n')),
            "批次只写了一半",
        ),
    ];
    for (name, edit, log, scene_text) in cases {
        let (temp, _record) = scene(name, edit, log, true);
        let before = evidence(&temp);
        let (code, _stdout, stderr) = finish(&temp);
        assert_ne!(code, 0, "{name}: finish 不得成功");
        assert!(stderr.contains(scene_text), "{name}: {stderr}");
        assert!(stderr.contains("保留现场"), "{name}: {stderr}");
        assert_untouched(&temp, &before, name);

        std::fs::remove_file(temp.path().join(logical_path())).expect("目标回到旧状态");
        let before = evidence(&temp);
        let (code, _stdout, stderr) = abandon(&temp);
        assert_ne!(code, 0, "{name}: abandon 不得成功");
        assert!(stderr.contains(scene_text), "{name}: {stderr}");
        assert_untouched(&temp, &before, name);
        assert_eq!(generation(&temp), 1);
    }
}

#[test]
fn second_format_generation_must_match_the_record() {
    let exact = |r: &ziyuan_storage::recovery::RecoveryRecordV2| Some(prefix() + &lines(r));
    // (名称, 状态, 代次, 事件簿含批次, 目标已写)
    let cases = [
        ("stuck-at-before", RecordState::Published, 0, true, true),
        (
            "stuck-at-before-absent",
            RecordState::Published,
            0,
            false,
            false,
        ),
        ("foreign-odd", RecordState::Published, 3, true, true),
        (
            "foreign-odd-absent",
            RecordState::Published,
            5,
            false,
            false,
        ),
        ("published-even", RecordState::Published, 2, true, true),
        (
            "published-even-absent",
            RecordState::Published,
            2,
            false,
            false,
        ),
        (
            "completed-foreign-even",
            RecordState::Completed,
            4,
            true,
            true,
        ),
        (
            "abandoned-foreign-even",
            RecordState::Abandoned,
            4,
            false,
            false,
        ),
    ];
    for (name, state, current, logged, written) in cases {
        let (temp, _record) = scene(
            name,
            |r| r.state = state,
            |r| if logged { exact(r) } else { Some(prefix()) },
            written,
        );
        set_generation(&temp, current);
        let before = evidence(&temp);
        for (action, result) in [("finish", finish(&temp)), ("abandon", abandon(&temp))] {
            let (code, stdout, stderr) = result;
            assert_ne!(code, 0, "{name} {action}: {stdout}");
            assert!(
                stderr.contains("当前代次") || stderr.contains("只处理"),
                "{name} {action}: {stderr}"
            );
            assert_untouched(&temp, &before, &format!("{name} {action}"));
        }
    }
}

#[test]
fn confirm_prepared_is_a_usage_error_on_other_records() {
    // 第一版记录：参数无意义，明确报用法错误且不改盘。
    let temp = Temp::new("confirm-v1");
    materialize_target(temp.path());
    let ws = Workspace::open(temp.path()).expect("打开工作目录");
    let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
    publish(&ws, &mut guard, RecordState::Published);
    drop(guard);
    let protected = [
        ".ziyuan/recovery.json",
        ".ziyuan/write.generation",
        "docs/notes.md",
    ];
    let snapshot = || protected.map(|p| std::fs::read(temp.path().join(p)).ok());
    let before = snapshot();
    for action in ["finish", "abandon"] {
        let (code, _stdout, stderr) = confirmed(&temp, action);
        assert_eq!(code, 2, "第一版 {action} 应为用法错误：{stderr}");
        assert!(stderr.contains("--confirm-prepared"), "{stderr}");
        assert!(
            stderr.contains(&format!("ziyuan text recovery {action} {OPERATION} --yes")),
            "{stderr}"
        );
        assert_eq!(snapshot(), before, "第一版 {action} 不得改盘");
        assert!(!temp.path().join(".ziyuan/court.log").exists());
    }

    // 第二版非“已准备”的记录同样拒绝，不能把已发布当成已准备顺手处理。
    for (state, logged, written) in [
        (RecordState::Published, false, true),
        (RecordState::Published, false, false),
        (RecordState::Completed, true, true),
        (RecordState::Abandoned, false, false),
    ] {
        let (temp, _record) = scene(
            "confirm-v2",
            |r| r.state = state,
            |r| {
                Some(if logged {
                    prefix() + &lines(r)
                } else {
                    prefix()
                })
            },
            written,
        );
        let before = evidence(&temp);
        for action in ["finish", "abandon"] {
            let (code, _stdout, stderr) = confirmed(&temp, action);
            assert_eq!(code, 2, "{state:?} {action}: {stderr}");
            assert_untouched(&temp, &before, &format!("{state:?} {action}"));
        }
    }
}

#[test]
fn second_format_partial_batch_is_refused_without_touching() {
    let (temp, record) = scene(
        "second-format-partial",
        |_| {},
        |r| {
            let batch = lines(r);
            Some(prefix() + &batch[..batch.len() / 2])
        },
        true,
    );
    let before = evidence(&temp);
    let (code, _stdout, stderr) = finish(&temp);
    assert_ne!(code, 0);
    assert!(stderr.contains("一半"), "{stderr}");
    assert_untouched(&temp, &before, "finish");
    assert!(read_log(&temp).len() < prefix().len() + lines(&record).len());
}

#[test]
fn second_format_model_record_with_absent_batch_is_finished() {
    let temp = Temp::new("second-format-model");
    let ws = Workspace::open(temp.path()).expect("打开工作目录");
    let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
    guard.reserve().expect("占下奇数世代");
    let record = model_record(&record_root(ws.root()));
    let bytes = ziyuan_storage::recovery::encode_record_v2(&record).expect("编码");
    let record_path = temp.path().join(".ziyuan/recovery.json");
    std::fs::write(&record_path, &bytes).expect("写记录");
    drop(guard);

    let (code, stdout, stderr) = finish(&temp);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(ws.generation().expect("读代次"), 2);
    assert!(!record_path.exists());
    assert_eq!(read_log(&temp), lines(&record), "只追加记录里的固定批次");
}

#[test]
fn second_format_changed_target_is_refused() {
    let (temp, _record) = scene(
        "second-format-target-changed",
        |_| {},
        |r| Some(prefix() + &lines(r)),
        false,
    );
    let full = temp.path().join(logical_path());
    std::fs::create_dir_all(full.parent().unwrap()).expect("建目标目录");
    std::fs::write(&full, b"hand edited").expect("写被改过的目标");
    let before = evidence(&temp);
    let (code, _stdout, stderr) = finish(&temp);
    assert_ne!(code, 0);
    assert!(stderr.contains("目标仍不是"), "{stderr}");
    assert_untouched(&temp, &before, "finish");
}

#[cfg(debug_assertions)]
fn run_with_interrupt(workspace: &Path, point: &str, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_ziyuan"))
        .args(args)
        .current_dir(workspace)
        .env("ZIYUAN_INTERRUPT_RECOVERY", point)
        .output()
        .expect("运行 ziyuan");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// 已发布、目标已写、批次未追加：真实准奏在写目标后中断的现场。
#[cfg(debug_assertions)]
fn published_continue(name: &str) -> Real {
    real_scene(name, "approve-after-target")
}

#[cfg(debug_assertions)]
fn with_state(record: &ziyuan_storage::recovery::RecoveryRecordV2, state: RecordState) -> Vec<u8> {
    let mut record = record.clone();
    record.state = state;
    ziyuan_storage::recovery::encode_record_v2(&record).expect("编码")
}

#[cfg(debug_assertions)]
fn interrupted_finish(real: &Real, point: &str, prepared: bool) -> i32 {
    let mut args = vec!["text", "recovery", "finish", real.op(), "--yes"];
    if prepared {
        args.push("--confirm-prepared");
    }
    run_with_interrupt(real.temp.path(), point, &args).0
}

#[cfg(debug_assertions)]
#[test]
fn interrupted_before_events_resumes_and_finishes() {
    let real = published_continue("interrupt-before-events");
    let before = real.evidence();
    assert_ne!(
        interrupted_finish(&real, "before-events", false),
        0,
        "中断点必须中止进程"
    );
    assert_eq!(real.evidence(), before, "补事件前中断不得改盘");

    let (code, stdout, stderr) = real.recovery("finish", false);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(real.log(), real.batch());
    assert_eq!(real.record_bytes(), None);
    assert_eq!(real.generation(), real.record.generation_after);
}

#[cfg(debug_assertions)]
#[test]
fn interrupted_after_events_resumes_without_duplicating() {
    let real = published_continue("interrupt-after-events");
    assert_ne!(
        interrupted_finish(&real, "after-events", false),
        0,
        "中断点必须中止进程"
    );
    let log = real.log();
    assert_eq!(log, real.batch());
    assert_eq!(
        real.record_bytes(),
        Some(with_state(&real.record, RecordState::Published))
    );
    assert_eq!(real.generation(), real.record.generation_before + 1);

    let (code, stdout, stderr) = real.recovery("finish", false);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(real.log(), log, "重试不得重复追加");
    assert_eq!(real.record_bytes(), None);
    assert_eq!(real.generation(), real.record.generation_after);
}

#[cfg(debug_assertions)]
#[test]
fn interrupted_after_finish_only_removes_the_record() {
    let real = published_continue("interrupt-after-finish");
    assert_ne!(
        interrupted_finish(&real, "after-finish", false),
        0,
        "中断点必须中止进程"
    );
    let log = real.log();
    assert_eq!(log, real.batch());
    assert_eq!(
        real.record_bytes(),
        Some(with_state(&real.record, RecordState::Completed))
    );
    assert_eq!(real.generation(), real.record.generation_after);

    let (code, stdout, stderr) = real.recovery("finish", false);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(real.record_bytes(), None);
    assert_eq!(real.generation(), real.record.generation_after);
    assert_eq!(real.log(), log);
}

#[cfg(debug_assertions)]
#[test]
fn interrupted_prepared_continue_keeps_or_publishes_the_record() {
    // 真正的“已准备”现场：补事件前中断，记录仍是已准备，盘上一字不动。
    let real = published_continue("interrupt-prepared-before");
    let prepared = with_state(&real.record, RecordState::Prepared);
    std::fs::write(real.temp.path().join(".ziyuan/recovery.json"), &prepared).unwrap();
    let before = real.evidence();
    assert_ne!(
        interrupted_finish(&real, "before-events", true),
        0,
        "中断点必须中止进程"
    );
    assert_eq!(real.evidence(), before, "已准备补事件前中断不得改盘");

    // 补事件后中断：记录已显式推进为已发布，批次恰好追加一次。
    assert_ne!(
        interrupted_finish(&real, "after-events", true),
        0,
        "中断点必须中止进程"
    );
    let log = real.log();
    assert_eq!(log, real.batch());
    assert_eq!(
        real.record_bytes(),
        Some(with_state(&real.record, RecordState::Published))
    );
    assert_eq!(real.generation(), real.record.generation_before + 1);

    // 现在是已发布：再带 --confirm-prepared 是用法错误，不改盘；去掉后收尾。
    let before = real.evidence();
    let (code, _stdout, stderr) = real.recovery("finish", true);
    assert_eq!(code, 2, "{stderr}");
    assert_eq!(
        real.evidence(),
        before,
        "已发布带 --confirm-prepared 不得改盘"
    );
    let (code, stdout, stderr) = real.recovery("finish", false);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(real.log(), log, "重试不得重复追加");
    assert_eq!(real.record_bytes(), None);
    assert_eq!(real.generation(), real.record.generation_after);
}
#[test]
fn prepared_record_requires_confirmation() {
    let (temp, _record) = scene(
        "prepared-no-confirm",
        |r| r.state = RecordState::Prepared,
        |_| Some(prefix()),
        true,
    );
    let before = evidence(&temp);
    for (action, result) in [("finish", finish(&temp)), ("abandon", abandon(&temp))] {
        let (code, _stdout, stderr) = result;
        assert_ne!(code, 0);
        assert!(stderr.contains("--confirm-prepared"), "{action}: {stderr}");
        assert!(stderr.contains(OPERATION), "{action}: {stderr}");
        assert_untouched(&temp, &before, action);
        assert_eq!(generation(&temp), 1);
    }
}

#[cfg(debug_assertions)]
#[test]
fn prepared_record_with_confirmation_continues() {
    let real = published_continue("prepared-continue");
    let prepared = with_state(&real.record, RecordState::Prepared);
    std::fs::write(real.temp.path().join(".ziyuan/recovery.json"), &prepared).unwrap();
    let (code, stdout, stderr) = real.recovery("finish", true);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(real.record_bytes(), None);
    assert_eq!(real.log(), real.batch());
    assert_eq!(real.generation(), real.record.generation_after);
}
#[test]
fn prepared_record_is_not_published_when_the_append_check_fails() {
    // 前缀里有一条本版读不了的事件：原始前缀摘要对得上，补事件时却读不出历史。
    // 此时记录必须仍是“已准备”，不能先改成已发布再失败。
    let future = "v1\tFUTUREKIND\tx\n";
    let (temp, _record) = scene(
        "prepared-append-fails",
        |r| {
            r.state = RecordState::Prepared;
            let raw = prefix() + future;
            r.events.expected_events += 1;
            r.events.expected_bytes = raw.len() as u64;
            r.events.history_sha256 = ziyuan_storage::content_sha256(raw.as_bytes());
        },
        |_| Some(prefix() + future),
        true,
    );
    let before = evidence(&temp);
    let (code, _stdout, stderr) = confirmed(&temp, "finish");
    assert_ne!(code, 0, "{stderr}");
    assert!(stderr.contains("更新版本"), "{stderr}");
    assert_untouched(&temp, &before, "补事件失败");
}

#[test]
fn prepared_record_with_confirmation_can_be_abandoned() {
    let (temp, _record) = scene(
        "prepared-abandon",
        |r| r.state = RecordState::Prepared,
        |_| Some(prefix()),
        false,
    );
    let (code, stdout, stderr) = confirmed(&temp, "abandon");
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(!temp.path().join(".ziyuan/recovery.json").exists());
    assert_eq!(read_log(&temp), prefix());
    assert_eq!(generation(&temp), 2);
}

#[test]
fn prepared_record_with_existing_batch_is_refused() {
    let (temp, _record) = scene(
        "prepared-exact",
        |r| r.state = RecordState::Prepared,
        |r| Some(prefix() + &lines(r)),
        true,
    );
    let before = evidence(&temp);
    let (code, _stdout, stderr) = confirmed(&temp, "finish");
    assert_ne!(code, 0);
    assert!(stderr.contains("矛盾"), "{stderr}");
    assert!(stderr.contains("保留现场"), "{stderr}");
    assert_untouched(&temp, &before, "finish");
}

// ---- 真实人工准奏现场：走真实命令行，在准奏事务的中断点上停下 ----

fn ok_out(workspace: &Path, args: &[&str]) -> String {
    let (code, stdout, stderr) = run(workspace, args);
    assert_eq!(code, 0, "{args:?}: {stdout}{stderr}");
    stdout
}

fn ok_value(workspace: &Path, args: &[&str]) -> serde_json::Value {
    let stdout = ok_out(workspace, args);
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{args:?} 不是 JSON：{e}\n{stdout}"))
}

/// 开国、建项目、导入一章；返回项目 ID、首块 ID 与其段 ID。
fn real_project(temp: &Temp) -> (String, String, Vec<String>) {
    let root = temp.path();
    ok_out(root, &["init"]);
    let project = ok_value(
        root,
        &[
            "text",
            "init",
            "--title",
            "准奏",
            "--language",
            "ja",
            "--source",
            "https://example.invalid/work",
            "--rights",
            "用户自述：仅供个人学习",
            "--json",
        ],
    );
    let id = project["id"].as_str().unwrap().to_owned();
    std::fs::write(root.join("a.txt"), "夜が明ける。\n\n港に船が着く。\n").unwrap();
    let project = ok_value(root, &["text", "import", &id, "a.txt", "--json"]);
    let block = &project["chapters"][0]["blocks"][0];
    let paragraphs = block["paragraphs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_owned())
        .collect();
    (
        id,
        block["block_id"].as_str().unwrap().to_owned(),
        paragraphs,
    )
}

/// 保存一份完整草稿并送审；`n` 只区分保存请求。
fn save_and_submit(
    temp: &Temp,
    project: &str,
    block: &str,
    paragraphs: &[String],
    text: &str,
    n: u32,
) {
    let root = temp.path();
    let view = ok_value(root, &["text", "human", "show", project, block, "--json"]);
    let save = serde_json::json!({
        "request_id": format!("save-{n}"),
        "expected_revision": view["draft"]["revision"],
        "basis": view["basis"],
        "paragraphs": paragraphs
            .iter()
            .map(|id| serde_json::json!({"id": id, "text": text}))
            .collect::<Vec<_>>(),
    });
    std::fs::write(root.join("draft.json"), save.to_string()).unwrap();
    ok_out(
        root,
        &[
            "text",
            "human",
            "save",
            project,
            block,
            "--file",
            "draft.json",
        ],
    );
    let submitted = ok_out(root, &["text", "human", "submit", project, block]);
    assert!(submitted.contains("已送审"), "{submitted}");
    assert!(
        submitted.contains(&format!(
            "下一步：ziyuan text human approve {project} {block}"
        )),
        "{submitted}"
    );
}

struct Real {
    temp: Temp,
    project: String,
    block: String,
    /// 准奏开始前事件簿的原始字节：记录的固定前缀。
    prefix: String,
    record: ziyuan_storage::recovery::RecoveryRecordV2,
}

impl Real {
    fn op(&self) -> &str {
        &self.record.operation_id
    }
    fn target(&self) -> std::path::PathBuf {
        let ziyuan_storage::recovery::ApplicationV2::Human(app) = &self.record.application else {
            unreachable!("人工准奏只写人工记录")
        };
        self.temp.path().join(&app.logical_path)
    }
    fn evidence(&self) -> Vec<(String, Option<Vec<u8>>)> {
        let root = self.temp.path();
        [
            root.join(".ziyuan/recovery.json"),
            root.join(".ziyuan/write.generation"),
            root.join(".ziyuan/court.log"),
            self.target(),
        ]
        .into_iter()
        .map(|path| (path.display().to_string(), std::fs::read(&path).ok()))
        .collect()
    }
    fn recovery(&self, action: &str, prepared: bool) -> (i32, String, String) {
        let mut args = vec!["text", "recovery", action, self.op(), "--yes"];
        if prepared {
            args.push("--confirm-prepared");
        }
        run(self.temp.path(), &args)
    }
    fn log(&self) -> String {
        read_log(&self.temp)
    }
    fn batch(&self) -> String {
        self.prefix.clone() + &lines(&self.record)
    }
    fn generation(&self) -> u64 {
        generation(&self.temp)
    }
    fn record_bytes(&self) -> Option<Vec<u8>> {
        std::fs::read(self.temp.path().join(".ziyuan/recovery.json")).ok()
    }
    fn after(&self) -> String {
        let ziyuan_storage::recovery::ApplicationV2::Human(app) = &self.record.application else {
            unreachable!()
        };
        app.after.clone()
    }
    fn target_hash(&self) -> Option<String> {
        std::fs::read(self.target())
            .ok()
            .map(|bytes| ziyuan_storage::content_sha256(&bytes))
    }
}

/// 在准奏事务的中断点 `point` 上中止真实子进程，读出盘上的恢复记录。
#[cfg(debug_assertions)]
fn interrupted_approval(temp: Temp, project: String, block: String, point: &str) -> Real {
    let prefix = read_log(&temp);
    let (code, stdout, stderr) = run_with_interrupt(
        temp.path(),
        point,
        &["text", "human", "approve", &project, &block],
    );
    assert_ne!(code, 0, "中断点必须中止进程：{stdout}{stderr}");
    let bytes = std::fs::read(temp.path().join(".ziyuan/recovery.json")).expect("记录仍在");
    let ziyuan_storage::recovery::DecodedRecovery::V2(record) =
        ziyuan_storage::recovery::decode_record_any(&bytes).expect("解码记录")
    else {
        panic!("人工准奏只写第二版记录")
    };
    assert_eq!(record.state, RecordState::Published);
    Real {
        temp,
        project,
        block,
        prefix,
        record,
    }
}

#[cfg(debug_assertions)]
fn real_scene(name: &str, point: &str) -> Real {
    let temp = Temp::new(name);
    let (project, block, paragraphs) = real_project(&temp);
    save_and_submit(&temp, &project, &block, &paragraphs, "天亮了。", 1);
    interrupted_approval(temp, project, block, point)
}

#[cfg(debug_assertions)]
#[test]
fn approval_interrupted_after_publish_leaves_old_target_and_can_be_abandoned() {
    let real = real_scene("approve-after-publish", "approve-after-publish");
    assert_eq!(real.log(), real.prefix, "发布记录后中断：事件簿不动");
    assert_eq!(real.target_hash(), None, "目标仍是旧内容（不存在）");
    assert_eq!(real.generation(), real.record.generation_before + 1);
    assert_eq!(real.record.targets.len(), 1);

    let before = real.evidence();
    let (code, stdout, _) = run(real.temp.path(), &["text", "recovery", "status", "--json"]);
    assert_eq!(code, 1);
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["kind"], "human");
    assert_eq!(value["operation_id"], real.op());
    assert_eq!(value["project"], real.project.as_str());
    assert_eq!(real.evidence(), before, "status 只读");
    let (code, _, stderr) = real.recovery("finish", false);
    assert_ne!(code, 0);
    assert!(stderr.contains("没有完成"), "{stderr}");
    assert_eq!(real.evidence(), before, "finish 拒绝不改盘");
    let blocked = run(
        real.temp.path(),
        &["text", "human", "approve", &real.project, &real.block],
    );
    assert_ne!(blocked.0, 0, "有未收尾记录时不得再准奏");
    assert_eq!(real.evidence(), before);

    let (code, stdout, stderr) = real.recovery("abandon", false);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(real.log(), real.prefix, "放弃不得追加事件");
    assert_eq!(real.target_hash(), None);
    assert_eq!(real.record_bytes(), None);
    assert_eq!(real.generation(), real.record.generation_after);

    // 放弃后同一送审可重新准奏，这次走完。
    let out = ok_out(
        real.temp.path(),
        &["text", "human", "approve", &real.project, &real.block],
    );
    assert!(out.contains("已准奏并落盘"), "{out}");
    assert_eq!(real.target_hash(), Some(real.after()));
    ok_out(
        real.temp.path(),
        &["text", "human", "finalize", &real.project, &real.block],
    );
}

#[cfg(debug_assertions)]
#[test]
fn approval_interrupted_after_target_is_finished_by_appending_the_recorded_batch() {
    let real = real_scene("approve-after-target", "approve-after-target");
    assert_eq!(real.log(), real.prefix, "写目标后中断：批次未追加");
    assert_eq!(real.target_hash(), Some(real.after()));
    let before = real.evidence();
    let (code, _, stderr) = real.recovery("abandon", false);
    assert_ne!(code, 0, "目标已写，不能放弃：{stderr}");
    assert_eq!(real.evidence(), before, "abandon 拒绝不改盘");

    let (code, stdout, stderr) = real.recovery("finish", false);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(real.log(), real.batch(), "补记恰为记录里的固定批次");
    assert_eq!(real.record_bytes(), None);
    assert_eq!(real.generation(), real.record.generation_after);
    assert_eq!(real.target_hash(), Some(real.after()));
    let out = ok_out(
        real.temp.path(),
        &["text", "human", "finalize", &real.project, &real.block],
    );
    assert!(
        out.contains(&format!(
            "下一步：ziyuan text export-finished {}",
            real.project
        )),
        "{out}"
    );
}

#[cfg(debug_assertions)]
#[test]
fn approval_interrupted_after_append_is_finished_without_duplicating() {
    let real = real_scene("approve-after-append", "approve-after-append");
    assert_eq!(real.log(), real.batch(), "追加后中断：批次已在固定位置");
    assert_eq!(real.target_hash(), Some(real.after()));
    let before = real.evidence();
    let (code, _, _) = real.recovery("abandon", false);
    assert_ne!(code, 0, "批次已追加，不能放弃");
    assert_eq!(real.evidence(), before);

    let (code, stdout, stderr) = real.recovery("finish", false);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(real.log(), real.batch(), "不得重复追加");
    assert_eq!(real.record_bytes(), None);
    assert_eq!(real.generation(), real.record.generation_after);
    let applied = ok_value(
        real.temp.path(),
        &[
            "text",
            "human",
            "apply",
            &real.project,
            &real.block,
            "--json",
        ],
    );
    assert_eq!(applied["already_written"], true);
    assert_eq!(real.log(), real.batch(), "apply 只读");
}

#[cfg(debug_assertions)]
#[test]
fn recovery_finish_redoes_the_human_admission_before_appending() {
    let temp = Temp::new("approve-recheck");
    let (project, block, paragraphs) = real_project(&temp);
    // 先借命令行写一份朱批，取下字节后删掉：送审时没有朱批，事后再放回即“材料已变”。
    ok_out(
        temp.path(),
        &["text", "zhupi", &project, &block, &paragraphs[0], "朱批"],
    );
    let zhupi = temp
        .path()
        .join(".ziyuan/text-projects")
        .join(&project)
        .join("zhupi.json");
    let changed = std::fs::read(&zhupi).expect("朱批文件");
    std::fs::remove_file(&zhupi).unwrap();
    save_and_submit(&temp, &project, &block, &paragraphs, "天亮了。", 1);
    let real = interrupted_approval(temp, project, block, "approve-after-target");
    std::fs::write(&zhupi, &changed).unwrap();

    let before = real.evidence();
    let (code, _, stderr) = real.recovery("finish", false);
    assert_ne!(code, 0);
    assert!(stderr.contains("补记前复核未通过"), "{stderr}");
    assert!(stderr.contains("朱批已变"), "{stderr}");
    assert_eq!(real.evidence(), before, "复核失败不改盘");

    // 已准备的同一现场：--confirm-prepared 也要先复核，记录不得被推进。
    let prepared = with_state(&real.record, RecordState::Prepared);
    std::fs::write(real.temp.path().join(".ziyuan/recovery.json"), &prepared).unwrap();
    let before = real.evidence();
    let (code, _, stderr) = real.recovery("finish", true);
    assert_ne!(code, 0);
    assert!(stderr.contains("补记前复核未通过"), "{stderr}");
    assert_eq!(real.evidence(), before, "复核失败不改盘");

    // 材料复原后复核通过，按记录补记。
    std::fs::remove_file(&zhupi).unwrap();
    let (code, stdout, stderr) = real.recovery("finish", true);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(real.log(), real.batch());
    assert_eq!(real.generation(), real.record.generation_after);
}

#[cfg(debug_assertions)]
#[test]
fn same_byte_approval_interrupted_has_zero_targets_and_checks_the_logical_output() {
    let temp = Temp::new("approve-same-bytes");
    let (project, block, paragraphs) = real_project(&temp);
    save_and_submit(&temp, &project, &block, &paragraphs, "天亮了。", 1);
    ok_out(temp.path(), &["text", "human", "approve", &project, &block]);
    ok_out(
        temp.path(),
        &["text", "human", "finalize", &project, &block],
    );
    // 第二轮：新修订号、同一字节。
    save_and_submit(&temp, &project, &block, &paragraphs, "天亮了。", 2);
    let real = interrupted_approval(temp, project, block, "approve-after-target");
    assert!(real.record.targets.is_empty(), "同字节准奏零目标");
    let written = std::fs::read(real.target()).unwrap();

    // 逻辑输出不在：不能收尾。
    std::fs::remove_file(real.target()).unwrap();
    let before = real.evidence();
    let (code, _, stderr) = real.recovery("finish", false);
    assert_ne!(code, 0, "{stderr}");
    assert_eq!(real.evidence(), before);

    std::fs::write(real.target(), &written).unwrap();
    let (code, stdout, stderr) = real.recovery("finish", false);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(real.log(), real.batch());
    assert_eq!(real.generation(), real.record.generation_after);
    let out = ok_out(
        real.temp.path(),
        &["text", "human", "finalize", &real.project, &real.block],
    );
    assert!(out.contains("已定稿"), "第二轮须能再定稿：{out}");
}

/// 把一份记录直接放上盘：占下奇数代次后写入，模拟记录本身与准入不一致的现场。
fn place_record(temp: &Temp, record: &ziyuan_storage::recovery::RecoveryRecordV2) {
    let ws = Workspace::open(temp.path()).expect("打开工作目录");
    let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
    guard.reserve().expect("占下奇数世代");
    let bytes = ziyuan_storage::recovery::encode_record_v2(record).expect("编码");
    std::fs::write(temp.path().join(".ziyuan/recovery.json"), bytes).expect("写记录");
}

fn approval_line(line: &str) -> ziyuan_core::HumanApproval {
    match ziyuan_storage::decode_event_line(line) {
        Ok(ziyuan_core::CourtEvent::HumanTranslationApproved { approval }) => *approval,
        other => panic!("不是准奏事件：{other:?}"),
    }
}

fn approval_event(approval: ziyuan_core::HumanApproval) -> String {
    ziyuan_storage::encode_event_line(&ziyuan_core::CourtEvent::HumanTranslationApproved {
        approval: Box::new(approval),
    })
}

#[cfg(debug_assertions)]
#[test]
fn recovery_finish_refuses_a_record_whose_target_is_not_the_frozen_candidate() {
    use ziyuan_storage::recovery::ApplicationV2;
    let real = real_scene("forged-target", "approve-after-target");
    let forged = b"forged target\n";
    let after = ziyuan_storage::content_sha256(forged);
    let mut record = real.record.clone();
    let last = record.events.encoded_events.len() - 1;
    let mut approval = approval_line(&record.events.encoded_events[last]);
    approval.request.target.after = after.clone();
    approval.request_hash = ziyuan_governance::human_approval_request_hash(&approval.request);
    let ApplicationV2::Human(app) = &mut record.application else {
        unreachable!()
    };
    app.after = after.clone();
    app.approval_request_hash = approval.request_hash.clone();
    record.targets[0].after = FileState::Present(after);
    record.events.encoded_events[last] = approval_event(approval);
    std::fs::write(
        real.temp.path().join(".ziyuan/recovery.json"),
        ziyuan_storage::recovery::encode_record_v2(&record).expect("编码"),
    )
    .unwrap();
    std::fs::write(real.target(), forged).unwrap();

    let before = real.evidence();
    let (code, _, stderr) = real.recovery("finish", false);
    assert_ne!(code, 0);
    assert!(stderr.contains("补记前复核未通过"), "{stderr}");
    assert!(stderr.contains("冻结候选"), "{stderr}");
    assert_eq!(real.evidence(), before, "复核失败不改盘");
}

#[test]
fn recovery_finish_refuses_a_record_whose_predecessor_was_replaced() {
    use ziyuan_storage::recovery::{ApplicationV2, HumanApplication, RecoveryRecordV2};
    let temp = Temp::new("forged-previous");
    let (project, block, paragraphs) = real_project(&temp);
    save_and_submit(&temp, &project, &block, &paragraphs, "天亮了。", 1);
    ok_out(temp.path(), &["text", "human", "approve", &project, &block]);
    save_and_submit(&temp, &project, &block, &paragraphs, "黎明。", 2);
    ok_out(temp.path(), &["text", "human", "approve", &project, &block]);
    // 第二轮已准奏；再造一份引用同一送审、仍以第一轮为前一来源的准奏记录。
    let log = read_log(&temp);
    let last = log.lines().last().unwrap();
    let mut approval = approval_line(last);
    let operation = "f".repeat(32);
    approval.request.request_id = "approve-again".into();
    approval.request_hash = ziyuan_governance::human_approval_request_hash(&approval.request);
    approval.operation_id = operation.clone();
    let r = approval.request.clone();
    let ws = Workspace::open(temp.path()).unwrap();
    let generation = ws.generation().unwrap();
    let record = RecoveryRecordV2 {
        operation_id: operation.clone(),
        workspace: record_root(ws.root()),
        generation_before: generation,
        generation_after: generation + 2,
        state: RecordState::Published,
        application: ApplicationV2::Human(HumanApplication {
            project: project.clone(),
            block: block.clone(),
            approval_request_id: r.request_id.clone(),
            approval_request_hash: approval.request_hash.clone(),
            previous: r.previous.clone(),
            target_format: r.target.format.clone(),
            logical_path: r.target.path.clone(),
            before: r.target.before.clone(),
            after: r.target.after.clone(),
        }),
        events: EventBatch {
            expected_events: log.lines().count() as u64 + 1,
            expected_bytes: log.len() as u64,
            history_sha256: ziyuan_storage::content_sha256(log.as_bytes()),
            encoded_events: vec![approval_event(approval)],
        },
        targets: vec![TargetState {
            path: r.target.path.clone(),
            before: FileState::Present(r.target.before.clone().unwrap()),
            after: FileState::Present(r.target.after.clone()),
        }],
    };
    drop(ws);
    place_record(&temp, &record);
    let before = evidence_at(&temp, &r.target.path);
    let (code, _, stderr) = run(
        temp.path(),
        &["text", "recovery", "finish", &operation, "--yes"],
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("补记前复核未通过"), "{stderr}");
    assert!(stderr.contains("前一来源"), "{stderr}");
    assert_eq!(evidence_at(&temp, &r.target.path), before, "复核失败不改盘");
}

fn evidence_at(temp: &Temp, logical: &str) -> Vec<Option<Vec<u8>>> {
    [
        ".ziyuan/recovery.json",
        ".ziyuan/write.generation",
        ".ziyuan/court.log",
        logical,
    ]
    .iter()
    .map(|path| std::fs::read(temp.path().join(path)).ok())
    .collect()
}
