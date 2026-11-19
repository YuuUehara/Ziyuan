//! 从 `cargo test` 的输出里抽出失败测试名和断言。
//!
//! 只认 cargo 的 `test result: FAILED` 与其后的失败名单。认不出时返回空，
//! 调用方继续只给出输出末尾。

/// 单次附在测试结果里的失败条数上限。
const MAX_FAILURES: usize = 8;
/// 每条断言摘录的字数上限。
const MAX_EXCERPT_CHARS: usize = 400;

/// cargo test 未通过时的失败名单与断言摘录。不是这种输出时为 `None`。
pub(super) fn cargo_failure_note(log: &str) -> Option<String> {
    if !log.contains("test result: FAILED") {
        return None;
    }
    let names = failure_names(log);
    if names.is_empty() {
        return None;
    }
    let mut out = String::from("〔失败测试〕\n");
    let shown = names.len().min(MAX_FAILURES);
    for name in names.iter().take(shown) {
        out.push_str(&format!("- {name}\n"));
        if let Some(excerpt) = excerpt_for(log, name) {
            for line in excerpt.lines() {
                out.push_str(&format!("  {line}\n"));
            }
        }
    }
    if names.len() > shown {
        out.push_str(&format!("（其余 {} 个失败未列出）\n", names.len() - shown));
    }
    Some(out)
}

/// 取最后一块「failures:」下面缩进的测试名。前面那块通常是各测试的 stdout，不是名单。
fn failure_names(log: &str) -> Vec<String> {
    let mut last = Vec::new();
    let mut current = Vec::new();
    let mut in_block = false;
    for line in log.lines() {
        if line.trim() == "failures:" {
            if !current.is_empty() {
                last = std::mem::take(&mut current);
            }
            in_block = true;
            continue;
        }
        if !in_block {
            continue;
        }
        if let Some(rest) = line.strip_prefix("    ") {
            let name = rest.trim();
            if !name.is_empty() {
                current.push(name.to_owned());
            }
            continue;
        }
        if line.trim().is_empty() {
            if !current.is_empty() {
                last = std::mem::take(&mut current);
                in_block = false;
            }
            continue;
        }
        in_block = false;
        current.clear();
    }
    if !current.is_empty() {
        last = current;
    }
    last
}

fn excerpt_for(log: &str, name: &str) -> Option<String> {
    let marker = format!("thread '{name}' panicked at");
    let start = log.find(&marker)?;
    let mut out = String::new();
    for (index, line) in log[start..].lines().enumerate() {
        if index > 0
            && (line.trim().is_empty()
                || line.starts_with("note: run with")
                || line.trim() == "failures:")
        {
            break;
        }
        if index >= 8 {
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line.trim_end());
        if out.chars().count() >= MAX_EXCERPT_CHARS {
            break;
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        "\
running 2 tests
test tests::ok ... ok
test tests::foo ... FAILED

failures:

---- tests::foo stdout ----
thread 'tests::foo' panicked at src/lib.rs:10:5:
assertion `left == right` failed
  left: 1
 right: 2
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:
    tests::foo
    tests::bar

test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
"
        .into()
    }

    #[test]
    fn cargo_note_lists_names_and_assertion() {
        let note = cargo_failure_note(&sample()).unwrap();
        assert!(note.starts_with("〔失败测试〕\n- tests::foo\n"), "{note}");
        assert!(note.contains("assertion `left == right` failed"), "{note}");
        assert!(note.contains("\n- tests::bar\n"), "{note}");
        assert!(!note.contains("RUST_BACKTRACE"), "{note}");
    }

    #[test]
    fn other_commands_and_compile_errors_are_left_alone() {
        assert!(cargo_failure_note("").is_none());
        assert!(cargo_failure_note("未通过（退出码 3）\n").is_none());
        assert!(cargo_failure_note("FAILED tests/test_app.py::test_x\n").is_none());
        assert!(
            cargo_failure_note("error: could not compile `demo`\nerror: test failed\n").is_none()
        );
    }

    #[test]
    fn extra_failures_are_counted_not_all_printed() {
        let mut log = String::from("test result: FAILED\nfailures:\n");
        for index in 0..10 {
            log.push_str(&format!("    tests::case{index}\n"));
        }
        let note = cargo_failure_note(&log).unwrap();
        assert!(note.contains("- tests::case0\n"), "{note}");
        assert!(note.contains("- tests::case7\n"), "{note}");
        assert!(!note.contains("tests::case8"), "{note}");
        assert!(note.contains("其余 2 个失败未列出"), "{note}");
    }
}
