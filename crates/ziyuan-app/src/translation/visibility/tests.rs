use super::*;
use crate::translation::*;
use serde_json::json;

fn project() -> Project {
    serde_json::from_value(json!({
        "id":"book","metadata":{"title":"书","source_language":"ja","target_language":"zh",
        "provenance":{"author":"","version":"","source":"","date":"","rights":"","scope":""}},
        "block_bytes":1000,"chapters":[
            {"id":"z","source_name":"","original_path":"","sha256":"","byte_len":0,
             "paragraphs":[
                 {"id":"pz","byte_start":0,"byte_end":3,"text":"秘密"},
                 {"id":"pm","byte_start":3,"byte_end":6,"text":"秘密"},
                 {"id":"pb","byte_start":6,"byte_end":9,"text":"秘密"}
             ],"blocks":[{"schema_version":1,"block_id":"b","paragraphs":[
                 {"id":"pm","text":"秘密"},{"id":"pb","text":"秘密"}
             ]}]},
            {"id":"a","source_name":"","original_path":"","sha256":"","byte_len":0,
             "paragraphs":[{"id":"pa","byte_start":0,"byte_end":3,"text":"秘密"}],
             "blocks":[{"schema_version":1,"block_id":"later","paragraphs":[{"id":"pa","text":"秘密"}]}]}
        ]
    })).unwrap()
}

fn pos(paragraph: &str) -> GlossaryPosition {
    GlossaryPosition {
        chapter: if paragraph == "pa" { "a" } else { "z" }.into(),
        paragraph: Some(paragraph.into()),
    }
}

fn term(id: &str) -> Term {
    serde_json::from_value(json!({
        "term_id":id,"source":"隐藏原文","category":"person_name","allowed":["隐藏译法"],
        "scope":{"kind":"project"},"available_from":{"chapter":"z","paragraph":"pm"},
        "evidence":[],"status":"tentative"
    }))
    .unwrap()
}

fn person(id: &str) -> Person {
    serde_json::from_value(json!({
        "person_id":id,"name":"同名隐藏姓名","aliases":["隐藏别名"],"terms":[],
        "available_from":{"chapter":"z","paragraph":"pm"},"address_rules":[],
        "voice":{"description":"隐藏口吻","examples":["隐藏例句"]},"evidence":[],"status":"confirmed"
    })).unwrap()
}

fn rule(target: &str, term: &str, speaker: GlossarySpeaker) -> AddressRule {
    AddressRule {
        target: target.into(),
        term: term.into(),
        speaker,
        scope: GlossaryScope::Project,
        note: None,
        evidence: vec![],
    }
}

fn evidence(paragraph: &str) -> GlossaryEvidence {
    serde_json::from_value(json!({
        "paragraph":paragraph,"quote":"秘密","explanation":"隐藏证据",
        "stance":"project_fact","status":"confirmed"
    }))
    .unwrap()
}

fn glossary(terms: Vec<Term>, persons: Vec<Person>) -> Glossary {
    Glossary::Structured(StructuredGlossary {
        format: 2,
        terms,
        persons,
    })
}

fn associations() -> Vec<ParagraphAssociations> {
    ["pb", "pm"]
        .map(|p| ParagraphAssociations {
            paragraph: p.into(),
            persons: vec!["owner".into(), "target".into()],
            targets: vec!["target".into()],
            speaker: Some(GlossarySpeaker::Unknown),
        })
        .to_vec()
}

fn eval(g: &Glossary, a: &[ParagraphAssociations]) -> VisibilityReport {
    evaluate_context_visibility(&project(), g, "b", a).unwrap()
}

