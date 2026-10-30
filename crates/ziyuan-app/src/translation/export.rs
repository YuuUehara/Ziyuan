//! 每次下诏前导出的只读材料：`translations/<项目>/<块>/`。从不触碰 `zh.json` 与 `proposals/`。
use super::{
    ContextSnapshot, Result, Translations, all_blocks, ensure_plain_dirs, find_block, pretty_json,
    sha256_hex, write_plain,
};
use crate::text_project::{BlockParagraph, Project};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;

/// 导出结果：所用原文哈希与上下文版本（下诏时记入关联）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Exported {
    pub project: String,
    pub block: String,
    pub source_hash: String,
    pub context_version: u32,
    /// 写出的文件（项目内相对路径）。
    pub files: Vec<String>,
}

/// 修订时另导出的两份材料（已序列化）。
pub(crate) struct ReviseMaterials {
    pub zhupi: Vec<u8>,
    pub reviews: Vec<u8>,
}

fn neighbor(paragraph: Option<&BlockParagraph>) -> serde_json::Value {
    match paragraph {
        Some(p) => json!([{ "id": p.id, "text": p.text }]),
        None => json!([]),
    }
}

/// 块所在章之前各章的前情摘要。
fn prior_summaries<'a>(
    context: Option<&'a ContextSnapshot>,
    chapter: &str,
) -> BTreeMap<&'a str, &'a str> {
    context
        .map(|c| {
            c.summaries
                .iter()
                .filter(|(id, _)| id.as_str() < chapter)
                .map(|(id, text)| (id.as_str(), text.as_str()))
                .collect()
        })
        .unwrap_or_default()
}

impl Translations {
    /// 导出 `source.json`、`context.json`、`neighbors.json`（用现行已批准上下文）。
    pub fn export(&self, project: &str, block: &str) -> Result<Exported> {
        let p = self.project(project)?;
        let context = self.current_context(project)?;
        self.export_with(&p, block, context.as_ref(), None)
    }

    pub(crate) fn export_with(
        &self,
        project: &Project,
        block: &str,
        context: Option<&ContextSnapshot>,
        revise: Option<&ReviseMaterials>,
    ) -> Result<Exported> {
        let (index, chapter, prompt) = find_block(project, block)?;
        let blocks = all_blocks(project);
        let source = prompt.serialized()?;
        let dir = ensure_plain_dirs(&self.root, &["translations", &project.id, block])?;
        let rel = |name: &str| format!("translations/{}/{block}/{name}", project.id);
        let mut files = Vec::new();

        write_plain(&dir.join("source.json"), &source)?;
        files.push(rel("source.json"));

        let version = context.map_or(0, |c| c.version);
        let context_json = json!({
            "version": version,
            "note": "已批准的上下文快照（只读）：术语表 glossary 的 allowed 为允许译法；summaries 为此前各章的前情摘要。",
            "glossary": context.map(|c| c.glossary.clone()).unwrap_or_default(),
            "style": context.map(|c| c.style.as_str()).unwrap_or(""),
            "summaries": prior_summaries(context, chapter),
        });
        write_plain(&dir.join("context.json"), &pretty_json(&context_json)?)?;
        files.push(rel("context.json"));

        let previous = index
            .checked_sub(1)
            .and_then(|i| blocks.get(i))
            .and_then(|(_, b)| b.paragraphs.last());
        let next = blocks
            .get(index + 1)
            .and_then(|(_, b)| b.paragraphs.first());
        let neighbors = json!({
            "note": "前后衔接段，只读参考：不得翻译，不得写入译稿。",
            "previous": neighbor(previous),
            "next": neighbor(next),
        });
        write_plain(&dir.join("neighbors.json"), &pretty_json(&neighbors)?)?;
        files.push(rel("neighbors.json"));

        if let Some(revise) = revise {
            write_plain(&dir.join("zhupi.json"), &revise.zhupi)?;
            files.push(rel("zhupi.json"));
            write_plain(&dir.join("reviews.json"), &revise.reviews)?;
            files.push(rel("reviews.json"));
        }
        Ok(Exported {
            project: project.id.clone(),
            block: block.to_owned(),
            source_hash: sha256_hex(&source),
            context_version: version,
            files,
        })
    }
}
