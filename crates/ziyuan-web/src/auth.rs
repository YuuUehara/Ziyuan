//! 令牌与登录：生成、读取令牌，校验请求，换取 Cookie。

use axum::Json;
use axum::extract::{Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io;

use crate::INDEX_HTML;
use crate::state::{ApiError, AppState};

/// 生成 32 位十六进制随机令牌。
pub fn random_token() -> io::Result<String> {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| io::Error::other("系统随机数不可用"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// 读取 `<root>/.ziyuan/web-token` 中的令牌；没有、内容不合格或 `rotate` 时生成新令牌写入
/// （Unix 下仅本人可读写）。旧令牌随之失效。
pub fn load_or_create_token(root: &std::path::Path, rotate: bool) -> io::Result<String> {
    let path = root.join(".ziyuan").join("web-token");
    if !rotate {
        match std::fs::read_to_string(&path) {
            Ok(text) if is_token(text.trim()) => return Ok(text.trim().to_owned()),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    let token = random_token()?;
    std::fs::create_dir_all(path.parent().expect("令牌路径含目录"))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    io::Write::write_all(&mut options.open(&path)?, format!("{token}\n").as_bytes())?;
    Ok(token)
}

pub(crate) fn is_token(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) const COOKIE: &str = "ziyuan_token";

/// 请求所带的令牌是否正确：`Authorization: Bearer <令牌>`，或登录后的 Cookie。
pub(crate) fn authorized(app: &AppState, headers: &axum::http::HeaderMap) -> bool {
    let expected = app.0.config.token.as_bytes();
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let cookies = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().strip_prefix(COOKIE)?.strip_prefix('='));
    bearer
        .into_iter()
        .chain(cookies)
        .any(|t| constant_time_eq(t.as_bytes(), expected))
}

pub(crate) async fn require_token(
    State(app): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if authorized(&app, request.headers()) {
        next.run(request).await
    } else {
        ApiError(StatusCode::UNAUTHORIZED, "缺少或错误的访问令牌".into()).into_response()
    }
}

/// 登录 Cookie：只随同站请求发送，脚本读不到；浏览器关闭前有效。
pub(crate) fn login_cookie(token: &str) -> String {
    format!("{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/")
}

/// 首页。带正确的 `?token=` 时种下 Cookie 并重定向到不含令牌的地址，以免令牌留在地址栏与历史记录。
pub(crate) async fn index(
    State(app): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    match query.get("token") {
        Some(token) if constant_time_eq(token.as_bytes(), app.0.config.token.as_bytes()) => (
            StatusCode::SEE_OTHER,
            [
                (header::SET_COOKIE, login_cookie(token)),
                (header::LOCATION, "/".to_owned()),
            ],
        )
            .into_response(),
        _ => Html(INDEX_HTML).into_response(),
    }
}

/// 登录框：令牌正确则种下 Cookie。
pub(crate) async fn login(State(app): State<AppState>, Json(body): Json<Value>) -> Response {
    let token = body["token"].as_str().unwrap_or("").trim();
    if constant_time_eq(token.as_bytes(), app.0.config.token.as_bytes()) {
        let cookie = login_cookie(token);
        ([(header::SET_COOKIE, cookie)], Json(json!({}))).into_response()
    } else {
        ApiError(StatusCode::UNAUTHORIZED, "令牌不正确".into()).into_response()
    }
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
