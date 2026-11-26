use super::*;

#[test]
fn candidate_names_are_strict() {
    assert!(is_candidate_name(&format!(
        "{TEMP_PREFIX}{}-1-1",
        std::process::id()
    )));
    assert!(is_candidate_name(".ziyuan-tmp-0-0-0"));
    // 前导零合法：协议只要求十进制可解析。
    assert!(is_candidate_name(".ziyuan-tmp-007-0-0"));
    // 各段类型自身的上界必须被接受。
    let maxima = format!("{TEMP_PREFIX}{}-{}-{}", u32::MAX, u128::MAX, u64::MAX);
    assert!(is_candidate_name(&maxima), "{maxima}");
    for bad in [
        // 段数
        ".ziyuan-tmp-1-1",
        ".ziyuan-tmp-1-1-1-1",
        ".ziyuan-tmp-",
        // 空段
        ".ziyuan-tmp--1-1",
        ".ziyuan-tmp-1--1",
        ".ziyuan-tmp-1-1-",
        // 符号与空白：`parse::<u32>()` 会接受 `+1`
        ".ziyuan-tmp-+1-1-1",
        ".ziyuan-tmp-1-+1-1",
        ".ziyuan-tmp- 1-1-1",
        ".ziyuan-tmp-1-1- 1",
        // 非 ASCII 数字
        ".ziyuan-tmp-１-1-1",
        // 额外后缀与空白
        ".ziyuan-tmp-1-1-1.bak",
        ".ziyuan-tmp-1-1-1 ",
        // 前缀相似
        ".ziyuan-tmpx-1-1-1",
        "ziyuan-tmp-1-1-1",
        ".ziyuan-lease-0123456789abcdef0123456789abcdef",
    ] {
        assert!(!is_candidate_name(bad), "{bad:?}");
    }
    // 各段上界再 +1 必须被拒绝。u128 无法在类型内加一，故直接写 `u128::MAX + 1` 的字面量。
    for overflow in [
        format!("{TEMP_PREFIX}{}-1-1", u32::MAX as u64 + 1),
        format!(
            "{TEMP_PREFIX}1-{}-1",
            "340282366920938463463374607431768211456"
        ),
        format!("{TEMP_PREFIX}1-1-{}", u64::MAX as u128 + 1),
    ] {
        assert!(!is_candidate_name(&overflow), "{overflow}");
    }
}

#[test]
fn fresh_names_are_candidates_and_distinct() {
    let first = fresh_temp_name();
    let second = fresh_temp_name();
    assert!(is_candidate_name(&first), "{first}");
    assert!(is_candidate_name(&second), "{second}");
    assert_ne!(first, second);
}

fn scratch(tag: &str) -> std::path::PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ziyuan-temp-cleanup-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn lease_paths(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(super::LEASE_PREFIX))
        })
        .collect();
    paths.sort();
    paths
}

