//! N0 固定样本的真实磁盘预览；不批准、不调用模型。
use super::support::{Fixture, material, tree};
use serde_json::{Value, json};
use std::fs;
use ziyuan_app::text_project::{NewProject, Project, Provenance, TextProjects};
use ziyuan_app::translation::{
    GlossarySpeaker, MaterialError, MaterialSave, ParagraphAssociations, StructuredGlossary,
    Translations,
};

type SampleParagraph = (&'static str, &'static str, &'static str);
type SampleChapter = (&'static str, &'static str, &'static [SampleParagraph]);
const CHAPTERS: &[SampleChapter] = &[
    (
        "c01",
        "z-first.txt",
        &[
            ("p01", "b01", "港では、名を明かさない女が灯台を見ていた。"),
            ("p02", "b01", "船長の蓮は言った。「今夜は出ない。」"),
            ("p03", "b02", "薬師の蓮は、船長とは別人だった。"),
        ],
    ),
    (
        "c02",
        "a-second.txt",
        &[
            ("p04", "b03", "皆はその女を「灯守」と呼んだ。"),
            ("p05", "b03", "弟は灯守を「姉さん」と呼んだ。"),
            ("p06", "b03", "監督は灯守に言った。「灯守さん、こちらへ。」"),
        ],
    ),
    (
        "c03",
        "m-third.txt",
        &[
            (
                "p07",
                "b04",
                "昨日のことを思い出した。女の本名は美緒だった。",
            ),
            (
                "p08",
                "b04",
                "この町で「月舟」は祭りの灯籠を指す。船ではない。",
            ),
            ("p09", "b05", "蓮は月舟を持って港へ戻った。"),
        ],
    ),
];

fn metadata() -> NewProject {
    NewProject {
        title: "N0 自创三章".into(),
        source_language: "ja".into(),
        target_language: "zh-CN".into(),
        provenance: Provenance::default(),
    }
}

fn position(id: &str) -> Value {
    let chapter = match id {
        "p01" | "p02" | "p03" => "c01",
        "p04" | "p05" | "p06" => "c02",
        "p07" | "p08" | "p09" => "c03",
        _ => panic!("unexpected fixed sample position"),
    };
    json!({"chapter":chapter,"paragraph":id})
}

fn n0_glossary() -> Value {
    let terms = [
        ("ren", "蓮", "莲", "p02", "p09", "confirmed"),
        ("lantern-keeper", "灯守", "守灯人", "p04", "p09", "confirmed"),
        ("mio", "美緒", "美绪", "p07", "p09", "confirmed"),
        ("sister", "姉さん", "姐姐", "p05", "p05", "confirmed"),
        ("moon-lantern", "月舟", "月舟", "p08", "p09", "tentative"),
    ].into_iter().map(|(id, source, allowed, from, to, status)| json!({
        "term_id":id,"source":source,"allowed":[allowed],
        "category":if matches!(id, "ren" | "lantern-keeper" | "mio") { "person_name" } else { "common" },
        "available_from":position(from),
        "scope":{"kind":"range","from":position(from),"to":position(to)},
        "evidence":[],"status":status,
    })).collect::<Vec<_>>();
    let people = [
        (
            "woman",
            "未明身份的女人",
            vec!["lantern-keeper", "mio"],
            "p01",
        ),
        ("ren-captain", "蓮", vec!["ren"], "p02"),
        ("ren-healer", "蓮", vec!["ren"], "p03"),
        ("brother", "弟", vec![], "p05"),
        ("supervisor", "監督", vec![], "p06"),
    ]
    .into_iter()
    .map(|(id, name, terms, from)| {
        json!({
            "person_id":id,"name":name,"aliases":[],"terms":terms,
            "available_from":position(from),"address_rules":[],
            "voice":{"description":"自创样本的人工说明"},"evidence":[],"status":"confirmed",
        })
    })
    .collect::<Vec<_>>();
    let mut result = json!({"format":2,"terms":terms,"persons":people});
    result["persons"][0]["address_rules"] = json!([{
        "target":"woman","speaker":{"kind":"person","person_id":"brother"},
        "scope":{"kind":"range","from":position("p05"),"to":position("p05")},
        "term":"sister","evidence":[],
    }]);
    result
}

/// N0 原文通过真实导入落盘；只将固定章段身份映射到导入身份，预期不由预览生成。
struct N0 {
    disk: Fixture,
    project: Project,
}
impl N0 {
    fn new() -> Self {
        let disk = Fixture::new();
        // 另建空项目，以三个固定文件按 N0 顺序导入；文件名字典序不是章序。
        let texts = TextProjects::new(&disk.root);
        let empty = texts.create(metadata()).unwrap();
        let sources = CHAPTERS
            .iter()
            .map(|(_, name, rows)| {
                let path = disk.root.join(name);
                fs::write(
                    &path,
                    rows.iter()
                        .map(|(_, _, text)| *text)
                        .collect::<Vec<_>>()
                        .join("\n\n"),
                )
                .unwrap();
                path
            })
            .collect::<Vec<_>>();
        let project = texts.import(&empty.id, &sources).unwrap();
        let mut disk = disk;
        disk.project = empty.id;
        Self { disk, project }
    }
    fn service(&self) -> Translations {
        self.disk.service()
    }
    fn paragraph(&self, id: &str) -> String {
        let n: usize = id[1..].parse().unwrap();
        self.project.chapters[(n - 1) / 3].paragraphs[(n - 1) % 3]
            .id
            .clone()
    }
    fn block(&self, id: &str) -> String {
        let pid = self.paragraph(id);
        self.project
            .chapters
            .iter()
            .flat_map(|c| &c.blocks)
            .find(|b| b.paragraphs.iter().any(|p| p.id == pid))
            .unwrap()
            .block_id
            .clone()
    }
    fn data(&self, mut value: Value) -> StructuredGlossary {
        fn bind(value: &mut Value, fixture: &N0) {
            match value {
                Value::Object(fields) => {
                    if let Some(Value::String(chapter)) = fields.get_mut("chapter") {
                        *chapter = fixture.project.chapters
                            [chapter[1..].parse::<usize>().unwrap() - 1]
                            .id
                            .clone();
                    }
                    if let Some(Value::String(paragraph)) = fields.get_mut("paragraph") {
                        *paragraph = fixture.paragraph(paragraph);
                    }
                    for field in fields.values_mut() {
                        bind(field, fixture);
                    }
                }
                Value::Array(items) => {
                    for item in items {
                        bind(item, fixture);
                    }
                }
                _ => {}
            }
        }
        bind(&mut value, self);
        material(value)
    }
    fn link(
        &self,
        paragraph: &str,
        persons: &[&str],
        targets: &[&str],
        speaker: Option<GlossarySpeaker>,
    ) -> ParagraphAssociations {
        ParagraphAssociations {
            paragraph: self.paragraph(paragraph),
            persons: persons.iter().map(|s| s.to_string()).collect(),
            targets: targets.iter().map(|s| s.to_string()).collect(),
            speaker,
        }
    }
}

fn digest<T: serde::Serialize + ?Sized>(value: &T) -> String {
    ring::digest::digest(&ring::digest::SHA256, &serde_json::to_vec(value).unwrap())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn preview_matches_n0_identity_only_expectations() {
    let f = N0::new();
    let data = f.data(n0_glossary());
    let first = f
        .service()
        .preview_material(
            &f.project.id,
            &data,
            &f.block("p01"),
            &[f.link("p01", &["woman"], &[], None)],
        )
        .unwrap();
    assert!(
        first.report.automatic.persons.is_empty(),
        "首章未来人物整卡依赖不得进入 automatic"
    );
    assert!(!first.report.automatic.terms.iter().any(|t| matches!(
        t.term_id.as_str(),
        "mio" | "lantern-keeper" | "sister" | "moon-lantern"
    )));
    let before = f
        .service()
        .preview_material(
            &f.project.id,
            &data,
            &f.block("p04"),
            &[f.link("p04", &["woman"], &[], None)],
        )
        .unwrap();
    assert_eq!(
        before
            .report
            .applicable
            .persons
            .iter()
            .map(|p| p.person_id.as_str())
            .collect::<Vec<_>>(),
        ["woman"]
    );
    assert!(before.report.automatic.persons.is_empty());
    assert_eq!(
        before
            .report
            .applicable
            .terms
            .iter()
            .map(|t| t.term_id.as_str())
            .collect::<Vec<_>>(),
        ["ren", "lantern-keeper"]
    );
    let links = [f.link("p07", &["woman", "ren-captain", "ren-healer"], &[], None)];
    let revealed = f
        .service()
        .preview_material(&f.project.id, &data, &f.block("p07"), &links)
        .unwrap();
    assert_eq!(
        revealed
            .report
            .automatic
            .persons
            .iter()
            .map(|p| p.person_id.as_str())
            .collect::<Vec<_>>(),
        ["woman", "ren-captain", "ren-healer"]
    );
    assert_eq!(
        revealed.report.automatic.persons[0].paragraphs,
        [f.paragraph("p07")]
    );
    assert_eq!(
        revealed.report.automatic.persons[0].status,
        ziyuan_app::translation::GlossaryStatus::Confirmed
    );
    let tentative = revealed
        .report
        .applicable
        .terms
        .iter()
        .find(|term| term.term_id == "moon-lantern")
        .unwrap();
    assert_eq!(
        tentative.status,
        ziyuan_app::translation::GlossaryStatus::Tentative
    );
    assert_eq!(
        tentative.paragraphs,
        [f.paragraph("p08"), f.paragraph("p09")]
    );
    assert!(
        !revealed
            .report
            .automatic
            .terms
            .iter()
            .any(|term| term.term_id == "moon-lantern")
    );
    let value = serde_json::to_value(&revealed).unwrap();
    fn no_body(value: &Value) {
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    assert!(
                        ![
                            "name",
                            "source",
                            "allowed",
                            "aliases",
                            "voice",
                            "evidence",
                            "quote",
                            "text",
                            "description",
                            "note"
                        ]
                        .contains(&key.as_str()),
                        "泄露正文域 {key}"
                    );
                    no_body(value);
                }
            }
            Value::Array(items) => {
                for item in items {
                    no_body(item);
                }
            }
            _ => {}
        }
    }
    no_body(&value);
    let serialized = value.to_string();
    for secret in [
        "未明身份的女人",
        "美緒",
        "美绪",
        "守灯人",
        "自创样本的人工说明",
    ] {
        assert!(!serialized.contains(secret));
    }
    assert_eq!(revealed.binding.content_hash, digest(&data));
    assert_eq!(revealed.binding.project_hash, digest(&f.project));
    assert_eq!(
        revealed.binding.block_hash,
        digest(&f.project.chapters[2].blocks[0])
    );
    assert_eq!(revealed.binding.associations_hash, digest(&links));
}

