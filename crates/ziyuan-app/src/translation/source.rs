//! Approval identity comes from event order, never task order or equal disk bytes.
use super::{Result, invalid, output_rel, sha256_hex};
use crate::{changes_version, selected_files};
use ziyuan_core::{CourtEvent, TranslationKind, TranslationSource};
use ziyuan_governance::Court;
use ziyuan_storage::event_reference;

/// Facts resolved from the log only. Disk and material freshness are separate checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTranslationSource {
    pub source: TranslationSource,
    pub translation_hash: Option<String>,
}

/// Resolve the last real approval for one block, including proposal-only revisions.
/// Human bindings, finalization and equal text do not create approval identities;
/// a human approval event does, at its own log position.
pub fn resolve_translation_source(
    history: &[CourtEvent],
    project: &str,
    block: &str,
) -> Result<ResolvedTranslationSource> {
    let mut court = Court::replay(history.get(..1).unwrap_or_default())
        .map_err(|e| invalid(format!("来源事件簿不能回放：{e}")))?;
    let output = output_rel(project, block);
    let mut resolved = ResolvedTranslationSource {
        source: TranslationSource::None {},
        translation_hash: None,
    };
    for (index, event) in history.iter().enumerate().skip(1) {
        court
            .apply(event)
            .map_err(|e| invalid(format!("来源事件不能回放：{e}")))?;
        match event {
            CourtEvent::ChangesApplied { task, skipped } => {
                let entry = court.task(*task).expect("replayed application has a task");
                let changes = entry
                    .changes
                    .as_ref()
                    .expect("replayed application has changes");
                let selected = selected_files(&changes.files, skipped);
                let target = selected.iter().find(|file| file.path == output);
                let writer = entry.translation.as_ref().filter(|link| {
                    link.project == project
                        && link.block == block
                        && link.kind.writes()
                        && link.output_path == output
                });
                let Some(link) = writer else {
                    if target.is_some() {
                        return Err(invalid("译稿曾由非本块译书准奏改写，不能猜测其来源"));
                    }
                    continue;
                };
                if let Some(file) = target {
                    resolved.translation_hash = Some(sha256_hex(file.after.as_bytes()));
                } else if link.kind != TranslationKind::Revise
                    || resolved.translation_hash.is_none()
                {
                    return Err(invalid("译书准奏没有可核验的译稿来源"));
                }
                resolved.source = TranslationSource::Model {
                    task: *task,
                    changes_version: changes_version(&changes.files),
                    event: event_reference(index as u64, event),
                };
            }
            CourtEvent::HumanTranslationApproved { approval } => {
                let request = &approval.request;
                if request.project != project
                    || request.block != block
                    || request.target.path != output
                {
                    continue;
                }
                resolved.translation_hash = Some(request.target.after.clone());
                resolved.source = TranslationSource::Human {
                    operation_id: approval.operation_id.clone(),
                    event: event_reference(index as u64, event),
                };
            }
            _ => {}
        }
    }

    Ok(resolved)
}
