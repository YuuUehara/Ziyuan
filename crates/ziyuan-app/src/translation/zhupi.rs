//! 朱批：皇帝逐段批注，存 `.ziyuan/text-projects/<项目>/zhupi.json`（带版本，变更即加一）。
use super::{Result, Translations, find_block, invalid, pretty_json};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ziyuan_storage::JsonFile;

const ZHUPI_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ZHUPI: usize = 2000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZhupiNote {
    pub text: String,
    pub updated: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ZhupiBook {
    version: u32,
    updated: u64,
    /// 块 ID → 段 ID → 朱批。
    blocks: BTreeMap<String, BTreeMap<String, ZhupiNote>>,
}

/// 一块的朱批与全项目朱批的现行版本。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct BlockZhupi {
    pub version: u32,
    /// 段 ID → 朱批。
    pub items: BTreeMap<String, ZhupiNote>,
}

impl Translations {
    fn zhupi_file(&self, project: &str) -> Result<JsonFile<ZhupiBook>> {
        Ok(JsonFile::new(
            self.store_dir(project)?.join("zhupi.json"),
            1,
            ZHUPI_BYTES,
        ))
    }

    fn check_paragraph(&self, project: &str, block: &str, paragraph: &str) -> Result<()> {
        let p = self.project(project)?;
        let (_, _, prompt) = find_block(&p, block)?;
        if !prompt.paragraphs.iter().any(|x| x.id == paragraph) {
            return Err(invalid(format!("块 {block} 没有段 {paragraph}")));
        }
        Ok(())
    }

    /// 写一段朱批（同文不改版本），返回现行版本。
    pub fn set_zhupi(
        &self,
        project: &str,
        block: &str,
        paragraph: &str,
        text: &str,
    ) -> Result<u32> {
        self.check_paragraph(project, block, paragraph)?;
        let text = text.trim();
        if text.is_empty() {
            return Err(invalid("朱批不能为空；要删去请用清除"));
        }
        if text.chars().count() > MAX_ZHUPI {
            return Err(invalid(format!("朱批至多 {MAX_ZHUPI} 字")));
        }
        if text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(invalid("朱批含控制字符"));
        }
        let (stored, version) = self.zhupi_file(project)?.update(|current| {
            let mut book = current.map(|s| s.value).unwrap_or_default();
            let now = crate::time::now();
            let items = book.blocks.entry(block.to_owned()).or_default();
            if items.get(paragraph).is_some_and(|n| n.text == text) {
                let v = book.version;
                return Ok::<_, super::TranslationError>((book, v));
            }
            items.insert(
                paragraph.to_owned(),
                ZhupiNote {
                    text: text.to_owned(),
                    updated: now,
                },
            );
            book.version += 1;
            book.updated = now;
            let v = book.version;
            Ok((book, v))
        })?;
        debug_assert_eq!(stored.value.version, version);
        Ok(version)
    }

    /// 清除一段朱批（本无朱批则不改版本），返回现行版本。
    pub fn clear_zhupi(&self, project: &str, block: &str, paragraph: &str) -> Result<u32> {
        self.check_paragraph(project, block, paragraph)?;
        let (_, version) = self.zhupi_file(project)?.update(|current| {
            let mut book = current.map(|s| s.value).unwrap_or_default();
            let removed = book
                .blocks
                .get_mut(block)
                .and_then(|items| items.remove(paragraph))
                .is_some();
            if book.blocks.get(block).is_some_and(BTreeMap::is_empty) {
                book.blocks.remove(block);
            }
            if removed {
                book.version += 1;
                book.updated = crate::time::now();
            }
            let v = book.version;
            Ok::<_, super::TranslationError>((book, v))
        })?;
        Ok(version)
    }

    /// 一块的朱批。
    pub fn zhupi(&self, project: &str, block: &str) -> Result<BlockZhupi> {
        let book = self
            .zhupi_file(project)?
            .load()?
            .map(|s| s.value)
            .unwrap_or_default();
        Ok(BlockZhupi {
            version: book.version,
            items: book.blocks.get(block).cloned().unwrap_or_default(),
        })
    }

    /// 全项目朱批的现行版本；从未朱批为 0。
    pub fn zhupi_version(&self, project: &str) -> Result<u32> {
        Ok(self
            .zhupi_file(project)?
            .load()?
            .map_or(0, |s| s.value.version))
    }

    /// 修订时导出的 `zhupi.json`：按块内段落顺序列出。
    pub(crate) fn zhupi_export(
        &self,
        project: &crate::text_project::Project,
        block: &str,
    ) -> Result<Vec<u8>> {
        let (_, _, prompt) = find_block(project, block)?;
        let zhupi = self.zhupi(&project.id, block)?;
        let items: Vec<_> = prompt
            .paragraphs
            .iter()
            .filter_map(|p| {
                zhupi
                    .items
                    .get(&p.id)
                    .map(|n| serde_json::json!({ "id": p.id, "zhupi": n.text }))
            })
            .collect();
        pretty_json(&serde_json::json!({
            "note": "皇帝朱批：修订时优先遵从；与审校意见冲突时按朱批改。",
            "version": zhupi.version,
            "block_id": block,
            "items": items,
        }))
    }
}
