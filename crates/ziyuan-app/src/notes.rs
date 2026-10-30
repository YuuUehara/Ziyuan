//! 御笔札记：皇帝亲手记下的词语或段落的认可译法，全局一份 `.ziyuan/notes.json`。
//! 不写事件簿，不随诏令自动交给模型；可并入某项目的上下文草稿（之后照常批准才生效）。
//! 不需要朝廷即可使用。
use crate::translation::{
    DirLock, GLOSSARY_FILE, GlossaryEntry, Result, STYLE_FILE, TranslationError, Translations,
    context_dir_rel, find_block, invalid, parse_glossary, pretty_json, read_text_opt, write_plain,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use ziyuan_storage::JsonFile;

const NOTES_BYTES: u64 = 4 * 1024 * 1024;
const MAX_WORD: usize = 200;
const MAX_PASSAGE: usize = 5000;
const MAX_REMARK: usize = 1000;
const STYLE_BYTES: usize = 256 * 1024;
const GLOSSARY_BYTES: usize = 1024 * 1024;
/// 文体草稿中收录札记段落的小节。
pub const EXAMPLES_HEADING: &str = "## 范例";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    Word,
    Passage,
}

impl NoteKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Word => "word",
            Self::Passage => "passage",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Word => "词语",
            Self::Passage => "段落",
        }
    }
}

impl std::str::FromStr for NoteKind {
    type Err = TranslationError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "word" => Ok(Self::Word),
            "passage" => Ok(Self::Passage),
            _ => Err(invalid(format!("札记种类须为 word 或 passage：{s}"))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// 32 位小写十六进制随机 ID。
    pub id: String,
    pub kind: NoteKind,
    pub source: String,
    pub zh: String,
    #[serde(default)]
    pub remark: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub block: Option<String>,
    #[serde(default)]
    pub paragraph: Option<String>,
    pub created: u64,
    pub updated: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct NoteBook {
    notes: Vec<Note>,
}

/// 新札记。出处（项目、块、段）可选；给了就核对：块须属项目，段须属块。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewNote {
    pub kind: NoteKind,
    pub source: String,
    pub zh: String,
    pub remark: String,
    pub project: Option<String>,
    pub block: Option<String>,
    pub paragraph: Option<String>,
}

/// 修改札记：`None` 的字段不改。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NoteEdit {
    pub kind: Option<NoteKind>,
    pub source: Option<String>,
    pub zh: Option<String>,
    pub remark: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NoteFilter {
    pub project: Option<String>,
    pub kind: Option<NoteKind>,
    /// 在原文、译法、备注中查找的子串（不分大小写）。
    pub search: Option<String>,
}

/// 并入草稿的结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Promotion {
    pub note: String,
    /// 写入的草稿文件（项目内相对路径）。
    pub path: String,
    /// 是否新写入；已有相同条目时为假，草稿不变。
    pub added: bool,
}

pub struct Notes {
    root: PathBuf,
}

