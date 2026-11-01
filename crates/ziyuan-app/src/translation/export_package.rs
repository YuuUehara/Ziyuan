//! Finished-export archives and no-clobber publication to user-selected directories.

use super::finished::ExportBundle;
use super::{Result, invalid};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

impl ExportBundle {
    /// Build a deterministic TAR containing only flat, generated regular-file names.
    pub fn archive(&self) -> Result<Vec<u8>> {
        let mut names = BTreeSet::from(["manifest.json"]);
        for file in &self.files {
            let name = file.name.as_str();
            if name.is_empty()
                || !name.as_bytes()[0].is_ascii_alphanumeric()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
                || !names.insert(name)
            {
                return Err(invalid(format!("不合法或重复的导出文件名：{name:?}")));
            }
        }
        let manifest = serde_json::to_vec_pretty(&self.manifest)?;
        let mut archive = tar::Builder::new(Vec::new());
        for (name, bytes) in self
            .files
            .iter()
            .map(|file| (file.name.as_str(), file.bytes.as_slice()))
            .chain(std::iter::once(("manifest.json", manifest.as_slice())))
        {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_mode(0o644);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_size(bytes.len() as u64);
            header.set_cksum();
            archive.append_data(&mut header, name, bytes)?;
        }
        Ok(archive.into_inner()?)
    }

    /// Publish one complete archive without replacing any existing destination.
    ///
    /// Parents must already be real directories. Like the importer, this portable
    /// implementation assumes a trusted parent filesystem: it does not defend
    /// against a malicious directory swap between validation and publication.
    pub fn write_archive(&self, workspace: &Path, destination: &Path) -> Result<()> {
        reject_parent_components(workspace)?;
        reject_parent_components(destination)?;
        if destination.as_os_str().is_empty()
            || !matches!(
                destination.components().next_back(),
                Some(Component::Normal(_))
            )
        {
            return Err(invalid("导出目标必须是文件路径"));
        }
        let cwd = std::env::current_dir()?;
        let workspace = if workspace.is_absolute() {
            workspace.to_path_buf()
        } else {
            cwd.join(workspace)
        };
        require_plain_directory_chain(&workspace)?;
        let workspace = workspace.canonicalize()?;
        let target = if destination.is_absolute() {
            destination.to_path_buf()
        } else {
            workspace.join(destination)
        };
        let parent = target
            .parent()
            .ok_or_else(|| invalid("导出目标没有父目录"))?;
        require_plain_directory_chain(parent)?;
        let parent = parent.canonicalize()?;
        let name = target
            .file_name()
            .ok_or_else(|| invalid("导出目标没有文件名"))?;
        let target = parent.join(name);
        for control in [".ziyuan", "translations", ".git", ".jj"] {
            let under_control = target
                .strip_prefix(&workspace)
                .ok()
                .and_then(|p| p.components().next())
                .and_then(|c| c.as_os_str().to_str())
                .is_some_and(|c| {
                    if cfg!(windows) {
                        c.eq_ignore_ascii_case(control)
                    } else {
                        c == control
                    }
                });
            if under_control {
                return Err(invalid("不能向工作区控制目录导出"));
            }
        }
        match fs::symlink_metadata(&target) {
            Ok(_) => return Err(invalid("导出目标已存在（不覆盖文件或符号链接）")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }

        // Validate and serialize before creating anything on disk.
        let bytes = self.archive()?;
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let (temporary, mut file) = loop {
            let path = parent.join(format!(
                ".ziyuan-export-{}-{}.tmp",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            // Do not let the temporary name coincide with the requested target.
            if path == target {
                continue;
            }
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => break (TemporaryArchive(path), file),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        };
        let written = file.write_all(&bytes).and_then(|()| file.sync_all());
        drop(file);
        written?;
        // hard_link is the commit point: no rename/copy fallback and no fallible
        // post-publication step that could report failure with a published target.
        fs::hard_link(&temporary.0, &target)?;
        Ok(())
    }
}

fn reject_parent_components(path: &Path) -> Result<()> {
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(invalid("导出路径不允许包含 .."));
    }
    Ok(())
}

fn require_plain_directory_chain(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        // `C:` / `\\?\C:` alone is not an absolute directory; inspect after RootDir.
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        let metadata = fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(invalid(format!("{} 不是普通目录", current.display())));
        }
    }
    Ok(())
}

struct TemporaryArchive(PathBuf);

impl Drop for TemporaryArchive {
    fn drop(&mut self) {
        // Only remove the create_new file owned by this invocation, never target.
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translation::finished::ExportFile;
    use std::io::Read;

    fn bundle() -> ExportBundle {
        ExportBundle {
            files: vec![
                ExportFile {
                    name: "chapter-01.md".into(),
                    bytes: b"first".to_vec(),
                },
                ExportFile {
                    name: "book.txt".into(),
                    bytes: b"second".to_vec(),
                },
            ],
            manifest: serde_json::json!({"warnings": ["example"]}),
            warnings: vec!["example".into()],
        }
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "ziyuan-export-test-{}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }

        fn entries(&self) -> BTreeSet<PathBuf> {
            fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn archive_has_all_files_manifest_and_safe_metadata() {
        let bundle = bundle();
        let bytes = bundle.archive().unwrap();
        assert_eq!(bytes, bundle.archive().unwrap());
        let mut archive = tar::Archive::new(bytes.as_slice());
        let mut actual = std::collections::BTreeMap::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            assert!(entry.header().entry_type().is_file());
            assert_eq!(entry.header().mode().unwrap(), 0o644);
            assert_eq!(entry.header().uid().unwrap(), 0);
            assert_eq!(entry.header().gid().unwrap(), 0);
            assert_eq!(entry.header().mtime().unwrap(), 0);
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            let mut body = Vec::new();
            entry.read_to_end(&mut body).unwrap();
            assert!(actual.insert(name, body).is_none());
        }
        assert_eq!(actual.len(), bundle.files.len() + 1);
        for file in bundle.files {
            assert_eq!(actual[&file.name], file.bytes);
        }
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&actual["manifest.json"]).unwrap(),
            bundle.manifest
        );
    }

