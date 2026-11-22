//! 工作区与环境只读自检（`ziyuan doctor` 与 `GET /api/doctor`）。
//!
//! 严格只读：不创建 `.ziyuan/`、不写入任何文件、绝不读取或回显 API Key / Token 等密文值。

use crate::translation::Translations;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use ziyuan_runtime::haoshe;
use ziyuan_storage::{EventLog, release_name};

const MAX_MODELS_JSON_BYTES: u64 = 256 * 1024;
const FORBIDDEN_SECRET_KEYS: &[&str] = &["api_key", "token", "secret", "password", "authorization"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorSeverity {
    Ok,
    Warn,
    Error,
}

impl DoctorSeverity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "正常",
            Self::Warn => "提示",
            Self::Error => "错误",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DoctorCheck {
    pub id: String,
    pub category: String,
    pub title: String,
    pub severity: DoctorSeverity,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    pub version: String,
    pub os: &'static str,
    pub arch: &'static str,
    pub workspace: String,
    pub overall: DoctorSeverity,
    pub ok_count: usize,
    pub warn_count: usize,
    pub error_count: usize,
    pub checks: Vec<DoctorCheck>,
}

/// 对工作区与运行环境执行全只读诊断。
pub fn diagnose_workspace(root: &Path) -> DoctorReport {
    let mut checks = Vec::new();
    let court = check_court(root, &mut checks);
    check_symbol_index(root, &mut checks);
    check_llm_env(&mut checks);
    check_models_json(root, court.as_ref(), &mut checks);
    check_sandbox(&mut checks);
    check_vcs(root, &mut checks);
    check_translation_and_memory(root, &mut checks);

    let ok_count = checks
        .iter()
        .filter(|c| c.severity == DoctorSeverity::Ok)
        .count();
    let warn_count = checks
        .iter()
        .filter(|c| c.severity == DoctorSeverity::Warn)
        .count();
    let error_count = checks
        .iter()
        .filter(|c| c.severity == DoctorSeverity::Error)
        .count();
    let overall = checks
        .iter()
        .map(|c| c.severity)
        .max()
        .unwrap_or(DoctorSeverity::Ok);

    DoctorReport {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        os: env::consts::OS,
        arch: env::consts::ARCH,
        workspace: root.display().to_string(),
        overall,
        ok_count,
        warn_count,
        error_count,
        checks,
    }
}

/// 将诊断报告渲染为适合命令行阅读的中文摘要。
pub fn format_doctor_report(report: &DoctorReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "紫垣环境自检（v{} · {}/{}）\n工作区：{}\n总评：{}（正常 {} · 提示 {} · 错误 {}）\n",
        report.version,
        report.os,
        report.arch,
        report.workspace,
        report.overall.label(),
        report.ok_count,
        report.warn_count,
        report.error_count
    ));
    for check in &report.checks {
        out.push_str(&format!(
            "\n[{}] {} — {}\n",
            check.severity.label(),
            check.title,
            check.summary
        ));
        if let Some(detail) = &check.detail {
            for line in detail.lines() {
                out.push_str(&format!("       {line}\n"));
            }
        }
    }
    out
}

fn env_present(name: &str) -> bool {
    env::var_os(name).is_some_and(|v| !v.is_empty())
}

