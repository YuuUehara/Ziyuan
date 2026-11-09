//! Portable translation workspace snapshots. Credentials and caches are never selected.
mod archive;
use super::{Result, Translations, invalid, sha256_hex};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Component, Path},
};
use ziyuan_storage::{LockScope, RecordState, Workspace, recovery::DecodedRecovery};

pub(super) const LIMIT: usize = 256 * 1024 * 1024;
pub(super) const MAX_FILES: usize = 20000;
pub(super) type Files = BTreeMap<String, Vec<u8>>;
pub(super) fn validate_log(files: &Files) -> Result<()> {
    if let Some(bytes) = files.get(".ziyuan/court.log") {
        if bytes.is_empty() {
            return Ok(());
        }
        let text = std::str::from_utf8(bytes).map_err(|_| invalid("事件簿不是UTF-8"))?;
        if !text.ends_with('\n') {
            return Err(invalid("事件簿有未收尾行，暂不能备份"));
        }
        let events = text
            .lines()
            .map(ziyuan_storage::decode_event_line)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(invalid)?;
        ziyuan_governance::Court::replay(&events).map_err(|e| invalid(e.to_string()))?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupFile {
    pub path: String,
    pub bytes: usize,
    pub sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupManifest {
    pub format: u32,
    pub producer_version: String,
    pub projects: Vec<String>,
    pub files: Vec<BackupFile>,
}
pub struct BackupBundle {
    pub manifest: BackupManifest,
    pub bytes: Vec<u8>,
}
pub use archive::{inspect_backup, restore_backup};

pub fn read_backup(path: &Path) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > LIMIT as u64 {
        return Err(invalid("备份须为256 MiB以内普通文件"));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > LIMIT {
        return Err(invalid("备份过大"));
    }
    Ok(bytes)
}
pub fn publish_backup(root: &Path, destination: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let destination = root.join(destination);
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("输出须有父目录"))?;
    plain_parents(parent)?;
    let parent = parent.canonicalize()?;
    let base = root.canonicalize()?;
    for control in [".ziyuan", "translations", ".git", ".jj"] {
        if parent.starts_with(base.join(control)) {
            return Err(invalid("不能向工作区数据目录导出备份"));
        }
    }
    let name = destination
        .file_name()
        .ok_or_else(|| invalid("输出须为文件"))?;
    let destination = parent.join(name);
    let temporary = parent.join(format!(
        ".ziyuan-backup-{}-{}.tmp",
        std::process::id(),
        crate::time::now()
    ));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    let result = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::hard_link(&temporary, &destination)
    })();
    let _ = fs::remove_file(&temporary);
    result.map_err(Into::into)
}

