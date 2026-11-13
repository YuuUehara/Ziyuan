//! 选择嵌入提供方。远程提供方在核对项目授权之前不会创建传输。
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ziyuan_memory::{
    DeterministicProvider, EmbedTransport, EmbeddingProvider, LocalProvider, MemoryError,
    RemoteProvider, sha256_hex,
};

use crate::AppError;
use crate::translation::Translations;

#[derive(Clone, Debug)]
pub struct RemoteEndpoint {
    pub endpoint: String,
    pub model: String,
    pub revision: String,
    pub dim: usize,
    pub api_key: String,
}

#[derive(Clone, Debug)]
pub enum MemorySelection {
    Local,
    Deterministic,
    Remote(RemoteEndpoint),
}

#[derive(Clone, Debug, Serialize)]
pub struct RemoteAuthPreview {
    pub fingerprint: String,
    pub endpoint: String,
    pub model: String,
    pub revision: String,
    pub dim: usize,
    pub fields: Vec<String>,
    pub projects: BTreeSet<String>,
    pub note: String,
}

#[derive(Serialize)]
struct CanonicalAuth<'a> {
    version: u32,
    provider: &'a str,
    endpoint: &'a str,
    model: &'a str,
    revision: &'a str,
    dim: usize,
    fields: &'a [&'a str],
    projects: &'a BTreeSet<String>,
}

#[derive(Serialize, Deserialize)]
struct StoredAuth {
    version: u32,
    provider: String,
    endpoint: String,
    model: String,
    revision: String,
    dim: usize,
    fields: Vec<String>,
    projects: BTreeSet<String>,
    fingerprint: String,
}

const FIELDS: [&str; 1] = ["source"];

pub fn selection_from_env(explicit: Option<&str>) -> Result<MemorySelection, AppError> {
    let configured = explicit
        .map(str::to_owned)
        .unwrap_or_else(|| std::env::var(ziyuan_memory::embed::PROVIDER_ENV).unwrap_or_default());
    match configured.trim() {
        "" | "none" => Err(AppError::External(
            "记忆库不可用: 未配置嵌入提供方；默认不外发，须显式设置 ZIYUAN_MEMORY_PROVIDER".into(),
        )),
        "local" => Ok(MemorySelection::Local),
        "deterministic" => Ok(MemorySelection::Deterministic),
        "remote" => Ok(MemorySelection::Remote(remote_from_env()?)),
        other => Err(AppError::External(format!(
            "ZIYUAN_MEMORY_PROVIDER 为 {other:?}，无法识别；可用值：none、local、deterministic、remote"
        ))),
    }
}

pub fn remote_from_env() -> Result<RemoteEndpoint, AppError> {
    let endpoint = require_embed("ZIYUAN_MEMORY_EMBED_URL")?;
    let model = require_embed("ZIYUAN_MEMORY_EMBED_MODEL")?;
    let revision = require_embed("ZIYUAN_MEMORY_EMBED_REVISION")?;
    let dim = require_embed("ZIYUAN_MEMORY_EMBED_DIM")?;
    let dim = dim.parse::<usize>().map_err(|_| {
        AppError::External("ZIYUAN_MEMORY_EMBED_DIM 须为正整数；不会因此联网".into())
    })?;
    if dim == 0 {
        return Err(AppError::External(
            "ZIYUAN_MEMORY_EMBED_DIM 不得为 0；不会因此联网".into(),
        ));
    }
    if !(endpoint.starts_with("https://") || endpoint.starts_with("http://")) {
        return Err(AppError::External(
            "远程嵌入端点须以 http:// 或 https:// 开头；不会因此联网".into(),
        ));
    }
    Ok(RemoteEndpoint {
        endpoint,
        model,
        revision,
        dim,
        api_key: std::env::var("ZIYUAN_MEMORY_EMBED_KEY").unwrap_or_default(),
    })
}

fn require_embed(name: &str) -> Result<String, AppError> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(AppError::External(format!(
            "远程嵌入缺少 {name}；不会读取 ZIYUAN_LLM_*，也不会联网"
        ))),
    }
}

