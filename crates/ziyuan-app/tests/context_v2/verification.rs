use super::*;
use serde_json::{Value, json};
use ziyuan_app::translation::visibility::ParagraphAssociations;

fn position(f: &Fx, index: usize) -> Value {
    json!({"chapter": f.project.chapters[0].id,
        "paragraph": f.project.chapters[0].blocks[0].paragraphs[index].id})
}

pub(super) fn association(f: &Fx, index: usize) -> ParagraphAssociations {
    serde_json::from_value(json!({
        "paragraph": f.project.chapters[0].blocks[0].paragraphs[index].id,
        "persons": ["p_1"], "targets": ["p_1"],
        "speaker": {"kind": "person", "person_id": "p_1"}
    }))
    .unwrap()
}

fn prepare(f: &Fx, glossary: &Value, text: &str) {
    let t = f.t();
    t.ensure_context_draft(&f.project.id).unwrap();
    f.write_glossary_only(&glossary.to_string());
    t.approve_context(&f.project.id).unwrap();
    t.export(&f.project.id, f.block()).unwrap();
    let mut target = f.project.chapters[0].blocks[0].clone();
    for paragraph in &mut target.paragraphs {
        paragraph.text = text.into();
    }
    fs::write(
        t.block_dir(&f.project.id, f.block()).join("zh.json"),
        target.serialized().unwrap(),
    )
    .unwrap();
}

#[test]
fn unassociated_people_and_conditional_terms_do_not_produce_hints() {
    let f = Fx::new();
    prepare(&f, &serde_json::from_str(V2_PERSON).unwrap(), "无关译文。");
    assert!(
        f.t()
            .verification(None, &f.project.id, f.block())
            .unwrap()
            .hints
            .is_empty()
    );
}

#[test]
fn associations_and_address_rules_only_apply_to_their_own_paragraph() {
    let f = Fx::with_source("勇者。\n\n勇者。\n");
    prepare(&f, &serde_json::from_str(V2_PERSON).unwrap(), "无关译文。");
    let association = association(&f, 0);
    let view = f
        .t()
        .verification_with_associations(None, &f.project.id, f.block(), &[association])
        .unwrap();
    assert!(view.hints.iter().any(|h| h.message.contains("第二版称谓")));
    assert!(
        view.hints
            .iter()
            .all(|h| h.paragraph == f.project.chapters[0].blocks[0].paragraphs[0].id),
        "{:?}",
        view.hints
    );
}

#[test]
fn term_scope_endpoints_filter_each_paragraph() {
    let f = Fx::with_source("勇者。\n\n勇者。\n");
    let mut glossary: Value = serde_json::from_str(V2_PERSON).unwrap();
    glossary["persons"] = json!([]);
    glossary["terms"][0]["scope"] =
        json!({"kind": "range", "from": position(&f, 1), "to": position(&f, 1)});
    prepare(&f, &glossary, "无关译文。");
    let hints = f
        .t()
        .verification(None, &f.project.id, f.block())
        .unwrap()
        .hints;
    assert_eq!(hints.len(), 1, "{hints:?}");
    assert_eq!(
        hints[0].paragraph,
        f.project.chapters[0].blocks[0].paragraphs[1].id
    );
}

#[test]
fn linked_allowed_translation_does_not_raise_a_person_name_hint() {
    let f = Fx::new();
    let mut glossary: Value = serde_json::from_str(V2_PERSON).unwrap();
    glossary["persons"][0]["terms"] = json!(["t-1"]);
    prepare(&f, &glossary, "勇士出发了。");
    let view = f
        .t()
        .verification_with_associations(None, &f.project.id, f.block(), &[association(&f, 0)])
        .unwrap();
    assert!(view.hints.is_empty(), "{:?}", view.hints);
}

