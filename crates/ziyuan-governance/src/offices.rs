//! 官职设置与玉玺：各官职的模型、权限上限与实际授予的玉玺。

use crate::{Court, GovernanceError};
use std::path::{Component, Path};
use ziyuan_core::{AgentId, CHANGELOG, Capability, CourtEvent, Department, Edict, Office, Seal};

pub(crate) fn validate_edict_seal(
    department: Department,
    seal: &Seal,
) -> Result<(), GovernanceError> {
    // 礼部修 CHANGELOG：可授 write_file，但范围只限 CHANGELOG.md 一个文件，且不能跑测试。
    let changelog = department == Department::Rites && seal.scope == [CHANGELOG];
    if department != Department::Works
        && let Some(&c) = seal
            .capabilities
            .iter()
            .find(|c| c.is_works_only() && !(changelog && **c == Capability::WriteFile))
    {
        return Err(GovernanceError::WorksOnly(c));
    }
    validate_seal(seal)
}

pub(crate) fn validate_seal(seal: &Seal) -> Result<(), GovernanceError> {
    if seal.scope.is_empty() {
        return Err(GovernanceError::InvalidScope(String::new()));
    }
    if let Some(bad) = seal.mcp.iter().find(|m| !ziyuan_core::valid_mcp_grant(m)) {
        return Err(GovernanceError::Court(format!(
            "外藩授权不合法：{bad}（须为 *、服务 或 服务/工具，由字母、数字、-、_、. 组成）"
        )));
    }
    for path in &seal.scope {
        let escapes = Path::new(path)
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir));
        if path.trim().is_empty() || escapes {
            return Err(GovernanceError::InvalidScope(path.clone()));
        }
    }
    Ok(())
}

impl Court {
    /// 为官职指定承办模型；`None` 恢复默认。
    pub fn propose_office_model(
        &self,
        office: Office,
        model: Option<String>,
    ) -> Result<CourtEvent, GovernanceError> {
        if let Some(m) = &model
            && (m.is_empty() || m.chars().any(char::is_whitespace))
        {
            return Err(GovernanceError::InvalidModel(m.clone()));
        }
        Ok(CourtEvent::OfficeModelSet { office, model })
    }

    /// 为官职设定玉玺上限；`None` 取消上限。
    pub fn propose_office_ceiling(
        &self,
        office: Office,
        ceiling: Option<Seal>,
    ) -> Result<CourtEvent, GovernanceError> {
        if let Some(seal) = &ceiling {
            validate_seal(seal)?;
        }
        Ok(CourtEvent::OfficeCeilingSet { office, ceiling })
    }

    /// 官职指定的承办模型；未指定时用默认模型。
    pub fn office_model(&self, office: Office) -> Option<&str> {
        self.office_models.get(&office.code()).map(String::as_str)
    }

    pub fn office_ceiling(&self, office: Office) -> Option<&Seal> {
        self.office_ceilings.get(&office.code())
    }

    /// `agent` 承办诏令时所用的模型（按其现任官职）；`None` 表示默认模型。
    pub fn model_of(&self, agent: AgentId) -> Option<&str> {
        self.office_of(agent).and_then(|o| self.office_model(o))
    }

    /// `agent` 承办 `edict` 时实际可用的玉玺：受其现任官职上限约束。
    /// 诏令的查阅范围与上限毫无交集时返回 `Err(SealExceedsCeiling)`。
    pub fn effective_seal(&self, agent: AgentId, edict: &Edict) -> Result<Seal, GovernanceError> {
        let mut seal = match self
            .office_of(agent)
            .and_then(|o| Some((o, self.office_ceiling(o)?)))
        {
            None => edict.seal.clone(),
            Some((office, ceiling)) => edict
                .seal
                .within(ceiling)
                .ok_or(GovernanceError::SealExceedsCeiling(office))?,
        };
        // 廷杖降级留用期间，收回工部动土之权与外藩工具。
        if self.demotion(agent) > 0 {
            seal.mcp.clear();
            seal.capabilities.retain(|c| !c.is_works_only());
        }
        Ok(seal)
    }
}