fn check_court(root: &Path, checks: &mut Vec<DoctorCheck>) -> Option<ziyuan_governance::Court> {
    let log_file = root.join(".ziyuan").join("court.log");
    let has_court = log_file.metadata().is_ok_and(|m| m.len() > 0);
    if !has_court {
        checks.push(DoctorCheck {
            id: "workspace.court".into(),
            category: "workspace".into(),
            title: "朝廷事件簿".into(),
            severity: DoctorSeverity::Warn,
            summary: "当前目录尚未建朝（无 .ziyuan/court.log）".into(),
            detail: Some(
                "可执行 `ziyuan init` 建朝，或直接用于纯译书工作区（`ziyuan text`）。".into(),
            ),
        });
        return None;
    }

    let log = EventLog::new(root);
    match log.inspect() {
        Ok(report) => {
            let min_ver = release_name(report.readable_since);
            if let Some(problem) = &report.problem {
                checks.push(DoctorCheck {
                    id: "workspace.court".into(),
                    category: "workspace".into(),
                    title: "朝廷事件簿".into(),
                    severity: DoctorSeverity::Error,
                    summary: format!("本版无法读取事件簿：{problem}"),
                    detail: Some(format!(
                        "已扫 {} 条事件，当前最低读取版本标记：{min_ver}。",
                        report.events
                    )),
                });
                return None;
            }
            let (severity, summary) = if report.torn_tail {
                (
                    DoctorSeverity::Warn,
                    format!(
                        "可读（{} 条事件，最低版本 {min_ver}），但末尾存在未写完的残行",
                        report.events
                    ),
                )
            } else {
                (
                    DoctorSeverity::Ok,
                    format!("健康（{} 条事件，最低读取版本 {min_ver}）", report.events),
                )
            };
            let court = crate::Session::open(log).ok().map(|s| s.court().clone());
            checks.push(DoctorCheck {
                id: "workspace.court".into(),
                category: "workspace".into(),
                title: "朝廷事件簿".into(),
                severity,
                summary,
                detail: report
                    .torn_tail
                    .then(|| "下次正常写入时会自动截除残行，无需手工编辑事件簿。".into()),
            });
            court
        }
        Err(err) => {
            checks.push(DoctorCheck {
                id: "workspace.court".into(),
                category: "workspace".into(),
                title: "朝廷事件簿".into(),
                severity: DoctorSeverity::Error,
                summary: format!("读取事件簿失败：{err}"),
                detail: None,
            });
            None
        }
    }
}

fn check_symbol_index(root: &Path, checks: &mut Vec<DoctorCheck>) {
    let idx = root.join(".ziyuan").join("index").join("symbols-v1.json");
    match fs::symlink_metadata(&idx) {
        Err(_) => {
            checks.push(DoctorCheck {
                id: "workspace.symbol_index".into(),
                category: "workspace".into(),
                title: "增量符号索引缓存".into(),
                severity: DoctorSeverity::Ok,
                summary: "尚未生成（首次调用 find_symbols / find_references 时自动建立）".into(),
                detail: None,
            });
        }
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                checks.push(DoctorCheck {
                    id: "workspace.symbol_index".into(),
                    category: "workspace".into(),
                    title: "增量符号索引缓存".into(),
                    severity: DoctorSeverity::Warn,
                    summary: "缓存路径为符号链接，运行时将忽略并回退扫描".into(),
                    detail: None,
                });
                return;
            }
            let size = meta.len();
            let parsed = fs::read(&idx)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
            match parsed.and_then(|v| v.get("files").and_then(Value::as_object).map(|m| m.len())) {
                Some(file_count) => {
                    checks.push(DoctorCheck {
                        id: "workspace.symbol_index".into(),
                        category: "workspace".into(),
                        title: "增量符号索引缓存".into(),
                        severity: DoctorSeverity::Ok,
                        summary: format!(
                            "已缓存 {file_count} 个文件（{:.1} KiB）",
                            size as f64 / 1024.0
                        ),
                        detail: None,
                    });
                }
                None => {
                    checks.push(DoctorCheck {
                        id: "workspace.symbol_index".into(),
                        category: "workspace".into(),
                        title: "增量符号索引缓存".into(),
                        severity: DoctorSeverity::Warn,
                        summary: format!("缓存文件格式异常（{size} 字节），下次检索将自动重建"),
                        detail: None,
                    });
                }
            }
        }
    }
}

