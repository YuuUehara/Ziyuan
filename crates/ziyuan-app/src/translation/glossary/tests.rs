use super::*;
use serde_json::{Value, json};

fn sample() -> Value {
    json!({
        "format":2,
        "terms":[{
            "term_id":"t-1","source":"先生","category":"title","allowed":["先生"],
            "note":"备注","scope":{"kind":"range","from":{"chapter":"z","paragraph":"pz"},
            "to":{"chapter":"a","paragraph":"pa"}},"available_from":{"chapter":"z"},
            "evidence":[evidence()],"status":"confirmed"
        }],
        "persons":[{
            "person_id":"p_1","name":"同名","reading":"よみ","aliases":["别名","同名"],
            "terms":["t-1"],"address_rules":[{
                "target":"p_2","speaker":{"kind":"person","person_id":"p_1"},
                "scope":{"kind":"project"},"term":"t-1","note":"备注","evidence":[evidence()]
            }],"voice":{"description":"庄重","examples":["先生来了"]},
            "evidence":[evidence()],"status":"tentative"
        },{
            "person_id":"p_2","name":"同名","aliases":["别名"],"terms":[],
            "address_rules":[],"voice":{"description":"轻快"},"evidence":[],"status":"confirmed"
        }]
    })
}

fn evidence() -> Value {
    json!({"paragraph":"pz","quote":"先生","explanation":"称呼","stance":"project_fact","status":"confirmed"})
}

fn parse(value: &Value) -> Result<Glossary> {
    Glossary::parse(&serde_json::to_vec(value).unwrap())
}

fn rejects(value: &Value, field: &str) {
    let error = parse(value).unwrap_err().to_string();
    assert!(error.contains(field), "{field}: {error}");
    assert!(
        error
            .chars()
            .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "{error}"
    );
}

fn project() -> Project {
    serde_json::from_value(json!({
        "id":"test","metadata":{"title":"书","source_language":"ja","target_language":"zh",
        "provenance":{"author":"","version":"","source":"","date":"","rights":"","scope":""}},
        "block_bytes":1000,"chapters":[
            {"id":"z","source_name":"","original_path":"","sha256":"","byte_len":0,
            "paragraphs":[
                {"id":"pz","byte_start":0,"byte_end":15,"text":"先生来了。"},
                {"id":"pb","byte_start":15,"byte_end":30,"text":"第二段。"}
            ],"blocks":[]},
            {"id":"a","source_name":"","original_path":"","sha256":"","byte_len":0,
            "paragraphs":[{"id":"pa","byte_start":0,"byte_end":9,"text":"末段。"}],"blocks":[]}
        ]
    }))
    .unwrap()
}

#[test]
fn accepts_all_categories_stances_statuses_and_speakers_without_merging_people() {
    for category in [
        "person_name",
        "place",
        "organization",
        "title",
        "culture",
        "common",
    ] {
        for stance in ["project_fact", "character_belief", "narration"] {
            for status in ["tentative", "confirmed"] {
                for speaker in [
                    json!({"kind":"person","person_id":"p_2"}),
                    json!({"kind":"narration"}),
                    json!({"kind":"unknown"}),
                ] {
                    let mut v = sample();
                    v["terms"][0]["category"] = json!(category);
                    v["terms"][0]["status"] = json!(status);
                    v["terms"][0]["evidence"][0]["stance"] = json!(stance);
                    v["persons"][0]["address_rules"][0]["speaker"] = speaker;
                    let parsed = parse(&v).unwrap();
                    parsed.validate_project(&project()).unwrap();
                    assert_eq!(serde_json::to_value(parsed).unwrap(), v);
                }
            }
        }
    }
}

#[test]
fn rejects_versions_and_missing_required_arrays() {
    for format in [
        json!(null),
        json!("2"),
        json!(2.0),
        json!(-1),
        json!(1),
        json!(true),
    ] {
        let mut v = sample();
        v["format"] = format;
        rejects(&v, "format");
    }
    for format in [json!(3), json!(3.5), json!(18446744073709551615u64)] {
        let mut v = sample();
        v["format"] = format;
        let err = parse(&v).unwrap_err().to_string();
        assert!(err.contains("升级"), "{err}");
    }
    let mut v = sample();
    v.as_object_mut().unwrap().remove("format");
    rejects(&v, "format");
    for pointer in [
        "",
        "/terms/0",
        "/persons/0",
        "/persons/0/address_rules/0",
        "/persons/0/voice",
    ] {
        let original = sample();
        let keys: Vec<_> = original
            .pointer(pointer)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        for key in keys {
            if ["note", "reading", "available_from", "examples"].contains(&key.as_str()) {
                continue;
            }
            let mut v = original.clone();
            v.pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(&key);
            rejects(&v, &key);
        }
    }
    for v in [json!(null), json!(true), json!(2), json!("bad")] {
        rejects(&v, "格式");
    }
}

