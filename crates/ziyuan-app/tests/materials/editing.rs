use super::support::{material, project, sample};
use serde_json::{Value, json};
use ziyuan_app::translation::{
    Glossary, MAX_MATERIAL_BYTES, MaterialDraft, MaterialError, MaterialIssueCode,
    MaterialPathSegment, MaterialReceipt, MaterialSave, check_material_editable, validate_material,
};

fn path(parts: Value) -> Vec<MaterialPathSegment> {
    serde_json::from_value(parts).unwrap()
}

fn issue(value: Value, expected_path: Value, code: MaterialIssueCode) {
    let data = material(value);
    check_material_editable(&data).unwrap();
    let report = validate_material(&project(), &data).unwrap();
    assert!(!report.valid);
    assert_eq!(report.issues.len(), 1);
    assert_eq!(report.issues[0].path, path(expected_path));
    assert_eq!(report.issues[0].code, code);
    let production = Glossary::parse(&serde_json::to_vec(&data).unwrap())
        .and_then(|g| g.validate_project(&project()));
    assert_eq!(
        report.issues[0].message,
        production.unwrap_err().to_string()
    );
}

#[test]
fn full_v2_round_trip_preserves_every_field() {
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
                    let mut value = sample();
                    value["terms"][0]["category"] = json!(category);
                    value["terms"][0]["status"] = json!(status);
                    value["terms"][0]["evidence"][0]["stance"] = json!(stance);
                    value["persons"][0]["address_rules"][0]["speaker"] = speaker;
                    let mut second_term = value["terms"][0].clone();
                    second_term["term_id"] = json!("t-2");
                    second_term["source"] = json!("同名术语");
                    value["terms"]
                        .as_array_mut()
                        .unwrap()
                        .insert(0, second_term);
                    value["persons"][0]["terms"] = json!(["t-2", "t-1"]);
                    let mut second_rule = value["persons"][0]["address_rules"][0].clone();
                    second_rule["term"] = json!("t-2");
                    value["persons"][0]["address_rules"]
                        .as_array_mut()
                        .unwrap()
                        .push(second_rule);
                    let second_evidence = json!({
                        "paragraph":"pb","quote":"第二","explanation":"第二条",
                        "stance":"narration","status":"tentative"
                    });
                    value["persons"][0]["address_rules"][0]["evidence"]
                        .as_array_mut()
                        .unwrap()
                        .push(second_evidence);
                    let data = material(value.clone());
                    check_material_editable(&data).unwrap();
                    let save = MaterialSave {
                        request_id: "request-1".into(),
                        expected_revision: None,
                        material: data.clone(),
                    };
                    let copy: MaterialSave =
                        serde_json::from_slice(&serde_json::to_vec(&save).unwrap()).unwrap();
                    assert_eq!(copy, save);
                    let mut revised = save.clone();
                    revised.expected_revision = Some("prior-revision".into());
                    assert_eq!(
                        serde_json::from_value::<MaterialSave>(
                            serde_json::to_value(&revised).unwrap()
                        )
                        .unwrap(),
                        revised
                    );
                    assert_eq!(serde_json::to_value(&copy.material).unwrap(), value);
                    let draft = MaterialDraft {
                        project: "test".into(),
                        revision: "revision-1".into(),
                        material: data.clone(),
                    };
                    assert_eq!(
                        serde_json::from_value::<MaterialDraft>(
                            serde_json::to_value(&draft).unwrap()
                        )
                        .unwrap(),
                        draft
                    );
                    let report = validate_material(&project(), &data).unwrap();
                    assert!(report.valid, "{report:?}");
                    assert!(report.issues.is_empty());
                    assert_eq!(report.content_hash.len(), 64);
                    assert_eq!(report.project_hash.len(), 64);
                }
            }
        }
    }
    let receipt = MaterialReceipt {
        revision: "r".into(),
        content_hash: "h".into(),
    };
    assert_eq!(
        serde_json::from_value::<MaterialReceipt>(serde_json::to_value(&receipt).unwrap()).unwrap(),
        receipt
    );
    let mut value = sample();
    value["persons"][1]["available_from"] = json!({"chapter":"a"});
    value["persons"][1]["reading"] = json!("べつ");
    value["persons"][1]["voice"]["examples"] = json!([]);
    value["terms"][0].as_object_mut().unwrap().remove("note");
    value["terms"][0]
        .as_object_mut()
        .unwrap()
        .remove("available_from");
    value["persons"][0]["address_rules"][0]
        .as_object_mut()
        .unwrap()
        .remove("note");
    let data = material(value.clone());
    assert_eq!(serde_json::to_value(data).unwrap(), value);
}

