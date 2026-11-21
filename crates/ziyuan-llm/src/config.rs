//! 多模型配置（`.ziyuan/models.json`）：仅存非机密路由信息与环境变量名，绝不落盘明文 API Key。

use crate::{DEFAULT_BASE_URL, PromptCache, anthropic, chatgpt, gemini};
use serde_json::Value;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use ziyuan_core::Price;
use ziyuan_runtime::BoxError;

const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const MAX_PROFILES: usize = 64;

/// 支持的模型协议类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    /// OpenAI 兼容 Chat Completions（OpenAI、DeepSeek、Ollama、通义千问、Kimi 等）。
    OpenAi,
    /// 原生 Anthropic Messages API（`/v1/messages`，实验性）。
    Anthropic,
    /// 原生 Google Gemini API（`models/{model}:streamGenerateContent?alt=sse`，实验性）。
    Gemini,
    /// ChatGPT 订阅 Responses API（实验性）。
    ChatGpt,
}

impl ProviderKind {
    pub fn parse(raw: &str) -> Result<Self, BoxError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "api" | "openai" | "openai-compatible" => Ok(Self::OpenAi),
            "anthropic" => Ok(Self::Anthropic),
            "gemini" => Ok(Self::Gemini),
            "chatgpt" => Ok(Self::ChatGpt),
            other => Err(format!(
                "不支持的模型提供方 {other:?}（可选：openai/api、anthropic、gemini、chatgpt）"
            )
            .into()),
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::ChatGpt => "chatgpt",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI 兼容",
            Self::Anthropic => "Anthropic（实验性）",
            Self::Gemini => "Gemini（实验性）",
            Self::ChatGpt => "ChatGPT 订阅（实验性）",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::OpenAi => DEFAULT_BASE_URL,
            Self::Anthropic => anthropic::DEFAULT_BASE_URL,
            Self::Gemini => gemini::DEFAULT_BASE_URL,
            Self::ChatGpt => chatgpt::API_BASE,
        }
    }
}

/// `.ziyuan/models.json` 中的单个模型配置项（无明文密钥）。
#[derive(Clone, Debug, PartialEq)]
pub struct ModelProfile {
    pub name: String,
    pub provider: ProviderKind,
    pub model: String,
    pub base_url: String,
    /// 仅存环境变量名（如 `ANTHROPIC_API_KEY`），不存密钥值。
    pub api_key_env: Option<String>,
    pub prompt_cache: Option<PromptCache>,
    pub price: Option<Price>,
}

/// 对外只读展示用的模型配置摘要（绝不包含 API Key 明文）。
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileSummary {
    pub name: String,
    pub provider: String,
    pub provider_label: String,
    pub model: String,
    pub base_url: String,
    pub api_key_env: Option<String>,
    pub api_key_ready: bool,
    pub is_default: bool,
}

/// `.ziyuan/models.json` 解析结果。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelsConfig {
    pub default_profile: Option<String>,
    pub profiles: BTreeMap<String, ModelProfile>,
}

impl ModelsConfig {
    pub fn path(root: &Path) -> PathBuf {
        root.join(".ziyuan").join("models.json")
    }

    /// 读取 `<root>/.ziyuan/models.json`；文件不存在时返回 `Ok(None)`。
    pub fn load(root: &Path) -> Result<Option<Self>, BoxError> {
        let path = Self::path(root);
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("无法读取 {}: {e}", path.display()).into()),
        };
        if !meta.file_type().is_file() {
            return Err(format!("{} 须为普通文件（不允许符号链接或目录）", path.display()).into());
        }
        if meta.len() > MAX_CONFIG_BYTES {
            return Err(format!("{} 超过 {MAX_CONFIG_BYTES} 字节上限", path.display()).into());
        }
        let raw =
            fs::read_to_string(&path).map_err(|e| format!("无法读取 {}: {e}", path.display()))?;
        Self::parse(&raw)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display()).into())
    }

    pub fn parse(raw: &str) -> Result<Self, BoxError> {
        let value: Value = serde_json::from_str(raw).map_err(|e| format!("JSON 格式错误: {e}"))?;
        let obj = value
            .as_object()
            .ok_or("顶层须为 JSON 对象（含 profiles）")?;
        for key in obj.keys() {
            if !matches!(key.as_str(), "default" | "profiles") {
                return Err(format!("顶层存在未知字段 {key:?}").into());
            }
        }
        let profiles_obj = obj
            .get("profiles")
            .and_then(Value::as_object)
            .ok_or("缺少 profiles 对象")?;
        if profiles_obj.len() > MAX_PROFILES {
            return Err(format!("profiles 数量超过上限（最多 {MAX_PROFILES} 项）").into());
        }

        let mut profiles = BTreeMap::new();
        for (name, entry) in profiles_obj {
            validate_profile_name(name)?;
            let profile = parse_profile(name, entry)?;
            profiles.insert(name.clone(), profile);
        }

        let default_profile = match obj.get("default") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => {
                let s = s.trim();
                if s.is_empty() {
                    None
                } else {
                    if !profiles.contains_key(s) {
                        return Err(format!("default 指向的配置 {s:?} 不在 profiles 中").into());
                    }
                    Some(s.to_owned())
                }
            }
            Some(_) => return Err("default 须为字符串或 null".into()),
        };

        Ok(Self {
            default_profile,
            profiles,
        })
    }

    pub fn summaries(&self) -> Vec<ProfileSummary> {
        self.profiles
            .values()
            .map(|p| {
                let api_key_ready = match (p.provider, &p.api_key_env) {
                    (ProviderKind::ChatGpt, _) => chatgpt::Account::from_env()
                        .ok()
                        .and_then(|acc| acc.load().ok().flatten())
                        .is_some_and(|c| c.plan_enabled()),
                    (_, Some(var)) => env::var(var).ok().is_some_and(|v| !v.is_empty()),
                    (_, None) => true,
                };
                ProfileSummary {
                    name: p.name.clone(),
                    provider: p.provider.code().into(),
                    provider_label: p.provider.label().into(),
                    model: p.model.clone(),
                    base_url: p.base_url.clone(),
                    api_key_env: p.api_key_env.clone(),
                    api_key_ready,
                    is_default: self.default_profile.as_deref() == Some(p.name.as_str()),
                }
            })
            .collect()
    }
}

