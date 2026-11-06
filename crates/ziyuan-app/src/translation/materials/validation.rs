use super::{MaterialError, MaterialIssue, MaterialIssueCode, MaterialValidation};
use crate::text_project::Project;
use crate::translation::{StructuredGlossary, sha256_hex};

pub const MAX_MATERIAL_BYTES: usize = 1024 * 1024;

fn compact(material: &StructuredGlossary) -> Result<Vec<u8>, MaterialError> {
    serde_json::to_vec(material).map_err(|error| {
        MaterialIssue {
            path: vec![],
            code: MaterialIssueCode::Shape,
            message: format!("资料序列化失败：{error}"),
        }
        .into()
    })
}

/// Re-serialize direct Rust inputs through the exact editable parsing boundary.
pub fn check_material_editable(material: &StructuredGlossary) -> Result<(), MaterialError> {
    let bytes = compact(material)?;
    check_editable_bytes(&bytes)
}

fn check_editable_bytes(bytes: &[u8]) -> Result<(), MaterialError> {
    if bytes.len() > MAX_MATERIAL_BYTES {
        return Err(MaterialIssue {
            path: vec![],
            code: MaterialIssueCode::Limit,
            message: "资料紧凑 UTF-8 JSON 至多 1 MiB；请保留输入并缩减资料".into(),
        }
        .into());
    }
    StructuredGlossary::parse_editable_issues(bytes)?;
    Ok(())
}

/// Validate the complete material using the production parser and project checks.
/// Save-boundary errors are errors; unfinished semantics are a bound invalid report.
pub fn validate_material(
    project: &Project,
    material: &StructuredGlossary,
) -> Result<MaterialValidation, MaterialError> {
    let bytes = compact(material)?;
    check_editable_bytes(&bytes)?;
    let result = StructuredGlossary::parse_strict_issues(&bytes)
        .and_then(|data| data.validate_project_issues(project));
    let project_bytes =
        serde_json::to_vec(project).map_err(|error| MaterialError::Project(error.into()))?;
    let issues: Vec<_> = result.err().into_iter().collect();
    Ok(MaterialValidation {
        valid: issues.is_empty(),
        issues,
        content_hash: sha256_hex(&bytes),
        project_hash: sha256_hex(&project_bytes),
    })
}
