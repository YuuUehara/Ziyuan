use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn search_records_reports_precedents_with_handler_and_verdict() {
    let record = |id, edict: &str| Record {
        task: ziyuan_core::TaskId(id),
        department: ziyuan_core::Department::Works,
        edict: edict.into(),
        worker: ziyuan_core::AgentId(4),
        handler: "陈工".into(),
        summary: "回奏".repeat(400).into(),
        verdict: "采纳：好".into(),
    };
    let tool = record_search(Arc::new(vec![record(1, "检查除零"), record(2, "整理文档")]));
    let out = tool.execute(r#"{"query":"除零"}"#).unwrap();
    assert!(out.starts_with("#1 工部 承办 #4 陈工（采纳：好）\n诏令：检查除零\n回奏："));
    assert!(!out.contains("整理文档") && out.ends_with('…'), "{out}");
    assert_eq!(
        tool.execute(r#"{"query":"数据库"}"#).unwrap(),
        "实录中无相关前例"
    );
    assert!(tool.execute(r#"{"query":" "}"#).is_err());
}

fn workspace(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ziyuan-tools-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let project = dir.join("project");
    for sub in ["src", "docs", "target", ".ziyuan"] {
        fs::create_dir_all(project.join(sub)).unwrap();
    }
    fs::write(dir.join("secret.txt"), "outside").unwrap();
    fs::write(
        project.join("src/main.rs"),
        "fn main() {\n    unsafe { run() }\n}\n",
    )
    .unwrap();
    fs::write(project.join("docs/a.md"), "unsafe doc\n").unwrap();
    fs::write(project.join("target/gen.rs"), "unsafe { built }").unwrap();
    fs::write(project.join(".env"), "unsafe=1").unwrap();
    fs::write(project.join(".ziyuan/court.log"), "unsafe").unwrap();
    dir
}

fn seal(capabilities: &[Capability], scope: &[&str]) -> Seal {
    Seal {
        capabilities: capabilities.iter().copied().collect(),
        scope: scope.iter().map(|s| s.to_string()).collect(),
        mcp: Default::default(),
    }
}

#[test]
fn a_missing_file_scope_can_be_created_but_nothing_else_written() {
    let root = workspace("changelog");
    let project = root.join("project");
    let seal = Seal {
        capabilities: [Capability::ReadFile, Capability::WriteFile].into(),
        scope: vec!["CHANGELOG.md".into()],
        mcp: Default::default(),
    };
    let sealed = sealed_tools(&project, &seal, None).unwrap();
    let write = sealed
        .tools
        .iter()
        .find(|t| t.spec().name == "write_file")
        .unwrap();
    let read = sealed
        .tools
        .iter()
        .find(|t| t.spec().name == "read_file")
        .unwrap();
    assert!(
        read.execute(r#"{"path":"CHANGELOG.md"}"#).is_err(),
        "尚不存在"
    );
    write
        .execute(r##"{"path":"CHANGELOG.md","content":"# Changelog\n"}"##)
        .unwrap();
    assert!(
        write
            .execute(r#"{"path":"src/main.rs","content":"x"}"#)
            .is_err()
    );
    assert!(
        write
            .execute(r#"{"path":"CHANGELOG.md.bak","content":"x"}"#)
            .is_err()
    );
    let changes = sealed.changes();
    assert_eq!(
        (
            changes.len(),
            changes[0].path.as_str(),
            changes[0].before.clone()
        ),
        (1, "CHANGELOG.md", None)
    );
    assert!(!project.join("CHANGELOG.md").exists(), "只暂存，不落盘");
    // 只有 CHANGELOG.md 可以尚不存在。
    let other = Seal {
        scope: vec!["NEWS.md".into()],
        ..seal.clone()
    };
    assert!(sealed_tools(&project, &other, None).is_err());
    fs::remove_dir_all(root).unwrap();
}

fn tool(root: &Path, seal: &Seal, name: &str) -> Box<dyn Tool> {
    sealed_tools(root, seal, None)
        .unwrap()
        .tools
        .into_iter()
        .find(|t| t.spec().name == name)
        .unwrap()
}

#[test]
fn read_file_stays_inside_root_and_out_of_denied_paths() {
    let dir = workspace("read");
    let root = dir.join("project");
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.join("secret.txt"), root.join("link.txt")).unwrap();
    let read = tool(&root, &Seal::default(), "read_file");

    assert!(
        read.execute(r#"{"path":"src/main.rs"}"#)
            .unwrap()
            .contains("unsafe")
    );
    assert!(read.execute(r#"{"path":"target/gen.rs"}"#).is_ok());
    let outside = dir.join("secret.txt").display().to_string();
    // 符号链接只在 Unix 上建。
    for path in [
        "../secret.txt",
        outside.as_str(),
        "link.txt",
        ".env",
        ".ziyuan/court.log",
    ]
    .into_iter()
    .filter(|p| cfg!(unix) || *p != "link.txt")
    {
        let args = serde_json::json!({ "path": path }).to_string();
        let err = read.execute(&args).unwrap_err().to_string();
        assert!(err.contains("拒绝访问"), "{path}: {err}");
    }
    assert!(read.execute(r#"{"file":"src/main.rs"}"#).is_err());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn search_reports_locations_and_skips_denied_and_build_output() {
    let dir = workspace("search");
    let search = tool(&dir.join("project"), &Seal::default(), "search_code");
    assert_eq!(
        search.execute(r#"{"query":"unsafe"}"#).unwrap(),
        "docs/a.md:1: unsafe doc\nsrc/main.rs:2: unsafe { run() }"
    );
    assert_eq!(search.execute(r#"{"query":"nothing"}"#).unwrap(), "无匹配");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn read_window_reports_line_counts_and_refuses_denied_paths() {
    let dir = workspace("window");
    let root = dir.join("project");
    let read = tool(&root, &Seal::default(), "read_file");
    let window = read
        .execute(r#"{"path":"src/main.rs","offset":2,"lines":1}"#)
        .unwrap();
    assert!(
        window.starts_with("〔第 2–2 行，共 3 行；后面还有〕\n    unsafe { run() }\n"),
        "{window}"
    );
    let tail = read
        .execute(r#"{"path":"src/main.rs","offset":3}"#)
        .unwrap();
    assert!(tail.contains("已到末尾") && tail.contains("}\n"), "{tail}");
    assert!(
        read.execute(r#"{"path":"src/main.rs","offset":9}"#)
            .unwrap_err()
            .to_string()
            .contains("行号超出范围")
    );
    assert!(
        read.execute(r#"{"path":"src/main.rs","offset":0}"#)
            .is_err()
    );
    assert!(read.execute(r#"{"path":"src/main.rs","lines":0}"#).is_err());
    let err = read
        .execute(r#"{"path":".env","offset":1,"lines":1}"#)
        .unwrap_err()
        .to_string();
    assert!(err.contains("拒绝访问"), "{err}");
    let err = read
        .execute(r#"{"path":".ziyuan/court.log","lines":1}"#)
        .unwrap_err()
        .to_string();
    assert!(err.contains("拒绝访问"), "{err}");
    // 不给行窗口时仍是原来的全文，没有行数说明。
    assert_eq!(
        read.execute(r#"{"path":"docs/a.md"}"#).unwrap(),
        "unsafe doc\n"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn search_can_limit_directory_case_and_page_without_leaving_scope() {
    let dir = workspace("search-page");
    let root = dir.join("project");
    let search = tool(&root, &Seal::default(), "search_code");
    let page = search
        .execute(r#"{"query":"unsafe","path":"src","limit":1}"#)
        .unwrap();
    assert!(
        page.starts_with("〔第 1–1 条，共 1 条〕\nsrc/main.rs:2:"),
        "{page}"
    );
    assert!(!page.contains("docs/a.md"), "{page}");
    let folded = search
        .execute(r#"{"query":"UNSAFE","ignore_case":true,"path":"docs"}"#)
        .unwrap();
    assert!(folded.contains("docs/a.md:1:"), "{folded}");
    let next = search
        .execute(r#"{"query":"unsafe","offset":1,"limit":1}"#)
        .unwrap();
    assert!(
        next.contains("第 2–2 条") && next.contains("src/main.rs"),
        "{next}"
    );
    assert!(
        search
            .execute(r#"{"query":"unsafe","path":".env"}"#)
            .unwrap_err()
            .to_string()
            .contains("拒绝访问")
    );
    assert!(
        search
            .execute(r#"{"query":"unsafe","path":".ziyuan"}"#)
            .unwrap_err()
            .to_string()
            .contains("拒绝访问")
    );
    assert!(
        search
            .execute(r#"{"query":"unsafe","path":"target"}"#)
            .unwrap_err()
            .to_string()
            .contains("不搜索构建产物")
    );
    assert!(
        search
            .execute(r#"{"query":"unsafe","path":"../secret.txt"}"#)
            .is_err()
    );
    assert!(search.execute(r#"{"query":"unsafe","limit":0}"#).is_err());
    assert!(search.execute(r#"{"query":"unsafe","offset":-1}"#).is_err());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn replace_text_stages_one_unique_span_and_rejects_ambiguous_or_denied() {
    let dir = workspace("replace");
    let root = dir.join("project");
    let sealed = sealed_tools(
        &root,
        &seal(&[Capability::ReadFile, Capability::WriteFile], &["src"]),
        None,
    )
    .unwrap();
    let find = |name: &str| sealed.tools.iter().find(|t| t.spec().name == name).unwrap();
    let replace = find("replace_text");
    replace
        .execute(r#"{"path":"src/main.rs","old":"unsafe { run() }","new":"run()"}"#)
        .unwrap();
    let read = find("read_file");
    assert!(
        read.execute(r#"{"path":"src/main.rs"}"#)
            .unwrap()
            .contains("run()")
    );
    assert!(
        !read
            .execute(r#"{"path":"src/main.rs"}"#)
            .unwrap()
            .contains("unsafe")
    );
    let changes = sealed.changes();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].path, "src/main.rs");
    assert!(changes[0].before.as_deref().unwrap().contains("unsafe"));
    assert!(changes[0].after.contains("    run()\n"));
    assert_eq!(
        fs::read_to_string(root.join("src/main.rs")).unwrap(),
        "fn main() {\n    unsafe { run() }\n}\n"
    );
    let err = replace
        .execute(r#"{"path":"src/main.rs","old":"n","new":"N"}"#)
        .unwrap_err()
        .to_string();
    assert!(err.contains("须唯一"), "{err}");
    assert!(
        replace
            .execute(r#"{"path":"src/main.rs","old":"不存在的句子","new":"x"}"#)
            .unwrap_err()
            .to_string()
            .contains("找不到")
    );
    assert!(
        replace
            .execute(r#"{"path":".env","old":"unsafe","new":"x"}"#)
            .unwrap_err()
            .to_string()
            .contains("拒绝访问")
    );
    // 新旧相同或 old 为空，也不能绕过禁区和越界。
    for args in [
        r#"{"path":".env","old":"unsafe","new":"unsafe"}"#,
        r#"{"path":".env","old":"","new":""}"#,
        r#"{"path":"../secret.txt","old":"outside","new":"outside"}"#,
        r#"{"path":"../secret.txt","old":"","new":"x"}"#,
    ] {
        let err = replace.execute(args).unwrap_err().to_string();
        assert!(err.contains("拒绝访问"), "{args}: {err}");
        assert!(!err.contains("未写入暂存"), "{args}: {err}");
    }
    assert_eq!(sealed.changes().len(), 1);
    assert!(
        replace
            .execute(r#"{"path":"docs/a.md","old":"unsafe","new":"x"}"#)
            .unwrap_err()
            .to_string()
            .contains("拒绝访问")
    );
    assert!(
        replace
            .execute(r#"{"path":"src/missing.rs","old":"a","new":"b"}"#)
            .is_err()
    );
    let readonly = sealed_tools(&root, &Seal::default(), None).unwrap();
    assert!(
        !readonly
            .tools
            .iter()
            .any(|tool| tool.spec().name == "replace_text")
    );
    let audited = audit_args(
        "replace_text",
        r#"{"path":"src/main.rs","old":"unsafe { run() }","new":"run()"}"#,
    );
    assert!(!audited.contains("unsafe { run() }"), "{audited}");
    assert!(audited.contains("全文见奏本"), "{audited}");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn run_tests_keeps_cargo_failure_names_when_the_assertion_is_not_in_the_tail() {
    let dir = workspace("cargo-fail");
    let root = dir.join("project");
    let mut log = "noise\n".repeat(400);
    log.push_str(
        "thread 'tests::foo' panicked at src/lib.rs:4:5:\n\
         assertion `left == right` failed\n  left: 1\n right: 2\n\n",
    );
    log.push_str(&"padding\n".repeat(2000));
    log.push_str(
        "failures:\n    tests::foo\n\n\
         test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
    );
    fs::write(root.join("fixture.txt"), log).unwrap();
    let show = if cfg!(windows) {
        "type fixture.txt & exit /b 1"
    } else {
        "cat fixture.txt; exit 1"
    };
    let sealed = sealed_tools(&root, &seal(&[Capability::RunTests], &["."]), Some(show)).unwrap();
    let result = sealed.tools[0].execute("{}");
    let output = match result {
        Err(error)
            if error
                .downcast_ref::<crate::haoshe::SandboxUnavailable>()
                .is_some() =>
        {
            RunTests(sealed.sandbox.clone())
                .run(crate::haoshe::Mode::Off)
                .unwrap_err()
                .to_string()
        }
        other => other.unwrap_err().to_string(),
    };
    let (head, note) = output
        .split_once("〔失败测试〕")
        .unwrap_or_else(|| panic!("应附上失败测试名：{output}"));
    assert!(head.contains("未通过"), "{head}");
    assert!(
        !head.contains("left == right"),
        "断言不在输出末尾时仍须单独摘出：{head}"
    );
    assert!(note.contains("- tests::foo"), "{note}");
    assert!(note.contains("left == right"), "{note}");
    // 非 cargo 的失败仍只给末尾，供都察院按工具成败判断。
    let other = sealed_tools(
        &root,
        &seal(&[Capability::RunTests], &["."]),
        Some("exit 3"),
    )
    .unwrap();
    let output = match other.tools[0].execute("{}") {
        Err(error)
            if error
                .downcast_ref::<crate::haoshe::SandboxUnavailable>()
                .is_some() =>
        {
            RunTests(other.sandbox.clone())
                .run(crate::haoshe::Mode::Off)
                .unwrap_err()
                .to_string()
        }
        other => other.unwrap_err().to_string(),
    };
    assert!(output.contains("未通过（退出码 3）"), "{output}");
    assert!(!output.contains("〔失败测试〕"), "{output}");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn list_files_shows_scope_and_staged_files_but_not_denied_or_build_output() {
    let dir = workspace("list");
    let root = dir.join("project");
    let sealed = sealed_tools(
        &root,
        &seal(&[Capability::ReadFile, Capability::WriteFile], &["."]),
        None,
    )
    .unwrap();
    let find = |name: &str| sealed.tools.iter().find(|t| t.spec().name == name).unwrap();
    let list = find("list_files");
    assert_eq!(
        list.execute("{}").unwrap(),
        "docs/a.md（11 字节）\nsrc/main.rs（35 字节）"
    );
    // 暂存的新文件也列出，并注明尚未落盘。
    find("write_file")
        .execute(r#"{"path":"src/new.rs","content":"x"}"#)
        .unwrap();
    assert_eq!(
        list.execute(r#"{"path":"src"}"#).unwrap(),
        "src/main.rs（35 字节）\nsrc/new.rs（暂存新建，1 字节）"
    );
    for path in [".ziyuan", "..", "target/../../secret.txt"] {
        let args = serde_json::json!({ "path": path }).to_string();
        assert!(list.execute(&args).is_err(), "{path}");
    }
    // 范围之外的目录不可列。
    let narrow = tool(
        &root,
        &seal(&[Capability::ReadFile], &["src"]),
        "list_files",
    );
    assert_eq!(narrow.execute("{}").unwrap(), "src/main.rs（35 字节）");
    assert!(narrow.execute(r#"{"path":"docs"}"#).is_err());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn seal_limits_tools_and_scope() {
    let dir = workspace("seal");
    let root = dir.join("project");
    let only_read = sealed_tools(&root, &seal(&[Capability::ReadFile], &["src"]), None)
        .unwrap()
        .tools;
    let names: Vec<_> = only_read.iter().map(|t| t.spec().name).collect();
    assert_eq!(
        names,
        [
            "read_file",
            "list_files",
            "file_outline",
            "vcs_status",
            "vcs_diff",
            "vcs_log"
        ]
    );
    assert!(only_read[0].execute(r#"{"path":"src/main.rs"}"#).is_ok());
    let err = only_read[0]
        .execute(r#"{"path":"docs/a.md"}"#)
        .unwrap_err()
        .to_string();
    assert!(err.contains("拒绝访问"), "{err}");

    // 重叠的范围合并，结果不重复；单个文件也可作为范围。
    let search = tool(
        &root,
        &seal(&[Capability::SearchCode], &["src/main.rs", "src", "./src"]),
        "search_code",
    );
    assert_eq!(
        search.execute(r#"{"query":"unsafe"}"#).unwrap(),
        "src/main.rs:2: unsafe { run() }"
    );

    assert!(
        sealed_tools(&root, &seal(&[], &["."]), None)
            .unwrap()
            .tools
            .is_empty()
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(&dir, root.join("up")).unwrap();
    for bad in [".ziyuan", "missing", "up"] {
        assert!(
            sealed_tools(&root, &seal(&Capability::ALL, &[bad]), None).is_err(),
            "{bad}"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn writes_are_staged_tested_in_a_copy_and_applied_only_without_conflicts() {
    let dir = workspace("write");
    let root = dir.join("project");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&dir, root.join("src/up")).unwrap();
    #[cfg(not(windows))]
    let check = "grep -q staged src/main.rs && test -f src/new/a.rs && test ! -e .env && test ! -d .ziyuan && echo COPY-OK";
    // cmd 的 echo 按本地代码页输出，标记用 ASCII。
    // 不使用 `>nul`：Windows Server 默认不对 AppContainer 开放 \Device\Null 写权限。
    #[cfg(windows)]
    let check = r"findstr /c:staged src\main.rs && if exist src\new\a.rs if not exist .env if not exist .ziyuan echo COPY-OK";
    let sealed = sealed_tools(&root, &seal(&Capability::ALL, &["src"]), Some(check)).unwrap();
    let find = |name: &str| sealed.tools.iter().find(|t| t.spec().name == name).unwrap();
    let (write, read, test) = (find("write_file"), find("read_file"), find("run_tests"));

    let original = fs::read_to_string(root.join("src/main.rs")).unwrap();
    write
        .execute(r#"{"path":"src/main.rs","content":"staged"}"#)
        .unwrap();
    write
        .execute(r#"{"path":"./src/new/a.rs","content":"新"}"#)
        .unwrap();
    // 暂存只在工具内可见，项目文件不变。
    assert_eq!(read.execute(r#"{"path":"src/main.rs"}"#).unwrap(), "staged");
    assert_eq!(read.execute(r#"{"path":"src/new/a.rs"}"#).unwrap(), "新");
    assert_eq!(
        fs::read_to_string(root.join("src/main.rs")).unwrap(),
        original
    );
    assert!(!root.join("src/new").exists());
    for path in [
        "docs/a.md",
        "../secret.txt",
        "src/up/secret.txt",
        "src",
        ".env",
    ]
    .into_iter()
    .filter(|p| cfg!(unix) || *p != "src/up/secret.txt")
    {
        let args = serde_json::json!({ "path": path, "content": "x" }).to_string();
        assert!(write.execute(&args).is_err(), "{path}");
    }

    // 本机没有号舍（或 Windows 外部 CARGO_TARGET_DIR 被 W2 策略拒绝）时 run_tests 返回 SandboxUnavailable；
    // 副本的内容改以 off 照常运行来核对。
    let run_tests = |sealed: &Sealed, tool: &dyn Tool| {
        let result = tool.execute("{}");
        let unavailable = (!crate::haoshe::shell_sandbox_available()
            || (cfg!(windows) && std::env::var_os("CARGO_TARGET_DIR").is_some()))
            && crate::haoshe::Mode::from_env().unwrap() != crate::haoshe::Mode::Off;
        if !unavailable {
            return result;
        }
        let error = result.unwrap_err();
        assert!(
            error
                .downcast_ref::<crate::haoshe::SandboxUnavailable>()
                .is_some(),
            "{error}"
        );
        RunTests(sealed.sandbox.clone()).run(crate::haoshe::Mode::Off)
    };
    let output = run_tests(&sealed, test.as_ref()).unwrap();
    assert!(
        output.contains("通过（退出码 0）") && output.contains("COPY-OK"),
        "{output}"
    );
    let failing = sealed_tools(
        &root,
        &seal(&[Capability::RunTests], &["."]),
        Some("exit 3"),
    )
    .unwrap();
    let output = run_tests(&failing, failing.tools[0].as_ref())
        .unwrap_err()
        .to_string();
    assert!(output.contains("未通过（退出码 3）"), "{output}");
    let unset = sealed_tools(&root, &seal(&[Capability::RunTests], &["."]), None).unwrap();
    assert!(unset.tools[0].execute("{}").is_err());

    let changes = sealed.changes();
    let summary: Vec<_> = changes
        .iter()
        .map(|c| (c.path.as_str(), c.before.as_deref()))
        .collect();
    assert_eq!(
        summary,
        [
            ("src/main.rs", Some(original.as_str())),
            ("src/new/a.rs", None)
        ]
    );
    // 呈报后文件被他人改动：一处也不写。
    fs::write(root.join("src/main.rs"), "他人改动").unwrap();
    let err = apply_changes(&root, &changes).unwrap_err().to_string();
    assert!(err.contains("已被改动"), "{err}");
    assert!(!root.join("src/new").exists());
    fs::write(root.join("src/main.rs"), &original).unwrap();
    apply_changes(&root, &changes).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("src/main.rs")).unwrap(),
        "staged"
    );
    assert_eq!(fs::read_to_string(root.join("src/new/a.rs")).unwrap(), "新");
    // 写到一半中断（main.rs 已写、a.rs 未写）后重试：续写未写的，已写的不动。
    fs::remove_file(root.join("src/new/a.rs")).unwrap();
    apply_changes(&root, &changes).unwrap();
    assert_eq!(fs::read_to_string(root.join("src/new/a.rs")).unwrap(), "新");
    // 全部已写完：再落盘一次什么也不改，也不报错。
    apply_changes(&root, &changes).unwrap();
    assert_eq!(base_state(&root, &changes[0]).unwrap(), BaseState::Written);
    // 逐字节比较：只把行尾改成 CRLF 也算被改动；改成非 UTF-8 内容同样拒绝。
    let crlf = FileChange {
        path: "src/main.rs".into(),
        before: Some("a\nb\n".into()),
        after: "a\nB\n".into(),
        deleted: false,
    };
    fs::write(root.join("src/main.rs"), "a\r\nb\r\n").unwrap();
    assert_eq!(base_state(&root, &crlf).unwrap(), BaseState::Changed);
    fs::write(root.join("src/main.rs"), [0xff, 0xfe, 0]).unwrap();
    assert_eq!(base_state(&root, &crlf).unwrap(), BaseState::Changed);
    let err = apply_changes(&root, std::slice::from_ref(&crlf))
        .unwrap_err()
        .to_string();
    assert!(err.contains("已被改动"), "{err}");
    fs::write(root.join("src/main.rs"), "a\nb\n").unwrap();
    assert_eq!(base_state(&root, &crlf).unwrap(), BaseState::Original);
    // 事件簿被篡改成越界路径时同样拒绝。
    let evil = [FileChange {
        path: "../evil".into(),
        before: None,
        after: String::new(),
        deleted: false,
    }];
    assert!(apply_changes(&root, &evil).is_err());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_failed_write_mid_batch_keeps_later_targets_untouched_and_retry_completes() {
    use replace::faults::{Faults, install};
    let dir = workspace("partial-batch");
    let root = dir.join("project");
    let original = fs::read_to_string(root.join("src/main.rs")).unwrap();
    let changes = [
        FileChange {
            path: "src/main.rs".into(),
            before: Some(original.clone()),
            after: "first".into(),
            deleted: false,
        },
        FileChange {
            path: "src/new/b.rs".into(),
            before: None,
            after: "second file content".into(),
            deleted: false,
        },
    ];
    {
        let _guard = install(Faults {
            skip_calls: 1,
            partial_write: true,
            ..Faults::default()
        });
        let err = apply_changes(&root, &changes).unwrap_err().to_string();
        assert!(err.contains("injected"), "{err}");
    }
    assert_eq!(
        fs::read_to_string(root.join("src/main.rs")).unwrap(),
        "first"
    );
    assert!(!root.join("src/new/b.rs").exists());
    let leftovers = |sub: &str| fs::read_dir(root.join(sub)).map_or(0, |d| d.count());
    assert_eq!((leftovers("src"), leftovers("src/new")), (2, 0));
    assert_eq!(base_state(&root, &changes[0]).unwrap(), BaseState::Written);
    assert_eq!(base_state(&root, &changes[1]).unwrap(), BaseState::Original);
    apply_changes(&root, &changes).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("src/main.rs")).unwrap(),
        "first"
    );
    assert_eq!(
        fs::read_to_string(root.join("src/new/b.rs")).unwrap(),
        "second file content"
    );
    fs::remove_dir_all(dir).unwrap();
}

/// 造一份“已死写入者”的租约与它认领的临时文件。
fn stale_temp(dir: &Path) -> PathBuf {
    stale_temp_named(dir, ".ziyuan-tmp-1-1-1", "0123456789abcdef0123456789abcdef")
}

/// 同 [`stale_temp`]，名字与租约 id 由调用方给：同一目录里两个陈旧临时文件
/// 各要自己的租约，否则会被判成“两个认领者”而整轮不删。
fn stale_temp_named(dir: &Path, name: &str, id: &str) -> PathBuf {
    let temp = dir.join(name);
    fs::write(&temp, b"partial").unwrap();
    // 行尾只能是 `\n`：租约里多一个 `\r` 就解析不出 id，会被判成 LeaseUnparsable
    // 而整轮不删，用例就测不到“陈旧即可清理”这件事。
    fs::write(
        dir.join(format!(".ziyuan-lease-{id}")),
        format!(
            "ziyuan-lease v1 temp={} id={id}\n",
            temp.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();
    temp
}

fn file_change(path: &str, after: &str) -> FileChange {
    FileChange {
        path: path.into(),
        before: None,
        after: after.into(),
        deleted: false,
    }
}

fn temp_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

#[test]
fn relative_parent_of_strips_only_the_last_component() {
    assert_eq!(relative_parent_of("src/deep/a.rs"), "src/deep");
    assert_eq!(relative_parent_of("a.rs"), "");
    // 词法上的 `./` 也一并抹掉：受保护集合里的条目就是这个拼法。
    assert_eq!(relative_parent_of("./src/a.rs"), "src");
}

#[test]
fn apply_changes_sweeps_a_stale_temp_next_to_the_target() {
    let dir = workspace("sweep-stale");
    let project = dir.join("project");
    let stale = stale_temp(&project.join("src"));
    let applied = apply_changes_protected(
        &project,
        &[file_change("src/new.rs", "fn main() {}\n")],
        &BTreeSet::new(),
    )
    .unwrap();
    assert!(applied.problems.is_empty(), "{:?}", applied.problems);
    assert!(!stale.exists(), "写入前应清掉同目录的陈旧临时文件");
    assert_eq!(
        fs::read_to_string(project.join("src/new.rs")).unwrap(),
        "fn main() {}\n"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn apply_changes_keeps_a_protected_temp() {
    let dir = workspace("sweep-protected");
    let project = dir.join("project");
    let stale = stale_temp(&project.join("src"));
    // 同目录里再放一个不受保护的陈旧临时文件：它必须被删掉，否则“什么都没扫”
    // 也能让上面那条存活断言通过，用例就证明不了受保护集合真的在起作用。
    let doomed = stale_temp_named(
        &project.join("src"),
        ".ziyuan-tmp-2-2-2",
        "fedcba9876543210fedcba9876543210",
    );
    let protected: BTreeSet<String> = [format!("src/{}", temp_name(&stale))].into_iter().collect();
    apply_changes_protected(&project, &[file_change("src/new.rs", "x\n")], &protected).unwrap();
    assert!(stale.exists(), "受保护路径不得删除");
    assert!(!doomed.exists(), "不受保护的陈旧临时文件应被清掉");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn apply_changes_leaves_other_directories_alone() {
    let dir = workspace("sweep-other");
    let project = dir.join("project");
    let elsewhere = stale_temp(&project.join("docs"));
    // 工作区根（`project`）自己的陈旧临时文件：只有“扫根而不是扫父目录”才会碰到它。
    let at_root = stale_temp(&project);
    // 目标目录的下一层：`sweep_dir` 不递归，所以它也应当活下来。
    let nested_dir = project.join("src/nested");
    fs::create_dir_all(&nested_dir).unwrap();
    let nested = stale_temp(&nested_dir);
    apply_changes_protected(
        &project,
        &[file_change("src/new.rs", "x\n")],
        &BTreeSet::new(),
    )
    .unwrap();
    assert!(elsewhere.exists(), "自动清理只碰目标所在目录");
    assert!(at_root.exists(), "自动清理不扫工作区根");
    assert!(nested.exists(), "自动清理不递归子目录");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn apply_changes_labels_the_sweep_from_the_lexical_change_path() {
    let dir = workspace("sweep-deep");
    let project = dir.join("project");
    let deep = project.join("src/deep");
    fs::create_dir_all(&deep).unwrap();
    let stale = stale_temp(&deep);
    let doomed = stale_temp_named(
        &deep,
        ".ziyuan-tmp-3-3-3",
        "00112233445566778899aabbccddeeff",
    );
    // 恢复记录的写法：工作区相对、词法拼法，与 `src/deep/a.rs` 的父目录一致。
    let protected: BTreeSet<String> = [format!("src/deep/{}", temp_name(&stale))]
        .into_iter()
        .collect();
    apply_changes_protected(&project, &[file_change("src/deep/a.rs", "a\n")], &protected).unwrap();
    assert!(stale.exists(), "词法标签 src/deep 应与受保护条目对得上");
    assert!(!doomed.exists(), "同目录里不受保护的陈旧临时文件应被清掉");
    assert_eq!(fs::read_to_string(deep.join("a.rs")).unwrap(), "a\n");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn apply_changes_still_writes_when_the_sweep_finds_nothing() {
    let dir = workspace("sweep-clean");
    let project = dir.join("project");
    let applied = apply_changes_protected(
        &project,
        &[file_change("src/a.rs", "a\n")],
        &BTreeSet::new(),
    )
    .unwrap();
    assert!(applied.problems.is_empty());
    assert_eq!(fs::read_to_string(project.join("src/a.rs")).unwrap(), "a\n");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn apply_changes_still_writes_when_the_sweep_fails() {
    let dir = workspace("sweep-fails");
    let project = dir.join("project");
    let src = project.join("src");
    let stale = stale_temp(&src);
    // `write_all` 先把根与目标目录 canonicalize，扫描时的候选路径就是那个拼法
    // （Windows 上是 `\\?\` 前缀的 verbatim 路径），布防必须用同一拼法才对得上。
    let swept_dir = src.canonicalize().unwrap();
    let armed = swept_dir.join(stale.file_name().unwrap());
    // 真实的“删不掉”只在特定权限与文件系统下出现，所以用测试钩子注入一次确定
    // 的删除失败：清理诊断要进报告，写入必须照旧落地（自动清理绝不阻断写入）。
    let _armed = temp_cleanup::tests_hooks::arm_fail_remove(&armed);
    let applied = apply_changes_protected(
        &project,
        &[file_change("src/new.rs", "written anyway\n")],
        &BTreeSet::new(),
    )
    .expect("清理失败不得让写入失败");
    assert!(
        !applied.problems.is_empty(),
        "删除失败必须留一条诊断: {:?}",
        applied.problems
    );
    assert_eq!(
        applied.problems[0].path.parent(),
        Some(swept_dir.as_path()),
        "诊断必须指向本次写入所在目录的那个候选: {:?}",
        applied.problems
    );
    assert!(stale.exists(), "注入的是删除失败，候选应当还在");
    assert_eq!(
        fs::read_to_string(project.join("src/new.rs")).unwrap(),
        "written anyway\n",
        "清理失败后目标文件仍须写入新内容"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn plain_apply_changes_never_sweeps() {
    // 不掌握受保护集合的调用方不该顺手清理：宁可不删。
    let dir = workspace("sweep-plain");
    let project = dir.join("project");
    let stale = stale_temp(&project.join("src"));
    apply_changes(&project, &[file_change("src/new.rs", "x\n")]).unwrap();
    assert!(stale.exists(), "apply_changes 不做清理");
    assert!(project.join("src/new.rs").is_file());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn file_outline_and_find_symbols_respect_scope_denied_and_staged_files() {
    let dir = workspace("symbols");
    let root = dir.join("project");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("web")).unwrap();
    fs::create_dir_all(root.join("target")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "// fn fake_in_comment() {}
pub struct Engine;
impl Engine {
pub fn boot(&self) {}
}
pub fn helper() {}
",
    )
    .unwrap();
    fs::write(
        root.join("web/app.mjs"),
        "export class Panel {
  render() {}
}
export function bootUi() {}
",
    )
    .unwrap();
    fs::write(
        root.join("target/gen.rs"),
        "pub fn generated_boot() {}
",
    )
    .unwrap();
    fs::write(
        root.join(".env"),
        "pub fn secret_boot() {}
",
    )
    .unwrap();

    let sealed = sealed_tools(
        &root,
        &seal(
            &[
                Capability::ReadFile,
                Capability::SearchCode,
                Capability::WriteFile,
            ],
            &["."],
        ),
        None,
    )
    .unwrap();
    let get = |name: &str| {
        sealed
            .tools
            .iter()
            .find(|t| t.spec().name == name)
            .expect(name)
    };

    // 1. file_outline 列出结构与方法，跳过注释；支持 kind 过滤。
    let outline = get("file_outline")
        .execute(r#"{"path":"src/lib.rs"}"#)
        .unwrap();
    assert!(
        outline.contains("Engine") && outline.contains("boot"),
        "{outline}"
    );
    assert!(!outline.contains("fake_in_comment"), "{outline}");
    let only_fn = get("file_outline")
        .execute(r#"{"path":"src/lib.rs","kind":"function"}"#)
        .unwrap();
    assert!(
        only_fn.contains("boot") && only_fn.contains("helper"),
        "{only_fn}"
    );
    assert!(!only_fn.contains("[struct]"), "{only_fn}");

    // 2. find_symbols 跳过 target 与 .env，支持 ignore_case、exact、path 过滤。
    let found = get("find_symbols")
        .execute(r#"{"query":"boot","ignore_case":true}"#)
        .unwrap();
    assert!(
        found.contains("src/lib.rs:4: [fn] impl Engine > boot"),
        "{found}"
    );
    assert!(found.contains("web/app.mjs:4: [fn] bootUi"), "{found}");
    assert!(
        !found.contains("generated_boot") && !found.contains("secret_boot"),
        "{found}"
    );

    let exact = get("find_symbols")
        .execute(r#"{"query":"boot","exact":true}"#)
        .unwrap();
    assert!(exact.contains("src/lib.rs:4:"), "{exact}");
    assert!(!exact.contains("bootUi"), "{exact}");

    // 3. 暂存新建文件对 file_outline 与 find_symbols 立即可见。
    get("write_file")
        .execute(r#"{"path":"src/extra.py","content":"def staged_boot():\n    return 1\n"}"#)
        .unwrap();
    let staged_outline = get("file_outline")
        .execute(r#"{"path":"src/extra.py"}"#)
        .unwrap();
    assert!(staged_outline.contains("staged_boot"), "{staged_outline}");
    let staged_found = get("find_symbols")
        .execute(r#"{"query":"staged_boot","path":"src"}"#)
        .unwrap();
    assert!(
        staged_found.contains("src/extra.py:1-2: [fn] staged_boot"),
        "{staged_found}"
    );
    assert!(outline.contains("3-5: [impl] Engine"), "{outline}");
    assert!(
        staged_outline.contains("1-2: [fn] staged_boot"),
        "{staged_outline}"
    );

    // 3b. read_file 支持 end_line 直接复用符号行号范围，且与 lines 互斥、校验边界。
    let impl_body = get("read_file")
        .execute(r#"{"path":"src/lib.rs","offset":3,"end_line":5}"#)
        .unwrap();
    assert!(
        impl_body.contains("〔第 3–5 行，共 6 行；后面还有〕"),
        "{impl_body}"
    );
    assert!(
        impl_body.contains("impl Engine") && impl_body.contains("pub fn boot"),
        "{impl_body}"
    );
    assert!(!impl_body.contains("pub fn helper"), "{impl_body}");
    let head = get("read_file")
        .execute(r#"{"path":"src/lib.rs","end_line":2}"#)
        .unwrap();
    assert!(head.contains("〔第 1–2 行，共 6 行；后面还有〕"), "{head}");
    assert!(
        get("read_file")
            .execute(r#"{"path":"src/lib.rs","offset":2,"lines":2,"end_line":4}"#)
            .is_err()
    );
    assert!(
        get("read_file")
            .execute(r#"{"path":"src/lib.rs","offset":5,"end_line":3}"#)
            .is_err()
    );
    assert!(
        get("read_file")
            .execute(r#"{"path":"src/lib.rs","end_line":0}"#)
            .is_err()
    );

    // 4. 越界与禁区拒绝。
    assert!(get("file_outline").execute(r#"{"path":".env"}"#).is_err());
    assert!(
        get("file_outline")
            .execute(r#"{"path":"../outside.rs"}"#)
            .is_err()
    );
    assert!(
        get("find_symbols")
            .execute(r#"{"query":"x","path":"target"}"#)
            .is_err()
    );

    let narrow = sealed_tools(
        &root,
        &seal(&[Capability::ReadFile, Capability::SearchCode], &["src"]),
        None,
    )
    .unwrap();
    let narrow_outline = narrow
        .tools
        .iter()
        .find(|t| t.spec().name == "file_outline")
        .unwrap();
    let narrow_find = narrow
        .tools
        .iter()
        .find(|t| t.spec().name == "find_symbols")
        .unwrap();
    assert!(narrow_outline.execute(r#"{"path":"web/app.mjs"}"#).is_err());
    let narrow_hits = narrow_find.execute(r#"{"query":"boot"}"#).unwrap();
    assert!(
        narrow_hits.contains("src/lib.rs") && !narrow_hits.contains("web/app.mjs"),
        "{narrow_hits}"
    );

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn replace_text_batch_edits_are_atomic_and_audit_redacts_edits() {
    let dir = workspace("batch-replace");
    let root = dir.join("project");
    fs::write(
        root.join("src/main.rs"),
        "fn alpha() -> i32 { 1 }\nfn beta() -> i32 { 2 }\nfn gamma() -> i32 { 3 }\n",
    )
    .unwrap();
    let seal = Seal {
        capabilities: [Capability::ReadFile, Capability::WriteFile].into(),
        scope: vec!["src".into()],
        mcp: Default::default(),
    };
    let sealed = sealed_tools(&root, &seal, None).unwrap();
    let find = |name: &str| sealed.tools.iter().find(|t| t.spec().name == name).unwrap();
    let replace = find("replace_text");

    let bad = serde_json::json!({
        "path": "src/main.rs",
        "edits": [
            {"old": "fn alpha() -> i32 { 1 }", "new": "fn alpha() -> i32 { 10 }"},
            {"old": "fn missing() {}", "new": "fn nope() {}"}
        ]
    })
    .to_string();
    let err = replace.execute(&bad).unwrap_err().to_string();
    assert!(err.contains("第 2 处补丁"), "{err}");
    assert!(sealed.changes().is_empty(), "失败批次不得留下半截改动");

    let mixed = serde_json::json!({
        "path": "src/main.rs",
        "old": "alpha",
        "new": "a",
        "edits": [{"old": "beta", "new": "b"}]
    })
    .to_string();
    assert!(replace.execute(&mixed).is_err());

    let ok = serde_json::json!({
        "path": "src/main.rs",
        "edits": [
            {"old": "fn alpha() -> i32 { 1 }", "new": "fn alpha() -> i32 { 10 }"},
            {"old": "fn gamma() -> i32 { 3 }", "new": "fn gamma() -> i32 { 30 }"}
        ]
    })
    .to_string();
    let out = replace.execute(&ok).unwrap();
    assert!(out.contains("替换 2 处"), "{out}");
    let changes = sealed.changes();
    assert_eq!(changes.len(), 1);
    assert!(!changes[0].deleted);
    assert!(changes[0].after.contains("10") && changes[0].after.contains("30"));

    let audited = audit_args("replace_text", &ok);
    assert!(!audited.contains("fn alpha()"), "{audited}");
    assert!(audited.contains("全文见奏本"), "{audited}");

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn delete_file_and_rename_file_stage_overlay_and_apply_cleanly() {
    let dir = workspace("delete-rename");
    let root = dir.join("project");
    fs::write(root.join("src/obsolete.rs"), "pub fn old() {}\n").unwrap();
    fs::write(root.join("src/moved.rs"), "pub fn stay() -> i32 { 7 }\n").unwrap();
    let seal = Seal {
        capabilities: [
            Capability::ReadFile,
            Capability::SearchCode,
            Capability::WriteFile,
        ]
        .into(),
        scope: vec!["src".into()],
        mcp: Default::default(),
    };
    let sealed = sealed_tools(&root, &seal, None).unwrap();
    let get = |name: &str| sealed.tools.iter().find(|t| t.spec().name == name).unwrap();

    assert!(get("delete_file").execute(r#"{"path":".env"}"#).is_err());
    assert!(
        get("rename_file")
            .execute(r#"{"from":"src/moved.rs","to":".env"}"#)
            .is_err()
    );

    get("delete_file")
        .execute(r#"{"path":"src/obsolete.rs"}"#)
        .unwrap();
    assert!(root.join("src/obsolete.rs").exists());
    assert!(
        get("read_file")
            .execute(r#"{"path":"src/obsolete.rs"}"#)
            .is_err()
    );
    assert!(
        get("file_outline")
            .execute(r#"{"path":"src/obsolete.rs"}"#)
            .is_err()
    );
    let listed = get("list_files").execute(r#"{}"#).unwrap();
    assert!(!listed.contains("obsolete.rs"), "{listed}");
    let searched = get("search_code").execute(r#"{"query":"fn old"}"#).unwrap();
    assert!(!searched.contains("obsolete.rs"), "{searched}");
    let syms = get("find_symbols").execute(r#"{"query":"old"}"#).unwrap();
    assert!(!syms.contains("obsolete.rs"), "{syms}");

    assert!(
        get("rename_file")
            .execute(r#"{"from":"src/moved.rs","to":"src/main.rs"}"#)
            .is_err()
    );
    get("rename_file")
        .execute(r#"{"from":"src/moved.rs","to":"src/renamed.rs"}"#)
        .unwrap();
    assert!(
        get("read_file")
            .execute(r#"{"path":"src/moved.rs"}"#)
            .is_err()
    );
    let renamed_text = get("read_file")
        .execute(r#"{"path":"src/renamed.rs"}"#)
        .unwrap();
    assert!(renamed_text.contains("fn stay()"), "{renamed_text}");

    get("write_file")
        .execute(r#"{"path":"src/temp.rs","content":"// temp\n"}"#)
        .unwrap();
    get("delete_file")
        .execute(r#"{"path":"src/temp.rs"}"#)
        .unwrap();

    let changes = sealed.changes();
    assert_eq!(changes.len(), 3, "{changes:?}");
    assert!(
        changes
            .iter()
            .any(|c| c.path == "src/obsolete.rs" && c.deleted)
    );
    assert!(
        changes
            .iter()
            .any(|c| c.path == "src/moved.rs" && c.deleted)
    );
    assert!(
        changes
            .iter()
            .any(|c| c.path == "src/renamed.rs" && !c.deleted && c.before.is_none())
    );

    apply_changes(&root, &changes).unwrap();
    assert!(!root.join("src/obsolete.rs").exists());
    assert!(!root.join("src/moved.rs").exists());
    assert_eq!(
        fs::read_to_string(root.join("src/renamed.rs")).unwrap(),
        "pub fn stay() -> i32 { 7 }\n"
    );
    assert_eq!(base_state(&root, &changes[0]).unwrap(), BaseState::Written);
    apply_changes(&root, &changes).unwrap();

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn test_presets_parse_and_enforce_strict_allowlist() {
    let presets = parse_test_presets(
        "check=cargo check --workspace; clippy = cargo clippy -- -D warnings\ntest=cargo test",
    )
    .unwrap();
    assert_eq!(presets.len(), 3);
    assert_eq!(presets["check"], "cargo check --workspace");
    assert_eq!(presets["clippy"], "cargo clippy -- -D warnings");
    assert_eq!(presets["test"], "cargo test");

    // 非法名称、缺命令、重复名均拒绝。
    assert!(parse_test_presets("bad name=cargo test").is_err());
    assert!(parse_test_presets("check=").is_err());
    assert!(parse_test_presets("no_equals").is_err());
    assert!(parse_test_presets("a=1;a=2").is_err());

    let dir = std::env::temp_dir().join(format!("ziyuan-preset-allowlist-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("a.txt"), "ok").unwrap();
    let sandbox = Arc::new(Sandbox::new(&dir, &[".".into()], Some("cargo test"), presets).unwrap());
    let runner = RunTests(sandbox);
    assert_eq!(runner.resolve_command(None).unwrap(), "cargo test");
    assert_eq!(
        runner.resolve_command(Some("check")).unwrap(),
        "cargo check --workspace"
    );
    assert_eq!(
        runner.resolve_command(Some("clippy")).unwrap(),
        "cargo clippy -- -D warnings"
    );
    let err = runner
        .resolve_command(Some("rm -rf /"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("未知测试预设") && err.contains("check"),
        "{err}"
    );
    assert!(runner.resolve_command(Some("")).is_err());
    assert!(runner.spec().description.contains("check="));

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn list_files_and_search_code_support_glob_regex_context_and_staged_new_files() {
    let dir = workspace("search-nav");
    let root = dir.join("project");
    fs::write(
        root.join("src/engine.rs"),
        "pub fn boot_alpha() {\n    let x = 1;\n    let y = 2;\n}\npub fn boot_beta() {}\n",
    )
    .unwrap();
    let sealed = sealed_tools(
        &root,
        &seal(
            &[
                Capability::ReadFile,
                Capability::SearchCode,
                Capability::WriteFile,
            ],
            &["."],
        ),
        None,
    )
    .unwrap();
    let get = |name: &str| sealed.tools.iter().find(|t| t.spec().name == name).unwrap();

    get("write_file")
        .execute(r#"{"path":"src/staged_mod.rs","content":"pub fn boot_gamma() {}\n"}"#)
        .unwrap();

    let rs_only = get("list_files").execute(r#"{"glob":"*.rs"}"#).unwrap();
    assert!(
        rs_only.contains("src/main.rs")
            && rs_only.contains("src/engine.rs")
            && rs_only.contains("src/staged_mod.rs")
            && !rs_only.contains("docs/a.md"),
        "{rs_only}"
    );
    let md_only = get("list_files")
        .execute(r#"{"glob":"docs/*.md"}"#)
        .unwrap();
    assert!(
        md_only.contains("docs/a.md") && !md_only.contains("src/main.rs"),
        "{md_only}"
    );
    assert!(get("list_files").execute(r#"{"glob":"*.[rs"}"#).is_err());

    let re_out = get("search_code")
        .execute(r#"{"query":"boot_(alpha|gamma)","regex":true,"glob":"src/*.rs"}"#)
        .unwrap();
    assert!(
        re_out.contains("src/engine.rs:1:")
            && re_out.contains("src/staged_mod.rs:1:")
            && !re_out.contains("boot_beta"),
        "{re_out}"
    );

    let ctx_out = get("search_code")
        .execute(r#"{"query":"let x = 1","path":"src/engine.rs","context_lines":1}"#)
        .unwrap();
    assert!(
        ctx_out.contains("src/engine.rs:1- pub fn boot_alpha() {")
            && ctx_out.contains("src/engine.rs:2:     let x = 1;")
            && ctx_out.contains("src/engine.rs:3-     let y = 2;"),
        "{ctx_out}"
    );

    assert!(
        get("search_code")
            .execute(r#"{"query":"[unclosed","regex":true}"#)
            .unwrap_err()
            .to_string()
            .contains("正则表达式无效")
    );
    assert!(
        get("search_code")
            .execute(r#"{"query":"boot","context_lines":6}"#)
            .is_err()
    );

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn find_references_skips_comments_strings_and_substrings_and_respects_scope_and_staged() {
    let dir = workspace("refs");
    let root = dir.join("project");
    fs::write(
        root.join("src/lib.rs"),
        "// compute() in comment\npub fn compute(v: i32) -> i32 {\n    let msg = \"compute() in string\";\n    let recompute = v + 1;\n    recompute\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("src/caller.rs"),
        "use crate::compute;\npub fn run_all() -> i32 {\n    compute(42)\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("docs/a.md"),
        "Mention compute in markdown prose.\n",
    )
    .unwrap();
    fs::write(root.join(".env"), "compute=secret\n").unwrap();

    let sealed = sealed_tools(
        &root,
        &seal(
            &[
                Capability::ReadFile,
                Capability::SearchCode,
                Capability::WriteFile,
            ],
            &["."],
        ),
        None,
    )
    .unwrap();
    let get = |name: &str| sealed.tools.iter().find(|t| t.spec().name == name).unwrap();

    let all = get("find_references")
        .execute(r#"{"symbol":"compute"}"#)
        .unwrap();
    assert!(all.contains("src/lib.rs:2 [定义]:"), "{all}");
    assert!(all.contains("src/caller.rs:1 [引用]:"), "{all}");
    assert!(all.contains("src/caller.rs:3 [引用]:"), "{all}");
    assert!(
        !all.contains("in comment")
            && !all.contains("in string")
            && !all.contains("recompute")
            && !all.contains("docs/a.md")
            && !all.contains(".env"),
        "{all}"
    );

    let refs_only = get("find_references")
        .execute(r#"{"symbol":"compute","include_definitions":false}"#)
        .unwrap();
    assert!(!refs_only.contains("[定义]"), "{refs_only}");
    assert!(refs_only.contains("src/caller.rs:3 [引用]:"), "{refs_only}");

    get("write_file")
        .execute(r#"{"path":"src/new_caller.rs","content":"pub fn extra() { compute(7); }\n"}"#)
        .unwrap();
    get("delete_file")
        .execute(r#"{"path":"src/caller.rs"}"#)
        .unwrap();
    let after_stage = get("find_references")
        .execute(r#"{"symbol":"compute","include_definitions":false}"#)
        .unwrap();
    assert!(
        after_stage.contains("src/new_caller.rs:1 [引用]:")
            && !after_stage.contains("src/caller.rs"),
        "{after_stage}"
    );

    assert!(
        get("find_references")
            .execute(r#"{"symbol":"bad name"}"#)
            .is_err()
    );
    assert!(
        get("find_references")
            .execute(r#"{"symbol":"compute","path":".env"}"#)
            .is_err()
    );

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn replace_text_line_range_disambiguates_and_validates_bounds() {
    let dir = workspace("replace-range");
    let root = dir.join("project");
    fs::write(
        root.join("src/multi.rs"),
        "fn a() -> Result<(), ()> {\n    return Ok(());\n}\n\nfn b() -> Result<(), ()> {\n    return Ok(());\n}\n\nfn c() -> Result<(), ()> {\n    return Ok(());\n}\n",
    )
    .unwrap();
    let sealed = sealed_tools(
        &root,
        &seal(&[Capability::ReadFile, Capability::WriteFile], &["src"]),
        None,
    )
    .unwrap();
    let replace = sealed
        .tools
        .iter()
        .find(|t| t.spec().name == "replace_text")
        .unwrap();

    let ambig = replace
        .execute(r#"{"path":"src/multi.rs","old":"return Ok(());","new":"Ok(())"}"#)
        .unwrap_err()
        .to_string();
    assert!(
        ambig.contains("匹配到 3 处")
            && ambig.contains("第 2、6、10 行")
            && ambig.contains("start_line/end_line"),
        "{ambig}"
    );

    replace
        .execute(r#"{"path":"src/multi.rs","old":"return Ok(());","new":"Ok(())","start_line":5,"end_line":7}"#)
        .unwrap();
    let after_one = &sealed.changes()[0].after;
    assert_eq!(after_one.matches("return Ok(());").count(), 2);
    assert_eq!(after_one.matches("    Ok(())").count(), 1);
    assert!(after_one.contains("fn b() -> Result<(), ()> {\n    Ok(())\n}"));

    replace
        .execute(r#"{"path":"src/multi.rs","edits":[{"old":"return Ok(());","new":"Ok(()) // a","start_line":1,"end_line":3},{"old":"return Ok(());","new":"Ok(()) // c","start_line":9,"end_line":11}]}"#)
        .unwrap();
    let after_batch = &sealed.changes()[0].after;
    assert!(after_batch.contains("Ok(()) // a"));
    assert!(after_batch.contains("Ok(()) // c"));
    assert_eq!(after_batch.matches("return Ok(());").count(), 0);

    assert!(
        replace
            .execute(r#"{"path":"src/multi.rs","old":"fn a","new":"fn x","start_line":0}"#)
            .unwrap_err()
            .to_string()
            .contains("从 1 起")
    );
    assert!(
        replace
            .execute(
                r#"{"path":"src/multi.rs","old":"fn a","new":"fn x","start_line":5,"end_line":3}"#
            )
            .unwrap_err()
            .to_string()
            .contains("不能大于")
    );
    assert!(
        replace
            .execute(
                r#"{"path":"src/multi.rs","old":"fn a","new":"fn x","start_line":1,"end_line":999}"#
            )
            .unwrap_err()
            .to_string()
            .contains("超出文件总行数")
    );
    assert!(
        replace
            .execute(
                r#"{"path":"src/multi.rs","start_line":1,"edits":[{"old":"fn a","new":"fn x"}]}"#
            )
            .is_err()
    );

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn replace_text_missing_match_reports_outside_range_or_candidate_lines() {
    let dir = workspace("replace-diag");
    let root = dir.join("project");
    fs::write(
        root.join("src/diag.rs"),
        "fn alpha() {\n    let count = 1;\n}\nfn beta() {\n    let total_items = compute_sum(a, b);\n}\n",
    )
    .unwrap();
    let sealed = sealed_tools(
        &root,
        &seal(&[Capability::ReadFile, Capability::WriteFile], &["src"]),
        None,
    )
    .unwrap();
    let replace = sealed
        .tools
        .iter()
        .find(|t| t.spec().name == "replace_text")
        .unwrap();

    let outside = replace
        .execute(r#"{"path":"src/diag.rs","old":"let total_items = compute_sum(a, b);","new":"let total_items = 0;","start_line":1,"end_line":3}"#)
        .unwrap_err()
        .to_string();
    assert!(
        outside.contains("找不到要替换的文本") && outside.contains("第 5 行存在完全匹配"),
        "{outside}"
    );

    let near = replace
        .execute(r#"{"path":"src/diag.rs","old":"let total_items = compute_sum(a, c);","new":"let total_items = 0;"}"#)
        .unwrap_err()
        .to_string();
    assert!(
        near.contains("找不到要替换的文本")
            && near.contains("最接近的候选行")
            && near.contains("L5:     let total_items = compute_sum(a, b);"),
        "{near}"
    );

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn run_tests_cancels_inflight_command_promptly() {
    let dir = workspace("run-tests-cancel");
    let root = dir.join("project");
    #[cfg(windows)]
    let slow_cmd = "ping -n 15 127.0.0.1";
    #[cfg(not(windows))]
    let slow_cmd = "sleep 15";

    let sandbox =
        Arc::new(Sandbox::new(&root, &[".".to_owned()], Some(slow_cmd), BTreeMap::new()).unwrap());
    let tool = RunTests(sandbox);

    let already = AtomicBool::new(true);
    let early = tool
        .run_preset(None, crate::haoshe::Mode::Off, Some(&already))
        .unwrap_err()
        .to_string();
    assert!(early.contains("已取消"), "{early}");

    let flag = Arc::new(AtomicBool::new(false));
    let trigger = Arc::clone(&flag);
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        trigger.store(true, Ordering::SeqCst);
    });
    let started = std::time::Instant::now();
    let err = tool
        .run_preset(None, crate::haoshe::Mode::Off, Some(&flag))
        .unwrap_err()
        .to_string();
    let elapsed = started.elapsed();
    handle.join().unwrap();
    assert!(err.contains("已取消"), "{err}");
    assert!(
        elapsed < Duration::from_secs(5),
        "取消传播过慢: {elapsed:?}"
    );

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn symbol_index_persists_invalidates_and_respects_staged_edits() {
    let dir = workspace("symbol-index-e2e");
    let root = dir.join("project");
    fs::create_dir_all(root.join(".ziyuan")).unwrap();
    fs::write(
        root.join("src/alpha.rs"),
        "pub fn alpha_calc(x: i32) -> i32 { x + 1 }\npub fn caller() -> i32 { alpha_calc(41) }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/beta.rs"),
        "pub fn beta_only() -> i32 { 7 }\n",
    )
    .unwrap();

    let run_seal = seal(
        &[
            Capability::ReadFile,
            Capability::SearchCode,
            Capability::WriteFile,
        ],
        &["src"],
    );
    let sealed1 = sealed_tools(&root, &run_seal, None).unwrap();
    fn find_tool<'a>(sealed: &'a Sealed, name: &str) -> &'a dyn Tool {
        sealed
            .tools
            .iter()
            .find(|t| t.spec().name == name)
            .unwrap()
            .as_ref()
    }

    let outline1 = find_tool(&sealed1, "file_outline")
        .execute(r#"{"path":"src/alpha.rs"}"#)
        .unwrap();
    assert!(outline1.contains("alpha_calc"), "{outline1}");

    let syms1 = find_tool(&sealed1, "find_symbols")
        .execute(r#"{"query":"alpha_calc"}"#)
        .unwrap();
    assert!(syms1.contains("src/alpha.rs"), "{syms1}");

    let refs1 = find_tool(&sealed1, "find_references")
        .execute(r#"{"symbol":"alpha_calc"}"#)
        .unwrap();
    assert!(refs1.contains("[定义]"), "{refs1}");
    assert!(refs1.contains("[引用]"), "{refs1}");

    let cache_file = root.join(".ziyuan/index/symbols-v1.json");
    assert!(cache_file.exists(), "应持久化符号索引缓存");

    // 新建 Sandbox 命中磁盘缓存，结果应完全一致。
    let sealed2 = sealed_tools(&root, &run_seal, None).unwrap();
    let syms2 = find_tool(&sealed2, "find_symbols")
        .execute(r#"{"query":"alpha_calc"}"#)
        .unwrap();
    assert_eq!(syms1, syms2);
    let refs2 = find_tool(&sealed2, "find_references")
        .execute(r#"{"symbol":"alpha_calc"}"#)
        .unwrap();
    assert_eq!(refs1, refs2);

    // 磁盘文件修改（尺寸变化即刻失效），应反映新符号。
    fs::write(
        root.join("src/beta.rs"),
        "pub fn beta_only() -> i32 { 7 }\npub fn beta_fresh_symbol() -> i32 { alpha_calc(2) }\n",
    )
    .unwrap();
    let syms3 = find_tool(&sealed2, "find_symbols")
        .execute(r#"{"query":"beta_fresh_symbol"}"#)
        .unwrap();
    assert!(syms3.contains("beta_fresh_symbol"), "{syms3}");
    let refs3 = find_tool(&sealed2, "find_references")
        .execute(r#"{"symbol":"alpha_calc"}"#)
        .unwrap();
    assert!(refs3.contains("src/beta.rs"), "{refs3}");

    // 暂存改动优先于磁盘与缓存，且不污染磁盘缓存。
    find_tool(&sealed2, "write_file")
        .execute(r#"{"path":"src/alpha.rs","content":"pub fn staged_only_fn() -> i32 { 99 }\n"}"#)
        .unwrap();
    let staged_outline = find_tool(&sealed2, "file_outline")
        .execute(r#"{"path":"src/alpha.rs"}"#)
        .unwrap();
    assert!(
        staged_outline.contains("staged_only_fn"),
        "{staged_outline}"
    );
    assert!(!staged_outline.contains("alpha_calc"), "{staged_outline}");

    let sealed3 = sealed_tools(&root, &run_seal, None).unwrap();
    let disk_outline = find_tool(&sealed3, "file_outline")
        .execute(r#"{"path":"src/alpha.rs"}"#)
        .unwrap();
    assert!(disk_outline.contains("alpha_calc"), "{disk_outline}");
    assert!(!disk_outline.contains("staged_only_fn"), "{disk_outline}");

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn symbol_impact_summarizes_definitions_grouped_call_sites_and_staged_overlay() {
    let dir = workspace("symbol-impact");
    let root = dir.join("project");
    fs::create_dir_all(root.join("secret")).unwrap();
    fs::write(
        root.join("src/math.rs"),
        "/// compute_score helper\npub fn compute_score(x: i32) -> i32 {\n    x + 1\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("src/caller.rs"),
        "use crate::math::compute_score;\n\npub fn run() -> i32 {\n    let a = compute_score(1);\n    // compute_score in comment\n    let label = \"compute_score\";\n    let _ = label;\n    a + compute_score_extra(2)\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("secret/hidden.rs"),
        "pub fn nope() -> i32 { compute_score(99) }\n",
    )
    .unwrap();

    let seal = Seal {
        capabilities: [
            Capability::ReadFile,
            Capability::SearchCode,
            Capability::WriteFile,
        ]
        .into(),
        scope: vec!["src".into()],
        mcp: BTreeSet::new(),
    };
    let sealed = sealed_tools(&root, &seal, None).unwrap();
    let get = |name: &str| sealed.tools.iter().find(|t| t.spec().name == name).unwrap();
    let impact = get("symbol_impact");
    assert!(impact.is_read_only());

    let report = impact
        .execute(r#"{"symbol":"compute_score","kind":"fn","context_lines":1}"#)
        .unwrap();
    assert!(
        report.contains("〔符号影响面：`compute_score`〕"),
        "{report}"
    );
    assert!(
        report.contains("src/math.rs:2-4 [fn] compute_score"),
        "{report}"
    );
    assert!(report.contains("- src/caller.rs（共 2 处）"), "{report}");
    assert!(
        report.contains("4:     let a = compute_score(1);"),
        "{report}"
    );
    assert!(
        !report.contains("secret/hidden.rs"),
        "越界目录不可出现：{report}"
    );
    assert!(
        !report.contains("compute_score_extra"),
        "更长标识符不应作为匹配行：{report}"
    );
    assert!(
        report.contains(
            "定义 1 处（分布于 1 个文件）；调用／引用 2 处（分布于 1 个文件，已展示 2 处）"
        ),
        "{report}"
    );

    // 暂存新增调用点与删除文件均立即反映在影响面中。
    get("write_file")
        .execute(
            r#"{"path":"src/staged.rs","content":"pub fn extra() -> i32 {\n    compute_score(42)\n}\n"}"#,
        )
        .unwrap();
    let after_stage = impact
        .execute(r#"{"symbol":"compute_score","max_call_sites":2,"context_lines":0}"#)
        .unwrap();
    assert!(
        after_stage.contains("调用／引用 3 处（分布于 2 个文件，已展示 2 处）"),
        "{after_stage}"
    );
    assert!(
        after_stage.contains("其余 1 处调用／引用未列出"),
        "{after_stage}"
    );

    // kind 不匹配时不报定义，仍能看到调用点。
    let wrong_kind = impact
        .execute(r#"{"symbol":"compute_score","kind":"struct"}"#)
        .unwrap();
    assert!(wrong_kind.contains("（无匹配定义）"), "{wrong_kind}");
    assert!(wrong_kind.contains("src/caller.rs"), "{wrong_kind}");

    // 非法参数被拒绝。
    assert!(impact.execute(r#"{"symbol":"bad name"}"#).is_err());
    assert!(
        impact
            .execute(r#"{"symbol":"compute_score","max_call_sites":0}"#)
            .is_err()
    );
    assert!(
        impact
            .execute(r#"{"symbol":"compute_score","context_lines":4}"#)
            .is_err()
    );

    fs::remove_dir_all(dir).unwrap();
}