fn check_llm_env(checks: &mut Vec<DoctorCheck>) {
    let provider_raw = env::var("ZIYUAN_LLM_PROVIDER").unwrap_or_default();
    let provider = provider_raw.trim().to_ascii_lowercase();
    let model = env::var("ZIYUAN_LLM_MODEL").unwrap_or_default();
    let cache_raw = env::var("ZIYUAN_LLM_PROMPT_CACHE").unwrap_or_default();
    let cache_valid = matches!(
        cache_raw.trim().to_ascii_lowercase().as_str(),
        "" | "auto" | "anthropic" | "on" | "1" | "true" | "off" | "0" | "false"
    );

    let provider_valid = matches!(
        provider.as_str(),
        "" | "api" | "openai" | "chatgpt" | "anthropic" | "gemini"
    );

    let key_flags = [
        ("ZIYUAN_LLM_API_KEY", env_present("ZIYUAN_LLM_API_KEY")),
        ("OPENAI_API_KEY", env_present("OPENAI_API_KEY")),
        ("ANTHROPIC_API_KEY", env_present("ANTHROPIC_API_KEY")),
        ("GEMINI_API_KEY", env_present("GEMINI_API_KEY")),
    ];
    let set_keys: Vec<&str> = key_flags
        .iter()
        .filter_map(|(name, present)| present.then_some(*name))
        .collect();

    if !provider_valid {
        checks.push(DoctorCheck {
            id: "llm.env".into(),
            category: "llm".into(),
            title: "默认模型环境变量".into(),
            severity: DoctorSeverity::Error,
            summary: format!("ZIYUAN_LLM_PROVIDER 值无效：{provider_raw}"),
            detail: Some("支持值：api / openai / chatgpt / anthropic / gemini。".into()),
        });
        return;
    }
    if !cache_valid {
        checks.push(DoctorCheck {
            id: "llm.env".into(),
            category: "llm".into(),
            title: "默认模型环境变量".into(),
            severity: DoctorSeverity::Error,
            summary: format!("ZIYUAN_LLM_PROMPT_CACHE 值无效：{cache_raw}"),
            detail: Some("支持值：auto / anthropic / on / off。".into()),
        });
        return;
    }

    let provider_label = if provider.is_empty() {
        "api（默认）"
    } else {
        provider.as_str()
    };
    let model_label = if model.trim().is_empty() {
        "未设（演示模式）"
    } else {
        model.trim()
    };
    let cache_label = if cache_raw.trim().is_empty() {
        "auto（默认）"
    } else {
        cache_raw.trim()
    };
    let keys_desc = if set_keys.is_empty() {
        "未检测到常见密钥环境变量".to_owned()
    } else {
        format!(
            "已设密钥变量：{}（仅检存在，不读取值）",
            set_keys.join("、")
        )
    };

    let severity = if model.trim().is_empty() && provider != "chatgpt" {
        DoctorSeverity::Warn
    } else {
        DoctorSeverity::Ok
    };

    checks.push(DoctorCheck {
        id: "llm.env".into(),
        category: "llm".into(),
        title: "默认模型环境变量".into(),
        severity,
        summary: format!("服务商：{provider_label} · 模型：{model_label} · 缓存：{cache_label}"),
        detail: Some(keys_desc),
    });
}