#[test]
fn invalid_hidden_entry_still_rejects_whole_preview() {
    let f = N0::new();
    let mut empty_allowed = f.data(n0_glossary());
    empty_allowed.terms[2].allowed.clear();
    let mut missing_position = f.data(n0_glossary());
    missing_position.terms[2]
        .available_from
        .as_mut()
        .unwrap()
        .paragraph = Some("missing".into());
    for data in [empty_allowed, missing_position] {
        assert!(
            !f.service()
                .validate_material_input(&f.project.id, &data)
                .unwrap()
                .valid
        );
        assert!(matches!(
            f.service()
                .preview_material(&f.project.id, &data, &f.block("p01"), &[]),
            Err(MaterialError::Issue(_))
        ));
    }
}

#[test]
fn preview_reads_current_project_and_does_not_write() {
    let f = N0::new();
    let data = f.data(n0_glossary());
    let before = tree(&f.disk.root);
    let view = f.service().material_view(&f.project.id).unwrap();
    assert_eq!(view.project, f.project);
    assert_eq!(view.project_hash, digest(&f.project));
    assert!(view.draft.is_none());
    let validation = f
        .service()
        .validate_material_input(&f.project.id, &data)
        .unwrap();
    assert!(validation.valid);
    assert_eq!(validation.project_hash, view.project_hash);
    f.service()
        .preview_material(&f.project.id, &data, &f.block("p01"), &[])
        .unwrap();
    for id in ["missing-project", "../bad"] {
        assert!(f.service().material_view(id).is_err());
        assert!(f.service().validate_material_input(id, &data).is_err());
        assert!(
            f.service()
                .preview_material(id, &data, &f.block("p01"), &[])
                .is_err()
        );
    }
    assert_eq!(tree(&f.disk.root), before);
    f.service()
        .save_material_draft(
            &f.project.id,
            &MaterialSave {
                request_id: "optional-draft".into(),
                expected_revision: None,
                material: data.clone(),
            },
        )
        .unwrap();
    let before = tree(&f.disk.root);
    assert_eq!(
        f.service()
            .material_view(&f.project.id)
            .unwrap()
            .draft
            .unwrap()
            .material,
        data
    );
    assert_eq!(tree(&f.disk.root), before);
    let original = f
        .disk
        .root
        .join(".ziyuan/text-projects")
        .join(&f.project.id)
        .join(&f.project.chapters[0].original_path);
    fs::write(original, "改过的原文").unwrap();
    let before = tree(&f.disk.root);
    assert!(f.service().material_view(&f.project.id).is_err());
    assert!(
        f.service()
            .validate_material_input(&f.project.id, &data)
            .is_err()
    );
    assert!(
        f.service()
            .preview_material(&f.project.id, &data, &f.block("p01"), &[])
            .is_err()
    );
    assert_eq!(tree(&f.disk.root), before);
}

