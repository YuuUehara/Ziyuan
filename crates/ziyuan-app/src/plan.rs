//! 条陈：先以只读诏令拟定方案，经皇帝批复后再按方案收窄范围授写权下发工部。

use std::collections::BTreeSet;
use std::path::{Component, Path};
use ziyuan_core::{Capability, Department, Edict, Seal, TaskId};
use ziyuan_governance::{GovernanceError, PLAN_TEMPLATE, Task, TaskStatus};
use ziyuan_storage::EventStore;

use crate::censorate::citations;
use crate::{AppError, Session, truncate};

const DENIED: &[&str] = &[".ziyuan", ".git", ".jj"];
const MAX_PATHS: usize = 32;

/// 从已完成只读诏令（条陈）中提炼出的工部写诏令预填方案。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanDraft {
    /// 原条陈诏令 ID。
    pub source_task: TaskId,
    /// 原诏令是否按内置「条陈」模板下达。
    pub is_plan_template: bool,
    /// 从条陈中识别出的目标路径（相对项目根，已过滤越界与禁区）。
    pub mentioned_paths: Vec<String>,
    /// 收窄后的工部玉玺范围（均已在磁盘存在；拟新建文件已收拢到最近已存在父目录）。
    pub scope: Vec<String>,
    /// 未能从条陈中提取到有效路径、已回退到原诏令范围时为 `true`。
    pub fallback_scope: bool,
    /// 拟新建（盘上尚不存在）而被收拢到父目录的路径，供前端提示皇帝。
    pub new_paths: Vec<String>,
}

/// 一件诏令是否可作为条陈转授工部写权：须已完成、原本未授写权限、无呈报改动，且非译书任务。
pub fn can_execute_from_plan(task: &Task) -> bool {
    matches!(task.status, TaskStatus::Completed { .. })
        && !task
            .edict
            .seal
            .capabilities
            .contains(&Capability::WriteFile)
        && task.changes.is_none()
        && task.translation.is_none()
}

/// 从已完成的只读诏令中提取拟改路径并收窄工部写权范围（不修改事件簿）。
pub fn draft_plan_execution(task: &Task, root: &Path) -> Result<PlanDraft, AppError> {
    if !matches!(task.status, TaskStatus::Completed { .. }) {
        return Err(GovernanceError::Court(format!(
            "诏令 #{} 尚未完成，不能依其条陈授写权",
            task.edict.id.0
        ))
        .into());
    }
    if task.translation.is_some() {
        return Err(GovernanceError::Court(format!(
            "诏令 #{} 为译书诏令，请经译书流程处置",
            task.edict.id.0
        ))
        .into());
    }
    if task
        .edict
        .seal
        .capabilities
        .contains(&Capability::WriteFile)
        || task.changes.is_some()
    {
        return Err(GovernanceError::Court(format!(
            "诏令 #{} 本身已有写权限，不是只读条陈",
            task.edict.id.0
        ))
        .into());
    }
    let summary = match &task.status {
        TaskStatus::Completed { summary, .. } => summary.as_ref(),
        _ => "",
    };
    let is_plan_template = task
        .template
        .as_ref()
        .is_some_and(|t| t.name == PLAN_TEMPLATE);

    let section_paths = extract_section_paths(summary);
    let from_section = !section_paths.is_empty();
    let raw_paths = if from_section {
        section_paths
    } else {
        extract_fallback_paths(summary)
    };

    let mut mentioned_paths = Vec::new();
    let mut scope = Vec::new();
    let mut new_paths = Vec::new();
    for path in raw_paths {
        if root.join(&path).exists() {
            push_unique(&mut mentioned_paths, path.clone());
            push_unique(&mut scope, path);
        } else if from_section {
            // 仅在显式「拟改路径」段内允许尚不存在的拟新建文件，并收拢到最近已存在父目录。
            let resolved = nearest_existing_parent(root, &path);
            push_unique(&mut mentioned_paths, path.clone());
            push_unique(&mut new_paths, path);
            push_unique(&mut scope, resolved);
        }
    }

    let scope = collapse_scope(scope);
    let (scope, fallback_scope) = if scope.is_empty() {
        (task.edict.seal.scope.clone(), true)
    } else {
        (scope, false)
    };

    Ok(PlanDraft {
        source_task: task.edict.id,
        is_plan_template,
        mentioned_paths,
        scope,
        fallback_scope,
        new_paths,
    })
}

/// 组装依条陈实施的工部诏令正文。
pub fn plan_edict_text(plan_task: &Task, scope: &[String], extra: &str) -> String {
    let summary = match &plan_task.status {
        TaskStatus::Completed { summary, .. } => summary.as_ref(),
        _ => "",
    };
    let mut out = format!(
        "依条陈诏令 #{} 实施改动（收窄范围：{}）：\n原诏令：{}\n\n已批复条陈（请严格按此方案实施，勿超范围改动）：\n{}",
        plan_task.edict.id.0,
        scope.join("、"),
        truncate(plan_task.edict.text.trim(), 600),
        truncate(summary.trim(), 4000),
    );
    let extra = extra.trim();
    if !extra.is_empty() {
        out.push_str(&format!("\n\n皇帝补充指示：{extra}"));
    }
    out
}