#[test]
fn independent_terms_use_actual_order_and_block_start_not_matching_paragraph() {
    let mut exact = term("exact");
    exact.evidence = vec![evidence("pm")];
    let mut mid = term("mid");
    mid.available_from = Some(pos("pb"));
    mid.scope = GlossaryScope::Range {
        from: pos("pb"),
        to: pos("pb"),
    };
    let mut missing = term("missing");
    missing.available_from = None;
    missing.status = GlossaryStatus::Confirmed;
    let mut future_evidence = term("future");
    future_evidence.evidence = vec![evidence("pb")];
    let r = eval(
        &glossary(vec![exact, mid, missing, future_evidence], vec![]),
        &[],
    );
    assert_eq!(r.applicable.terms.len(), 4);
    assert_eq!(r.applicable.terms[1].paragraphs, ["pb"]);
    assert_eq!(
        r.automatic
            .terms
            .iter()
            .map(|t| t.term_id.as_str())
            .collect::<Vec<_>>(),
        ["exact"]
    );
    assert_eq!(r.automatic.terms[0].status, GlossaryStatus::Tentative);
    assert_eq!(r.automatic.terms[0].paragraphs, ["pm", "pb"]);
    for reason in [
        VisibilityReason::MissingFrom,
        VisibilityReason::AfterBlockStart,
        VisibilityReason::FutureEvidence,
    ] {
        assert!(r.diagnostics.iter().any(|d| d.reason == reason));
    }
}

#[test]
fn whole_chapter_availability_starts_before_its_paragraphs() {
    let mut current = term("current_chapter");
    current.available_from = Some(GlossaryPosition {
        chapter: "z".into(),
        paragraph: None,
    });
    let mut later = term("later_chapter");
    later.available_from = Some(GlossaryPosition {
        chapter: "a".into(),
        paragraph: None,
    });
    let r = eval(&glossary(vec![current, later], vec![]), &[]);
    assert_eq!(r.applicable.terms.len(), 2);
    assert_eq!(
        r.automatic
            .terms
            .iter()
            .map(|t| t.term_id.as_str())
            .collect::<Vec<_>>(),
        ["current_chapter"]
    );
    assert!(r.diagnostics.iter().any(|d| {
        d.identity.id == "later_chapter" && d.reason == VisibilityReason::AfterBlockStart
    }));
}

#[test]
fn repeated_rule_paragraph_diagnostics_are_grouped_and_missing_annotations_are_locatable() {
    let mut owner = person("owner");
    owner.address_rules = vec![rule("target", "conditional", GlossarySpeaker::Unknown)];
    let r = eval(
        &glossary(vec![term("conditional")], vec![owner, person("target")]),
        &[],
    );
    let identity = identity(VisibilityKind::AddressRule, "owner", Some(0));
    let missing: Vec<_> = r
        .diagnostics
        .iter()
        .filter(|d| d.identity == identity && d.reason == VisibilityReason::UnassociatedPerson)
        .collect();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].paragraphs, ["pm", "pb"]);
}

#[test]
fn an_unassociated_target_is_accepted_but_diagnosed() {
    let mut owner = person("owner");
    owner.address_rules = vec![rule("target", "conditional", GlossarySpeaker::Unknown)];
    let g = glossary(
        vec![term("conditional")],
        vec![owner, person("target"), person("speaker")],
    );
    let mut a = associations();
    for annotation in &mut a {
        annotation.persons.retain(|id| id != "target");
    }
    let r = eval(&g, &a);
    assert!(r.diagnostics.iter().any(|d| {
        d.identity.kind == VisibilityKind::AddressRule
            && d.reason == VisibilityReason::UnassociatedTarget
            && d.paragraphs == ["pm", "pb"]
    }));
    assert!(r.applicable.address_rules.is_empty());
}

#[test]
fn exact_person_speaker_match_does_not_require_membership_in_associated_persons() {
    let mut owner = person("owner");
    owner.address_rules = vec![rule(
        "target",
        "conditional",
        GlossarySpeaker::Person {
            person_id: "speaker".into(),
        },
    )];
    let g = glossary(
        vec![term("conditional")],
        vec![owner, person("target"), person("speaker")],
    );
    let mut a = associations();
    for annotation in &mut a {
        annotation.speaker = Some(GlossarySpeaker::Person {
            person_id: "speaker".into(),
        });
    }
    let r = eval(&g, &a);
    assert_eq!(r.applicable.address_rules.len(), 1);
    assert_eq!(r.automatic.address_rules.len(), 1);
}

