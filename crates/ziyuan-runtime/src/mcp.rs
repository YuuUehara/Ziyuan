//! 外藩朝贡：外部 MCP 服务的工具，按玉玺的 `mcp` 授权逐件交给承办官员。
//!
//! 服务在 `<项目>/.ziyuan/mcp.json` 中登记（官员读不到 `.ziyuan`，改不了名册），
//! 本地服务走 stdio，远端服务走 Streamable HTTP：
//!
//! ```json
//! { "servers": {
//!     "docs": { "command": "node", "args": ["server.js"], "env": { "TOKEN": "${DOCS_TOKEN}" } },
//!     "wiki": { "url": "https://mcp.example.com/mcp", "headers": { "Authorization": "Bearer ${WIKI_TOKEN}" } }
//! } }
//! ```
//!
//! 每次承办按需连上所授的服务，办完即断。stdio 子进程以项目根目录为工作目录，只继承少数无害的
//! 环境变量与名册中写明的变量；HTTP 服务只收到名册中写明的请求头。`${VAR}` 取自本进程环境，
//! 因此凭据不落在名册里，模型 API Key 也不会交给外藩。HTTP 服务须用 https（本机地址可用 http），
//! 以免凭据明文过网。
//! 外部工具做什么由服务自己决定，**不受玉玺查阅范围约束**；朝廷能做的是：默认不授、逐件授权、
//! 每次调用照常审计（`ToolInvoked`），输出照常作为外来文书包上边界、经锦衣卫检查。
use crate::BoxError;
use crate::agent_loop::{Tool, ToolSpec};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
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
pub enum ServerConfig {
    /// 本地子进程，经标准输入输出交谈。
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        /// 入号舍（名册写 `"haoshe": true` 或 `{ "network": false, "write": [...] }`）；`None` 为照常运行。
        haoshe: Option<Haoshe>,
    },
    /// 远端服务（Streamable HTTP）。
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

/// 本地服务的号舍：默认可联网（许多外藩本就是去取网上的东西），只可写一个临时目录
/// （设为 `TMPDIR`）与 `write` 所列目录（相对项目根，或以 `~/` 开头）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Haoshe {
    pub network: crate::haoshe::Network,
    pub write: Vec<String>,
}

/// https 一律可用；http 只许本机地址。
fn allowed_url(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host.split(':').next().unwrap_or_default(),
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
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
    let table = |v: &Value| -> BTreeMap<String, String> {
        v.as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
            .collect()
    };
    let mut servers = BTreeMap::new();
    for (name, server) in parsed["servers"].as_object().into_iter().flatten() {
        if !ziyuan_core::valid_mcp_grant(name) || name.contains('/') || name == "*" {
            return Err(format!("{CONFIG}：服务名不合法：{name}").into());
        }
        let text = |key: &str| server[key].as_str().filter(|c| !c.trim().is_empty());
        let config = match (text("command"), text("url")) {
            (Some(command), None) => ServerConfig::Stdio {
                command: command.to_owned(),
                args: strings(&server["args"]),
                env: table(&server["env"]),
                haoshe: match &server["haoshe"] {
                    Value::Null | Value::Bool(false) => None,
                    Value::Bool(true) => Some(Haoshe {
                        network: crate::haoshe::Network::Open,
                        write: Vec::new(),
                    }),
                    Value::Object(o) => Some(Haoshe {
                        network: match o.get("network") {
                            None | Some(Value::Bool(true)) => crate::haoshe::Network::Open,
                            Some(Value::Bool(false)) => crate::haoshe::Network::Off,
                            Some(Value::Array(_)) => crate::haoshe::Network::only(strings(
                                &o["network"],
                            ))
                            .map_err(|e| format!("{CONFIG}：服务 {name} 的 haoshe.network：{e}"))?,
                            Some(_) => {
                                return Err(format!(
                                    "{CONFIG}：服务 {name} 的 haoshe.network 须为 true、false 或域名列表"
                                )
                                .into());
                            }
                        },
                        write: strings(&server["haoshe"]["write"]),
                    }),
                    _ => {
                        return Err(format!(
                            "{CONFIG}：服务 {name} 的 haoshe 须为 true、false 或对象"
                        )
                        .into());
                    }
                },
            },
            (None, Some(url)) if allowed_url(url) => ServerConfig::Http {
                url: url.to_owned(),
                headers: table(&server["headers"]),
            },
            (None, Some(url)) => {
                return Err(format!(
                    "{CONFIG}：服务 {name} 的 url 须为 https（本机地址可用 http）：{url}"
                )
                .into());
            }
            (Some(_), Some(_)) => {
                return Err(format!("{CONFIG}：服务 {name} 的 command 与 url 只能择一").into());
            }
            (None, None) => {
                return Err(format!("{CONFIG}：服务 {name} 缺少 command 或 url").into());
            }
        };
        servers.insert(name.clone(), config);
    }
    Ok(servers)
}

