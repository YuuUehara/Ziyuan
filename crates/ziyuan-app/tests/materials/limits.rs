use super::support::{
    Fixture, current, draft_bytes, material, receipt_count, sample, save_with, tree, unfinished,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use ziyuan_app::translation::{
    MAX_MATERIAL_BYTES, MaterialDraft, MaterialError, MaterialIssueCode, MaterialPathSegment,
    MaterialReceipt, Person, StructuredGlossary,
};
use ziyuan_storage::JsonFileError;

const MAX_ENVELOPE_BYTES: u64 = 8 * 1024 * 1024;

fn min_term() -> Value {
    json!({"term_id":"","source":"","category":"common","allowed":[],
        "scope":{"kind":"project"},"evidence":[],"status":"tentative"})
}

fn min_person() -> Value {
    json!({"person_id":"","name":"","aliases":[],"terms":[],"address_rules":[],
        "voice":{"description":""},"evidence":[],"status":"tentative"})
}

fn min_rule() -> Value {
    json!({"target":"","speaker":{"kind":"unknown"},"scope":{"kind":"project"},"term":"","evidence":[]})
}

fn min_evidence() -> Value {
    json!({"paragraph":"","quote":"","explanation":"","stance":"narration","status":"tentative"})
}

fn many(item: Value, n: usize) -> Value {
    Value::Array(vec![item; n])
}

fn compact_len(data: &StructuredGlossary) -> usize {
    serde_json::to_vec(data).unwrap().len()
}

fn path(parts: Value) -> Vec<MaterialPathSegment> {
    serde_json::from_value(parts).unwrap()
}

fn assert_limit(result: Result<MaterialReceipt, MaterialError>, expected: &[MaterialPathSegment]) {
    match result {
        Err(MaterialError::Issue(issue)) => {
            assert_eq!(issue.code, MaterialIssueCode::Limit, "{issue:?}");
            assert_eq!(issue.path, expected, "{issue:?}");
        }
        other => panic!("应报保存边界上限问题，实际：{other:?}"),
    }
}

#[test]
fn every_text_item_and_id_limit_is_inclusive_at_save() {
    let ascii64 = "a".repeat(64);
    let cases: Vec<(&str, Value, Value, Value)> = vec![
        (
            "/terms/0/source",
            json!("字".repeat(200)),
            json!("字".repeat(201)),
            json!(["terms", 0, "source"]),
        ),
        (
            "/persons/0/name",
            json!("𠀀".repeat(200)),
            json!("𠀀".repeat(201)),
            json!(["persons", 0, "name"]),
        ),
        (
            "/persons/0/reading",
            json!("よ".repeat(200)),
            json!("よ".repeat(201)),
            json!(["persons", 0, "reading"]),
        ),
        (
            "/persons/0/evidence/0/quote",
            json!("a".repeat(200)),
            json!("a".repeat(201)),
            json!(["persons", 0, "evidence", 0, "quote"]),
        ),
        (
            "/terms/0/note",
            json!("字".repeat(500)),
            json!("字".repeat(501)),
            json!(["terms", 0, "note"]),
        ),
        (
            "/persons/0/voice/description",
            json!("é".repeat(500)),
            json!("é".repeat(501)),
            json!(["persons", 0, "voice", "description"]),
        ),
        (
            "/persons/0/address_rules/0/evidence/0/explanation",
            json!("字".repeat(500)),
            json!("字".repeat(501)),
            json!([
                "persons",
                0,
                "address_rules",
                0,
                "evidence",
                0,
                "explanation"
            ]),
        ),
        (
            "/terms/0/allowed",
            many(json!("译"), 16),
            many(json!("译"), 17),
            json!(["terms", 0, "allowed"]),
        ),
        (
            "/terms/0/allowed",
            json!(["字".repeat(200)]),
            json!(["字".repeat(201)]),
            json!(["terms", 0, "allowed", 0]),
        ),
        (
            "/persons/0/aliases",
            many(json!("别名"), 16),
            many(json!("别名"), 17),
            json!(["persons", 0, "aliases"]),
        ),
        (
            "/persons/0/voice/examples",
            many(json!(""), 16),
            many(json!(""), 17),
            json!(["persons", 0, "voice", "examples"]),
        ),
        (
            "/persons/0/terms",
            many(json!("t-1"), 5000),
            many(json!("t-1"), 5001),
            json!(["persons", 0, "terms"]),
        ),
        (
            "/terms",
            many(min_term(), 5000),
            many(min_term(), 5001),
            json!(["terms"]),
        ),
        (
            "/persons",
            many(min_person(), 5000),
            many(min_person(), 5001),
            json!(["persons"]),
        ),
        (
            "/persons/0/address_rules",
            many(min_rule(), 5000),
            many(min_rule(), 5001),
            json!(["persons", 0, "address_rules"]),
        ),
        (
            "/persons/0/evidence",
            many(min_evidence(), 5000),
            many(min_evidence(), 5001),
            json!(["persons", 0, "evidence"]),
        ),
        (
            "/terms/0/term_id",
            json!(format!("{}a", "字".repeat(21))),
            json!(format!("{}ab", "字".repeat(21))),
            json!(["terms", 0, "term_id"]),
        ),
        (
            "/persons/0/address_rules/0/target",
            json!(ascii64),
            json!(format!("{ascii64}a")),
            json!(["persons", 0, "address_rules", 0, "target"]),
        ),
        (
            "/terms/0/scope/from/chapter",
            json!("ü".repeat(32)),
            json!(format!("{}x", "ü".repeat(32))),
            json!(["terms", 0, "scope", "from", "chapter"]),
        ),
    ];
    let f = Fixture::new();
    let mut revision = None;
    for (i, (pointer, fits, over, location)) in cases.into_iter().enumerate() {
        let mut value = sample();
        *value.pointer_mut(pointer).unwrap() = over;
        let before = tree(&f.root);
        assert_limit(
            save_with(&f, &format!("over-{i}"), revision.clone(), material(value)),
            &path(location),
        );
        assert!(tree(&f.root) == before, "{pointer} 超限不得改动盘面");

        let mut value = sample();
        *value.pointer_mut(pointer).unwrap() = fits;
        let data = material(value);
        let receipt = save_with(&f, &format!("fits-{i}"), revision.clone(), data.clone())
            .unwrap_or_else(|e| panic!("{pointer} 恰好等于上限应可保存：{e:?}"));
        assert_eq!(current(&f).material, data, "{pointer}");
        revision = Some(receipt.revision);
    }
}

/// Fill notes until the compact material is exactly `MAX_MATERIAL_BYTES`.
fn exact_max_material() -> StructuredGlossary {
    let mut data = material(sample());
    data.terms[0].note = None;
    let term = data.terms[0].clone();
    data.terms = vec![term; 2500];
    let mut remaining = MAX_MATERIAL_BYTES - compact_len(&data);
    for term in &mut data.terms {
        // Adding the note field costs ten compact JSON bytes (,"note":"").
        if remaining >= 10 {
            let bytes = (remaining - 10).min(500);
            term.note = Some("x".repeat(bytes));
            remaining -= bytes + 10;
        }
    }
    assert_eq!(remaining, 0);
    assert_eq!(compact_len(&data), MAX_MATERIAL_BYTES);
    data
}

#[test]
fn material_byte_limit_is_inclusive_at_save() {
    let f = Fixture::new();
    let mut data = exact_max_material();
    let receipt = save_with(&f, "exact", None, data.clone()).unwrap();
    assert_eq!(current(&f).material, data);
    let bytes = draft_bytes(&f);
    let before = tree(&f.root);
    let note = data
        .terms
        .iter_mut()
        .find_map(|term| term.note.as_mut().filter(|s| s.len() < 500))
        .unwrap();
    note.push('x');
    assert_eq!(compact_len(&data), MAX_MATERIAL_BYTES + 1);
    assert_limit(save_with(&f, "over", Some(receipt.revision), data), &[]);
    assert_eq!(draft_bytes(&f), bytes);
    assert!(tree(&f.root) == before);
}

/// Empty person-term IDs have the largest pretty-envelope expansion of any editable value
/// (3 compact bytes, 16 pretty bytes at their indentation depth).
fn worst_case_material() -> StructuredGlossary {
    let mut data = material(json!({"format":2,"terms":[],"persons":[]}));
    let mut full: Value = min_person();
    full["terms"] = many(json!(""), 5000);
    let full: Person = serde_json::from_value(full).unwrap();
    while compact_len(&data) <= MAX_MATERIAL_BYTES {
        data.persons.push(full.clone());
    }
    // Each trailing empty ID costs three compact bytes (,"").
    let over = compact_len(&data) - MAX_MATERIAL_BYTES;
    let terms = &mut data.persons.last_mut().unwrap().terms;
    terms.truncate(terms.len() - over.div_ceil(3));
    let remaining = MAX_MATERIAL_BYTES - compact_len(&data);
    assert!(remaining <= 200, "{remaining}");
    data.persons.last_mut().unwrap().name = "x".repeat(remaining);
    assert_eq!(compact_len(&data), MAX_MATERIAL_BYTES);
    data
}

#[test]
fn worst_case_full_material_fits_the_envelope() {
    let f = Fixture::new();
    let data = worst_case_material();
    let receipt = save_with(&f, "worst", None, data.clone()).unwrap();
    let len = draft_bytes(&f).len() as u64;
    println!("worst-case 1 MiB material envelope: {len} bytes");
    assert!(len <= MAX_ENVELOPE_BYTES, "{len}");
    let draft = current(&f);
    assert_eq!(draft.revision, receipt.revision);
    assert_eq!(draft.material, data);
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedRequest {
    hash: String,
    receipt: MaterialReceipt,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Book {
    draft: MaterialDraft,
    requests: BTreeMap<String, SavedRequest>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema_version: u32,
    revision: u64,
    data: Book,
}

fn encode(envelope: &Envelope) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(envelope).unwrap();
    bytes.push(b'\n');
    bytes
}

fn filler(n: usize) -> SavedRequest {
    SavedRequest {
        hash: format!("{n:064x}"),
        receipt: MaterialReceipt {
            revision: format!("{n:032x}"),
            content_hash: format!("{n:064x}"),
        },
    }
}

/// Legal, distinct receipts whose IDs are padded so that a following save of request
/// `next_id` publishes exactly `target` bytes.
fn pad_to_next_save(seed: &Envelope, next_id: &str, target: u64) -> Envelope {
    let size = |e: &Envelope| encode(e).len() as u64;
    let mut probe = seed.clone();
    probe.data.requests.insert("p".into(), filler(0));
    let per_entry = size(&probe) - size(seed) - 1;
    let base = size(seed) + per_entry + next_id.len() as u64;
    let diff = target - base;
    let k = diff.div_ceil(per_entry + 128);
    let ids = diff - k * per_entry;
    let (each, extra) = (ids / k, ids % k);
    assert!((8..128).contains(&each), "{each}");
    let mut out = seed.clone();
    for i in 0..k {
        let len = (each + u64::from(i < extra)) as usize;
        let mut id = format!("f{i}-");
        id.push_str(&"x".repeat(len - id.len()));
        out.data.requests.insert(id, filler(i as usize + 1));
    }
    let mut predicted = out.clone();
    predicted
        .data
        .requests
        .insert(next_id.into(), filler(usize::MAX));
    predicted.revision += 1;
    assert_eq!(size(&predicted), target);
    out
}

fn assert_too_large(result: Result<MaterialReceipt, MaterialError>) {
    match result {
        Err(MaterialError::Storage(JsonFileError::TooLarge { limit })) => {
            assert_eq!(limit, MAX_ENVELOPE_BYTES)
        }
        other => panic!("完整信封超限应明确拒绝，实际：{other:?}"),
    }
}

#[test]
fn envelope_limit_is_inclusive_and_receipts_are_never_pruned() {
    let f = Fixture::new();
    let seed = save_with(&f, "seed", None, unfinished()).unwrap();
    let file = f.root.join(f.draft_path());
    let stored: Envelope = serde_json::from_slice(&draft_bytes(&f)).unwrap();
    assert_eq!(
        encode(&stored),
        draft_bytes(&f),
        "测试信封编码须与存储逐字节相同"
    );

    let over = pad_to_next_save(&stored, "next", MAX_ENVELOPE_BYTES + 1);
    fs::write(&file, encode(&over)).unwrap();
    assert_eq!(current(&f).revision, seed.revision, "填充回执须是合法草稿");
    let count = receipt_count(&f);
    let before = tree(&f.root);
    assert_too_large(save_with(
        &f,
        "next",
        Some(seed.revision.clone()),
        unfinished(),
    ));
    assert!(tree(&f.root) == before, "超限保存不得覆盖旧草稿");
    assert_eq!(save_with(&f, "seed", None, unfinished()).unwrap(), seed);
    assert!(tree(&f.root) == before, "超限后旧请求重试只返回历史回执");

    let exact = pad_to_next_save(&stored, "next", MAX_ENVELOPE_BYTES);
    fs::write(&file, encode(&exact)).unwrap();
    let next = save_with(&f, "next", Some(seed.revision.clone()), unfinished()).unwrap();
    assert_eq!(draft_bytes(&f).len() as u64, MAX_ENVELOPE_BYTES);
    assert_eq!(current(&f).revision, next.revision);
    let full_count = receipt_count(&f);
    assert_eq!(full_count, exact.data.requests.len() + 1);
    assert!(full_count >= count);

    let full = tree(&f.root);
    assert_too_large(save_with(
        &f,
        "after",
        Some(next.revision.clone()),
        unfinished(),
    ));
    assert!(tree(&f.root) == full, "满信封后新保存明确拒绝且不覆旧");
    assert_eq!(save_with(&f, "seed", None, unfinished()).unwrap(), seed);
    assert_eq!(
        save_with(&f, "next", Some(seed.revision), unfinished()).unwrap(),
        next
    );
    assert!(tree(&f.root) == full, "回执不被淘汰，重试不写盘");
    assert_eq!(receipt_count(&f), full_count);
}
