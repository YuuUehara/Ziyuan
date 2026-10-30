use super::{
    Chapter, DEFAULT_BLOCK_BYTES, NewProject, Project, Result, TextProjectError, chunk_paragraphs,
    invalid, split_paragraphs,
};
use ring::{
    digest::{SHA256, digest},
    rand::{SecureRandom, SystemRandom},
};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use ziyuan_storage::JsonFile;

const MANIFEST_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct TextProjects {
    root: PathBuf,
    manifest_bytes: u64,
}
impl TextProjects {
    /// `workspace` is the user's project/workspace directory, not `.ziyuan` itself.
    pub fn new(workspace: impl AsRef<Path>) -> Self {
        Self {
            root: workspace.as_ref().join(".ziyuan/text-projects"),
            manifest_bytes: MANIFEST_BYTES,
        }
    }
    #[doc(hidden)]
    pub fn with_manifest_limit(mut self, bytes: u64) -> Self {
        self.manifest_bytes = bytes;
        self
    }
    pub fn project_dir(&self, id: &str) -> Result<PathBuf> {
        valid_id(id)?;
        Ok(self.root.join(id))
    }
    fn manifest(&self, id: &str) -> Result<JsonFile<Project>> {
        let dir = self.project_dir(id)?;
        if let Ok(m) = fs::symlink_metadata(&dir)
            && (!m.is_dir() || m.file_type().is_symlink())
        {
            return Err(invalid("项目路径必须为普通目录"));
        }
        Ok(JsonFile::new(
            dir.join("project.json"),
            1,
            self.manifest_bytes,
        ))
    }
    pub fn create(&self, metadata: NewProject) -> Result<Project> {
        validate_metadata(&metadata)?;
        fs::create_dir_all(&self.root)?;
        loop {
            let id = random_id()?;
            let dir = self.project_dir(&id)?;
            match fs::create_dir(&dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
            let project = Project {
                id,
                metadata,
                block_bytes: DEFAULT_BLOCK_BYTES,
                chapters: Vec::new(),
            };
            self.manifest(&project.id)?
                .update::<_, TextProjectError>(|old| {
                    if old.is_some() {
                        return Err(invalid("拒绝覆盖已有项目"));
                    }
                    Ok((project.clone(), ()))
                })?;
            return Ok(project);
        }
    }
    pub fn import(&self, id: &str, sources: &[PathBuf]) -> Result<Project> {
        self.import_with_limit(id, sources, DEFAULT_BLOCK_BYTES)
    }
    /// Import once, in user-supplied order. All original and prompt files are staged under a
    /// unique hidden directory. The locked atomic manifest replacement is the only publication.
    /// Failed/crashed staging is intentionally not removed: cleanup never deletes user files.
    pub fn import_with_limit(
        &self,
        id: &str,
        sources: &[PathBuf],
        limit: usize,
    ) -> Result<Project> {
        if sources.is_empty() {
            return Err(invalid("至少指定一个 TXT 文件"));
        }
        let dir = self.project_dir(id)?;
        if !dir.is_dir() {
            return Err(invalid("项目不存在"));
        }
        let (stored, ()) = self
            .manifest(id)?
            .update::<_, TextProjectError>(|current| {
                let mut project = current.ok_or_else(|| invalid("项目不存在"))?.value;
                validate_project(&project, id)?;
                if !project.chapters.is_empty() {
                    return Err(invalid("已有原文，拒绝覆盖或原地更新版本"));
                }
                if limit == 0 || limit > DEFAULT_BLOCK_BYTES {
                    return Err(invalid("无效分块上限"));
                }
                let bundle = format!(".import-{}", random_id()?);
                let mut snapshots = Vec::new();
                for (i, path) in sources.iter().enumerate() {
                    let chapter_id = format!("c{:04}", i + 1);
                    let source = Source::read(path)?;
                    let paragraphs = split_paragraphs(&chapter_id, &source.bytes)?;
                    let blocks = chunk_paragraphs(&chapter_id, &paragraphs, limit)?;
                    project.chapters.push(Chapter {
                        original_path: format!("{bundle}/originals/{chapter_id}.txt"),
                        id: chapter_id,
                        source_name: path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        sha256: sha256(&source.bytes),
                        byte_len: source.bytes.len(),
                        paragraphs,
                        blocks,
                    });
                    snapshots.push(source);
                }
                // Checked before staging so an oversized import leaves no hidden copy behind.
                if estimated_manifest_bytes(&project)? > self.manifest_bytes {
                    return Err(invalid(format!(
                        "这些原文合计太长，项目记录会超过 {} MiB 上限；请按卷或章节范围拆成多个项目分别导入",
                        self.manifest_bytes / (1024 * 1024)
                    )));
                }
                let stage = dir.join(&bundle);
                fs::create_dir(&stage)?;
                fs::create_dir(stage.join("originals"))?;
                fs::create_dir(stage.join("blocks"))?;
                for (chapter, source) in project.chapters.iter().zip(&snapshots) {
                    write_new(&dir.join(&chapter.original_path), &source.bytes)?;
                    for block in &chapter.blocks {
                        write_new(
                            &stage
                                .join("blocks")
                                .join(format!("{}.json", block.block_id)),
                            &block.serialized()?,
                        )?;
                    }
                }
                // Recheck every source, including earlier chapters, immediately before publication.
                for snapshot in &mut snapshots {
                    snapshot.verify()?;
                }
                project.block_bytes = limit;
                Ok((project, ()))
            })?;
        Ok(stored.value)
    }
    /// 全部文本项目，按 ID 排序。每个项目都按 [`show`](Self::show) 校验（原文副本、分块副本与
    /// 记录相符）；校验不过的跳过——一个坏掉的项目不该让整张列表都列不出来。
    /// 尚未建过项目（没有 `.ziyuan/text-projects/`）时为空，不是错误。
    pub fn list(&self) -> Result<Vec<Project>> {
        let mut ids = Vec::new();
        match fs::read_dir(&self.root) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                        continue;
                    };
                    if valid_id(&name).is_err() {
                        continue;
                    }
                    match entry.file_type() {
                        Ok(t) if t.is_dir() && !t.is_symlink() => ids.push(name),
                        _ => {}
                    }
                }
            }
        }
        ids.sort();
        Ok(ids.iter().filter_map(|id| self.show(id).ok()).collect())
    }
    /// Validates stored schema, source hashes, source correspondence and prompt copies.
    pub fn show(&self, id: &str) -> Result<Project> {
        let project = self
            .manifest(id)?
            .load()?
            .ok_or_else(|| invalid("项目不存在"))?
            .value;
        validate_project(&project, id)?;
        let dir = self.project_dir(id)?;
        for (i, chapter) in project.chapters.iter().enumerate() {
            let expected = format!("c{:04}", i + 1);
            let parts: Vec<_> = chapter.original_path.split('/').collect();
            if chapter.id != expected
                || parts.len() != 3
                || !parts[0].starts_with(".import-")
                || valid_id(parts[0].strip_prefix(".import-").unwrap_or("")).is_err()
                || parts[1] != "originals"
                || parts[2] != format!("{expected}.txt")
            {
                return Err(invalid(format!("{expected}: 无效原文路径或章节 ID")));
            }
            require_plain_dir(&dir.join(parts[0]))?;
            require_plain_dir(&dir.join(parts[0]).join("originals"))?;
            require_plain_dir(&dir.join(parts[0]).join("blocks"))?;
            let raw = read_plain(&dir.join(&chapter.original_path))?;
            if sha256(&raw) != chapter.sha256 || raw.len() != chapter.byte_len {
                return Err(TextProjectError::Stale(chapter.id.clone()));
            }
            let paragraphs = split_paragraphs(&expected, &raw)?;
            let blocks = chunk_paragraphs(&expected, &paragraphs, project.block_bytes)?;
            if paragraphs != chapter.paragraphs || blocks != chapter.blocks {
                return Err(TextProjectError::Stale(chapter.id.clone()));
            }
            for block in &blocks {
                let path = dir
                    .join(parts[0])
                    .join("blocks")
                    .join(format!("{}.json", block.block_id));
                if read_plain(&path)? != block.serialized()? {
                    return Err(TextProjectError::Stale(block.block_id.clone()));
                }
            }
        }
        Ok(project)
    }
}

