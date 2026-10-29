//! 兵部驿站 · GitHub：把带标签的 issue / PR（及其中成员以 `/ziyuan` 开头的评论）转为诏令，
//! 把回奏写回评论，把准奏的改动开为草稿 PR。
//!
//! GitHub 凭据只在本 crate 中读取与使用：不交给朝廷（`Session`）、执行后端或任何模型提示，
//! 错误信息也不含令牌。草稿 PR 经 Git Data API 在远端直接建分支与提交，不碰本地版本库。
use serde_json::{Value, json};
use std::time::Duration;
use ziyuan_core::FileChange;

pub type Error = Box<dyn std::error::Error>;

/// 评论以此开头（去掉前导空白）才入队。
pub const COMMAND: &str = "/ziyuan";
/// 默认的入队标签。
pub const DEFAULT_LABEL: &str = "ziyuan";
/// GitHub 评论正文上限为 65536 字符，留些余量。
const MAX_COMMENT: usize = 60_000;
/// 查找已发评论时至多翻的页数（每页 100 条）。
const MAX_COMMENT_PAGES: u32 = 10;

/// 外发的评论与 PR 正文末尾的隐藏标记：带上驿传的幂等键，重试时据此认出已发出的那件。
pub fn marker(op: &str) -> String {
    format!("<!-- ziyuan-op: {op} -->")
}

/// 草稿 PR 那个提交的说明末尾的尾注，据此认出远端分支是否为同一次驿传所建。
fn op_trailer(op: &str) -> String {
    format!("Ziyuan-Op: {op}")
}

pub struct GitHub {
    api: String,
    repo: String,
    token: String,
    agent: ureq::Agent,
}

/// 一件 issue 或 PR（GitHub 的 issues 接口两者都返回）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub pull_request: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    pub id: u64,
    pub body: String,
    pub url: String,
    /// 作者是仓库所有者、组织成员或协作者；只有他们的 `/ziyuan` 评论入队。
    pub member: bool,
}

/// 待入队的一件：驿站名（含来处，用于去重）与诏令内容。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dispatch {
    pub station: String,
    pub text: String,
}

/// 驿站名：`github:<owner/repo>#<编号>`，评论再加 `/c<评论ID>`。
pub fn station(repo: &str, number: u64, comment: Option<u64>) -> String {
    match comment {
        Some(id) => format!("github:{repo}#{number}/c{id}"),
        None => format!("github:{repo}#{number}"),
    }
}

/// 从驿站名取出仓库与 issue / PR 编号；不是 GitHub 驿站则为 `None`。
pub fn parse_station(station: &str) -> Option<(&str, u64)> {
    let rest = station.strip_prefix("github:")?;
    let (repo, tail) = rest.rsplit_once('#')?;
    let number = tail.split('/').next()?.parse().ok()?;
    Some((repo, number))
}

/// 一件带标签的 issue / PR 应入队的诏令：本身一件，另加成员以 `/ziyuan` 开头的每条评论各一件。
pub fn intake(repo: &str, issue: &Issue, comments: &[Comment]) -> Vec<Dispatch> {
    let kind = if issue.pull_request { "PR" } else { "issue" };
    let mut out = vec![Dispatch {
        station: station(repo, issue.number, None),
        text: format!(
            "GitHub {kind} #{}：{}\n\n{}",
            issue.number,
            issue.title.trim(),
            issue.body.trim()
        )
        .trim_end()
        .to_owned(),
    }];
    for c in comments.iter().filter(|c| c.member) {
        let Some(ask) = c.body.trim_start().strip_prefix(COMMAND) else {
            continue;
        };
        let ask = ask.trim();
        if ask.is_empty() {
            continue;
        }
        out.push(Dispatch {
            station: station(repo, issue.number, Some(c.id)),
            text: format!(
                "GitHub {kind} #{}（{}）下的评论：{ask}",
                issue.number,
                issue.title.trim()
            ),
        });
    }
    out
}