/// 其中的 `${VAR}` 换成本进程环境中的值（未设为空），其余原样，如 `Bearer ${TOKEN}`。
fn expand(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        let Some(len) = rest[start + 2..].find('}') else {
            break;
        };
        out.push_str(&rest[..start]);
        out.push_str(&std::env::var(&rest[start + 2..start + 2 + len]).unwrap_or_default());
        rest = &rest[start + 3 + len..];
    }
    out.push_str(rest);
    out
}

/// 启动 stdio 子进程：工作目录为项目根，环境只给无害的几项与名册写明的变量。
/// 名册要求入号舍的，号舍不可用时拒绝启动（`ZIYUAN_SANDBOX=off` 时照常运行）。
fn spawn(
    root: &Path,
    name: &str,
    program: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    haoshe: Option<&Haoshe>,
) -> Result<Transport, BoxError> {
    let mut scratch = None;
    let mut door = None;
    let mut command = match haoshe {
        Some(cell) if crate::haoshe::Mode::from_env()? != crate::haoshe::Mode::Off => {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "ziyuan-mcp-{name}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir)?;
            let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
            let mut writable = vec![dir.clone()];
            writable.extend(
                cell.write
                    .iter()
                    .filter_map(|w| match w.strip_prefix("~/") {
                        Some(rest) => home.as_ref().map(|h| h.join(rest)),
                        None => Some(root.join(w)),
                    }),
            );
            let rules = crate::haoshe::Rules {
                readable: vec![root.to_path_buf()],
                writable,
                hidden: vec![root.join(".ziyuan")],
                network: cell.network.clone(),
            };
            let confined = crate::haoshe::confine(program, args, &rules).map_err(|reason| {
                let _ = std::fs::remove_dir_all(&dir);
                format!("外藩 {name} 须入号舍，但号舍不可用：{reason}")
            })?;
            scratch = Some(dir);
            door = confined.proxy;
            confined.command
        }
        _ => {
            let mut command = Command::new(program);
            command.args(args);
            command
        }
    };
    command
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
    for (key, value) in env {
        command.env(key, expand(value));
    }
    if let Some(dir) = &scratch {
        command.env("TMPDIR", dir);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("外藩 {name} 启动失败（{program}）：{e}"))?;
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
    Ok(Transport::Stdio {
        child,
        stdin,
        lines,
        scratch,
        _door: door,
    })
}

/// 外藩服务的一件工具。
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// 一个已连上的外藩服务；`Drop` 时结束子进程，或请远端结束会话。
pub struct Server {
    name: String,
    transport: Transport,
    next_id: u64,
}

enum Transport {
    Stdio {
        child: Child,
        stdin: ChildStdin,
        lines: Receiver<String>,
        /// 号舍里可写的临时目录，服务停后删去。
        scratch: Option<std::path::PathBuf>,
        /// 按域名放行时的门吏，随服务一同撤下。
        _door: Option<crate::haoshe::Proxy>,
    },
    Http {
        agent: ureq::Agent,
        url: String,
        headers: Vec<(String, String)>,
        /// 服务在握手时给的会话 ID，之后每个请求都带上。
        session: Option<String>,
        /// 握手议定的协议版本，之后每个请求都带上。
        protocol: Option<String>,
    },
}

/// 外藩发来的一条消息该怎么处置。
enum Incoming {
    /// 本请求的回复。
    Reply(Result<Value, String>),
    /// 外藩反向发来的请求（ID），一概回绝。
    Request(Value),
    /// 通知或别的回复，略过。
    Skip,
}

