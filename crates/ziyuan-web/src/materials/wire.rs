//! Strict request DTOs decoded from raw bytes, so duplicate keys are never silently dropped.
use serde::Deserialize;
use serde::de::{self, DeserializeOwned, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::collections::HashSet;
use std::fmt;
use ziyuan_app::translation::materials::MaterialSave;
use ziyuan_app::translation::{GlossarySpeaker, ParagraphAssociations, StructuredGlossary};

/// Root containers count as layer 1; scalars add no layer.
pub(crate) const MAX_DEPTH: usize = 32;

pub(crate) trait Bound {
    fn workspace_id(&self) -> &str;
}

pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    // serde diagnostics can quote arbitrary input keys, strings and enum tags.
    // Keep only location metadata, never the diagnostic itself.
    let message = |e: serde_json::Error| {
        format!(
            "资料请求格式不对（第 {} 行、第 {} 列）；请检查字段、类型及嵌套层数",
            e.line(),
            e.column()
        )
    };
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    Shape::new(0)
        .deserialize(&mut deserializer)
        .and_then(|_| deserializer.end())
        .map_err(message)?;
    serde_json::from_slice(bytes).map_err(message)
}

/// Tags of internally tagged variants without fields (`GlossaryScope::Project`,
/// `GlossarySpeaker::{Narration, Unknown}`). serde ignores extra keys on such unit
/// variants even under `deny_unknown_fields`, so they are refused here instead. Every
/// object carrying `kind` in these request bodies is one of those tagged enums.
const TAG_ONLY_KINDS: &[&str] = &["project", "narration", "unknown"];

/// Structure-only pass: rejects duplicate keys, excessive depth and extra keys on
/// tag-only variants before typed decoding. Only `kind` values are captured.
struct Shape {
    depth: usize,
    capture: bool,
}

impl Shape {
    fn new(depth: usize) -> Self {
        Self {
            depth,
            capture: false,
        }
    }

    fn child<E: de::Error>(&self) -> Result<usize, E> {
        if self.depth >= MAX_DEPTH {
            return Err(E::custom(format!("JSON 嵌套至多 {MAX_DEPTH} 层")));
        }
        Ok(self.depth + 1)
    }
}

impl<'de> DeserializeSeed<'de> for Shape {
    type Value = Option<String>;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Option<String>, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Shape {
    type Value = Option<String>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JSON 值")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        Ok(self.capture.then(|| v.to_owned()))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let depth = self.child::<A::Error>()?;
        while seq.next_element_seed(Shape::new(depth))?.is_some() {}
        Ok(None)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let depth = self.child::<A::Error>()?;
        let mut keys = HashSet::new();
        let mut kind = None;
        while let Some(key) = map.next_key::<String>()? {
            if keys.contains(&key) {
                return Err(de::Error::custom(format!("字段 `{key}` 重复")));
            }
            let capture = key == "kind";
            let value = map.next_value_seed(Shape { depth, capture })?;
            if capture {
                kind = value;
            }
            keys.insert(key);
        }
        if let Some(kind) = kind
            && TAG_ONLY_KINDS.contains(&kind.as_str())
            && keys.len() > 1
        {
            return Err(de::Error::custom(format!(
                "`kind` 为 `{kind}` 的对象不能有其他字段"
            )));
        }
        Ok(None)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ValidateBody {
    workspace_id: String,
    pub(crate) material: StructuredGlossary,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SaveBody {
    workspace_id: String,
    request_id: String,
    expected_revision: Option<String>,
    material: StructuredGlossary,
}

impl SaveBody {
    pub(crate) fn into_save(self) -> MaterialSave {
        MaterialSave {
            request_id: self.request_id,
            expected_revision: self.expected_revision,
            material: self.material,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreviewBody {
    workspace_id: String,
    pub(crate) material: StructuredGlossary,
    pub(crate) block: String,
    associations: Vec<AssociationDto>,
}

impl PreviewBody {
    pub(crate) fn associations(&mut self) -> Vec<ParagraphAssociations> {
        std::mem::take(&mut self.associations)
            .into_iter()
            .map(AssociationDto::into_public)
            .collect()
    }
}

/// The public `ParagraphAssociations` only derives `Serialize`; this is its strict input form.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssociationDto {
    paragraph: String,
    persons: Vec<String>,
    targets: Vec<String>,
    #[serde(default)]
    speaker: Option<SpeakerDto>,
}

/// Empty struct variants so `deny_unknown_fields` also rejects extra keys on tag-only kinds.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SpeakerDto {
    Person { person_id: String },
    Narration {},
    Unknown {},
}

impl AssociationDto {
    fn into_public(self) -> ParagraphAssociations {
        ParagraphAssociations {
            paragraph: self.paragraph,
            persons: self.persons,
            targets: self.targets,
            speaker: self.speaker.map(|speaker| match speaker {
                SpeakerDto::Person { person_id } => GlossarySpeaker::Person { person_id },
                SpeakerDto::Narration {} => GlossarySpeaker::Narration,
                SpeakerDto::Unknown {} => GlossarySpeaker::Unknown,
            }),
        }
    }
}

impl Bound for ValidateBody {
    fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
}
impl Bound for SaveBody {
    fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
}
impl Bound for PreviewBody {
    fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
}