#[test]
fn empty_glossary_produces_an_identity_only_local_report() {
    let r = eval(&glossary(vec![], vec![]), &[]);
    assert_eq!(r.project, "book");
    assert_eq!(r.block, "b");
    assert_eq!(r.applicable, VisibilitySelection::default());
    assert_eq!(r.automatic, VisibilitySelection::default());
    assert!(r.diagnostics.is_empty());
}

#[test]
fn whole_chapter_and_cross_chapter_ranges_are_inclusive_not_lexical() {
    for (from, to, expected) in [
        (pos("pm"), pos("pb"), vec!["pm", "pb"]),
        (pos("pz"), pos("pm"), vec!["pm"]),
        (pos("pb"), pos("pa"), vec!["pb"]),
        (
            GlossaryPosition {
                chapter: "z".into(),
                paragraph: None,
            },
            GlossaryPosition {
                chapter: "z".into(),
                paragraph: None,
            },
            vec!["pm", "pb"],
        ),
        (pos("pa"), pos("pa"), vec![]),
    ] {
        let mut t = term("t");
        t.scope = GlossaryScope::Range { from, to };
        let r = eval(&glossary(vec![t], vec![]), &[]);
        if expected.is_empty() {
            assert!(r.applicable.terms.is_empty());
            assert!(
                r.diagnostics
                    .iter()
                    .any(|d| d.reason == VisibilityReason::OutsideScope)
            );
        } else {
            assert_eq!(r.applicable.terms[0].paragraphs, expected);
            assert_eq!(r.automatic.terms, r.applicable.terms);
        }
    }
}

