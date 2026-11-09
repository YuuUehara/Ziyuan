use super::*;
use std::fs;
use ziyuan_governance::{Court, TaskStatus};
use ziyuan_storage::{EventLog, EventStore, LockScope, Workspace};

impl Translations {
    pub fn preview_source_update(
        &self,
        id: &str,
        input: &SourceUpdate,
    ) -> Result<SourceUpdatePreview> {
        Ok(self.prepare_source_update(id, input)?.2)
    }
    fn prepare_source_update(
        &self,
        id: &str,
        input: &SourceUpdate,
    ) -> Result<(
        Project,
        Project,
        SourceUpdatePreview,
        Vec<ziyuan_core::CourtEvent>,
    )> {
        let (old, candidate, mut preview) = self.texts().prepare_update(id, input)?;
        let mut bindings = Vec::new();
        let context = self.store_dir(id)?.join("context");
        if context.exists() {
            for entry in fs::read_dir(context)? {
                let path = entry?.path().join("glossary.json");
                if path.is_file() {
                    let bytes = fs::read(path)?;
                    let glossary = Glossary::parse(&bytes)?;
                    if let Err(error) = glossary.validate_project(&candidate) {
                        preview
                            .blockers
                            .push(format!("旧上下文证据无法保留：{error}"));
                    }
                    bindings.push(sha256_hex(&bytes));
                }
            }
        }
        bindings.sort();
        let history = EventLog::new(self.root())
            .load()
            .map_err(|e| invalid(e.to_string()))?;
        if !history.is_empty() {
            let court = Court::replay(&history).map_err(|e| invalid(e.to_string()))?;
            for task in court.tasks() {
                if task
                    .translation
                    .as_ref()
                    .is_some_and(|l| l.project == id && preview.changed_blocks.contains(&l.block))
                    && matches!(
                        task.status,
                        TaskStatus::Pending | TaskStatus::Assigned { .. }
                    )
                {
                    preview
                        .blockers
                        .push(format!("诏令 {} 尚未完成，请先收尾或撤销", task.edict.id.0));
                }
            }
        }
        preview.fingerprint = sha256_hex(&serde_json::to_vec(&(
            &preview.fingerprint,
            bindings,
            history
                .iter()
                .map(ziyuan_storage::encode_event_line)
                .collect::<Vec<_>>(),
        ))?);
        Ok((old, candidate, preview, history))
    }
    pub fn apply_source_update(
        &self,
        id: &str,
        input: &SourceUpdate,
        expected: &str,
    ) -> Result<Project> {
        let workspace = Workspace::open(self.root()).map_err(|e| invalid(e.to_string()))?;
        let _guard = workspace
            .lock(LockScope::Workspace)
            .map_err(|e| invalid(e.to_string()))?;
        workspace
            .ensure_writable()
            .map_err(|e| invalid(e.to_string()))?;
        let (old, candidate, preview, history) = self.prepare_source_update(id, input)?;
        if preview.fingerprint != expected {
            return Err(invalid("预览后原文或引用材料有变，请重新预览"));
        }
        if !preview.blockers.is_empty() {
            return Err(invalid(preview.blockers.join("；")));
        }
        let mut published = None;
        EventLog::new(self.root())
            .append_if_unchanged_then(&history, &[], || {
                published =
                    Some(
                        self.texts()
                            .publish_update(&old, candidate, input, &preview.chapter)?,
                    );
                Ok::<_, TranslationError>(())
            })
            .map_err(|e| invalid(e.to_string()))??;
        published.ok_or_else(|| invalid("原文发布未完成"))
    }
}
