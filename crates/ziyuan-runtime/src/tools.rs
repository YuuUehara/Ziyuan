//! 只读工作区工具：限定在项目根目录内，不跟随指向根目录之外的符号链接。
use crate::BoxError;
use crate::agent_loop::{Tool, ToolSpec};
use serde_json::Value;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// 单次 `read_file` 最多返回的字节数。
const MAX_READ_BYTES: usize = 64 * 1024;
/// 单次 `search_code` 最多返回的匹配行数。
const MAX_MATCHES: usize = 50;
/// 搜索时跳过的目录：版本控制、构建产物与朝廷事件簿。
const SKIPPED_DIRS: &[&str] = &[".git", ".jj", ".ziyuan", "target", "node_modules"];

/// 以 `root` 为边界的只读工具集。
pub fn read_only_tools(root: &Path) -> io::Result<Vec<Box<dyn Tool>>> {
    let root = root.canonicalize()?;
    Ok(vec![
        Box::new(ReadFile { root: root.clone() }),
        Box::new(SearchCode { root }),
    ])
}

struct ReadFile {
    root: PathBuf,
}

impl Tool for ReadFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: format!(
                "读取项目内的文本文件（相对项目根目录的路径）。超过 {MAX_READ_BYTES} 字节会被截断。"
            ),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"相对项目根目录的文件路径"}},"required":["path"],"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let path = resolve(&self.root, &string_arg(args_json, "path")?)?;
        let bytes = fs::read(&path)?;
        let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_READ_BYTES)]);
        if bytes.len() > MAX_READ_BYTES {
            Ok(format!("{text}\n[已截断：文件共 {} 字节]", bytes.len()))
        } else {
            Ok(text.into_owned())
        }
    }
}

struct SearchCode {
    root: PathBuf,
}

impl Tool for SearchCode {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search_code".into(),
            description: format!(
                "在项目文本文件中按字面量搜索（区分大小写），返回 `路径:行号: 内容`，最多 {MAX_MATCHES} 条。"
            ),
            input_schema_json: r#"{"type":"object","properties":{"query":{"type":"string","description":"要查找的字面量文本"}},"required":["query"],"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let query = string_arg(args_json, "query")?;
        if query.is_empty() {
            return Err("query 不能为空".into());
        }
        let mut matches = Vec::new();
        let truncated = search(&self.root, &self.root, &query, &mut matches)?;
        if matches.is_empty() {
            return Ok("无匹配".into());
        }
        let mut out = matches.join("\n");
        if truncated {
            out.push_str(&format!("\n[已截断：仅显示前 {MAX_MATCHES} 条]"));
        }
        Ok(out)
    }
}

/// 深度优先遍历；返回是否因达到上限而提前停止。不进入符号链接。
fn search(root: &Path, dir: &Path, query: &str, out: &mut Vec<String>) -> io::Result<bool> {
    let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            if SKIPPED_DIRS.iter().any(|d| entry.file_name() == *d) {
                continue;
            }
            if search(root, &path, query, out)? {
                return Ok(true);
            }
        } else if file_type.is_file() {
            // 非 UTF-8（二进制）文件直接跳过。
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let relative = path.strip_prefix(root).unwrap_or(&path).display();
            for (number, line) in text.lines().enumerate() {
                if line.contains(query) {
                    if out.len() == MAX_MATCHES {
                        return Ok(true);
                    }
                    out.push(format!("{relative}:{}: {}", number + 1, line.trim()));
                }
            }
        }
    }
    Ok(false)
}

fn string_arg(args_json: &str, key: &str) -> Result<String, BoxError> {
    let mut args: Value = serde_json::from_str(args_json)?;
    match args.get_mut(key).map(Value::take) {
        Some(Value::String(value)) => Ok(value),
        _ => Err(format!("缺少字符串参数 `{key}`").into()),
    }
}

/// 把相对路径解析为根目录内的真实路径；拒绝绝对路径、`..` 逃逸和指向根目录外的符号链接。
fn resolve(root: &Path, relative: &str) -> Result<PathBuf, BoxError> {
    let path = root.join(relative).canonicalize()?;
    if !path.starts_with(root) {
        return Err(format!("拒绝访问项目根目录之外的路径: {relative}").into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ziyuan-tools-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("project/src")).unwrap();
        fs::create_dir_all(dir.join("project/target")).unwrap();
        fs::write(dir.join("secret.txt"), "outside").unwrap();
        fs::write(
            dir.join("project/src/main.rs"),
            "fn main() {\n    unsafe { run() }\n}\n",
        )
        .unwrap();
        fs::write(dir.join("project/target/gen.rs"), "unsafe { built }").unwrap();
        dir
    }

    fn tool(root: &Path, name: &str) -> Box<dyn Tool> {
        read_only_tools(root)
            .unwrap()
            .into_iter()
            .find(|t| t.spec().name == name)
            .unwrap()
    }

    #[test]
    fn read_file_stays_inside_root() {
        let dir = workspace("read");
        let root = dir.join("project");
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("secret.txt"), root.join("link.txt")).unwrap();
        let read = tool(&root, "read_file");

        assert!(
            read.execute(r#"{"path":"src/main.rs"}"#)
                .unwrap()
                .contains("unsafe")
        );
        let outside = dir.join("secret.txt").display().to_string();
        for path in ["../secret.txt", outside.as_str(), "link.txt"] {
            let args = serde_json::json!({ "path": path }).to_string();
            let err = read.execute(&args).unwrap_err().to_string();
            assert!(err.contains("拒绝访问"), "{path}: {err}");
        }
        assert!(read.execute(r#"{"file":"src/main.rs"}"#).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn search_reports_locations_and_skips_build_output() {
        let dir = workspace("search");
        let search = tool(&dir.join("project"), "search_code");
        assert_eq!(
            search.execute(r#"{"query":"unsafe"}"#).unwrap(),
            "src/main.rs:2: unsafe { run() }"
        );
        assert_eq!(search.execute(r#"{"query":"nothing"}"#).unwrap(), "无匹配");
        fs::remove_dir_all(dir).unwrap();
    }
}