#[test]
fn drop_removes_an_unreleased_lease() {
    let dir = scratch("lease");
    let name = fresh_temp_name();
    let path = {
        let mut lease = Lease::hold(&dir, &name).unwrap();
        let paths = lease_paths(&dir);
        assert_eq!(paths.len(), 1, "{paths:?}");
        assert_eq!(paths[0], lease.path());
        // Windows 上锁内不能被第二句柄读，所以经持有者自己的句柄读。
        let text = lease.contents().unwrap();
        assert!(text.ends_with('\n'), "{text:?}");
        assert!(text.starts_with("ziyuan-lease v1 "), "{text:?}");
        assert!(text.contains(&format!("temp={name}")), "{text:?}");
        // 第二句柄取不到锁（Windows 上跨句柄互斥成立）。
        #[cfg(windows)]
        {
            let probe = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&paths[0])
                .unwrap();
            assert!(
                matches!(probe.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
                "写入者必须持有租约锁"
            );
        }
        paths[0].clone()
    };
    assert!(!path.exists(), "释放后租约必须消失");
    assert!(lease_paths(&dir).is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn release_removes_the_lease_and_reports_success() {
    let dir = scratch("release-ok");
    let name = fresh_temp_name();
    let lease = Lease::hold(&dir, &name).unwrap();
    let path = lease.path().to_path_buf();
    lease.release().unwrap();
    assert!(!path.exists(), "release 成功后租约必须消失");
    assert!(lease_paths(&dir).is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn release_tolerates_a_lease_already_gone() {
    let dir = scratch("release-gone");
    let lease = Lease::hold(&dir, &fresh_temp_name()).unwrap();
    std::fs::remove_file(lease.path()).unwrap();
    assert!(lease.release().is_ok(), "已被删掉的租约不算失败");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn release_reports_a_lease_it_could_not_delete() {
    let dir = scratch("release-fails");
    let name = fresh_temp_name();
    let lease = Lease::hold(&dir, &name).unwrap();
    let path = lease.path().to_path_buf();
    // 让删除必然失败：只读父目录不跨平台，所以经测试钩子在关句柄之后把租约换成目录。
    tests_hooks::arm_swap_to_dir(&path);
    let result = lease.release();
    tests_hooks::clear();
    assert!(result.is_err(), "租约删不掉时必须能报告出来");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn hold_aborts_when_its_name_was_reclaimed_before_the_lock() {
    let dir = scratch("reclaimed");
    let name = fresh_temp_name();
    // 在"写完内容"与"加锁"之间插入一次删除，模拟孤儿租约路径抢先回收。
    tests_hooks::arm_reclaim();
    let result = Lease::hold(&dir, &name);
    tests_hooks::clear();
    let error = result.err().expect("租约被回收后必须放弃本次写入");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted, "{error}");
    assert!(lease_paths(&dir).is_empty());
    assert!(!dir.join(&name).exists(), "放弃时不得建临时文件");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn hold_rejects_names_that_are_not_bare() {
    let dir = scratch("bare");
    // 反斜杠、冒号、NUL、空白在任何平台上都不接受，免得同一份判断在 Unix 上放宽。
    // 空格与制表符也必须拒绝：租约行按空格切字段，名字里带空格会写出自家解析器读不懂的租约。
    for bad in [
        "",
        "..",
        ".",
        "sub/temp",
        "sub\\temp",
        "C:temp",
        "a:b",
        "a b",
        "a\tb",
        "a\nb",
        "a\rb",
        "a\0b",
    ] {
        assert!(Lease::hold(&dir, bad).is_err(), "{bad:?}");
    }
    assert!(lease_paths(&dir).is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_second_creator_never_truncates_an_existing_lease() {
    let dir = scratch("conflict");
    let name = fresh_temp_name();
    let mut first = Lease::hold(&dir, &name).unwrap();
    let path = first.path().to_path_buf();
    let before = first.contents().unwrap();
    // `create_new` 是冲突原语：同名第二次创建必须失败，且不改原内容。
    assert_eq!(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::AlreadyExists
    );
    assert_eq!(first.contents().unwrap(), before);
    drop(first);
    std::fs::remove_dir_all(dir).unwrap();
}

fn empty_protected() -> std::collections::BTreeSet<String> {
    std::collections::BTreeSet::new()
}

fn hex_id(seed: u64) -> String {
    format!("{seed:032x}")
}

/// 已死写入者留下的合法租约：内容对、没人持锁。
fn dead_lease(dir: &std::path::Path, id: &str, temp: &str) {
    std::fs::write(
        dir.join(format!(".ziyuan-lease-{id}")),
        lease_line(temp, id),
    )
    .unwrap();
}

fn write_temp(dir: &std::path::Path, name: &str) {
    std::fs::write(dir.join(name), b"partial").unwrap();
}

/// 设计第 2 步的“列出租约”，测试里由调用方代劳。
fn leases(dir: &std::path::Path) -> Vec<(std::path::PathBuf, String)> {
    lease_entries(dir).unwrap()
}

fn keep_reason(item: &Item) -> Reason {
    match item.outcome {
        Outcome::Keep(reason) => reason,
        Outcome::Remove => panic!("期望保留，实际可删除: {:?}", item.path),
    }
}

#[cfg(unix)]
fn tempfile_target(dir: &std::path::Path) -> std::path::PathBuf {
    let outside = dir.with_extension("outside");
    std::fs::write(&outside, b"keep me").unwrap();
    outside
}

#[test]
fn parse_accepts_the_protocol_line_and_nothing_else() {
    let id = hex_id(7);
    let name = fresh_temp_name();
    // 返回值借用入参，所以先绑到变量上，不能借临时 `String`。
    let line = lease_line(&name, &id);
    let parsed = parse_lease(&line, &id).unwrap();
    assert_eq!(parsed, name);

    let cases = [
        // 版本不是 v1：单独一类原因，不是笼统的不可解析。
        (
            format!("ziyuan-lease v2 temp={name} id={id}\n"),
            Reason::LeaseUnknownVersion,
        ),
        // 顺序错、字段多一个、少一个、重复、缺少行尾换行。
        (
            format!("ziyuan-lease v1 id={id} temp={name}\n"),
            Reason::LeaseUnparsable,
        ),
        (
            format!("ziyuan-lease v1 temp={name} id={id} extra=1\n"),
            Reason::LeaseUnparsable,
        ),
        (
            format!("ziyuan-lease v1 temp={name}\n"),
            Reason::LeaseUnparsable,
        ),
        (
            format!("ziyuan-lease v1 temp={name} id={id} id={id}\n"),
            Reason::LeaseUnparsable,
        ),
        (
            format!("ziyuan-lease v1 temp={name} id={id}"),
            Reason::LeaseUnparsable,
        ),
        // temp 必须是同目录裸文件名。
        (
            format!("ziyuan-lease v1 temp=sub/{name} id={id}\n"),
            Reason::LeaseUnparsable,
        ),
        // id 必须 32 位小写十六进制，且与文件名一致。
        (
            format!("ziyuan-lease v1 temp={name} id={id}\n"),
            Reason::LeaseIdMismatch,
        ),
        (
            format!("ziyuan-lease v1 temp={name} id=ABCDEFABCDEFABCDEFABCDEFABCDEFAB\n"),
            Reason::LeaseUnparsable,
        ),
        // 标签错。
        (
            format!("ziyuan-loan v1 temp={name} id={id}\n"),
            Reason::LeaseUnparsable,
        ),
    ];
    for (text, expected) in cases {
        // `LeaseIdMismatch` 那一行传的实参故意与内容里的 `id=` 不同，验证比对的是文件名 id。
        let file_id = if expected == Reason::LeaseIdMismatch {
            hex_id(9)
        } else {
            id.clone()
        };
        assert_eq!(
            parse_lease(&text, &file_id).unwrap_err(),
            expected,
            "{text:?}"
        );
    }
}

#[test]
fn read_lease_rejects_oversized_and_non_utf8() {
    let dir = scratch("read");
    let path = dir.join(".ziyuan-lease-probe");
    std::fs::write(&path, vec![b'a'; 4097]).unwrap();
    let mut file = std::fs::File::open(&path).unwrap();
    assert_eq!(read_lease(&mut file).unwrap_err(), Reason::LeaseUnparsable);
    // 恰好上限的内容不因超限被拒：读取成功，随后由解析判为不可解析。
    std::fs::write(&path, vec![b'a'; 4096]).unwrap();
    let mut file = std::fs::File::open(&path).unwrap();
    let text = read_lease(&mut file).unwrap();
    assert_eq!(text.len(), 4096);
    assert_eq!(parse_lease(&text, &hex_id(1)), Err(Reason::LeaseUnparsable));
    // 非 UTF-8。
    std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
    let mut file = std::fs::File::open(&path).unwrap();
    assert_eq!(read_lease(&mut file).unwrap_err(), Reason::LeaseUnparsable);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn absent_orphan_lease_is_reported() {
    let dir = scratch("lease-absent");
    let id = hex_id(28);
    let mut problems = Vec::new();
    let item = decide_lease(
        &dir,
        &dir.join(format!(".ziyuan-lease-{id}")),
        &id,
        true,
        &mut problems,
    );
    assert_eq!(keep_reason(&item), Reason::LeaseAlreadyAbsent);
    assert!(problems.is_empty(), "{problems:?}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn protected_relative_path_wins_before_any_io() {
    let dir = scratch("protected");
    let name = fresh_temp_name();
    write_temp(&dir, &name);
    // 受保护集合命中时，即使目录里没有任何租约也报 RecoveryReferenced 而非
    // TempWithoutLease —— 说明判断发生在打开任何句柄之前。
    let mut protected = empty_protected();
    protected.insert(name.clone());
    let mut problems = Vec::new();
    let item = decide_candidate(
        &dir,
        &name,
        &name,
        &leases(&dir),
        &protected,
        true,
        &mut problems,
    );
    assert_eq!(keep_reason(&item), Reason::RecoveryReferenced);
    assert!(dir.join(&name).exists(), "受保护项绝不能被删");
    assert!(problems.is_empty());

    // 只比文件名会把嵌套候选当成普通候选：这里断言正确形式 `sub/<name>` 生效。
    let sub = dir.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    write_temp(&sub, &name);
    let relative = format!("sub/{name}");
    let mut protected = empty_protected();
    protected.insert(relative.clone());
    let item = decide_candidate(
        &sub,
        &name,
        &relative,
        &leases(&sub),
        &protected,
        true,
        &mut problems,
    );
    assert_eq!(keep_reason(&item), Reason::RecoveryReferenced);
    // 反例：集合里只有裸名时不该命中。
    let mut protected = empty_protected();
    protected.insert(name.clone());
    let item = decide_candidate(
        &sub,
        &name,
        &relative,
        &leases(&sub),
        &protected,
        true,
        &mut problems,
    );
    assert_eq!(keep_reason(&item), Reason::TempWithoutLease);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn candidate_type_is_checked_before_leases() {
    let dir = scratch("type");
    let name = fresh_temp_name();
    // 目录形态的候选：不是普通文件，且此时已有一份合法认领租约。
    std::fs::create_dir(dir.join(&name)).unwrap();
    let id = hex_id(11);
    dead_lease(&dir, &id, &name);
    let mut problems = Vec::new();
    let item = decide_candidate(
        &dir,
        &name,
        &name,
        &leases(&dir),
        &empty_protected(),
        true,
        &mut problems,
    );
    assert_eq!(keep_reason(&item), Reason::TempNotRegular);
    assert!(dir.join(&name).is_dir(), "非普通文件绝不能被删");
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn symlinked_candidate_is_never_followed() {
    let dir = scratch("symlink");
    let name = fresh_temp_name();
    let outside = tempfile_target(&dir);
    std::os::unix::fs::symlink(&outside, dir.join(&name)).unwrap();
    let mut problems = Vec::new();
    let item = decide_candidate(
        &dir,
        &name,
        &name,
        &leases(&dir),
        &empty_protected(),
        true,
        &mut problems,
    );
    assert_eq!(keep_reason(&item), Reason::TempNotRegular);
    assert!(outside.exists(), "链接目标不得被删");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn one_claimant_is_removed_with_its_lease() {
    let dir = scratch("claimant");
    let name = fresh_temp_name();
    write_temp(&dir, &name);
    let id = hex_id(13);
    dead_lease(&dir, &id, &name);
    let mut problems = Vec::new();
    let item = decide_candidate(
        &dir,
        &name,
        &name,
        &leases(&dir),
        &empty_protected(),
        true,
        &mut problems,
    );
    assert_eq!(item.outcome, Outcome::Remove);
    assert!(item.acted, "真删掉的项必须标记 acted");
    assert!(problems.is_empty(), "{problems:?}");
    assert!(!dir.join(&name).exists());
    assert!(lease_paths(&dir).is_empty(), "删除后不得留下租约");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn preview_reports_removable_without_deleting() {
    let dir = scratch("preview");
    let name = fresh_temp_name();
    write_temp(&dir, &name);
    let id = hex_id(15);
    dead_lease(&dir, &id, &name);
    let mut problems = Vec::new();
    let item = decide_candidate(
        &dir,
        &name,
        &name,
        &leases(&dir),
        &empty_protected(),
        false,
        &mut problems,
    );
    assert_eq!(item.outcome, Outcome::Remove);
    assert!(!item.acted, "预览不得删除任何东西");
    assert!(dir.join(&name).is_file());
    assert_eq!(lease_paths(&dir).len(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn two_claimants_are_ambiguous() {
    let dir = scratch("ambiguous");
    let name = fresh_temp_name();
    write_temp(&dir, &name);
    dead_lease(&dir, &hex_id(17), &name);
    dead_lease(&dir, &hex_id(18), &name);
    let mut problems = Vec::new();
    let item = decide_candidate(
        &dir,
        &name,
        &name,
        &leases(&dir),
        &empty_protected(),
        true,
        &mut problems,
    );
    assert_eq!(keep_reason(&item), Reason::TempAmbiguous);
    assert!(dir.join(&name).is_file(), "歧义时绝不猜主人");
    assert_eq!(lease_paths(&dir).len(), 2);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn bad_lease_blocks_deletion_of_a_claimed_candidate() {
    let dir = scratch("bad-lease");
    let name = fresh_temp_name();
    write_temp(&dir, &name);
    dead_lease(&dir, &hex_id(19), &name);
    // 同一目录里另有一份读不懂的租约：本轮不删，报它的原因。
    std::fs::write(
        dir.join(format!(".ziyuan-lease-{}", hex_id(20))),
        b"garbage\n",
    )
    .unwrap();
    let mut problems = Vec::new();
    let item = decide_candidate(
        &dir,
        &name,
        &name,
        &leases(&dir),
        &empty_protected(),
        true,
        &mut problems,
    );
    assert_eq!(keep_reason(&item), Reason::LeaseUnparsable);
    assert!(dir.join(&name).is_file());
    assert_eq!(lease_paths(&dir).len(), 2);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn two_claimants_with_a_bad_lease_report_ambiguity() {
    let dir = scratch("ambiguous-bad");
    let name = fresh_temp_name();
    write_temp(&dir, &name);
    dead_lease(&dir, &hex_id(21), &name);
    dead_lease(&dir, &hex_id(22), &name);
    std::fs::write(
        dir.join(format!(".ziyuan-lease-{}", hex_id(23))),
        b"ziyuan-lease v3\n",
    )
    .unwrap();
    let mut problems = Vec::new();
    let item = decide_candidate(
        &dir,
        &name,
        &name,
        &leases(&dir),
        &empty_protected(),
        true,
        &mut problems,
    );
    assert_eq!(keep_reason(&item), Reason::TempAmbiguous);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_busy_lease_keeps_the_candidate() {
    let dir = scratch("busy");
    let name = fresh_temp_name();
    write_temp(&dir, &name);
    let holder = Lease::hold(&dir, &name).unwrap();
    let mut problems = Vec::new();
    let item = decide_candidate(
        &dir,
        &name,
        &name,
        &leases(&dir),
        &empty_protected(),
        true,
        &mut problems,
    );
    // 两个平台都会跨句柄报出冲突：Windows 靠共享/锁机制，Unix 上 `File::try_lock`
    // 是 `flock(2)`、锁属于打开文件描述，同进程两次独立 `open(2)` 也互斥，第二次拿
    // `EWOULDBLOCK`。Windows 做不到的是阻止不配合的进程重命名或删除已加锁的文件，
    // 那是设计的“已知限制”，与这条断言无关。
    assert_eq!(keep_reason(&item), Reason::LeaseBusy);
    drop(holder);
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(windows)]
#[test]
fn a_deleting_lease_is_never_an_io_failure() {
    // Windows 的“删除中”窗口：名字已经被删、删它的一方还握着句柄，于是它既不在
    // `NotFound` 的意义上不存在（`stat` 未必报 `NotFound`），又打不开
    // （`ERROR_ACCESS_DENIED`）。窗口落在 `stat` 还是 `open` 上视时机而定，结论
    // 都一样：这一轮用不上这份租约，保留候选，**不记 I/O 失败**——记了会让一次干净
    // 的并发清理随机退 1。具体保留原因因此不写死：`LeaseBusy`（打不开）与
    // `TempWithoutLease`（`stat` 就读不出这份租约）都符合契约。
    let (dir, name, _id) = sweep_fixture("lease-delete-pending");
    let lease = lease_paths(&dir).pop().expect("夹具里应当有一份租约");
    // std 打开时带 `FILE_SHARE_DELETE`，所以这句删除让名字立刻进入删除中，直到
    // 句柄关掉才真正消失——正是另一个清理者正在删时的那段窗口。
    let held = File::open(&lease).unwrap();
    std::fs::remove_file(&lease).unwrap();
    let report = sweep_dir(&dir, "", &empty_protected(), true);
    assert!(report.problems.is_empty(), "{report:?}");
    assert!(
        !report.needs_attention(),
        "并发删除中的租约不是失败: {report:?}"
    );
    assert!(dir.join(&name).is_file(), "读不出归属证据时绝不删候选");
    assert!(
        matches!(temp_item(&report, &name).outcome, Outcome::Keep(_)),
        "{report:?}"
    );
    drop(held);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn lease_that_is_not_a_file_is_never_followed() {
    let dir = scratch("lease-dir");
    let name = fresh_temp_name();
    write_temp(&dir, &name);
    let id = hex_id(24);
    // 目录冒充租约：必须报 LeaseNotRegular 而不是当作“没有租约”去删候选。
    std::fs::create_dir(dir.join(format!(".ziyuan-lease-{id}"))).unwrap();
    let mut problems = Vec::new();
    let item = decide_candidate(
        &dir,
        &name,
        &name,
        &leases(&dir),
        &empty_protected(),
        true,
        &mut problems,
    );
    assert_eq!(keep_reason(&item), Reason::LeaseNotRegular);
    assert!(dir.join(&name).is_file(), "读不懂的归属证据必须保护候选");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn orphan_lease_is_reclaimed_only_when_applying() {
    let dir = scratch("orphan");
    let gone = fresh_temp_name();
    let id = hex_id(25);
    dead_lease(&dir, &id, &gone);
    let mut problems = Vec::new();
    let preview = decide_lease(
        &dir,
        &dir.join(format!(".ziyuan-lease-{id}")),
        &id,
        false,
        &mut problems,
    );
    assert_eq!(preview.outcome, Outcome::Remove);
    assert!(!preview.acted);
    assert_eq!(lease_paths(&dir).len(), 1, "预览不得删孤儿租约");
    let applied = decide_lease(
        &dir,
        &dir.join(format!(".ziyuan-lease-{id}")),
        &id,
        true,
        &mut problems,
    );
    assert_eq!(applied.outcome, Outcome::Remove);
    assert!(applied.acted);
    assert!(lease_paths(&dir).is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn orphan_lease_with_a_live_temp_is_kept() {
    let dir = scratch("orphan-live");
    let name = fresh_temp_name();
    write_temp(&dir, &name);
    let id = hex_id(26);
    dead_lease(&dir, &id, &name);
    let mut problems = Vec::new();
    let item = decide_lease(
        &dir,
        &dir.join(format!(".ziyuan-lease-{id}")),
        &id,
        true,
        &mut problems,
    );
    assert_eq!(keep_reason(&item), Reason::TempStillPresent);
    assert_eq!(lease_paths(&dir).len(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn candidate_vanishing_before_delete_is_not_an_error() {
    let dir = scratch("vanish");
    let name = fresh_temp_name();
    write_temp(&dir, &name);
    let id = hex_id(27);
    dead_lease(&dir, &id, &name);
    tests_hooks::arm_vanish(&dir.join(&name));
    let mut problems = Vec::new();
    let item = decide_candidate(
        &dir,
        &name,
        &name,
        &leases(&dir),
        &empty_protected(),
        true,
        &mut problems,
    );
    tests_hooks::clear();
    assert_eq!(keep_reason(&item), Reason::TempAlreadyAbsent);
    assert!(problems.is_empty(), "已不存在不是失败");
    assert_eq!(
        lease_paths(&dir).len(),
        1,
        "候选没了就不动租约，留给孤儿路径"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

fn temp_item<'a>(report: &'a Report, name: &str) -> &'a Item {
    report
        .items
        .iter()
        .find(|item| {
            item.kind == Kind::Temp
                && item
                    .path
                    .file_name()
                    .is_some_and(|file| file.to_string_lossy() == name)
        })
        .unwrap_or_else(|| panic!("没有 {name} 的报告项: {report:?}"))
}

fn sweep_fixture(tag: &str) -> (std::path::PathBuf, String, String) {
    let dir = scratch(tag);
    let name = fresh_temp_name();
    write_temp(&dir, &name);
    let id = hex_id(31);
    dead_lease(&dir, &id, &name);
    (dir, name, id)
}

#[test]
fn sweep_reports_candidates_and_leases_separately() {
    let (dir, name, _id) = sweep_fixture("sweep-counts");
    // 一个普通文件冒充候选形状但不符合严格命名。
    std::fs::write(dir.join(".ziyuan-tmp-not-a-candidate"), b"x").unwrap();
    let report = sweep_dir(&dir, "", &empty_protected(), true);
    let temps = report.counts(Kind::Temp);
    let leases = report.counts(Kind::Lease);
    assert_eq!(temps.candidates, 1, "{report:?}");
    assert_eq!(temps.removable, 1, "{report:?}");
    assert_eq!(temps.removed, 1, "{report:?}");
    assert_eq!(temps.kept, 0, "{report:?}");
    assert_eq!(leases.candidates, 0, "{report:?}");
    // 候选先被删掉，租约随之消失，孤儿路径看不到它。
    assert_eq!(report.removed_total(), 1);
    assert!(report.problems.is_empty(), "{report:?}");
    assert!(!dir.join(&name).exists());
    assert!(lease_paths(&dir).is_empty());
    assert_eq!(report.mode, Mode::Apply);
    assert_eq!(report.root, dir.clone());
    assert_eq!(report.scope(), ".");
    // 删掉的候选在报告里必须带上两个删除原因之一，不能留空。
    let removed = temp_item(&report, &name);
    assert_eq!(removed.outcome, Outcome::Remove, "{report:?}");
    assert_eq!(removed.reason_name(), "WriterDeadLeaseVerified");
    let json = report.to_json();
    let items = json["items"].as_array().unwrap();
    assert!(!items.is_empty(), "{json}");
    assert!(
        items.iter().all(|item| !item["reason"].is_null()),
        "报告里的原因不得为空: {json}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sweep_preview_deletes_nothing_and_reports_removable() {
    let (dir, name, _id) = sweep_fixture("sweep-preview");
    let gone = fresh_temp_name();
    let orphan = hex_id(32);
    dead_lease(&dir, &orphan, &gone);
    let before = lease_paths(&dir).len();
    let report = sweep_dir(&dir, "", &empty_protected(), false);
    assert_eq!(report.mode, Mode::Preview);
    assert_eq!(report.counts(Kind::Temp).removable, 1, "{report:?}");
    assert_eq!(report.counts(Kind::Temp).removed, 0, "{report:?}");
    assert_eq!(report.removed_total(), 0);
    assert!(dir.join(&name).is_file(), "预览不得删候选");
    assert_eq!(lease_paths(&dir).len(), before, "预览不得删孤儿租约");
    // 孤儿租约必须真的被报成可清理项：上面只证明“没删”，一个从不报告孤儿
    // 租约的回归也能通过。库里两份租约：一份认领仍活着的候选（保留），一份孤儿。
    let leases = report.counts(Kind::Lease);
    assert_eq!(leases.candidates, 2, "{report:?}");
    assert_eq!(leases.removable, 1, "{report:?}");
    assert_eq!(leases.removed, 0, "{report:?}");
    assert_eq!(leases.kept, 1, "{report:?}");
    let orphan_item = report
        .items
        .iter()
        .find(|item| item.kind == Kind::Lease && item.outcome == Outcome::Remove)
        .unwrap_or_else(|| panic!("没有可清理的租约报告项: {report:?}"));
    assert_eq!(orphan_item.reason_name(), "OrphanLease");
    assert!(!orphan_item.acted, "预览不得删孤儿租约: {report:?}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sweep_keeps_a_protected_candidate() {
    let (dir, name, _id) = sweep_fixture("sweep-protected");
    let mut protected = empty_protected();
    protected.insert(name.clone());
    let report = sweep_dir(&dir, "", &protected, true);
    assert_eq!(
        keep_reason(temp_item(&report, &name)),
        Reason::RecoveryReferenced
    );
    assert!(dir.join(&name).is_file());
    assert_eq!(report.counts(Kind::Temp).removed, 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sweep_keeps_a_nested_protected_candidate() {
    let root = scratch("sweep-nested-protected");
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let name = fresh_temp_name();
    write_temp(&sub, &name);
    // 受保护集合存的是相对工作区根的路径，所以只有 `join_relative` 把
    // `sub` 拼在前面，这条才能命中。
    let mut protected = empty_protected();
    protected.insert(format!("sub/{name}"));
    let report = sweep_dir(&sub, "sub", &protected, true);
    assert_eq!(report.scope(), "sub", "{report:?}");
    assert_eq!(
        keep_reason(temp_item(&report, &name)),
        Reason::RecoveryReferenced
    );
    assert!(sub.join(&name).is_file(), "受保护的嵌套候选不得被删");
    assert_eq!(report.counts(Kind::Temp).removed, 0, "{report:?}");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn needs_attention_distinguishes_preview_from_failure() {
    // 预览里有可清理项不是失败：报告已逐项标出可清理数，由脚本决定是否加 `--yes`。
    let (dir, _name, _id) = sweep_fixture("sweep-attention-preview");
    let preview = sweep_dir(&dir, "", &empty_protected(), false);
    assert_eq!(preview.counts(Kind::Temp).removable, 1, "{preview:?}");
    assert!(!preview.needs_attention(), "{preview:?}");
    std::fs::remove_dir_all(dir).unwrap();

    // 目录没扫全就是失败，哪怕一个项都没判出来。
    let root = scratch("sweep-attention-unscanned");
    let not_a_dir = root.join("regular.txt");
    std::fs::write(&not_a_dir, b"x").unwrap();
    let unscanned = sweep_dir(&not_a_dir, "", &empty_protected(), true);
    assert!(!unscanned.unscanned.is_empty(), "{unscanned:?}");
    assert!(unscanned.needs_attention(), "{unscanned:?}");
    std::fs::remove_dir_all(root).unwrap();

    // 空目录扫得干干净净，也不是失败。
    let empty = scratch("sweep-attention-empty");
    let clean = sweep_dir(&empty, "", &empty_protected(), true);
    assert!(clean.items.is_empty(), "{clean:?}");
    assert!(!clean.needs_attention(), "{clean:?}");
    std::fs::remove_dir_all(empty).unwrap();
}

#[test]
fn report_never_serializes_an_absolute_path() {
    let dir = scratch("sweep-absolute");
    let mut report = Report::new(Mode::Preview, &dir, "", true);
    // std 的 `fs` 错误今天不带路径，所以这里直接造一条带绝对路径的消息：
    // 契约要在出口处成立，而不是指望恰好没有错误带路径。
    report.problems.push(Problem {
        path: dir.join("leak.txt"),
        message: format!("删除失败：{}", dir.display()),
    });
    let json = report.to_json().to_string();
    let root = dir.display().to_string();
    // 两种形态都要断言：JSON 文本里反斜杠是转义的，只查单反斜杠形态在 Windows
    // 上永远为真（等于没查）。
    assert!(!json.contains(&root), "绝对根漏进报告: {json}");
    let escaped_root = root.replace('\\', r"\\");
    assert!(!json.contains(&escaped_root), "绝对根漏进报告: {json}");
    // JSON 文本里的 `\\` 是一个反斜杠：路径一律 `/` 分隔，连转义形态都不该有。
    assert!(!json.contains(r"\\"), "报告里不该有反斜杠路径: {json}");
    assert!(!json.contains("//"), "报告里不该有 `//` 前缀: {json}");
    // 盘符形态（`X:`）也不该出现。
    assert!(
        !json
            .as_bytes()
            .windows(2)
            .any(|window| window[0].is_ascii_alphabetic() && window[1] == b':'),
        "报告里出现了盘符形态: {json}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sweep_records_a_directory_it_cannot_list() {
    let dir = scratch("sweep-unscanned");
    let not_a_dir = dir.join("regular.txt");
    std::fs::write(&not_a_dir, b"x").unwrap();
    let report = sweep_dir(&not_a_dir, "", &empty_protected(), true);
    assert_eq!(report.unscanned, vec![not_a_dir.clone()], "{report:?}");
    assert!(report.items.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sweep_never_reports_an_empty_directory_as_failure() {
    let dir = scratch("sweep-empty");
    let report = sweep_dir(&dir, "", &empty_protected(), true);
    assert!(report.items.is_empty());
    assert!(report.problems.is_empty());
    assert!(report.unscanned.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn workspace_sweep_reaches_nested_directories() {
    let dir = scratch("tree");
    let nested = dir.join("src").join("deep");
    std::fs::create_dir_all(&nested).unwrap();
    let target = fresh_temp_name();
    write_temp(&nested, &target);
    let id = hex_id(41);
    dead_lease(&nested, &id, &target);
    let report = sweep_workspace(&dir, &empty_protected(), true);
    assert!(report.recursive);
    assert_eq!(report.counts(Kind::Temp).removed, 1, "{report:?}");
    assert!(!nested.join(&target).exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn workspace_sweep_protects_nested_relative_paths() {
    let dir = scratch("tree-protected");
    let nested = dir.join("sub");
    std::fs::create_dir_all(&nested).unwrap();
    let name = fresh_temp_name();
    write_temp(&nested, &name);
    let mut protected = empty_protected();
    protected.insert(format!("sub/{name}"));
    let report = sweep_workspace(&dir, &protected, true);
    assert_eq!(
        keep_reason(temp_item(&report, &name)),
        Reason::RecoveryReferenced
    );
    assert!(nested.join(&name).is_file());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn workspace_sweep_skips_control_and_build_directories() {
    let dir = scratch("tree-skip");
    for skipped in [".ziyuan", ".git", ".jj", "target", "node_modules"] {
        let sub = dir.join(skipped);
        std::fs::create_dir_all(&sub).unwrap();
        let name = fresh_temp_name();
        write_temp(&sub, &name);
        dead_lease(&sub, &hex_id(42), &name);
    }
    let report = sweep_workspace(&dir, &empty_protected(), true);
    assert!(report.items.is_empty(), "{report:?}");
    for skipped in [".ziyuan", ".git", ".jj", "target", "node_modules"] {
        assert_eq!(std::fs::read_dir(dir.join(skipped)).unwrap().count(), 2);
    }
    // 跳过不等于“没发生”：报告必须写明跳过了哪些，不能声称扫全了。
    assert_eq!(
        report.skipped,
        vec![
            ".git".to_string(),
            ".jj".to_string(),
            ".ziyuan".to_string(),
            "node_modules".to_string(),
            "target".to_string(),
        ],
        "{report:?}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn workspace_sweep_reports_skipped_paths_not_names() {
    // 两棵不同子树里同名的跳过目录：只报名字的话，去重会把它们并成一条，
    // 报告就说不出“到底哪两处没扫”。
    let root = scratch("tree-skip-paths");
    for sub in ["a", "b"] {
        let dir = root.join(sub).join("target");
        std::fs::create_dir_all(&dir).unwrap();
        let name = fresh_temp_name();
        write_temp(&dir, &name);
        dead_lease(&dir, &hex_id(46), &name);
    }
    let report = sweep_workspace(&root, &empty_protected(), true);
    assert!(report.items.is_empty(), "{report:?}");
    assert_eq!(
        report.skipped,
        vec!["a/target".to_string(), "b/target".to_string()],
        "{report:?}"
    );
    assert_eq!(
        report.to_json()["skipped"].as_array().map(Vec::len),
        Some(2),
        "{report:?}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[test]
fn workspace_sweep_reports_an_unencodable_name_as_unscanned() {
    use std::os::windows::ffi::OsStringExt;
    // 名字里放一个孤立代理项：Windows 的文件名是 UTF-16 码元序列，允许这种名字，
    // 而 `OsStr::to_str()` 会失败——正是 `to_str()` 那一支要接住的形态。
    // 期望：进 `unscanned`（这棵子树确实没扫），不进 `problems`（不是失败）。
    let root = scratch("tree-non-utf8");
    let mut wide: Vec<u16> = "legacy-".encode_utf16().collect();
    wide.push(0xD800);
    wide.extend("-.txt".encode_utf16());
    let name = std::ffi::OsString::from_wide(&wide);
    if std::fs::write(root.join(&name), b"x").is_err() {
        // 文件系统不接受这种名字：跳过，不报假失败。
        eprintln!("跳过：这个文件系统不接受孤立代理项的名字");
        std::fs::remove_dir_all(&root).unwrap();
        return;
    }
    let report = sweep_workspace(&root, &empty_protected(), true);
    assert!(report.problems.is_empty(), "{report:?}");
    // 递归与单层两个入口都必须记到：少记一处，报告就声称扫全了。
    assert_eq!(report.unscanned, vec![root.join(&name)], "{report:?}");
    let direct = sweep_dir(&root, "", &empty_protected(), true);
    assert!(direct.problems.is_empty(), "{direct:?}");
    assert_eq!(direct.unscanned, vec![root.join(&name)], "{direct:?}");
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn workspace_sweep_ignores_a_file_named_like_a_skipped_dir() {
    // 名字叫 `target` 的**文件**不是“没进入的目录”：类型判定必须排在跳过判定之前，
    // 否则一个普通文件会被报成“跳过了目录”。它也不符合候选命名，所以两份清单里
    // 都不该有它。
    let root = scratch("tree-target-file");
    std::fs::write(root.join("target"), b"x").unwrap();
    let report = sweep_workspace(&root, &empty_protected(), true);
    assert!(report.skipped.is_empty(), "{report:?}");
    assert!(report.items.is_empty(), "{report:?}");
    assert!(root.join("target").is_file(), "普通文件不得被删");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn workspace_sweep_orders_children_by_name() {
    // 子目录倒着建（`b` 先、`a` 后），报告里的项仍必须 `a` 在前：`children.sort()`
    // 是下探顺序的唯一依据，删掉它，报告顺序就跟着文件系统的返回次序走。
    let root = scratch("tree-order");
    for sub in ["b", "a"] {
        let dir = root.join(sub);
        std::fs::create_dir_all(&dir).unwrap();
        let name = fresh_temp_name();
        write_temp(&dir, &name);
        dead_lease(&dir, &hex_id(47), &name);
    }
    let report = sweep_workspace(&root, &empty_protected(), true);
    let order: Vec<String> = report
        .items
        .iter()
        .filter(|item| item.kind == Kind::Temp)
        .map(|item| report.display_relative(&item.path))
        .collect();
    assert_eq!(order.len(), 2, "{report:?}");
    assert!(
        order[0].starts_with("a/") && order[1].starts_with("b/"),
        "子目录必须按名字下探: {order:?}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn two_cleaners_never_double_count_a_single_candidate() {
    let (dir, name, _id) = sweep_fixture("tree-race");
    // 两个清理者并发扫同一棵树：谁先删都一样，合计只能记一次删除。
    let root = std::sync::Arc::new(dir.clone());
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let root = root.clone();
            std::thread::spawn(move || sweep_workspace(&root, &empty_protected(), true))
        })
        .collect();
    let reports: Vec<Report> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let temps_removed: usize = reports
        .iter()
        .map(|report| report.counts(Kind::Temp).removed)
        .sum();
    assert_eq!(temps_removed, 1, "同一候选只能被记一次删除");
    assert!(!dir.join(&name).exists());
    assert!(lease_paths(&dir).is_empty());
    for report in &reports {
        // 落败的一方把“已被别人删掉”当成功收尾，不报失败。
        assert!(report.problems.is_empty(), "{report:?}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_reused_pid_without_a_lease_is_never_removed() {
    let dir = scratch("tree-reuse");
    // 旧版遗留、无租约：即使名字格式合法也不动（PID 只作诊断）。
    let name = ".ziyuan-tmp-4242-1-1";
    write_temp(&dir, name);
    let report = sweep_workspace(&dir, &empty_protected(), true);
    assert_eq!(
        keep_reason(temp_item(&report, name)),
        Reason::TempWithoutLease
    );
    assert!(dir.join(name).is_file(), "无归属证据的文件绝不删除");
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn workspace_sweep_never_follows_a_linked_directory() {
    let dir = scratch("tree-link");
    let outside = dir.with_extension("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let name = fresh_temp_name();
    write_temp(&outside, &name);
    dead_lease(&outside, &hex_id(43), &name);
    std::os::unix::fs::symlink(&outside, dir.join("link")).unwrap();
    let report = sweep_workspace(&dir, &empty_protected(), true);
    assert!(report.items.is_empty(), "{report:?}");
    assert!(outside.join(&name).is_file(), "链接目标不得被清理");
    std::fs::remove_dir_all(dir).unwrap();
    std::fs::remove_dir_all(outside).unwrap();
}

#[cfg(windows)]
#[test]
fn workspace_sweep_never_follows_a_junction() {
    use std::os::windows::process::CommandExt;
    // 目录 junction 必须在工作区边界上止步：跟进去就会删到工作区之外。
    // 实测（这个工具链）：`symlink_metadata(junction)` 给出
    // `is_dir()=false, is_symlink()=true`，而跟随版的 `metadata()` 给出
    // `is_dir()=true, is_symlink()=false`——所以挡住 junction 的是“用不跟随的
    // 元数据”这件事，换成 `metadata()` 时 `is_symlink()` 也救不回来。这条用例
    // 盯的是结果（外面那份残留原样还在），不是某一半条件。
    let dir = scratch("tree-junction");
    let outside = dir.with_extension("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let name = fresh_temp_name();
    write_temp(&outside, &name);
    dead_lease(&outside, &hex_id(45), &name);
    let link = dir.join("link");
    // 走 `raw_arg`：`arg` 的反斜杠转义 cmd.exe 认不得，路径里有空格时会静默建不出来。
    let created = std::process::Command::new("cmd.exe")
        .raw_arg(format!(
            "/C mklink /J \"{}\" \"{}\"",
            link.display(),
            outside.display()
        ))
        // 成败只看退出码；`mklink` 会往 stdout 打一行，别把它混进测试输出。
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if !created.is_ok_and(|status| status.success()) {
        // 环境里没有 mklink 或不允许建 junction：跳过而不是报假失败。
        eprintln!("跳过：这个环境建不出目录 junction");
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
        return;
    }
    let report = sweep_workspace(&dir, &empty_protected(), true);
    assert!(report.items.is_empty(), "{report:?}");
    assert!(
        outside.join(&name).is_file(),
        "junction 指向的残留不得被清理"
    );
    // junction 自己用 `remove_dir` 摘掉：`remove_dir_all` 在 reparse point 上的
    // 行为不值得依赖，摘错了就顺着删到目标里去了。
    std::fs::remove_dir(&link).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&outside).unwrap();
}

#[test]
fn workspace_sweep_stops_at_the_depth_cap() {
    let root = scratch("tree-depth");
    // 一条 `MAX_DEPTH + 2` 层深的链，每层都留一份可清理的残留（临时文件 + 已死租约）。
    // 有上限才停得住：只靠跳过名单的话，换个名字的产物目录就能让扫描跑上几分钟且毫无输出。
    let mut dir = root.clone();
    let mut chain = Vec::new();
    for depth in 1..=MAX_DEPTH + 2 {
        dir = dir.join(format!("d{depth}"));
        std::fs::create_dir_all(&dir).unwrap();
        let name = fresh_temp_name();
        write_temp(&dir, &name);
        dead_lease(&dir, &hex_id(61), &name);
        chain.push((dir.clone(), name));
    }
    let report = sweep_workspace(&root, &empty_protected(), true);
    // 上限以内都扫了：根自己没有残留，`d1` 到 `d{MAX_DEPTH-1}` 各删一个。
    assert_eq!(
        report.counts(Kind::Temp).removed,
        MAX_DEPTH - 1,
        "{report:?}"
    );
    // 到上限的那一层整个没进去：报告必须写明“没走到这里”，不能保持沉默。
    let expected_cap = (1..=MAX_DEPTH)
        .map(|depth| format!("d{depth}"))
        .collect::<Vec<_>>()
        .join("/");
    assert_eq!(report.capped, vec![expected_cap.clone()], "{report:?}");
    assert_eq!(
        report.to_json()["capped"].as_array().map(Vec::len),
        Some(1),
        "上限必须进 JSON: {report:?}"
    );
    // 上限不是读失败：混进 `unscanned` 会让一棵合法的深目录树每次运行都退 1。
    assert!(report.unscanned.is_empty(), "{report:?}");
    assert!(report.problems.is_empty(), "{report:?}");
    // 上限及更深的层原样留着：没进去就不许说清过。
    for (dir, name) in chain.iter().skip(MAX_DEPTH - 1) {
        assert!(
            dir.join(name).is_file(),
            "超出上限的层不得被清理: {}",
            dir.display()
        );
    }
    assert!(
        !report.needs_attention(),
        "深度上限不是失败，不得让命令退 1: {report:?}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn workspace_sweep_records_a_directory_it_cannot_list_once() {
    // 列举失败的目录只该出现一次：`scan_into` 记一条，`walk` 紧接着的第二次
    // `read_dir` 也必然失败；若那边也记一条，同一目录就会在报告里出现两遍，
    // 读的人会当成两个目录。去重也一并兜住“同目录被两处各记一次”。
    let root = scratch("tree-unscanned");
    let not_a_dir = root.join("regular.txt");
    std::fs::write(&not_a_dir, b"x").unwrap();
    // 直接调 `walk`：去重会把重复项抹平，经入口断言就查不出“下探那一步又记了一条”。
    let mut raw = Report::new(Mode::Apply, &root, "", true);
    walk(&mut raw, &not_a_dir, "", &empty_protected(), true, 0);
    assert_eq!(
        raw.unscanned,
        vec![not_a_dir.clone()],
        "下探那一步不得重记: {raw:?}"
    );
    let report = sweep_workspace(&not_a_dir, &empty_protected(), true);
    assert_eq!(report.unscanned, vec![not_a_dir.clone()], "{report:?}");
    assert_eq!(
        report.to_json()["unscanned"].as_array().map(Vec::len),
        Some(1),
        "{report:?}"
    );
    assert!(report.items.is_empty(), "{report:?}");
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn workspace_sweep_scans_a_linked_root() {
    // 根是用户亲手指的，链接与否都得扫；链接判定只对下探到的子目录生效。
    let real = scratch("tree-linked-root");
    let name = fresh_temp_name();
    write_temp(&real, &name);
    dead_lease(&real, &hex_id(44), &name);
    let link = real.with_extension("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let report = sweep_workspace(&link, &empty_protected(), true);
    assert_eq!(report.counts(Kind::Temp).removed, 1, "{report:?}");
    assert!(!real.join(&name).exists(), "链接根指向的残留必须被清掉");
    std::fs::remove_file(&link).unwrap();
    std::fs::remove_dir_all(real).unwrap();
}

#[test]
fn problems_recorded_once_are_drained_once() {
    let _ = drain_problems();
    let path = std::path::PathBuf::from(".ziyuan-lease-probe");
    note_problem(path.clone(), "删除失败".into());
    let drained = drain_problems();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].path, path);
    assert_eq!(drained[0].message, "删除失败");
    assert!(drain_problems().is_empty(), "取走后不得重复上报");
}
