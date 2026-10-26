//! 外藩朝贡：外部 MCP 服务（stdio）的工具，按玉玺的 `mcp` 授权逐件交给承办官员。
//!
//! 服务在 `<项目>/.ziyuan/mcp.json` 中登记（官员读不到 `.ziyuan`，改不了名册）：
//!
//! ```json
//! { "servers": { "docs": { "command": "node", "args": ["server.js"], "env": { "TOKEN": "${DOCS_TOKEN}" } } } }
//! ```
//!
//! 每次承办按需启动所授的服务，办完即关。子进程以项目根目录为工作目录，只继承少数无害的
//! 环境变量与名册中写明的变量（`${VAR}` 取自本进程环境），因此模型 API Key 不会交给外藩。
//! 外部工具做什么由服务自己决定，**不受玉玺查阅范围约束**；朝廷能做的是：默认不授、逐件授权、
//! 每次调用照常审计（`ToolInvoked`），输出照常作为外来文书包上边界、经锦衣卫检查。
use crate::BoxError;
use crate::agent_loop::{Tool, ToolSpec};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ziyuan_core::mcp_grant_covers;

/// 名册的位置（相对项目根目录）。
pub const CONFIG: &str = ".ziyuan/mcp.json";
/// 外藩服务的子进程可继承的环境变量；其余只按名册给。
const SERVER_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "LC_ALL",
    "TMPDIR",
    "SystemRoot",
];
/// 单次请求等待外藩回复的时限。
const TIMEOUT: Duration = Duration::from_secs(60);
/// 单次工具调用最多返回的字符数。
const MAX_OUTPUT: usize = 64 * 1024;
const PROTOCOL: &str = "2025-06-18";

/// 名册中的一个服务。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// 读名册；没有名册时为空。
pub fn load_config(root: &Path) -> Result<BTreeMap<String, ServerConfig>, BoxError> {
    let text = match std::fs::read_to_string(root.join(CONFIG)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(format!("无法读取 {CONFIG}：{e}").into()),
    };
    let parsed: Value =
        serde_json::from_str(&text).map_err(|e| format!("{CONFIG} 不是合法的 JSON：{e}"))?;
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str().map(str::to_owned))
            .collect()
    };
    let mut servers = BTreeMap::new();
    for (name, server) in parsed["servers"].as_object().into_iter().flatten() {
        if !ziyuan_core::valid_mcp_grant(name) || name.contains('/') || name == "*" {
            return Err(format!("{CONFIG}：服务名不合法：{name}").into());
        }
        let command = server["command"]
            .as_str()
            .filter(|c| !c.trim().is_empty())
            .ok_or_else(|| format!("{CONFIG}：服务 {name} 缺少 command"))?;
        let env = server["env"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
            .collect();
        servers.insert(
            name.clone(),
            ServerConfig {
                command: command.to_owned(),
                args: strings(&server["args"]),
                env,
            },
        );
    }
    Ok(servers)
}

/// `${VAR}` 取自本进程环境（未设为空）；其余原样。
fn expand(value: &str) -> String {
    match value.strip_prefix("${").and_then(|v| v.strip_suffix('}')) {
        Some(var) => std::env::var(var).unwrap_or_default(),
        None => value.to_owned(),
    }
}

