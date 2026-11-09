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
    fn comments_path(&self, project: &str) -> Result<PathBuf> {
        self.project(project)?;
        Ok(
            super::check_plain_dirs(self.root(), &[".ziyuan", "text-projects", project])?
                .join("comments.json"),
        )
    }
    fn load_comments(&self, project: &str) -> Result<CommentBook> {
        let path = self.comments_path(project)?;
        let book: CommentBook = match self.read_local_translation_file(&path)? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| invalid(format!("comments.json 格式不对：{e}"))),
            None => Ok(CommentBook {
                version: 1,
                comments: Vec::new(),
            }),
        }?;
        if book.version != 1 || book.comments.len() > 10000 {
            return Err(invalid("评论版本或数量不支持"));
        }
        Ok(book)
    }

    fn save_comments(&self, project: &str, book: &CommentBook) -> Result<()> {
        if book.comments.len() > 10000 {
            return Err(invalid("本地记录超过10000条上限"));
        }
        let path = self.comments_path(project)?;
        let bytes = serde_json::to_vec_pretty(book)?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err(invalid("本地记录超过32 MiB"));
        }
        super::write_plain(&path, &[bytes.as_slice(), b"\n"].concat())
    }

    pub fn comment_list(&self, project: &str, block: Option<&str>) -> Result<Vec<Comment>> {
        if let Some(block) = block {
            self.check_local_comment_anchor(project, block, "")?;
        }
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
        let _guard = self.lock_local_translation(project)?;
        if body.trim().is_empty() || body.len() > 16000 {
            return Err(invalid("评论内容不能为空"));
        }
        if block.trim().is_empty() {
            return Err(invalid("评论须指向一个块"));
        }
        self.check_local_comment_anchor(project, block, paragraph)?;
        let mut book = self.load_comments(project)?;
        let next = book
            .comments
            .iter()
            .filter_map(|c| c.id.strip_prefix("c-").and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| invalid("评论序号溢出"))?;
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
        let _guard = self.lock_local_translation(project)?;
        if body.trim().is_empty() || body.len() > 16000 {
            return Err(invalid("评论内容不能为空"));
        }
        let mut book = self.load_comments(project)?;
        let source = book
            .comments
            .iter()
            .find(|c| c.id == parent)
            .cloned()
            .ok_or_else(|| invalid("没有这条评论"))?;
        self.check_local_comment_anchor(project, &source.block, &source.paragraph)?;
        let next = book
            .comments
            .iter()
            .filter_map(|c| c.id.strip_prefix("c-").and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| invalid("评论序号溢出"))?;
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
        let _guard = self.lock_local_translation(project)?;
        if assignee.len() > 1000 {
            return Err(invalid("复核者名称过长"));
        }
        let mut book = self.load_comments(project)?;
        let comment = book
            .comments
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| invalid("没有这条评论"))?;
        self.check_local_comment_anchor(project, &comment.block, &comment.paragraph)?;
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
        let _guard = self.lock_local_translation(project)?;
        let mut book = self.load_comments(project)?;
        let comment = book
            .comments
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| invalid("没有这条评论"))?;
        self.check_local_comment_anchor(project, &comment.block, &comment.paragraph)?;
        comment.resolved = resolved;
        let out = comment.clone();
        self.save_comments(project, &book)?;
        Ok(out)
    }
}