    #[test]
    fn invalid_or_duplicate_archive_names_leave_no_files() {
        let fixture = Fixture::new();
        for name in [
            "",
            ".",
            "..",
            "../escape",
            "/absolute",
            "a/b",
            "a\\b",
            "C:drive",
            "非ASCII",
            "a\n",
            "manifest.json",
            "book.txt",
        ] {
            let mut bundle = bundle();
            bundle.files[0].name = name.into();
            assert!(bundle.archive().is_err(), "{name:?}");
            assert!(
                bundle
                    .write_archive(&fixture.0, Path::new("bad.tar"))
                    .is_err()
            );
            assert!(fixture.entries().is_empty());
        }
    }

    #[test]
    fn publish_is_complete_no_clobber_and_allows_outside_workspace() {
        let fixture = Fixture::new();
        let workspace = fixture.0.join("workspace");
        fs::create_dir(&workspace).unwrap();
        let bundle = bundle();
        bundle
            .write_archive(&workspace, Path::new("book.tar"))
            .unwrap();
        assert_eq!(
            fs::read(workspace.join("book.tar")).unwrap(),
            bundle.archive().unwrap()
        );
        let target = fixture.0.join("outside.tar");
        bundle.write_archive(&workspace, &target).unwrap();
        fs::write(&target, b"keep me").unwrap();
        let before = fixture.entries();
        assert!(bundle.write_archive(&workspace, &target).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"keep me");
        assert_eq!(fixture.entries(), before);
        assert_eq!(fs::read_dir(&workspace).unwrap().count(), 1);
    }

    #[test]
    fn concurrent_publish_has_one_winner_and_no_temporary_files() {
        let fixture = Fixture::new();
        let barrier = std::sync::Barrier::new(8);
        let successes = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        bundle()
                            .write_archive(&fixture.0, Path::new("race.tar"))
                            .is_ok()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| usize::from(handle.join().unwrap()))
                .sum::<usize>()
        });
        assert_eq!(successes, 1);
        assert_eq!(
            fixture.entries(),
            BTreeSet::from([fixture.0.join("race.tar")])
        );
        assert_eq!(
            fs::read(fixture.0.join("race.tar")).unwrap(),
            bundle().archive().unwrap()
        );
    }

    #[test]
    fn refuses_parent_traversal_control_paths_and_missing_parents() {
        let fixture = Fixture::new();
        for control in [".ziyuan", "translations", ".git", ".jj"] {
            fs::create_dir(fixture.0.join(control)).unwrap();
            fs::create_dir(fixture.0.join(control).join("nested")).unwrap();
        }
        let before = fixture.entries();
        for destination in [
            "../escape.tar",
            "missing/book.tar",
            ".ziyuan/book.tar",
            "translations/nested/book.tar",
            ".git/book.tar",
            ".jj/nested/book.tar",
            "",
        ] {
            assert!(
                bundle()
                    .write_archive(&fixture.0, Path::new(destination))
                    .is_err(),
                "{destination}"
            );
        }
        for control in [".ziyuan", "translations", ".git", ".jj"] {
            assert!(
                bundle()
                    .write_archive(&fixture.0, &fixture.0.join(control).join("nested/book.tar"))
                    .is_err()
            );
            assert_eq!(
                fs::read_dir(fixture.0.join(control).join("nested"))
                    .unwrap()
                    .count(),
                0
            );
        }
        assert_eq!(fixture.entries(), before);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_parents_workspace_and_targets_including_dangling_links() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let real = fixture.0.join("real");
        fs::create_dir(&real).unwrap();
        symlink(&real, fixture.0.join("alias")).unwrap();
        symlink(fixture.0.join("absent"), fixture.0.join("dangling.tar")).unwrap();
        fs::write(real.join("existing.tar"), b"original").unwrap();
        symlink(real.join("existing.tar"), fixture.0.join("target.tar")).unwrap();
        let before = fixture.entries();
        for target in ["alias/new.tar", "dangling.tar", "target.tar"] {
            assert!(
                bundle()
                    .write_archive(&fixture.0, Path::new(target))
                    .is_err()
            );
        }
        assert!(
            bundle()
                .write_archive(&fixture.0.join("alias"), Path::new("new.tar"))
                .is_err()
        );
        assert_eq!(fixture.entries(), before);
        assert_eq!(fs::read_dir(&real).unwrap().count(), 1);
        assert_eq!(fs::read(real.join("existing.tar")).unwrap(), b"original");
    }
}
