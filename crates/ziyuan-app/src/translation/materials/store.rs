//! Independent unapproved material draft: `.ziyuan/text-projects/<project>/material-draft.json`.
//! Text and request receipts publish together through one `JsonFile` replacement; saving is
//! never approval and emits no events, snapshots or context changes.
use super::{
    MaterialDraft, MaterialError, MaterialIssue, MaterialIssueCode, MaterialPathSegment,
    MaterialReceipt, MaterialSave, check_material_editable,
};
use crate::translation::{
    StructuredGlossary, TranslationError, Translations, check_plain_dirs, ensure_plain_dirs,
    sha256_hex,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::{fs, io};
use ziyuan_storage::{JsonFile, JsonFileError, LockScope, Stored, Workspace};

const DRAFT_FILE: &str = "material-draft.json";
const SCHEMA_VERSION: u32 = 1;
const MAX_BOOK_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedMaterialRequest {
    hash: String,
    receipt: MaterialReceipt,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaterialBook {
    draft: MaterialDraft,
    #[serde(deserialize_with = "unique_requests")]
    requests: BTreeMap<String, SavedMaterialRequest>,
}

/// A plain map would let a later duplicate key silently replace an earlier receipt.
fn unique_requests<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, SavedMaterialRequest>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Unique;
    impl<'de> serde::de::Visitor<'de> for Unique {
        type Value = BTreeMap<String, SavedMaterialRequest>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("资料草稿请求回执表")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some((id, saved)) = map.next_entry::<String, SavedMaterialRequest>()? {
                if out.contains_key(&id) {
                    return Err(serde::de::Error::custom(format!(
                        "资料草稿请求回执重复：{id}"
                    )));
                }
                out.insert(id, saved);
            }
            Ok(out)
        }
    }
    deserializer.deserialize_map(Unique)
}

fn corrupt(message: impl Into<String>) -> MaterialError {
    MaterialError::Storage(JsonFileError::Corrupt(message.into()))
}

fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_revision(text: &str) -> bool {
    is_lower_hex(text, 32)
}

/// Same rule as human draft request IDs; also the only accepted receipt-map keys.
fn is_request_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn check_request_id(id: &str) -> Result<(), MaterialError> {
    if is_request_id(id) {
        return Ok(());
    }
    Err(MaterialIssue {
        path: vec![MaterialPathSegment::Field("request_id".into())],
        code: MaterialIssueCode::Id,
        message: "资料草稿请求身份须为 1–128 位 ASCII 字母数字、- 或 _".into(),
    }
    .into())
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, MaterialError> {
    serde_json::to_vec(value)
        .map_err(|e| MaterialError::Storage(JsonFileError::Serialize(e.to_string())))
}

/// Same digest as `MaterialValidation::content_hash`: compact serialization of the material.
fn content_hash(material: &StructuredGlossary) -> Result<String, MaterialError> {
    Ok(sha256_hex(&encode(material)?))
}

/// Tuple serialization fixes the field order; the material itself is a typed struct.
fn request_hash(project: &str, request: &MaterialSave) -> Result<String, MaterialError> {
    Ok(sha256_hex(&encode(&(
        project,
        &request.expected_revision,
        &request.material,
    ))?))
}

fn new_revision() -> Result<String, MaterialError> {
    let mut random = [0u8; 16];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| io::Error::other("无法生成资料草稿修订身份"))?;
    Ok(random.iter().map(|b| format!("{b:02x}")).collect())
}

impl MaterialBook {
    /// Save-boundary violations mean the file is not something this editor wrote; semantic
    /// problems stay editable content and are not checked here.
    fn check(&self, project: &str) -> Result<(), MaterialError> {
        if self.draft.project != project || !is_revision(&self.draft.revision) {
            return Err(corrupt("资料草稿项目身份或修订形状不符"));
        }
        match check_material_editable(&self.draft.material) {
            Ok(()) => {}
            Err(MaterialError::Issue(issue)) => {
                return Err(corrupt(format!("资料草稿超出保存边界：{issue}")));
            }
            Err(error) => return Err(error),
        }
        for (id, saved) in &self.requests {
            if !is_request_id(id)
                || !is_lower_hex(&saved.hash, 64)
                || !is_revision(&saved.receipt.revision)
                || !is_lower_hex(&saved.receipt.content_hash, 64)
            {
                return Err(corrupt("资料草稿请求回执形状不符"));
            }
        }
        let hash = content_hash(&self.draft.material)?;
        if !self.requests.values().any(|saved| {
            saved.receipt.revision == self.draft.revision && saved.receipt.content_hash == hash
        }) {
            return Err(corrupt("资料草稿现行修订缺少相符回执"));
        }
        Ok(())
    }
}