pub(super) fn safe_path(name: &str) -> Result<()> {
    if !selected(name) || name.contains('\\') || name.len() > 1024 {
        return Err(invalid("备份含范围外路径"));
    }
    for part in name.split('/') {
        let base = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with(['.', ' '])
            || part
                .chars()
                .any(|c| c.is_control() || ":*?\"<>|".contains(c))
            || matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (base.len() == 4
                && (base.starts_with("COM") || base.starts_with("LPT"))
                && base.as_bytes()[3].is_ascii_digit())
        {
            return Err(invalid("备份含不安全或不可移植路径"));
        }
    }
    Ok(())
}
fn selected(name: &str) -> bool {
    name.starts_with(".ziyuan/text-projects/")
        || name.starts_with("translations/")
        || matches!(
            name,
            ".ziyuan/court.log"
                | ".ziyuan/notes.json"
                | ".ziyuan/write-protocol"
                | ".ziyuan/write.generation"
        )
}
pub(super) fn plain_parents(path: &Path) -> Result<()> {
    let mut current = std::path::PathBuf::new();
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err(invalid("路径不能含 .."));
        }
        current.push(component);
        let meta = fs::symlink_metadata(&current)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(invalid("父路径须为普通目录"));
        }
    }
    Ok(())
}
fn collect(root: &Path, path: &Path, files: &mut Files, total: &mut usize) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if meta.file_type().is_symlink() {
        return Err(invalid("备份拒绝符号链接"));
    }
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            collect(root, &entry?.path(), files, total)?;
        }
    } else if meta.is_file() {
        let name = path
            .strip_prefix(root)
            .map_err(|_| invalid("备份越界"))?
            .to_str()
            .ok_or_else(|| invalid("路径不是UTF-8"))?
            .replace('\\', "/");
        if name.ends_with(".lock") || name.contains(".ziyuan-tmp-") {
            return Ok(());
        }
        safe_path(&name)?;
        if files.len() >= MAX_FILES || meta.len() > LIMIT as u64 {
            return Err(invalid("备份超出文件数量或大小上限"));
        }
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take((LIMIT + 1) as u64)
            .read_to_end(&mut bytes)?;
        *total = total
            .checked_add(bytes.len())
            .ok_or_else(|| invalid("备份过大"))?;
        if *total > LIMIT {
            return Err(invalid("备份超过256 MiB"));
        }
        files.insert(name, bytes);
    } else {
        return Err(invalid("备份只接受普通文件"));
    }
    Ok(())
}
fn snapshot(root: &Path) -> Result<Files> {
    let mut files = Files::new();
    let mut total = 0;
    for name in [
        ".ziyuan/text-projects",
        "translations",
        ".ziyuan/court.log",
        ".ziyuan/notes.json",
        ".ziyuan/write-protocol",
        ".ziyuan/write.generation",
    ] {
        collect(root, &root.join(name), &mut files, &mut total)?;
    }
    Ok(files)
}
pub(super) fn all_projects(root: &Path) -> Result<Vec<String>> {
    let texts = crate::text_project::TextProjects::new(root);
    let directory = root.join(".ziyuan/text-projects");
    let mut ids = Vec::new();
    if !directory.exists() {
        return Ok(ids);
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let meta = entry.file_type()?;
        if !meta.is_dir() || meta.is_symlink() {
            return Err(invalid("项目根目录含非普通目录"));
        }
        let id = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("项目名不是UTF-8"))?;
        texts.show(&id)?;
        ids.push(id);
    }
    ids.sort();
    Ok(ids)
}
impl Translations {
    /// Includes all text projects because their approvals share the court event stream.
    pub fn backup_workspace(&self) -> Result<BackupBundle> {
        plain_parents(self.root())?;
        let workspace = Workspace::open(self.root()).map_err(|e| invalid(e.to_string()))?;
        let _guard = workspace
            .lock(LockScope::Workspace)
            .map_err(|e| invalid(e.to_string()))?;
        workspace
            .ensure_writable()
            .map_err(|e| invalid(e.to_string()))?;
        if let Some(record) = workspace
            .pending_record_any()
            .map_err(|e| invalid(e.to_string()))?
        {
            let state = match record {
                DecodedRecovery::V1(r) => r.state,
                DecodedRecovery::V2(r) => r.state,
            };
            if matches!(state, RecordState::Prepared | RecordState::Published) {
                return Err(invalid("须先收尾受管写入再备份"));
            }
        }
        let projects = all_projects(self.root())?;
        if projects.is_empty() {
            return Err(invalid("尚无文本项目可备份"));
        }
        let files = snapshot(self.root())?;
        validate_log(&files)?;
        if files != snapshot(self.root())? {
            return Err(invalid("备份期间材料有变，请重试"));
        }
        let manifest = BackupManifest {
            format: 1,
            producer_version: env!("CARGO_PKG_VERSION").into(),
            projects,
            files: files
                .iter()
                .map(|(path, bytes)| BackupFile {
                    path: path.clone(),
                    bytes: bytes.len(),
                    sha256: sha256_hex(bytes),
                })
                .collect(),
        };
        let bytes = archive::encode(&manifest, &files)?;
        Ok(BackupBundle { manifest, bytes })
    }
}