fn check_models_json(
    root: &Path,
    court: Option<&ziyuan_governance::Court>,
    checks: &mut Vec<DoctorCheck>,
) {
    let path = root.join(".ziyuan").join("models.json");
    let meta = match fs::symlink_metadata(&path) {
        Err(_) => {
            checks.push(DoctorCheck {
                id: "llm.models_json".into(),
                category: "llm".into(),
                title: "异构模型路由表（.ziyuan/models.json）".into(),
                severity: DoctorSeverity::Ok,
                summary: "未配置（按默认环境变量运行）".into(),
                detail: None,
            });
            return;
        }
        Ok(m) => m,
    };

    if meta.file_type().is_symlink() {
        checks.push(DoctorCheck {
            id: "llm.models_json".into(),
            category: "llm".into(),
            title: "异构模型路由表（.ziyuan/models.json）".into(),
            severity: DoctorSeverity::Error,
            summary: "拒绝符号链接 .ziyuan/models.json".into(),
            detail: None,
        });
        return;
    }
    if meta.len() > MAX_MODELS_JSON_BYTES {
        checks.push(DoctorCheck {
            id: "llm.models_json".into(),
            category: "llm".into(),
            title: "异构模型路由表（.ziyuan/models.json）".into(),
            severity: DoctorSeverity::Error,
            summary: format!("文件体积超过 256 KiB 上限（{} 字节）", meta.len()),
            detail: None,
        });
        return;
    }

    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            checks.push(DoctorCheck {
                id: "llm.models_json".into(),
                category: "llm".into(),
                title: "异构模型路由表（.ziyuan/models.json）".into(),
                severity: DoctorSeverity::Error,
                summary: format!("读取失败：{e}"),
                detail: None,
            });
            return;
        }
    };
    let parsed: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            checks.push(DoctorCheck {
                id: "llm.models_json".into(),
                category: "llm".into(),
                title: "异构模型路由表（.ziyuan/models.json）".into(),
                severity: DoctorSeverity::Error,
                summary: format!("JSON 解析失败：{e}"),
                detail: None,
            });
            return;
        }
    };

    let Some(profiles_obj) = parsed.get("profiles").and_then(Value::as_object) else {
        checks.push(DoctorCheck {
            id: "llm.models_json".into(),
            category: "llm".into(),
            title: "异构模型路由表（.ziyuan/models.json）".into(),
            severity: DoctorSeverity::Error,
            summary: "缺少顶层 profiles 对象".into(),
            detail: None,
        });
        return;
    };

    let mut forbidden_hits = Vec::new();
    let mut ready_count = 0usize;
    let mut missing_env_profiles = Vec::new();
    let mut known_profiles = BTreeSet::new();
    let mut unready_profiles = BTreeSet::new();

    for (name, item) in profiles_obj {
        known_profiles.insert(name.as_str());
        if let Some(obj) = item.as_object() {
            for key in obj.keys() {
                let lower = key.to_ascii_lowercase();
                if FORBIDDEN_SECRET_KEYS.contains(&lower.as_str()) {
                    forbidden_hits.push(format!("{name}.{key}"));
                }
            }
            let provider = obj
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("openai");
            let env_name = obj.get("api_key_env").and_then(Value::as_str);
            let ready = match (provider, env_name) {
                ("chatgpt", _) => true,
                (_, Some(env_var)) if !env_var.trim().is_empty() => env_present(env_var.trim()),
                ("anthropic", None) => env_present("ANTHROPIC_API_KEY"),
                ("gemini", None) => env_present("GEMINI_API_KEY"),
                _ => env_present("ZIYUAN_LLM_API_KEY") || env_present("OPENAI_API_KEY"),
            };
            if ready {
                ready_count += 1;
            } else {
                unready_profiles.insert(name.as_str());
                let hint = env_name.unwrap_or(match provider {
                    "anthropic" => "ANTHROPIC_API_KEY",
                    "gemini" => "GEMINI_API_KEY",
                    _ => "ZIYUAN_LLM_API_KEY / OPENAI_API_KEY",
                });
                missing_env_profiles.push(format!("{name}（缺 {hint}）"));
            }
        }
    }

    if !forbidden_hits.is_empty() {
        checks.push(DoctorCheck {
            id: "llm.models_json".into(),
            category: "llm".into(),
            title: "异构模型路由表（.ziyuan/models.json）".into(),
            severity: DoctorSeverity::Error,
            summary: format!(
                "检测到禁止的明文密钥字段：{}（请改为 api_key_env 环境变量名）",
                forbidden_hits.join("、")
            ),
            detail: None,
        });
        return;
    }

    let mut office_notes = Vec::new();
    if let Some(c) = court {
        for office in ziyuan_core::Office::all() {
            if let Some(model_name) = c.office_model(office)
                && unready_profiles.contains(model_name)
            {
                office_notes.push(format!(
                    "{} -> {model_name}（对应密钥未就绪）",
                    office.code()
                ));
            }
        }
    }

    let severity = if !missing_env_profiles.is_empty() || !office_notes.is_empty() {
        DoctorSeverity::Warn
    } else {
        DoctorSeverity::Ok
    };

    let mut details = Vec::new();
    if !missing_env_profiles.is_empty() {
        details.push(format!("未就绪配置：{}", missing_env_profiles.join("；")));
    }
    if !office_notes.is_empty() {
        details.push(format!("已分派官职提醒：{}", office_notes.join("；")));
    }

    checks.push(DoctorCheck {
        id: "llm.models_json".into(),
        category: "llm".into(),
        title: "异构模型路由表（.ziyuan/models.json）".into(),
        severity,
        summary: format!(
            "共 {} 项配置（已就绪 {} 项，零明文密钥）",
            profiles_obj.len(),
            ready_count
        ),
        detail: (!details.is_empty()).then(|| details.join("\n")),
    });
}