/// 路径按段百分号编码（保留 `/`）。
fn encode_path(path: &str) -> String {
    let mut out = String::new();
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl GitHub {
    /// `ZIYUAN_GITHUB_REPO`（`owner/repo`）未设时为 `Ok(None)`。令牌取 `ZIYUAN_GITHUB_TOKEN`，
    /// 否则 `GITHUB_TOKEN`；`ZIYUAN_GITHUB_API` 可改 API 地址（GitHub Enterprise）。
    pub fn from_env() -> Result<Option<Self>, Error> {
        let Ok(repo) = std::env::var("ZIYUAN_GITHUB_REPO") else {
            return Ok(None);
        };
        let token = std::env::var("ZIYUAN_GITHUB_TOKEN")
            .or_else(|_| std::env::var("GITHUB_TOKEN"))
            .map_err(
                |_| "已设 ZIYUAN_GITHUB_REPO，但缺少 ZIYUAN_GITHUB_TOKEN（或 GITHUB_TOKEN）",
            )?;
        let api =
            std::env::var("ZIYUAN_GITHUB_API").unwrap_or_else(|_| "https://api.github.com".into());
        Self::new(api, repo, token).map(Some)
    }

    pub fn new(
        api: impl Into<String>,
        repo: impl Into<String>,
        token: String,
    ) -> Result<Self, Error> {
        let repo = repo.into();
        let valid = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        };
        match repo.split_once('/') {
            Some((owner, name)) if valid(owner) && valid(name) => {}
            _ => return Err(format!("仓库须为 owner/repo：{repo}").into()),
        }
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Self {
            api: api.into().trim_end_matches('/').to_owned(),
            repo,
            token,
            agent,
        })
    }

    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// 调用 `/repos/<repo><path>`；非 2xx 返回错误（含状态与 GitHub 的说明，不含令牌）。
    fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, Error> {
        self.call_optional(method, path, body)?
            .ok_or_else(|| format!("GitHub 返回 404 Not Found（{method} {path}）").into())
    }

    /// 同 [`Self::call`]，但 404 为 `Ok(None)`（如查找尚不存在的分支）。
    fn call_optional(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<Option<Value>, Error> {
        let url = format!("{}/repos/{}{path}", self.api, self.repo);
        let auth = format!("Bearer {}", self.token);
        let response = match body {
            None => self
                .agent
                .get(&url)
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .header("User-Agent", "ziyuan")
                .header("Authorization", &auth)
                .call(),
            Some(body) => {
                let request = match method {
                    "PATCH" => self.agent.patch(&url),
                    _ => self.agent.post(&url),
                };
                request
                    .header("Accept", "application/vnd.github+json")
                    .header("X-GitHub-Api-Version", "2022-11-28")
                    .header("User-Agent", "ziyuan")
                    .header("Authorization", &auth)
                    .header("Content-Type", "application/json")
                    .send(body.to_string())
            }
        }
        .map_err(|e| format!("GitHub 请求失败（{method} {path}）：{e}"))?;
        let status = response.status();
        let text = response.into_body().read_to_string().unwrap_or_default();
        if status.as_u16() == 404 {
            return Ok(None);
        }
        if !status.is_success() {
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["message"].as_str().map(str::to_owned))
                .unwrap_or_else(|| text.chars().take(200).collect());
            return Err(format!("GitHub 返回 {status}（{method} {path}）：{message}").into());
        }
        Ok(Some(serde_json::from_str(&text).unwrap_or(Value::Null)))
    }

    /// 文件在 `commit` 时的原文；不存在为 `None`。
    fn file_at(&self, path: &str, commit: &str) -> Result<Option<String>, Error> {
        let url = format!(
            "{}/repos/{}/contents/{}?ref={commit}",
            self.api,
            self.repo,
            encode_path(path)
        );
        let response = self
            .agent
            .get(&url)
            .header("Accept", "application/vnd.github.raw+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "ziyuan")
            .header("Authorization", format!("Bearer {}", self.token))
            .call()
            .map_err(|e| format!("GitHub 请求失败（读取 {path}）：{e}"))?;
        match response.status().as_u16() {
            404 => Ok(None),
            200 => Ok(Some(response.into_body().read_to_string()?)),
            status => Err(format!("GitHub 返回 {status}（读取 {path}）").into()),
        }
    }

    /// 带 `label` 的未关闭 issue 与 PR（至多 100 件）。
    pub fn labeled(&self, label: &str) -> Result<Vec<Issue>, Error> {
        let list = self.call(
            "GET",
            &format!(
                "/issues?state=open&per_page=100&labels={}",
                encode_path(label)
            ),
            None,
        )?;
        Ok(list
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|i| {
                Some(Issue {
                    number: i["number"].as_u64()?,
                    title: i["title"].as_str().unwrap_or_default().to_owned(),
                    body: i["body"].as_str().unwrap_or_default().to_owned(),
                    pull_request: !i["pull_request"].is_null(),
                })
            })
            .collect())
    }

    /// issue / PR 下的评论（至多 100 条）。
    pub fn comments(&self, number: u64) -> Result<Vec<Comment>, Error> {
        self.comments_page(number, 1)
    }

    /// 评论的第 `page` 页（每页 100 条，从 1 起，旧者在前）。
    fn comments_page(&self, number: u64, page: u32) -> Result<Vec<Comment>, Error> {
        let list = self.call(
            "GET",
            &format!("/issues/{number}/comments?per_page=100&page={page}"),
            None,
        )?;
        Ok(list
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|c| {
                Some(Comment {
                    id: c["id"].as_u64()?,
                    body: c["body"].as_str().unwrap_or_default().to_owned(),
                    url: c["html_url"].as_str().unwrap_or_default().to_owned(),
                    member: matches!(
                        c["author_association"].as_str(),
                        Some("OWNER" | "MEMBER" | "COLLABORATOR")
                    ),
                })
            })
            .collect())
    }

    /// 在 issue / PR 下发评论，返回评论网址。过长的正文截断。正文末尾带幂等键 `op` 的
    /// 隐藏标记；已有带同一标记的评论（上次发出而未及记账）则不再发，返回其网址。
    pub fn comment(&self, number: u64, body: &str, op: &str) -> Result<String, Error> {
        let mark = marker(op);
        for page in 1..=MAX_COMMENT_PAGES {
            let comments = self.comments_page(number, page)?;
            if let Some(found) = comments.iter().find(|c| c.body.contains(&mark)) {
                return Ok(found.url.clone());
            }
            if comments.len() < 100 {
                break;
            }
        }
        let body: String = match body.char_indices().nth(MAX_COMMENT) {
            Some((at, _)) => format!("{}\n\n……（下略）", &body[..at]),
            None => body.to_owned(),
        };
        let body = format!("{body}\n\n{mark}");
        let made = self.call(
            "POST",
            &format!("/issues/{number}/comments"),
            Some(json!({ "body": body })),
        )?;
        Ok(made["html_url"].as_str().unwrap_or_default().to_owned())
    }

    /// 以默认分支最新提交为底，在远端新建分支 `branch`、一次提交写入 `files` 的 `after`，
    /// 再开草稿 PR，返回 PR 网址。先核对每个文件的远端原文与 `before` 一致（新建的须不存在），
    /// 不一致则不开，以免 PR 悄悄冲掉远端的改动。
    /// `message` 为 PR 里那个提交的说明（与本地提交的相同，便于对应），末尾另加 `Ziyuan-Op` 尾注。
    ///
    /// 按幂等键 `op` 续作：`branch` 上已有正文带同一标记的 PR 就返回它；分支已由同一 `op` 建好
    /// （提交带同一尾注）而 PR 未开，就只开 PR。分支或 PR 已存在却不是这次驿传所建，则报错、不覆盖。
    pub fn draft_pull_request(
        &self,
        branch: &str,
        title: &str,
        body: &str,
        message: &str,
        files: &[FileChange],
        op: &str,
    ) -> Result<String, Error> {
        if files.is_empty() {
            return Err("没有可提交的改动".into());
        }
        let mark = marker(op);
        let owner = self.repo.split_once('/').map_or("", |(o, _)| o);
        let open = self.call(
            "GET",
            &format!("/pulls?state=all&head={owner}:{}", encode_path(branch)),
            None,
        )?;
        if let Some(pr) = open.as_array().and_then(|list| list.first()) {
            if pr["body"].as_str().is_some_and(|b| b.contains(&mark)) {
                return Ok(pr["html_url"].as_str().unwrap_or_default().to_owned());
            }
            return Err(format!("远端分支 {branch} 已有 PR，不是这次驿传所开，不另开").into());
        }
        let repo = self.call("GET", "", None)?;
        let base = repo["default_branch"]
            .as_str()
            .ok_or("GitHub 未返回默认分支")?
            .to_owned();
        let existing = self.call_optional(
            "GET",
            &format!("/git/ref/heads/{}", encode_path(branch)),
            None,
        )?;
        match existing {
            Some(head) => {
                let sha = head["object"]["sha"]
                    .as_str()
                    .ok_or("GitHub 未返回分支提交")?;
                let commit = self.call("GET", &format!("/git/commits/{sha}"), None)?;
                let ours = commit["message"]
                    .as_str()
                    .is_some_and(|m| m.lines().any(|l| l.trim() == op_trailer(op)));
                if !ours {
                    return Err(format!("远端已有分支 {branch}，不是这次驿传所建，不覆盖").into());
                }
            }
            None => self.push_branch(&base, branch, message, files, op)?,
        }
        let pr = self.call(
            "POST",
            "/pulls",
            Some(json!({ "title": title, "head": branch, "base": base, "body": format!("{body}\n\n{mark}"), "draft": true })),
        )?;
        Ok(pr["html_url"].as_str().unwrap_or_default().to_owned())
    }

    /// 核对远端原文后，以 `base` 最新提交为底建一个提交，并建分支 `branch` 指向它。
    fn push_branch(
        &self,
        base: &str,
        branch: &str,
        message: &str,
        files: &[FileChange],
        op: &str,
    ) -> Result<(), Error> {
        let head = self.call(
            "GET",
            &format!("/git/ref/heads/{}", encode_path(base)),
            None,
        )?;
        let parent = head["object"]["sha"]
            .as_str()
            .ok_or("GitHub 未返回分支提交")?
            .to_owned();
        let commit = self.call("GET", &format!("/git/commits/{parent}"), None)?;
        let tree = commit["tree"]["sha"]
            .as_str()
            .ok_or("GitHub 未返回目录树")?
            .to_owned();
        for file in files {
            if self.file_at(&file.path, &parent)? != file.before {
                return Err(format!(
                    "远端 {base} 上的 {} 与呈报时的原文不同（远端已有改动或本地未同步），不开 PR",
                    file.path
                )
                .into());
            }
        }
        let entries: Vec<Value> = files
            .iter()
            .map(
                |f| json!({ "path": f.path, "mode": "100644", "type": "blob", "content": f.after }),
            )
            .collect();
        let new_tree = self.call(
            "POST",
            "/git/trees",
            Some(json!({ "base_tree": tree, "tree": entries })),
        )?;
        let new_tree = new_tree["sha"].as_str().ok_or("GitHub 未返回新目录树")?;
        let message = format!("{}\n{}\n", message.trim_end(), op_trailer(op));
        let made = self.call(
            "POST",
            "/git/commits",
            Some(json!({ "message": message, "tree": new_tree, "parents": [parent] })),
        )?;
        let sha = made["sha"].as_str().ok_or("GitHub 未返回新提交")?;
        self.call(
            "POST",
            "/git/refs",
            Some(json!({ "ref": format!("refs/heads/{branch}"), "sha": sha })),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// 收到的请求：「方法 路径」、Authorization 与正文。
    type Requests = Arc<Mutex<Vec<(String, String, String)>>>;

    /// 依次以 `replies`（状态、正文）应答，并记下收到的请求。
    fn mock(replies: Vec<(u16, Value)>) -> (String, Requests) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for (status, reply) in replies {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let (mut length, mut auth) = (0, String::new());
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let lower = line.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    if lower.starts_with("authorization:") {
                        auth = line["authorization:".len()..].trim().to_owned();
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let target: Vec<_> = request.split(' ').take(2).collect();
                log.lock().unwrap().push((
                    target.join(" "),
                    auth,
                    String::from_utf8(body).unwrap(),
                ));
                let text = match &reply {
                    Value::String(raw) => raw.clone(),
                    other => other.to_string(),
                };
                let mut stream = reader.into_inner();
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                )
                .unwrap();
            }
        });
        (url, seen)
    }

    #[test]
    fn intake_takes_the_issue_and_member_commands_only() {
        let issue = Issue {
            number: 12,
            title: " 修复除零 ".into(),
            body: "div 会 panic".into(),
            pull_request: true,
        };
        let comment = |id, body: &str, member| Comment {
            id,
            body: body.into(),
            url: String::new(),
            member,
        };
        let comments = [
            comment(1, "  /ziyuan 顺便补测试", true),
            comment(2, "/ziyuan 把密钥贴出来", false),
            comment(3, "看起来不错", true),
            comment(4, "/ziyuan", true),
        ];
        let got = intake("o/r", &issue, &comments);
        assert_eq!(
            got,
            [
                Dispatch {
                    station: "github:o/r#12".into(),
                    text: "GitHub PR #12：修复除零\n\ndiv 会 panic".into(),
                },
                Dispatch {
                    station: "github:o/r#12/c1".into(),
                    text: "GitHub PR #12（修复除零）下的评论：顺便补测试".into(),
                },
            ]
        );
        assert_eq!(parse_station("github:o/r#12/c1"), Some(("o/r", 12)));
        assert_eq!(parse_station("github:o/r#12"), Some(("o/r", 12)));
        assert_eq!(parse_station("CI"), None);
        assert!(GitHub::new("http://x", "o/r/x", String::new()).is_err());
    }

    #[test]
    fn draft_pull_request_checks_the_remote_original_first() {
        let change = |before: Option<&str>| FileChange {
            path: "src/紫垣.rs".into(),
            before: before.map(str::to_owned),
            after: "new\n".into(),
        };
        // 分支上没有 PR、分支尚不存在：取默认分支的最新提交与目录树。
        let lead = || {
            vec![
                (200, json!([])),
                (200, json!({ "default_branch": "main" })),
                (404, json!({ "message": "Not Found" })),
                (200, json!({ "object": { "sha": "c0" } })),
                (200, json!({ "tree": { "sha": "t0" } })),
            ]
        };
        // 远端原文已变：不开 PR，不建分支。
        let mut replies = lead();
        replies.push((200, Value::String("someone else\n".into())));
        let (url, seen) = mock(replies);
        let hub = GitHub::new(url, "o/r", "ghp_secret".into()).unwrap();
        let err = hub
            .draft_pull_request(
                "ziyuan/task-1",
                "t",
                "b",
                "m",
                &[change(Some("old\n"))],
                "op1",
            )
            .unwrap_err();
        assert!(err.to_string().contains("与呈报时的原文不同"), "{err}");
        assert!(!err.to_string().contains("ghp_secret"));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 6);
        assert_eq!(
            seen[0].0,
            "GET /repos/o/r/pulls?state=all&head=o:ziyuan/task-1"
        );
        assert_eq!(seen[2].0, "GET /repos/o/r/git/ref/heads/ziyuan/task-1");
        assert_eq!(
            seen[5].0,
            "GET /repos/o/r/contents/src/%E7%B4%AB%E5%9E%A3.rs?ref=c0"
        );
        assert_eq!(seen[5].1, "Bearer ghp_secret");

        // 原文一致：建树、提交（带 Ziyuan-Op 尾注）、分支，再开草稿 PR（正文带标记）。
        let mut replies = lead();
        replies.extend([
            (200, Value::String("old\n".into())),
            (201, json!({ "sha": "t1" })),
            (201, json!({ "sha": "c1" })),
            (201, json!({ "ref": "refs/heads/ziyuan/task-1" })),
            (201, json!({ "html_url": "https://github.com/o/r/pull/9" })),
        ]);
        let (url, seen) = mock(replies);
        let hub = GitHub::new(url, "o/r", "ghp_secret".into()).unwrap();
        let pr = hub
            .draft_pull_request(
                "ziyuan/task-1",
                "标题",
                "正文",
                "说明\n\nZiyuan-Edict: 1\n",
                &[change(Some("old\n"))],
                "op1",
            )
            .unwrap();
        assert_eq!(pr, "https://github.com/o/r/pull/9");
        let seen = seen.lock().unwrap();
        let body = |i: usize| serde_json::from_str::<Value>(&seen[i].2).unwrap();
        assert_eq!(seen[6].0, "POST /repos/o/r/git/trees");
        assert_eq!(body(6)["base_tree"], "t0");
        assert_eq!(body(6)["tree"][0]["content"], "new\n");
        assert_eq!(body(7)["parents"], json!(["c0"]));
        assert_eq!(
            body(7)["message"],
            "说明\n\nZiyuan-Edict: 1\nZiyuan-Op: op1\n"
        );
        assert_eq!(body(8)["ref"], "refs/heads/ziyuan/task-1");
        assert_eq!(seen[9].0, "POST /repos/o/r/pulls");
        assert_eq!(
            (body(9)["draft"].clone(), body(9)["base"].clone()),
            (json!(true), json!("main"))
        );
        assert_eq!(body(9)["body"], format!("正文\n\n{}", marker("op1")));
    }

    #[test]
    fn draft_pull_request_resumes_the_same_dispatch_only() {
        let files = [FileChange {
            path: "a.rs".into(),
            before: None,
            after: "x".into(),
        }];
        let open = |hub: &GitHub, op| hub.draft_pull_request("b", "t", "正文", "m", &files, op);
        // 上次已开出 PR 而未及记账：认出标记，直接返回，不再开。
        let (url, seen) = mock(vec![(
            200,
            json!([{ "html_url": "https://github.com/o/r/pull/3", "body": format!("正文\n\n{}", marker("op1")) }]),
        )]);
        let hub = GitHub::new(url, "o/r", "x".into()).unwrap();
        assert_eq!(open(&hub, "op1").unwrap(), "https://github.com/o/r/pull/3");
        assert_eq!(seen.lock().unwrap().len(), 1);

        // 分支已由同一驿传建好而 PR 未开：不再建树与提交，只开 PR。
        let (url, seen) = mock(vec![
            (200, json!([])),
            (200, json!({ "default_branch": "main" })),
            (200, json!({ "object": { "sha": "c1" } })),
            (200, json!({ "message": "m\nZiyuan-Op: op1\n" })),
            (201, json!({ "html_url": "https://github.com/o/r/pull/4" })),
        ]);
        let hub = GitHub::new(url, "o/r", "x".into()).unwrap();
        assert_eq!(open(&hub, "op1").unwrap(), "https://github.com/o/r/pull/4");
        let seen = seen.lock().unwrap();
        assert_eq!(seen[3].0, "GET /repos/o/r/git/commits/c1");
        assert_eq!(seen[4].0, "POST /repos/o/r/pulls");

        // 分支或 PR 不是这次驿传所建：不覆盖、不另开。
        let (url, _) = mock(vec![
            (200, json!([])),
            (200, json!({ "default_branch": "main" })),
            (200, json!({ "object": { "sha": "c9" } })),
            (200, json!({ "message": "别人的提交\nZiyuan-Op: op0\n" })),
        ]);
        let hub = GitHub::new(url, "o/r", "x".into()).unwrap();
        let err = open(&hub, "op1").unwrap_err().to_string();
        assert!(err.contains("不是这次驿传所建"), "{err}");
        let (url, _) = mock(vec![(
            200,
            json!([{ "html_url": "u", "body": "别人开的" }]),
        )]);
        let hub = GitHub::new(url, "o/r", "x".into()).unwrap();
        let err = open(&hub, "op1").unwrap_err().to_string();
        assert!(err.contains("不是这次驿传所开"), "{err}");
    }

    #[test]
    fn comments_are_posted_once_per_dispatch() {
        let comment = |id: u64, body: String| json!({ "id": id, "body": body, "html_url": format!("https://github.com/o/r/issues/5#c{id}") });
        // 已有带同一标记的评论：不再发。
        let (url, seen) = mock(vec![(
            200,
            json!([
                comment(1, "旁人".into()),
                comment(2, format!("回奏\n\n{}", marker("op7")))
            ]),
        )]);
        let hub = GitHub::new(url, "o/r", "x".into()).unwrap();
        assert_eq!(
            hub.comment(5, "回奏", "op7").unwrap(),
            "https://github.com/o/r/issues/5#c2"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].0,
            "GET /repos/o/r/issues/5/comments?per_page=100&page=1"
        );
        drop(seen);

        // 没有：发出，正文末尾带标记。
        let (url, seen) = mock(vec![
            (
                200,
                json!([comment(1, format!("旧的\n\n{}", marker("op6")))]),
            ),
            (
                201,
                json!({ "html_url": "https://github.com/o/r/issues/5#c3" }),
            ),
        ]);
        let hub = GitHub::new(url, "o/r", "x".into()).unwrap();
        assert_eq!(
            hub.comment(5, "回奏", "op7").unwrap(),
            "https://github.com/o/r/issues/5#c3"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen[1].0, "POST /repos/o/r/issues/5/comments");
        let posted: Value = serde_json::from_str(&seen[1].2).unwrap();
        assert_eq!(posted["body"], format!("回奏\n\n{}", marker("op7")));
    }

    #[test]
    fn new_files_must_be_absent_remotely() {
        let (url, _) = mock(vec![
            (200, json!([])),
            (200, json!({ "default_branch": "main" })),
            (404, json!({ "message": "Not Found" })),
            (200, json!({ "object": { "sha": "c0" } })),
            (200, json!({ "tree": { "sha": "t0" } })),
            (200, Value::String("already here".into())),
        ]);
        let hub = GitHub::new(url, "o/r", "x".into()).unwrap();
        let new = FileChange {
            path: "NEW.md".into(),
            before: None,
            after: "hi".into(),
        };
        assert!(
            hub.draft_pull_request("b", "t", "", "m", &[new], "op")
                .is_err()
        );
    }
}
