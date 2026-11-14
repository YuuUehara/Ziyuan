use super::{BlockParagraph, DEFAULT_BLOCK_BYTES, Paragraph, PromptBlock, Result, invalid};

pub fn split_paragraphs(chapter_id: &str, bytes: &[u8]) -> Result<Vec<Paragraph>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| invalid(format!("{chapter_id}: 仅接受 UTF-8 TXT")))?;
    let mut paragraphs = Vec::new();
    let mut offset = 0;
    let mut start = None;
    let mut end = 0;
    for line in text.split_inclusive('\n') {
        let body = match line.strip_suffix('\n') {
            Some(body) => body.strip_suffix('\r').unwrap_or(body),
            None => line,
        };
        if body.trim().is_empty() {
            if let Some(start) = start.take() {
                push(&mut paragraphs, chapter_id, text, start, end);
            }
        } else {
            start.get_or_insert(offset);
            end = offset + body.len();
        }
        offset += line.len();
    }
    if let Some(start) = start {
        push(&mut paragraphs, chapter_id, text, start, end);
    }
    if paragraphs.is_empty() {
        return Err(invalid(format!("{chapter_id}: 原文为空或仅有空白")));
    }
    Ok(paragraphs)
}
fn push(out: &mut Vec<Paragraph>, chapter: &str, text: &str, start: usize, end: usize) {
    out.push(Paragraph {
        id: format!("{chapter}-p{:04}", out.len() + 1),
        byte_start: start,
        byte_end: end,
        text: text[start..end].replace("\r\n", "\n"),
        inlines: Vec::new(),
    });
}

fn indented_item_bytes(item: &BlockParagraph) -> Result<usize> {
    let bytes = serde_json::to_vec_pretty(item)?;
    let lines = bytes.iter().filter(|&&b| b == b'\n').count() + 1;
    Ok(bytes.len() + lines * 4)
}

/// Stable paragraph IDs are independent of block size. Oversized paragraphs are never split.
pub fn chunk_paragraphs(
    chapter: &str,
    paragraphs: &[Paragraph],
    limit: usize,
) -> Result<Vec<PromptBlock>> {
    if limit == 0 || limit > DEFAULT_BLOCK_BYTES {
        return Err(invalid("分块字节上限须在 1..=49152"));
    }
    let mut blocks = Vec::new();
    let mut block = empty(chapter, 1);
    let mut block_bytes = block.serialized()?.len();
    for paragraph in paragraphs {
        let item = BlockParagraph {
            id: paragraph.id.clone(),
            text: paragraph.text.clone(),
            inlines: paragraph.inlines.clone(),
        };
        let item_len = indented_item_bytes(&item)?;
        let next_bytes = if block.paragraphs.is_empty() {
            block_bytes + 4 + item_len
        } else {
            block_bytes + 2 + item_len
        };
        if next_bytes > limit {
            if !block.paragraphs.is_empty() {
                blocks.push(block);
            }
            block = empty(chapter, blocks.len() + 1);
            let single_bytes = block.serialized()?.len() + 4 + item_len;
            block.paragraphs.push(item);
            if single_bytes > limit || block.serialized()?.len() > limit {
                return Err(invalid(format!(
                    "{chapter}: {} 单段超过 {limit} 字节，拒绝静默拆段",
                    paragraph.id
                )));
            }
            block_bytes = single_bytes;
        } else {
            block.paragraphs.push(item);
            block_bytes = next_bytes;
        }
    }
    if !block.paragraphs.is_empty() {
        blocks.push(block);
    }
    Ok(blocks)
}
fn empty(chapter: &str, index: usize) -> PromptBlock {
    PromptBlock {
        schema_version: 1,
        block_id: format!("{chapter}-b{index:04}"),
        paragraphs: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_project::Inline;

    #[test]
    fn incremental_chunk_sizes_match_serialized_pretty_json_exactly() {
        let paragraphs: Vec<Paragraph> = (1..=320)
            .map(|i| {
                let inlines = if i % 7 == 0 {
                    vec![
                        Inline::Ruby {
                            base: "漢字".into(),
                            reading: "かんじ".into(),
                            byte_start: 0,
                            byte_end: 10,
                        },
                        Inline::Emphasis {
                            text: "強調".into(),
                        },
                    ]
                } else {
                    Vec::new()
                };
                Paragraph {
                    id: format!("c0001-p{i:04}"),
                    byte_start: i,
                    byte_end: i + 10,
                    text: format!("第{i}段\n\"\\{}\tテスト", "あ".repeat((i % 23) + 1)),
                    inlines,
                }
            })
            .collect();
        let blocks = chunk_paragraphs("c0001", &paragraphs, 1024).unwrap();
        assert!(blocks.len() > 20);
        for block in &blocks {
            assert!(block.serialized().unwrap().len() <= 1024);
        }
        // Verify every non-final block would have exceeded `limit` by adding the next paragraph.
        for pair in blocks.windows(2) {
            let mut combined = pair[0].clone();
            combined.paragraphs.push(pair[1].paragraphs[0].clone());
            assert!(combined.serialized().unwrap().len() > 1024);
        }
    }
}