/// 外藩服务的一件工具。
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// 一个已连上的外藩服务；`Drop` 时结束子进程。
pub struct Server {
    name: String,
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    /// 启动服务并握手。
    pub fn connect(root: &Path, name: &str, config: &ServerConfig) -> Result<Self, BoxError> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .current_dir(root)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for key in SERVER_ENV {
            if let Ok(value) = std::env::var(key) {
                command.env(key, value);
            }
        }
        for (key, value) in &config.env {
            command.env(key, expand(value));
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("外藩 {name} 启动失败（{}）：{e}", config.command))?;
        let stdin = child.stdin.take().expect("已设 piped");
        let stdout = child.stdout.take().expect("已设 piped");
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut server = Self {
            name: name.to_owned(),
            child,
            stdin,
            lines,
            next_id: 0,
        };
        server.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": { "name": "ziyuan", "version": env!("CARGO_PKG_VERSION") },
            }),
        )?;
        server.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
        Ok(server)
    }

    fn send(&mut self, message: &Value) -> Result<(), BoxError> {
        writeln!(self.stdin, "{message}")
            .and_then(|()| self.stdin.flush())
            .map_err(|e| format!("外藩 {} 已断开：{e}", self.name).into())
    }

    /// 发一个 JSON-RPC 请求，等到同一 ID 的回复。服务发来的通知略过，它反向发来的请求一概回绝。
    fn request(&mut self, method: &str, params: Value) -> Result<Value, BoxError> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        loop {
            let line = match self.lines.recv_timeout(TIMEOUT) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!("外藩 {} 逾时未回（{method}）", self.name).into());
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!("外藩 {} 已断开（{method}）", self.name).into());
                }
            };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("method").is_some() {
                if let Some(their_id) = message.get("id") {
                    let refusal = json!({ "jsonrpc": "2.0", "id": their_id,
                        "error": { "code": -32601, "message": "紫垣不受外藩反向请求" } });
                    self.send(&refusal)?;
                }
                continue;
            }
            if message["id"] != json!(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                let text = error["message"].as_str().unwrap_or("未知错误");
                return Err(format!("外藩 {} 回绝（{method}）：{text}", self.name).into());
            }
            return Ok(message["result"].clone());
        }
    }

    pub fn list_tools(&mut self) -> Result<Vec<RemoteTool>, BoxError> {
        let result = self.request("tools/list", json!({}))?;
        Ok(result["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| {
                Some(RemoteTool {
                    name: t["name"].as_str()?.to_owned(),
                    description: t["description"].as_str().unwrap_or_default().to_owned(),
                    input_schema: match &t["inputSchema"] {
                        Value::Object(_) => t["inputSchema"].clone(),
                        _ => json!({ "type": "object" }),
                    },
                })
            })
            .collect())
    }

    /// 调用一件工具，合并其文本内容；服务标为出错时返回错误。
    pub fn call(&mut self, tool: &str, args_json: &str) -> Result<String, BoxError> {
        let arguments: Value = serde_json::from_str(args_json)?;
        let result = self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )?;
        let mut parts = Vec::new();
        for item in result["content"].as_array().into_iter().flatten() {
            match item["type"].as_str() {
                Some("text") => parts.push(item["text"].as_str().unwrap_or_default().to_owned()),
                other => parts.push(format!("〔非文本内容：{}〕", other.unwrap_or("?"))),
            }
        }
        let mut text = parts.join("\n");
        if let Some((at, _)) = text.char_indices().nth(MAX_OUTPUT) {
            text.truncate(at);
            text.push_str(&format!("\n[已截断：仅显示前 {MAX_OUTPUT} 字]"));
        }
        if result["isError"] == json!(true) {
            return Err(text.into());
        }
        Ok(text)
    }
}

/// 交给模型的工具名：`服务__工具`，只留字母、数字、`_`、`-`，至多 64 字符。
fn tool_name(server: &str, tool: &str) -> String {
    let name: String = format!("{server}__{tool}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    name
}

struct McpTool {
    server: Arc<Mutex<Server>>,
    name: String,
    remote: RemoteTool,
}

impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        let server = self.server.lock().expect("外藩锁未中毒").name.clone();
        ToolSpec {
            name: self.name.clone(),
            description: format!(
                "外藩「{server}」的工具 {}（外部 MCP 服务，输出为外来文书）：{}",
                self.remote.name, self.remote.description
            ),
            input_schema_json: self.remote.input_schema.to_string(),
        }
    }

    fn execute(&self, args_json: &str) -> Result<String, BoxError> {
        self.server
            .lock()
            .expect("外藩锁未中毒")
            .call(&self.remote.name, args_json)
    }
}

