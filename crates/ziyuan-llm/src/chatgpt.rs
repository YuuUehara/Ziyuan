//! ChatGPT 订阅（Sign in with ChatGPT）：用户在浏览器中登录并授权紫垣使用其 ChatGPT 订阅，
//! 之后的模型调用经 Responses API 计入订阅额度，不需要 API Key。
//!
//! 流程见 <https://developers.openai.com/siwc/token-sharing-open-source>：OAuth 授权码 + PKCE，
//! 回调到本机 `127.0.0.1`；首次登录以 `dynamic_agent_client` 注册，此后沿用发下的 `client_id`。
//! 凭据存在用户配置目录（`ZIYUAN_CONFIG_DIR`，默认 `~/.config/ziyuan/chatgpt.json`，仅本人可读），
//! 不进项目、事件簿与任何模型提示。访问令牌一小时过期，过期前自动续期；续期以文件锁排队，
//! 多个进程不会争用轮换的续期令牌。
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use ziyuan_runtime::BoxError;

/// OpenAI 的授权服务；`ZIYUAN_CHATGPT_ISSUER` 可改（测试用）。
pub const ISSUER: &str = "https://auth.openai.com";
/// 订阅额度适用的 API；也是 OAuth 的 `resource`。
pub const API_BASE: &str = "https://api.openai.com/v1";
/// 用户查看与管理用量的页面。
pub const USAGE_URL: &str = "https://chatgpt.com/settings/usage";
const AGENT_NAME: &str = "ZiYuan";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
/// 有此授权才能把模型调用计入订阅额度。
const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
const CALLBACK_PATH: &str = "/auth/callback";
const LOGIN_PORT: u16 = 1455;
const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);
/// 访问令牌剩余不足此数时先续期。
const REFRESH_MARGIN: u64 = 300;

/// 保存的登录：注册（`client_id` 与身份）在退出登录后保留，下次登录沿用；令牌退出时清空。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Credentials {
    pub email: String,
    pub subject: String,
    pub client_id: String,
    pub id_token: String,
    pub access_token: String,
    pub refresh_token: String,
    /// 访问令牌过期时刻（Unix 秒）。
    pub expires_at: u64,
    pub scopes: Vec<String>,
}

impl Credentials {
    /// 已登录且授权使用订阅额度。
    pub fn plan_enabled(&self) -> bool {
        !self.refresh_token.is_empty() && self.scopes.iter().any(|s| s == PLAN_SCOPE)
    }

    fn to_json(&self) -> Value {
        json!({
            "email": self.email,
            "subject": self.subject,
            "client_id": self.client_id,
            "id_token": self.id_token,
            "access_token": self.access_token,
            "refresh_token": self.refresh_token,
            "expires_at": self.expires_at,
            "scopes": self.scopes,
        })
    }

    fn from_json(v: &Value) -> Self {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_owned();
        Self {
            email: s("email"),
            subject: s("subject"),
            client_id: s("client_id"),
            id_token: s("id_token"),
            access_token: s("access_token"),
            refresh_token: s("refresh_token"),
            expires_at: v["expires_at"].as_u64().unwrap_or(0),
            scopes: v["scopes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect(),
        }
    }
}

/// 授权服务的端点（取自 OpenID 发现文档）。
struct Endpoints {
    issuer: String,
    authorize: String,
    token: String,
    jwks: String,
    revocation: Option<String>,
}

/// 本机的 ChatGPT 账号：凭据文件所在目录与授权服务。
pub struct Account {
    dir: PathBuf,
    issuer: String,
    agent: ureq::Agent,
}

impl Account {
    pub fn new(dir: PathBuf, issuer: impl Into<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            dir,
            issuer: issuer.into().trim_end_matches('/').to_owned(),
            agent,
        }
    }