fn check_sandbox(checks: &mut Vec<DoctorCheck>) {
    let mode_res = haoshe::Mode::from_env();
    let probe = haoshe::probe_status();
    let net_res = haoshe::Network::from_env();
    let toolchains = haoshe::toolchain_dirs_from_env();
    let missing_toolchains: Vec<String> = toolchains
        .iter()
        .filter(|p| !p.exists())
        .map(|p| p.display().to_string())
        .collect();
    let test_presets = env::var("ZIYUAN_TEST_COMMANDS")
        .ok()
        .map(|raw| raw.split(';').filter(|s| !s.trim().is_empty()).count())
        .unwrap_or(0);

    let (severity, summary) = match (&mode_res, &probe, &net_res) {
        (Err(e), _, _) => (
            DoctorSeverity::Error,
            format!("ZIYUAN_SANDBOX 配置错误：{e}"),
        ),
        (_, _, Err(e)) => (
            DoctorSeverity::Error,
            format!("ZIYUAN_SANDBOX_ALLOW 配置错误：{e}"),
        ),
        (Ok(haoshe::Mode::Off), _, _) => (
            DoctorSeverity::Warn,
            "已通过 ZIYUAN_SANDBOX=off 关闭号舍沙箱".into(),
        ),
        (Ok(_), Ok(ok_desc), Ok(_)) if !missing_toolchains.is_empty() => (
            DoctorSeverity::Warn,
            format!("{ok_desc}；但 ZIYUAN_SANDBOX_TOOLCHAIN 含不存在目录"),
        ),
        (Ok(_), Ok(ok_desc), Ok(_)) => (DoctorSeverity::Ok, ok_desc.clone()),
        (Ok(_), Err(reason), Ok(_)) => (
            DoctorSeverity::Warn,
            format!("本机号舍不可用（run_tests 将失败关闭）：{reason}"),
        ),
    };

    let mut lines = Vec::new();
    if let Ok(net) = &net_res {
        let net_text = match net {
            haoshe::Network::Off => "全断网（默认）".to_owned(),
            haoshe::Network::Open => "开放联网".to_owned(),
            haoshe::Network::Only(domains) => {
                format!("门吏白名单（{} 项：{}）", domains.len(), domains.join("、"))
            }
        };
        lines.push(format!("出站策略：{net_text}"));
    }
    if !toolchains.is_empty() {
        lines.push(format!(
            "只读工具链快照目录：{} 项{}",
            toolchains.len(),
            if missing_toolchains.is_empty() {
                String::new()
            } else {
                format!("（缺失：{}）", missing_toolchains.join("、"))
            }
        ));
    }
    if test_presets > 0 {
        lines.push(format!(
            "ZIYUAN_TEST_COMMANDS 命名检查预设：{test_presets} 项"
        ));
    }

    checks.push(DoctorCheck {
        id: "sandbox.probe".into(),
        category: "sandbox".into(),
        title: "号舍沙箱与执行策略".into(),
        severity,
        summary,
        detail: (!lines.is_empty()).then(|| lines.join("\n")),
    });
}

fn tool_version(program: &str) -> Option<String> {
    Command::new(program)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        })
}

