//! 模型 Adapter：OpenAI 兼容的 Chat Completions API（OpenAI、DeepSeek、通义千问、Kimi、Ollama 等），
//! 以及用 ChatGPT 订阅额度调用的 Responses API（见 [`chatgpt`]）。
//!
//! 同步、流式（SSE）；文本片段经 `ExecutionHooks::text_delta` 实时报告。只实现 Tool Calling 所需的最小字段。
use std::collections::BTreeMap;
use std::env;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use ziyuan_core::Price;
use ziyuan_runtime::agent_loop::{Message, ModelProvider, ModelTurn, ToolSpec};
use ziyuan_runtime::{BoxError, ExecutionHooks, ModelCatalog};

pub mod anthropic;
pub mod chatgpt;
pub mod config;
pub mod gemini;
mod openai;
mod responses;

pub use anthropic::AnthropicMessages;
pub use config::{ModelProfile, ModelsConfig, ProfileSummary, ProviderKind};
pub use gemini::GeminiNative;
pub use openai::OpenAiCompatible;

pub(crate) const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// 度支余额低于此数时才向 API 传 `max_tokens`（再高的值有些服务会拒绝）。
pub(crate) const MAX_TOKENS_CAP: u64 = 8192;

/// 请求失败（网络错误、429、5xx）时的重试：第 n 次重试前等待 `base × 2ⁿ`（不超过 `max`），
/// 服务端给了 `Retry-After` 则照办（同样不超过 `max`）。流式响应开始之后不重试。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retry {
    pub attempts: u32,
    pub base: Duration,
    pub max: Duration,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            attempts: 3,
            base: Duration::from_secs(1),
            max: Duration::from_secs(30),
        }
    }
}

impl Retry {
    pub(crate) fn delay(&self, retry: u32, after: Option<Duration>) -> Duration {
        after
            .unwrap_or_else(|| self.base.saturating_mul(1 << retry.min(16)))
            .min(self.max)
    }
}

/// 提示词缓存（Prompt Caching）控制策略。
///
/// - [`PromptCache::Auto`]（默认）：对 Claude 模型或 Anthropic 端点自动附加
///   `cache_control: {"type": "ephemeral"}`（覆盖 system prompt、工具定义末尾与历史断点）；
///   对 OpenAI / DeepSeek 等原生自动前缀缓存后端保持原始消息格式不破坏兼容性。
/// - [`PromptCache::Anthropic`]：强制附加 Anthropic 风格 `cache_control` 断点
///   （适用于自定义模型名经网关转发至 Claude 的场景）。
/// - [`PromptCache::Off`]：不附加 `cache_control` 字段（仍照常统计响应中的缓存命中用量）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PromptCache {
    #[default]
    Auto,
    Anthropic,
    Off,
}

impl PromptCache {
    pub fn parse(raw: &str) -> Result<Self, BoxError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(Self::Auto),
            "anthropic" | "on" | "1" | "true" => Ok(Self::Anthropic),
            "off" | "0" | "false" => Ok(Self::Off),
            other => Err(format!(
                "ZIYUAN_LLM_PROMPT_CACHE 只能是 auto、anthropic/on 或 off，而不是 {other}"
            )
            .into()),
        }
    }

    pub fn from_env() -> Result<Self, BoxError> {
        match env::var("ZIYUAN_LLM_PROMPT_CACHE") {
            Ok(raw) => Self::parse(&raw),
            Err(_) => Ok(Self::Auto),
        }
    }

    pub(crate) fn uses_anthropic_cache_control(self, base_url: &str, model: &str) -> bool {
        match self {
            Self::Off => false,
            Self::Anthropic => true,
            Self::Auto => {
                model.to_ascii_lowercase().contains("claude")
                    || base_url.to_ascii_lowercase().contains("anthropic")
            }
        }
    }
}

/// 凭据：API Key（走 Chat Completions），或 ChatGPT 订阅（走 Responses API）。
#[derive(Clone)]
pub(crate) enum Auth {
    Key(Option<String>),
    Plan(Arc<chatgpt::Account>),
}

/// 一次请求的失败：可否重试、服务端要求等多久、服务端的请求 ID；
/// `reauth` 为被拒的订阅访问令牌（401），续期后可再试一次。
pub(crate) struct Failure {
    pub(crate) error: BoxError,
    pub(crate) retryable: bool,
    pub(crate) after: Option<Duration>,
    pub(crate) request_id: Option<String>,
    pub(crate) reauth: Option<String>,
}

