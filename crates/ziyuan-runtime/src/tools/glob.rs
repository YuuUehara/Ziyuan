//! 轻量路径 glob 匹配器：供 `list_files`、`search_code` 与 `find_references` 过滤相对路径。
//!
//! 将 glob 编译为受容量限制的线性时间正则（`regex::RegexBuilder`），避免路径回溯放大。
//! 支持 `*`（单段通配）、`?`（单字符）、`**`（跨零或多层目录）、`[abc]`／`[!a-z]` 字符类与 `{rs,toml}` 多选。
//! 模式中不含 `/` 时按文件名匹配（如 `*.rs` 可匹配任意子目录下的 `.rs` 文件）；含 `/` 时按 `/` 归一化的相对路径全匹配。

use crate::BoxError;
use regex::{Regex, RegexBuilder};

const MAX_GLOB_LEN: usize = 256;
const MAX_BRACE_ALTS: usize = 32;
const MAX_REGEX_SIZE: usize = 256 * 1024;

pub(super) struct GlobMatcher {
    regex: Regex,
    basename_only: bool,
}

impl GlobMatcher {
    pub(super) fn compile(raw: &str) -> Result<Self, BoxError> {
        let normalized = raw.trim().replace('\\', "/");
        let pattern = normalized.strip_prefix("./").unwrap_or(&normalized);
        if pattern.is_empty() {
            return Err("glob 不能为空".into());
        }
        if pattern.len() > MAX_GLOB_LEN {
            return Err(format!("glob 过长（最多 {MAX_GLOB_LEN} 字节）").into());
        }
        let basename_only = !pattern.contains('/');
        let mut expr = String::with_capacity(pattern.len() * 2 + 4);
        expr.push('^');
        translate_glob(pattern, &mut expr, false)?;
        expr.push('$');
        let regex = RegexBuilder::new(&expr)
            .size_limit(MAX_REGEX_SIZE)
            .build()
            .map_err(|e| format!("glob 模式无效（{raw}）：{e}"))?;
        Ok(Self {
            regex,
            basename_only,
        })
    }

    pub(super) fn is_match(&self, rel_path: &str) -> bool {
        let candidate = if self.basename_only {
            rel_path.rsplit('/').next().unwrap_or(rel_path)
        } else {
            rel_path
        };
        self.regex.is_match(candidate)
    }
}

fn translate_glob(pattern: &str, out: &mut String, in_brace: bool) -> Result<(), BoxError> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    // 折叠连续的 `*`
                    while i + 1 < chars.len() && chars[i + 1] == '*' {
                        i += 1;
                    }
                    if i + 1 < chars.len() && chars[i + 1] == '/' {
                        out.push_str("(?:.+/)?");
                        i += 2;
                        continue;
                    }
                    out.push_str(".*");
                } else {
                    out.push_str("[^/]*");
                }
            }
            '?' => out.push_str("[^/]"),
            '[' => {
                let start = i;
                i += 1;
                if i >= chars.len() {
                    return Err("glob 字符类 `[` 未闭合".into());
                }
                let negated = matches!(chars[i], '!' | '^');
                if negated {
                    i += 1;
                }
                let mut body = String::new();
                let mut closed = false;
                while i < chars.len() {
                    if chars[i] == ']' && !body.is_empty() {
                        closed = true;
                        break;
                    }
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                    }
                    if matches!(chars[i], '\\' | '[' | ']') {
                        body.push('\\');
                    }
                    body.push(chars[i]);
                    i += 1;
                }
                if !closed {
                    return Err(format!(
                        "glob 字符类未闭合：{}",
                        chars[start..].iter().collect::<String>()
                    )
                    .into());
                }
                out.push('[');
                if negated {
                    out.push('^');
                }
                out.push_str(&body);
                out.push(']');
            }
            '{' => {
                if in_brace {
                    return Err("glob 不支持嵌套 `{...}`".into());
                }
                let start = i + 1;
                let mut end = None;
                let mut j = start;
                while j < chars.len() {
                    if chars[j] == '}' {
                        end = Some(j);
                        break;
                    }
                    if chars[j] == '{' {
                        return Err("glob 不支持嵌套 `{...}`".into());
                    }
                    j += 1;
                }
                let Some(close) = end else {
                    return Err("glob 花括号 `{` 未闭合".into());
                };
                let inner: String = chars[start..close].iter().collect();
                let alts: Vec<&str> = inner.split(',').collect();
                if alts.len() > MAX_BRACE_ALTS {
                    return Err(format!("glob 花括号选项过多（最多 {MAX_BRACE_ALTS} 项）").into());
                }
                out.push_str("(?:");
                for (idx, alt) in alts.iter().enumerate() {
                    if idx > 0 {
                        out.push('|');
                    }
                    translate_glob(alt, out, true)?;
                }
                out.push(')');
                i = close;
            }
            '}' => return Err("glob 出现多余的 `}`".into()),
            c => {
                if is_regex_meta(c) {
                    out.push('\\');
                }
                out.push(c);
            }
        }
        i += 1;
    }
    Ok(())
}

fn is_regex_meta(c: char) -> bool {
    matches!(
        c,
        '.' | '+' | '(' | ')' | '|' | '^' | '$' | '\\' | '[' | ']' | '{' | '}' | '*' | '?'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_basenames_recursive_paths_braces_and_char_classes() {
        let rs = GlobMatcher::compile("*.rs").unwrap();
        assert!(rs.is_match("main.rs"));
        assert!(rs.is_match("crates/ziyuan-runtime/src/tools.rs"));
        assert!(!rs.is_match("crates/ziyuan-runtime/Cargo.toml"));

        let deep = GlobMatcher::compile("crates/**/*.rs").unwrap();
        assert!(deep.is_match("crates/a.rs"));
        assert!(deep.is_match("crates/ziyuan-runtime/src/tools.rs"));
        assert!(!deep.is_match("src/main.rs"));

        let braces = GlobMatcher::compile("*.{rs,toml}").unwrap();
        assert!(braces.is_match("src/lib.rs"));
        assert!(braces.is_match("Cargo.toml"));
        assert!(!braces.is_match("README.md"));

        let classes = GlobMatcher::compile("src/test_[0-9]?.py").unwrap();
        assert!(classes.is_match("src/test_1a.py"));
        assert!(!classes.is_match("src/test_ab.py"));
        assert!(!classes.is_match("src/sub/test_1a.py"));

        let neg = GlobMatcher::compile("src/[!_]*.rs").unwrap();
        assert!(neg.is_match("src/lib.rs"));
        assert!(!neg.is_match("src/_gen.rs"));

        let dot_slash = GlobMatcher::compile("./src/*.rs").unwrap();
        assert!(dot_slash.is_match("src/main.rs"));
        assert!(!dot_slash.is_match("src/sub/main.rs"));
    }

    #[test]
    fn glob_rejects_malformed_or_empty_patterns() {
        assert!(GlobMatcher::compile("").is_err());
        assert!(GlobMatcher::compile("   ").is_err());
        assert!(GlobMatcher::compile("*.[rs").is_err());
        assert!(GlobMatcher::compile("*.{rs,toml").is_err());
        assert!(GlobMatcher::compile("*.{a,{b,c}}").is_err());
        assert!(GlobMatcher::compile("a}b").is_err());
    }
}