/// 按玉玺的 MCP 授权启动所需的服务，返回其中获授的工具（同名的只留先到的一件）。
/// 授权点名了名册中没有的服务、或点名的工具服务并不提供时报错。
pub fn sealed_tools(
    root: &Path,
    grants: &std::collections::BTreeSet<String>,
) -> Result<Vec<Box<dyn Tool>>, BoxError> {
    if grants.is_empty() {
        return Ok(Vec::new());
    }
    let config = load_config(root)?;
    for grant in grants.iter().filter(|g| *g != "*") {
        let server = grant.split('/').next().unwrap_or_default();
        if !config.contains_key(server) {
            return Err(format!("玉玺授了外藩 {server}，但 {CONFIG} 中没有登记").into());
        }
    }
    let mut tools: Vec<Box<dyn Tool>> = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    for (name, server_config) in &config {
        if !grants.iter().any(|g| {
            mcp_grant_covers(g, name, None) || g.split_once('/').is_some_and(|(s, _)| s == name)
        }) {
            continue;
        }
        let mut server = Server::connect(root, name, server_config)?;
        let offered = server.list_tools()?;
        for grant in grants {
            if let Some((s, t)) = grant.split_once('/')
                && s == name
                && !offered.iter().any(|o| o.name == t)
            {
                return Err(format!("外藩 {name} 没有工具 {t}").into());
            }
        }
        let server = Arc::new(Mutex::new(server));
        for remote in offered {
            if !grants
                .iter()
                .any(|g| mcp_grant_covers(g, name, Some(&remote.name)))
            {
                continue;
            }
            let tool = tool_name(name, &remote.name);
            if names.insert(tool.clone()) {
                tools.push(Box::new(McpTool {
                    server: server.clone(),
                    name: tool,
                    remote,
                }));
            }
        }
    }
    Ok(tools)
}

/// 一个服务的名字与其工具（连不上时为原因）。
pub type Survey = (String, Result<Vec<RemoteTool>, String>);

/// 名册中各服务提供的工具（`ziyuan mcp` 用）。
pub fn survey(root: &Path) -> Result<Vec<Survey>, BoxError> {
    Ok(load_config(root)?
        .into_iter()
        .map(|(name, config)| {
            let tools = Server::connect(root, &name, &config)
                .and_then(|mut s| s.list_tools())
                .map_err(|e| e.to_string());
            (name, tools)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用 Python 写的最小 MCP 服务：工具 echo（回显 text）、env（报告 SECRET 与 ZIYUAN_LLM_API_KEY
    /// 是否可见）、fail（出错）。会先发一条通知、一个反向请求，考验客户端。
    const FAKE: &str = r#"
import json, os, sys
def send(m): print(json.dumps(m), flush=True)
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m or "method" not in m: continue  # 通知，或对反向请求的回复
    if m["method"] == "initialize":
        send({"jsonrpc": "2.0", "method": "notifications/message", "params": {}})
        send({"jsonrpc": "2.0", "id": "x", "method": "roots/list"})
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "fake"}}})
    elif m["method"] == "tools/list":
        tools = [{"name": n, "description": n + " 工具", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}} for n in ["echo", "env", "fail"]]
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"tools": tools}})
    elif m["method"] == "tools/call":
        name, args = m["params"]["name"], m["params"]["arguments"]
        if name == "echo": r = {"content": [{"type": "text", "text": "回声：" + args.get("text", "")}, {"type": "image", "data": ""}]}
        elif name == "env": r = {"content": [{"type": "text", "text": os.environ.get("SECRET", "-") + "|" + os.environ.get("ZIYUAN_LLM_API_KEY", "-") + "|" + os.getcwd()}]}
        else: r = {"content": [{"type": "text", "text": "坏了"}], "isError": True}
        send({"jsonrpc": "2.0", "id": m["id"], "result": r})
