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
    });
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
    for paragraph in paragraphs {
        let item = BlockParagraph {
            id: paragraph.id.clone(),
            text: paragraph.text.clone(),
        };
        block.paragraphs.push(item.clone());
        if block.serialized()?.len() > limit {
            block.paragraphs.pop();
            if !block.paragraphs.is_empty() {
                blocks.push(block);
            }
            block = empty(chapter, blocks.len() + 1);
            block.paragraphs.push(item);
            if block.serialized()?.len() > limit {
                return Err(invalid(format!(
                    "{chapter}: {} 单段超过 {limit} 字节，拒绝静默拆段",
                    paragraph.id
                )));
            }
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