pub(crate) fn push_unique(list: &mut Vec<String>, item: String) {
    if !list.contains(&item) {
        list.push(item);
    }
}

fn is_path_section_heading(line: &str) -> Option<&str> {
    let stripped = line
        .trim()
        .trim_start_matches(['#', '*', '-', '>', '•', '·', ' '])
        .trim_end_matches(['*', ' ']);
    const HEADERS: &[&str] = &[
        "拟改路径",
        "修改路径",
        "拟改文件",
        "修改文件",
        "涉及文件",
        "改动文件",
        "拟修改路径",
        "拟修改文件",
    ];
    for header in HEADERS {
        if let Some(rest) = stripped.strip_prefix(header) {
            let rest = rest.trim_start_matches(['*', ' ']);
            if rest.is_empty() {
                return Some("");
            }
            if let Some(after) = rest.strip_prefix('：').or_else(|| rest.strip_prefix(':')) {
                return Some(after.trim());
            }
        }
    }
    None
}

fn is_other_section_heading(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return true;
    }
    let clean = trimmed
        .trim_start_matches(['*', '>', ' '])
        .trim_end_matches(['*', ' ']);
    const STOPS: &[&str] = &[
        "改动方案",
        "修改方案",
        "实施方案",
        "验证方案",
        "测试方案",
        "风险",
        "潜在风险",
        "边界",
        "结论",
        "依据",
        "未办之事",
        "说明",
    ];
    STOPS.iter().any(|s| {
        clean.strip_prefix(s).is_some_and(|rest| {
            let r = rest.trim_start_matches(['*', ' ']);
            r.is_empty() || r.starts_with(['：', ':'])
        })
    })
}

fn extract_section_paths(summary: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_section = false;
    let mut collected_in_section = false;
    for line in summary.lines() {
        if let Some(inline) = is_path_section_heading(line) {
            in_section = true;
            collected_in_section = false;
            if !inline.is_empty() {
                for piece in inline.split(['，', ',', '、', '；', ';']) {
                    if let Some(p) = sanitize_path_candidate(piece, true) {
                        push_unique(&mut out, p);
                        collected_in_section = true;
                        if out.len() >= MAX_PATHS {
                            return out;
                        }
                    }
                }
            }
            continue;
        }
        if !in_section {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if collected_in_section {
                in_section = false;
            }
            continue;
        }
        if is_other_section_heading(trimmed) {
            in_section = false;
            continue;
        }
        if trimmed.starts_with("```") {
            continue;
        }
        // 去掉列表符号后取首个路径片段。
        let item = strip_list_prefix(trimmed);
        if let Some(p) = sanitize_path_candidate(item, true) {
            push_unique(&mut out, p);
            collected_in_section = true;
            if out.len() >= MAX_PATHS {
                return out;
            }
        }
    }
    out
}

fn extract_fallback_paths(summary: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (path, _) in citations(summary) {
        if let Some(clean) = sanitize_path_candidate(&path, false) {
            push_unique(&mut out, clean);
            if out.len() >= MAX_PATHS {
                break;
            }
        }
    }
    out
}

fn strip_list_prefix(line: &str) -> &str {
    let mut s = line.trim();
    loop {
        let trimmed = s.trim_start_matches(['-', '*', '•', '·', ' ']);
        let after_num = trimmed.trim_start_matches(|c: char| c.is_ascii_digit());
        if after_num.len() != trimmed.len() && after_num.starts_with(['.', '、', ')', '）']) {
            let first = after_num.chars().next().expect("非空").len_utf8();
            s = after_num[first..].trim_start();
            continue;
        }
        if trimmed.len() != s.len() {
            s = trimmed;
            continue;
        }
        break;
    }
    s
}

