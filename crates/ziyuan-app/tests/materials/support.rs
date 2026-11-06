use serde_json::{Value, json};
use ziyuan_app::text_project::Project;
use ziyuan_app::translation::StructuredGlossary;

pub fn sample() -> Value {
    json!({
        "format":2,
        "terms":[{
            "term_id":"t-1","source":"先生","category":"title","allowed":["先生","老师"],
            "note":"备注\n制表\t","scope":{"kind":"range","from":{"chapter":"z","paragraph":"pz"},
            "to":{"chapter":"a","paragraph":"pa"}},"available_from":{"chapter":"z"},
            "evidence":[evidence()],"status":"confirmed"
        }],
        "persons":[{
            "person_id":"p_1","available_from":{"chapter":"z","paragraph":"pz"},
            "name":"同名","reading":"よみ","aliases":["别名","同名"],
            "terms":["t-1"],"address_rules":[{
                "target":"p_2","speaker":{"kind":"person","person_id":"p_1"},
                "scope":{"kind":"project"},"term":"t-1","note":"备注","evidence":[evidence()]
            }],"voice":{"description":"庄重","examples":["先生来了","第二例"]},
            "evidence":[evidence()],"status":"tentative"
        },{
            "person_id":"p_2","name":"同名","aliases":[],"terms":[],
            "address_rules":[],"voice":{"description":"轻快"},"evidence":[],"status":"confirmed"
        }]
    })
}

fn evidence() -> Value {
    json!({"paragraph":"pz","quote":"先生","explanation":"称呼","stance":"project_fact","status":"confirmed"})
}

pub fn material(value: Value) -> StructuredGlossary {
    serde_json::from_value(value).unwrap()
}

pub fn project() -> Project {
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