#[test]
fn rejects_unknown_fields_at_every_nested_object_with_path() {
    for pointer in [
        "",
        "/terms/0",
        "/terms/0/scope",
        "/terms/0/scope/from",
        "/terms/0/available_from",
        "/terms/0/evidence/0",
        "/persons/0",
        "/persons/0/voice",
        "/persons/0/address_rules/0",
        "/persons/0/address_rules/0/speaker",
        "/persons/0/address_rules/0/scope",
        "/persons/0/address_rules/0/evidence/0",
    ] {
        let mut v = sample();
        v.pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("oops".into(), json!(1));
        let err = parse(&v).unwrap_err().to_string();
        assert!(
            err.contains("oops") && err.contains(pointer.replace('/', ".").trim_start_matches('.')),
            "{pointer}: {err}"
        );
    }
    for speaker in [
        json!({"kind":"narration","person_id":"p_1"}),
        json!({"kind":"unknown","person_id":"p_1"}),
    ] {
        let mut v = sample();
        v["persons"][0]["address_rules"][0]["speaker"] = speaker;
        rejects(&v, "speaker");
    }
}

#[test]
fn rejects_duplicate_json_keys_including_tagged_objects() {
    for input in [
        r#"{"format":2,"format":2,"terms":[],"persons":[]}"#,
        r#"{"format":2,"terms":[{"term_id":"a","term_id":"b"}],"persons":[]}"#,
        r#"[{"source":"a","source":"b","allowed":["x"]}]"#,
    ] {
        let err = Glossary::parse(input.as_bytes()).unwrap_err().to_string();
        assert!(err.contains("重复"), "{err}");
    }
    let input = sample()
        .to_string()
        .replace(r#""kind":"person""#, r#""kind":"person","kind":"person""#);
    assert!(
        Glossary::parse(input.as_bytes())
            .unwrap_err()
            .to_string()
            .contains("重复")
    );
}

#[test]
fn rejects_duplicate_ids_and_dangling_references_but_ids_have_separate_namespaces() {
    for array in ["terms", "persons"] {
        let mut v = sample();
        let duplicate = v[array][0].clone();
        v[array].as_array_mut().unwrap().push(duplicate);
        rejects(&v, array);
    }
    for pointer in [
        "/persons/0/terms/0",
        "/persons/0/address_rules/0/target",
        "/persons/0/address_rules/0/speaker/person_id",
        "/persons/0/address_rules/0/term",
    ] {
        let mut v = sample();
        *v.pointer_mut(pointer).unwrap() = json!("missing");
        rejects(&v, pointer.rsplit('/').next().unwrap());
    }
    let mut v = sample();
    v["persons"][1]["person_id"] = json!("t-1");
    v["persons"][0]["address_rules"][0]["target"] = json!("t-1");
    parse(&v).unwrap();
}

#[test]
fn rejects_bad_ids_texts_enums_and_array_limits() {
    for pointer in [
        "/terms/0/term_id",
        "/persons/0/person_id",
        "/persons/0/terms/0",
        "/terms/0/scope/from/chapter",
        "/terms/0/evidence/0/paragraph",
    ] {
        for bad in [
            "".to_owned(),
            "中文".to_owned(),
            "a b".to_owned(),
            "x".repeat(65),
        ] {
            let mut v = sample();
            *v.pointer_mut(pointer).unwrap() = json!(bad);
            rejects(&v, pointer.rsplit('/').next().unwrap());
        }
    }
    for pointer in [
        "/terms/0/source",
        "/terms/0/allowed/0",
        "/persons/0/name",
        "/persons/0/aliases/0",
        "/persons/0/reading",
        "/persons/0/voice/examples/0",
        "/terms/0/evidence/0/quote",
    ] {
        for bad in [" ".to_owned(), "x".repeat(201), "a\u{0}b".to_owned()] {
            let mut v = sample();
            *v.pointer_mut(pointer).unwrap() = json!(bad);
            rejects(&v, pointer.rsplit('/').next().unwrap());
        }
    }
    for pointer in [
        "/terms/0/note",
        "/persons/0/voice/description",
        "/terms/0/evidence/0/explanation",
        "/persons/0/address_rules/0/note",
    ] {
        let mut v = sample();
        *v.pointer_mut(pointer).unwrap() = json!("x".repeat(501));
        rejects(&v, pointer.rsplit('/').next().unwrap());
    }
    for pointer in [
        "/terms/0/category",
        "/terms/0/status",
        "/terms/0/evidence/0/stance",
        "/persons/0/status",
        "/persons/0/address_rules/0/speaker/kind",
        "/terms/0/scope/kind",
    ] {
        let mut v = sample();
        *v.pointer_mut(pointer).unwrap() = json!("bad");
        rejects(&v, pointer.rsplit('/').next().unwrap());
    }
    for pointer in [
        "/terms/0/allowed",
        "/persons/0/aliases",
        "/persons/0/voice/examples",
    ] {
        let mut v = sample();
        *v.pointer_mut(pointer).unwrap() = json!(vec!["a"; 17]);
        rejects(&v, pointer.rsplit('/').next().unwrap());
    }
    let mut v = sample();
    v["terms"][0]["allowed"] = json!([]);
    rejects(&v, "allowed");
    for pointer in [
        "/terms",
        "/persons",
        "/terms/0/evidence",
        "/persons/0/terms",
        "/persons/0/address_rules",
    ] {
        let mut v = sample();
        let first = v.pointer(pointer).unwrap()[0].clone();
        *v.pointer_mut(pointer).unwrap() = Value::Array(vec![first; 5001]);
        rejects(&v, pointer.rsplit('/').next().unwrap());
    }
    let mut v = sample();
    v["terms"][0]["source"] = json!("a\n\tb");
    parse(&v).unwrap();
}

#[test]
fn validates_actual_order_inclusive_whole_chapter_endpoints_and_optional_availability() {
    for (from, to) in [
        (
            json!({"chapter":"z"}),
            json!({"chapter":"z","paragraph":"pz"}),
        ),
        (
            json!({"chapter":"z","paragraph":"pb"}),
            json!({"chapter":"z"}),
        ),
        (
            json!({"chapter":"z","paragraph":"pz"}),
            json!({"chapter":"z","paragraph":"pb"}),
        ),
        (json!({"chapter":"z"}), json!({"chapter":"a"})),
    ] {
        let mut v = sample();
        v["terms"][0]["scope"] = json!({"kind":"range","from":from,"to":to});
        parse(&v).unwrap().validate_project(&project()).unwrap();
    }
    let mut v = sample();
    v["terms"][0]
        .as_object_mut()
        .unwrap()
        .remove("available_from");
    parse(&v).unwrap().validate_project(&project()).unwrap();
    for (from, to) in [
        (json!({"chapter":"a"}), json!({"chapter":"z"})),
        (
            json!({"chapter":"z","paragraph":"pb"}),
            json!({"chapter":"z","paragraph":"pz"}),
        ),
    ] {
        let mut v = sample();
        v["terms"][0]["scope"] = json!({"kind":"range","from":from,"to":to});
        assert!(
            parse(&v)
                .unwrap()
                .validate_project(&project())
                .unwrap_err()
                .to_string()
                .contains("scope")
        );
    }
}

#[test]
fn rejects_unknown_or_foreign_positions_and_mismatching_evidence_everywhere() {
    for pointer in [
        "/terms/0/scope/from",
        "/terms/0/scope/to",
        "/terms/0/available_from",
        "/persons/0/address_rules/0/scope/from",
    ] {
        for position in [
            json!({"chapter":"missing"}),
            json!({"chapter":"z","paragraph":"missing"}),
            json!({"chapter":"z","paragraph":"pa"}),
        ] {
            let mut v = sample();
            v["persons"][0]["address_rules"][0]["scope"] = v["terms"][0]["scope"].clone();
            *v.pointer_mut(pointer).unwrap() = position;
            let err = parse(&v)
                .unwrap()
                .validate_project(&project())
                .unwrap_err()
                .to_string();
            assert!(
                err.contains(pointer.rsplit('/').next().unwrap()),
                "{pointer}: {err}"
            );
        }
    }
    for pointer in [
        "/terms/0/evidence/0",
        "/persons/0/evidence/0",
        "/persons/0/address_rules/0/evidence/0",
    ] {
        for (field, value) in [
            ("paragraph", "missing"),
            ("quote", "不存在"),
            ("paragraph", "pa"),
        ] {
            let mut v = sample();
            v.pointer_mut(pointer).unwrap()[field] = json!(value);
            let err = parse(&v)
                .unwrap()
                .validate_project(&project())
                .unwrap_err()
                .to_string();
            assert!(err.contains("evidence"), "{err}");
        }
    }
    parse(&json!([]))
        .unwrap()
        .validate_project(&project())
        .unwrap();
}

#[test]
fn structured_compatibility_facade_refuses_with_feature_message() {
    let err =
        crate::translation::parse_glossary(br#"{"format":2,"terms":[],"persons":[]}"#).unwrap_err();
    assert_eq!(err.to_string(), "人物／术语资料尚未开放");
}

#[test]
fn person_availability_roundtrips_and_validates_without_changing_omission() {
    let old = sample();
    assert_eq!(serde_json::to_value(parse(&old).unwrap()).unwrap(), old);
    let mut v = old;
    v["persons"][0]["available_from"] = json!({"chapter":"z","paragraph":"pb"});
    let parsed = parse(&v).unwrap();
    parsed.validate_project(&project()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), v);
    for position in [
        json!({"chapter":"missing"}),
        json!({"chapter":"z","paragraph":"pa"}),
    ] {
        v["persons"][0]["available_from"] = position;
        let err = parse(&v).unwrap().validate_project(&project()).unwrap_err();
        assert!(err.to_string().contains("persons.0.available_from"));
    }
    for bad in [json!(null), json!({"chapter":"z","extra":1})] {
        v["persons"][0]["available_from"] = bad;
        rejects(&v, "available_from");
    }
}

fn editable(value: &Value) -> Result<StructuredGlossary> {
    StructuredGlossary::parse_editable(&serde_json::to_vec(value).unwrap())
}

#[test]
fn editable_shape_accepts_empty_but_strict_parse_still_refuses() {
    let mut v = sample();
    for pointer in [
        "/terms/0/term_id",
        "/terms/0/source",
        "/terms/0/note",
        "/terms/0/scope/from/chapter",
        "/terms/0/scope/from/paragraph",
        "/terms/0/available_from/chapter",
        "/terms/0/evidence/0/paragraph",
        "/terms/0/evidence/0/quote",
        "/terms/0/evidence/0/explanation",
        "/persons/0/person_id",
        "/persons/0/name",
        "/persons/0/reading",
        "/persons/0/aliases/0",
        "/persons/0/terms/0",
        "/persons/0/voice/description",
        "/persons/0/voice/examples/0",
        "/persons/0/address_rules/0/target",
        "/persons/0/address_rules/0/term",
        "/persons/0/address_rules/0/speaker/person_id",
        "/persons/0/address_rules/0/note",
    ] {
        let mut single = sample();
        *single.pointer_mut(pointer).unwrap() = json!("");
        assert!(parse(&single).is_err(), "{pointer}");
        assert_eq!(
            serde_json::to_value(editable(&single).unwrap()).unwrap(),
            single
        );
        *v.pointer_mut(pointer).unwrap() = json!("");
    }
    v["terms"][0]["allowed"] = json!([]);
    let saved = editable(&v).unwrap();
    saved.validate_editable_shape().unwrap();
    assert_eq!(serde_json::to_value(saved).unwrap(), v);
    assert!(parse(&v).is_err());
}

#[test]
fn editable_shape_keeps_invalid_ids_and_references() {
    for pointer in [
        "/terms/0/term_id",
        "/persons/0/person_id",
        "/persons/0/terms/0",
        "/persons/0/address_rules/0/target",
        "/persons/0/address_rules/0/term",
        "/persons/0/address_rules/0/speaker/person_id",
        "/terms/0/scope/from/chapter",
        "/terms/0/evidence/0/paragraph",
    ] {
        for id in ["中文 ID", "missing"] {
            let mut v = sample();
            *v.pointer_mut(pointer).unwrap() = json!(id);
            assert_eq!(serde_json::to_value(editable(&v).unwrap()).unwrap(), v);
        }
    }
    for array in ["terms", "persons"] {
        let mut v = sample();
        let duplicate = v[array][0].clone();
        v[array].as_array_mut().unwrap().push(duplicate);
        editable(&v).unwrap();
        assert!(parse(&v).is_err());
    }
    for (pointer, value) in [
        ("/persons/0/terms/0", json!("missing")),
        (
            "/terms/0/scope",
            json!({"kind":"range","from":{"chapter":"a"},"to":{"chapter":"z"}}),
        ),
        ("/terms/0/evidence/0/quote", json!("不存在")),
    ] {
        let mut v = sample();
        *v.pointer_mut(pointer).unwrap() = value;
        editable(&v).unwrap();
        assert!(
            parse(&v)
                .and_then(|g| g.validate_project(&project()))
                .is_err()
        );
    }
}

#[test]
fn editable_shape_rejects_unknown_duplicate_and_overlong_fields() {
    for (pointer, bad) in [
        ("/persons/0/name", json!("字".repeat(201))),
        ("/persons/0/voice/description", json!("字".repeat(501))),
        ("/persons/0/person_id", json!("字".repeat(22))),
        ("/persons/0/name", json!("a\u{0}b")),
        ("/persons/0/person_id", json!("a\u{0}b")),
        ("/terms/0/allowed", json!(vec![""; 17])),
        ("/terms/0/category", json!("invalid")),
        ("/persons/0/voice", json!([])),
        ("/format", json!(2.0)),
        ("/format", json!(3)),
    ] {
        let mut v = sample();
        *v.pointer_mut(pointer).unwrap() = bad;
        assert!(editable(&v).is_err(), "{pointer}");
    }
    let mut v = sample();
    v["persons"][0]["voice"]["unknown"] = json!("");
    assert!(
        editable(&v)
            .unwrap_err()
            .to_string()
            .contains("persons.0.voice.unknown")
    );
    let duplicate = sample()
        .to_string()
        .replace(r#""kind":"person""#, r#""kind":"person","kind":"person""#);
    assert!(
        StructuredGlossary::parse_editable(duplicate.as_bytes())
            .unwrap_err()
            .to_string()
            .contains("persons.0.address_rules.0.speaker.kind 字段重复")
    );
    let mut direct: StructuredGlossary = serde_json::from_value(sample()).unwrap();
    direct.persons[0].name = "x".repeat(201);
    assert!(direct.validate_editable_shape().is_err());
    direct.persons[0].name.clear();
    direct.format = 3;
    assert!(direct.validate_editable_shape().is_err());
}

#[test]
fn production_errors_and_legacy_serialization_stay_fixed() {
    for (bytes, message) in [
        (
            r#"{"format":2,"format":2}"#,
            "glossary.json 格式不对：glossary.json.format 字段重复 at line 1 column 20",
        ),
        (
            r#"{"format":2,"terms":[],"persons":[],"z":1}"#,
            "glossary.json.z 为未知字段",
        ),
        (
            r#"{"format":2.0,"terms":[],"persons":[]}"#,
            "glossary.json.format 格式不对：只接受整数 2",
        ),
        (
            r#"[{"source":"","allowed":[]}]"#,
            "术语表第 1 条的原文不能为空",
        ),
    ] {
        assert_eq!(
            Glossary::parse(bytes.as_bytes()).unwrap_err().to_string(),
            message
        );
    }
    let bytes = br#"[{"source":"a","allowed":["b"],"scope":"legacy","note":"note"}]"#;
    assert_eq!(
        serde_json::to_vec(&Glossary::parse(bytes).unwrap()).unwrap(),
        bytes
    );
}

#[test]
fn strict_json_bounds_container_depth_without_changing_production_boundary() {
    use super::super::strict_json::parse_strict_json;
    for object in [false, true] {
        for layers in [32, 33] {
            let (open, close) = if object {
                (r#"{"x":"#, "}")
            } else {
                ("[", "]")
            };
            let bytes = format!("{}null{}", open.repeat(layers), close.repeat(layers));
            let result = parse_strict_json(bytes.as_bytes(), "资料", Some(32));
            if layers == 32 {
                result.unwrap();
            } else {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("JSON 嵌套至多 32 层")
                );
            }
            parse_strict_json(bytes.as_bytes(), "glossary.json", None).unwrap();
        }
    }
    let bytes = format!("{}null{}", "[".repeat(128), "]".repeat(128));
    assert!(
        parse_strict_json(bytes.as_bytes(), "glossary.json", None)
            .unwrap_err()
            .to_string()
            .contains("recursion limit exceeded")
    );
    for bytes in ["null null", r#"{"x":[{"a":1,"a":2}]}"#] {
        assert!(parse_strict_json(bytes.as_bytes(), "资料", Some(32)).is_err());
    }
    // Production must report the existing unknown-field error, not the new 32-layer bound.
    let nested = format!("{}null{}", "[".repeat(33), "]".repeat(33));
    let bytes = format!(r#"{{"format":2,"terms":[],"persons":[],"extra":{nested}}}"#);
    assert_eq!(
        Glossary::parse(bytes.as_bytes()).unwrap_err().to_string(),
        "glossary.json.extra 为未知字段"
    );
    assert!(
        StructuredGlossary::parse_editable(bytes.as_bytes())
            .unwrap_err()
            .to_string()
            .contains("JSON 嵌套至多 32 层")
    );
}

#[test]
fn deepest_material_and_association_shapes_parse_but_invalid_nodes_never_panic() {
    use super::super::strict_json::parse_strict_json;
    let mut v = sample();
    v["persons"][0]["available_from"] = json!({"chapter":"z","paragraph":"pz"});
    v["persons"][0]["address_rules"][0]["scope"] = v["terms"][0]["scope"].clone();
    let bytes = serde_json::to_vec(&v).unwrap();
    let parsed = editable(&v).unwrap();
    parsed.validate_editable_shape().unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), v);
    // Deepest supported material path: root/persons/person/rules/rule/evidence/item.
    parse_strict_json(&bytes, "资料", Some(7)).unwrap();
    assert!(parse_strict_json(&bytes, "资料", Some(6)).is_err());
    let associations = json!([{
        "paragraph":"pz","persons":["p_1"],
        "speaker":{"kind":"person","person_id":"p_1"}
    }]);
    let bytes = serde_json::to_vec(&associations).unwrap();
    assert_eq!(
        parse_strict_json(&bytes, "关联", Some(32)).unwrap(),
        associations
    );
    let duplicate = br#"[{"speaker":{"kind":"person","person_id":"a","person_id":"b"}}]"#;
    assert!(
        parse_strict_json(duplicate, "关联", Some(32))
            .unwrap_err()
            .to_string()
            .contains("关联.0.speaker.person_id 字段重复")
    );
    for pointer in [
        "",
        "/terms/0",
        "/terms/0/scope",
        "/terms/0/scope/from",
        "/terms/0/evidence/0",
        "/persons/0",
        "/persons/0/voice",
        "/persons/0/address_rules/0",
        "/persons/0/address_rules/0/speaker",
    ] {
        for bad in [json!(null), json!([]), json!(3), json!(""), json!({})] {
            let mut invalid = v.clone();
            *invalid.pointer_mut(pointer).unwrap() = bad;
            assert!(editable(&invalid).is_err(), "{pointer}");
        }
        let mut unknown = v.clone();
        unknown
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), json!(true));
        assert!(editable(&unknown).is_err(), "{pointer}");
    }
}