fn classify(message: &Value, id: u64) -> Incoming {
    if message.get("method").is_some() {
        return match message.get("id") {
            Some(their_id) => Incoming::Request(their_id.clone()),
            None => Incoming::Skip,
        };
    }
    if message["id"] != json!(id) {
        return Incoming::Skip;
    }
    match message.get("error") {
        Some(error) => Incoming::Reply(Err(error["message"]
            .as_str()
            .unwrap_or("未知错误")
            .to_owned())),
        None => Incoming::Reply(Ok(message["result"].clone())),
    }
}

fn refusal(their_id: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": their_id,
        "error": { "code": -32601, "message": "紫垣不受外藩反向请求" } })
}

impl Drop for Server {
    fn drop(&mut self) {
        match &mut self.transport {
            Transport::Stdio { child, scratch, .. } => {
                let _ = child.kill();
                let _ = child.wait();
                if let Some(dir) = scratch {
                    let _ = std::fs::remove_dir_all(dir);
                }
            }
            Transport::Http {
                agent,
                url,
                headers,
                session: Some(session),
                ..
            } => {
                let mut request = agent
                    .delete(url.as_str())
                    .header("Mcp-Session-Id", &*session);
                for (key, value) in headers.iter() {
                    request = request.header(key, value);
                }
                let _ = request.call();
            }
            Transport::Http { .. } => {}
        }
    }
}

