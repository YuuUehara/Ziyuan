//! 只读工作区工具。可用的工具与可查阅的路径由诏令的玉玺（[`Seal`]）决定。
//!
//! 所有路径先 canonicalize 再检查，因此 `..`、绝对路径与符号链接都无法越出玉玺的查阅范围。
//! 无论玉玺如何，事件簿与版本库元数据（`.ziyuan`、`.git`、`.jj`）及 `.env*` 文件一律不可读。
use crate::BoxError;
use crate::agent_loop::{Tool, ToolSpec};
use serde_json::Value;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use ziyuan_core::{Capability, Seal};

/// 单次 `read_file` 最多返回的字节数。
const MAX_READ_BYTES: usize = 64 * 1024;
/// 单次 `search_code` 最多返回的匹配行数。
const MAX_MATCHES: usize = 50;
/// 永远不可查阅的目录：朝廷事件簿与版本库元数据。
const DENIED_DIRS: &[&str] = &[".ziyuan", ".git", ".jj"];
/// 搜索时跳过（但可直接读取）的目录：构建产物。
const SKIPPED_DIRS: &[&str] = &["target", "node_modules"];

/// 按玉玺构造工具：只包含其授予的工具，且都限定在其查阅范围内。
/// 查阅范围不存在、越出 `root` 或落在禁区时返回错误。
pub fn sealed_tools(root: &Path, seal: &Seal) -> Result<Vec<Box<dyn Tool>>, BoxError> {
    let sandbox = Arc::new(Sandbox::new(root, &seal.scope)?);
    Ok(seal
        .capabilities
        .iter()
        .map(|capability| -> Box<dyn Tool> {
            match capability {
                Capability::ReadFile => Box::new(ReadFile(sandbox.clone())),
                Capability::SearchCode => Box::new(SearchCode(sandbox.clone())),
            }
        })
        .collect())
}

/// 项目根目录与玉玺查阅范围（均为 canonical 路径；互相包含的范围已合并）。
struct Sandbox {
    root: PathBuf,
    scope: Vec<PathBuf>,
}

impl Sandbox {
    fn new(root: &Path, scope: &[String]) -> Result<Self, BoxError> {
        let root = root.canonicalize()?;
        let mut entries = Vec::new();
        for path in scope {
            let resolved = root
                .join(path)
                .canonicalize()
                .map_err(|e| format!("查阅范围 {path} 无法访问: {e}"))?;
            if !resolved.starts_with(&root) || is_denied(&root, &resolved) {
                return Err(format!("查阅范围越出项目或属于禁区: {path}").into());
            }
            entries.push(resolved);
        }
        // 排序后父目录在子路径之前，去掉已被覆盖的范围，避免搜索结果重复。
        entries.sort();
        let mut scope: Vec<PathBuf> = Vec::new();
        for entry in entries {
            if !scope.iter().any(|kept| entry.starts_with(kept)) {
                scope.push(entry);
            }
        }
        Ok(Self { root, scope })
    }

    fn resolve(&self, relative: &str) -> Result<PathBuf, BoxError> {
        let path = self.root.join(relative).canonicalize()?;
        if !self.scope.iter().any(|s| path.starts_with(s)) || is_denied(&self.root, &path) {
            return Err(format!("拒绝访问玉玺查阅范围之外的路径: {relative}").into());
        }
        Ok(path)
    }

    fn display(&self, path: &Path) -> String {
        match path.strip_prefix(&self.root) {
            Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
            Ok(rel) => rel.display().to_string(),
            Err(_) => path.display().to_string(),
        }
    }

    fn scope_label(&self) -> String {
        let labels: Vec<_> = self.scope.iter().map(|p| self.display(p)).collect();
        labels.join("、")
    }