    /// 配置目录取 `ZIYUAN_CONFIG_DIR`，否则 `$XDG_CONFIG_HOME/ziyuan`、`~/.config/ziyuan`
    /// （Windows 为 `%APPDATA%\ziyuan`）。
    pub fn from_env() -> Result<Self, BoxError> {
        let var = |k| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let dir = var("ZIYUAN_CONFIG_DIR")
            .or_else(|| var("XDG_CONFIG_HOME").map(|d| d.join("ziyuan")))
            .or_else(|| var("HOME").map(|d| d.join(".config/ziyuan")))
            .or_else(|| var("APPDATA").map(|d| d.join("ziyuan")))
            .ok_or("找不到配置目录：请设 ZIYUAN_CONFIG_DIR")?;
        let issuer = std::env::var("ZIYUAN_CHATGPT_ISSUER").unwrap_or_else(|_| ISSUER.into());
        Ok(Self::new(dir, issuer))
    }

    fn path(&self) -> PathBuf {
        self.dir.join("chatgpt.json")
    }

    /// 已保存的登录（含已退出、只剩注册的）。
    pub fn load(&self) -> Result<Option<Credentials>, BoxError> {
        match fs::read_to_string(self.path()) {
            Ok(text) => Ok(Some(Credentials::from_json(&serde_json::from_str(&text)?))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, creds: &Credentials) -> Result<(), BoxError> {
        write_private(&self.path(), &creds.to_json().to_string())
    }

    /// 本机的 `ext_agent_host_id`：首次登录前生成，此后不变（`urn:uuid:` + UUIDv4）。
    pub fn host_id(&self) -> Result<String, BoxError> {
        let path = self.dir.join("host-id");
        if let Ok(id) = fs::read_to_string(&path)
            && id.trim().starts_with("urn:uuid:")
        {
            return Ok(id.trim().to_owned());
        }
        let mut b = random::<16>()?;
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
        let id = format!(
            "urn:uuid:{}-{}-{}-{}-{}",
            &h[..8],
            &h[8..12],
            &h[12..16],
            &h[16..20],
            &h[20..]
        );
        write_private(&path, &id)?;
        Ok(id)
    }

    fn endpoints(&self) -> Result<Endpoints, BoxError> {
        let doc = self.get_json(&format!("{}/.well-known/openid-configuration", self.issuer))?;
        let s = |k: &str| {
            doc[k]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| BoxError::from(format!("授权服务的发现文档缺少 {k}")))
        };
        Ok(Endpoints {
            issuer: s("issuer")?,
            authorize: s("authorization_endpoint")?,
            token: s("token_endpoint")?,
            jwks: s("jwks_uri")?,
            revocation: s("revocation_endpoint").ok(),
        })
    }

    fn get_json(&self, url: &str) -> Result<Value, BoxError> {
        let mut response = self.agent.get(url).call()?;
        let status = response.status();
        let text = response.body_mut().read_to_string()?;
        if !status.is_success() {
            return Err(format!("{url} 返回 {status}").into());
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// 向令牌端点提交表单，返回 (状态码, JSON)。
    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<(u16, Value), BoxError> {
        let mut response = self.agent.post(url).send_form(form.iter().copied())?;
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string()?;
        Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
    }

    /// 在浏览器中登录并授权：`open` 收到授权地址（不含任何令牌），应打开浏览器或提示用户打开。
    /// 返回保存好的凭据与是否为首次注册。未授权使用订阅额度时登录仍保存，但返回错误。
    pub fn login(&self, open: impl FnOnce(&str)) -> Result<(Credentials, bool), BoxError> {
        let ep = self.endpoints()?;
        let host = self.host_id()?;
        let saved = self.load()?.filter(|c| !c.client_id.is_empty());
        let listener = TcpListener::bind(("127.0.0.1", LOGIN_PORT))
            .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))?;
        let redirect = format!(
            "http://127.0.0.1:{}{CALLBACK_PATH}",
            listener.local_addr()?.port()
        );
        let state = b64url(&random::<32>()?);
        let nonce = b64url(&random::<32>()?);
        let verifier = b64url(&random::<32>()?);
        let challenge =
            b64url(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref());
        let mut params = vec![
            ("response_type", "code"),
            ("redirect_uri", redirect.as_str()),
            ("scope", SCOPES),
            ("resource", API_BASE),
            ("state", state.as_str()),
            ("nonce", nonce.as_str()),
            ("code_challenge_method", "S256"),
            ("code_challenge", challenge.as_str()),
            ("ext_agent_host_id", host.as_str()),
        ];
        // 不带 id_token_hint：授权地址会显示在终端里，不让它带上任何令牌。
        match &saved {
            Some(c) => {
                params.push(("client_id", c.client_id.as_str()));
                if !c.email.is_empty() {
                    params.push(("login_hint", c.email.as_str()));
                }
            }
            None => {
                params.push(("client_id", "dynamic_agent_client"));
                params.push(("agent_name_hint", AGENT_NAME));
            }
        }
        let query: Vec<String> = params
            .iter()
            .map(|(k, v)| format!("{k}={}", percent(v)))
            .collect();
        open(&format!("{}?{}", ep.authorize, query.join("&")));

        let callback = wait_for_callback(&listener)?;
        if callback.get("state") != Some(&state) {
            return Err("回调的 state 不符，已放弃这次登录".into());
        }
        if let Some(error) = callback.get("error") {
            return Err(match error.as_str() {
                "access_denied" => "已拒绝授权；未登录".to_owned(),
                other => format!("授权失败：{other}"),
            }
            .into());
        }
        let code = callback.get("code").ok_or("回调缺少授权码")?;
        let client_id = match (&saved, callback.get("client_id")) {
            (None, Some(id)) => id.clone(),
            (None, None) => return Err("首次注册的回调没有发下 client_id，注册未完成".into()),
            (Some(c), Some(id)) if *id != c.client_id => {
                return Err("回调的 client_id 与已注册的不符，已放弃".into());
            }
            (Some(c), _) => c.client_id.clone(),
        };
        let (status, tokens) = self.post_form(
            &ep.token,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", &client_id),
                ("code", code),
                ("code_verifier", &verifier),
                ("redirect_uri", &redirect),
                ("resource", API_BASE),
            ],
        )?;
        if status != 200 {
            return Err(format!("换取令牌失败（{status}）：{}", oauth_error(&tokens)).into());
        }
        let id_token = tokens["id_token"].as_str().ok_or("令牌响应缺少 id_token")?;
        let jwks = self.get_json(&ep.jwks)?;
        let claims = verify_id_token(id_token, &jwks, &ep.issuer, &client_id, &nonce, now())?;
        let subject = claims["sub"].as_str().unwrap_or_default().to_owned();
        if let Some(c) = &saved
            && !c.subject.is_empty()
            && c.subject != subject
        {
            return Err(
                "登录的不是先前注册的那个账号；如要换账号，先删去配置目录中的 chatgpt.json".into(),
            );
        }
        let creds = Credentials {
            email: claims["email"].as_str().unwrap_or_default().to_owned(),
            subject,
            client_id,
            id_token: id_token.to_owned(),
            ..token_fields(&tokens, Credentials::default())
        };
        self.save(&creds)?;
        if !creds.plan_enabled() {
            return Err(
                "已登录，但未授权紫垣使用 ChatGPT 订阅额度；重新 `ziyuan login` 时请勾选".into(),
            );
        }
        Ok((creds, saved.is_none()))
    }