#[test]
fn unfinished_material_is_editable_but_not_valid() {
    let mut value = sample();
    value["persons"][0]["name"] = json!("");
    issue(
        value,
        json!(["persons", 0, "name"]),
        MaterialIssueCode::Required,
    );
    let mut value = sample();
    value["terms"][0]["allowed"] = json!([]);
    issue(
        value,
        json!(["terms", 0, "allowed"]),
        MaterialIssueCode::Required,
    );
    let mut value = sample();
    value["terms"][0]["term_id"] = json!("坏 ID");
    issue(value, json!(["terms", 0, "term_id"]), MaterialIssueCode::Id);
    let mut value = sample();
    value["persons"][1]["person_id"] = json!("p_1");
    issue(
        value,
        json!(["persons", 1, "person_id"]),
        MaterialIssueCode::Id,
    );
    let mut value = sample();
    value["persons"][0]["terms"] = json!(["missing"]);
    issue(
        value,
        json!(["persons", 0, "terms", 0]),
        MaterialIssueCode::Reference,
    );
    let mut value = sample();
    value["persons"][0]["available_from"]["chapter"] = json!("missing");
    issue(
        value,
        json!(["persons", 0, "available_from", "chapter"]),
        MaterialIssueCode::Position,
    );
    let mut value = sample();
    value["terms"][0]["scope"]["from"] = json!({"chapter":"a"});
    value["terms"][0]["scope"]["to"] = json!({"chapter":"z"});
    issue(
        value,
        json!(["terms", 0, "scope"]),
        MaterialIssueCode::Position,
    );
    let mut value = sample();
    value["persons"][0]["address_rules"][0]["evidence"][0]["quote"] = json!("不在原文");
    issue(
        value,
        json!(["persons", 0, "address_rules", 0, "evidence", 0, "quote"]),
        MaterialIssueCode::Evidence,
    );
    for (pointer, location, code) in [
        (
            "/persons/0/address_rules/0/target",
            json!(["persons", 0, "address_rules", 0, "target"]),
            MaterialIssueCode::Reference,
        ),
        (
            "/persons/0/address_rules/0/term",
            json!(["persons", 0, "address_rules", 0, "term"]),
            MaterialIssueCode::Reference,
        ),
        (
            "/persons/0/address_rules/0/speaker/person_id",
            json!(["persons", 0, "address_rules", 0, "speaker", "person_id"]),
            MaterialIssueCode::Reference,
        ),
        (
            "/terms/0/scope/from/paragraph",
            json!(["terms", 0, "scope", "from", "paragraph"]),
            MaterialIssueCode::Position,
        ),
        (
            "/persons/0/evidence/0/paragraph",
            json!(["persons", 0, "evidence", 0, "paragraph"]),
            MaterialIssueCode::Evidence,
        ),
    ] {
        let mut value = sample();
        *value.pointer_mut(pointer).unwrap() = json!("missing");
        issue(value, location, code);
    }
    let mut value = sample();
    value["persons"][0]["available_from"] = json!({"chapter":"a","paragraph":"pz"});
    issue(
        value,
        json!(["persons", 0, "available_from", "paragraph"]),
        MaterialIssueCode::Position,
    );
    let mut value = sample();
    value["persons"][0]["person_id"] = json!("");
    issue(
        value,
        json!(["persons", 0, "person_id"]),
        MaterialIssueCode::Id,
    );
    let mut value = sample();
    value["terms"][0]["source"] = json!("   ");
    issue(
        value,
        json!(["terms", 0, "source"]),
        MaterialIssueCode::Required,
    );
}

