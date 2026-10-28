//! 翰林院实录及其检索。

use std::sync::Arc;

use crate::office::Department;
use crate::{AgentId, TaskId};

/// 翰林院实录：一件已结诏令的始末（由事件簿推出，不另记事件）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub task: TaskId,
    pub department: Department,
    /// 诏令内容。
    pub edict: String,
    /// 承办官员及其姓名（已削籍者为空）。
    pub worker: AgentId,
    pub handler: String,
    /// 回奏全文（与朝廷状态共用一份）。
    pub summary: Arc<str>,
    /// 皇帝评定与都察院复核，如「采纳：好」「御史弹劾」；尚无则为「待评定」。
    pub verdict: String,
}

/// 检索词：ASCII 词（小写，≥2 字符）与中文等其他字符的相邻二字组。
pub(crate) fn search_terms(text: &str) -> std::collections::BTreeSet<String> {
    let mut terms = std::collections::BTreeSet::new();
    let mut word = String::new();
    let mut run: Vec<char> = Vec::new();
    let flush_word = |word: &mut String, terms: &mut std::collections::BTreeSet<String>| {
        if word.chars().count() >= 2 {
            terms.insert(word.to_lowercase());
        }
        word.clear();
    };
    let flush_run = |run: &mut Vec<char>, terms: &mut std::collections::BTreeSet<String>| {
        for pair in run.windows(2) {
            terms.insert(pair.iter().collect());
        }
        if run.len() == 1 {
            terms.insert(run[0].to_string());
        }
        run.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            flush_run(&mut run, &mut terms);
            word.push(c);
        } else if c.is_alphanumeric() {
            flush_word(&mut word, &mut terms);
            run.push(c);
        } else {
            flush_word(&mut word, &mut terms);
            flush_run(&mut run, &mut terms);
        }
    }
    flush_word(&mut word, &mut terms);
    flush_run(&mut run, &mut terms);
    terms
}

pub(crate) fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 中文等其他字符（相邻者组成二字组）。
pub(crate) fn is_run_char(c: char) -> bool {
    c.is_alphanumeric() && !c.is_ascii_alphanumeric()
}

/// `text` 中某个完整的 ASCII 词（不分大小写）等于 `word`（已小写）。
pub(crate) fn has_word(text: &str, word: &str) -> bool {
    let (t, w) = (text.as_bytes(), word.as_bytes());
    t.windows(w.len()).enumerate().any(|(i, window)| {
        window.eq_ignore_ascii_case(w)
            && (i == 0 || !is_word_byte(t[i - 1]))
            && t.get(i + w.len()).is_none_or(|&b| !is_word_byte(b))
    })
}

/// `text` 中 `c` 单独成串（前后都不是中文等字符）。
pub(crate) fn has_single(text: &str, c: char) -> bool {
    text.match_indices(c).any(|(i, m)| {
        !text[..i].chars().next_back().is_some_and(is_run_char)
            && !text[i + m.len()..].chars().next().is_some_and(is_run_char)
    })
}

/// 实录含有的检索词 `term`（取自 [`search_terms`]）。与对原文求 `search_terms` 再查集合等价，
/// 但不为每件实录建词表：二字组的两字都是中文等字符，原文含此子串即含此词。
pub(crate) fn record_has(record: &Record, term: &str) -> bool {
    let fields = [&*record.edict, &*record.summary, &*record.verdict];
    let mut chars = term.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => fields.iter().any(|t| has_single(t, c)),
        (Some(c), Some(_)) if !c.is_ascii() => fields.iter().any(|t| t.contains(term)),
        _ => fields.iter().any(|t| has_word(t, term)),
    }
}

/// 列出实录：`query` 为空时全部（新者在前），否则按检索词至多 20 件。
pub fn find_records<'a>(records: &'a [Record], query: &str) -> Vec<&'a Record> {
    if query.trim().is_empty() {
        records.iter().rev().collect()
    } else {
        search_records(records, query, 20)
    }
}

/// 按与 `query` 共有的检索词数检索实录，得分高者在前（同分时新者在前），至多 `limit` 件。
/// 直接在原文中查各检索词，不为每件实录建词表。
pub fn search_records<'a>(records: &'a [Record], query: &str, limit: usize) -> Vec<&'a Record> {
    let wanted = search_terms(query);
    let mut scored: Vec<(usize, &Record)> = records
        .iter()
        .map(|r| (wanted.iter().filter(|t| record_has(r, t)).count(), r))
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.task.cmp(&a.1.task)));
    scored.into_iter().take(limit).map(|(_, r)| r).collect()
}
