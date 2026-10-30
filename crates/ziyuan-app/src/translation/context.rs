//! 上下文草稿（`translations/<项目>/context/`）与已批准快照（`.ziyuan/text-projects/<项目>/context/v<N>/`）。
use super::{
    Result, Translations, context_dir_rel, ensure_plain_dirs, invalid, pretty_json, read_plain_opt,
    read_text_opt, sha256_hex,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

pub const GLOSSARY_FILE: &str = "glossary.json";
pub const STYLE_FILE: &str = "style.md";
pub const SUMMARIES_DIR: &str = "summaries";
const MANIFEST_FILE: &str = "manifest.json";

const GLOSSARY_BYTES: usize = 1024 * 1024;
const STYLE_BYTES: usize = 256 * 1024;
const SUMMARY_BYTES: usize = 64 * 1024;
const MAX_ENTRIES: usize = 5000;
const MAX_TERM: usize = 200;
const MAX_ALLOWED: usize = 16;
const MAX_NOTE: usize = 500;

/// 术语表一条：原文、允许译法（至少一种）、可选作用范围与备注。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlossaryEntry {
    pub source: String,
    pub allowed: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    /// 快照内相对路径（`/` 分隔），如 `glossary.json`、`summaries/c0001.md`。
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextManifest {
    pub version: u32,
    pub created: u64,
    pub files: Vec<ManifestFile>,
}

/// 一个已批准、已核对哈希的上下文快照。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ContextSnapshot {
    pub version: u32,
    pub created: u64,
    pub glossary: Vec<GlossaryEntry>,
    pub style: String,
    /// 章 ID → 前情摘要。
    pub summaries: BTreeMap<String, String>,
    pub files: Vec<ManifestFile>,
}

impl ContextSnapshot {
    /// `summaries/<章>.md` 的哈希，按章 ID。
    pub fn summary_hashes(&self) -> BTreeMap<String, String> {
        summary_hashes(&self.files)
    }
}

pub(crate) fn summary_hashes(files: &[ManifestFile]) -> BTreeMap<String, String> {
    files
        .iter()
        .filter_map(|f| {
            let chapter = f
                .path
                .strip_prefix("summaries/")?
                .strip_suffix(".md")?
                .to_owned();
            Some((chapter, f.sha256.clone()))
        })
        .collect()
}

fn check_text(what: &str, text: &str, max: usize) -> Result<()> {
    if text.trim().is_empty() {
        return Err(invalid(format!("{what}不能为空")));
    }
    if text.chars().count() > max {
        return Err(invalid(format!("{what}至多 {max} 字")));
    }
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(invalid(format!("{what}含控制字符")));
    }
    Ok(())
}

/// 校验术语表 JSON：数组，每条原文与各允许译法非空、有长度上限，不得有未知字段。
pub fn parse_glossary(bytes: &[u8]) -> Result<Vec<GlossaryEntry>> {
    let entries: Vec<GlossaryEntry> = serde_json::from_slice(bytes)
        .map_err(|e| invalid(format!("{GLOSSARY_FILE} 格式不对：{e}")))?;
    if entries.len() > MAX_ENTRIES {
        return Err(invalid(format!("术语表至多 {MAX_ENTRIES} 条")));
    }
    for (i, entry) in entries.iter().enumerate() {
        let at = |what: &str| format!("术语表第 {} 条的{what}", i + 1);
        check_text(&at("原文"), &entry.source, MAX_TERM)?;
        if entry.allowed.is_empty() || entry.allowed.len() > MAX_ALLOWED {
            return Err(invalid(format!(
                "{}须为 1–{MAX_ALLOWED} 种",
                at("允许译法")
            )));
        }
        for allowed in &entry.allowed {
            check_text(&at("允许译法"), allowed, MAX_TERM)?;
        }
        if let Some(scope) = &entry.scope {
            check_text(&at("作用范围"), scope, MAX_NOTE)?;
        }
        if let Some(note) = &entry.note {
            check_text(&at("备注"), note, MAX_NOTE)?;
        }
    }
    Ok(entries)
}

/// 草稿中的一份文件：快照内相对路径与字节。
struct DraftFile {
    path: String,
    bytes: Vec<u8>,
}

