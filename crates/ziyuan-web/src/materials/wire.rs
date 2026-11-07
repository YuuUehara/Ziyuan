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
    let message = |e: serde_json::Error| format!("资料请求格式不对：{e}");
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    Shape { depth: 0 }
        .deserialize(&mut deserializer)
        .and_then(|()| deserializer.end())
        .map_err(message)?;
    serde_json::from_slice(bytes).map_err(message)
}

/// Structure-only pass: rejects duplicate keys and excessive depth before typed decoding.
struct Shape {
    depth: usize,
}

impl Shape {
    fn child<E: de::Error>(&self) -> Result<Self, E> {
        if self.depth >= MAX_DEPTH {
            return Err(E::custom(format!("JSON 嵌套至多 {MAX_DEPTH} 层")));
        }
        Ok(Self {
            depth: self.depth + 1,
        })
    }
}

impl<'de> DeserializeSeed<'de> for Shape {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Shape {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JSON 值")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let child = self.child::<A::Error>()?;
        while seq
            .next_element_seed(Shape { depth: child.depth })?
            .is_some()
        {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let child = self.child::<A::Error>()?;
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(de::Error::custom(format!("字段 `{key}` 重复")));
            }
            map.next_value_seed(Shape { depth: child.depth })?;
        }
        Ok(())
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