#[test]
fn temporary_associations_are_validated_and_distinguish_speakers() {
    let f = N0::new();
    let data = f.data(n0_glossary());
    for speaker in [
        None,
        Some(GlossarySpeaker::Unknown),
        Some(GlossarySpeaker::Narration),
        Some(GlossarySpeaker::Person {
            person_id: "supervisor".into(),
        }),
    ] {
        let links = [f.link("p05", &["woman", "brother"], &["woman"], speaker)];
        let preview = f
            .service()
            .preview_material(&f.project.id, &data, &f.block("p04"), &links)
            .unwrap();
        assert!(preview.report.applicable.address_rules.is_empty());
        assert!(
            !preview
                .report
                .applicable
                .terms
                .iter()
                .any(|t| t.term_id == "sister")
        );
    }
    let link = f.link(
        "p05",
        &["woman", "brother"],
        &["woman"],
        Some(GlossarySpeaker::Person {
            person_id: "brother".into(),
        }),
    );
    let matched = f
        .service()
        .preview_material(
            &f.project.id,
            &data,
            &f.block("p04"),
            std::slice::from_ref(&link),
        )
        .unwrap();
    assert_eq!(matched.report.applicable.address_rules.len(), 1);
    assert_eq!(matched.report.applicable.address_rules[0].rule_index, 0);
    assert!(matched.report.automatic.address_rules.is_empty());
    for kind in [GlossarySpeaker::Unknown, GlossarySpeaker::Narration] {
        let mut value = n0_glossary();
        value["persons"][0]["address_rules"][0]["speaker"] = serde_json::to_value(&kind).unwrap();
        let data = f.data(value);
        let explicit = f.link("p05", &["woman"], &["woman"], Some(kind));
        assert_eq!(
            f.service()
                .preview_material(&f.project.id, &data, &f.block("p04"), &[explicit])
                .unwrap()
                .report
                .applicable
                .address_rules
                .len(),
            1
        );
    }
    let before = tree(&f.disk.root);
    for links in [
        vec![link.clone(), link.clone()],
        vec![ParagraphAssociations {
            paragraph: "missing".into(),
            ..link.clone()
        }],
        vec![f.link("p01", &[], &[], None)],
        vec![f.link("p05", &["missing"], &[], None)],
        vec![f.link("p05", &["woman", "woman"], &[], None)],
        vec![f.link("p05", &[], &["woman", "woman"], None)],
        vec![f.link("p05", &[], &["missing"], None)],
        vec![f.link(
            "p05",
            &[],
            &[],
            Some(GlossarySpeaker::Person {
                person_id: "missing".into(),
            }),
        )],
    ] {
        assert!(
            f.service()
                .preview_material(&f.project.id, &data, &f.block("p04"), &links)
                .is_err()
        );
    }
    assert!(
        f.service()
            .preview_material(&f.project.id, &data, "missing", &[])
            .is_err()
    );
    assert_eq!(tree(&f.disk.root), before);
}

