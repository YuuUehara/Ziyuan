//! 模型 Adapter：OpenAI 兼容的 Chat Completions API（OpenAI、DeepSeek、通义千问、Kimi、Ollama 等），
//! 以及用 ChatGPT 订阅额度调用的 Responses API（见 [`chatgpt`]）。
//!
//! 同步、流式（SSE）；文本片段经 `ExecutionHooks::text_delta` 实时报告。只实现 Tool Calling 所需的最小字段。
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::env;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use ziyuan_core::{Price, TokenUsage};
use ziyuan_runtime::agent_loop::{Message, ModelProvider, ModelTurn, ToolRequest, ToolSpec};
use ziyuan_runtime::{BoxError, ExecutionHooks, ModelCatalog, guard};

pub mod anthropic;
pub mod chatgpt;
pub mod config;
pub mod gemini;
mod responses;

pub use anthropic::AnthropicMessages;
pub use config::{ModelProfile, ModelsConfig, ProfileSummary, ProviderKind};
pub use gemini::GeminiNative;

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

    fn uses_anthropic_cache_control(self, base_url: &str, model: &str) -> bool {
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
enum Auth {
    Key(Option<String>),
    Plan(Arc<chatgpt::Account>),
}

pub struct OpenAiCompatible {
    base_url: String,
    auth: Auth,
    model: String,
    display_name: Option<String>,
    agent: ureq::Agent,
    retry: Retry,
    prompt_cache: PromptCache,
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

impl OpenAiCompatible {
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
    ) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            auth: Auth::Key(api_key),
            model: model.into(),
            display_name: None,
            agent,
            retry: Retry::default(),
            prompt_cache: PromptCache::Auto,
        }
    }

    /// 用 ChatGPT 订阅额度调用 `base_url`（通常为 [`chatgpt::API_BASE`]）的 Responses API。
    pub fn chatgpt_plan(
        account: Arc<chatgpt::Account>,
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let mut this = Self::new(base_url, None, model);
        this.auth = Auth::Plan(account);
        this
    }

    /// 用的是 ChatGPT 订阅额度。
    pub fn uses_chatgpt_plan(&self) -> bool {
        matches!(self.auth, Auth::Plan(_))
    }

    pub fn with_retry(mut self, retry: Retry) -> Self {
        self.retry = retry;
        self
    }

    pub fn with_prompt_cache(mut self, prompt_cache: PromptCache) -> Self {
        self.prompt_cache = prompt_cache;
        self
    }

    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    /// 读取 `ZIYUAN_LLM_MODEL`（必需）、`ZIYUAN_LLM_BASE_URL`、`ZIYUAN_LLM_API_KEY`；
    /// `ZIYUAN_LLM_PROVIDER=chatgpt` 改用 `ziyuan login` 登录的 ChatGPT 订阅（不需要 Key）。
    /// 未设置模型时返回 `None`；选了订阅却未登录、未授权时报错。
    pub fn from_env() -> Result<Option<Self>, BoxError> {
        let Some(model) = env::var("ZIYUAN_LLM_MODEL").ok().filter(|m| !m.is_empty()) else {
            return Ok(None);
        };
        let base_url = env::var("ZIYUAN_LLM_BASE_URL")
            .ok()
            .filter(|u| !u.is_empty());
        let api_key = env::var("ZIYUAN_LLM_API_KEY")
            .ok()
            .filter(|k| !k.is_empty());
        let mut retry = Retry::default();
        if let Some(n) = env::var("ZIYUAN_LLM_RETRIES")
            .ok()
            .and_then(|n| n.parse().ok())
        {
            retry.attempts = n;
        }
        let this = match env::var("ZIYUAN_LLM_PROVIDER").unwrap_or_default().as_str() {
            "" | "api" | "openai" => Self::new(
                base_url.unwrap_or_else(|| DEFAULT_BASE_URL.into()),
                api_key,
                model,
            ),
            "chatgpt" => {
                let account = chatgpt::Account::from_env()?;
                if !account.load()?.is_some_and(|c| c.plan_enabled()) {
                    return Err("ZIYUAN_LLM_PROVIDER=chatgpt：尚未登录 ChatGPT 订阅，请先执行 `ziyuan login`".into());
                }
                Self::chatgpt_plan(
                    Arc::new(account),
                    base_url.unwrap_or_else(|| chatgpt::API_BASE.into()),
                    model,
                )
            }
            other => {
                return Err(
                    format!("ZIYUAN_LLM_PROVIDER 只能是 api 或 chatgpt，而不是 {other}").into(),
                );
            }
        };
        let prompt_cache = PromptCache::from_env()?;
        Ok(Some(this.with_retry(retry).with_prompt_cache(prompt_cache)))
    }

    pub fn model(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.model)
    }

    /// 同一服务地址与 Key 下的模型目录，供按官职切换模型。
    pub fn catalog(&self) -> Catalog {
        let default_kind = match self.auth {
            Auth::Key(_) => ProviderKind::OpenAi,
            Auth::Plan(_) => ProviderKind::ChatGpt,
        };
        Catalog {
            default_kind,
            base_url: self.base_url.clone(),
            auth: self.auth.clone(),
            retry: self.retry,
            prompt_cache: self.prompt_cache,
            profiles: BTreeMap::new(),
        }
    }
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
    default_kind: ProviderKind,
    base_url: String,
    auth: Auth,
    retry: Retry,
    prompt_cache: PromptCache,
    profiles: BTreeMap<String, ModelProfile>,
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

impl ModelProvider for OpenAiCompatible {
    fn model_name(&self) -> Option<&str> {
        Some(self.model())
    }

    /// 每次调用带一个 trace ID（请求头 `X-Client-Request-Id`，重试沿用），出错时连同服务端的
    /// 请求 ID 写进错误信息，便于对照服务商的日志。
    fn next_turn(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
        hooks: &dyn ExecutionHooks,
    ) -> Result<ModelTurn, BoxError> {
        let body = match self.auth {
            Auth::Key(_) => {
                let mut encoded_messages = encode_messages(messages);
                let mut encoded_tools = if tools.is_empty() {
                    None
                } else {
                    Some(encode_tools(tools)?)
                };
                if self
                    .prompt_cache
                    .uses_anthropic_cache_control(&self.base_url, &self.model)
                {
                    apply_anthropic_cache_control(&mut encoded_messages, encoded_tools.as_mut());
                }
                let mut body = json!({
                    "model": self.model,
                    "messages": encoded_messages,
                    "stream": true,
                    // 让流的最后一块携带 usage；不支持的服务会忽略。
                    "stream_options": {"include_usage": true},
                });
                if let Some(encoded_tools) = encoded_tools {
                    body["tools"] = encoded_tools;
                }
                // 度支将尽：输出至多用完余额。
                if let Some(room) = hooks.max_output().filter(|&room| room < MAX_TOKENS_CAP) {
                    body[output_cap_field(&self.base_url)] = json!(room.max(1));
                }
                body
            }
            // 订阅额度不收输出上限：度支只能在调用前核算（agent_loop 的余量检查）与事后记账。
            Auth::Plan(_) => responses::body(&self.model, messages, tools)?,
        }
        .to_string();
        let trace = format!("zy-{}", &guard::nonce()[..12]);
        hooks.model_trace(&trace);
        let mut retry = 0;
        let mut reauthed = false;
        loop {
            let failure = match self.attempt(&body, &trace, hooks) {
                Ok(turn) => return Ok(turn),
                Err(failure) => failure,
            };
            // 订阅访问令牌被拒：续期后再试一次，不计入重试。
            if let (Auth::Plan(account), Some(stale), false) =
                (&self.auth, &failure.reauth, reauthed)
            {
                reauthed = true;
                account
                    .refresh(Some(stale.as_str()))
                    .map_err(|e| format!("{e}（trace {trace}）"))?;
                continue;
            }
            if !failure.retryable || retry >= self.retry.attempts || hooks.is_cancelled() {
                let server = failure
                    .request_id
                    .map_or(String::new(), |id| format!("，服务端 {id}"));
                let tries = if retry > 0 {
                    format!("，已重试 {retry} 次")
                } else {
                    String::new()
                };
                return Err(format!("{}（trace {trace}{server}{tries}）", failure.error).into());
            }
            // 退避期间也响应撤销。
            let wait = self.retry.delay(retry, failure.after);
            let until = std::time::Instant::now() + wait;
            while std::time::Instant::now() < until {
                if hooks.is_cancelled() {
                    return Err("已撤销，不再重试".into());
                }
                std::thread::sleep(
                    (until - std::time::Instant::now()).min(Duration::from_millis(50)),
                );
            }
            retry += 1;
        }
    }
}