/// 某项配置缺少必需凭据或账号时返回的占位提供方：仅在真正调用该配置时报错，
/// 避免影响朝廷中其他使用正常配置的官职。
struct FailingProvider {
    model: String,
    message: String,
}

impl ModelProvider for FailingProvider {
    fn model_name(&self) -> Option<&str> {
        Some(&self.model)
    }

    fn next_turn(
        &self,
        _messages: &[Message],
        _tools: &[ToolSpec],
        _hooks: &dyn ExecutionHooks,
    ) -> Result<ModelTurn, BoxError> {
        Err(self.message.clone().into())
    }
}

/// 异构多模型目录：优先按 `.ziyuan/models.json` 中的配置名路由至对应提供方协议、
/// 端点与凭据环境变量；未登记于配置表时回落到默认提供方。
pub struct Catalog {
    pub(crate) default_kind: ProviderKind,
    pub(crate) base_url: String,
    pub(crate) auth: Auth,
    pub(crate) retry: Retry,
    pub(crate) prompt_cache: PromptCache,
    pub(crate) profiles: BTreeMap<String, ModelProfile>,
}

impl Catalog {
    pub fn with_profiles(mut self, profiles: BTreeMap<String, ModelProfile>) -> Self {
        self.profiles = profiles;
        self
    }

    /// 按配置表或默认提供方构造单次模型实例（供异构路由与单测使用）。
    fn build_from_profile(&self, profile: &ModelProfile) -> Box<dyn ModelProvider> {
        let prompt_cache = profile.prompt_cache.unwrap_or(self.prompt_cache);
        let api_key = match &profile.api_key_env {
            Some(var) => match env::var(var).ok().filter(|v| !v.is_empty()) {
                Some(key) => Some(key),
                None => {
                    return Box::new(FailingProvider {
                        model: profile.name.clone(),
                        message: format!(
                            "模型配置 {} 需要环境变量 {var}（当前未设置）",
                            profile.name
                        ),
                    });
                }
            },
            None => None,
        };
        match profile.provider {
            ProviderKind::OpenAi => Box::new(
                OpenAiCompatible::new(profile.base_url.clone(), api_key, profile.model.clone())
                    .with_display_name(profile.name.clone())
                    .with_retry(self.retry)
                    .with_prompt_cache(prompt_cache),
            ),
            ProviderKind::Anthropic => Box::new(
                AnthropicMessages::new(profile.base_url.clone(), api_key, profile.model.clone())
                    .with_display_name(profile.name.clone())
                    .with_retry(self.retry)
                    .with_prompt_cache(prompt_cache),
            ),
            ProviderKind::Gemini => Box::new(
                GeminiNative::new(profile.base_url.clone(), api_key, profile.model.clone())
                    .with_display_name(profile.name.clone())
                    .with_retry(self.retry),
            ),
            ProviderKind::ChatGpt => match chatgpt::Account::from_env() {
                Ok(account) => match account.load() {
                    Ok(Some(creds)) if creds.plan_enabled() => Box::new(
                        OpenAiCompatible::chatgpt_plan(
                            Arc::new(account),
                            profile.base_url.clone(),
                            profile.model.clone(),
                        )
                        .with_display_name(profile.name.clone())
                        .with_retry(self.retry),
                    ),
                    Ok(_) => Box::new(FailingProvider {
                        model: profile.name.clone(),
                        message: format!(
                            "模型配置 {} 使用 ChatGPT 订阅，但尚未登录，请先执行 `ziyuan login`",
                            profile.name
                        ),
                    }),
                    Err(e) => Box::new(FailingProvider {
                        model: profile.name.clone(),
                        message: format!("模型配置 {} 读取 ChatGPT 凭据失败: {e}", profile.name),
                    }),
                },
                Err(e) => Box::new(FailingProvider {
                    model: profile.name.clone(),
                    message: format!("模型配置 {} 初始化 ChatGPT 账号失败: {e}", profile.name),
                }),
            },
        }
    }

    fn build_default_provider(&self, model: &str) -> Box<dyn ModelProvider> {
        let key = match &self.auth {
            Auth::Key(k) => k.clone(),
            Auth::Plan(_) => None,
        };
        match self.default_kind {
            ProviderKind::OpenAi | ProviderKind::ChatGpt => {
                let mut provider = OpenAiCompatible::new(self.base_url.clone(), None, model);
                provider.auth = self.auth.clone();
                Box::new(
                    provider
                        .with_retry(self.retry)
                        .with_prompt_cache(self.prompt_cache),
                )
            }
            ProviderKind::Anthropic => Box::new(
                AnthropicMessages::new(self.base_url.clone(), key, model)
                    .with_retry(self.retry)
                    .with_prompt_cache(self.prompt_cache),
            ),
            ProviderKind::Gemini => Box::new(
                GeminiNative::new(self.base_url.clone(), key, model).with_retry(self.retry),
            ),
        }
    }
}