    /// 深度优先搜索；返回是否因达到上限而提前停止。不进入符号链接。
    fn search(&self, path: &Path, query: &str, out: &mut Vec<String>) -> io::Result<bool> {
        if !path.is_dir() {
            return Ok(self.search_file(path, query, out));
        }
        let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let file_type = entry.file_type()?;
            let name = entry.file_name();
            let skipped = SKIPPED_DIRS.iter().any(|d| name == *d);
            let truncated = if file_type.is_dir() && !skipped {
                !is_denied(&self.root, &entry.path()) && self.search(&entry.path(), query, out)?
            } else if file_type.is_file() {
                self.search_file(&entry.path(), query, out)
            } else {
                false
            };
            if truncated {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn search_file(&self, path: &Path, query: &str, out: &mut Vec<String>) -> bool {
        // 禁区文件与非 UTF-8（二进制）文件直接跳过。
        if is_denied(&self.root, path) {
            return false;
        }
        let Ok(text) = fs::read_to_string(path) else {
            return false;
        };
        let relative = self.display(path);
        for (number, line) in text.lines().enumerate() {
            if line.contains(query) {
                if out.len() == MAX_MATCHES {
                    return true;
                }
                out.push(format!("{relative}:{}: {}", number + 1, line.trim()));
            }
        }
        false
    }
}

/// `path` 相对 `root` 的任一组成部分是禁区目录或 `.env*`。
fn is_denied(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .any(|c| {
            let name = c.as_os_str().to_string_lossy();
            DENIED_DIRS.contains(&name.as_ref()) || name.starts_with(".env")
        })
}

struct ReadFile(Arc<Sandbox>);

impl Tool for ReadFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Capability::ReadFile.code().into(),
            description: format!(
                "读取项目内的文本文件（相对项目根目录的路径）。只能读取授权范围：{}。超过 {MAX_READ_BYTES} 字节会被截断。",
                self.0.scope_label()
            ),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string","description":"相对项目根目录的文件路径"}},"required":["path"],"additionalProperties":false}"#.into(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        let path = self.0.resolve(&string_arg(args_json, "path")?)?;
        let bytes = fs::read(&path)?;
        let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_READ_BYTES)]);
        if bytes.len() > MAX_READ_BYTES {
            Ok(format!("{text}\n[已截断：文件共 {} 字节]", bytes.len()))
        } else {
            Ok(text.into_owned())
        }
    }
}

struct SearchCode(Arc<Sandbox>);

impl Tool for SearchCode {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: Capability::SearchCode.code().into(),
            description: format!(
                "在授权范围（{}）内的文本文件中按字面量搜索（区分大小写），返回 `路径:行号: 内容`，最多 {MAX_MATCHES} 条。",
                self.0.scope_label()
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
        let mut truncated = false;
        for path in &self.0.scope {
            if self.0.search(path, &query, &mut matches)? {
                truncated = true;
                break;
            }
        }
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

fn string_arg(args_json: &str, key: &str) -> Result<String, BoxError> {
    let mut args: Value = serde_json::from_str(args_json)?;
    match args.get_mut(key).map(Value::take) {
        Some(Value::String(value)) => Ok(value),
        _ => Err(format!("缺少字符串参数 `{key}`").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
    }

    fn tool(root: &Path, seal: &Seal, name: &str) -> Box<dyn Tool> {
        sealed_tools(root, seal)
            .unwrap()
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
        for path in [
            "../secret.txt",
            outside.as_str(),
            "link.txt",
            ".env",
            ".ziyuan/court.log",
        ] {
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
    fn seal_limits_tools_and_scope() {
        let dir = workspace("seal");
        let root = dir.join("project");
        let only_read = sealed_tools(&root, &seal(&[Capability::ReadFile], &["src"])).unwrap();
        let names: Vec<_> = only_read.iter().map(|t| t.spec().name).collect();
        assert_eq!(names, ["read_file"]);
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

        assert!(sealed_tools(&root, &seal(&[], &["."])).unwrap().is_empty());
        #[cfg(unix)]
        std::os::unix::fs::symlink(&dir, root.join("up")).unwrap();
        for bad in [".ziyuan", "missing", "up"] {
            assert!(
                sealed_tools(&root, &seal(&Capability::ALL, &[bad])).is_err(),
                "{bad}"
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