#[test]
fn rules_intersect_scope_association_target_and_exact_speaker_and_keep_conflicts() {
    let mut owner = person("owner");
    owner.address_rules = vec![
        rule("target", "conditional", GlossarySpeaker::Unknown),
        rule("target", "conditional", GlossarySpeaker::Unknown),
        rule("target", "conditional", GlossarySpeaker::Narration),
        rule(
            "target",
            "conditional",
            GlossarySpeaker::Person {
                person_id: "target".into(),
            },
        ),
    ];
    owner.address_rules[0].scope = GlossaryScope::Range {
        from: pos("pb"),
        to: pos("pa"),
    };
    let mut t = term("conditional");
    t.scope = GlossaryScope::Range {
        from: pos("pm"),
        to: pos("pb"),
    };
    let g = glossary(
        vec![t, term("same-source-independent")],
        vec![owner, person("target")],
    );
    let a = associations();
    let r = eval(&g, &a);
    assert_eq!(
        r.applicable
            .address_rules
            .iter()
            .map(|r| r.rule_index)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(r.applicable.address_rules[0].paragraphs, ["pb"]);
    assert_eq!(r.applicable.address_rules[1].paragraphs, ["pm", "pb"]);
    assert_eq!(r.applicable, r.automatic);
    assert_eq!(
        r.automatic
            .persons
            .iter()
            .map(|p| p.person_id.as_str())
            .collect::<Vec<_>>(),
        ["owner", "target"]
    );
    // Input annotation order never dictates paragraph order.
    assert_eq!(r.automatic.terms[0].paragraphs, ["pm", "pb"]);
    for speaker in [
        None,
        Some(GlossarySpeaker::Narration),
        Some(GlossarySpeaker::Person {
            person_id: "target".into(),
        }),
    ] {
        let mut a = a.clone();
        for annotation in &mut a {
            annotation.speaker = speaker.clone();
        }
        let r = eval(&g, &a);
        if speaker.is_none() {
            assert!(r.automatic.address_rules.is_empty());
            assert_eq!(r.automatic.terms.len(), 1);
            assert!(
                r.diagnostics
                    .iter()
                    .any(|d| d.reason == VisibilityReason::UnannotatedSpeaker)
            );
        } else {
            assert_eq!(r.automatic.address_rules.len(), 1);
        }
    }
    for remove in ["owner", "target"] {
        let mut a = a.clone();
        for annotation in &mut a {
            annotation.persons.retain(|p| p != remove);
        }
        let r = eval(&g, &a);
        assert!(r.applicable.address_rules.is_empty());
        assert_eq!(r.applicable.terms.len(), 1);
    }
    let mut a = a;
    for annotation in &mut a {
        annotation.targets.clear();
    }
    assert!(eval(&g, &a).applicable.address_rules.is_empty());
}

#[test]
fn identity_never_inferred_and_report_contains_no_card_or_evidence_text() {
    let g = glossary(vec![term("name")], vec![person("owner"), person("target")]);
    let r = eval(&g, &[]);
    assert!(r.applicable.persons.is_empty());
    assert_eq!(r.automatic.terms.len(), 1);
    let mut a = associations();
    for annotation in &mut a {
        annotation.persons = vec!["target".into()];
    }
    let r = eval(&g, &a);
    assert_eq!(r.automatic.persons.len(), 1);
    assert_eq!(r.automatic.persons[0].person_id, "target");
    let serialized = serde_json::to_string(&r).unwrap();
    for forbidden in [
        "隐藏原文",
        "隐藏译法",
        "隐藏姓名",
        "隐藏别名",
        "隐藏口吻",
        "隐藏例句",
        "隐藏证据",
        "秘密",
        "\"source\":",
        "\"allowed\":",
        "\"name\":",
        "\"aliases\":",
        "\"voice\":",
        "\"evidence\":",
    ] {
        assert!(!serialized.contains(forbidden), "{serialized}");
    }
    assert_eq!(r, eval(&g, &a));
}

#[test]
fn whole_card_checks_missing_future_own_and_rule_evidence_and_linked_terms() {
    for kind in [
        "from",
        "own_evidence",
        "rule_evidence",
        "linked_term",
        "rule_term",
    ] {
        for at in ["pm", "pb"] {
            let mut owner = person("owner");
            owner.address_rules = vec![rule("target", "conditional", GlossarySpeaker::Unknown)];
            let mut linked = term("linked");
            // A visible term outside scope is a valid card dependency, not an applicable term.
            linked.scope = GlossaryScope::Range {
                from: pos("pa"),
                to: pos("pa"),
            };
            owner.terms = vec!["linked".into()];
            let mut conditional = term("conditional");
            match kind {
                "from" => owner.available_from = Some(pos(at)),
                "own_evidence" => owner.evidence = vec![evidence(at)],
                "rule_evidence" => owner.address_rules[0].evidence = vec![evidence(at)],
                "linked_term" => linked.available_from = Some(pos(at)),
                "rule_term" => conditional.available_from = Some(pos(at)),
                _ => unreachable!(),
            }
            let g = glossary(
                vec![conditional, linked, term("independent")],
                vec![owner, person("target")],
            );
            let r = eval(&g, &associations());
            assert_eq!(r.applicable.persons.len(), 2);
            assert_eq!(
                r.automatic.persons.iter().any(|p| p.person_id == "owner"),
                at == "pm",
                "{kind} {at}"
            );
            assert_eq!(
                r.automatic.address_rules.len(),
                usize::from(at == "pm"),
                "{kind} {at}"
            );
            assert_eq!(
                r.automatic.terms.iter().any(|t| t.term_id == "conditional"),
                at == "pm",
                "{kind} {at}"
            );
            assert!(r.automatic.terms.iter().any(|t| t.term_id == "independent"));
            assert!(!r.applicable.terms.iter().any(|t| t.term_id == "linked"));
        }
    }
    for missing in ["person", "term", "rule_term"] {
        let mut owner = person("owner");
        owner.terms = vec!["linked".into()];
        owner.address_rules = vec![rule("target", "conditional", GlossarySpeaker::Unknown)];
        let mut linked = term("linked");
        let mut conditional = term("conditional");
        match missing {
            "person" => owner.available_from = None,
            "term" => linked.available_from = None,
            "rule_term" => conditional.available_from = None,
            _ => unreachable!(),
        }
        let r = eval(
            &glossary(vec![linked, conditional], vec![owner, person("target")]),
            &associations(),
        );
        assert!(!r.automatic.persons.iter().any(|p| p.person_id == "owner"));
        assert!(r.applicable.persons.iter().any(|p| p.person_id == "owner"));
        assert!(r.automatic.address_rules.is_empty());
    }
}

#[test]
fn whole_card_person_dependencies_use_greatest_viable_subset_and_propagate_failure() {
    for reverse in [false, true] {
        for future in [false, true] {
            let mut owner = person("owner");
            let mut target = person("target");
            let mut speaker = person("speaker");
            owner.address_rules = vec![rule(
                "target",
                "t",
                GlossarySpeaker::Person {
                    person_id: "speaker".into(),
                },
            )];
            target.address_rules = vec![rule("owner", "t", GlossarySpeaker::Unknown)];
            // Dependency is in an inapplicable rule too: scope/association cannot bypass the card gate.
            speaker.address_rules = vec![rule("speaker", "t", GlossarySpeaker::Unknown)];
            if future {
                speaker.available_from = Some(pos("pb"));
            }
            let mut persons = vec![owner, target, speaker];
            if reverse {
                persons.reverse();
            }
            let mut a = associations();
            for annotation in &mut a {
                annotation.persons.push("speaker".into());
                annotation.speaker = Some(GlossarySpeaker::Person {
                    person_id: "speaker".into(),
                });
            }
            let r = eval(&glossary(vec![term("t")], persons.clone()), &a);
            assert_eq!(r.automatic.persons.len(), if future { 0 } else { 3 });
            assert_eq!(r.automatic.address_rules.len(), usize::from(!future));
            if !future {
                assert_eq!(
                    r.automatic
                        .persons
                        .iter()
                        .map(|p| p.person_id.as_str())
                        .collect::<Vec<_>>(),
                    persons
                        .iter()
                        .map(|p| p.person_id.as_str())
                        .collect::<Vec<_>>()
                );
            } else {
                assert!(
                    r.diagnostics
                        .iter()
                        .any(|d| d.reason == VisibilityReason::InvisibleDependency)
                );
                assert!(r.automatic.terms.is_empty());
                assert_eq!(r.applicable.terms.len(), 1);
            }
        }
    }
}

fn rejects(
    project: &Project,
    glossary: &Glossary,
    associations: &[ParagraphAssociations],
    field: &str,
) {
    let e = evaluate_context_visibility(project, glossary, "b", associations)
        .unwrap_err()
        .to_string();
    assert!(e.contains(field), "{field}: {e}");
    assert!(
        e.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "{e}"
    );
}

#[test]
fn complete_validation_rejects_invalid_direct_values_even_when_hidden() {
    let valid = glossary(vec![term("t")], vec![person("owner"), person("target")]);
    let bytes = serde_json::to_value(&valid).unwrap();
    for (pointer, bad, field) in [
        ("/format", json!(1), "format"),
        ("/terms/0/term_id", json!("bad id"), "term_id"),
        ("/terms/0/allowed", json!([]), "allowed"),
        ("/terms/0/source", json!(""), "source"),
        (
            "/terms/0/available_from",
            json!({"chapter":"missing"}),
            "available_from",
        ),
        (
            "/terms/0/scope",
            json!({"kind":"range","from":{"chapter":"a"},"to":{"chapter":"z"}}),
            "scope",
        ),
        (
            "/persons/0/available_from",
            json!({"chapter":"z","paragraph":"pa"}),
            "available_from",
        ),
        ("/persons/0/name", json!(""), "name"),
        ("/persons/0/terms", json!(["missing"]), "terms"),
        (
            "/persons/0/evidence",
            json!([{"paragraph":"pa","quote":"不存在","explanation":"说明","stance":"narration","status":"tentative"}]),
            "evidence",
        ),
    ] {
        let mut v = bytes.clone();
        *v.pointer_mut(pointer).unwrap() = bad;
        let g = Glossary::Structured(serde_json::from_value(v).unwrap());
        rejects(&project(), &g, &[], field);
    }
    for key in ["terms", "persons"] {
        let mut v = bytes.clone();
        let dup = v[key][0].clone();
        v[key].as_array_mut().unwrap().push(dup);
        rejects(
            &project(),
            &Glossary::Structured(serde_json::from_value(v).unwrap()),
            &[],
            key,
        );
    }
    let mut p = person("owner");
    p.address_rules = vec![rule("missing", "t", GlossarySpeaker::Unknown)];
    rejects(
        &project(),
        &glossary(vec![term("t")], vec![p]),
        &[],
        "target",
    );
    let e = evaluate_context_visibility(&project(), &Glossary::Legacy(vec![]), "b", &[])
        .unwrap_err()
        .to_string();
    assert!(e.contains("旧词表须人工确认并显式升级"));
}

#[test]
fn associations_reject_duplicates_unknown_people_paragraphs_and_foreign_paragraphs() {
    let g = glossary(vec![], vec![person("owner"), person("target")]);
    let valid = associations();
    for kind in [
        "duplicate_paragraph",
        "foreign",
        "unknown_paragraph",
        "duplicate_person",
        "duplicate_target",
        "unknown_person",
        "unknown_target",
        "unknown_speaker",
    ] {
        let mut a = valid.clone();
        let field = match kind {
            "duplicate_paragraph" => {
                a.push(a[0].clone());
                "paragraph"
            }
            "foreign" => {
                a[0].paragraph = "pa".into();
                "paragraph"
            }
            "unknown_paragraph" => {
                a[0].paragraph = "missing".into();
                "paragraph"
            }
            "duplicate_person" => {
                a[0].persons.push("owner".into());
                "persons"
            }
            "duplicate_target" => {
                a[0].targets.push("target".into());
                "targets"
            }
            "unknown_person" => {
                a[0].persons.push("missing".into());
                "persons"
            }
            "unknown_target" => {
                a[0].targets.push("missing".into());
                "targets"
            }
            "unknown_speaker" => {
                a[0].speaker = Some(GlossarySpeaker::Person {
                    person_id: "missing".into(),
                });
                "speaker"
            }
            _ => unreachable!(),
        };
        rejects(&project(), &g, &a, field);
    }
}

#[test]
fn project_structure_and_all_block_references_are_validated_before_filtering() {
    let g = glossary(vec![], vec![]);
    for kind in [
        "chapter",
        "paragraph",
        "block",
        "copy",
        "order",
        "foreign",
        "empty",
        "unknown",
        "version",
        "duplicate_reference",
    ] {
        let mut p = project();
        match kind {
            "chapter" => p.chapters[1].id = "z".into(),
            "paragraph" => p.chapters[1].paragraphs[0].id = "pm".into(),
            "block" => p.chapters[1].blocks[0].block_id = "b".into(),
            "copy" => p.chapters[1].blocks[0].paragraphs[0].text = "changed".into(),
            "order" => p.chapters[0].blocks[0].paragraphs.reverse(),
            "foreign" => {
                p.chapters[0].blocks[0].paragraphs[0] =
                    p.chapters[1].blocks[0].paragraphs[0].clone()
            }
            "empty" => p.chapters[0].blocks[0].paragraphs.clear(),
            "unknown" => p.chapters[0].blocks[0].paragraphs[0].id = "missing".into(),
            "version" => p.chapters[0].blocks[0].schema_version = 2,
            "duplicate_reference" => {
                let dup = p.chapters[0].blocks[0].paragraphs[0].clone();
                p.chapters[0].blocks[0].paragraphs.push(dup);
            }
            _ => unreachable!(),
        }
        assert!(
            evaluate_context_visibility(&p, &g, "b", &[]).is_err(),
            "{kind}"
        );
    }
    assert!(evaluate_context_visibility(&project(), &g, "missing", &[]).is_err());
    // Serialize before/after the pure call: no mutation of project or glossary.
    let p = project();
    let before = serde_json::to_value((&p, &g)).unwrap();
    evaluate_context_visibility(&p, &g, "b", &[]).unwrap();
    assert_eq!(before, serde_json::to_value((&p, &g)).unwrap());
}