#[test]
fn unrelated_linked_term_cannot_hide_a_missing_person_translation() {
    let f = Fx::with_source("勇者は城へ。\n");
    let mut glossary: Value = serde_json::from_str(V2_PERSON).unwrap();
    glossary["persons"][0]["terms"] = json!(["t-1"]);
    glossary["persons"][0]["address_rules"] = json!([]);
    glossary["terms"][0]["source"] = json!("城");
    glossary["terms"][0]["allowed"] = json!(["城堡"]);
    prepare(&f, &glossary, "去了城堡。");
    let view = f
        .t()
        .verification_with_associations(None, &f.project.id, f.block(), &[association(&f, 0)])
        .unwrap();
    assert!(
        view.hints.iter().any(|h| h.message.contains("第二版人物")),
        "{:?}",
        view.hints
    );
}

#[test]
fn local_retrospective_hint_marks_material_not_visible_at_block_start() {
    let f = Fx::with_source("勇者。\n\n勇者。\n");
    let mut glossary: Value = serde_json::from_str(V2_PERSON).unwrap();
    glossary["persons"] = json!([]);
    glossary["terms"][0]["available_from"] = position(&f, 1);
    prepare(&f, &glossary, "无关译文。");
    let view = f.t().verification(None, &f.project.id, f.block()).unwrap();
    assert_eq!(view.hints.len(), 2);
    assert!(
        view.hints
            .iter()
            .all(|h| h.message.contains("仅供本地回顾"))
    );
}

#[test]
fn qa_uses_the_same_term_scope_and_requires_conditional_associations() {
    let f = Fx::with_source("勇者。\n\n勇者。\n");
    let mut glossary: Value = serde_json::from_str(V2_PERSON).unwrap();
    prepare(&f, &glossary, "无关译文。");
    assert!(
        !f.t()
            .qa(&f.project.id, f.block())
            .unwrap()
            .issues
            .iter()
            .any(|i| i.code == "term_inconsistent")
    );
    glossary["persons"] = json!([]);
    glossary["terms"][0]["scope"] =
        json!({"kind": "range", "from": position(&f, 1), "to": position(&f, 1)});
    prepare(&f, &glossary, "无关译文。");
    let issues = f
        .t()
        .qa(&f.project.id, f.block())
        .unwrap()
        .issues
        .into_iter()
        .filter(|i| i.code == "term_inconsistent")
        .collect::<Vec<_>>();
    assert_eq!(issues.len(), 1);
    assert_eq!(
        issues[0].paragraph,
        f.project.chapters[0].blocks[0].paragraphs[1].id
    );
}

#[test]
fn exact_speaker_and_rule_scope_are_required_and_checks_are_readonly() {
    let f = Fx::with_source("勇者。\n\n勇者。\n");
    let mut glossary: Value = serde_json::from_str(V2_PERSON).unwrap();
    glossary["persons"][0]["address_rules"][0]["scope"] =
        json!({"kind": "range", "from": position(&f, 1), "to": position(&f, 1)});
    prepare(&f, &glossary, "无关译文。");
    let before = tree(&f.root);
    let mut second = association(&f, 1);
    second.speaker = None;
    for speaker in [
        None,
        Some(ziyuan_app::translation::GlossarySpeaker::Unknown),
        Some(ziyuan_app::translation::GlossarySpeaker::Narration),
    ] {
        second.speaker = speaker;
        let view = f
            .t()
            .verification_with_associations(
                None,
                &f.project.id,
                f.block(),
                &[association(&f, 0), second.clone()],
            )
            .unwrap();
        assert!(
            !view
                .hints
                .iter()
                .any(|h| h.message.contains("第二版称谓") || h.message.contains("第二版术语"))
        );
    }
    let view = f
        .t()
        .verification_with_associations(
            None,
            &f.project.id,
            f.block(),
            &[association(&f, 0), association(&f, 1)],
        )
        .unwrap();
    let rules = view
        .hints
        .iter()
        .filter(|h| h.message.contains("第二版称谓"))
        .collect::<Vec<_>>();
    assert_eq!(rules.len(), 1);
    assert_eq!(
        rules[0].paragraph,
        f.project.chapters[0].blocks[0].paragraphs[1].id
    );
    assert_eq!(tree(&f.root), before);
}