impl Server {
    /// 连上服务（stdio 则启动子进程）并握手。
    pub fn connect(root: &Path, name: &str, config: &ServerConfig) -> Result<Self, BoxError> {
        let transport = match config {
            ServerConfig::Stdio {
                command,
                args,
                env,
                haoshe,
            } => spawn(root, name, command, args, env, haoshe.as_ref())?,
            ServerConfig::Http { url, headers } => Transport::Http {
                agent: ureq::Agent::config_builder()
                    .http_status_as_error(false)
                    .timeout_global(Some(TIMEOUT))
                    .build()
                    .into(),
                url: url.clone(),
                headers: headers
                    .iter()
                    .map(|(k, v)| (k.clone(), expand(v)))
                    .collect(),
                session: None,
                protocol: None,
            },
        };
        let mut server = Self {
            name: name.to_owned(),
            transport,
            next_id: 0,
        };
        let result = server.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": { "name": "ziyuan", "version": env!("CARGO_PKG_VERSION") },
            }),
        )?;
        if let Transport::Http { protocol, .. } = &mut server.transport {
            *protocol = Some(
                result["protocolVersion"]
                    .as_str()
                    .unwrap_or(PROTOCOL)
                    .to_owned(),
            );
        }
        server.notify(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
        Ok(server)
    }

    /// 发一条不需回复的消息（通知，或对反向请求的回绝）。
    fn notify(&mut self, message: &Value) -> Result<(), BoxError> {
        match &mut self.transport {
            Transport::Stdio { stdin, .. } => writeln!(stdin, "{message}")
                .and_then(|()| stdin.flush())
                .map_err(|e| format!("外藩 {} 已断开：{e}", self.name).into()),
            Transport::Http { .. } => {
                let response = self.post(message)?;
                let status = response.status().as_u16();
                if (200..300).contains(&status) {
                    Ok(())
                } else {
                    Err(format!("外藩 {} 回绝（HTTP {status}）", self.name).into())
                }
            }
        }
    }

    /// HTTP：POST 一条 JSON-RPC 消息，带上名册中的请求头、会话 ID 与协议版本。
    fn post(&self, message: &Value) -> Result<ureq::http::Response<ureq::Body>, BoxError> {
        let Transport::Http {
            agent,
            url,
            headers,
            session,
            protocol,
        } = &self.transport
        else {
            unreachable!("只在 HTTP 外藩上调用");
        };
        let mut request = agent
            .post(url.as_str())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        for (key, value) in headers {
            request = request.header(key, value);
        }
        if let Some(session) = session {
            request = request.header("Mcp-Session-Id", session);
        }
        if let Some(protocol) = protocol {
            request = request.header("MCP-Protocol-Version", protocol);
        }
        request
            .send(message.to_string())
            .map_err(|e| format!("外藩 {} 连不上：{e}", self.name).into())
    }

    /// 发一个 JSON-RPC 请求，等到同一 ID 的回复。服务发来的通知略过，它反向发来的请求一概回绝。
    fn request(&mut self, method: &str, params: Value) -> Result<Value, BoxError> {
        self.next_id += 1;
        let id = self.next_id;
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let reply = match self.transport {
            Transport::Stdio { .. } => self.request_stdio(&message, id, method)?,
            Transport::Http { .. } => self.request_http(&message, id, method)?,
        };
        reply.map_err(|text| format!("外藩 {} 回绝（{method}）：{text}", self.name).into())
    }

    fn request_stdio(
        &mut self,
        message: &Value,
        id: u64,
        method: &str,
    ) -> Result<Result<Value, String>, BoxError> {
        self.notify(message)?;
        loop {
            let Transport::Stdio { lines, .. } = &self.transport else {
                unreachable!("只在 stdio 外藩上调用");
            };
            let line = match lines.recv_timeout(TIMEOUT) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!("外藩 {} 逾时未回（{method}）", self.name).into());
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!("外藩 {} 已断开（{method}）", self.name).into());
                }
            };
            let Ok(incoming) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match classify(&incoming, id) {
                Incoming::Reply(reply) => return Ok(reply),
                Incoming::Request(their_id) => self.notify(&refusal(their_id))?,
                Incoming::Skip => {}
            }
        }
    }

    /// HTTP：回复或是一条 JSON，或是一段 SSE 流（逐条处理，直到本请求的回复）。
    fn request_http(
        &mut self,
        message: &Value,
        id: u64,
        method: &str,
    ) -> Result<Result<Value, String>, BoxError> {
        let response = self.post(message)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let mut body = String::new();
            let _ = response
                .into_body()
                .into_reader()
                .take(500)
                .read_to_string(&mut body);
            return Err(
                format!("外藩 {} 回绝（{method}，HTTP {status}）：{body}", self.name).into(),
            );
        }
        if let (Some(value), Transport::Http { session, .. }) = (
            response.headers().get("mcp-session-id"),
            &mut self.transport,
        ) {
            *session = value.to_str().ok().map(str::to_owned);
        }
        let event_stream = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let mut reader = BufReader::new(response.into_body().into_reader());
        if !event_stream {
            let mut text = String::new();
            reader.read_to_string(&mut text)?;
            let incoming: Value = serde_json::from_str(&text)
                .map_err(|e| format!("外藩 {} 的回复不是 JSON（{method}）：{e}", self.name))?;
            return match classify(&incoming, id) {
                Incoming::Reply(reply) => Ok(reply),
                _ => Err(format!("外藩 {} 答非所问（{method}）", self.name).into()),
            };
        }
        let mut data = String::new();
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                return Err(format!("外藩 {} 未回复就断开了（{method}）", self.name).into());
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if let Some(value) = line.strip_prefix("data:") {
                data.push_str(value.strip_prefix(' ').unwrap_or(value));
                data.push('\n');
                continue;
            }
            if !line.is_empty() || data.is_empty() {
                continue; // event:、id: 等字段与注释略过；空行为一条事件的结尾。
            }
            let event = std::mem::take(&mut data);
            let Ok(incoming) = serde_json::from_str::<Value>(&event) else {
                continue;
            };
            match classify(&incoming, id) {
                Incoming::Reply(reply) => return Ok(reply),
                Incoming::Request(their_id) => self.notify(&refusal(their_id))?,
                Incoming::Skip => {}
            }
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
sys.stdin.reconfigure(encoding="utf-8")  # MCP 规定 UTF-8；Windows 上管道默认按本地代码页解码
def send(m): print(json.dumps(m), flush=True)
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m or "method" not in m: continue  # 通知，或对反向请求的回复
    if m["method"] == "initialize":
        send({"jsonrpc": "2.0", "method": "notifications/message", "params": {}})
        send({"jsonrpc": "2.0", "id": "x", "method": "roots/list"})
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "fake"}}})
    elif m["method"] == "tools/list":
        tools = [{"name": n, "description": n + " 工具", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}} for n in ["echo", "env", "fail", "probe"]]
        send({"jsonrpc": "2.0", "id": m["id"], "result": {"tools": tools}})
    elif m["method"] == "tools/call":
        name, args = m["params"]["name"], m["params"]["arguments"]
        if name == "echo": r = {"content": [{"type": "text", "text": "回声：" + args.get("text", "")}, {"type": "image", "data": ""}]}
        elif name == "env": r = {"content": [{"type": "text", "text": os.environ.get("SECRET", "-") + "|" + os.environ.get("ZIYUAN_LLM_API_KEY", "-") + "|" + os.getcwd()}]}
        elif name == "probe":
            out = []
            for path in [args["outside"], os.path.join(os.environ.get("TMPDIR", "/tmp"), "ok")]:
                try: open(path, "w").write("x"); out.append("w")
                except OSError: out.append("-")
            r = {"content": [{"type": "text", "text": "".join(out)}]}
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
        // 整服务授权给全部四件；不授则一件也没有，也不启动服务。
        assert_eq!(sealed_tools(&root, &grants(&["fake"])).unwrap().len(), 4);
        assert_eq!(sealed_tools(&root, &grants(&["*"])).unwrap().len(), 8);
        assert!(sealed_tools(&root, &grants(&[])).unwrap().is_empty());
        // 名册里没有的服务、服务没有的工具：报错。
        let missing = sealed_tools(&root, &grants(&["nope"])).err().unwrap();
        assert!(missing.to_string().contains("没有登记"), "{missing}");
        let missing = sealed_tools(&root, &grants(&["fake/rm"])).err().unwrap();
        assert!(missing.to_string().contains("没有工具 rm"), "{missing}");
        drop(tools);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn servers_in_haoshe_write_only_their_scratch_dir() {
        let Some(root) = project("haoshe") else {
            return;
        };
        let config = json!({ "servers": {
            "cell": { "command": "python3", "args": ["-I", "fake.py"], "haoshe": { "network": false } },
            "free": { "command": "python3", "args": ["-I", "fake.py"] },
        }});
        std::fs::write(root.join(CONFIG), config.to_string()).unwrap();
        let outside = root.join("escaped");
        let args = json!({ "outside": outside }).to_string();
        let confined = crate::haoshe::confine("true", &[], &crate::haoshe::Rules::default());
        if confined.is_err() {
            // 本机没有号舍：名册要求入号舍的服务拒绝启动，而不是照常运行。
            let error = sealed_tools(&root, &grants(&["cell"])).err().unwrap();
            assert!(error.to_string().contains("号舍不可用"), "{error}");
        } else {
            let tools = sealed_tools(&root, &grants(&["cell/probe"])).unwrap();
            // 项目里写不进去，自己的临时目录写得了。
            assert_eq!(tools[0].execute(&args).unwrap(), "-w");
            assert!(!outside.exists());
            drop(tools);
        }
        // 对照：不入号舍的服务两处都写得了，可见探测本身有效。
        let tools = sealed_tools(&root, &grants(&["free/probe"])).unwrap();
        assert_eq!(tools[0].execute(&args).unwrap(), "ww");
        drop(tools);
        std::fs::write(
            root.join(CONFIG),
            r#"{"servers":{"x":{"command":"a","haoshe":1}}}"#,
        )
        .unwrap();
        assert!(
            load_config(&root)
                .unwrap_err()
                .to_string()
                .contains("haoshe")
        );
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
        // 先停服务：Windows 上删不掉仍是某进程工作目录的目录。
        drop(tools);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// 最小的 Streamable HTTP 服务：initialize 回 JSON 并发会话 ID；tools/list 回一段 SSE 流
    /// （先发通知与反向请求）；tools/call 的 echo 回显 text 与收到的 Authorization、会话 ID。
    /// 不带会话 ID 的后续请求回 400。启动后先在标准输出打印端口。
    const FAKE_HTTP: &str = r#"
import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
class H(BaseHTTPRequestHandler):
    # 真实服务都是 HTTP/1.1 长连接；HTTP/1.0 时 ureq 偶尔复用已被关闭的连接，测试会时好时坏。
    # 长连接要多线程服务，否则一条连接占住，后一个请求就等不到。
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def reply(self, status, body=b"", kind="application/json", headers=()):
        self.send_response(status)
        self.send_header("Content-Type", kind)
        for k, v in headers: self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers(); self.wfile.write(body)
    def do_DELETE(self): self.reply(200)
    def do_POST(self):
        m = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if "method" not in m or "id" not in m: return self.reply(202)
        if m["method"] == "initialize":
            r = {"jsonrpc": "2.0", "id": m["id"], "result": {"protocolVersion": "2025-06-18", "capabilities": {}}}
            return self.reply(200, json.dumps(r).encode(), headers=[("Mcp-Session-Id", "s-1")])
        if self.headers.get("Mcp-Session-Id") != "s-1": return self.reply(400, b"no session")
        if m["method"] == "tools/list":
            tools = [{"name": "echo", "description": "回显", "inputSchema": {"type": "object"}}]
            events = [{"jsonrpc": "2.0", "method": "notifications/progress", "params": {}},
                      {"jsonrpc": "2.0", "id": "r", "method": "sampling/createMessage"},
                      {"jsonrpc": "2.0", "id": m["id"], "result": {"tools": tools}}]
            body = "".join("event: message\ndata: " + json.dumps(e) + "\n\n" for e in events)
            return self.reply(200, body.encode(), "text/event-stream")
        a = m["params"]["arguments"]
        text = a.get("text", "") + "|" + self.headers.get("Authorization", "-") + "|" + self.headers.get("MCP-Protocol-Version", "-")
        r = {"jsonrpc": "2.0", "id": m["id"], "result": {"content": [{"type": "text", "text": text}]}}
        self.reply(200, json.dumps(r).encode())
s = ThreadingHTTPServer(("127.0.0.1", 0), H)
print(s.server_address[1], flush=True)
s.serve_forever()
"#;

    #[test]
    fn http_servers_speak_streamable_http() {
        let Some(root) = project("http") else { return };
        std::fs::write(root.join("fake_http.py"), FAKE_HTTP).unwrap();
        /// 测试失败 panic 时也结束服务，免得它占着输出管道让测试挂住。
        struct Kill(Child);
        impl Drop for Kill {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Kill(
            Command::new("python3")
                .args(["-I", "fake_http.py"])
                .current_dir(&root)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut port = String::new();
        BufReader::new(child.0.stdout.take().unwrap())
            .read_line(&mut port)
            .unwrap();
        // 请求头中的 `${VAR}` 取自环境：用 PATH 代替，避免改动测试进程的环境变量。
        let config = json!({ "servers": { "wiki": {
            "url": format!("http://127.0.0.1:{}/mcp", port.trim()),
            "headers": { "Authorization": "Bearer ${PATH}" },
        }}});
        std::fs::write(root.join(CONFIG), config.to_string()).unwrap();
        let tools = sealed_tools(&root, &grants(&["wiki"])).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].spec().name, "wiki__echo");
        let out = tools[0].execute(r#"{"text":"你好"}"#).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        assert_eq!(out, format!("你好|Bearer {path}|2025-06-18"));
        drop((tools, child));
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
        // HTTP 服务：明文 http 只许本机地址；command 与 url 只能择一。
        for (url, ok) in [
            ("https://mcp.example.com/mcp", true),
            ("http://127.0.0.1:9/mcp", true),
            ("http://localhost/mcp", true),
            ("http://[::1]:9/mcp", true),
            ("http://example.com/mcp", false),
            ("http://127.0.0.1.evil.com/mcp", false),
            ("ftp://x", false),
        ] {
            let config = json!({ "servers": { "w": { "url": url } } });
            std::fs::write(root.join(CONFIG), config.to_string()).unwrap();
            assert_eq!(load_config(&root).is_ok(), ok, "{url}");
        }
        let both = json!({ "servers": { "w": { "url": "https://x", "command": "y" } } });
        std::fs::write(root.join(CONFIG), both.to_string()).unwrap();
        assert!(load_config(&root).unwrap_err().to_string().contains("择一"));
        assert_eq!(expand("plain"), "plain");
        assert_eq!(expand("${ZIYUAN_TEST_UNSET_VAR}x"), "x");
        assert_eq!(tool_name("my.srv", "do it"), "my_srv__do_it");
        std::fs::remove_dir_all(root).unwrap();
    }
}