    /// 可用的访问令牌：快过期则先续期。
    pub fn access_token(&self) -> Result<String, BoxError> {
        let creds = self
            .load()?
            .filter(Credentials::plan_enabled)
            .ok_or("未登录 ChatGPT 订阅，或未授权使用订阅额度：请先执行 `ziyuan login`")?;
        if creds.expires_at > now() + REFRESH_MARGIN {
            return Ok(creds.access_token);
        }
        self.refresh(Some(&creds.access_token))
    }

    /// 续期：持文件锁，锁内重读凭据；若访问令牌已不是 `stale`（别的进程刚续过）就直接用新的。
    /// `stale` 为 `None` 时无条件续期。续期令牌失效时清空令牌，须重新登录。
    pub fn refresh(&self, stale: Option<&str>) -> Result<String, BoxError> {
        fs::create_dir_all(&self.dir)?;
        let lock = File::create(self.dir.join("chatgpt.lock"))?;
        lock.lock()?;
        let mut creds = self
            .load()?
            .filter(Credentials::plan_enabled)
            .ok_or("未登录 ChatGPT 订阅：请先执行 `ziyuan login`")?;
        if stale.is_some_and(|s| s != creds.access_token)
            && creds.expires_at > now() + REFRESH_MARGIN
        {
            return Ok(creds.access_token);
        }
        let ep = self.endpoints()?;
        let (status, tokens) = self.post_form(
            &ep.token,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", &creds.client_id),
                ("refresh_token", &creds.refresh_token),
                ("resource", API_BASE),
            ],
        )?;
        if status != 200 {
            let error = oauth_error(&tokens);
            let code = tokens["error"].as_str().unwrap_or_default();
            if matches!(
                code,
                "invalid_grant"
                    | "invalid_refresh_token"
                    | "token_expired"
                    | "refresh_token_expired"
                    | "refresh_token_invalidated"
                    | "refresh_token_reused"
            ) {
                self.clear_tokens(creds)?;
                return Err(
                    format!("ChatGPT 登录已失效（{error}）：请重新执行 `ziyuan login`").into(),
                );
            }
            return Err(format!("ChatGPT 令牌续期失败（{status}）：{error}").into());
        }
        creds = token_fields(&tokens, creds);
        self.save(&creds)?;
        Ok(creds.access_token)
    }

    fn clear_tokens(&self, creds: Credentials) -> Result<(), BoxError> {
        self.save(&Credentials {
            email: creds.email,
            subject: creds.subject,
            client_id: creds.client_id,
            ..Credentials::default()
        })
    }

    /// 退出登录：先在授权服务撤销续期令牌，再清空本地令牌（保留注册，下次登录沿用）。
    /// 返回远端是否确认撤销。
    pub fn logout(&self) -> Result<bool, BoxError> {
        let Some(creds) = self.load()? else {
            return Ok(true);
        };
        let mut revoked = creds.refresh_token.is_empty();
        if !revoked
            && let Ok(ep) = self.endpoints()
            && let Some(url) = ep.revocation
        {
            revoked = self
                .post_form(
                    &url,
                    &[
                        ("token", &creds.refresh_token),
                        ("token_type_hint", "refresh_token"),
                        ("client_id", &creds.client_id),
                    ],
                )
                .is_ok_and(|(status, _)| status == 200);
        }
        self.clear_tokens(creds)?;
        Ok(revoked)
    }

    /// 本账号可选的模型：(模型名, 显示名)，按服务端顺序。
    pub fn models(&self, api_base: &str) -> Result<Vec<(String, String)>, BoxError> {
        let token = self.access_token()?;
        let mut response = self
            .agent
            .get(format!("{}/models", api_base.trim_end_matches('/')))
            .header("Authorization", format!("Bearer {token}"))
            .call()?;
        let status = response.status();
        let text = response.body_mut().read_to_string()?;
        if !status.is_success() {
            let snippet: String = text.chars().take(300).collect();
            return Err(format!("列出模型失败（{status}）：{snippet}").into());
        }
        let v: Value = serde_json::from_str(&text)?;
        Ok(v["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| m["visibility"].as_str().is_none_or(|v| v == "list"))
            .filter_map(|m| {
                let slug = m["slug"].as_str()?.to_owned();
                let name = m["display_name"].as_str().unwrap_or(&slug).to_owned();
                Some((slug, name))
            })
            .collect())
    }
}

