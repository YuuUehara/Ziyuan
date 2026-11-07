//! Opaque workspace namespace for browser session isolation; never an authorization.
use ring::hmac;
use std::path::Path;

const DOMAIN: &[u8] = b"ziyuan-web/materials/workspace-id/v1";

/// Canonicalized once when the service starts; a failure only disables the material routes.
pub(crate) fn workspace_identity(root: &Path, token: &str) -> Result<String, String> {
    let canonical = std::fs::canonicalize(root).map_err(|_| {
        "无法确定本地资料的工作区身份：工作区根目录无法规范化；请确认目录存在后重启服务".to_owned()
    })?;
    Ok(derive(&canonical, token))
}

pub(super) fn derive(path: &Path, token: &str) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, token.as_bytes());
    let mut context = hmac::Context::with_key(&key);
    for part in [DOMAIN, &native_bytes(path)] {
        context.update(&(part.len() as u64).to_be_bytes());
        context.update(part);
    }
    context
        .sign()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(windows)]
fn native_bytes(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(unix)]
fn native_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}
