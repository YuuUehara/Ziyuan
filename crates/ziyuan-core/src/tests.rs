use super::*;
use crate::agent::*;
use crate::changes::*;
use crate::office::*;
use crate::records::*;
use crate::seal::*;

#[test]
fn mcp_grants_are_narrowed_by_the_ceiling() {
    let seal = |mcp: &[&str]| Seal {
        mcp: mcp.iter().map(|m| (*m).to_owned()).collect(),
        ..Seal::default()
    };
    let mcp = |s: Option<Seal>| s.unwrap().mcp.into_iter().collect::<Vec<_>>();
    // 上限只许 gh 的 search：整服务授权收窄为这一件，他服务的去掉。
    let ceiling = seal(&["gh/search", "docs"]);
    assert_eq!(mcp(seal(&["gh", "web"]).within(&ceiling)), ["gh/search"]);
    assert_eq!(mcp(seal(&["*"]).within(&ceiling)), ["docs", "gh/search"]);
    assert_eq!(mcp(seal(&["docs/read"]).within(&ceiling)), ["docs/read"]);
    assert_eq!(
        mcp(seal(&["gh", "web"]).within(&seal(&["*"]))),
        ["gh", "web"]
    );
    assert!(mcp(seal(&["gh"]).within(&seal(&[]))).is_empty());
    assert!(valid_mcp_grant("my-srv/tool_1") && valid_mcp_grant("*"));
    assert!(!valid_mcp_grant("a/b/c") && !valid_mcp_grant("a b") && !valid_mcp_grant("/x"));
    assert_eq!(
        seal(&["gh/search"]).to_string(),
        "工具：read_file、search_code；范围：.；外藩：gh/search"
    );
}

#[test]
fn records_are_found_by_shared_words_and_chinese_bigrams() {
    let record = |id, edict: &str, summary: &str| Record {
        task: TaskId(id),
        department: Department::Works,
        edict: edict.into(),
        worker: AgentId(4),
        handler: "某".into(),
        summary: summary.into(),
        verdict: "待评定".into(),
    };
    let records = [
        record(1, "检查除零", "div 未处理除数为零"),
        record(2, "整理 README", "补了目录"),
        record(3, "再查除零", "Div 已改为返回 0"),
        record(4, "查 lock_order 与锁", "Mutex 锁"),
        record(5, "查死锁", "divide 与 lock_order2"),
    ];
    let ids = |q| {
        search_records(&records, q, 5)
            .iter()
            .map(|r| r.task.0)
            .collect::<Vec<_>>()
    };
    // 「除零」二字组命中 1、3；同分新者在前。英文不分大小写。
    assert_eq!(ids("div 函数会不会除零"), [3, 1]);
    assert_eq!(ids("readme"), [2]);
    assert!(ids("数据库").is_empty());
    assert_eq!(search_records(&records, "除零", 1).len(), 1);
    // 英文按整词：div 不中 divide，lock_order 不中 lock_order2。
    assert_eq!(ids("LOCK_ORDER"), [4]);
    assert_eq!(ids("div"), [3, 1]);
    // 单字只中单独成串的：「锁」中 4，不中「死锁」里的锁。
    assert_eq!(ids("锁"), [4]);
}

