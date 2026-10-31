//! 准奏落盘的受管写入事务：工作区锁、世代号与恢复记录。
//!
//! 落盘先取工作区锁、推进世代号、写下「已发布」的恢复记录，再写文件、记事件，最后收尾
//! （记录转为「已完成」、世代号回到偶数、删掉记录）。中途失败时记录与奇数世代号留在盘上，
//! 之后的写入一律被拒，须先经 CLI 收尾；这是本模块各处断言的要点。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ziyuan_core::{Capability, CourtEvent, Department, FileChange, Seal, TaskId};
use ziyuan_runtime::{
    BoxError, CancelToken, ExecutionHooks, Job, Limits, Scheduler, WorkerBackend,
};
use ziyuan_storage::recovery::{ApplicationSource, EventBatch, FileState, TargetState};
use ziyuan_storage::{LockScope, MemoryEventLog, RecordState, Workspace, WriteGuard};

use crate::{AppError, Session};

const ORIGINAL: &str = "old\nkeep\n";
const CHANGED: &str = "new\nkeep\n";
const CHANGED_TWICE: &str = "newer\nkeep\n";

/// 临时工作区：每次新目录，析构时尽力删掉。
struct Root(PathBuf);
impl Root {
    fn new(name: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "ziyuan-write-txn-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("a.txt"), ORIGINAL).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn workspace(&self) -> Workspace {
        Workspace::open(&self.0).unwrap()
    }

    fn read(&self) -> String {
        std::fs::read_to_string(self.0.join("a.txt")).unwrap()
    }

    fn generation(&self) -> u64 {
        self.workspace().generation().unwrap()
    }

    fn record(&self) -> Option<ziyuan_storage::RecoveryRecord> {
        self.workspace().pending_record().unwrap()
    }

    /// 目标文件从只读改回可写，供收尾后重试。
    fn make_writable(&self) {
        let target = self.0.join("a.txt");
        let mut permissions = std::fs::metadata(&target).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&target, permissions).unwrap();
    }

    fn make_readonly(&self) {
        let target = self.0.join("a.txt");
        let mut permissions = std::fs::metadata(&target).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&target, permissions).unwrap();
    }

    /// `.ziyuan` 之外的临时文件（替换文件时所用）。
    fn temps(&self) -> Vec<String> {
        let mut found = Vec::new();
        let mut stack = vec![self.0.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n == ".ziyuan") {
                        continue;
                    }
                    stack.push(path);
                    continue;
                }
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if name.starts_with(".ziyuan-tmp-") || name.ends_with(".tmp") {
                    found.push(name);
                }
            }
        }
        found
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        self.make_writable();
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 呈报 `a.txt` 由 `before`（`None` 即新建）改为 `after` 的改动，不含工具调用。
struct Rewrite {
    before: Option<&'static str>,
    after: &'static str,
}
impl WorkerBackend for Rewrite {
    fn execute(&self, _job: &Job, hooks: &dyn ExecutionHooks) -> Result<String, BoxError> {
        hooks.changes_proposed(vec![FileChange {
            path: "a.txt".into(),
            before: self.before.map(str::to_owned),
            after: self.after.into(),
        }]);
        Ok("改毕".into())
    }
}

fn scheduler(backend: impl WorkerBackend + 'static) -> Scheduler {
    Scheduler::new(
        std::sync::Arc::new(backend),
        Limits {
            max_concurrent: 4,
            timeout: Duration::from_secs(10),
        },
    )
}

/// 只执行、不落盘：呈报改动后项目仍是原文，说明执行本身不写文件。
async fn proposed_only(
    backend: impl WorkerBackend + 'static,
    text: &'static str,
) -> (Session<&'static mut MemoryEventLog>, TaskId) {
    let store: &'static mut MemoryEventLog = Box::leak(Box::default());
    let mut session = Session::found(store).unwrap();
    let seal = Seal {
        capabilities: [Capability::WriteFile].into(),
        scope: vec![".".into()],
        mcp: Default::default(),
    };
    let task = session
        .issue(Department::Works, text.into(), seal)
        .unwrap()
        .id;
    session
        .run(&[task], &scheduler(backend), &CancelToken::new(), |_| {})
        .await
        .unwrap();
    (session, task)
}