#[test]
fn diagnostic_wire_shape_and_production_display_remain_stable() {
    for (pointer, replacement, expected) in [
        (
            "/persons/0/name",
            "",
            json!({
                "path":["persons",0,"name"],"code":"required",
                "message":"glossary.json.persons.0.name不能为空"
            }),
        ),
        (
            "/persons/1/person_id",
            "p_1",
            json!({
                "path":["persons",1,"person_id"],"code":"id",
                "message":"persons.1 ID 重复：p_1"
            }),
        ),
        (
            "/persons/0/available_from/chapter",
            "missing",
            json!({
                "path":["persons",0,"available_from","chapter"],"code":"position",
                "message":"persons.0.available_from.chapter 不属于本项目：missing"
            }),
        ),
    ] {
        let mut value = sample();
        *value.pointer_mut(pointer).unwrap() = json!(replacement);
        let data = material(value);
        let report = validate_material(&project(), &data).unwrap();
        assert_eq!(serde_json::to_value(&report.issues[0]).unwrap(), expected);
        assert_eq!(
            Glossary::parse(&serde_json::to_vec(&data).unwrap())
                .and_then(|g| g.validate_project(&project()))
                .unwrap_err()
                .to_string(),
            expected["message"].as_str().unwrap()
        );
    }
    assert_eq!(
        MaterialError::Conflict { current: None }.to_string(),
        "资料草稿修订冲突，现行修订为 None；请保留输入并重新读取"
    );
    assert_eq!(
        MaterialError::RequestConflict.to_string(),
        "资料草稿请求身份已被不同内容使用"
    );
}

#[test]
fn empty_material_is_valid_and_hashes_bind_exact_input() {
    let data = material(json!({"format":2,"terms":[],"persons":[]}));
    check_material_editable(&data).unwrap();
    let first = validate_material(&project(), &data).unwrap();
    assert!(first.valid);
    let mut p = project();
    p.chapters.reverse();
    assert_ne!(
        first.project_hash,
        validate_material(&p, &data).unwrap().project_hash
    );
    assert_ne!(
        first.content_hash,
        validate_material(&project(), &material(sample()))
            .unwrap()
            .content_hash
    );
}

#[test]
fn dto_unknown_fields_are_rejected() {
    let data = sample();
    assert!(
        serde_json::from_value::<MaterialSave>(json!({
            "request_id":"r","expected_revision":null,"material":data,"extra":true
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<MaterialDraft>(json!({
            "project":"p","revision":"r","material":sample(),"extra":true
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<MaterialReceipt>(json!({
            "revision":"r","content_hash":"h","extra":true
        }))
        .is_err()
    );
}

#[test]
fn directly_constructed_material_obeys_shape_and_resource_limits() {
    let mut data = material(sample());
    data.format = 3;
    let MaterialError::Issue(error) = check_material_editable(&data).unwrap_err() else {
        panic!()
    };
    assert_eq!(error.code, MaterialIssueCode::Shape);
    assert_eq!(error.path, path(json!(["format"])));
    for (value, expected) in [
        (json!("字".repeat(201)), json!(["terms", 0, "source"])),
        (json!("bad\u{0}"), json!(["terms", 0, "source"])),
    ] {
        let mut data = sample();
        data["terms"][0]["source"] = value;
        let MaterialError::Issue(error) = check_material_editable(&material(data)).unwrap_err()
        else {
            panic!()
        };
        assert_eq!(error.code, MaterialIssueCode::Limit);
        assert_eq!(error.path, path(expected));
    }
    let mut data = material(sample());
    data.terms[0].term_id = "字".repeat(22);
    let MaterialError::Issue(error) = check_material_editable(&data).unwrap_err() else {
        panic!()
    };
    assert_eq!(error.code, MaterialIssueCode::Limit);
    assert_eq!(error.path, path(json!(["terms", 0, "term_id"])));
}

#[test]
fn compact_utf8_size_boundary_is_inclusive() {
    let mut data = material(sample());
    data.terms[0].note = None;
    let term = data.terms[0].clone();
    data.terms = vec![term; 2500];
    let base = serde_json::to_vec(&data).unwrap().len();
    assert!(base < MAX_MATERIAL_BYTES);
    let mut remaining = MAX_MATERIAL_BYTES - base;
    for term in &mut data.terms {
        // Adding the note field costs ten compact JSON bytes (,"note":"").
        if remaining >= 10 {
            let bytes = (remaining - 10).min(500);
            term.note = Some("x".repeat(bytes));
            remaining -= bytes + 10;
        }
    }
    assert_eq!(remaining, 0);
    assert_eq!(serde_json::to_vec(&data).unwrap().len(), MAX_MATERIAL_BYTES);
    check_material_editable(&data).unwrap();
    let note = data
        .terms
        .iter_mut()
        .find_map(|term| term.note.as_mut().filter(|s| s.len() < 500))
        .unwrap();
    note.push('x');
    let MaterialError::Issue(error) = check_material_editable(&data).unwrap_err() else {
        panic!()
    };
    assert!(error.path.is_empty());
    assert_eq!(error.code, MaterialIssueCode::Limit);
}