#[test]
fn editable_resource_bounds_are_inclusive_and_apply_to_nested_arrays() {
    for (pointer, max) in [
        ("/terms/0/source", 200),
        ("/terms/0/allowed/0", 200),
        ("/persons/0/voice/examples/0", 200),
        ("/persons/0/address_rules/0/evidence/0/explanation", 500),
    ] {
        let mut v = sample();
        *v.pointer_mut(pointer).unwrap() = json!("字".repeat(max));
        editable(&v).unwrap().validate_editable_shape().unwrap();
        *v.pointer_mut(pointer).unwrap() = json!("字".repeat(max + 1));
        assert!(editable(&v).is_err(), "{pointer}");
    }
    let mut v = sample();
    v["persons"][0]["person_id"] = json!(format!("{}x", "字".repeat(21)));
    v["persons"][0]["name"] = json!(" \n\t");
    editable(&v).unwrap();
    v["persons"][0]["person_id"] = json!(format!("{}xx", "字".repeat(21)));
    assert!(editable(&v).is_err());
    for (pointer, max) in [
        ("/terms/0/allowed", 16),
        ("/persons/0/aliases", 16),
        ("/persons/0/voice/examples", 16),
        ("/terms", 5000),
        ("/persons", 5000),
        ("/terms/0/evidence", 5000),
        ("/persons/0/terms", 5000),
        ("/persons/0/address_rules", 5000),
        ("/persons/0/evidence", 5000),
        ("/persons/0/address_rules/0/evidence", 5000),
    ] {
        let mut v = sample();
        let item = v.pointer(pointer).unwrap()[0].clone();
        *v.pointer_mut(pointer).unwrap() = Value::Array(vec![item.clone(); max]);
        editable(&v).unwrap().validate_editable_shape().unwrap();
        v.pointer_mut(pointer)
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(item);
        assert!(editable(&v).is_err(), "{pointer}");
        let direct: StructuredGlossary = serde_json::from_value(v).unwrap();
        assert!(direct.validate_editable_shape().is_err(), "{pointer}");
    }
}