impl Translations {
    /// Requires an already verified project; checks ancestors, target and lock. Reads use
    /// `check_plain_dirs` and never create a directory, lock or draft.
    fn material_file(
        &self,
        project: &str,
        write: bool,
    ) -> Result<JsonFile<MaterialBook>, MaterialError> {
        self.project(project)?;
        let parts = [".ziyuan", "text-projects", project];
        let dir = if write {
            ensure_plain_dirs(self.root(), &parts)
        } else {
            check_plain_dirs(self.root(), &parts)
        }
        .map_err(|error| match error {
            TranslationError::Invalid(_) => MaterialError::Path(format!(
                "资料草稿目录 .ziyuan/text-projects/{project} 含符号链接或非普通目录，已拒绝"
            )),
            other => other.into(),
        })?;
        let file = JsonFile::new(dir.join(DRAFT_FILE), SCHEMA_VERSION, MAX_BOOK_BYTES);
        for path in [file.path(), file.lock_path()] {
            match fs::symlink_metadata(path) {
                Ok(m) if m.is_file() && !m.file_type().is_symlink() => {}
                Ok(_) => {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    return Err(MaterialError::Path(format!(
                        "资料草稿路径 .ziyuan/text-projects/{project}/{name} 不是普通文件，已拒绝（含符号链接）"
                    )));
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(file)
    }

    fn load_material_book(
        &self,
        file: &JsonFile<MaterialBook>,
        project: &str,
    ) -> Result<Option<Stored<MaterialBook>>, MaterialError> {
        file.load()?
            .map(|stored| {
                stored.value.check(project)?;
                Ok(stored)
            })
            .transpose()
    }

    /// Current unapproved material draft, or `None` before the first explicit save.
    /// Read-only: never takes the workspace lock or creates files.
    pub fn material_draft(&self, project: &str) -> Result<Option<MaterialDraft>, MaterialError> {
        let file = self.material_file(project, false)?;
        Ok(self
            .load_material_book(&file, project)?
            .map(|stored| stored.value.draft))
    }

    /// Save type-valid (possibly semantically unfinished) material. Lock order is always
    /// workspace, then the draft file; the update closure only checks memory.
    pub fn save_material_draft(
        &self,
        project: &str,
        request: &MaterialSave,
    ) -> Result<MaterialReceipt, MaterialError> {
        check_request_id(&request.request_id)?;
        check_material_editable(&request.material)?;
        // Project and path refusals happen before locking so they never create workspace
        // control files; the checks are repeated under the lock.
        self.material_file(project, false)?;
        let workspace = Workspace::open(self.root())?;
        let _guard = workspace.lock(LockScope::Workspace)?;
        workspace.ensure_writable()?;
        let file = self.material_file(project, true)?;
        let current = self.load_material_book(&file, project)?;
        let hash = request_hash(project, request)?;
        // Must return before `update`, which always publishes a new file.
        if let Some(saved) = current
            .as_ref()
            .and_then(|stored| stored.value.requests.get(&request.request_id))
        {
            return if saved.hash == hash {
                Ok(saved.receipt.clone())
            } else {
                Err(MaterialError::RequestConflict)
            };
        }
        let revision = current
            .as_ref()
            .map(|stored| stored.value.draft.revision.clone());
        // The storage counter `update` will publish under (not the random draft revision).
        let publish = current
            .as_ref()
            .map_or(Some(1), |stored| stored.revision.checked_add(1))
            .ok_or_else(|| corrupt("资料草稿发布计数已到上限"))?;
        if request.expected_revision != revision {
            return Err(MaterialError::Conflict { current: revision });
        }
        let draft = MaterialDraft {
            project: project.into(),
            revision: new_revision()?,
            material: request.material.clone(),
        };
        let receipt = MaterialReceipt {
            revision: draft.revision.clone(),
            content_hash: content_hash(&draft.material)?,
        };
        let mut requests = current
            .map(|stored| stored.value.requests)
            .unwrap_or_default();
        requests.insert(
            request.request_id.clone(),
            SavedMaterialRequest {
                hash,
                receipt: receipt.clone(),
            },
        );
        let book = MaterialBook { draft, requests };
        // A book that would fail its own load check must never be published.
        book.check(project)?;
        // Refuse an oversized envelope before `update` could create a missing lock file;
        // `update` re-checks with the same encoder when it publishes.
        file.check_size(publish, &book)?;
        file.update::<_, MaterialError>(|stored| {
            let now = stored.map(|s| s.value.draft.revision);
            if now != revision {
                return Err(MaterialError::Conflict { current: now });
            }
            Ok((book, ()))
        })?;
        Ok(receipt)
    }
}