fn validate_profile_name(name: &str) -> Result<(), BoxError> {
    if name.is_empty() || name.len() > 64 || name.chars().any(char::is_whitespace) {
        return Err(format!("配置名 {name:?} 须为 1–64 字符且不含空白").into());
    }
    if name == "default" {
        return Err("配置名不能为保留字 \"default\"".into());
    }
    Ok(())
}

fn validate_env_var_name(var: &str) -> Result<(), BoxError> {
    let mut chars = var.chars();
    let Some(first) = chars.next() else {
        return Err("api_key_env 不能为空".into());
    };
    if var.len() > 128
        || !(first.is_ascii_alphabetic() || first == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(format!(
            "api_key_env {var:?} 不是合法的环境变量名（仅允许 ASCII 字母、数字与下划线，不能填明文密钥）"
        )
        .into());
    }
    Ok(())
}

fn parse_profile(name: &str, value: &Value) -> Result<ModelProfile, BoxError> {
    let obj = value
        .as_object()
        .ok_or_else(|| format!("配置 {name:?} 须为 JSON 对象"))?;
    for key in obj.keys() {
        if matches!(
            key.as_str(),
            "api_key" | "apiKey" | "key" | "token" | "secret"
        ) {
            return Err(format!(
                "配置 {name:?} 禁止写入明文密钥字段 {key:?}；请改用 \"api_key_env\" 指定环境变量名"
            )
            .into());
        }
        if !matches!(
            key.as_str(),
            "provider" | "model" | "base_url" | "api_key_env" | "prompt_cache" | "price"
        ) {
            return Err(format!("配置 {name:?} 存在未知字段 {key:?}").into());
        }
    }

    let provider = match obj.get("provider") {
        None | Some(Value::Null) => ProviderKind::OpenAi,
        Some(Value::String(s)) => ProviderKind::parse(s)?,
        Some(_) => return Err(format!("配置 {name:?} 的 provider 须为字符串").into()),
    };

    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty() && m.len() <= 128 && !m.chars().any(char::is_whitespace))
        .ok_or_else(|| format!("配置 {name:?} 缺少合法的 model 字段（非空且无空白）"))?
        .to_owned();

    let base_url = match obj.get("base_url") {
        None | Some(Value::Null) => provider.default_base_url().to_owned(),
        Some(Value::String(s)) => {
            let trimmed = s.trim().trim_end_matches('/');
            if trimmed.is_empty() {
                provider.default_base_url().to_owned()
            } else if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
                return Err(
                    format!("配置 {name:?} 的 base_url 须以 http:// 或 https:// 开头").into(),
                );
            } else {
                trimmed.to_owned()
            }
        }
        Some(_) => return Err(format!("配置 {name:?} 的 base_url 须为字符串").into()),
    };

    let api_key_env = match obj.get("api_key_env") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                validate_env_var_name(trimmed)?;
                Some(trimmed.to_owned())
            }
        }
        Some(_) => return Err(format!("配置 {name:?} 的 api_key_env 须为字符串").into()),
    };

    let prompt_cache = match obj.get("prompt_cache") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(PromptCache::parse(s)?),
        Some(_) => return Err(format!("配置 {name:?} 的 prompt_cache 须为字符串").into()),
    };

    let price = match obj.get("price") {
        None | Some(Value::Null) => None,
        Some(Value::Object(p)) => {
            for k in p.keys() {
                if !matches!(k.as_str(), "input" | "output" | "currency") {
                    return Err(format!("配置 {name:?} 的 price 存在未知字段 {k:?}").into());
                }
            }
            let input = p
                .get("input")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or_else(|| format!("配置 {name:?} 的 price.input 须为非负数字"))?;
            let output = p
                .get("output")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or_else(|| format!("配置 {name:?} 的 price.output 须为非负数字"))?;
            let currency = match p.get("currency") {
                None | Some(Value::Null) => "元".to_owned(),
                Some(Value::String(c)) if !c.trim().is_empty() => c.trim().to_owned(),
                Some(_) => {
                    return Err(format!("配置 {name:?} 的 price.currency 须为非空字符串").into());
                }
            };
            Some(Price {
                input_per_million: input,
                output_per_million: output,
                currency,
            })
        }
        Some(_) => return Err(format!("配置 {name:?} 的 price 须为对象").into()),
    };

    Ok(ModelProfile {
        name: name.to_owned(),
        provider,
        model,
        base_url,
        api_key_env,
        prompt_cache,
        price,
    })
}
