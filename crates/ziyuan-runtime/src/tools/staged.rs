//! 暂存区自审与回退工具：`staged_diff`（只读预览内存暂存统一 diff）与 `revert_staged`（撤销暂存回到磁盘原状）。

use std::sync::Arc;
use ziyuan_core::FileChange;

use crate::BoxError;
use crate::agent_loop::{Tool, ToolSpec};

use super::{
    MAX_STAGED_DIFF_BYTES, Sandbox, clip_str, object_args, optional_bool, optional_string,
};

pub(super) struct StagedDiff(pub(super) Arc<Sandbox>);

impl Tool for StagedDiff {
    fn is_read_only(&self) -> bool {
        true
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "staged_diff".into(),
            description: format!(
                "只读查看本次诏令在内存暂存区中尚未落盘的统一 diff（含新建、改写、删除与逐段段号 `[第 N 段]`，最多 {MAX_STAGED_DIFF_BYTES} 字节）。便于在调用 run_tests 或撰写回奏前自审暂存改动（vcs_diff 仅看磁盘版本库，不含内存暂存改动）。可选传入授权范围（{}）内的文件或子目录 path 过滤。",
                self.0.scope_label()
            ),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"可选：只查看该相对文件或子目录下的暂存改动；省略则汇总全部暂存改动"}},"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(args_json, &["path"])?;
        let filter_key = match optional_string(&args, "path")? {
            Some(raw) if !raw.trim().is_empty() && raw.trim() != "." => {
                let (_, key) = self.0.resolve_write(raw.trim())?;
                Some(key)
            }
            _ => None,
        };
        let changes: Vec<FileChange> = {
            let staged = self.0.staged.lock().expect("暂存区锁未中毒");
            staged
                .values()
                .filter(|c| c.deleted || c.before.as_deref() != Some(c.after.as_str()))
                .filter(|c| match filter_key.as_deref() {
                    None => true,
                    Some(k) => c.path == k || c.path.starts_with(&format!("{k}/")),
                })
                .cloned()
                .collect()
        };
        if changes.is_empty() {
            return Ok(match filter_key {
                Some(k) => format!("{k} 当前无暂存改动"),
                None => "当前暂存区无未呈报改动".into(),
            });
        }
        Ok(render_staged_unified_diff(&changes, MAX_STAGED_DIFF_BYTES))
    }
}

fn render_staged_unified_diff(changes: &[FileChange], max_bytes: usize) -> String {
    let mut global_hunk = 0usize;
    let mut total_add = 0usize;
    let mut total_del = 0usize;
    let mut blocks: Vec<String> = Vec::with_capacity(changes.len());

    for change in changes {
        let hunks = change.hunks();
        let old_header = if change.before.is_none() {
            "--- /dev/null".to_owned()
        } else {
            format!("--- a/{}", change.path)
        };
        let new_header = if change.deleted {
            "+++ /dev/null".to_owned()
        } else {
            format!("+++ b/{}", change.path)
        };
        let mut block = format!("{old_header}\n{new_header}");
        if hunks.is_empty() {
            block.push_str("\n（新建空文件，0 行）");
            blocks.push(block);
            continue;
        }
        let mut line_delta: isize = 0;
        for hunk in &hunks {
            global_hunk += 1;
            let old_count = hunk.lines.iter().filter(|(m, _)| *m != '+').count();
            let new_count = hunk.lines.iter().filter(|(m, _)| *m != '-').count();
            let adds = hunk.lines.iter().filter(|(m, _)| *m == '+').count();
            let dels = hunk.lines.iter().filter(|(m, _)| *m == '-').count();
            total_add += adds;
            total_del += dels;
            let old_start = if old_count == 0 && (change.before.is_none() || hunk.lines.is_empty())
            {
                0
            } else {
                hunk.start
            };
            let new_start = if new_count == 0 && change.deleted {
                0
            } else {
                (hunk.start as isize + line_delta).max(0) as usize
            };
            line_delta += new_count as isize - old_count as isize;
            if change.deleted && hunk.lines.is_empty() {
                block.push_str(&format!(
                    "\n@@ -{old_start},{old_count} +{new_start},{new_count} @@ [第 {global_hunk} 段]（删除空文件）"
                ));
            } else {
                block.push_str(&format!(
                    "\n@@ -{old_start},{old_count} +{new_start},{new_count} @@ [第 {global_hunk} 段]"
                ));
                for (mark, line) in &hunk.lines {
                    block.push('\n');
                    block.push(*mark);
                    block.push_str(line);
                }
            }
        }
        blocks.push(block);
    }

    let summary = format!(
        "〔暂存奏本预览：共 {} 个文件，{} 段，+{} -{}〕\n",
        changes.len(),
        global_hunk,
        total_add,
        total_del
    );
    let body = blocks.join("\n");
    let full = format!("{summary}{body}");
    if full.len() <= max_bytes {
        full
    } else {
        let clipped = clip_str(&full, max_bytes);
        format!("{clipped}\n[已按 {max_bytes} 字节截断；可传入 path 按单文件查看]")
    }
}

pub(super) struct RevertStaged(pub(super) Arc<Sandbox>);

impl Tool for RevertStaged {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "revert_staged".into(),
            description: format!(
                "撤销内存暂存区中尚未落盘的改动，使对应文件恢复到磁盘原状（新建文件取消暂存、已删或已改文件还原为磁盘原文，后续 read_file / run_tests 即见原状）。可传授权范围（{}）内的单文件 `path`，或传 `all: true` 清空本次全部暂存改动。",
                self.0.scope_label()
            ),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"要撤销暂存改动的相对文件路径（与 all 二选一）"},"all":{"type":"boolean","description":"为 true 时清空本次全部暂存改动（与 path 二选一）"}},"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let args = object_args(args_json, &["path", "all"])?;
        let path_opt = optional_string(&args, "path")?;
        let has_all = args.contains_key("all");
        if path_opt.is_some() && has_all {
            return Err("path 与 all 不能同时传入".into());
        }
        let all = optional_bool(&args, "all")?.unwrap_or(false);
        if all {
            let mut staged = self.0.staged.lock().expect("暂存区锁未中毒");
            if staged.is_empty() {
                return Ok("当前暂存区本无改动".into());
            }
            let keys: Vec<String> = staged.keys().cloned().collect();
            staged.clear();
            return Ok(format!(
                "已撤销全部暂存改动（共 {} 项：{}），各文件已恢复为磁盘原状",
                keys.len(),
                keys.join("、")
            ));
        }
        let Some(relative) = path_opt else {
            return Err("须指定 `path`（单文件路径）或 `all: true`（清空全部暂存）".into());
        };
        let (_, key) = self.0.resolve_write(&relative)?;
        let mut staged = self.0.staged.lock().expect("暂存区锁未中毒");
        let Some(removed) = staged.remove(&key) else {
            return Ok(format!("{key} 当前无暂存改动"));
        };
        if removed.deleted {
            Ok(format!("已撤销对 {key} 的暂存删除，恢复为磁盘原状"))
        } else if removed.before.is_none() {
            Ok(format!("已撤销暂存新建的 {key}"))
        } else {
            Ok(format!("已撤销 {key} 的暂存改动，恢复为磁盘原状"))
        }
    }
}