/// Upper bound for the stored envelope: it nests `project` one level deeper (two more spaces
/// per line) and adds a few header fields.
fn estimated_manifest_bytes(project: &Project) -> Result<u64> {
    let body = serde_json::to_vec_pretty(project).map_err(|e| invalid(e.to_string()))?;
    let lines = body.iter().filter(|&&b| b == b'\n').count() + 1;
    Ok((body.len() + lines * 2 + 256) as u64)
}
fn validate_metadata(m: &NewProject) -> Result<()> {
    if m.title.trim().is_empty() {
        return Err(invalid("标题不能为空"));
    }
    if !matches!(m.source_language.as_str(), "ja" | "en") {
        return Err(invalid("原文语言仅支持 ja/en"));
    }
    if m.target_language != "zh-CN" {
        return Err(invalid("目标语言仅支持 zh-CN"));
    }
    Ok(())
}
fn validate_project(p: &Project, id: &str) -> Result<()> {
    if p.id != id {
        return Err(invalid("项目 ID 不匹配"));
    }
    validate_metadata(&p.metadata)?;
    if p.block_bytes == 0 || p.block_bytes > DEFAULT_BLOCK_BYTES {
        return Err(invalid("无效分块上限"));
    }
    Ok(())
}
fn valid_id(id: &str) -> Result<()> {
    if id.len() != 32
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("无效项目或暂存 ID"));
    }
    Ok(())
}
fn random_id() -> Result<String> {
    let mut bytes = [0; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| invalid("无法生成安全随机 ID"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub(super) fn sha256(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
fn require_plain_dir(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err(invalid("项目子目录不是普通目录"));
    }
    Ok(())
}
fn read_plain(path: &Path) -> Result<Vec<u8>> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_file() || m.file_type().is_symlink() {
        return Err(invalid("仅接受普通文件，拒绝符号链接"));
    }
    Ok(fs::read(path)?)
}
struct Source {
    path: PathBuf,
    file: File,
    metadata: fs::Metadata,
    bytes: Vec<u8>,
}
impl Source {
    fn read(path: &Path) -> Result<Self> {
        if !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("txt"))
        {
            return Err(invalid("仅支持 .txt 原文"));
        }
        let before = fs::symlink_metadata(path)?;
        if !before.is_file() || before.file_type().is_symlink() {
            return Err(invalid("仅接受普通文件，拒绝符号链接"));
        }
        let mut file = File::open(path)?;
        let metadata = file.metadata()?;
        if !same_metadata(&before, &metadata) {
            return Err(TextProjectError::SourceChanged(path.display().to_string()));
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let mut source = Self {
            path: path.to_owned(),
            file,
            metadata,
            bytes,
        };
        source.verify()?;
        Ok(source)
    }
    fn verify(&mut self) -> Result<()> {
        use std::io::{Seek, SeekFrom};
        let changed = || TextProjectError::SourceChanged(self.path.display().to_string());
        let now = fs::symlink_metadata(&self.path).map_err(|_| changed())?;
        if now.file_type().is_symlink()
            || !now.is_file()
            || !same_metadata(&self.metadata, &now)
            || !same_metadata(&self.metadata, &self.file.metadata()?)
        {
            return Err(changed());
        }
        self.file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        self.file.read_to_end(&mut bytes)?;
        if bytes != self.bytes
            || read_plain(&self.path).map_err(|_| changed())? != self.bytes
            || !same_metadata(&self.metadata, &self.file.metadata()?)
        {
            return Err(changed());
        }
        Ok(())
    }
}
fn same_metadata(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    let same =
        a.len() == b.len() && a.modified().ok() == b.modified().ok() && a.is_file() == b.is_file();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        same && a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        same
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_is_sorted_and_skips_broken_projects() {
        let workspace = std::env::temp_dir().join(format!("ziyuan-list-{}", random_id().unwrap()));
        fs::create_dir_all(&workspace).unwrap();
        let metadata = || NewProject {
            title: "作品".into(),
            source_language: "ja".into(),
            target_language: "zh-CN".into(),
            provenance: Default::default(),
        };
        let source = workspace.join("c1.txt");
        fs::write(&source, "一。\n\n二。\n").unwrap();
        let texts = TextProjects::new(&workspace);
        let mut ids = Vec::new();
        for _ in 0..3 {
            let project = texts.create(metadata()).unwrap();
            texts
                .import(&project.id, std::slice::from_ref(&source))
                .unwrap();
            ids.push(project.id);
        }
        ids.sort();
        let listed = |texts: &TextProjects| {
            texts
                .list()
                .unwrap()
                .into_iter()
                .map(|p| p.id)
                .collect::<Vec<_>>()
        };
        // 还没有任何项目时为空，不是错误。
        assert!(
            TextProjects::new(workspace.join("empty"))
                .list()
                .unwrap()
                .is_empty()
        );
        assert_eq!(listed(&texts), ids);
        // 原文副本被改动：只跳过这一项，其余照列。
        let broken = &ids[1];
        let chapter = texts.show(broken).unwrap().chapters[0]
            .original_path
            .clone();
        fs::write(texts.project_dir(broken).unwrap().join(chapter), "改过。\n").unwrap();
        assert!(texts.show(broken).is_err());
        assert_eq!(listed(&texts), vec![ids[0].clone(), ids[2].clone()]);
        // 非法目录名与散落文件都不算项目。
        fs::create_dir_all(texts.root.join("not-a-project")).unwrap();
        fs::write(texts.root.join("stray.json"), b"{}").unwrap();
        assert_eq!(listed(&texts), vec![ids[0].clone(), ids[2].clone()]);
        let _ = fs::remove_dir_all(&workspace);
    }

    #[test]
    fn source_snapshot_detects_same_length_change_and_removal() {
        let path = std::env::temp_dir().join(format!("ziyuan-source-{}.txt", random_id().unwrap()));
        fs::write(&path, b"before").unwrap();
        let mut source = Source::read(&path).unwrap();
        fs::write(&path, b"after!").unwrap();
        assert!(matches!(
            source.verify(),
            Err(TextProjectError::SourceChanged(_))
        ));
        drop(source);
        let mut source = Source::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(matches!(
            source.verify(),
            Err(TextProjectError::SourceChanged(_))
        ));
    }
}