impl OpenAiCompatible {
    fn attempt(
        &self,
        body: &str,
        trace: &str,
        hooks: &dyn ExecutionHooks,
    ) -> Result<ModelTurn, Failure> {
        let fail = |error: BoxError, retryable: bool| Failure {
            error,
            retryable,
            after: None,
            request_id: None,
            reauth: None,
        };
        let (path, token) = match &self.auth {
            Auth::Key(key) => ("chat/completions", key.clone()),
            Auth::Plan(account) => (
                "responses",
                Some(account.access_token().map_err(|e| fail(e, false))?),
            ),
        };
        let mut request = self
            .agent
            .post(format!("{}/{path}", self.base_url))
            .header("Content-Type", "application/json")
            .header("X-Client-Request-Id", trace);
        if let Some(token) = &token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        // 连接失败、超时等网络错误可以重试。
        let response = request
            .send(body)
            .map_err(|e| fail(format!("模型 API 请求失败: {e}").into(), true))?;
        let status = response.status();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let request_id = header("x-request-id").or_else(|| header("request-id"));
        let after = header("retry-after")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let is_stream = header("content-type").is_some_and(|v| v.starts_with("text/event-stream"));
        let mut body = response.into_body();
        let failed = |error: BoxError, retryable: bool| Failure {
            error,
            retryable,
            after,
            request_id: request_id.clone(),
            reauth: None,
        };
        if !status.is_success() {
            let text = body.read_to_string().unwrap_or_default();
            let snippet: String = text.chars().take(500).collect();
            let mut retryable = status.as_u16() == 429 || status.is_server_error();
            let mut error = format!("模型 API 返回 {status}: {snippet}");
            if let Auth::Plan(_) = self.auth {
                let (plan_retryable, note) = responses::plan_error(status.as_u16(), &text);
                retryable = plan_retryable;
                if let Some(note) = note {
                    error = format!("{note}（{status}）");
                }
                if status.as_u16() == 401 {
                    return Err(Failure {
                        reauth: token,
                        ..failed(error.into(), false)
                    });
                }
            }
            return Err(failed(error.into(), retryable));
        }
        // 以下已开始接收答复（可能已流式显示了文字），出错不再重试。
        if let Auth::Plan(_) = self.auth {
            return responses::read_events(BufReader::new(body.into_reader()), hooks)
                .map_err(|e| failed(e, false));
        }
        if !is_stream {
            // 不支持流式的服务会忽略 `stream`，直接返回完整 JSON。
            let parsed = body
                .read_to_string()
                .map_err(BoxError::from)
                .and_then(|text| serde_json::from_str::<Value>(&text).map_err(BoxError::from));
            let response = parsed.map_err(|e| failed(e, false))?;
            report_usage(&response, hooks);
            let turn = decode_turn(&response).map_err(|e| failed(e, false))?;
            if let ModelTurn::Final(text) = &turn {
                hooks.text_delta(text);
            }
            return Ok(turn);
        }
        read_stream(BufReader::new(body.into_reader()), hooks).map_err(|e| failed(e, false))
    }
}

/// 流式响应中逐片拼接的工具调用。
#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

/// 解析 SSE（`data: {chunk}` … `data: [DONE]`）。文本片段实时交给 `hooks`；
/// 工具调用按 `index` 拼接。每读一行检查一次撤销。
fn read_stream(reader: impl BufRead, hooks: &dyn ExecutionHooks) -> Result<ModelTurn, BoxError> {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut calls: Vec<PartialCall> = Vec::new();
    let mut done = false;
    for line in reader.lines() {
        if hooks.is_cancelled() {
            return Err("已撤销，中止流式响应".into());
        }
        let line = line?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data == "[DONE]" {
            done = true;
            break;
        }
        let chunk: Value = serde_json::from_str(data)?;
        if let Some(error) = chunk.get("error") {
            return Err(format!("模型流式响应返回错误: {error}").into());
        }
        report_usage(&chunk, hooks);
        let delta = &chunk["choices"][0]["delta"];
        // 思考模式的思维链不显示，但须保留以便回传。
        if let Some(text) = reasoning_of(delta) {
            reasoning.push_str(text);
        }
        if let Some(text) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            hooks.text_delta(text);
            content.push_str(text);
        }
        for part in delta["tool_calls"].as_array().into_iter().flatten() {
            let index = part["index"]
                .as_u64()
                .map(|i| i as usize)
                .filter(|i| *i <= calls.len())
                .ok_or_else(|| format!("tool_call 的 index 无效: {part}"))?;
            if index == calls.len() {
                calls.push(PartialCall::default());
            }
            let call = &mut calls[index];
            if let Some(id) = part["id"].as_str() {
                call.id = id.to_owned();
            }
            if let Some(name) = part.pointer("/function/name").and_then(Value::as_str) {
                call.name.push_str(name);
            }
            if let Some(args) = part.pointer("/function/arguments").and_then(Value::as_str) {
                call.arguments.push_str(args);
            }
        }
    }
    if !done {
        return Err("流式响应在 [DONE] 之前中断".into());
    }
    let (think, rest) = split_think(&content);
    if let Some(think) = think {
        reasoning.push_str(think);
        content = rest.to_owned();
    }
    if calls.is_empty() {
        return Ok(ModelTurn::Final(content));
    }
    // 调用工具前的话自成一行，不与之后的回奏粘连。
    if !content.is_empty() && !content.ends_with('\n') {
        hooks.text_delta("\n");
    }
    calls
        .into_iter()
        .map(|call| {
            if call.id.is_empty() || call.name.is_empty() {
                return Err(BoxError::from("流式 tool_call 缺少 id 或 name"));
            }
            Ok(ToolRequest {
                id: call.id,
                name: call.name,
                args_json: call.arguments,
            })
        })
        .collect::<Result<_, _>>()
        .map(|calls| ModelTurn::Calls {
            calls,
            text: content,
            reasoning,
        })
}

fn encode_tools(tools: &[ToolSpec]) -> Result<Value, BoxError> {
    tools
        .iter()
        .map(|tool| {
            let parameters: Value = serde_json::from_str(&tool.input_schema_json)?;
            Ok(json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": parameters,
                },
            }))
        })
        .collect()
}

