use super::*;
use std::{
    collections::BTreeSet,
    io::{Cursor, Write},
    sync::atomic::{AtomicU64, Ordering},
};

pub(super) fn encode(manifest: &BackupManifest, files: &Files) -> Result<Vec<u8>> {
    let metadata = serde_json::to_vec(manifest)?;
    let mut tar = tar::Builder::new(Vec::new());
    for (name, bytes) in files
        .iter()
        .map(|(p, b)| (p.as_str(), b.as_slice()))
        .chain(std::iter::once(("manifest.json", metadata.as_slice())))
    {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_mtime(0);
        header.set_cksum();
        tar.append_data(&mut header, name, bytes)?;
    }
    let bytes = tar.into_inner()?;
    if bytes.len() > LIMIT {
        return Err(invalid("备份包超过256 MiB"));
    }
    Ok(bytes)
}
fn decode(bytes: &[u8]) -> Result<(BackupManifest, Files)> {
    if bytes.len() > LIMIT {
        return Err(invalid("备份包超过256 MiB"));
    }
    let mut tar = tar::Archive::new(Cursor::new(bytes));
    let mut files = Files::new();
    let mut seen = BTreeSet::new();
    let mut total = 0usize;
    let mut prefixes = std::collections::BTreeMap::new();
    for entry in tar.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            return Err(invalid("备份不能包含链接或目录条目"));
        }
        let name = std::str::from_utf8(&entry.path_bytes())
            .map_err(|_| invalid("路径不是UTF-8"))?
            .to_owned();
        if name != "manifest.json" {
            safe_path(&name)?;
        }
        let mut prefix = String::new();
        for component in name.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            if let Some(existing) = prefixes.insert(prefix.to_lowercase(), prefix.clone())
                && existing != prefix
            {
                return Err(invalid("备份路径组件大小写冲突"));
            }
        }
        if !seen.insert(name.to_lowercase()) || files.len() > MAX_FILES {
            return Err(invalid("备份路径重复或数量过大"));
        }
        let mut contents = Vec::new();
        entry
            .by_ref()
            .take((LIMIT + 1) as u64)
            .read_to_end(&mut contents)?;
        total = total
            .checked_add(contents.len())
            .ok_or_else(|| invalid("备份过大"))?;
        if total > LIMIT {
            return Err(invalid("备份解包超过256 MiB"));
        }
        files.insert(name, contents);
    }
    let metadata = files
        .remove("manifest.json")
        .ok_or_else(|| invalid("缺少备份清单"))?;
    let manifest: BackupManifest = serde_json::from_slice(&metadata)?;
    if manifest.format != 1 || manifest.files.len() != files.len() {
        return Err(invalid("不支持的备份版本或清单不完整"));
    }
    let mut declared = BTreeSet::new();
    for file in &manifest.files {
        if !declared.insert(&file.path) {
            return Err(invalid("清单路径重复"));
        }
        let bytes = files
            .get(&file.path)
            .ok_or_else(|| invalid("清单文件缺失"))?;
        if file.bytes != bytes.len() || file.sha256 != sha256_hex(bytes) {
            return Err(invalid("备份校验失败"));
        }
    }
    for path in files.keys() {
        let id = path
            .strip_prefix(".ziyuan/text-projects/")
            .or_else(|| path.strip_prefix("translations/"))
            .and_then(|p| p.split('/').next());
        if let Some(id) = id
            && !manifest.projects.iter().any(|p| p == id)
        {
            return Err(invalid("归档含清单外项目"));
        }
    }
    validate_log(&files)?;
    Ok((manifest, files))
}
pub fn inspect_backup(bytes: &[u8]) -> Result<BackupManifest> {
    Ok(decode(bytes)?.0)
}

pub fn restore_backup(bytes: &[u8], destination: &Path) -> Result<BackupManifest> {
    let (manifest, files) = decode(bytes)?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| invalid("恢复目标须有父目录"))?;
    plain_parents(parent)?;
    if !matches!(
        destination.components().next_back(),
        Some(Component::Normal(_))
    ) {
        return Err(invalid("恢复目标须为新目录"));
    }
    if fs::symlink_metadata(destination).is_ok() {
        return Err(invalid("恢复目标已存在，拒绝覆盖"));
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let stage = parent.join(format!(
        ".ziyuan-restore-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&stage)?;
    // A failed validation intentionally retains the staging directory for diagnosis.
    for (name, bytes) in files {
        let file = stage.join(name);
        fs::create_dir_all(file.parent().ok_or_else(|| invalid("备份路径无父目录"))?)?;
        let mut handle = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(file)?;
        handle.write_all(&bytes)?;
        handle.sync_all()?;
    }
    let restored = all_projects(&stage)?;
    if restored != manifest.projects || restored.is_empty() {
        return Err(invalid("恢复项目与清单不符"));
    }
    if stage
        .join(".ziyuan/court.log")
        .metadata()
        .is_ok_and(|m| m.len() > 0)
    {
        crate::Session::open(ziyuan_storage::EventLog::new(&stage))
            .map_err(|e| invalid(format!("备份事件簿不可回放：{e}")))?;
    }
    let workspace = Workspace::open(&stage).map_err(|e| invalid(e.to_string()))?;
    workspace
        .ensure_writable()
        .map_err(|e| invalid(e.to_string()))?;
    if fs::symlink_metadata(destination).is_ok() {
        return Err(invalid("恢复目标已出现，拒绝覆盖"));
    }
    fs::rename(&stage, destination)?;
    Ok(manifest)
}
