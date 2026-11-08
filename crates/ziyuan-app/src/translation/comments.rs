//! Local review comments. Single-user, no cloud sync; resolving a comment never edits the translation.
use super::{Result, Translations, invalid};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub block: String,
    #[serde(default)]
    pub paragraph: String,
    pub body: String,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CommentBook {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub comments: Vec<Comment>,
}
fn one() -> u32 {
    1
}

impl Translations {
    fn comments_path(&self, project: &str) -> PathBuf {
        self.root
            .join(".ziyuan")
            .join("text-projects")
            .join(project)
            .join("comments.json")
    }

    fn load_comments(&self, project: &str) -> Result<CommentBook> {
        let path = self.comments_path(project);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| invalid(format!("comments.json 格式不对：{e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(CommentBook {
                version: 1,
                comments: Vec::new(),
            }),
            Err(e) => Err(invalid(format!("读取 comments.json 失败：{e}"))),
        }
    }

    fn save_comments(&self, project: &str, book: &CommentBook) -> Result<()> {
        let path = self.comments_path(project);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| invalid(format!("创建 comments.json 目录失败：{e}")))?;
        }
        let bytes = serde_json::to_vec_pretty(book).map_err(|e| invalid(e.to_string()))?;
        std::fs::write(&path, [bytes.as_slice(), b"\n"].concat())
            .map_err(|e| invalid(format!("写入 comments.json 失败：{e}")))
    }

    pub fn comment_list(&self, project: &str, block: Option<&str>) -> Result<Vec<Comment>> {
        let mut comments = self.load_comments(project)?.comments;
        if let Some(block) = block {
            comments.retain(|c| c.block == block);
        }
        Ok(comments)
    }

    pub fn comment_add(
        &self,
        project: &str,
        block: &str,
        paragraph: &str,
        body: &str,
    ) -> Result<Comment> {
        if body.trim().is_empty() {
            return Err(invalid("评论内容不能为空"));
        }
        if block.trim().is_empty() {
            return Err(invalid("评论须指向一个块"));
        }
        let mut book = self.load_comments(project)?;
        let next = book
            .comments
            .iter()
            .filter_map(|c| c.id.strip_prefix("c-").and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0)
            + 1;
        let comment = Comment {
            id: format!("c-{next}"),
            block: block.to_owned(),
            paragraph: paragraph.to_owned(),
            body: body.to_owned(),
            resolved: false,
            parent_id: None,
            assignee: None,
        };
        book.comments.push(comment.clone());
        self.save_comments(project, &book)?;
        Ok(comment)
    }

    pub fn comment_reply(&self, project: &str, parent: &str, body: &str) -> Result<Comment> {
        if body.trim().is_empty() {
            return Err(invalid("评论内容不能为空"));
        }
        let mut book = self.load_comments(project)?;
        let source = book
            .comments
            .iter()
            .find(|c| c.id == parent)
            .cloned()
            .ok_or_else(|| invalid("没有这条评论"))?;
        let next = book
            .comments
            .iter()
            .filter_map(|c| c.id.strip_prefix("c-").and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0)
            + 1;
        let comment = Comment {
            id: format!("c-{next}"),
            block: source.block,
            paragraph: source.paragraph,
            body: body.to_owned(),
            resolved: false,
            parent_id: Some(parent.to_owned()),
            assignee: None,
        };
        book.comments.push(comment.clone());
        self.save_comments(project, &book)?;
        Ok(comment)
    }

    pub fn comment_assign(&self, project: &str, id: &str, assignee: &str) -> Result<Comment> {
        let mut book = self.load_comments(project)?;
        let comment = book
            .comments
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| invalid("没有这条评论"))?;
        comment.assignee = if assignee.trim().is_empty() {
            None
        } else {
            Some(assignee.to_owned())
        };
        let out = comment.clone();
        self.save_comments(project, &book)?;
        Ok(out)
    }

    pub fn comment_resolve(&self, project: &str, id: &str, resolved: bool) -> Result<Comment> {
        let mut book = self.load_comments(project)?;
        let comment = book
            .comments
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| invalid("没有这条评论"))?;
        comment.resolved = resolved;
        let out = comment.clone();
        self.save_comments(project, &book)?;
        Ok(out)
    }
}