/// 连续的 `ToolCall` 合并为一条带 `tool_calls` 的 assistant 消息。
fn encode_messages(messages: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for message in messages {
        match message {
            Message::System(text) => out.push(json!({"role": "system", "content": text})),
            Message::User(text) => out.push(json!({"role": "user", "content": text})),
            Message::Assistant(text) => out.push(json!({"role": "assistant", "content": text})),
            Message::CallPrelude { text, reasoning } => {
                let mut message = json!({
                    "role": "assistant",
                    "content": if text.is_empty() { Value::Null } else { json!(text) },
                    "tool_calls": [],
                });
                if !reasoning.is_empty() {
                    message["reasoning_content"] = json!(reasoning);
                }
                out.push(message);
            }
            Message::ToolCall {
                id,
                name,
                args_json,
            } => {
                let call = json!({
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": args_json},
                });
                match out.last_mut() {
                    Some(last) if last.get("tool_calls").is_some() => {
                        last["tool_calls"].as_array_mut().expect("数组").push(call);
                    }
                    _ => out
                        .push(json!({"role": "assistant", "content": null, "tool_calls": [call]})),
                }
            }
            Message::ToolOutput { call_id, output } => {
                out.push(json!({"role": "tool", "tool_call_id": call_id, "content": output}))
            }
        }
    }
    out
}

/// 为 OpenAI 兼容的 Anthropic / OpenRouter 请求注入 `cache_control: {"type": "ephemeral"}` 断点：
/// 1) system prompt；2) 工具定义数组末项；3) 多轮对话中倒数第二条之前最近的可缓存文本消息。
///
/// 至多 3 个断点（低于 Anthropic 4 断点上限）。
fn apply_anthropic_cache_control(messages: &mut [Value], tools: Option<&mut Value>) {
    let mark_content_block = |msg: &mut Value| -> bool {
        let Some(text) = msg.get("content").and_then(Value::as_str) else {
            return false;
        };
        if text.is_empty() {
            return false;
        }
        let text = text.to_owned();
        msg["content"] = json!([{
            "type": "text",
            "text": text,
            "cache_control": { "type": "ephemeral" }
        }]);
        true
    };

    // 断点 1：system prompt（最多标首个非空 system，避免多条 system 耗尽断点额度）。
    for msg in messages.iter_mut() {
        if msg["role"] == "system" && mark_content_block(msg) {
            break;
        }
    }

    // 断点 2：工具定义末尾（标最后一件工具即覆盖全部工具 schema）。
    if let Some(tools) = tools
        && let Some(last_tool) = tools.as_array_mut().and_then(|arr| arr.last_mut())
        && let Some(obj) = last_tool.as_object_mut()
    {
        obj.insert("cache_control".into(), json!({ "type": "ephemeral" }));
    }

    // 断点 3：多轮历史边界（跳过末尾最新一条，给前序最近一条非 system 文本消息加缓存点）。
    let non_system: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter_map(|(i, m)| (m["role"] != "system").then_some(i))
        .collect();
    if non_system.len() >= 2 {
        for &idx in non_system[..non_system.len() - 1].iter().rev() {
            if mark_content_block(&mut messages[idx]) {
                break;
            }
        }
    }
}

/// 解析 OpenAI / DeepSeek / Anthropic 兼容端点的 `usage` 字段（流式时位于最后一块）。
fn report_usage(value: &Value, hooks: &dyn ExecutionHooks) {
    let usage = &value["usage"];
    let cache_read = usage["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .or_else(|| usage["prompt_cache_hit_tokens"].as_u64())
        .or_else(|| usage["cache_read_input_tokens"].as_u64())
        .unwrap_or(0);
    let cache_write = usage["cache_creation_input_tokens"]
        .as_u64()
        .or_else(|| usage["prompt_tokens_details"]["cache_write_tokens"].as_u64())
        .unwrap_or(0);

    let (input, output) = match (
        usage["prompt_tokens"].as_u64(),
        usage["completion_tokens"].as_u64(),
    ) {
        (Some(input), Some(output)) => (input, output),
        _ => {
            // 兼容 Anthropic 风格字段名：其 `input_tokens` 不含缓存读写部分，统一折算进总输入。
            let (Some(uncached_input), Some(output)) = (
                usage["input_tokens"].as_u64(),
                usage["output_tokens"].as_u64(),
            ) else {
                return;
            };
            (
                uncached_input
                    .saturating_add(cache_read)
                    .saturating_add(cache_write),
                output,
            )
        }
    };
    hooks.model_usage(TokenUsage::with_cache(
        input,
        output,
        cache_read,
        cache_write,
    ));
}

fn decode_turn(response: &Value) -> Result<ModelTurn, BoxError> {
    let message = &response["choices"][0]["message"];
    if let Some(calls) = message["tool_calls"].as_array().filter(|c| !c.is_empty()) {
        return calls
            .iter()
            .map(|call| {
                let field = |pointer: &str| {
                    call.pointer(pointer)
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| BoxError::from(format!("tool_call 缺少 {pointer}")))
                };
                Ok(ToolRequest {
                    id: field("/id")?,
                    name: field("/function/name")?,
                    args_json: field("/function/arguments")?,
                })
            })
            .collect::<Result<_, BoxError>>()
            .map(|calls| ModelTurn::Calls {
                calls,
                text: message["content"].as_str().unwrap_or_default().to_owned(),
                reasoning: reasoning_of(message).unwrap_or_default().to_owned(),
            });
    }
    match message["content"].as_str() {
        Some(content) => Ok(ModelTurn::Final(split_think(content).1.to_owned())),
        None => Err(format!("无法识别的模型响应: {response}").into()),
    }
}

/// 输出上限的参数名：OpenAI 已弃用 `max_tokens`（推理模型直接拒绝），改用
/// `max_completion_tokens`；DeepSeek、Ollama 等仍只认 `max_tokens`。按服务地址的主机名区分。
fn output_cap_field(base_url: &str) -> &'static str {
    let host = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest)
        .split(['/', ':'])
        .next()
        .unwrap_or_default();
    if host == "api.openai.com" {
        "max_completion_tokens"
    } else {
        "max_tokens"
    }
}

/// 思维链：DeepSeek 为 `reasoning_content`，Ollama 等为 `reasoning`。
fn reasoning_of(message: &Value) -> Option<&str> {
    message["reasoning_content"]
        .as_str()
        .or_else(|| message["reasoning"].as_str())
}