/// 令牌响应中的访问令牌、续期令牌（轮换时换新）、过期时刻与授权范围。
fn token_fields(tokens: &Value, mut creds: Credentials) -> Credentials {
    if let Some(t) = tokens["access_token"].as_str() {
        creds.access_token = t.to_owned();
    }
    if let Some(t) = tokens["refresh_token"].as_str() {
        creds.refresh_token = t.to_owned();
    }
    creds.expires_at = now() + tokens["expires_in"].as_u64().unwrap_or(3600);
    if let Some(scope) = tokens["scope"].as_str() {
        creds.scopes = scope.split_whitespace().map(str::to_owned).collect();
    }
    creds
}

fn oauth_error(v: &Value) -> String {
    match (v["error"].as_str(), v["error_description"].as_str()) {
        (Some(e), Some(d)) => format!("{e}: {d}"),
        (Some(e), None) => e.to_owned(),
        _ => v.to_string().chars().take(300).collect(),
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn random<const N: usize>() -> Result<[u8; N], BoxError> {
    let mut b = [0u8; N];
    SystemRandom::new()
        .fill(&mut b)
        .map_err(|_| "取随机数失败")?;
    Ok(b)
}

/// 原子写入、仅本人可读（Unix 0600）。
fn write_private(path: &Path, text: &str) -> Result<(), BoxError> {
    let dir = path.parent().ok_or("路径无目录")?;
    fs::create_dir_all(dir)?;
    let tmp = path.with_extension("tmp");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// 等浏览器回到 `CALLBACK_PATH`，返回查询参数；其他路径（如 favicon）答 404 后继续等。
fn wait_for_callback(
    listener: &TcpListener,
) -> Result<std::collections::HashMap<String, String>, BoxError> {
    listener.set_nonblocking(true)?;
    let deadline = std::time::Instant::now() + LOGIN_TIMEOUT;
    loop {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() > deadline {
                    return Err("等待浏览器登录超时（10 分钟）".into());
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        // 读完请求头再答复。
        let mut header = String::new();
        while reader.read_line(&mut header)? > 2 {
            header.clear();
        }
        let target = line.split_whitespace().nth(1).unwrap_or_default();
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let mut stream = reader.into_inner();
        if path != CALLBACK_PATH {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            continue;
        }
        let params: std::collections::HashMap<String, String> = query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (unpercent(k), unpercent(v)))
            .collect();
        let body = if params.contains_key("code") {
            "紫垣已收到授权，可以关闭此页，回到终端。"
        } else {
            "紫垣未获授权，可以关闭此页，回到终端查看原因。"
        };
        let page = format!("<!doctype html><meta charset=utf-8><title>紫垣</title><p>{body}</p>");
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        );
        return Ok(params);
    }
}

/// 校验 ID token：RS256 签名对得上授权服务公布的公钥，签发者、受众、过期时间与 nonce 都相符。
/// 返回其声明。
pub(crate) fn verify_id_token(
    token: &str,
    jwks: &Value,
    issuer: &str,
    client_id: &str,
    nonce: &str,
    now: u64,
) -> Result<Value, BoxError> {
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("ID token 格式不对".into());
    };
    let header: Value = serde_json::from_slice(&unb64url(h)?)?;
    if header["alg"] != "RS256" {
        return Err(format!("ID token 的签名算法 {} 不受支持", header["alg"]).into());
    }
    let key = jwks["keys"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|k| k["kid"] == header["kid"] && k["kty"] == "RSA")
        .ok_or("授权服务的公钥中找不到 ID token 所用的那一把")?;
    let field = |k: &str| unb64url(key[k].as_str().unwrap_or_default());
    ring::signature::RsaPublicKeyComponents {
        n: field("n")?,
        e: field("e")?,
    }
    .verify(
        &ring::signature::RSA_PKCS1_2048_8192_SHA256,
        format!("{h}.{p}").as_bytes(),
        &unb64url(s)?,
    )
    .map_err(|_| "ID token 签名校验失败")?;
    let claims: Value = serde_json::from_slice(&unb64url(p)?)?;
    let audience_ok = match &claims["aud"] {
        Value::String(a) => a == client_id,
        Value::Array(a) => a.iter().any(|a| a == client_id),
        _ => false,
    };
    if claims["iss"] != issuer {
        return Err("ID token 的签发者不符".into());
    }
    if !audience_ok {
        return Err("ID token 的受众不是本次注册的 client_id".into());
    }
    if claims["exp"].as_u64().is_none_or(|exp| exp <= now) {
        return Err("ID token 已过期".into());
    }
    if claims["nonce"] != nonce {
        return Err("ID token 的 nonce 不符".into());
    }
    if claims["sub"].as_str().is_none_or(str::is_empty) {
        return Err("ID token 缺少 sub".into());
    }
    Ok(claims)
}

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// base64url，不补 `=`。
pub(crate) fn b64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |n, &b| n << 8 | b as u32) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// 解 base64url（容许 `=` 补位与标准 base64 的 `+/`）。
pub(crate) fn unb64url(text: &str) -> Result<Vec<u8>, BoxError> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.bytes().filter(|&c| c != b'=') {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return Err("base64 含非法字符".into()),
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn unpercent(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = |c: u8| (c as char).to_digit(16);
                match (hex(b[i + 1]), hex(b[i + 2])) {
                    (Some(hi), Some(lo)) => {
                        out.push((hi * 16 + lo) as u8);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OpenAiCompatible;
    use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
    use std::collections::HashMap;
    use std::io::Read;
    use std::sync::{Arc, Mutex};
    use ziyuan_core::TokenUsage;
    use ziyuan_runtime::ExecutionHooks;
    use ziyuan_runtime::agent_loop::{Message, ModelProvider, ModelTurn, ToolSpec};

    /// 仅供测试的 RSA 密钥（PKCS#8 DER），模拟授权服务给 ID token 签名。
    const TEST_KEY: &[u8] = include_bytes!("../tests-data/test-only-rsa.pk8");

    /// 收到的请求：(方法, 路径, 正文, Authorization)。
    type Seen = Arc<Mutex<Vec<(String, String, String, String)>>>;

    /// 按路径答复的模拟服务；`reply` 返回 (状态, 正文)，正文以 `data:` 开头时按 SSE 发送。
    fn router(
        reply: impl Fn(&str, &str, &str) -> (u16, String) + Send + 'static,
    ) -> (String, Seen) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen: Seen = Arc::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut reader = BufReader::new(stream.unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let (mut length, mut auth) = (0, String::new());
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    let lower = header.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    if lower.starts_with("authorization:") {
                        auth = header["authorization:".len()..].trim().to_owned();
                    }
                    if header == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let body = String::from_utf8(body).unwrap();
                let mut words = line.split_whitespace();
                let method = words.next().unwrap_or_default().to_owned();
                let path = words.next().unwrap_or_default().to_owned();
                let (status, text) = reply(&method, &path, &body);
                log.lock().unwrap().push((method, path, body, auth));
                let kind = if text.starts_with("data:") {
                    "text/event-stream"
                } else {
                    "application/json"
                };
                let _ = write!(
                    reader.into_inner(),
                    "HTTP/1.1 {status} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
            }
        });
        (url, seen)
    }

    fn form(body: &str) -> HashMap<String, String> {
        body.split('&')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (unpercent(k), unpercent(v)))
            .collect()
    }

    fn sign(claims: &Value) -> String {
        let key = RsaKeyPair::from_pkcs8(TEST_KEY).unwrap();
        let head = b64url(json!({"alg": "RS256", "kid": "k1"}).to_string().as_bytes());
        let body = b64url(claims.to_string().as_bytes());
        let mut sig = vec![0; key.public().modulus_len()];
        key.sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            format!("{head}.{body}").as_bytes(),
            &mut sig,
        )
        .unwrap();
        format!("{head}.{body}.{}", b64url(&sig))
    }

    fn jwks() -> Value {
        let key = RsaKeyPair::from_pkcs8(TEST_KEY).unwrap();
        let public = ring::rsa::PublicKeyComponents::<Vec<u8>>::from(key.public());
        json!({"keys": [{"kty": "RSA", "kid": "k1", "n": b64url(&public.n), "e": b64url(&public.e)}]})
    }

    #[derive(Default)]
    struct Hooks {
        text: Mutex<String>,
        usage: Mutex<Vec<TokenUsage>>,
    }
    impl ExecutionHooks for Hooks {
        fn text_delta(&self, text: &str) {
            self.text.lock().unwrap().push_str(text);
        }
        fn model_usage(&self, usage: TokenUsage) {
            self.usage.lock().unwrap().push(usage);
        }
    }

    fn sse(events: &[Value]) -> String {
        events.iter().map(|e| format!("data: {e}\n\n")).collect()
    }

    /// 模拟授权服务与 API 走一遍：首次注册登录（PKCE、ID token 校验）、访问令牌被拒后续期重试、
    /// Responses 流里的文字与 namespace 工具调用、订阅额度用尽、退出时撤销续期令牌。
    /// 未经真实 ChatGPT 账号实测。
    #[test]
    fn plan_login_inference_refresh_and_logout() {
        // 授权页地址里的 PKCE challenge 与 nonce，供令牌端点核对与签发。
        let authorize: Arc<Mutex<HashMap<String, String>>> = Arc::default();
        let issuer = Arc::new(Mutex::new(String::new()));
        let (auth_ref, issuer_ref) = (authorize.clone(), issuer.clone());
        let responses = Arc::new(Mutex::new(0));
        let (url, seen) = router(move |method, path, body| {
            let issuer = issuer_ref.lock().unwrap().clone();
            match (method, path) {
                ("GET", "/.well-known/openid-configuration") => (
                    200,
                    json!({
                        "issuer": issuer,
                        "authorization_endpoint": format!("{issuer}/authorize"),
                        "token_endpoint": format!("{issuer}/token"),
                        "jwks_uri": format!("{issuer}/jwks"),
                        "revocation_endpoint": format!("{issuer}/revoke"),
                    })
                    .to_string(),
                ),
                ("GET", "/jwks") => (200, jwks().to_string()),
                ("POST", "/token") => {
                    let f = form(body);
                    if f["grant_type"] == "authorization_code" {
                        let a = auth_ref.lock().unwrap();
                        let challenge = b64url(
                            ring::digest::digest(
                                &ring::digest::SHA256,
                                f["code_verifier"].as_bytes(),
                            )
                            .as_ref(),
                        );
                        if f["code"] != "code-1" || challenge != a["code_challenge"] {
                            return (400, json!({"error": "invalid_grant"}).to_string());
                        }
                        let id_token = sign(&json!({
                            "iss": issuer, "aud": f["client_id"], "sub": "user-1",
                            "email": "emperor@example.com", "nonce": a["nonce"], "exp": now() + 600,
                        }));
                        return (200, json!({
                            "id_token": id_token, "access_token": "at-1", "refresh_token": "rt-1",
                            "expires_in": 3600, "scope": SCOPES,
                        }).to_string());
                    }
                    // 续期：轮换续期令牌。
                    assert_eq!(f["refresh_token"], "rt-1");
                    (200, json!({"access_token": "at-2", "refresh_token": "rt-2", "expires_in": 3600}).to_string())
                }
                ("POST", "/revoke") => (200, "{}".into()),
                ("POST", "/v1/responses") => {
                    let mut n = responses.lock().unwrap();
                    *n += 1;
                    match *n {
                        1 => (401, json!({"detail": "token expired"}).to_string()),
                        2 => (
                            200,
                            sse(&[
                                json!({"type": "response.output_text.delta", "delta": "查一下"}),
                                json!({"type": "response.output_item.done", "item": {
                                "type": "function_call", "call_id": "c1", "name": "read_file",
                                "namespace": "ziyuan", "arguments": "{\"path\":\"a\"}"}}),
                                json!({"type": "response.completed", "response": {
                                "usage": {"input_tokens": 12, "output_tokens": 5}}}),
                            ]),
                        ),
                        _ => (
                            200,
                            sse(&[
                                json!({"type": "response.output_text.delta", "delta": "半句"}),
                                json!({"type": "response.failed", "response": {"error": {
                                "code": "subscription_sharing_usage_limit_exceeded", "message": "limit"}}}),
                            ]),
                        ),
                    }
                }
                _ => (404, "{}".into()),
            }
        });
        *issuer.lock().unwrap() = url.clone();
        let dir = std::env::temp_dir().join(format!(
            "ziyuan-chatgpt-{}",
            b64url(&random::<6>().unwrap())
        ));
        let account = Arc::new(Account::new(dir.clone(), &url));

        // 浏览器：从授权地址取参数，带授权码回到回调地址。
        let (creds, first) = account
            .login(|link| {
                let params = form(link.split_once('?').unwrap().1);
                assert_eq!(params["client_id"], "dynamic_agent_client");
                assert_eq!(params["code_challenge_method"], "S256");
                assert!(params["ext_agent_host_id"].starts_with("urn:uuid:"));
                assert!(params["scope"].contains("chatgpt.tokens.use.direct"));
                let callback = format!(
                    "{}?code=code-1&state={}&client_id=oaiapp_1",
                    params["redirect_uri"],
                    percent(&params["state"])
                );
                *authorize.lock().unwrap() = params;
                std::thread::spawn(move || ureq::get(&callback).call().unwrap());
            })
            .unwrap();
        assert!(first);
        assert_eq!(
            (creds.email.as_str(), creds.client_id.as_str()),
            ("emperor@example.com", "oaiapp_1")
        );
        assert!(creds.plan_enabled());
        // 换了 nonce 的 ID token 不认。
        assert!(
            verify_id_token(&creds.id_token, &jwks(), &url, "oaiapp_1", "other", now()).is_err()
        );

        let model =
            OpenAiCompatible::chatgpt_plan(account.clone(), format!("{url}/v1"), "gpt-test");
        let hooks = Hooks::default();
        let tools = [ToolSpec {
            name: "read_file".into(),
            description: "读文件".into(),
            input_schema_json: r#"{"type":"object"}"#.into(),
        }];
        let history = [
            Message::System("你是工部尚书".into()),
            Message::User("看看 a".into()),
        ];
        let turn = model.next_turn(&history, &tools, &hooks).unwrap();
        let ModelTurn::Calls { calls, text, .. } = turn else {
            panic!("{turn:?}")
        };
        assert_eq!(
            (calls[0].id.as_str(), calls[0].name.as_str(), text.as_str()),
            ("c1", "read_file", "查一下")
        );
        assert_eq!(
            hooks.usage.lock().unwrap()[0],
            TokenUsage {
                input: 12,
                output: 5
            }
        );
        {
            let seen = seen.lock().unwrap();
            let calls: Vec<_> = seen.iter().filter(|r| r.1 == "/v1/responses").collect();
            // 被拒的是旧令牌，续期后用新令牌重发同一请求。
            assert_eq!(
                (calls[0].3.as_str(), calls[1].3.as_str()),
                ("Bearer at-1", "Bearer at-2")
            );
            let body: Value = serde_json::from_str(&calls[1].2).unwrap();
            assert_eq!(
                (body["store"].clone(), body["stream"].clone()),
                (json!(false), json!(true))
            );
            assert_eq!(body["instructions"], "你是工部尚书");
            assert_eq!(
                body["input"],
                json!([{"role": "user", "content": "看看 a"}])
            );
            assert_eq!(body["tools"][0]["type"], "namespace");
            assert_eq!(body["tools"][0]["tools"][0]["name"], "read_file");
            assert!(body.get("max_output_tokens").is_none() && body.get("temperature").is_none());
        }
        assert_eq!(account.load().unwrap().unwrap().refresh_token, "rt-2");

        let error = model
            .next_turn(&history, &tools, &hooks)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("额度已用尽") && error.contains(USAGE_URL),
            "{error}"
        );

        assert!(account.logout().unwrap());
        let revoked = seen
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.1 == "/revoke")
            .cloned()
            .unwrap();
        assert_eq!(form(&revoked.2)["token"], "rt-2");
        let after = account.load().unwrap().unwrap();
        assert!(!after.plan_enabled() && after.access_token.is_empty());
        assert_eq!(after.client_id, "oaiapp_1", "注册保留，下次登录沿用");
        fs::remove_dir_all(dir).unwrap();
    }
}