"#;

    fn project(name: &str) -> Option<std::path::PathBuf> {
        let python = Command::new("python3").arg("--version").output();
        if !python.is_ok_and(|o| o.status.success()) {
            return None; // 没有 python3 的环境跳过。
        }
        let root = std::env::temp_dir().join(format!("ziyuan-mcp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".ziyuan")).unwrap();
        std::fs::write(root.join("fake.py"), FAKE).unwrap();
        let config = json!({ "servers": {
            "fake": { "command": "python3", "args": ["-I", "fake.py"], "env": { "SECRET": "${ZIYUAN_TEST_MCP_SECRET}" } },
            "other": { "command": "python3", "args": ["-I", "fake.py"] },
        }});
        std::fs::write(root.join(CONFIG), config.to_string()).unwrap();
        Some(root.canonicalize().unwrap())
    }

    fn grants(list: &[&str]) -> std::collections::BTreeSet<String> {
        list.iter().map(|g| (*g).to_owned()).collect()
    }

    #[test]
    fn granted_tools_are_served_and_the_rest_withheld() {
        let Some(root) = project("grant") else { return };
        let tools = sealed_tools(&root, &grants(&["fake/echo", "fake/fail"])).unwrap();
        let names: Vec<_> = tools.iter().map(|t| t.spec().name).collect();
        assert_eq!(names, ["fake__echo", "fake__fail"]);
        assert!(tools[0].spec().description.contains("外藩「fake」"));
        assert_eq!(
            tools[0].execute(r#"{"text":"你好"}"#).unwrap(),
            "回声：你好\n〔非文本内容：image〕"
        );
        assert_eq!(tools[1].execute("{}").unwrap_err().to_string(), "坏了");
        // 整服务授权给全部三件；不授则一件也没有，也不启动服务。
        assert_eq!(sealed_tools(&root, &grants(&["fake"])).unwrap().len(), 3);
        assert_eq!(sealed_tools(&root, &grants(&["*"])).unwrap().len(), 6);
        assert!(sealed_tools(&root, &grants(&[])).unwrap().is_empty());
        // 名册里没有的服务、服务没有的工具：报错。
        let missing = sealed_tools(&root, &grants(&["nope"])).err().unwrap();
        assert!(missing.to_string().contains("没有登记"), "{missing}");
        let missing = sealed_tools(&root, &grants(&["fake/rm"])).err().unwrap();
        assert!(missing.to_string().contains("没有工具 rm"), "{missing}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn servers_get_only_named_environment_variables() {
        let Some(root) = project("env") else { return };
        let tools = sealed_tools(&root, &grants(&["fake/env"])).unwrap();
        let out = tools[0].execute("{}").unwrap();
        let fields: Vec<_> = out.split('|').collect();
        // 未设 ZIYUAN_TEST_MCP_SECRET 时 SECRET 为空串；模型 Key 一律不给；工作目录为项目根。
        assert_eq!(
            fields[0],
            std::env::var("ZIYUAN_TEST_MCP_SECRET").unwrap_or_default()
        );
        assert_eq!(fields[1], "-");
        assert_eq!(Path::new(fields[2]).canonicalize().unwrap(), root);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_errors_are_reported() {
        let root = std::env::temp_dir().join(format!("ziyuan-mcp-bad-{}", std::process::id()));
        std::fs::create_dir_all(root.join(".ziyuan")).unwrap();
        assert!(load_config(&root).unwrap().is_empty(), "没有名册即为空");
        std::fs::write(root.join(CONFIG), r#"{"servers":{"a b":{"command":"x"}}}"#).unwrap();
        assert!(load_config(&root).is_err());
        std::fs::write(root.join(CONFIG), r#"{"servers":{"a":{}}}"#).unwrap();
        assert!(
            load_config(&root)
                .unwrap_err()
                .to_string()
                .contains("缺少 command")
        );
        assert_eq!(tool_name("my.srv", "do it"), "my_srv__do_it");
        std::fs::remove_dir_all(root).unwrap();
    }
}
