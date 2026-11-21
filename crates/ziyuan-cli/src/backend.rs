//! 模型后端的选择、异构多模型目录与 ChatGPT 登录。

use std::env;
use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use ziyuan_core::Price;
use ziyuan_llm::{ModelsConfig, ResolvedBackend, price_for_in_workspace, resolve_backend};
use ziyuan_runtime::{AgentBackend, DemoBackend, Limits, ModelCatalog, WorkerBackend};

/// 单件诏令默认最多的模型调用步数（`ZIYUAN_MAX_STEPS` 可改）。
pub(crate) const MAX_AGENT_STEPS: usize = 12;
pub(crate) const DEFAULT_LIMITS: Limits = Limits {
    max_concurrent: 4,
    timeout: Duration::from_secs(300),
};

/// 登录 ChatGPT 订阅（凭据存于用户配置目录，不在项目里）。
pub(crate) fn login(models_only: bool) -> Result<(), ziyuan_runtime::BoxError> {
    use ziyuan_llm::chatgpt::{API_BASE, Account, USAGE_URL};
    let account = Account::from_env()?;
    if !models_only {
        let (creds, first) = account.login(|url| {
            println!("请在浏览器中登录 ChatGPT 并授权紫垣：\n{url}");
            open_browser(url);
        })?;
        println!("已登录 ChatGPT：{}", creds.email);
        if first {
            println!("已用 ChatGPT 订阅：紫垣的模型调用计入你的订阅额度。管理用量：{USAGE_URL}");
        }
    }
    let base = env::var("ZIYUAN_LLM_BASE_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| API_BASE.into());
    println!("可选模型（设 ZIYUAN_LLM_PROVIDER=chatgpt 与 ZIYUAN_LLM_MODEL=<模型>）：");
    for (slug, name) in account.models(&base)? {
        println!("  {slug:<24}{name}");
    }
    Ok(())
}

pub(crate) fn logout() -> Result<(), ziyuan_runtime::BoxError> {
    if ziyuan_llm::chatgpt::Account::from_env()?.logout()? {
        println!("已退出 ChatGPT 订阅登录。");
    } else {
        println!("已清空本机令牌；授权服务未确认撤销，可在 ChatGPT 设置中断开紫垣。");
    }
    Ok(())
}

/// 尽力打开浏览器；失败时用户照打印的地址自行打开。
pub(crate) fn open_browser(url: &str) {
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    let _ = command
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// 尝试解析环境变量或 `.ziyuan/models.json` 所配的模型；配置有误时视为未配，执行时再报错。
pub(crate) fn resolved_backend() -> Option<ResolvedBackend> {
    resolve_backend(env::current_dir().ok().as_deref())
        .ok()
        .flatten()
}

pub(crate) fn model_label() -> String {
    resolved_backend().map_or("演示（未接模型）".into(), |m| m.model_label)
}

/// 按当前工作区（含 `.ziyuan/models.json` 自定义单价）查询某模型／配置名的单价；
/// 使用 ChatGPT 订阅额度时不按 token 计价，返回 `None`。
pub(crate) fn workspace_price_for(model: &str) -> Option<Price> {
    if let Some(resolved) = resolved_backend() {
        return resolved.catalog.price(model);
    }
    let cwd = env::current_dir().ok();
    price_for_in_workspace(cwd.as_deref(), model)
}

/// 供 Web 界面只读展示的 `.ziyuan/models.json` 摘要（绝不含明文 API Key）。
pub(crate) fn web_model_profiles(root: &Path) -> Vec<ziyuan_web::ModelProfileInfo> {
    ModelsConfig::load(root)
        .ok()
        .flatten()
        .map(|cfg| {
            cfg.summaries()
                .into_iter()
                .map(|s| ziyuan_web::ModelProfileInfo {
                    name: s.name,
                    provider: s.provider,
                    provider_label: s.provider_label,
                    model: s.model,
                    base_url: s.base_url,
                    api_key_env: s.api_key_env,
                    api_key_ready: s.api_key_ready,
                    is_default: s.is_default,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 解析真实模型与异构路由目录（环境变量优先，其次 `.ziyuan/models.json` 的 `default`）；
/// 两者均未配置时使用演示后端。
pub(crate) fn worker_backend() -> Result<Arc<dyn WorkerBackend>, Box<dyn Error>> {
    let cwd = env::current_dir()?;
    let Some(resolved) = resolve_backend(Some(&cwd)).map_err(|e| e as Box<dyn Error>)? else {
        return Ok(Arc::new(DemoBackend));
    };
    let plan = if resolved.uses_chatgpt_plan {
        "，计入 ChatGPT 订阅额度"
    } else {
        ""
    };
    eprintln!(
        "执行后端：{} [{}]{plan}（工具与查阅范围由各诏令的玉玺决定）",
        resolved.model_label,
        resolved.provider_kind.label(),
    );
    let catalog = Box::new(resolved.catalog);
    let steps = match env::var("ZIYUAN_MAX_STEPS") {
        Ok(n) => n
            .parse()
            .ok()
            .filter(|&n| n > 0)
            .ok_or("ZIYUAN_MAX_STEPS 须为正整数")?,
        Err(_) => MAX_AGENT_STEPS,
    };
    let context_tokens = match env::var("ZIYUAN_CONTEXT_TOKENS") {
        Ok(n) => n
            .parse::<u64>()
            .ok()
            .filter(|&n| n > 0)
            .ok_or("ZIYUAN_CONTEXT_TOKENS 须为正整数（旧工具输出的节略阈值）")?,
        Err(env::VarError::NotPresent) => ziyuan_runtime::agent_loop::CONTEXT_TOKENS,
        Err(_) => return Err("ZIYUAN_CONTEXT_TOKENS 须为有效 Unicode 正整数".into()),
    };
    eprintln!("上下文节略阈值：{context_tokens} tokens（不是模型窗口上限）");
    let test_presets = match env::var("ZIYUAN_TEST_COMMANDS") {
        Ok(raw) => ziyuan_runtime::tools::parse_test_presets(&raw).map_err(|e| e.to_string())?,
        Err(env::VarError::NotPresent) => Default::default(),
        Err(_) => return Err("ZIYUAN_TEST_COMMANDS 须为有效 Unicode 字符串".into()),
    };
    Ok(Arc::new(
        AgentBackend::new(resolved.provider, cwd, steps)
            .with_context_tokens(context_tokens)
            .with_catalog(catalog)
            .with_test_command(env::var("ZIYUAN_TEST_COMMAND").ok())
            .with_test_presets(test_presets),
    ))
}
