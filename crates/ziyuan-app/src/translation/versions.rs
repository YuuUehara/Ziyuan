//! Manual, immutable translation snapshots for version comparison.
use super::{Result, Translations, invalid};
use std::path::PathBuf;

fn safe_label(label: &str) -> String {
    label.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

impl Translations {
    fn versions_dir(&self, project: &str, block: &str) -> PathBuf {
        self.root.join(".ziyuan").join("text-projects").join(project).join("versions").join(block)
    }

    pub fn snapshot_translation(&self, project: &str, block: &str, label: &str) -> Result<String> {
        let bytes = self.read_translation(project, block)?.ok_or_else(|| invalid("当前块没有译稿，不能存快照"))?;
        let dir = self.versions_dir(project, block);
        std::fs::create_dir_all(&dir).map_err(|e| invalid(format!("创建版本目录失败：{e}")))?;
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let id = format!("{stamp}-{}.json", safe_label(if label.trim().is_empty() { "manual" } else { label }));
        std::fs::write(dir.join(&id), bytes).map_err(|e| invalid(format!("写快照失败：{e}")))?;
        Ok(id)
    }

    pub fn list_translation_versions(&self, project: &str, block: &str) -> Result<Vec<String>> {
        let dir = self.versions_dir(project, block);
        let Ok(entries) = std::fs::read_dir(&dir) else { return Ok(Vec::new()); };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".json") { out.push(name); }
        }
        out.sort();
        Ok(out)
    }

    pub fn read_translation_version(&self, project: &str, block: &str, id: &str) -> Result<Vec<u8>> {
        if id.contains('/') || id.contains('\\') || id.contains("..") {
            return Err(invalid("版本 ID 不合法"));
        }
        std::fs::read(self.versions_dir(project, block).join(id)).map_err(|e| invalid(format!("读取快照失败：{e}")))
    }
}