fn sanitize_path_candidate(raw: &str, allow_plain_name: bool) -> Option<String> {
    let mut s = raw.trim();
    // 若以反引号开头，取第一对反引号内的路径。
    if let Some(rest) = s.strip_prefix('`') {
        if let Some((inside, _)) = rest.split_once('`') {
            s = inside.trim();
        } else {
            s = rest.trim();
        }
    } else {
        // 截去路径后的说明（空白、括号、破折号、中文冒号）。
        if let Some(idx) = s.find(|c: char| {
            c.is_whitespace() || matches!(c, '（' | '(' | '—' | '：' | '，' | ',' | '；' | ';')
        }) {
            s = &s[..idx];
        }
    }
    s = s
        .trim_matches(['`', '"', '\'', '「', '」', '《', '》', '。'])
        .trim();
    // 去掉行号后缀（`path:12` 或 `path:12-30`），但保留盘符探测以供拒绝。
    if let Some((before, after)) = s.rsplit_once(':') {
        let is_lines = !after.is_empty() && after.bytes().all(|b| b.is_ascii_digit() || b == b'-');
        if is_lines && !before.is_empty() {
            s = before;
        }
    }
    let normalized = s.replace('\\', "/");
    let normalized = normalized.trim_start_matches("./").trim_end_matches('/');
    if normalized.is_empty() || normalized == "." {
        return None;
    }
    if normalized.starts_with('/') || normalized.contains(':') {
        return None;
    }
    if normalized
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let path = Path::new(normalized);
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    if normalized
        .split('/')
        .any(|seg| DENIED.contains(&seg) || seg.starts_with(".env"))
    {
        return None;
    }
    if !normalized.contains(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let looks_like_file = normalized.contains('/')
        || normalized.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty() && ext.len() >= 2 && ext.starts_with(|c: char| c.is_ascii_alphabetic())
        });
    if !looks_like_file && !allow_plain_name {
        return None;
    }
    Some(normalized.to_owned())
}

pub(crate) fn nearest_existing_parent(root: &Path, relative: &str) -> String {
    let mut cur = Path::new(relative).parent();
    while let Some(parent) = cur {
        let text = parent.to_string_lossy().replace('\\', "/");
        if text.is_empty() {
            break;
        }
        if root.join(parent).exists() {
            return text;
        }
        cur = parent.parent();
    }
    ".".to_owned()
}

/// 若已包含某个祖先目录（含 `.`），则去掉其下冗余的子路径，保持顺序稳定。
pub(crate) fn collapse_scope(paths: Vec<String>) -> Vec<String> {
    if paths.iter().any(|p| p == ".") {
        return vec![".".to_owned()];
    }
    fn parts(p: &str) -> Vec<&str> {
        p.split('/')
            .filter(|seg| !seg.is_empty() && *seg != ".")
            .collect()
    }
    let mut out = Vec::new();
    for p in &paths {
        let p_parts = parts(p);
        let covered = paths.iter().any(|other| {
            if other == p {
                return false;
            }
            let o_parts = parts(other);
            o_parts.len() < p_parts.len() && p_parts.starts_with(&o_parts)
        });
        if !covered {
            push_unique(&mut out, p.clone());
        }
    }
    out
}

pub(crate) fn validate_scope_override(
    root: &Path,
    custom: Vec<String>,
) -> Result<Vec<String>, AppError> {
    let mut normalized = Vec::new();
    for raw in custom {
        let p = raw.trim().replace('\\', "/");
        if p.is_empty() {
            return Err(GovernanceError::InvalidScope(raw).into());
        }
        let path = Path::new(&p);
        let escapes = path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir));
        if escapes
            || p.split('/')
                .any(|seg| DENIED.contains(&seg) || seg.starts_with(".env"))
        {
            return Err(GovernanceError::InvalidScope(p).into());
        }
        if !root.join(&p).exists() {
            return Err(GovernanceError::Court(format!("查阅范围不存在：{p}")).into());
        }
        push_unique(&mut normalized, p);
    }
    Ok(normalized)
}

impl<S: EventStore> Session<S> {
    /// 依已完成的只读诏令（条陈）下发一件工部写诏令：自动收窄查阅范围（可由皇帝通过
    /// `scope_override` 改定），授予 `read_file`、`search_code`、`write_file`（以及可选
    /// `run_tests`）。改动仍须暂存、经皇帝逐段准奏后才落盘。
    pub fn issue_from_plan(
        &mut self,
        plan_task: TaskId,
        root: &Path,
        scope_override: Option<Vec<String>>,
        run_tests: bool,
        extra: String,
    ) -> Result<(Edict, PlanDraft), AppError> {
        self.reload()?;
        let entry = self
            .court
            .task(plan_task)
            .ok_or(GovernanceError::UnknownEdict(plan_task))?
            .clone();
        let mut draft = draft_plan_execution(&entry, root)?;
        if let Some(custom) = scope_override
            && !custom.is_empty()
        {
            draft.scope = validate_scope_override(root, custom)?;
            draft.fallback_scope = false;
        }
        let mut capabilities: BTreeSet<Capability> = [
            Capability::ReadFile,
            Capability::SearchCode,
            Capability::WriteFile,
        ]
        .into();
        if run_tests {
            capabilities.insert(Capability::RunTests);
        }
        let seal = Seal {
            capabilities,
            scope: draft.scope.clone(),
            mcp: BTreeSet::new(),
        };
        let text = plan_edict_text(&entry, &draft.scope, &extra);
        let edict = self.issue(Department::Works, text, seal)?;
        Ok((edict, draft))
    }
}