/// 打开内存事件簿上的会话，下诏、执行完并呈报改动；改动尚未落盘。
async fn prepared() -> (Session<&'static mut MemoryEventLog>, TaskId) {
    proposed_only(
        Rewrite {
            before: Some(ORIGINAL),
            after: CHANGED,
        },
        "改 a.txt",
    )
    .await
}

fn applied(session: &Session<impl ziyuan_storage::EventStore>) -> usize {
    session
        .history()
        .unwrap()
        .iter()
        .filter(|e| matches!(e, CourtEvent::ChangesApplied { .. }))
        .count()
}

/// 准奏必须被拒；`T` 是成功时返回的清理诊断（被拒时无从谈起）。
fn error<T: std::fmt::Debug>(result: Result<T, AppError>) -> String {
    match result {
        Ok(value) => panic!("应被拒绝：{value:?}"),
        Err(e) => e.to_string(),
    }
}

/// 把记录里存的根目录写成编解码器认得的形状（去掉 Windows 的 `\\?\` 前缀）。
fn os_path(root: &Path) -> PathBuf {
    match root.to_string_lossy().strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => root.to_path_buf(),
    }
}

/// 一份合法、但与本次写入无关的「已发布」记录。
fn foreign_record(root: &Path, task: TaskId) -> ziyuan_storage::RecoveryRecord {
    ziyuan_storage::RecoveryRecord {
        operation_id: "0f1e2d3c4b5a69788796a5b4c3d2e1f0".into(),
        workspace: os_path(root),
        generation_before: 0,
        generation_after: 2,
        state: RecordState::Published,
        application: ApplicationSource {
            task: task.0,
            changes_version: "v1".into(),
            skipped: Vec::new(),
        },
        events: EventBatch {
            expected_events: 1,
            expected_bytes: 0,
            history_sha256: "9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0"
                .into(),
            encoded_events: vec![ziyuan_storage::encode_event_line(
                &CourtEvent::ChangesApplied {
                    task,
                    skipped: Vec::new(),
                },
            )],
        },
        targets: vec![TargetState {
            path: "a.txt".into(),
            before: FileState::Present(
                "93862b8f30e3b375e8a4ebfc514bab781f8e817fb28cc8e6774c4d116e73f0cd".into(),
            ),
            after: FileState::Present(
                "5a4b3c2d1e0f9a8b7c6d5e4f3a2b1c0d5a4b3c2d1e0f9a8b7c6d5e4f3a2b1c0d".into(),
            ),
        }],
    }
}

/// 收尾：`complete` → `finish` → `remove_record`，并断言世代号回到偶数。
fn finish_on_disk(root: &Root, operation_id: &str) {
    let ws = root.workspace();
    let mut guard: WriteGuard = ws.lock(LockScope::Workspace).unwrap();
    guard.complete(operation_id).unwrap();
    guard.finish().unwrap();
    guard.remove_record().unwrap();
    drop(guard);
    assert_eq!(root.generation() % 2, 0);
    assert!(root.record().is_none());
}

#[tokio::test]
async fn approval_advances_the_generation_by_two_and_leaves_no_record() {
    let root = Root::new("ok");
    let (mut session, task) = prepared().await;
    assert_eq!(root.generation(), 0);

    session
        .apply_reviewed(task, root.path(), Vec::new(), None)
        .unwrap();

    assert_eq!(root.read(), CHANGED);
    assert_eq!(root.generation(), 2, "偶数世代号净进 2");
    assert!(!root.path().join(".ziyuan/recovery.json").exists());
    assert_eq!(applied(&session), 1);
    assert!(root.temps().is_empty(), "{:?}", root.temps());
}