impl ModelCatalog for Catalog {
    fn provider(&self, model: &str) -> Box<dyn ModelProvider> {
        if let Some(profile) = self.profiles.get(model) {
            return self.build_from_profile(profile);
        }
        self.build_default_provider(model)
    }

    /// 订阅额度不按 token 计价，不估算花费；`.ziyuan/models.json` 自定义单价优先于内置价目。
    fn price(&self, model: &str) -> Option<Price> {
        if let Some(profile) = self.profiles.get(model) {
            if profile.provider == ProviderKind::ChatGpt {
                return None;
            }
            return profile.price.clone().or_else(|| price_for(&profile.model));
        }
        match self.auth {
            Auth::Key(_) => price_for(model),
            Auth::Plan(_) => None,
        }
    }
}

/// 解析后的默认执行模型与异构路由目录（整合环境变量与可选 `.ziyuan/models.json`）。
pub struct ResolvedBackend {
    pub provider: Box<dyn ModelProvider>,
    pub catalog: Catalog,
    pub model_label: String,
    pub provider_kind: ProviderKind,
    pub uses_chatgpt_plan: bool,
}

fn retry_from_env() -> Retry {
    let mut retry = Retry::default();
    if let Some(n) = env::var("ZIYUAN_LLM_RETRIES")
        .ok()
        .and_then(|n| n.parse().ok())
    {
        retry.attempts = n;
    }
    retry
}

fn provider_from_profile_strict(
    profile: &ModelProfile,
    retry: Retry,
    default_prompt_cache: PromptCache,
) -> Result<(Box<dyn ModelProvider>, Auth), BoxError> {
    let prompt_cache = profile.prompt_cache.unwrap_or(default_prompt_cache);
    let api_key = match &profile.api_key_env {
        Some(var) => {
            let val = env::var(var)
                .ok()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    format!(
                        "默认模型配置 {} 需要环境变量 {var}（当前未设置）",
                        profile.name
                    )
                })?;
            Some(val)
        }
        None => None,
    };
    match profile.provider {
        ProviderKind::OpenAi => Ok((
            Box::new(
                OpenAiCompatible::new(
                    profile.base_url.clone(),
                    api_key.clone(),
                    profile.model.clone(),
                )
                .with_display_name(profile.name.clone())
                .with_retry(retry)
                .with_prompt_cache(prompt_cache),
            ),
            Auth::Key(api_key),
        )),
        ProviderKind::Anthropic => Ok((
            Box::new(
                AnthropicMessages::new(
                    profile.base_url.clone(),
                    api_key.clone(),
                    profile.model.clone(),
                )
                .with_display_name(profile.name.clone())
                .with_retry(retry)
                .with_prompt_cache(prompt_cache),
            ),
            Auth::Key(api_key),
        )),
        ProviderKind::Gemini => Ok((
            Box::new(
                GeminiNative::new(
                    profile.base_url.clone(),
                    api_key.clone(),
                    profile.model.clone(),
                )
                .with_display_name(profile.name.clone())
                .with_retry(retry),
            ),
            Auth::Key(api_key),
        )),
        ProviderKind::ChatGpt => {
            let account = chatgpt::Account::from_env()?;
            if !account.load()?.is_some_and(|c| c.plan_enabled()) {
                return Err(format!(
                    "默认模型配置 {} 使用 ChatGPT 订阅，但尚未登录，请先执行 `ziyuan login`",
                    profile.name
                )
                .into());
            }
            let account = Arc::new(account);
            Ok((
                Box::new(
                    OpenAiCompatible::chatgpt_plan(
                        account.clone(),
                        profile.base_url.clone(),
                        profile.model.clone(),
                    )
                    .with_display_name(profile.name.clone())
                    .with_retry(retry),
                ),
                Auth::Plan(account),
            ))
        }
    }
}

