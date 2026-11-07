//! 未批准资料的只读编辑视图、整份校验和身份预览。
use super::{
    MaterialBinding, MaterialError, MaterialPreview, MaterialValidation, MaterialView,
    validate_material,
};
use crate::text_project::Project;
use crate::translation::{
    Glossary, ParagraphAssociations, StructuredGlossary, Translations, evaluate_context_visibility,
    find_block, sha256_hex,
};
use serde::Serialize;

fn digest<T: Serialize + ?Sized>(value: &T) -> Result<String, MaterialError> {
    let bytes = serde_json::to_vec(value).map_err(|error| MaterialError::Project(error.into()))?;
    Ok(sha256_hex(&bytes))
}

/// 两次读取均必须经过项目原文／块副本核验；操作失败也不省略最终重读。
/// 私有读取边界同时供三种只读操作复用，不接受调用者提供的 Project。
fn with_verified_project<T>(
    mut read: impl FnMut() -> Result<Project, MaterialError>,
    operation: impl FnOnce(&Project, &str) -> Result<T, MaterialError>,
) -> Result<T, MaterialError> {
    let project = read()?;
    let hash = digest(&project)?;
    let result = operation(&project, &hash);
    let current = read()?;
    if digest(&current)? != hash {
        return Err(MaterialError::ProjectChanged);
    }
    result
}

impl Translations {
    /// 当前章段、原文及块选择信息和可选服务器草稿；不创建资料目录或锁。
    pub fn material_view(&self, project: &str) -> Result<MaterialView, MaterialError> {
        with_verified_project(
            || self.project(project).map_err(MaterialError::from),
            |verified, hash| {
                Ok(MaterialView {
                    project: verified.clone(),
                    project_hash: hash.into(),
                    draft: self.material_draft(project)?,
                })
            },
        )
    }

    /// 对当前输入作整份校验，绑定当前真实项目；不保存或批准。
    pub fn validate_material_input(
        &self,
        project: &str,
        material: &StructuredGlossary,
    ) -> Result<MaterialValidation, MaterialError> {
        with_verified_project(
            || self.project(project).map_err(MaterialError::from),
            |verified, hash| {
                let mut validation = validate_material(verified, material)?;
                validation.project_hash = hash.into();
                Ok(validation)
            },
        )
    }

    /// 临时关联只用于本次纯判定，不持久化、不拼装提示词、不调用模型。
    pub fn preview_material(
        &self,
        project: &str,
        material: &StructuredGlossary,
        block: &str,
        associations: &[ParagraphAssociations],
    ) -> Result<MaterialPreview, MaterialError> {
        with_verified_project(
            || self.project(project).map_err(MaterialError::from),
            |verified, hash| {
                let validation = validate_material(verified, material)?;
                if let Some(issue) = validation.issues.into_iter().next() {
                    return Err(issue.into());
                }
                let (_, _, target) =
                    find_block(verified, block).map_err(MaterialError::Visibility)?;
                let report = evaluate_context_visibility(
                    verified,
                    &Glossary::Structured(material.clone()),
                    block,
                    associations,
                )
                .map_err(MaterialError::Visibility)?;
                Ok(MaterialPreview {
                    binding: MaterialBinding {
                        content_hash: validation.content_hash,
                        project_hash: hash.into(),
                        block_hash: digest(target)?,
                        associations_hash: digest(associations)?,
                    },
                    report,
                })
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_project::Project;
    use crate::text_project::{NewProject, Provenance, TextProjects};
    use crate::translation::{
        Glossary, MaterialError, StructuredGlossary, Translations, evaluate_context_visibility,
    };
    use std::fs;
    use ziyuan_storage::JsonFile;

    #[test]
    fn project_change_during_preview_rejects_old_report() {
        let root = std::env::temp_dir().join(format!(
            "ziyuan-material-preview-change-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let texts = TextProjects::new(&root);
        let empty = texts
            .create(NewProject {
                title: "并发预览".into(),
                source_language: "ja".into(),
                target_language: "zh-CN".into(),
                provenance: Provenance::default(),
            })
            .unwrap();
        let source = root.join("source.txt");
        fs::write(&source, "自创原文。").unwrap();
        let project = texts.import(&empty.id, &[source]).unwrap();
        let service = Translations::new(&root);
        let data: StructuredGlossary =
            serde_json::from_str(r#"{"format":2,"terms":[],"persons":[]}"#).unwrap();
        // 在真实判定与最终重读之间改写合法清单；仅测试私有闭包控制时序，
        // 无全局环境开关、等待碰运气或可从生产访问的测试钩子。
        let result = with_verified_project(
            || service.project(&project.id).map_err(MaterialError::from),
            |verified, _| {
                let report = evaluate_context_visibility(
                    verified,
                    &Glossary::Structured(data),
                    &verified.chapters[0].blocks[0].block_id,
                    &[],
                )
                .map_err(MaterialError::Visibility)?;
                let file = JsonFile::<Project>::new(
                    texts.project_dir(&project.id).unwrap().join("project.json"),
                    3,
                    32 * 1024 * 1024,
                );
                file.update::<_, MaterialError>(|current| {
                    let mut value = current.unwrap().value;
                    value.metadata.title = "项目已变化".into();
                    Ok((value, ()))
                })
                .unwrap();
                Ok(report)
            },
        );
        assert!(matches!(result, Err(MaterialError::ProjectChanged)));
        assert_eq!(
            service.project(&project.id).unwrap().metadata.title,
            "项目已变化"
        );
    }
}
