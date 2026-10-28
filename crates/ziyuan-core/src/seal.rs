//! 玉玺：工具权限、查阅范围与外藩（MCP）授权。

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use crate::ParseError;

/// 官员执行诏令时可使用的工具。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Capability {
    ReadFile,
    SearchCode,
    /// 写文件：改动只暂存，呈报皇帝落盘后才写入项目。
    WriteFile,
    /// 在项目副本中（含暂存改动）运行皇帝设定的测试命令。
    RunTests,
}
impl Capability {
    pub const ALL: [Self; 4] = [
        Self::ReadFile,
        Self::SearchCode,
        Self::WriteFile,
        Self::RunTests,
    ];
    /// 默认玉玺授予的只读工具。
    pub const READ_ONLY: [Self; 2] = [Self::ReadFile, Self::SearchCode];

    /// 与工具名一致。
    pub fn code(self) -> &'static str {
        match self {
            Self::ReadFile => "read_file",
            Self::SearchCode => "search_code",
            Self::WriteFile => "write_file",
            Self::RunTests => "run_tests",
        }
    }

    /// 动土木之权（写文件、跑测试）：唯工部诏令可授。
    pub fn is_works_only(self) -> bool {
        matches!(self, Self::WriteFile | Self::RunTests)
    }
}
impl FromStr for Capability {
    type Err = ParseError;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|c| c.code() == input)
            .ok_or_else(|| ParseError::UnknownCapability(input.into()))
    }
}

/// 玉玺（Yuxi）：盖在诏令上的权限令牌，限定承办官员可用的工具与可查阅的项目路径。
///
/// 由皇帝下诏时给定、随 `EdictIssued` 记入事件簿；运行时只按此令牌构造工具，官员无法越权。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seal {
    /// 允许的工具；为空表示只能凭诏令文本作答。
    pub capabilities: BTreeSet<Capability>,
    /// 可查阅的范围：相对项目根目录的目录或文件，`.` 表示整个项目。至少一项。
    pub scope: Vec<String>,
    /// 外藩朝贡：授予的外部 MCP 工具，`服务` 授该服务的全部工具，`服务/工具` 只授一件，
    /// `*` 授全部服务。默认不授。外部工具不受查阅范围约束（见 `ziyuan-runtime::mcp`）。
    pub mcp: BTreeSet<String>,
}

/// MCP 授权是否合法：`*`、`服务` 或 `服务/工具`，各段为字母、数字、`-`、`_`、`.`。
pub fn valid_mcp_grant(grant: &str) -> bool {
    let part = |s: &str| {
        !s.is_empty()
            && s.len() <= 64
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    match grant.split_once('/') {
        _ if grant == "*" => true,
        Some((server, tool)) => part(server) && part(tool),
        None => part(grant),
    }
}

/// 授权 `outer` 是否涵盖授权 `inner`。
pub(crate) fn mcp_grant_includes(outer: &str, inner: &str) -> bool {
    match inner.split_once('/') {
        _ if inner == "*" => outer == "*",
        Some((server, tool)) => mcp_grant_covers(outer, server, Some(tool)),
        None => mcp_grant_covers(outer, inner, None),
    }
}

/// 授权 `grant` 是否涵盖服务 `server` 的工具 `tool`（`tool` 为 `None` 时问的是整个服务）。
pub fn mcp_grant_covers(grant: &str, server: &str, tool: Option<&str>) -> bool {
    match grant.split_once('/') {
        _ if grant == "*" => true,
        Some((s, t)) => s == server && tool == Some(t),
        None => grant == server,
    }
}
impl Default for Seal {
    /// 全部只读工具，整个项目。
    fn default() -> Self {
        Self {
            capabilities: Capability::READ_ONLY.into(),
            scope: vec![".".into()],
            mcp: BTreeSet::new(),
        }
    }
}
impl Seal {
    /// 受 `ceiling`（官职上限）约束后的玉玺：工具取交集；查阅范围只保留落在上限内的部分
    /// （诏令路径在上限路径之下则保留，上限路径在诏令路径之下则收窄为上限路径）。
    /// 两者没有共同范围时返回 `None`。
    pub fn within(&self, ceiling: &Seal) -> Option<Seal> {
        fn parts(path: &str) -> Vec<&str> {
            path.split('/')
                .filter(|p| !p.is_empty() && *p != ".")
                .collect()
        }
        let under = |inner: &str, outer: &str| parts(inner).starts_with(&parts(outer));
        let mut scope: Vec<String> = Vec::new();
        for path in &self.scope {
            let narrowed: Vec<&String> = if ceiling.scope.iter().any(|c| under(path, c)) {
                vec![path]
            } else {
                ceiling.scope.iter().filter(|c| under(c, path)).collect()
            };
            for p in narrowed {
                if !scope.contains(p) {
                    scope.push(p.clone());
                }
            }
        }
        // MCP 授权：被上限某项涵盖的保留；否则收窄为上限中被它涵盖的那几项。
        let mut mcp = BTreeSet::new();
        for grant in &self.mcp {
            if ceiling.mcp.iter().any(|c| mcp_grant_includes(c, grant)) {
                mcp.insert(grant.clone());
            } else {
                let narrower = ceiling.mcp.iter().filter(|c| mcp_grant_includes(grant, c));
                mcp.extend(narrower.cloned());
            }
        }
        (!scope.is_empty()).then(|| Seal {
            capabilities: self
                .capabilities
                .intersection(&ceiling.capabilities)
                .copied()
                .collect(),
            scope,
            mcp,
        })
    }
}
impl fmt::Display for Seal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tools: Vec<_> = self.capabilities.iter().map(|c| c.code()).collect();
        let tools = if tools.is_empty() {
            "无".to_owned()
        } else {
            tools.join("、")
        };
        write!(f, "工具：{tools}；范围：{}", self.scope.join("、"))?;
        if !self.mcp.is_empty() {
            let mcp: Vec<_> = self.mcp.iter().map(String::as_str).collect();
            write!(f, "；外藩：{}", mcp.join("、"))?;
        }
        Ok(())
    }
}