fn check_vcs(root: &Path, checks: &mut Vec<DoctorCheck>) {
    let jj_ver = tool_version("jj");
    let git_ver = tool_version("git");
    let has_jj = root.join(".jj").exists();
    let has_git = root.join(".git").exists();

    let mut parts = Vec::new();
    if let Some(v) = &jj_ver {
        parts.push(format!(
            "{v}{}",
            if has_jj {
                "（当前目录为 jj 仓）"
            } else {
                ""
            }
        ));
    } else {
        parts.push("未检测到 jj".into());
    }
    if let Some(v) = &git_ver {
        parts.push(format!(
            "{v}{}",
            if has_git {
                "（当前目录为 git 仓）"
            } else {
                ""
            }
        ));
    } else {
        parts.push("未检测到 git".into());
    }

    let severity = if jj_ver.is_none() && git_ver.is_none() {
        DoctorSeverity::Warn
    } else {
        DoctorSeverity::Ok
    };

    checks.push(DoctorCheck {
        id: "vcs.tools".into(),
        category: "vcs".into(),
        title: "版本库工具（jj / git）".into(),
        severity,
        summary: parts.join(" · "),
        detail: None,
    });
}

fn check_translation_and_memory(root: &Path, checks: &mut Vec<DoctorCheck>) {
    let projects_count = Translations::new(root)
        .texts()
        .list()
        .map(|v| v.len())
        .unwrap_or(0);
    let mem_provider = env::var("ZIYUAN_MEMORY_PROVIDER").unwrap_or_default();
    let mem_label = if mem_provider.trim().is_empty() {
        "本地离线（默认）".to_owned()
    } else {
        mem_provider.trim().to_owned()
    };

    checks.push(DoctorCheck {
        id: "translation.workspace".into(),
        category: "translation".into(),
        title: "翰林院译书与译文记忆".into(),
        severity: DoctorSeverity::Ok,
        summary: format!("已建译书项目：{projects_count} 个 · 记忆后端：{mem_label}"),
        detail: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = env::temp_dir().join(format!(
            "ziyuan-doctor-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn diagnose_uninitialized_workspace_is_readonly_and_never_creates_ziyuan_dir() {
        let dir = temp_dir("uninit");
        let report = diagnose_workspace(&dir);
        assert!(
            !dir.join(".ziyuan").exists(),
            "doctor 绝不得创建 .ziyuan 目录"
        );
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.id == "workspace.court" && c.severity == DoctorSeverity::Warn)
        );
        let rendered = format_doctor_report(&report);
        assert!(rendered.contains("紫垣环境自检"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn diagnose_detects_plaintext_secret_in_models_json_without_echoing_value() {
        let dir = temp_dir("secret");
        let ziyuan_dir = dir.join(".ziyuan");
        fs::create_dir_all(&ziyuan_dir).unwrap();
        let secret_sentinel = "sk-super-secret-value-never-leak-99999";
        fs::write(
            ziyuan_dir.join("models.json"),
            format!(
                r#"{{"profiles":{{"bad":{{"provider":"openai","model":"gpt-4o","api_key":"{secret_sentinel}"}}}}}}"#
            ),
        )
        .unwrap();

        let report = diagnose_workspace(&dir);
        let check = report
            .checks
            .iter()
            .find(|c| c.id == "llm.models_json")
            .unwrap();
        assert_eq!(check.severity, DoctorSeverity::Error);
        assert!(check.summary.contains("明文密钥"));

        let json_out = serde_json::to_string(&report).unwrap();
        let text_out = format_doctor_report(&report);
        assert!(!json_out.contains(secret_sentinel));
        assert!(!text_out.contains(secret_sentinel));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn diagnose_founded_workspace_reports_healthy_event_log() {
        let dir = temp_dir("founded");
        let log = EventLog::new(&dir);
        crate::Session::found(log).unwrap();

        let report = diagnose_workspace(&dir);
        let check = report
            .checks
            .iter()
            .find(|c| c.id == "workspace.court")
            .unwrap();
        assert_eq!(check.severity, DoctorSeverity::Ok);
        assert!(check.summary.contains("健康"));
        let _ = fs::remove_dir_all(&dir);
    }
}