/// 统一解析模型后端与异构路由目录：
///
/// 优先级：`ZIYUAN_LLM_*` 环境变量覆盖默认模型（若仅设 `ZIYUAN_LLM_MODEL` 且其值匹配
/// `.ziyuan/models.json` 中的配置名，则直接启用该配置）；未设 `ZIYUAN_LLM_MODEL` 时若
/// `.ziyuan/models.json` 指定了 `default`，则以该默认配置启动；否则返回 `Ok(None)`（演示模式）。
/// 只要存在 `.ziyuan/models.json`，其中全部配置均注入 `Catalog` 供按官职／按部异构路由。
pub fn resolve_backend(root: Option<&Path>) -> Result<Option<ResolvedBackend>, BoxError> {
    let config = match root {
        Some(dir) => ModelsConfig::load(dir)?,
        None => None,
    };
    let profiles = config
        .as_ref()
        .map(|c| c.profiles.clone())
        .unwrap_or_default();
    let retry = retry_from_env();
    let prompt_cache = PromptCache::from_env()?;

    let env_model = env::var("ZIYUAN_LLM_MODEL").ok().filter(|m| !m.is_empty());
    let env_provider = env::var("ZIYUAN_LLM_PROVIDER")
        .ok()
        .filter(|p| !p.is_empty());
    let env_base_url = env::var("ZIYUAN_LLM_BASE_URL")
        .ok()
        .filter(|u| !u.is_empty());
    let env_api_key = env::var("ZIYUAN_LLM_API_KEY")
        .ok()
        .filter(|k| !k.is_empty());

    // 若仅设 ZIYUAN_LLM_MODEL 且命中 models.json 配置名，则直接启用该配置。
    if let Some(model_name) = &env_model
        && env_provider.is_none()
        && env_base_url.is_none()
        && env_api_key.is_none()
        && let Some(profile) = profiles.get(model_name)
    {
        let (provider, auth) = provider_from_profile_strict(profile, retry, prompt_cache)?;
        let catalog = Catalog {
            default_kind: profile.provider,
            base_url: profile.base_url.clone(),
            auth,
            retry,
            prompt_cache: profile.prompt_cache.unwrap_or(prompt_cache),
            profiles: profiles.clone(),
        };
        return Ok(Some(ResolvedBackend {
            provider,
            catalog,
            model_label: profile.name.clone(),
            provider_kind: profile.provider,
            uses_chatgpt_plan: profile.provider == ProviderKind::ChatGpt,
        }));
    }

    if let Some(model) = env_model {
        let kind = match env_provider.as_deref() {
            Some(raw) => ProviderKind::parse(raw)?,
            None => ProviderKind::OpenAi,
        };
        let base_url = env_base_url.unwrap_or_else(|| kind.default_base_url().to_owned());
        let (provider, auth): (Box<dyn ModelProvider>, Auth) = match kind {
            ProviderKind::OpenAi => (
                Box::new(
                    OpenAiCompatible::new(base_url.clone(), env_api_key.clone(), model.clone())
                        .with_retry(retry)
                        .with_prompt_cache(prompt_cache),
                ),
                Auth::Key(env_api_key),
            ),
            ProviderKind::Anthropic => (
                Box::new(
                    AnthropicMessages::new(base_url.clone(), env_api_key.clone(), model.clone())
                        .with_retry(retry)
                        .with_prompt_cache(prompt_cache),
                ),
                Auth::Key(env_api_key),
            ),
            ProviderKind::Gemini => (
                Box::new(
                    GeminiNative::new(base_url.clone(), env_api_key.clone(), model.clone())
                        .with_retry(retry),
                ),
                Auth::Key(env_api_key),
            ),
            ProviderKind::ChatGpt => {
                let account = chatgpt::Account::from_env()?;
                if !account.load()?.is_some_and(|c| c.plan_enabled()) {
                    return Err("ZIYUAN_LLM_PROVIDER=chatgpt：尚未登录 ChatGPT 订阅，请先执行 `ziyuan login`".into());
                }
                let account = Arc::new(account);
                (
                    Box::new(
                        OpenAiCompatible::chatgpt_plan(
                            account.clone(),
                            base_url.clone(),
                            model.clone(),
                        )
                        .with_retry(retry)
                        .with_prompt_cache(prompt_cache),
                    ),
                    Auth::Plan(account),
                )
            }
        };
        let catalog = Catalog {
            default_kind: kind,
            base_url,
            auth,
            retry,
            prompt_cache,
            profiles,
        };
        return Ok(Some(ResolvedBackend {
            provider,
            catalog,
            model_label: model,
            provider_kind: kind,
            uses_chatgpt_plan: kind == ProviderKind::ChatGpt,
        }));
    }

    if let Some(cfg) = config
        && let Some(default_name) = &cfg.default_profile
        && let Some(profile) = cfg.profiles.get(default_name).cloned()
    {
        let (provider, auth) = provider_from_profile_strict(&profile, retry, prompt_cache)?;
        let catalog = Catalog {
            default_kind: profile.provider,
            base_url: profile.base_url.clone(),
            auth,
            retry,
            prompt_cache: profile.prompt_cache.unwrap_or(prompt_cache),
            profiles: cfg.profiles,
        };
        return Ok(Some(ResolvedBackend {
            provider,
            catalog,
            model_label: profile.name,
            provider_kind: profile.provider,
            uses_chatgpt_plan: profile.provider == ProviderKind::ChatGpt,
        }));
    }

    Ok(None)
}