pub fn preview_remote_auth(
    endpoint: &RemoteEndpoint,
    projects: &BTreeSet<String>,
) -> Result<RemoteAuthPreview, AppError> {
    if projects.is_empty() {
        return Err(AppError::External("至少授权一个项目".into()));
    }
    Ok(RemoteAuthPreview {
        fingerprint: auth_fingerprint(
            &endpoint.endpoint,
            &endpoint.model,
            &endpoint.revision,
            endpoint.dim,
            projects,
        ),
        endpoint: endpoint.endpoint.clone(),
        model: endpoint.model.clone(),
        revision: endpoint.revision.clone(),
        dim: endpoint.dim,
        fields: FIELDS.iter().map(|field| (*field).to_owned()).collect(),
        projects: projects.clone(),
        note: "本次只记录授权，不发送原文或译文。之后建库和查询只会发送已授权项目的原文。".into(),
    })
}

pub fn store_remote_auth(
    root: &Path,
    endpoint: &RemoteEndpoint,
    projects: &BTreeSet<String>,
    confirm: &str,
) -> Result<RemoteAuthPreview, AppError> {
    let preview = preview_remote_auth(endpoint, projects)?;
    if preview.fingerprint != confirm {
        return Err(AppError::External(
            "授权预览已变化，请重新预览后再确认；未写入授权，也未联网".into(),
        ));
    }
    for project in projects {
        Translations::new(root)
            .project(project)
            .map_err(|_| AppError::External(format!("项目 {project} 不存在，不能授权")))?;
    }
    let stored = StoredAuth {
        version: 1,
        provider: "remote".into(),
        endpoint: endpoint.endpoint.clone(),
        model: endpoint.model.clone(),
        revision: endpoint.revision.clone(),
        dim: endpoint.dim,
        fields: preview.fields.clone(),
        projects: projects.clone(),
        fingerprint: preview.fingerprint.clone(),
    };
    let path = auth_path(root)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| AppError::External(error.to_string()))?;
        if std::fs::symlink_metadata(parent)
            .map(|meta| meta.file_type().is_symlink() || !meta.is_dir())
            .unwrap_or(true)
        {
            return Err(AppError::External("记忆目录须为普通目录".into()));
        }
    }
    let bytes = serde_json::to_vec_pretty(&stored)
        .map_err(|error| AppError::External(error.to_string()))?;
    let temporary = path.with_extension("json.partial");
    std::fs::write(&temporary, &bytes).map_err(|error| AppError::External(error.to_string()))?;
    std::fs::rename(&temporary, &path).map_err(|error| AppError::External(error.to_string()))?;
    Ok(preview)
}

pub fn open(
    root: &Path,
    requested: &BTreeSet<String>,
    selection: MemorySelection,
) -> Result<Box<dyn EmbeddingProvider>, AppError> {
    match selection {
        MemorySelection::Local => Ok(Box::new(LocalProvider::new())),
        MemorySelection::Deterministic => Ok(Box::new(DeterministicProvider::new())),
        MemorySelection::Remote(endpoint) => {
            let provider =
                open_remote_with_transport(root, requested, &endpoint, Box::new(HttpTransport))?;
            Ok(Box::new(provider))
        }
    }
}

pub fn open_remote_with_transport(
    root: &Path,
    requested: &BTreeSet<String>,
    endpoint: &RemoteEndpoint,
    transport: Box<dyn EmbedTransport>,
) -> Result<RemoteProvider, AppError> {
    let stored = read_auth(root)?;
    if stored.version != 1 || stored.provider != "remote" || stored.fields != ["source"] {
        return Err(AppError::External(
            "远程嵌入授权范围不是「只发送原文」；请重新授权。未联网".into(),
        ));
    }
    let fingerprint = auth_fingerprint(
        &stored.endpoint,
        &stored.model,
        &stored.revision,
        stored.dim,
        &stored.projects,
    );
    if stored.fingerprint != fingerprint
        || stored.endpoint != endpoint.endpoint
        || stored.model != endpoint.model
        || stored.revision != endpoint.revision
        || stored.dim != endpoint.dim
    {
        return Err(AppError::External(
            "远程嵌入授权与当前端点或模型不一致，须重新授权；未联网".into(),
        ));
    }
    if requested
        .iter()
        .any(|project| !stored.projects.contains(project))
    {
        return Err(AppError::External(
            "有项目不在远程嵌入授权内；未发送该项目的原文".into(),
        ));
    }
    RemoteProvider::armed(
        endpoint.endpoint.clone(),
        endpoint.model.clone(),
        endpoint.revision.clone(),
        endpoint.dim,
        endpoint.api_key.clone(),
        transport,
    )
    .map_err(|error| AppError::External(error.to_string()))
}