fn random_id() -> Result<String> {
    let mut bytes = [0; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| invalid("无法生成安全随机 ID"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn check_field(
    what: &str,
    text: &str,
    max: usize,
    single_line: bool,
    required: bool,
) -> Result<()> {
    if required && text.trim().is_empty() {
        return Err(invalid(format!("{what}不能为空")));
    }
    if text.chars().count() > max {
        return Err(invalid(format!("{what}至多 {max} 字")));
    }
    if text
        .chars()
        .any(|c| c.is_control() && (single_line || (c != '\n' && c != '\t')))
    {
        return Err(invalid(format!(
            "{what}{}",
            if single_line {
                "须为单行"
            } else {
                "含控制字符"
            }
        )));
    }
    Ok(())
}

fn check_note(note: &Note) -> Result<()> {
    let (max, single) = match note.kind {
        NoteKind::Word => (MAX_WORD, true),
        NoteKind::Passage => (MAX_PASSAGE, false),
    };
    check_field("原文", &note.source, max, single, true)?;
    check_field("译法", &note.zh, max, single, true)?;
    check_field("备注", &note.remark, MAX_REMARK, false, false)
}

fn quoted(text: &str) -> String {
    text.lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn marker(id: &str) -> String {
    format!("<!-- ziyuan-note:{id} -->")
}

/// 把 `entry` 放进 `## 范例` 一节的末尾；没有此节就在文末新建。
fn insert_example(style: &str, entry: &str) -> String {
    let lines: Vec<&str> = style.lines().collect();
    let Some(start) = lines.iter().position(|l| l.trim_end() == EXAMPLES_HEADING) else {
        let mut out = style.trim_end().to_owned();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!("{EXAMPLES_HEADING}\n\n{entry}\n"));
        return out;
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("# ") || l.starts_with("## "))
        .map_or(lines.len(), |i| start + 1 + i);
    let mut section: Vec<&str> = lines[..end].to_vec();
    while section.len() > start + 1 && section.last().is_some_and(|l| l.trim().is_empty()) {
        section.pop();
    }
    let mut out = section.join("\n");
    out.push_str("\n\n");
    out.push_str(entry);
    out.push('\n');
    if end < lines.len() {
        out.push('\n');
        out.push_str(&lines[end..].join("\n"));
        out.push('\n');
    }
    out
}

impl Notes {
    /// `root` 是用户的项目目录（不是 `.ziyuan`）。
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    fn file(&self) -> JsonFile<NoteBook> {
        JsonFile::new(self.root.join(".ziyuan/notes.json"), 1, NOTES_BYTES)
    }

    fn check_link(&self, note: &Note) -> Result<()> {
        match (&note.project, &note.block, &note.paragraph) {
            (None, None, None) => Ok(()),
            (None, _, _) => Err(invalid("出处须先给项目")),
            (Some(_), None, Some(_)) => Err(invalid("给段号须先给块")),
            (Some(project), block, paragraph) => {
                let p = Translations::new(&self.root).project(project)?;
                if let Some(block) = block {
                    let (_, _, prompt) = find_block(&p, block)?;
                    if let Some(paragraph) = paragraph
                        && !prompt.paragraphs.iter().any(|x| &x.id == paragraph)
                    {
                        return Err(invalid(format!("块 {block} 没有段 {paragraph}")));
                    }
                }
                Ok(())
            }
        }
    }

    pub fn add(&self, new: NewNote) -> Result<Note> {
        let now = crate::time::now();
        let note = Note {
            id: random_id()?,
            kind: new.kind,
            source: new.source.trim().to_owned(),
            zh: new.zh.trim().to_owned(),
            remark: new.remark.trim().to_owned(),
            project: new.project,
            block: new.block,
            paragraph: new.paragraph,
            created: now,
            updated: now,
        };
        check_note(&note)?;
        self.check_link(&note)?;
        self.file().update(|current| {
            let mut book = current.map(|s| s.value).unwrap_or_default();
            book.notes.push(note.clone());
            Ok::<_, TranslationError>((book, ()))
        })?;
        Ok(note)
    }

    /// 按创建先后列出符合 `filter` 的札记。
    pub fn list(&self, filter: &NoteFilter) -> Result<Vec<Note>> {
        let book = self.file().load()?.map(|s| s.value).unwrap_or_default();
        let search = filter.search.as_deref().map(str::to_lowercase);
        Ok(book
            .notes
            .into_iter()
            .filter(|n| filter.project.is_none() || n.project == filter.project)
            .filter(|n| filter.kind.is_none_or(|k| k == n.kind))
            .filter(|n| {
                search.as_deref().is_none_or(|s| {
                    [&n.source, &n.zh, &n.remark]
                        .iter()
                        .any(|f| f.to_lowercase().contains(s))
                })
            })
            .collect())
    }

    pub fn get(&self, id: &str) -> Result<Note> {
        self.list(&NoteFilter::default())?
            .into_iter()
            .find(|n| n.id == id)
            .ok_or_else(|| invalid(format!("没有札记 {id}")))
    }

    pub fn edit(&self, id: &str, edit: NoteEdit) -> Result<Note> {
        let (_, note) = self.file().update(|current| {
            let mut book = current.map(|s| s.value).unwrap_or_default();
            let note = book
                .notes
                .iter_mut()
                .find(|n| n.id == id)
                .ok_or_else(|| invalid(format!("没有札记 {id}")))?;
            if let Some(kind) = edit.kind {
                note.kind = kind;
            }
            if let Some(source) = &edit.source {
                note.source = source.trim().to_owned();
            }
            if let Some(zh) = &edit.zh {
                note.zh = zh.trim().to_owned();
            }
            if let Some(remark) = &edit.remark {
                note.remark = remark.trim().to_owned();
            }
            note.updated = crate::time::now();
            check_note(note)?;
            let note = note.clone();
            Ok::<_, TranslationError>((book, note))
        })?;
        Ok(note)
    }

    pub fn remove(&self, id: &str) -> Result<Note> {
        let (_, note) = self.file().update(|current| {
            let mut book = current.map(|s| s.value).unwrap_or_default();
            let at = book
                .notes
                .iter()
                .position(|n| n.id == id)
                .ok_or_else(|| invalid(format!("没有札记 {id}")))?;
            let note = book.notes.remove(at);
            Ok::<_, TranslationError>((book, note))
        })?;
        Ok(note)
    }

    /// 并入 `project` 的上下文草稿：词语进术语表 `glossary.json`（原文与译法都相同的条目已有则不写），
    /// 段落进 `style.md` 的「范例」一节（同一札记已并入则不写）。草稿须再经批准才生效。
    pub fn promote(&self, id: &str, project: &str) -> Result<Promotion> {
        let note = self.get(id)?;
        let t = Translations::new(&self.root);
        t.ensure_context_draft(project)?;
        let dir = self.root.join(context_dir_rel(project));
        // 草稿是读改写：先加锁，免得两个进程同时并入时丢条目。
        let _lock = DirLock::acquire(&dir)?;
        match note.kind {
            NoteKind::Word => {
                let path = dir.join(GLOSSARY_FILE);
                let text = read_text_opt(&path, GLOSSARY_FILE, GLOSSARY_BYTES)?
                    .unwrap_or_else(|| "[]\n".into());
                let mut entries = parse_glossary(text.as_bytes())?;
                let exists = entries
                    .iter()
                    .any(|e| e.source == note.source && e.allowed.contains(&note.zh));
                if !exists {
                    entries.push(GlossaryEntry {
                        source: note.source.clone(),
                        allowed: vec![note.zh.clone()],
                        scope: None,
                        note: (!note.remark.is_empty()).then(|| note.remark.clone()),
                    });
                    parse_glossary(&pretty_json(&entries)?)?;
                    write_plain(&path, &pretty_json(&entries)?)?;
                }
                Ok(Promotion {
                    note: note.id,
                    path: format!("{}/{GLOSSARY_FILE}", context_dir_rel(project)),
                    added: !exists,
                })
            }
            NoteKind::Passage => {
                let path = dir.join(STYLE_FILE);
                let style = read_text_opt(&path, STYLE_FILE, STYLE_BYTES)?.unwrap_or_default();
                let mark = marker(&note.id);
                let exists = style.contains(&mark);
                if !exists {
                    let mut entry = format!(
                        "{mark}\n原文：\n{}\n\n译文：\n{}",
                        quoted(&note.source),
                        quoted(&note.zh)
                    );
                    if !note.remark.is_empty() {
                        entry.push_str(&format!("\n\n备注：{}", note.remark));
                    }
                    let updated = insert_example(&style, &entry);
                    if updated.len() > STYLE_BYTES {
                        return Err(invalid(format!(
                            "{STYLE_FILE} 将超过 {STYLE_BYTES} 字节上限"
                        )));
                    }
                    write_plain(&path, updated.as_bytes())?;
                }
                Ok(Promotion {
                    note: note.id,
                    path: format!("{}/{STYLE_FILE}", context_dir_rel(project)),
                    added: !exists,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::insert_example;

    #[test]
    fn examples_go_at_the_end_of_their_section() {
        assert_eq!(insert_example("", "甲"), "## 范例\n\n甲\n");
        assert_eq!(
            insert_example("# 文体\n\n语气平实。\n", "甲"),
            "# 文体\n\n语气平实。\n\n## 范例\n\n甲\n"
        );
        assert_eq!(
            insert_example("# 文体\n## 范例\n\n甲\n\n## 其他\n乙\n", "丙"),
            "# 文体\n## 范例\n\n甲\n\n丙\n\n## 其他\n乙\n"
        );
    }
}