/// 估算花费所用的单价（支持 `.ziyuan/models.json` 配置单价、内置参考价目与 `ZIYUAN_LLM_PRICE`）。
pub fn price_for(model: &str) -> Option<Price> {
    price_quote(model).map(|(price, _)| price)
}

/// 按项目 `.ziyuan/models.json`（若有）与内置价目查询模型估算单价。
pub fn price_for_in_workspace(root: Option<&Path>, model: &str) -> Option<Price> {
    if let Some(dir) = root
        && let Ok(Some(cfg)) = ModelsConfig::load(dir)
        && let Some(profile) = cfg.profiles.get(model)
    {
        if profile.provider == ProviderKind::ChatGpt {
            return None;
        }
        return profile.price.clone().or_else(|| price_for(&profile.model));
    }
    price_for(model)
}

/// Current estimation rate and its provenance, not a historical billing rate.
pub fn price_quote(model: &str) -> Option<(Price, &'static str)> {
    let bare = model
        .rsplit_once('/')
        .map_or(model, |(_, suffix)| suffix)
        .trim();
    let lower = bare.to_ascii_lowercase();

    // DeepSeek（人民币元 / 百万 token）
    let deepseek = match lower.as_str() {
        "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4-flash-vision-exp" => Some((2.0, 8.0)),
        "deepseek-v4-pro" => Some((9.0, 27.0)),
        "deepseek-chat" => Some((2.0, 8.0)),
        "deepseek-reasoner" => Some((4.0, 16.0)),
        _ => None,
    };
    if let Some((input, output)) = deepseek {
        return Some((
            Price {
                input_per_million: input,
                output_per_million: output,
                currency: "元".into(),
            },
            "内置 DeepSeek 2026-10 高峰未命中缓存估价（未实时核价）",
        ));
    }

    // 主流国际旗舰参考估价（USD / 百万 token，未实时核价，仅作估算上限）
    let usd = if lower.starts_with("claude-opus-4") {
        Some((15.0, 75.0))
    } else if lower.starts_with("claude-sonnet-4")
        || lower.starts_with("claude-3-7-sonnet")
        || lower.starts_with("claude-3-5-sonnet")
    {
        Some((3.0, 15.0))
    } else if lower.starts_with("claude-haiku-4") || lower.starts_with("claude-3-5-haiku") {
        Some((0.8, 4.0))
    } else if lower.starts_with("gpt-5-mini") {
        Some((0.25, 2.0))
    } else if lower == "gpt-5" || lower.starts_with("gpt-5-") {
        Some((1.25, 10.0))
    } else if lower.starts_with("gpt-4.1-mini") {
        Some((0.40, 1.60))
    } else if lower == "gpt-4.1" || lower.starts_with("gpt-4.1-") {
        Some((2.0, 8.0))
    } else if lower.starts_with("gpt-4o-mini") {
        Some((0.15, 0.60))
    } else if lower == "gpt-4o" || lower.starts_with("gpt-4o-") {
        Some((2.5, 10.0))
    } else if lower.starts_with("gemini-2.5-pro") {
        Some((1.25, 10.0))
    } else if lower.starts_with("gemini-2.5-flash") {
        Some((0.15, 0.60))
    } else {
        None
    };
    if let Some((input, output)) = usd {
        return Some((
            Price {
                input_per_million: input,
                output_per_million: output,
                currency: "USD".into(),
            },
            "内置旗舰参考估价（未实时核价，仅作估算）",
        ));
    }

    env::var("ZIYUAN_LLM_PRICE")
        .ok()
        .and_then(|s| parse_price(&s))
        .map(|price| (price, "ZIYUAN_LLM_PRICE（本次读取，非历史费率）"))
}

fn parse_price(spec: &str) -> Option<Price> {
    let mut parts = spec.split(',').map(str::trim);
    let input: f64 = parts.next()?.parse().ok()?;
    let output: f64 = parts.next()?.parse().ok()?;
    let currency = parts.next().filter(|c| !c.is_empty()).unwrap_or("元");
    if !input.is_finite() || !output.is_finite() || input < 0.0 || output < 0.0 {
        return None;
    }
    Some(Price {
        input_per_million: input,
        output_per_million: output,
        currency: currency.into(),
    })
}

#[cfg(test)]
mod tests;
