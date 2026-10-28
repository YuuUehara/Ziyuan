//! 诏令模板：内置与自定义模板的保存、删除与套用。

use crate::edicts::check_criteria;
use crate::offices::validate_edict_seal;
use crate::{Court, GovernanceError, TaskStatus};
use serde::{Deserialize, Serialize};
use ziyuan_core::{Capability, CourtEvent, Department, Seal, TaskId, Template};

/// 诏令所用的模板（见 `CourtEvent::TemplateApplied`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateUse {
    pub name: String,
    pub criteria: Vec<String>,
}

/// 内置的翰林院模板：审查 unsafe、补测试、写文档。可以同名覆盖，不能撤去。
pub fn builtin_templates() -> Vec<(&'static str, Template)> {
    let seal = |tools: &[Capability]| Seal {
        capabilities: tools.iter().copied().collect(),
        scope: vec![".".into()],
        mcp: Default::default(),
    };
    let template = |text: &str, tools: &[Capability], criteria: &[&str]| Template {
        department: Department::Works,
        text: text.into(),
        seal: seal(tools),
        criteria: criteria.iter().map(|c| (*c).to_owned()).collect(),
    };
    use Capability::{ReadFile, RunTests, SearchCode, WriteFile};
    vec![
        (
            "审查unsafe",
            template(
                "审查项目中的 unsafe 代码：逐处说明用途、所依赖的安全前提，以及能否改为安全写法。",
                &[ReadFile, SearchCode],
                &[
                    "列出每一处 unsafe 的文件与行号",
                    "每一处都说明它依赖的安全前提",
                    "指出可以去掉的 unsafe，或说明为何不能去掉",
                ],
            ),
        ),
        (
            "补测试",
            template(
                "为下列代码补充单元测试，并运行测试确认通过：",
                &[ReadFile, SearchCode, WriteFile, RunTests],
                &[
                    "新增的测试覆盖诏令所指的行为",
                    "run_tests 运行通过",
                    "不改变被测代码的行为",
                ],
            ),
        ),
        (
            "写文档",
            template(
                "为下列内容撰写或更新文档：",
                &[ReadFile, SearchCode, WriteFile],
                &[
                    "文档与代码现状一致，不写不存在的功能",
                    "说明用法并给出示例",
                    "只改动文档文件",
                ],
            ),
        ),
    ]
}

impl Court {
    /// 翰林院存模板：名为 1–20 字、不含空白；内容非空；玉玺合乎部门（写权限唯工部）；
    /// 验收标准 1–8 条，各条单行、至多 200 字。
    pub fn propose_template(
        &self,
        name: String,
        template: Template,
    ) -> Result<CourtEvent, GovernanceError> {
        let bad = |why: &str| Err(GovernanceError::Template(why.into()));
        let count = name.chars().count();
        if count == 0 || count > 20 || name.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return bad("模板名须为 1–20 字，不含空白");
        }
        if template.text.trim().is_empty() {
            return bad("模板内容不能为空");
        }
        validate_edict_seal(template.department, &template.seal)?;
        check_criteria(&template.criteria)?;
        Ok(CourtEvent::TemplateSaved {
            name,
            template: Box::new(template),
        })
    }

    /// 撤去自定的模板；内置模板不能撤去。
    pub fn propose_template_removal(&self, name: String) -> Result<CourtEvent, GovernanceError> {
        if !self.templates.contains_key(&name) {
            let why = if builtin_templates().iter().any(|(n, _)| *n == name) {
                format!("「{name}」是内置模板，只能覆盖，不能撤去")
            } else {
                format!("没有自定的模板「{name}」")
            };
            return Err(GovernanceError::Template(why));
        }
        Ok(CourtEvent::TemplateRemoved { name })
    }

    /// 按名取模板：自定的优先，其次内置。
    pub fn template(&self, name: &str) -> Option<Template> {
        self.templates.get(name).cloned().or_else(|| {
            builtin_templates()
                .into_iter()
                .find(|(n, _)| *n == name)
                .map(|(_, t)| t)
        })
    }

    /// 全部模板（名、模板、是否内置原样）：内置的在前（自定的覆盖同名内置的），其余自定的按名排序。
    pub fn templates(&self) -> Vec<(String, Template, bool)> {
        let builtin = builtin_templates();
        let mut all: Vec<_> = builtin
            .iter()
            .map(|(name, t)| match self.templates.get(*name) {
                Some(custom) => ((*name).to_owned(), custom.clone(), false),
                None => ((*name).to_owned(), t.clone(), true),
            })
            .collect();
        all.extend(
            self.templates
                .iter()
                .filter(|(name, _)| !builtin.iter().any(|(b, _)| b == name))
                .map(|(name, t)| (name.clone(), t.clone(), false)),
        );
        all
    }

    /// 诏令按模板下达：诏令须待办、尚未用过模板；验收标准取当前模板的。
    pub fn propose_template_use(
        &self,
        task: TaskId,
        name: &str,
    ) -> Result<CourtEvent, GovernanceError> {
        self.check_template_use(task)?;
        let template = self
            .template(name)
            .ok_or_else(|| GovernanceError::Template(format!("没有模板「{name}」")))?;
        Ok(CourtEvent::TemplateApplied {
            task,
            name: name.to_owned(),
            criteria: template.criteria,
        })
    }

    pub(crate) fn check_template_use(&self, task: TaskId) -> Result<(), GovernanceError> {
        self.open_task(task)?;
        let entry = &self.tasks[&task];
        if entry.status != TaskStatus::Pending {
            return Err(GovernanceError::Template(format!(
                "诏令 #{} 已开办",
                task.0
            )));
        }
        if entry.template.is_some() {
            return Err(GovernanceError::Template(format!(
                "诏令 #{} 已按模板下达",
                task.0
            )));
        }
        Ok(())
    }
}