/// 有些服务把思维链以 `<think>…</think>` 写在正文开头：分出（思维链, 正文）。
fn split_think(content: &str) -> (Option<&str>, &str) {
    let trimmed = content.trim_start();
    trimmed
        .strip_prefix("<think>")
        .and_then(|rest| rest.split_once("</think>"))
        .map_or((None, content), |(think, rest)| {
            (Some(think.trim()), rest.trim_start())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use ziyuan_core::{Agent, AgentId, Capability, Department, Edict, Seal, TaskId};
    use ziyuan_runtime::{AgentBackend, Job, WorkerBackend};

    /// 每个请求的 JSON 请求体、Authorization 头与 X-Client-Request-Id 头。
    type Requests = Arc<Mutex<Vec<(Value, String, String)>>>;

    fn mock_server(replies: Vec<(u16, String)>) -> (String, Requests) {
        mock_server_with(
            replies
                .into_iter()
                .map(|(s, r)| (s, r, String::new()))
                .collect(),
        )
    }

    /// 依次返回 `replies`（状态、正文、额外响应头），并记录收到的请求。
    fn mock_server_with(replies: Vec<(u16, String, String)>) -> (String, Requests) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        thread::spawn(move || {
            for (status, reply, extra) in replies {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let (mut length, mut auth, mut trace) = (0, String::new(), String::new());
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let lower = line.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    if lower.starts_with("x-client-request-id:") {
                        trace = line["x-client-request-id:".len()..].trim().to_owned();
                    }
                    if lower.starts_with("authorization:") {
                        auth = line["authorization:".len()..].trim().to_owned();
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                log.lock()
                    .unwrap()
                    .push((serde_json::from_slice(&body).unwrap(), auth, trace));
                let mut stream = reader.into_inner();
                // 以 `data:` 开头的回复按 SSE 流发送，其余按普通 JSON。
                let content_type = if reply.starts_with("data:") {
                    "text/event-stream"
                } else {
                    "application/json"
                };
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .unwrap();
            }
        });
        (url, seen)
    }

    fn tool_call_reply(id: &str, name: &str, args: &str) -> String {
        json!({"choices": [{"message": {"role": "assistant", "content": null, "tool_calls": [
            {"id": id, "type": "function", "function": {"name": name, "arguments": args}}
        ]}}]})
        .to_string()
    }

    fn final_reply(text: &str) -> String {
        json!({"choices": [{"message": {"role": "assistant", "content": text}}]}).to_string()
    }

    #[test]
    fn agent_backend_reads_project_through_api() {
        let root = std::env::temp_dir().join(format!("ziyuan-llm-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("lib.rs"), "unsafe fn danger() {}\n").unwrap();
        let (url, seen) = mock_server(vec![
            (
                200,
                tool_call_reply("c1", "search_code", r#"{"query":"unsafe"}"#),
            ),
            (200, final_reply("lib.rs 第 1 行有 unsafe")),
        ]);
        // 回奏不合体例会发回重写，那次请求打到已关闭的模拟服务上：不重试，照收原奏。
        let no_retry = Retry {
            attempts: 0,
            ..Retry::default()
        };
        let default = OpenAiCompatible::new(url, Some("sk-test".into()), "m").with_retry(no_retry);
        let catalog = Box::new(default.catalog());
        let backend = AgentBackend::new(Box::new(default), root.clone(), 4).with_catalog(catalog);
        let actor = Agent {
            id: AgentId(4),
            name: "陈工".into(),
            speciality: Department::Works,
        };
        let edict = Edict {
            id: TaskId(1),
            department: Department::Works,
            text: "查 unsafe".into(),
            // 只盖 search_code：模型不应看到 read_file。
            seal: Seal {
                capabilities: [Capability::SearchCode].into(),
                scope: vec![".".into()],
                mcp: Default::default(),
            },
        };
        // 官职指定了另一个模型：请求应发给该模型，沿用同一服务地址与 Key。
        let job = Job {
            actor,
            edict,
            model: Some("deepseek-v4-pro".into()),
            budget: None,
            treasury: None,
            urgent: false,
            // 有实录时另给只读的 search_records，不受玉玺工具所限。
            records: std::sync::Arc::new(vec![ziyuan_core::Record {
                task: TaskId(0),
                department: Department::Works,
                edict: "旧诏".into(),
                worker: AgentId(4),
                handler: "陈工".into(),
                summary: "旧奏".into(),
                verdict: "待评定".into(),
            }]),
            workspace_override: None,
            workspace_lease: None,
        };
        assert_eq!(
            backend.execute(&job, &()).unwrap(),
            "lib.rs 第 1 行有 unsafe"
        );
        assert_eq!(
            backend.price("deepseek-v4-pro").unwrap().output_per_million,
            27.0
        );

        let seen = seen.lock().unwrap();
        let (first, auth, _) = &seen[0];
        assert_eq!(auth, "Bearer sk-test");
        assert_eq!(first["model"], "deepseek-v4-pro");
        assert_eq!(first["messages"][0]["role"], "system");
        assert_eq!(first["messages"][1]["content"], "查 unsafe");
        let names: Vec<_> = first["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "search_code",
                "find_symbols",
                "find_references",
                "search_records"
            ]
        );
        let system = first["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("工具：search_code"), "{system}");
        // 第二次请求带回工具调用与真实工具输出。
        let messages = &seen[1].0["messages"];
        assert_eq!(messages[2]["tool_calls"][0]["id"], "c1");
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "c1");
        // 工具输出包在外来文书边界中，暗号与系统提示中的一致。
        let nonce = system
            .split("〔外来文书 ")
            .nth(1)
            .and_then(|rest| rest.split(' ').next())
            .unwrap();
        assert_eq!(
            messages[3]["content"],
            format!(
                "〔外来文书 {nonce} 起〕\nlib.rs:1: unsafe fn danger() {{}}\n〔外来文书 {nonce} 止〕"
            )
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// 记录流式文本；可在收到 `cancel_after` 个片段后报告已撤销。
    #[derive(Default)]
    struct Recorder {
        deltas: std::cell::RefCell<Vec<String>>,
        usage: std::cell::RefCell<Vec<TokenUsage>>,
        traces: std::cell::RefCell<Vec<String>>,
        cancel_after: Option<usize>,
    }
    impl ExecutionHooks for Recorder {
        fn is_cancelled(&self) -> bool {
            self.cancel_after
                .is_some_and(|n| self.deltas.borrow().len() >= n)
        }
        fn text_delta(&self, text: &str) {
            self.deltas.borrow_mut().push(text.into());
        }
        fn model_usage(&self, usage: TokenUsage) {
            self.usage.borrow_mut().push(usage);
        }
        fn model_trace(&self, trace: &str) {
            self.traces.borrow_mut().push(trace.into());
        }
    }

    fn sse(chunks: &[Value], done: bool) -> String {
        let mut out: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
        if done {
            out.push_str("data: [DONE]\n\n");
        }
        out
    }

    fn delta(delta: Value) -> Value {
        json!({"choices": [{"index": 0, "delta": delta}]})
    }

    /// 按 OpenAI Chat Completions 文档的流式格式回放（未经真实账号实测）：首块 `content: null`、
    /// 工具调用首块 `arguments` 为空串、`finish_reason: "tool_calls"`、末块 `choices` 为空只带 usage。
    #[test]
    fn openai_shaped_stream_is_understood() {
        let chunk = |choices: Value| {
            json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1,
                   "model": "gpt-5-mini", "system_fingerprint": null, "choices": choices})
        };
        let reply = sse(
            &[
                chunk(
                    json!([{"index": 0, "delta": {"role": "assistant", "content": null,
                    "tool_calls": [{"index": 0, "id": "call_1", "type": "function",
                    "function": {"name": "read_file", "arguments": ""}}], "refusal": null},
                    "logprobs": null, "finish_reason": null}]),
                ),
                chunk(json!([{"index": 0, "delta": {"tool_calls": [{"index": 0,
                    "function": {"arguments": "{\"path\":"}}]}, "finish_reason": null}])),
                chunk(json!([{"index": 0, "delta": {"tool_calls": [{"index": 0,
                    "function": {"arguments": "\"src/lib.rs\"}"}}]}, "finish_reason": null}])),
                chunk(json!([{"index": 0, "delta": {}, "finish_reason": "tool_calls"}])),
                {
                    let mut last = chunk(json!([]));
                    last["usage"] = json!({"prompt_tokens": 120, "completion_tokens": 40,
                        "total_tokens": 160, "prompt_tokens_details": {"cached_tokens": 0},
                        "completion_tokens_details": {"reasoning_tokens": 32}});
                    last
                },
            ],
            true,
        );
        let (url, seen) = mock_server(vec![(200, reply)]);
        let hooks = Recorder::default();
        let turn = OpenAiCompatible::new(url, Some("k".into()), "gpt-5-mini")
            .next_turn(&[Message::User("q".into())], &[], &hooks)
            .unwrap();
        let ModelTurn::Calls { calls, text, .. } = turn else {
            panic!("应为工具调用: {turn:?}");
        };
        assert_eq!((calls.len(), calls[0].id.as_str()), (1, "call_1"));
        assert_eq!(calls[0].args_json, r#"{"path":"src/lib.rs"}"#);
        assert!(text.is_empty());
        // 用量之前先报 trace ID，与请求头一致（事件簿据此把 trace 记在用量上）。
        assert_eq!(*hooks.traces.borrow(), [seen.lock().unwrap()[0].2.clone()]);
        // 推理 token 已含在 completion_tokens 内，不重复计。
        assert_eq!(
            *hooks.usage.borrow(),
            [TokenUsage {
                input: 120,
                output: 40,
                cache_read: 0,
                cache_write: 0,
            }]
        );
    }

    #[test]
    fn ollama_reasoning_and_inline_think_stay_out_of_the_answer() {
        // Ollama 的思维链在 `reasoning`；另一些服务把 `<think>` 写在正文开头。
        let ollama = sse(
            &[
                delta(json!({"role": "assistant", "content": "", "reasoning": "想"})),
                delta(json!({"content": "", "reasoning": "一想"})),
                delta(json!({"content": "工具后"})),
                delta(
                    json!({"tool_calls": [{"index": 0, "id": "a", "type": "function",
                    "function": {"name": "read_file", "arguments": "{}"}}]}),
                ),
            ],
            true,
        );
        let inline = sse(
            &[delta(
                json!({"content": "<think>\n先想\n</think>\n\n7\n理由"}),
            )],
            true,
        );
        let (url, _) = mock_server(vec![(200, ollama), (200, inline)]);
        let model = OpenAiCompatible::new(url, None, "m");
        let ask = [Message::User("q".into())];
        let turn = model.next_turn(&ask, &[], &Recorder::default()).unwrap();
        assert!(
            matches!(&turn, ModelTurn::Calls { reasoning, text, .. } if reasoning == "想一想" && text == "工具后"),
            "{turn:?}"
        );
        let turn = model.next_turn(&ask, &[], &Recorder::default()).unwrap();
        assert!(
            matches!(&turn, ModelTurn::Final(t) if t == "7\n理由"),
            "{turn:?}"
        );
        assert_eq!(
            split_think("答：<think>不是开头</think>"),
            (None, "答：<think>不是开头</think>")
        );
    }

    #[test]
    fn streamed_tool_calls_are_assembled_across_chunks() {
        let reply = sse(
            &[
                delta(json!({"role": "assistant", "reasoning_content": "想一想"})),
                delta(json!({"content": "先查"})),
                delta(
                    json!({"tool_calls": [{"index": 0, "id": "a", "type": "function",
                    "function": {"name": "read_file", "arguments": "{\"pa"}}]}),
                ),
                delta(
                    json!({"tool_calls": [{"index": 0, "function": {"arguments": "th\":\"x\"}"}}]}),
                ),
                delta(
                    json!({"tool_calls": [{"index": 1, "id": "b", "type": "function",
                    "function": {"name": "search_code", "arguments": "{}"}}]}),
                ),
            ],
            true,
        );
        let (url, seen) = mock_server(vec![(200, reply)]);
        let hooks = Recorder::default();
        let turn = OpenAiCompatible::new(url, None, "m")
            .next_turn(&[Message::User("q".into())], &[], &hooks)
            .unwrap();
        let ModelTurn::Calls {
            calls,
            text,
            reasoning,
        } = turn
        else {
            panic!("应为工具调用: {turn:?}");
        };
        assert_eq!((text.as_str(), reasoning.as_str()), ("先查", "想一想"));
        // 思维链与调用前的话随下一次请求回传（DeepSeek 思考模式要求），且与工具调用同在一条消息。
        let mut history = vec![Message::CallPrelude { text, reasoning }];
        history.extend(calls.iter().map(|c| Message::ToolCall {
            id: c.id.clone(),
            name: c.name.clone(),
            args_json: c.args_json.clone(),
        }));
        let encoded = encode_messages(&history);
        assert_eq!(encoded.len(), 1);
        assert_eq!(encoded[0]["reasoning_content"], "想一想");
        assert_eq!(encoded[0]["content"], "先查");
        assert_eq!(encoded[0]["tool_calls"].as_array().unwrap().len(), 2);
        let calls: Vec<_> = calls
            .iter()
            .map(|c| (c.id.as_str(), c.name.as_str(), c.args_json.as_str()))
            .collect();
        assert_eq!(
            calls,
            [
                ("a", "read_file", r#"{"path":"x"}"#),
                ("b", "search_code", "{}")
            ]
        );
        assert_eq!(
            *hooks.deltas.borrow(),
            ["先查", "\n"],
            "思维链不显示；调用前的话以换行结束"
        );
        assert_eq!(seen.lock().unwrap()[0].0["stream"], true);
    }

    #[test]
    fn streamed_text_is_reported_as_it_arrives() {
        let mut chunks: Vec<_> = ["紫", "垣", "回奏"]
            .iter()
            .map(|t| delta(json!({"content": t})))
            .collect();
        chunks.push(json!({"choices": [], "usage": {"prompt_tokens": 321, "completion_tokens": 9, "total_tokens": 330}}));
        let (url, _) = mock_server(vec![(200, sse(&chunks, true))]);
        let hooks = Recorder::default();
        let turn = OpenAiCompatible::new(url, None, "m")
            .next_turn(&[Message::User("q".into())], &[], &hooks)
            .unwrap();
        assert!(
            matches!(turn, ModelTurn::Final(ref t) if t == "紫垣回奏"),
            "{turn:?}"
        );
        assert_eq!(*hooks.deltas.borrow(), ["紫", "垣", "回奏"]);
        assert_eq!(
            *hooks.usage.borrow(),
            [TokenUsage {
                input: 321,
                output: 9,
                cache_read: 0,
                cache_write: 0,
            }]
        );
    }

    #[test]
    fn stream_stops_when_cancelled_or_cut_off() {
        let chunks: Vec<_> = ["一", "二", "三"]
            .iter()
            .map(|t| delta(json!({"content": t})))
            .collect();
        let (url, _) = mock_server(vec![(200, sse(&chunks, true)), (200, sse(&chunks, false))]);
        let model = OpenAiCompatible::new(url, None, "m");
        let messages = [Message::User("q".into())];

        let hooks = Recorder {
            cancel_after: Some(1),
            ..Recorder::default()
        };
        let err = model.next_turn(&messages, &[], &hooks).unwrap_err();
        assert!(err.to_string().contains("撤销"), "{err}");
        assert_eq!(*hooks.deltas.borrow(), ["一"], "撤销后不应再读取");

        let err = model.next_turn(&messages, &[], &()).unwrap_err();
        assert!(err.to_string().contains("[DONE]"), "{err}");
    }

    #[test]
    fn prices_parse_and_estimate_cost() {
        let price = parse_price("2, 8").unwrap();
        let usage = TokenUsage {
            input: 1_000_000,
            output: 500_000,
            cache_read: 0,
            cache_write: 0,
        };
        assert_eq!(price.cost(usage), 6.0);
        assert_eq!(price.describe(usage), "约 6.0000 元");
        assert_eq!(parse_price("0.5,1.5,USD").unwrap().currency, "USD");
        assert!(parse_price("免费").is_none() && parse_price("1").is_none());
        for invalid in ["NaN,1", "1,inf", "-1,2", "2,-0.1"] {
            assert!(parse_price(invalid).is_none(), "{invalid}");
        }
        let (price, source) = price_quote("deepseek-v4-pro").unwrap();
        assert_eq!(price.input_per_million, 9.0);
        assert_eq!(price.output_per_million, 27.0);
        assert!(source.contains("2026-10"));
    }

    #[test]
    fn http_error_is_reported_with_status() {
        let (url, _) = mock_server(vec![(401, r#"{"error":"bad key"}"#.into())]);
        let model = OpenAiCompatible::new(url, None, "m");
        let err = model
            .next_turn(&[Message::User("hi".into())], &[], &())
            .unwrap_err()
            .to_string();
        assert!(err.contains("401") && err.contains("bad key"), "{err}");
    }

    fn quick(url: String, attempts: u32) -> OpenAiCompatible {
        OpenAiCompatible::new(url, None, "m").with_retry(Retry {
            attempts,
            base: Duration::from_millis(10),
            max: Duration::from_millis(200),
        })
    }

    #[test]
    fn transient_failures_are_retried_with_the_same_trace_id() {
        let (url, seen) = mock_server_with(vec![
            (503, "busy".into(), String::new()),
            (429, "slow down".into(), "Retry-After: 0\r\n".into()),
            (200, final_reply("好"), String::new()),
        ]);
        let turn = quick(url, 3).next_turn(&[Message::User("hi".into())], &[], &());
        assert!(matches!(turn, Ok(ModelTurn::Final(t)) if t == "好"));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert!(
            seen[0].2.starts_with("zy-") && seen[0].2.len() == 15,
            "{}",
            seen[0].2
        );
        assert!(seen.iter().all(|r| r.2 == seen[0].2), "重试沿用同一 trace");
    }

    #[test]
    fn retries_give_up_with_trace_and_server_request_id() {
        let busy = || {
            (
                502,
                "bad gateway".to_owned(),
                "X-Request-Id: req-42\r\n".to_owned(),
            )
        };
        let (url, seen) = mock_server_with(vec![busy(), busy()]);
        let err = quick(url, 1)
            .next_turn(&[Message::User("hi".into())], &[], &())
            .unwrap_err()
            .to_string();
        let trace = seen.lock().unwrap()[1].2.clone();
        assert!(
            err.contains("502") && err.contains(&format!("trace {trace}")),
            "{err}"
        );
        assert!(
            err.contains("服务端 req-42") && err.contains("已重试 1 次"),
            "{err}"
        );
        // 4xx（429 除外）是请求本身的问题，不重试。
        let (url, seen) = mock_server(vec![(400, "bad request".into())]);
        assert!(
            quick(url, 3)
                .next_turn(&[Message::User("hi".into())], &[], &())
                .is_err()
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    struct Cap(u64);
    impl ExecutionHooks for Cap {
        fn max_output(&self) -> Option<u64> {
            Some(self.0)
        }
    }

    #[test]
    fn low_allowance_caps_max_tokens() {
        let messages = [Message::User("hi".into())];
        let (url, seen) = mock_server(vec![(200, final_reply("a")), (200, final_reply("b"))]);
        let model = quick(url, 0);
        model.next_turn(&messages, &[], &Cap(300)).unwrap();
        model.next_turn(&messages, &[], &Cap(1_000_000)).unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0["max_tokens"], 300);
        assert!(seen[1].0.get("max_tokens").is_none(), "余额充裕时不传");
        assert_eq!(
            output_cap_field("https://api.openai.com/v1"),
            "max_completion_tokens"
        );
        for other in [
            "https://api.deepseek.com/v1",
            "http://127.0.0.1:11434/v1",
            "https://x.dev/api.openai.com/v1",
        ] {
            assert_eq!(output_cap_field(other), "max_tokens", "{other}");
        }
    }

    #[test]
    fn parallel_tool_calls_share_one_assistant_message() {
        let messages = [
            Message::User("q".into()),
            Message::ToolCall {
                id: "a".into(),
                name: "read_file".into(),
                args_json: "{}".into(),
            },
            Message::ToolCall {
                id: "b".into(),
                name: "search_code".into(),
                args_json: "{}".into(),
            },
            Message::ToolOutput {
                call_id: "a".into(),
                output: "1".into(),
            },
            Message::ToolOutput {
                call_id: "b".into(),
                output: "2".into(),
            },
        ];
        let encoded = encode_messages(&messages);
        assert_eq!(encoded.len(), 4);
        assert_eq!(encoded[1]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(encoded[3]["tool_call_id"], "b");
    }

    #[test]
    fn prompt_cache_injects_anthropic_breakpoints_only_when_applicable() {
        let messages = vec![
            Message::System("你是工部官员".into()),
            Message::User("读取 src/lib.rs".into()),
            Message::ToolCall {
                id: "c1".into(),
                name: "read_file".into(),
                args_json: r#"{"path":"src/lib.rs"}"#.into(),
            },
            Message::ToolOutput {
                call_id: "c1".into(),
                output: "fn main() {}".into(),
            },
            Message::User("再检查一下测试".into()),
        ];
        let tools = vec![
            ToolSpec {
                name: "read_file".into(),
                description: "读文件".into(),
                input_schema_json: r#"{"type":"object"}"#.into(),
            },
            ToolSpec {
                name: "write_file".into(),
                description: "写文件".into(),
                input_schema_json: r#"{"type":"object"}"#.into(),
            },
        ];

        // 1. Auto + Claude 模型：自动加 system / 末个 tool / 历史边界三处 cache_control。
        let (url, seen) = mock_server(vec![(200, final_reply("ok"))]);
        OpenAiCompatible::new(url, Some("k".into()), "anthropic/claude-sonnet-4.5")
            .with_retry(Retry {
                attempts: 0,
                ..Retry::default()
            })
            .next_turn(&messages, &tools, &())
            .unwrap();
        let body = &seen.lock().unwrap()[0].0;
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(
            body["tools"][1]["cache_control"],
            json!({"type": "ephemeral"})
        );
        // 历史断点打在倒数第二条（tool output）上，最后一条新用户输入保持纯文本。
        assert_eq!(
            body["messages"][3]["content"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert_eq!(body["messages"][4]["content"], "再检查一下测试");

        // 2. Auto + DeepSeek / OpenAI：保持原始纯文本格式，不注入 cache_control。
        let (url, seen) = mock_server(vec![(200, final_reply("ok"))]);
        OpenAiCompatible::new(url, Some("k".into()), "deepseek-v4-pro")
            .with_retry(Retry {
                attempts: 0,
                ..Retry::default()
            })
            .next_turn(&messages, &tools, &())
            .unwrap();
        let body = &seen.lock().unwrap()[0].0;
        assert_eq!(body["messages"][0]["content"], "你是工部官员");
        assert!(body["tools"][1].get("cache_control").is_none());

        // 3. Off + Claude：显式关闭时不注入。
        let (url, seen) = mock_server(vec![(200, final_reply("ok"))]);
        OpenAiCompatible::new(url, Some("k".into()), "claude-opus-4")
            .with_prompt_cache(PromptCache::Off)
            .with_retry(Retry {
                attempts: 0,
                ..Retry::default()
            })
            .next_turn(&messages, &tools, &())
            .unwrap();
        let body = &seen.lock().unwrap()[0].0;
        assert_eq!(body["messages"][0]["content"], "你是工部官员");
        assert!(body["tools"][1].get("cache_control").is_none());

        // 4. Anthropic 强制模式：自定义模型名也注入。
        let (url, seen) = mock_server(vec![(200, final_reply("ok"))]);
        OpenAiCompatible::new(url, Some("k".into()), "my-proxy-model")
            .with_prompt_cache(PromptCache::Anthropic)
            .with_retry(Retry {
                attempts: 0,
                ..Retry::default()
            })
            .next_turn(&messages, &tools, &())
            .unwrap();
        let body = &seen.lock().unwrap()[0].0;
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert_eq!(PromptCache::parse("auto").unwrap(), PromptCache::Auto);
        assert_eq!(PromptCache::parse("on").unwrap(), PromptCache::Anthropic);
        assert_eq!(PromptCache::parse("off").unwrap(), PromptCache::Off);
        assert!(PromptCache::parse("bogus").is_err());
    }

    #[test]
    fn report_usage_parses_openai_deepseek_and_anthropic_cache_fields() {
        // OpenAI / OpenRouter: prompt_tokens_details.cached_tokens + cache_write_tokens
        let hooks = Recorder::default();
        report_usage(
            &json!({
                "usage": {
                    "prompt_tokens": 1200,
                    "completion_tokens": 80,
                    "prompt_tokens_details": {
                        "cached_tokens": 900,
                        "cache_write_tokens": 200
                    }
                }
            }),
            &hooks,
        );
        // DeepSeek: prompt_cache_hit_tokens
        report_usage(
            &json!({
                "usage": {
                    "prompt_tokens": 500,
                    "completion_tokens": 40,
                    "prompt_cache_hit_tokens": 350,
                    "prompt_cache_miss_tokens": 150
                }
            }),
            &hooks,
        );
        // Anthropic 风格：input_tokens 不含 cache_read/cache_creation，折算入总 input
        report_usage(
            &json!({
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "cache_read_input_tokens": 800,
                    "cache_creation_input_tokens": 300
                }
            }),
            &hooks,
        );
        assert_eq!(
            *hooks.usage.borrow(),
            [
                TokenUsage::with_cache(1200, 80, 900, 200),
                TokenUsage::with_cache(500, 40, 350, 0),
                TokenUsage::with_cache(1200, 50, 800, 300),
            ]
        );
    }

    type RawRequests = Arc<Mutex<Vec<(Value, String)>>>;

    fn mock_http(replies: Vec<(u16, &'static str, &'static str, String)>) -> (String, RawRequests) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        thread::spawn(move || {
            for (status, content_type, extra, reply) in replies {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut length = 0usize;
                let mut raw_headers = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let lower = line.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    raw_headers.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                log.lock().unwrap().push((
                    serde_json::from_slice(&body).unwrap(),
                    raw_headers.to_ascii_lowercase(),
                ));
                let mut stream = reader.into_inner();
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .unwrap();
            }
        });
        (url, seen)
    }

    fn expect_final(turn: ModelTurn) -> String {
        match turn {
            ModelTurn::Final(text) => text,
            other => panic!("expected ModelTurn::Final, got {other:?}"),
        }
    }

    #[test]
    fn anthropic_native_streams_tools_thinking_and_cache_usage() {
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":120,\"cache_creation_input_tokens\":300,\"cache_read_input_tokens\":800,\"output_tokens\":1}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"先看源码\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"我来查一下\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"src/lib.rs\\\"}\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":45}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let (url, seen) = mock_http(vec![
            (
                529,
                "application/json",
                "",
                "{\"error\":\"overloaded\"}".into(),
            ),
            (200, "text/event-stream", "", sse.into()),
        ]);
        let messages = vec![
            Message::System("你是工部尚书".into()),
            Message::User("检查 lib.rs".into()),
            Message::CallPrelude {
                text: "先列出文件".into(),
                reasoning: "内部思维链不回传".into(),
            },
            Message::ToolCall {
                id: "toolu_0".into(),
                name: "list_files".into(),
                args_json: "{}".into(),
            },
            Message::ToolOutput {
                call_id: "toolu_0".into(),
                output: "src/lib.rs".into(),
            },
            Message::User("请继续读取".into()),
        ];
        let tools = vec![ToolSpec {
            name: "read_file".into(),
            description: "读取文件".into(),
            input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#
                .into(),
        }];
        let hooks = Recorder::default();
        let provider = AnthropicMessages::new(url, Some("sk-ant-test".into()), "claude-sonnet-4-5")
            .with_retry(Retry {
                attempts: 1,
                base: Duration::from_millis(1),
                max: Duration::from_millis(5),
            });
        let turn = provider.next_turn(&messages, &tools, &hooks).unwrap();
        match turn {
            ModelTurn::Calls {
                calls,
                text,
                reasoning,
            } => {
                assert_eq!(text, "我来查一下");
                assert_eq!(reasoning, "先看源码");
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id, "toolu_1");
                assert_eq!(calls[0].name, "read_file");
                assert_eq!(calls[0].args_json, r#"{"path":"src/lib.rs"}"#);
            }
            other => panic!("expected tool calls, got {other:?}"),
        }
        assert_eq!(
            *hooks.usage.borrow(),
            [TokenUsage::with_cache(1220, 45, 800, 300)]
        );
        assert_eq!(hooks.deltas.borrow().concat(), "我来查一下\n");

        let recorded = seen.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        let (body, headers) = &recorded[1];
        assert!(headers.contains("x-api-key: sk-ant-test"));
        assert!(headers.contains("anthropic-version: 2023-06-01"));
        assert_eq!(body["model"], "claude-sonnet-4-5");
        assert_eq!(
            body["system"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert_eq!(
            body["tools"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
        // 连续 user 消息（ToolOutput + User）合并为同一条 user 消息，且倒数第二条消息有缓存断点。
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"][1]["type"], "tool_use");
        assert_eq!(
            msgs[1]["content"][1]["cache_control"],
            json!({"type": "ephemeral"})
        );
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
        assert_eq!(msgs[2]["content"][1]["type"], "text");
    }

    #[test]
    fn gemini_native_streams_tools_thought_and_cached_usage() {
        let sse = concat!(
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"先分析结构\",\"thought\":true},{\"text\":\"开始查阅\"}]}}]}\n\n",
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"name\":\"search_code\",\"args\":{\"query\":\"main\"}}}]}}],\"usageMetadata\":{\"promptTokenCount\":900,\"candidatesTokenCount\":35,\"cachedContentTokenCount\":600}}\n\n"
        );
        let (url, seen) = mock_http(vec![(200, "text/event-stream", "", sse.into())]);
        let messages = vec![
            Message::System("你是都察院御史".into()),
            Message::User("查 main".into()),
            Message::ToolCall {
                id: "gemini-call-0".into(),
                name: "list_files".into(),
                args_json: "{}".into(),
            },
            Message::ToolOutput {
                call_id: "gemini-call-0".into(),
                output: "src/main.rs".into(),
            },
        ];
        let tools = vec![ToolSpec {
            name: "search_code".into(),
            description: "搜索代码".into(),
            input_schema_json: r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{"query":{"type":"string"}}}"#.into(),
        }];
        let hooks = Recorder::default();
        let provider = GeminiNative::new(url, Some("gem-secret-key".into()), "gemini-2.5-pro")
            .with_retry(Retry {
                attempts: 0,
                ..Retry::default()
            });
        let turn = provider.next_turn(&messages, &tools, &hooks).unwrap();
        match turn {
            ModelTurn::Calls {
                calls,
                text,
                reasoning,
            } => {
                assert_eq!(text, "开始查阅");
                assert_eq!(reasoning, "先分析结构");
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id, "gemini-call-0");
                assert_eq!(calls[0].name, "search_code");
                assert_eq!(calls[0].args_json, r#"{"query":"main"}"#);
            }
            other => panic!("expected tool calls, got {other:?}"),
        }
        assert_eq!(
            *hooks.usage.borrow(),
            [TokenUsage::with_cache(900, 35, 600, 0)]
        );

        let recorded = seen.lock().unwrap();
        let (body, headers) = &recorded[0];
        assert!(headers.contains("x-goog-api-key: gem-secret-key"));
        assert!(!headers.contains("key=gem-secret-key"));
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "你是都察院御史"
        );
        assert!(
            body["tools"][0]["functionDeclarations"][0]["parameters"]
                .get("$schema")
                .is_none()
        );
        assert!(
            body["tools"][0]["functionDeclarations"][0]["parameters"]
                .get("additionalProperties")
                .is_none()
        );
        // ToolOutput 对应回传的 functionResponse.name 还原为 list_files。
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["name"],
            "list_files"
        );
    }

    #[test]
    fn models_config_forbids_plaintext_secrets_and_validates_fields() {
        let bad_secret = r#"{
            "profiles": {
                "claude": {
                    "provider": "anthropic",
                    "model": "claude-sonnet-4-5",
                    "api_key": "sk-ant-never-put-here"
                }
            }
        }"#;
        let err = ModelsConfig::parse(bad_secret).unwrap_err().to_string();
        assert!(err.contains("禁止写入明文密钥"), "{err}");

        let bad_default = r#"{
            "default": "missing",
            "profiles": {
                "local": { "model": "qwen3:32b", "base_url": "http://localhost:11434/v1" }
            }
        }"#;
        assert!(ModelsConfig::parse(bad_default).is_err());

        let valid = r#"{
            "default": "deepseek",
            "profiles": {
                "deepseek": {
                    "provider": "openai",
                    "base_url": "https://api.deepseek.com/v1",
                    "model": "deepseek-v4-pro",
                    "api_key_env": "DEEPSEEK_API_KEY"
                },
                "claude-sonnet": {
                    "provider": "anthropic",
                    "model": "claude-sonnet-4-5",
                    "api_key_env": "ANTHROPIC_API_KEY",
                    "prompt_cache": "anthropic",
                    "price": { "input": 3.0, "output": 15.0, "currency": "USD" }
                },
                "gemini-pro": {
                    "provider": "gemini",
                    "model": "gemini-2.5-pro",
                    "api_key_env": "GEMINI_API_KEY"
                }
            }
        }"#;
        let cfg = ModelsConfig::parse(valid).unwrap();
        assert_eq!(cfg.default_profile.as_deref(), Some("deepseek"));
        assert_eq!(cfg.profiles.len(), 3);
        assert_eq!(
            cfg.profiles["claude-sonnet"].provider,
            ProviderKind::Anthropic
        );
        assert_eq!(cfg.profiles["gemini-pro"].provider, ProviderKind::Gemini);
    }

    #[test]
    fn heterogeneous_catalog_routes_openai_anthropic_and_gemini_and_fails_missing_key_per_profile()
    {
        let (openai_url, _) = mock_server(vec![(200, final_reply("来自 OpenAI 兼容"))]);
        let (anthropic_url, _) = mock_http(vec![(
            200,
            "application/json",
            "",
            json!({
                "content": [{"type": "text", "text": "来自 Anthropic 原生"}],
                "usage": {"input_tokens": 10, "output_tokens": 5}
            })
            .to_string(),
        )]);
        let (gemini_url, _) = mock_http(vec![(
            200,
            "application/json",
            "",
            json!({
                "candidates": [{"content": {"role": "model", "parts": [{"text": "来自 Gemini 原生"}]}}],
                "usageMetadata": {"promptTokenCount": 12, "candidatesTokenCount": 6}
            })
            .to_string(),
        )]);

        let cfg = ModelsConfig::parse(
            &json!({
                "default": "oai",
                "profiles": {
                    "oai": {
                        "provider": "openai",
                        "base_url": openai_url,
                        "model": "deepseek-v4-pro"
                    },
                    "ant": {
                        "provider": "anthropic",
                        "base_url": anthropic_url,
                        "model": "claude-sonnet-4-5",
                        "price": {"input": 3.0, "output": 15.0, "currency": "USD"}
                    },
                    "gem": {
                        "provider": "gemini",
                        "base_url": gemini_url,
                        "model": "gemini-2.5-pro"
                    },
                    "needs-key": {
                        "provider": "anthropic",
                        "base_url": "http://127.0.0.1:9/v1",
                        "model": "claude-opus-4",
                        "api_key_env": "ZIYUAN_TEST_DEFINITELY_UNSET_KEY_99999"
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let catalog = OpenAiCompatible::new("http://127.0.0.1:9/v1", None, "fallback")
            .with_retry(Retry {
                attempts: 0,
                ..Retry::default()
            })
            .catalog()
            .with_profiles(cfg.profiles);

        let msgs = [Message::User("请回奏".into())];
        assert_eq!(
            expect_final(catalog.provider("oai").next_turn(&msgs, &[], &()).unwrap()),
            "来自 OpenAI 兼容"
        );
        assert_eq!(
            expect_final(catalog.provider("ant").next_turn(&msgs, &[], &()).unwrap()),
            "来自 Anthropic 原生"
        );
        assert_eq!(
            expect_final(catalog.provider("gem").next_turn(&msgs, &[], &()).unwrap()),
            "来自 Gemini 原生"
        );
        // 缺少环境变量仅在调用该配置时报错，不影响其余配置。
        let err = catalog
            .provider("needs-key")
            .next_turn(&msgs, &[], &())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("ZIYUAN_TEST_DEFINITELY_UNSET_KEY_99999"),
            "{err}"
        );

        assert_eq!(catalog.price("oai").unwrap().currency, "元");
        assert_eq!(catalog.price("ant").unwrap().currency, "USD");
        assert_eq!(catalog.price("gem").unwrap().currency, "USD");
        assert_eq!(
            price_for("anthropic/claude-sonnet-4-5")
                .unwrap()
                .input_per_million,
            3.0
        );
        assert_eq!(price_for("gpt-5").unwrap().currency, "USD");
    }
}
