//! N0 预登记自创三章样本，独立验证公开判定接口，不调用模型或生产批准。
//! 固定原文／位置／预期摘自 docs/fixtures/novel-workbench-n0.json；
//! 这里保留副本，测试运行及打包不依赖包外文档。
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use ziyuan_app::text_project::{
    BlockParagraph, Chapter, NewProject, Paragraph, Project, PromptBlock, Provenance, TextProjects,
};
use ziyuan_app::translation::{
    Glossary, GlossarySpeaker, ParagraphAssociations, evaluate_context_visibility,
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

fn n0_project() -> Project {
    Project {
        id: "n0-lantern".into(),
        metadata: metadata(),
        block_bytes: 48 * 1024,
        chapters: CHAPTERS
            .iter()
            .map(|(id, name, rows)| {
                let mut offset = 0;
                let mut paragraphs = Vec::new();
                let mut blocks: Vec<PromptBlock> = Vec::new();
                let mut original = String::new();
                for &(pid, bid, source) in *rows {
                    paragraphs.push(Paragraph {
                        id: pid.into(),
                        byte_start: offset,
                        byte_end: offset + source.len(),
                        text: source.into(),
                        inlines: Vec::new(),
                    });
                    original.push_str(source);
                    original.push_str("\n\n");
                    offset = original.len();
                    if blocks.last().is_none_or(|b| b.block_id != bid) {
                        blocks.push(PromptBlock {
                            schema_version: 1,
                            block_id: bid.into(),
                            paragraphs: Vec::new(),
                        });
                    }
                    blocks.last_mut().unwrap().paragraphs.push(BlockParagraph {
                        id: pid.into(),
                        text: source.into(),
                        inlines: Vec::new(),
                    });
                }
                let hash: String = ring::digest::digest(&ring::digest::SHA256, original.as_bytes())
                    .as_ref()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                Chapter {
                    id: (*id).into(),
                    source_name: (*name).into(),
                    original_path: format!("originals/{id}.txt"),
                    sha256: hash,
                    byte_len: original.len(),
                    paragraphs,
                    blocks,
                    parsing: None,
                }
            })
            .collect(),
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

fn associations(
    paragraph: &str,
    persons: &[&str],
    targets: &[&str],
    speaker: Option<GlossarySpeaker>,
) -> ParagraphAssociations {
    ParagraphAssociations {
        paragraph: paragraph.into(),
        persons: persons.iter().map(|s| (*s).into()).collect(),
        targets: targets.iter().map(|s| (*s).into()).collect(),
        speaker,
    }
}

fn report(data: &Value, block: &str, links: &[ParagraphAssociations]) -> Value {
    let glossary = Glossary::parse(&serde_json::to_vec(data).unwrap()).unwrap();
    serde_json::to_value(
        evaluate_context_visibility(&n0_project(), &glossary, block, links).unwrap(),
    )
    .unwrap()
}

fn ids(report: &Value, layer: &str, kind: &str, field: &str) -> Vec<String> {
    report[layer][kind]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item[field].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn n0_namesakes_and_manual_alias_link_do_not_assign_or_merge_identity() {
    let data = n0_glossary();
    let unlinked = report(&data, "b02", &[]);
    assert!(ids(&unlinked, "applicable", "persons", "person_id").is_empty());
    let linked = report(
        &data,
        "b02",
        &[associations(
            "p03",
            &["ren-captain", "ren-healer"],
            &[],
            None,
        )],
    );
    assert_eq!(
        ids(&linked, "applicable", "persons", "person_id"),
        ["ren-captain", "ren-healer"]
    );
    assert_eq!(
        ids(&linked, "automatic", "persons", "person_id"),
        ["ren-captain", "ren-healer"]
    );
    let before_reveal = report(&data, "b03", &[associations("p04", &["woman"], &[], None)]);
    assert_eq!(
        ids(&before_reveal, "applicable", "persons", "person_id"),
        ["woman"]
    );
    assert!(ids(&before_reveal, "automatic", "persons", "person_id").is_empty());
    let after_reveal = report(&data, "b04", &[associations("p07", &["woman"], &[], None)]);
    assert_eq!(
        ids(&after_reveal, "automatic", "persons", "person_id"),
        ["woman"]
    );
    assert_eq!(data, n0_glossary(), "判定不得自动升级或改写身份");
}

#[test]
fn n0_inclusive_scope_and_block_start_are_distinct_gates() {
    let link = associations(
        "p05",
        &["woman", "brother"],
        &["woman"],
        Some(GlossarySpeaker::Person {
            person_id: "brother".into(),
        }),
    );
    let view = report(&n0_glossary(), "b03", &[link]);
    assert_eq!(
        ids(&view, "applicable", "terms", "term_id"),
        ["ren", "lantern-keeper", "sister"]
    );
    assert_eq!(
        view["applicable"]["address_rules"][0]["paragraphs"],
        json!(["p05"])
    );
    assert_eq!(
        ids(&view, "automatic", "terms", "term_id"),
        ["ren", "lantern-keeper"]
    );
    assert!(
        view["automatic"]["address_rules"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        view["automatic"]["terms"][1]["paragraphs"],
        json!(["p04", "p05", "p06"])
    );
}

#[test]
fn n0_wrong_or_unannotated_speaker_cannot_bypass_a_conditional_term() {
    for speaker in [
        None,
        Some(GlossarySpeaker::Unknown),
        Some(GlossarySpeaker::Narration),
        Some(GlossarySpeaker::Person {
            person_id: "supervisor".into(),
        }),
    ] {
        let view = report(
            &n0_glossary(),
            "b03",
            &[associations(
                "p05",
                &["woman", "brother", "supervisor"],
                &["woman"],
                speaker,
            )],
        );
        assert!(!ids(&view, "applicable", "terms", "term_id").contains(&"sister".into()));
        assert!(
            view["applicable"]["address_rules"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    let outside = report(
        &n0_glossary(),
        "b03",
        &[associations(
            "p06",
            &["woman", "brother"],
            &["woman"],
            Some(GlossarySpeaker::Person {
                person_id: "brother".into(),
            }),
        )],
    );
    assert!(
        outside["applicable"]["address_rules"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut data = n0_glossary();
    data["persons"][0]["address_rules"][0]["speaker"] = json!({"kind":"unknown"});
    let explicit = report(
        &data,
        "b03",
        &[associations(
            "p05",
            &["woman"],
            &["woman"],
            Some(GlossarySpeaker::Unknown),
        )],
    );
    assert_eq!(
        explicit["applicable"]["address_rules"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let absent = report(
        &data,
        "b03",
        &[associations("p05", &["woman"], &["woman"], None)],
    );
    assert!(
        absent["applicable"]["address_rules"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn n0_future_name_and_midblock_tentative_information_stay_out_of_automatic_ids() {
    let early = report(&n0_glossary(), "b01", &[]);
    assert!(ids(&early, "automatic", "terms", "term_id").is_empty());
    let flashback = report(&n0_glossary(), "b04", &[]);
    assert!(!ids(&flashback, "automatic", "terms", "term_id").contains(&"moon-lantern".into()));
    let later = report(&n0_glossary(), "b05", &[]);
    let terms = later["automatic"]["terms"].as_array().unwrap();
    assert_eq!(terms.last().unwrap()["term_id"], "moon-lantern");
    assert_eq!(terms.last().unwrap()["status"], "tentative");
    for view in [early, flashback, later] {
        let text = view.to_string();
        for forbidden in [
            "美緒",
            "美绪",
            "未明身份的女人",
            "自创样本的人工说明",
            "\"allowed\"",
            "\"source\"",
            "\"voice\"",
        ] {
            assert!(
                !text.contains(forbidden),
                "本地报告不应含资料正文：{forbidden}"
            );
        }
    }
}

#[test]
fn conflicting_rules_keep_input_order_and_both_judgment_states() {
    let mut data = n0_glossary();
    let mut term = data["terms"][3].clone();
    term["term_id"] = json!("sister-alternative");
    term["allowed"] = json!(["阿姐"]);
    term["status"] = json!("tentative");
    data["terms"].as_array_mut().unwrap().push(term);
    let mut rule = data["persons"][0]["address_rules"][0].clone();
    rule["term"] = json!("sister-alternative");
    data["persons"][0]["address_rules"]
        .as_array_mut()
        .unwrap()
        .push(rule);
    let view = report(
        &data,
        "b03",
        &[associations(
            "p05",
            &["woman", "brother"],
            &["woman"],
            Some(GlossarySpeaker::Person {
                person_id: "brother".into(),
            }),
        )],
    );
    let rules = view["applicable"]["address_rules"].as_array().unwrap();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0]["rule_index"], 0);
    assert_eq!(rules[1]["rule_index"], 1);
    assert_eq!(rules[0]["term_status"], "confirmed");
    assert_eq!(rules[1]["term_status"], "tentative");
    assert_eq!(
        ids(&view, "applicable", "terms", "term_id"),
        ["ren", "lantern-keeper", "sister", "sister-alternative"]
    );
}

#[test]
fn whole_card_cycles_keep_visible_members_and_propagate_future_dependencies_in_both_orders() {
    for reverse in [false, true] {
        for mode in [
            "visible",
            "future-person",
            "future-evidence",
            "missing-term-origin",
        ] {
            let mut data = n0_glossary();
            let mut a = json!({
                "person_id":"loop-a","name":"循环甲","aliases":["循环甲别名"],"terms":["lantern-keeper"],
                "available_from":position("p04"),"voice":{"description":"仅本地的口吻"},
                "evidence":[],"status":"tentative","address_rules":[{
                    "target":"loop-b","speaker":{"kind":"narration"},"scope":{"kind":"project"},
                    "term":"lantern-keeper","evidence":[]
                }]
            });
            let mut b = a.clone();
            b["person_id"] = json!("loop-b");
            b["name"] = json!("循环乙");
            b["address_rules"][0]["target"] = json!("loop-a");
            if mode == "future-person" {
                b["available_from"] = position("p05");
            }
            if mode == "future-evidence" {
                b["evidence"] = json!([{
                    "paragraph":"p05","quote":"姉さん","explanation":"后段才出现的依据",
                    "stance":"character_belief","status":"tentative"
                }]);
            }
            if mode == "missing-term-origin" {
                data["terms"][1]
                    .as_object_mut()
                    .unwrap()
                    .remove("available_from");
            }
            if reverse {
                std::mem::swap(&mut a, &mut b);
            }
            data["persons"].as_array_mut().unwrap().extend([a, b]);
            let view = report(
                &data,
                "b03",
                &[associations(
                    "p04",
                    &["loop-a", "loop-b"],
                    &["loop-a", "loop-b"],
                    Some(GlossarySpeaker::Narration),
                )],
            );
            let people = ids(&view, "automatic", "persons", "person_id");
            if mode == "visible" {
                assert_eq!(
                    people,
                    if reverse {
                        vec!["loop-b", "loop-a"]
                    } else {
                        vec!["loop-a", "loop-b"]
                    },
                    "{mode}"
                );
                assert_eq!(
                    view["automatic"]["address_rules"].as_array().unwrap().len(),
                    2
                );
            } else {
                assert!(people.is_empty(), "{mode}: {people:?}");
                assert!(
                    view["automatic"]["address_rules"]
                        .as_array()
                        .unwrap()
                        .is_empty(),
                    "{mode}"
                );
                assert!(
                    !ids(&view, "automatic", "terms", "term_id").contains(&"lantern-keeper".into()),
                    "{mode}"
                );
            }
            assert!(
                ids(&view, "automatic", "terms", "term_id").contains(&"ren".into()),
                "整卡失败不得倒过来隐藏独立人名词条"
            );
        }
    }
}

#[test]
fn legacy_scope_and_invalid_hidden_items_are_not_silently_interpreted_or_skipped() {
    let p = n0_project();
    let old =
        Glossary::parse(br#"[{"source":"x","allowed":["y"],"scope":"chapter two"}]"#).unwrap();
    let error = evaluate_context_visibility(&p, &old, "b03", &[])
        .unwrap_err()
        .to_string();
    assert!(error.contains("升级"), "{error}");
    let mut malformed = Glossary::parse(&serde_json::to_vec(&n0_glossary()).unwrap()).unwrap();
    let Glossary::Structured(ref mut data) = malformed else {
        unreachable!()
    };
    data.terms[2].source.clear(); // 未来资料也必须完整校验。
    assert!(evaluate_context_visibility(&p, &malformed, "b01", &[]).is_err());
    let valid = Glossary::parse(&serde_json::to_vec(&n0_glossary()).unwrap()).unwrap();
    assert!(
        evaluate_context_visibility(
            &p,
            &valid,
            "b03",
            &[associations("p05", &["woman", "woman"], &[], None)]
        )
        .is_err()
    );
    assert!(
        evaluate_context_visibility(
            &p,
            &valid,
            "b03",
            &[associations("p07", &["woman"], &[], None)]
        )
        .is_err()
    );
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(base).unwrap().to_owned();
            if path.is_dir() {
                out.insert(relative, None);
                visit(base, &path, out);
            } else {
                out.insert(relative, Some(fs::read(path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

#[test]
fn visibility_reopen_child() {
    let Some(root) = std::env::var_os("ZIYUAN_TEST_VISIBILITY_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let id = fs::read_to_string(root.join("project-id")).unwrap();
    let project = TextProjects::new(&root).show(&id).unwrap();
    let glossary = Glossary::parse(&fs::read(root.join("visibility-input.json")).unwrap()).unwrap();
    let result = evaluate_context_visibility(
        &project,
        &glossary,
        &project.chapters[0].blocks[0].block_id,
        &[],
    )
    .unwrap();
    println!(
        "VISIBILITY_REPORT:{}",
        serde_json::to_string(&result).unwrap()
    );
}

#[test]
fn actual_project_reopens_in_a_fresh_process_and_pure_evaluation_leaves_disk_unchanged() {
    static N: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "ziyuan-visibility-reopen-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&root).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    let texts = TextProjects::new(&root);
    let p = texts.create(metadata()).unwrap();
    let source = root.join("source.txt");
    fs::write(
        &source,
        "皆はその女を「灯守」と呼んだ。\n\n弟は灯守を「姉さん」と呼んだ。\n",
    )
    .unwrap();
    let project = texts.import(&p.id, &[source]).unwrap();
    fs::write(root.join("project-id"), &p.id).unwrap();
    let chapter = &project.chapters[0];
    let data = json!({"format":2,"terms":[{
        "term_id":"lantern-keeper","source":"灯守","allowed":["守灯人"],"category":"common",
        "scope":{"kind":"project"},"available_from":{"chapter":chapter.id,"paragraph":chapter.paragraphs[0].id},
        "evidence":[],"status":"tentative"
    }],"persons":[]});
    let bytes = serde_json::to_vec(&data).unwrap();
    fs::write(root.join("visibility-input.json"), &bytes).unwrap();
    let before = tree(&root);
    let result = evaluate_context_visibility(
        &project,
        &Glossary::parse(&bytes).unwrap(),
        &chapter.blocks[0].block_id,
        &[],
    )
    .unwrap();
    let expected = serde_json::to_value(result).unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "visibility_reopen_child", "--nocapture"])
        .env("ZIYUAN_TEST_VISIBILITY_ROOT", &root)
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let stdout = String::from_utf8(child.stdout).unwrap();
    let value: Value = serde_json::from_str(
        stdout
            .lines()
            .find_map(|line| line.strip_prefix("VISIBILITY_REPORT:"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value, expected);
    assert!(
        tree(&root) == before,
        "真实项目读取及判定不得补文件、锁或目录"
    );
}