/// 文件事件簿上的会话：时辰由 `EventLog::stamps_time` 决定是否记录，
/// 记录里的「预期条数」必须与之相符，否则恢复时对不上账。
async fn prepared_on_a_file_log(root: &Root) -> (Session<ziyuan_storage::EventLog>, TaskId) {
    let mut session = Session::found(ziyuan_storage::EventLog::new(root.path())).unwrap();
    let seal = Seal {
        capabilities: [Capability::WriteFile].into(),
        scope: vec![".".into()],
        mcp: Default::default(),
    };
    let task = session
        .issue(Department::Works, "改 a.txt".into(), seal)
        .unwrap()
        .id;
    session
        .run(
            &[task],
            &scheduler(Rewrite {
                before: Some(ORIGINAL),
                after: CHANGED,
            }),
            &CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    (session, task)
}

#[tokio::test]
async fn a_file_event_log_records_the_clock_and_the_batch_agrees_with_the_record() {
    // 真实事件簿：本批会多记一条时辰，「预期条数」须把它算进去，而不是硬写 +2。
    // 记错了，`text recovery finish` 就无法据记录核对事件簿。
    let root = Root::new("file-log-clock");
    let (mut session, task) = prepared_on_a_file_log(&root).await;
    let clock_before = session
        .history()
        .unwrap()
        .iter()
        .filter(|e| matches!(e, CourtEvent::Clock(_)))
        .count();

    // 看一眼记录发布前的预期：正是本事务要写出的那一批。
    session
        .apply_reviewed(task, root.path(), Vec::new(), None)
        .unwrap();

    let history = session.history().unwrap();
    let clock_after = history
        .iter()
        .filter(|e| matches!(e, CourtEvent::Clock(_)))
        .count();
    // 开国时已记过一条时辰，本批在同分钟内，故不应再记——本批只有申请那一件事。
    assert_eq!(clock_before, 1, "开国时记过一条时辰");
    assert_eq!(clock_after, clock_before, "同一分钟内的批次不应再记时辰");
    // 盘上事件簿与内存一致，说明记录里 pinned 的批次就是真实写出的批次。
    let on_disk =
        ziyuan_storage::EventStore::load(&ziyuan_storage::EventLog::new(root.path())).unwrap();
    assert_eq!(on_disk.len(), history.len(), "盘上事件簿条数应与内存一致");
    assert_eq!(root.generation(), 2);
    assert!(!root.path().join(".ziyuan/recovery.json").exists());
}

#[tokio::test]
async fn a_file_event_log_does_not_stamp_the_clock_twice_for_one_batch() {
    // 连做两次准奏：同一分钟内第二次不应再记时辰，两批长度随之不同。
    // 记录若按「最多 +2」硬算，其中一批就会与实际对不上。
    let root = Root::new("file-log-twice");
    let (mut session, task) = prepared_on_a_file_log(&root).await;
    session
        .apply_reviewed(task, root.path(), Vec::new(), None)
        .unwrap();
    // 第一次准奏已收尾：世代号回到偶数，盘上不留记录。
    assert_eq!(root.generation(), 2);
    assert!(!root.path().join(".ziyuan/recovery.json").exists());

    let seal = Seal {
        capabilities: [Capability::WriteFile].into(),
        scope: vec![".".into()],
        mcp: Default::default(),
    };
    let second = session
        .issue(Department::Works, "再改 a.txt".into(), seal)
        .unwrap()
        .id;
    let before_second = session.history().unwrap().len();
    session
        .run(
            &[second],
            &scheduler(Rewrite {
                before: Some(CHANGED),
                after: CHANGED_TWICE,
            }),
            &CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    session
        .apply_reviewed(second, root.path(), Vec::new(), None)
        .unwrap();

    assert_eq!(root.read(), CHANGED_TWICE);
    let history = session.history().unwrap();
    let second_growth = history.len() - before_second;
    // 执行任务本身也会记事件（入奏、呈报、办结），故不看总长度，只看准奏那批的尾巴：
    // 受管写入写出的批次至多是「一条时辰 + 一条 ChangesApplied」。
    assert!(
        matches!(history.last(), Some(CourtEvent::ChangesApplied { .. })),
        "最后一条应是本批的 ChangesApplied"
    );
    // 盘上事件簿的条数与内存一致：记录钉死的时辰批次就是实际写出的那一批。
    let on_disk =
        ziyuan_storage::EventStore::load(&ziyuan_storage::EventLog::new(root.path())).unwrap();
    assert_eq!(on_disk.len(), history.len(), "盘上事件簿条数应与内存一致");
    assert!(second_growth >= 1, "{second_growth}");
    assert_eq!(root.generation(), 4);
    assert!(!root.path().join(".ziyuan/recovery.json").exists());
}

#[tokio::test]
async fn the_runtime_proposes_before_anything_is_written() {
    // 呈报与落盘是两件事：执行完项目仍是原文，也不推进世代号。
    let root = Root::new("proposal");
    let (_, task) = proposed_only(
        Rewrite {
            before: Some(ORIGINAL),
            after: CHANGED,
        },
        "改 a.txt",
    )
    .await;
    assert_eq!(root.read(), ORIGINAL);
    assert_eq!(root.generation(), 0);
    assert_eq!(task.0, 1);
}

#[tokio::test]
async fn file_write_failure_leaves_the_published_record_and_blocks_the_next_write() {
    let root = Root::new("write-failure");
    root.make_readonly();
    let (mut session, task) = prepared().await;

    let message = error(session.apply_reviewed(task, root.path(), Vec::new(), None));

    // 文件未写、事件未记，留下一份「已发布」的记录与奇数世代号。
    assert_eq!(root.read(), ORIGINAL, "只读目标不得被改写");
    assert_eq!(applied(&session), 0, "落盘失败不得记事件");
    assert_eq!(root.generation() % 2, 1, "失败的落盘须留下奇数世代号");
    let record = root.record().expect("失败后须留下恢复记录");
    assert_eq!(record.state, RecordState::Published);
    assert!(
        record.operation_id.len() == 32
            && record
                .operation_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{}",
        record.operation_id
    );
    assert_eq!((record.generation_before, record.generation_after), (0, 2));
    assert_eq!(record.application.task, task.0);
    assert_eq!(
        record.targets,
        [TargetState {
            path: "a.txt".into(),
            // printf 'old\nkeep\n' | sha256sum
            before: FileState::Present(
                "93862b8f30e3b375e8a4ebfc514bab781f8e817fb28cc8e6774c4d116e73f0cd".into(),
            ),
            after: FileState::Present(crate::translation::sha256_hex(CHANGED.as_bytes())),
        }]
    );
    assert!(!record.events.encoded_events.is_empty());
    // 首次失败的原话就是文件写不进去，不是「须先收尾」。
    assert!(!message.contains("未收尾"), "{message}");

    // 再次准奏被拒，并点名须先收尾的操作；文件与世代号一个字节也不动。
    let message = error(session.apply_reviewed(task, root.path(), Vec::new(), None));
    assert!(
        message.contains(&record.operation_id) && message.contains("未收尾"),
        "{message}"
    );
    assert_eq!(root.read(), ORIGINAL);
    assert_eq!(root.generation() % 2, 1);
    assert!(root.temps().is_empty(), "{:?}", root.temps());

    // 收尾后恢复可写，且能重新落盘。
    finish_on_disk(&root, &record.operation_id);
    root.make_writable();
    session
        .apply_reviewed(task, root.path(), Vec::new(), None)
        .unwrap();
    assert_eq!(root.read(), CHANGED);
    assert_eq!(root.generation(), 4, "收尾进 1、本次落盘再进 2");
    assert!(!root.path().join(".ziyuan/recovery.json").exists());
    assert!(root.temps().is_empty(), "{:?}", root.temps());
}

#[tokio::test]
async fn a_stale_target_is_rejected_before_anything_is_published() {
    // 写前就能断定的失败（原文已被改动）不该留下恢复记录：否则一次本来就没写的准奏
    // 会把工作区锁死，须人工收尾。
    let root = Root::new("stale");
    let (mut session, task) = prepared().await;
    std::fs::write(root.path().join("a.txt"), "他人改动\n").unwrap();

    let message = error(session.apply_reviewed(task, root.path(), Vec::new(), None));
    assert!(message.contains("已被改动"), "{message}");
    assert_eq!(root.read(), "他人改动\n");
    assert!(root.record().is_none(), "未动文件即不得留下恢复记录");
    assert_eq!(root.generation(), 0, "未动文件即不得推进世代号");
    assert_eq!(applied(&session), 0);

    // 改回原文后照常准奏，无需任何收尾。
    std::fs::write(root.path().join("a.txt"), ORIGINAL).unwrap();
    session
        .apply_reviewed(task, root.path(), Vec::new(), None)
        .unwrap();
    assert_eq!(root.read(), CHANGED);
    assert_eq!(root.generation(), 2);
    assert!(root.record().is_none());
}

#[tokio::test]
async fn a_pending_record_from_another_operation_blocks_before_any_write() {
    let root = Root::new("pending");
    let (mut session, task) = prepared().await;

    let ws = root.workspace();
    let record = foreign_record(ws.root(), task);
    let operation = record.operation_id.clone();
    let mut guard = ws.lock(LockScope::Workspace).unwrap();
    guard.publish(&record).unwrap();
    guard.reserve().unwrap();
    drop(guard);
    let (bytes, generation) = (
        std::fs::read(root.path().join("a.txt")).unwrap(),
        root.generation(),
    );

    let message = error(session.apply_reviewed(task, root.path(), Vec::new(), None));
    assert!(message.contains(&operation), "{message}");
    assert!(message.contains("未收尾"), "{message}");
    assert_eq!(
        std::fs::read(root.path().join("a.txt")).unwrap(),
        bytes,
        "被拒时有待收尾的记录，一个字节也不写"
    );
    assert_eq!(root.generation(), generation, "被拒的写入不得推进世代号");
    assert_eq!(applied(&session), 0);

    finish_on_disk(&root, &operation);
    session
        .apply_reviewed(task, root.path(), Vec::new(), None)
        .unwrap();
    assert_eq!(root.read(), CHANGED);
    assert!(root.temps().is_empty(), "{:?}", root.temps());
}

/// 在 `dir` 上准奏一件改动；撞上「另一进程正在写入」的拒绝时，等它收尾后换一份新会话重来。
///
/// 受管写入在**锁外**就先查一次 `ensure_writable`，撞上另一进程刚发布的记录或它留下的奇数
/// 世代号时会立刻拒绝——那是设计如此（那一刻写下去就会与对方的收尾相撞），真实用户看到的
/// 就是「请稍候重试」。所以拒绝不算失败：只要工作区此刻仍不可写，就等对方收尾再来一次；
/// 工作区可写却仍失败，说明是别的毛病（原文被改动、路径越界），原样报出。
fn approve_with_retry(dir: &Path, text: &'static str, change: FileChange) -> Result<(), String> {
    let mut last = String::new();
    // 20ms 一次、最多 2 秒：足够覆盖对方的写入与收尾，也不会把真正的卡死拖成超时。
    for _ in 0..100 {
        let mut store = MemoryEventLog::default();
        let mut session = Session::found(&mut store).unwrap();
        let seal = Seal {
            capabilities: [Capability::WriteFile].into(),
            scope: vec![".".into()],
            mcp: Default::default(),
        };
        let task = session
            .issue(Department::Works, text.into(), seal)
            .unwrap()
            .id;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime
            .block_on(session.run(
                &[task],
                &scheduler(Specific(change.clone())),
                &CancelToken::new(),
                |_| {},
            ))
            .unwrap();
        match session.apply_reviewed(task, dir, Vec::new(), None) {
            Ok(_) => return Ok(()),
            Err(error) => {
                // 只有「工作区还有未收尾的写入」才是可重试的拒绝。
                let retryable = Workspace::open(dir)
                    .map(|workspace| workspace.ensure_writable().is_err())
                    .unwrap_or(false);
                if !retryable {
                    return Err(error.to_string());
                }
                last = error.to_string();
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(format!("等对方收尾等了 2 秒仍未成功：{last}"))
}

#[tokio::test]
async fn concurrent_approvals_on_two_tasks_never_interleave() {
    // 两个线程各持自己的会话，同时对同一根目录准奏，无论次序如何，文件都只会是两件改动
    // 各自的结果，绝不会是两者的混合。
    //
    // 这里必须容忍「被拒后重试」：撞上另一个进程刚发布的记录或它留下的奇数世代号时，
    // 受管写入会立刻拒绝（见 `approve_with_retry`），而那是设计行为，不是缺陷。
    let root = std::sync::Arc::new(Root::new("concurrent"));
    // 两件不同的改动：一件把 a.txt 改为 CHANGED，一件新建 b.txt。
    let dir = root.path().to_path_buf();
    let open = |text: &'static str, change: FileChange, name: &'static str| {
        let dir = dir.clone();
        std::thread::spawn(move || (name, approve_with_retry(&dir, text, change)))
    };
    // 读者看到的始终是「原文」或「改后内容」之一；b.txt 只在第二件成功时出现。
    let a = open(
        "改 a.txt",
        FileChange {
            path: "a.txt".into(),
            before: Some(ORIGINAL.into()),
            after: CHANGED.into(),
        },
        "a",
    );
    let b = open(
        "新建 b.txt",
        FileChange {
            path: "b.txt".into(),
            before: None,
            after: "b\n".into(),
        },
        "b",
    );
    let results = [a.join().unwrap(), b.join().unwrap()];
    let successes = results.iter().filter(|(_, r)| r.is_ok()).count();
    assert_eq!(successes, 2, "{results:?}");
    assert_eq!(root.read(), CHANGED);
    assert_eq!(
        std::fs::read_to_string(root.path().join("b.txt")).unwrap(),
        "b\n"
    );
    assert_eq!(root.generation() % 2, 0);
    assert!(root.record().is_none(), "两次都收尾成功，不留记录");
    assert!(root.temps().is_empty(), "{:?}", root.temps());
}

/// 确定性版本：盘上摆好一条「已发布、未收尾」的记录，准奏的头一次必被拒；等另一进程收尾后
/// 重试即成功。
///
/// 上一条并发用例撞不撞得上这种拒绝要看调度，所以这里用 `foreign_record` 把现场摆死，
/// 把「等对方收尾、再换新会话重来」这条路确定地走一遍——`approve_with_retry` 的可重试判据
/// 一旦写错（比如恒判为不可重试），本用例立刻失败。
#[test]
fn approval_retries_until_the_other_process_finishes() {
    let root = std::sync::Arc::new(Root::new("retry-after-other"));
    let ws = root.workspace();
    let record = foreign_record(ws.root(), TaskId(7));
    let operation = record.operation_id.clone();
    let mut guard = ws.lock(LockScope::Workspace).unwrap();
    guard.publish(&record).unwrap();
    guard.reserve().unwrap();
    drop(guard);
    // 头一次准奏必定被拒：记录已发布、世代号为奇数。
    assert!(root.workspace().ensure_writable().is_err());

    let finisher = {
        let root = std::sync::Arc::clone(&root);
        let operation = operation.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            finish_on_disk(&root, &operation);
        })
    };
    let result = approve_with_retry(
        root.path(),
        "改 a.txt",
        FileChange {
            path: "a.txt".into(),
            before: Some(ORIGINAL.into()),
            after: CHANGED.into(),
        },
    );
    finisher.join().unwrap();
    result.unwrap();

    assert_eq!(root.read(), CHANGED);
    assert_eq!(
        root.generation() % 2,
        0,
        "收尾与被拒的重试都不得留下奇数世代号"
    );
    assert!(root.record().is_none(), "重试成功后不留记录");
    assert!(root.temps().is_empty(), "{:?}", root.temps());
}

/// 只呈报指定的那一处改动。
struct Specific(FileChange);
impl WorkerBackend for Specific {
    fn execute(&self, _job: &Job, hooks: &dyn ExecutionHooks) -> Result<String, BoxError> {
        hooks.changes_proposed(vec![self.0.clone()]);
        Ok("改毕".into())
    }
}

#[tokio::test]
async fn a_foreign_process_holding_the_lock_makes_approval_time_out() {
    let root = Root::new("lock");
    let (mut session, task) = prepared().await;
    let mut child = LockChild::holding(root.path());

    let started = Instant::now();
    let message = error(session.apply_reviewed(task, root.path(), Vec::new(), None));
    // 默认时限 5 秒；这里只断言确实等过锁，且拒绝时未写任何东西。
    assert!(started.elapsed() >= Duration::from_secs(4), "{message}");
    assert_eq!(root.read(), ORIGINAL);
    assert_eq!(root.generation(), 0, "未取得锁即不得推进世代号");
    assert!(root.record().is_none(), "未取得锁即不得写记录");
    assert_eq!(applied(&session), 0);

    child.release(root.path());
}

/// 子进程占住工作区写锁，直到父进程放下 `release` 文件。
struct LockChild(std::process::Child);
impl LockChild {
    fn holding(root: &Path) -> Self {
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "write_txn_tests::child_holds_the_workspace_lock",
                "--nocapture",
            ])
            .env("ZIYUAN_WRITE_TXN_LOCK_ROOT", root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut child = Self(child);
        let deadline = Instant::now() + Duration::from_secs(15);
        while !root.join("locked").exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "child exited before locking"
            );
            assert!(Instant::now() < deadline, "child failed to take the lock");
            std::thread::sleep(Duration::from_millis(10));
        }
        child
    }

    fn release(&mut self, root: &Path) {
        std::fs::write(root.join("release"), "").unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(Instant::now() < deadline, "child failed to exit");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for LockChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn managed_write_sweeps_a_stale_temp_next_to_the_target() {
    let root = Root::new("sweep-stale");
    // 已死写入者的残留：一份合法租约，加上它认领的临时文件，与目标同目录。
    let stale = root.path().join(".ziyuan-tmp-1-1-1");
    std::fs::write(&stale, "partial").unwrap();
    std::fs::write(
        root.path()
            .join(".ziyuan-lease-0123456789abcdef0123456789abcdef"),
        "ziyuan-lease v1 temp=.ziyuan-tmp-1-1-1 \
         id=0123456789abcdef0123456789abcdef\n",
    )
    .unwrap();

    let (mut session, task) = prepared().await;
    session
        .apply_reviewed(task, root.path(), Vec::new(), None)
        .unwrap();

    assert_eq!(root.read(), CHANGED);
    assert!(!stale.exists(), "写入前应清掉目标目录里的陈旧临时文件");
    assert!(root.temps().is_empty(), "{:?}", root.temps());
    let leases: Vec<String> = std::fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !leases.iter().any(|name| name.starts_with(".ziyuan-lease-")),
        "{leases:?}"
    );
}

#[tokio::test]
async fn managed_write_leaves_other_directories_alone() {
    // 同名的陈旧残留若不在本次写入目标所在目录，就不归本轮清理：自动清理只碰目标目录。
    // 本条只有否定的一半（没清就是对的）；正向的一半由
    // `managed_write_sweeps_a_stale_temp_next_to_the_target` 与运行库的
    // `apply_changes_leaves_other_directories_alone` 提供，故此处不另设正控。
    let root = Root::new("sweep-elsewhere");
    let elsewhere = root.path().join("docs");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let stale = elsewhere.join(".ziyuan-tmp-1-1-1");
    std::fs::write(&stale, "partial").unwrap();
    let lease = elsewhere.join(".ziyuan-lease-0123456789abcdef0123456789abcdef");
    std::fs::write(
        &lease,
        "ziyuan-lease v1 temp=.ziyuan-tmp-1-1-1 \
         id=0123456789abcdef0123456789abcdef\n",
    )
    .unwrap();

    let (mut session, task) = prepared().await;
    session
        .apply_reviewed(task, root.path(), Vec::new(), None)
        .unwrap();

    assert_eq!(root.read(), CHANGED);
    assert!(stale.exists(), "目标目录之外的陈旧临时文件不得被删");
    assert!(lease.exists(), "目标目录之外的孤儿租约不得被删");
}

#[test]
fn child_holds_the_workspace_lock() {
    let Some(root) = std::env::var_os("ZIYUAN_WRITE_TXN_LOCK_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let ws = Workspace::open(&root).unwrap();
    let _guard = ws.lock(LockScope::Workspace).unwrap();
    std::fs::write(root.join("locked"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !root.join("release").exists() {
        assert!(Instant::now() < deadline, "parent never released child");
        std::thread::sleep(Duration::from_millis(10));
    }
}
