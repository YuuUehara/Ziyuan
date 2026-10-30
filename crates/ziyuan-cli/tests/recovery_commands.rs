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
            history_sha256: "9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0"
                .into(),
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
    let temp = Temp::new("finish");
    materialize_target(temp.path());
    let ws = Workspace::open(temp.path()).expect("打开工作目录");
    let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
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

    let (code, _, stderr) = run(
        temp.path(),
        &["text", "recovery", "finish", OPERATION, "--yes"],
    );
    assert_eq!(code, 0, "文件已是新内容时应允许收尾：{stderr}");

    let reopened = Workspace::open(temp.path()).expect("重新打开");
    reopened.ensure_writable().expect("收尾后应可写入");
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
fn finish_accepts_crlf_content_written_verbatim() {
    // 落盘按字节原样写，不做行尾规范化；记录里的摘要是对写后的字节算的，
    // 因此含 CRLF 的目标也应能顺利收尾。这里把这一点钉住，免得日后再猜。
    let temp = Temp::new("crlf");
    let crlf = b"line one\r\nline two\r\n";
    let ws = Workspace::open(temp.path()).expect("打开工作目录");
    let mut guard = ws.lock(LockScope::Workspace).expect("取锁");
    let mut rec = record(&record_root(temp.path()), RecordState::Published);
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

    let (code, stdout, _) = run(temp.path(), &["text", "recovery", "status", "--json"]);
    assert_eq!(code, 1);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("输出应为 JSON");
    assert_eq!(value["state"], "published");
    assert_eq!(value["operation_id"], OPERATION);
    assert_eq!(value["task"], 7);
}
