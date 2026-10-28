//! 模型后端的选择与 ChatGPT 登录。

use std::env;
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;
use ziyuan_llm::OpenAiCompatible;
use ziyuan_runtime::{AgentBackend, DemoBackend, Limits, WorkerBackend};

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

/// 环境变量所配的模型；配置有误（如选了订阅却未登录）时视为未配，执行时再报错。
pub(crate) fn env_model() -> Option<OpenAiCompatible> {
    OpenAiCompatible::from_env().ok().flatten()
}

/// 按 token 计价（未用 ChatGPT 订阅额度）。
pub(crate) fn plan_free(model: Option<&OpenAiCompatible>) -> bool {
    !model.is_some_and(OpenAiCompatible::uses_chatgpt_plan)
}

pub(crate) fn model_label() -> String {
    env_model().map_or("演示（未接模型）".into(), |m| m.model().to_owned())
}

/// 设置了 `ZIYUAN_LLM_MODEL` 时使用真实模型与只读工具（以当前目录为项目根），否则使用演示后端。
pub(crate) fn worker_backend() -> Result<Arc<dyn WorkerBackend>, Box<dyn Error>> {
    let Some(model) = OpenAiCompatible::from_env().map_err(|e| e as Box<dyn Error>)? else {
        return Ok(Arc::new(DemoBackend));
    };
    let plan = if model.uses_chatgpt_plan() {
        "，计入 ChatGPT 订阅额度"
    } else {
        ""
    };
    eprintln!(
        "执行后端：{}{plan}（工具与查阅范围由各诏令的玉玺决定）",
        model.model()
    );
    let catalog = Box::new(model.catalog());
    let steps = match env::var("ZIYUAN_MAX_STEPS") {
        Ok(n) => n
            .parse()
            .ok()
            .filter(|&n| n > 0)
            .ok_or("ZIYUAN_MAX_STEPS 须为正整数")?,
        Err(_) => MAX_AGENT_STEPS,
    };
    Ok(Arc::new(
        AgentBackend::new(Box::new(model), env::current_dir()?, steps)
            .with_catalog(catalog)
            .with_test_command(env::var("ZIYUAN_TEST_COMMAND").ok()),
    ))
}