#[test]
fn whole_card_cycles_propagate_future_dependencies() {
    let f = N0::new();
    for reverse in [false, true] {
        for future in [false, true] {
            let mut value = n0_glossary();
            let a = json!({"person_id":"loop-a","name":"循环甲","aliases":[],"terms":["lantern-keeper"],
                "available_from":position("p04"),"voice":{"description":"本地口吻"},"evidence":[],"status":"tentative",
                "address_rules":[{"target":"loop-b","speaker":{"kind":"narration"},"scope":{"kind":"project"},"term":"lantern-keeper","evidence":[]}]});
            let mut b = a.clone();
            b["person_id"] = json!("loop-b");
            b["address_rules"][0]["target"] = json!("loop-a");
            if future {
                b["available_from"] = position("p05");
            }
            let people = value["persons"].as_array_mut().unwrap();
            if reverse {
                people.extend([b, a]);
            } else {
                people.extend([a, b]);
            }
            let data = f.data(value);
            let links = [f.link(
                "p04",
                &["loop-a", "loop-b"],
                &["loop-a", "loop-b"],
                Some(GlossarySpeaker::Narration),
            )];
            let result = f
                .service()
                .preview_material(&f.project.id, &data, &f.block("p04"), &links)
                .unwrap();
            let ids = result
                .report
                .automatic
                .persons
                .iter()
                .map(|p| p.person_id.as_str())
                .collect::<Vec<_>>();
            if future {
                assert!(ids.is_empty());
            } else if reverse {
                assert_eq!(ids, ["loop-b", "loop-a"]);
            } else {
                assert_eq!(ids, ["loop-a", "loop-b"]);
            }
        }
    }
}