#[test]
fn hunks_select_keeps_line_endings_and_new_files_can_be_dropped() {
    // CRLF、末行无换行：取舍后逐字节保留；相隔 7 行的两处为两段，相隔 6 行并为一段。
    let before: String = (1..=12).map(|i| format!("l{i}\r\n")).collect::<String>() + "end";
    let change = FileChange {
        path: "a".into(),
        before: Some(before.clone()),
        after: before.replace("l1\r\n", "L1\r\n").replace("end", "END"),
    };
    let hunks = change.hunks();
    assert_eq!(hunks.len(), 2, "{hunks:?}");
    assert_eq!(
        hunks[0].lines[..2],
        [('-', "l1".into()), ('+', "L1".into())]
    );
    assert_eq!(hunks[1].start, 10);
    assert_eq!(
        change.select(|h| h == 1).unwrap(),
        before.replace("end", "END")
    );
    assert_eq!(change.select(|_| true).unwrap(), change.after);
    assert_eq!(change.select(|_| false).unwrap(), before);
    let near = FileChange {
        after: before
            .replace("l1\r\n", "L1\r\n")
            .replace("l8\r\n", "L8\r\n"),
        ..change
    };
    assert_eq!(near.hunks().len(), 1);

    let created = FileChange {
        path: "b".into(),
        before: None,
        after: "x\ny".into(),
    };
    assert_eq!(created.hunks().len(), 1);
    assert_eq!(created.select(|_| false), None);
    assert_eq!(created.select(|_| true).as_deref(), Some("x\ny"));
    let empty = FileChange {
        after: String::new(),
        ..created
    };
    assert!(empty.hunks().is_empty());
    assert_eq!(empty.select(|_| false).as_deref(), Some(""));
}

fn seal(tools: &[Capability], scope: &[&str]) -> Seal {
    Seal {
        capabilities: tools.iter().copied().collect(),
        scope: scope.iter().map(|s| s.to_string()).collect(),
        mcp: Default::default(),
    }
}

#[test]
fn report_sections_are_found_at_line_starts() {
    let good = "## 结论\n可以。\n**依据**：src/lib.rs:3\n3. 未办：无";
    assert!(missing_sections(good).is_empty());
    assert!(missing_sections("【结论】行\n- 依据：甲\n未办之事：无").is_empty());
    // 段落标题须在行首；正文里提到「结论」不算。
    assert_eq!(
        missing_sections("我的结论是可以，依据如下。\n依据：甲"),
        ["结论", "未办之事"]
    );
    assert_eq!(missing_sections(""), REPORT_SECTIONS);
}

#[test]
fn personas_vary_and_are_stable() {
    let labels: std::collections::BTreeSet<_> = (1..=40)
        .map(|id| Persona::of(AgentId(id)).label())
        .collect();
    assert!(labels.len() >= 12, "{labels:?}");
    assert_eq!(Persona::of(AgentId(7)), Persona::of(AgentId(7)));
    assert!(Persona::of(AgentId(3)).prompt().contains("不改变你的职权"));
}

#[test]
fn seal_within_ceiling_narrows_tools_and_scope() {
    use Capability::*;
    let ceiling = seal(&[SearchCode], &["crates/core", "docs"]);
    // 默认玉玺（整个项目、全部工具）收窄为上限本身。
    assert_eq!(
        Seal::default().within(&ceiling),
        Some(seal(&[SearchCode], &["crates/core", "docs"]))
    );
    // 上限内的路径保留；上限外的去掉；`./` 前缀与尾部斜杠不影响比较。
    assert_eq!(
        seal(
            &[ReadFile, SearchCode],
            &["./crates/core/src", "README.md", "docs/"]
        )
        .within(&ceiling),
        Some(seal(&[SearchCode], &["./crates/core/src", "docs/"]))
    );
    // 同名前缀不算在内：crates/core2 不在 crates/core 之下。
    assert_eq!(seal(&[ReadFile], &["crates/core2"]).within(&ceiling), None);
    // 上限为整个项目时不收窄范围。
    let open = seal(&[ReadFile], &["."]);
    assert_eq!(
        seal(&[ReadFile, SearchCode], &["src"]).within(&open),
        Some(seal(&[ReadFile], &["src"]))
    );
}

#[test]
fn translation_kinds_parse_from_their_codes() {
    for kind in TranslationKind::ALL {
        assert_eq!(kind.code().parse::<TranslationKind>(), Ok(kind));
    }
    assert!(TranslationKind::Translate.writes() && TranslationKind::Revise.writes());
    assert!(!TranslationKind::ReviewFidelity.writes() && !TranslationKind::ReviewStyle.writes());
    assert_eq!(
        "polish".parse::<TranslationKind>(),
        Err(ParseError::UnknownTranslationKind("polish".into()))
    );
}