fn read_auth(root: &Path) -> Result<StoredAuth, AppError> {
    let path = auth_path(root)?;
    let meta = std::fs::symlink_metadata(&path).map_err(|_| {
        AppError::External("远程嵌入尚未授权；请先运行 text memory authorize。未联网".into())
    })?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(AppError::External("授权文件须为普通文件；未联网".into()));
    }
    let bytes = std::fs::read(&path).map_err(|error| AppError::External(error.to_string()))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| AppError::External("远程嵌入授权无法读取，请重新授权；未联网".into()))
}

fn auth_path(root: &Path) -> Result<PathBuf, AppError> {
    Ok(root.join(".ziyuan/memory/remote-authorization.json"))
}

fn auth_fingerprint(
    endpoint: &str,
    model: &str,
    revision: &str,
    dim: usize,
    projects: &BTreeSet<String>,
) -> String {
    let body = CanonicalAuth {
        version: 1,
        provider: "remote",
        endpoint,
        model,
        revision,
        dim,
        fields: &FIELDS,
        projects,
    };
    sha256_hex(&serde_json::to_vec(&body).unwrap_or_default())
}

struct HttpTransport;

impl EmbedTransport for HttpTransport {
    fn post_embeddings(
        &self,
        request: &ziyuan_memory::EmbedCall<'_>,
    ) -> Result<Vec<Vec<f32>>, MemoryError> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .build()
            .into();
        let payload = serde_json::to_vec(&serde_json::json!({
            "model": request.model,
            "input": request.texts,
        }))
        .map_err(|_| MemoryError::Config("远程嵌入请求无法编码".into()))?;
        let mut call = agent
            .post(request.endpoint)
            .header("Content-Type", "application/json");
        if !request.api_key.is_empty() {
            call = call.header("Authorization", format!("Bearer {}", request.api_key));
        }
        let response = call
            .send(&payload)
            .map_err(|_| MemoryError::Config("远程嵌入请求失败，未记录正文或密钥".into()))?;
        let status = response.status();
        let mut body = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(8 * 1024 * 1024)
            .read_to_end(&mut body)
            .map_err(|_| MemoryError::Config("远程嵌入响应无法读取，未记录正文".into()))?;
        if !status.is_success() {
            return Err(MemoryError::Config(format!(
                "远程嵌入被拒绝，HTTP {}；未记录响应正文",
                status.as_u16()
            )));
        }
        #[derive(Deserialize)]
        struct Payload {
            data: Vec<Item>,
        }
        #[derive(Deserialize)]
        struct Item {
            index: usize,
            embedding: Vec<f32>,
        }
        let payload: Payload = serde_json::from_slice(&body)
            .map_err(|_| MemoryError::Config("远程嵌入响应无法解析，未记录响应正文".into()))?;
        let mut data = payload.data;
        data.sort_by_key(|item| item.index);
        Ok(data.into_iter().map(|item| item.embedding).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use ziyuan_memory::EmbedCall;

    struct Bomb(Arc<AtomicUsize>);
    impl EmbedTransport for Bomb {
        fn post_embeddings(&self, _: &EmbedCall<'_>) -> Result<Vec<Vec<f32>>, MemoryError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(MemoryError::Config("传输不该被调用".into()))
        }
    }

    #[test]
    fn missing_authorization_does_not_touch_transport() {
        let root = std::env::temp_dir().join(format!("ziyuan-remote-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".ziyuan")).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let endpoint = RemoteEndpoint {
            endpoint: "https://embed.example/v1".into(),
            model: "m".into(),
            revision: "1".into(),
            dim: 2,
            api_key: "secret".into(),
        };
        let error = match open_remote_with_transport(
            &root,
            &BTreeSet::from(["p".into()]),
            &endpoint,
            Box::new(Bomb(calls.clone())),
        ) {
            Err(error) => error,
            Ok(_) => panic!("未授权不得装配远程提供方"),
        };
        assert!(error.to_string().contains("尚未授权"), "{error}");
        assert!(error.to_string().contains("未联网"), "{error}");
        assert!(!error.to_string().contains("secret"), "{error}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_dir_all(root);
    }
}