fn version_of(name: &str) -> Option<u32> {
    let digits = name.strip_prefix('v')?;
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn create_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

impl Translations {
    fn draft_dir(&self, project: &str) -> std::path::PathBuf {
        self.root.join(context_dir_rel(project))
    }

    fn snapshots_dir(&self, project: &str) -> Result<std::path::PathBuf> {
        Ok(self.store_dir(project)?.join("context"))
    }

    /// 草稿缺什么补什么：`glossary.json`（空数组）、`style.md`、`summaries/`。已有的不动。
    pub fn ensure_context_draft(&self, project: &str) -> Result<()> {
        self.project(project)?;
        let dir = ensure_plain_dirs(&self.root, &["translations", project, "context"])?;
        ensure_plain_dirs(&dir, &[SUMMARIES_DIR])?;
        for (name, body) in [
            (GLOSSARY_FILE, "[]\n"),
            (
                STYLE_FILE,
                "# 文体\n\n（在此写明译文的语气、人称、用字等要求。）\n",
            ),
        ] {
            if read_plain_opt(&dir.join(name))?.is_none() {
                match create_new(&dir.join(name), body.as_bytes()) {
                    Err(super::TranslationError::Io(e))
                        if e.kind() == io::ErrorKind::AlreadyExists => {}
                    other => other?,
                }
            }
        }
        Ok(())
    }

    /// 读并校验草稿；缺的术语表视为空，缺的文体视为空串。`chapters` 为本项目的章 ID。
    fn read_draft(&self, project: &str, chapters: &[String]) -> Result<Vec<DraftFile>> {
        let dir = self.draft_dir(project);
        let mut files = Vec::new();
        let glossary = read_text_opt(&dir.join(GLOSSARY_FILE), GLOSSARY_FILE, GLOSSARY_BYTES)?
            .unwrap_or_else(|| "[]\n".into());
        parse_glossary(glossary.as_bytes())?;
        files.push(DraftFile {
            path: GLOSSARY_FILE.into(),
            bytes: glossary.into_bytes(),
        });
        let style =
            read_text_opt(&dir.join(STYLE_FILE), STYLE_FILE, STYLE_BYTES)?.unwrap_or_default();
        files.push(DraftFile {
            path: STYLE_FILE.into(),
            bytes: style.into_bytes(),
        });
        let summaries = dir.join(SUMMARIES_DIR);
        match fs::symlink_metadata(&summaries) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
            Ok(m) if !m.is_dir() || m.file_type().is_symlink() => {
                return Err(invalid("summaries 须为普通目录"));
            }
            Ok(_) => {
                let mut names = Vec::new();
                for entry in fs::read_dir(&summaries)? {
                    let name = entry?.file_name();
                    let name = name
                        .to_str()
                        .ok_or_else(|| invalid("summaries 下有非 UTF-8 文件名"))?
                        .to_owned();
                    names.push(name);
                }
                names.sort();
                for name in names {
                    let chapter = name.strip_suffix(".md").unwrap_or("");
                    if !chapters.iter().any(|c| c == chapter) {
                        return Err(invalid(format!(
                            "summaries/{name}：须为本项目的章 ID 加 .md（如 c0001.md）"
                        )));
                    }
                    let what = format!("summaries/{name}");
                    let text = read_text_opt(&summaries.join(&name), &what, SUMMARY_BYTES)?
                        .ok_or_else(|| invalid(format!("{what} 读取时已不存在")))?;
                    files.push(DraftFile {
                        path: what,
                        bytes: text.into_bytes(),
                    });
                }
            }
        }
        Ok(files)
    }

    /// 批准上下文草稿：校验后存为新快照 `v<N>`（N 递增，从不覆盖），返回版本。
    /// 草稿与现行快照完全相同时不建新版，返回现行版本。
    pub fn approve_context(&self, project: &str) -> Result<u32> {
        let p = self.project(project)?;
        let chapters: Vec<String> = p.chapters.iter().map(|c| c.id.clone()).collect();
        let draft = self.read_draft(project, &chapters)?;
        let files: Vec<ManifestFile> = draft
            .iter()
            .map(|f| ManifestFile {
                path: f.path.clone(),
                sha256: sha256_hex(&f.bytes),
                bytes: f.bytes.len() as u64,
            })
            .collect();
        if let Some(current) = self.current_context(project)?
            && current.files == files
        {
            return Ok(current.version);
        }
        let base = self.snapshots_dir(project)?;
        fs::create_dir_all(&base)?;
        let mut version = self.highest_dir(&base)?.saturating_add(1);
        let dir = loop {
            let dir = base.join(format!("v{version}"));
            match fs::create_dir(&dir) {
                Ok(()) => break dir,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => version += 1,
                Err(e) => return Err(e.into()),
            }
        };
        fs::create_dir(dir.join(SUMMARIES_DIR))?;
        for file in &draft {
            create_new(&dir.join(&file.path), &file.bytes)?;
        }
        // 清单最后写：有清单的快照才算批准完成。
        let manifest = ContextManifest {
            version,
            created: crate::time::now(),
            files,
        };
        create_new(&dir.join(MANIFEST_FILE), &pretty_json(&manifest)?)?;
        Ok(version)
    }

    /// 已有的最大版本目录号（含未写完清单的），没有为 0。
    fn highest_dir(&self, base: &Path) -> Result<u32> {
        let mut highest = 0;
        match fs::read_dir(base) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
            Ok(entries) => {
                for entry in entries {
                    if let Some(v) = entry?.file_name().to_str().and_then(version_of) {
                        highest = highest.max(v);
                    }
                }
            }
        }
        Ok(highest)
    }

    /// 已批准（清单已写成）的版本，升序。
    pub fn context_versions(&self, project: &str) -> Result<Vec<u32>> {
        let base = self.snapshots_dir(project)?;
        let mut versions = Vec::new();
        match fs::read_dir(&base) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if let Some(v) = entry.file_name().to_str().and_then(version_of)
                        && read_plain_opt(&entry.path().join(MANIFEST_FILE))?.is_some()
                    {
                        versions.push(v);
                    }
                }
            }
        }
        versions.sort_unstable();
        Ok(versions)
    }

    /// 某一版的清单（不核对文件哈希）。
    pub fn context_manifest(&self, project: &str, version: u32) -> Result<ContextManifest> {
        let dir = self.snapshots_dir(project)?.join(format!("v{version}"));
        let bytes = read_plain_opt(&dir.join(MANIFEST_FILE))?
            .ok_or_else(|| invalid(format!("上下文 v{version} 不存在或未批准完成")))?;
        let manifest: ContextManifest = serde_json::from_slice(&bytes)
            .map_err(|e| invalid(format!("上下文 v{version} 清单损坏：{e}")))?;
        if manifest.version != version {
            return Err(invalid(format!("上下文 v{version} 清单版本不符")));
        }
        Ok(manifest)
    }

    /// 某一版快照，逐文件核对哈希。
    pub fn context_snapshot(&self, project: &str, version: u32) -> Result<ContextSnapshot> {
        let manifest = self.context_manifest(project, version)?;
        let dir = self.snapshots_dir(project)?.join(format!("v{version}"));
        let mut snapshot = ContextSnapshot {
            version,
            created: manifest.created,
            glossary: Vec::new(),
            style: String::new(),
            summaries: BTreeMap::new(),
            files: manifest.files.clone(),
        };
        for file in &manifest.files {
            let valid_path = file.path == GLOSSARY_FILE
                || file.path == STYLE_FILE
                || file
                    .path
                    .strip_prefix("summaries/")
                    .is_some_and(|n| n.ends_with(".md") && !n.contains(['/', '\\']) && n != ".md");
            if !valid_path || file.path.contains("..") {
                return Err(invalid(format!(
                    "上下文 v{version} 清单含异常路径：{}",
                    file.path
                )));
            }
            let bytes = read_plain_opt(&dir.join(&file.path))?
                .ok_or_else(|| invalid(format!("上下文 v{version} 缺 {}", file.path)))?;
            if sha256_hex(&bytes) != file.sha256 || bytes.len() as u64 != file.bytes {
                return Err(invalid(format!(
                    "上下文 v{version} 的 {} 与清单哈希不符（快照被改动）",
                    file.path
                )));
            }
            let text = String::from_utf8(bytes)
                .map_err(|_| invalid(format!("上下文 v{version} 的 {} 不是 UTF-8", file.path)))?;
            if file.path == GLOSSARY_FILE {
                snapshot.glossary = parse_glossary(text.as_bytes())?;
            } else if file.path == STYLE_FILE {
                snapshot.style = text;
            } else if let Some(chapter) = file
                .path
                .strip_prefix("summaries/")
                .and_then(|n| n.strip_suffix(".md"))
            {
                snapshot.summaries.insert(chapter.to_owned(), text);
            }
        }
        Ok(snapshot)
    }

    /// 现行（最新已批准）的快照；从未批准为 `None`（即版本 0）。
    pub fn current_context(&self, project: &str) -> Result<Option<ContextSnapshot>> {
        match self.context_versions(project)?.last() {
            None => Ok(None),
            Some(&v) => self.context_snapshot(project, v).map(Some),
        }
    }

    /// 现行上下文版本；从未批准为 0。
    pub fn context_version(&self, project: &str) -> Result<u32> {
        Ok(self.context_versions(project)?.last().copied().unwrap_or(0))
    }
}
