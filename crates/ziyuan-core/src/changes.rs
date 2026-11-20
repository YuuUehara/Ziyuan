//! 工部改动：文件改动与差异块。

use serde::{Deserialize, Serialize};

fn is_false(value: &bool) -> bool {
    !*value
}

/// 承办官员暂存的一处文件改动（支持新建、改写与删除；改名拆为删旧建新两处改动）。
/// `before` 为改动时项目中的原文，新建文件为 `None`；落盘前据此确认文件在此期间未被他人改动。
/// `deleted` 为 `true` 时表示删除该文件，此时 `before` 必为 `Some`、`after` 为空串。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// 相对项目根目录的路径（`/` 分隔）。
    pub path: String,
    pub before: Option<String>,
    pub after: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub deleted: bool,
}

/// 统一 diff 中一段改动：`start` 为原文起始行号（从 1 起），`lines` 为（标记, 行）——
/// `' '` 上下文、`'-'` 删除、`'+'` 新增；行不含换行符。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub start: usize,
    pub lines: Vec<(char, String)>,
}

/// 每段前后保留的上下文行数；相隔不超过两倍上下文的改动并为一段。
pub(crate) const HUNK_CONTEXT: usize = 3;

impl FileChange {
    /// 逐行比较（行含换行符，以保留行尾）：(标记, 原文行号, 行)。
    pub(crate) fn ops(&self) -> Vec<(char, usize, &str)> {
        let old: Vec<&str> = self
            .before
            .as_deref()
            .map_or(Vec::new(), |b| b.split_inclusive('\n').collect());
        if self.deleted {
            return old
                .into_iter()
                .enumerate()
                .map(|(i, l)| ('-', i, l))
                .collect();
        }
        let new: Vec<&str> = self.after.split_inclusive('\n').collect();
        let (n, m) = (old.len(), new.len());
        let mut ops = Vec::with_capacity(n.max(m));
        if n.saturating_mul(m) > 4_000_000 {
            // 太大时不求最小差异：整段删去、整段新增。
            ops.extend(old.iter().enumerate().map(|(i, l)| ('-', i, *l)));
            ops.extend(new.iter().map(|l| ('+', n, *l)));
            return ops;
        }
        // 最长公共子序列：lcs[i][j] 为 old[i..] 与 new[j..] 的公共行数。
        let mut lcs = vec![vec![0u32; m + 1]; n + 1];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[i][j] = if old[i] == new[j] {
                    lcs[i + 1][j + 1] + 1
                } else {
                    lcs[i + 1][j].max(lcs[i][j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && old[i] == new[j] {
                ops.push((' ', i, old[i]));
                (i, j) = (i + 1, j + 1);
            } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
                ops.push(('-', i, old[i]));
                i += 1;
            } else {
                ops.push(('+', i, new[j]));
                j += 1;
            }
        }
        ops
    }

    /// 各改动行（`ops` 的下标）所属的段号（从 0 起），与 [`hunks`](Self::hunks) 一致。
    pub(crate) fn hunk_of(ops: &[(char, usize, &str)]) -> Vec<Option<usize>> {
        let mut owner = vec![None; ops.len()];
        let mut current: Option<(usize, usize)> = None; // (段号, 上一改动行)
        for (k, op) in ops.iter().enumerate() {
            if op.0 == ' ' {
                continue;
            }
            let hunk = match current {
                Some((h, last)) if k - last - 1 <= 2 * HUNK_CONTEXT => h,
                Some((h, _)) => h + 1,
                None => 0,
            };
            owner[k] = Some(hunk);
            current = Some((hunk, k));
        }
        owner
    }

    /// 按段切分的差异，供逐段准奏。新建与删除均只占一段（含删除空文件）。
    pub fn hunks(&self) -> Vec<Hunk> {
        let ops = self.ops();
        if self.deleted && ops.is_empty() {
            // 删除空文件也占一段，使皇帝可逐段准奏或驳回。
            return vec![Hunk {
                start: 1,
                lines: Vec::new(),
            }];
        }
        let owner = Self::hunk_of(&ops);
        let mut hunks: Vec<(usize, usize, usize)> = Vec::new(); // (段号, 首改动行, 末改动行)
        for (k, h) in owner.iter().enumerate() {
            match (h, hunks.last_mut()) {
                (Some(h), Some(last)) if last.0 == *h => last.2 = k,
                (Some(h), _) => hunks.push((*h, k, k)),
                (None, _) => {}
            }
        }
        hunks
            .into_iter()
            .map(|(_, first, last)| {
                let from = first.saturating_sub(HUNK_CONTEXT);
                let to = (last + HUNK_CONTEXT).min(ops.len() - 1);
                Hunk {
                    start: ops[from].1 + 1,
                    lines: ops[from..=to]
                        .iter()
                        .map(|(mark, _, text)| {
                            let text = text.strip_suffix('\n').unwrap_or(text);
                            (*mark, text.strip_suffix('\r').unwrap_or(text).to_owned())
                        })
                        .collect(),
                }
            })
            .collect()
    }

    /// 只采纳 `keep(段号)` 为真的段（段号从 0 起）后的文件内容；`None` 表示文件不存在
    /// （新建文件被整段驳回，或删除文件被准奏）。全部采纳即落盘结果，全部驳回即 `before`。
    pub fn select(&self, keep: impl Fn(usize) -> bool) -> Option<String> {
        if self.deleted {
            return if keep(0) { None } else { self.before.clone() };
        }
        let ops = self.ops();
        let owner = Self::hunk_of(&ops);
        let mut out = String::new();
        let mut any_new = false;
        for ((mark, _, text), hunk) in ops.iter().zip(owner) {
            let kept = hunk.is_some_and(&keep);
            match mark {
                ' ' => out.push_str(text),
                '-' if !kept => out.push_str(text),
                '+' if kept => {
                    any_new = true;
                    out.push_str(text);
                }
                _ => {}
            }
        }
        let created = any_new || !ops.iter().any(|op| op.0 == '+');
        (self.before.is_some() || created).then_some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deleted_file_is_single_hunk_and_supports_skip_and_empty_files() {
        let non_empty = FileChange {
            path: "src/old.rs".into(),
            before: Some("fn a() {}\nfn b() {}\n".into()),
            after: String::new(),
            deleted: true,
        };
        let hunks = non_empty.hunks();
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].lines.len(), 2);
        assert!(hunks[0].lines.iter().all(|(m, _)| *m == '-'));
        assert_eq!(non_empty.select(|_| true), None);
        assert_eq!(non_empty.select(|_| false), non_empty.before);

        let empty = FileChange {
            path: "empty.txt".into(),
            before: Some(String::new()),
            after: String::new(),
            deleted: true,
        };
        assert_eq!(empty.hunks().len(), 1);
        assert_eq!(empty.select(|_| true), None);
        assert_eq!(empty.select(|_| false), Some(String::new()));
    }
}
