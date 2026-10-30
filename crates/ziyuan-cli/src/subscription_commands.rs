//! `ziyuan follow`：本地的作品关注与更新列表。不开朝廷会话、不读模型配置；
//! 只有 `follow check` 联网，且只查询固定的 syosetu 官方作品信息 API，不下载正文。
//! 开启「新章节自动入队」时，`follow check` 之后才打开朝廷，把新章节提醒入兵部驿站。

use serde_json::{Value, json};
use std::env;
use std::error::Error;
use std::time::{SystemTime, UNIX_EPOCH};
use ziyuan_app::subscriptions::{
    CheckMode, CheckStatus, CheckSummary, Notice, NoticeKind, ObservedWork, SourceError,
    Subscription, SubscriptionError, Subscriptions, SyosetuSource, WorkSource, normalize_code,
};
use ziyuan_app::{FollowCourier, FollowCourierStatus, follow_courier, format_time, tz_offset};

pub(crate) const FOLLOW_HELP: &str = r#"作品关注（不需要 ziyuan init，不调用模型；只查作品信息，不下载正文）：
  ziyuan follow add <作品链接|作品代码>
                                       关注 syosetu 作品，如 https://ncode.syosetu.com/n1234ab/ 或 n1234ab
                                       （不接受章节链接）；只写本地列表，不联网；已关注的不做改动
  ziyuan follow list                   列出关注的作品、上次记录的章节数与上次检查的错误
  ziyuan follow remove <作品链接|作品代码>
                                       取消关注，并删除该作品的更新提醒
  ziyuan follow check                  立即向 syosetu 官方 API 查询作品信息；首次检查只记录基线，
                                       之后章节数或作品信息变化才产生提醒。每次最多 50 部，
                                       同一作品两次检查至少间隔 60 秒；有作品检查失败时退出码 1
  ziyuan follow updates [--all]        列出未读的更新提醒；--all 连同已读的一起列出
  ziyuan follow read <提醒ID>          把一条提醒标为已读
  ziyuan follow queue [on|off]         查看或设置「新章节自动入队」（默认关闭）。开启后 follow check
                                       与 Web 的检查把未读的新章节提醒入兵部驿站（交工部，只入队不执行），
                                       站名 syosetu:<作品代码>#<章节数>，同一站名只入队一次；
                                       朝廷尚未建立（ziyuan init）或入队失败时不影响检查，下次检查再补
  取消未关注的作品或标记不存在的提醒时，退出码 1（--json 仍输出 "found": false）。
  以上命令均可加 --json，以 JSON 输出到标准输出。列表存于 .ziyuan/subscriptions/state.json。
  此命令退出后不会在后台检查；需要定时检查时，在运行中的 Web 服务中开启。
  官方信息约有数分钟至两小时的延迟，R18 与被搜索排除的作品查不到。"#;

pub(crate) enum FollowAction {
    Help,
    Add(String),
    List,
    Remove(String),
    Check,
    Updates {
        all: bool,
    },
    Read(String),
    /// `None` 只查看当前设置。
    Queue(Option<bool>),
}

const ADD_USAGE: &str = "用法: ziyuan follow add <作品链接|作品代码>";
const REMOVE_USAGE: &str = "用法: ziyuan follow remove <作品链接|作品代码>";
const UPDATES_USAGE: &str = "用法: ziyuan follow updates [--all]";
const READ_USAGE: &str = "用法: ziyuan follow read <提醒ID>";
const QUEUE_USAGE: &str = "用法: ziyuan follow queue [on|off]";
/// 与 `subscriptions::check` 的单次上限与手动冷却一致。
const BATCH: usize = 50;
const COOLDOWN: u64 = 60;

pub(crate) fn parse(args: Vec<String>) -> Result<FollowAction, Box<dyn Error>> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(FollowAction::Help);
    }
    let Some((action, rest)) = args.split_first() else {
        return Ok(FollowAction::Help);
    };
    Ok(match action.as_str() {
        "help" => FollowAction::Help,
        "add" => FollowAction::Add(code_arg(rest, ADD_USAGE)?),
        "remove" => FollowAction::Remove(code_arg(rest, REMOVE_USAGE)?),
        "list" if rest.is_empty() => FollowAction::List,
        "list" => return Err("用法: ziyuan follow list".into()),
        "check" if rest.is_empty() => FollowAction::Check,
        "check" => {
            return Err("用法: ziyuan follow check（只查询固定的官方 API，不接受其他参数）".into());
        }
        "updates" => match rest {
            [] => FollowAction::Updates { all: false },
            [flag] if flag == "--all" => FollowAction::Updates { all: true },
            [a, b] if a == "--all" && b == "--all" => return Err("--all 只能给一次".into()),
            _ => return Err(UPDATES_USAGE.into()),
        },
        "queue" => match rest {
            [] => FollowAction::Queue(None),
            [v] if v == "on" => FollowAction::Queue(Some(true)),
            [v] if v == "off" => FollowAction::Queue(Some(false)),
            _ => return Err(QUEUE_USAGE.into()),
        },
        "read" => match rest {
            [id] if id.len() == 32
                && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
            {
                FollowAction::Read(id.clone())
            }
            [id] if !id.starts_with("--") => {
                return Err(format!(
                    "提醒 ID 应为 32 位小写十六进制（见 ziyuan follow updates）：{id}"
                )
                .into());
            }
            _ => return Err(READ_USAGE.into()),
        },
        other => {
            return Err(
                format!("未知的 follow 子命令：{other}。运行 ziyuan follow help 查看用法").into(),
            );
        }
    })
}

fn code_arg(rest: &[String], usage: &str) -> Result<String, Box<dyn Error>> {
    match rest {
        [input] if !input.starts_with("--") => normalize_code(input).map_err(|_| {
            format!(
                "作品链接或代码无效：{input}。应为作品目录链接 https://ncode.syosetu.com/n1234ab/ \
                 或作品代码 n1234ab（不接受章节链接）"
            )
            .into()
        }),
        _ => Err(usage.into()),
    }
}

pub(crate) fn run(action: FollowAction, json: bool) -> Result<(), Box<dyn Error>> {
    if let FollowAction::Help = action {
        println!("{FOLLOW_HELP}");
        return Ok(());
    }
    let root = env::current_dir()?;
    let service = Subscriptions::new(&root);
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    match action {
        FollowAction::Help => unreachable!("已在上面处理"),
        FollowAction::Add(code) => {
            let existed = service
                .list()
                .map_err(failure)?
                .iter()
                .any(|s| s.code == code);
            let item = service.add(&code, now).map_err(failure)?;
            match json {
                true => print_json(json!({ "added": !existed, "subscription": view(&item) }))?,
                false if existed => println!("{} 已在关注列表中，未做改动。", item.code),
                false => {
                    println!("已关注 {}（{}）。", item.code, url(&item.code));
                    println!(
                        "尚未联网；运行 `ziyuan follow check` 记录当前章节数作为基线，之后的变化才会提醒。"
                    );
                }
            }
        }
        FollowAction::List => {
            let items = service.list().map_err(failure)?;
            let unread = service.updates(true).map_err(failure)?.len();
            match json {
                true => print_json(json!({
                    "subscriptions": items.iter().map(view).collect::<Vec<_>>(),
                    "unread": unread,
                }))?,
                false => print_list(&items, unread),
            }
        }
        FollowAction::Remove(code) => {
            let found = service.remove(&code).map_err(failure)?;
            match json {
                true => print_json(json!({ "code": code, "found": found }))?,
                false if found => println!("已取消关注 {code}，并删除它的更新提醒。"),
                false => {}
            }
            if !found {
                return Err(format!("未关注 {code}，无需取消").into());
            }
        }
        FollowAction::Check => {
            let report = check(&service, &SyosetuSource::new(), now).map_err(failure)?;
            // 入队的成败只写进报告，不改变检查的退出码。
            let courier = follow_courier(&root);
            match json {
                true => {
                    let mut value = report.to_json();
                    value["courier"] = serde_json::to_value(&courier)?;
                    print_json(value)?
                }
                false => {
                    report.print();
                    print_courier(&courier);
                }
            }
            if report.summary.failed > 0 {
                return Err(format!(
                    "{} 部作品检查失败（见上方报告）；其余结果已保存",
                    report.summary.failed
                )
                .into());
            }
        }
        FollowAction::Updates { all } => {
            let notices = service.updates(!all).map_err(failure)?;
            match json {
                true => print_json(json!({
                    "all": all,
                    "notices": notices.iter().map(notice_view).collect::<Vec<_>>(),
                }))?,
                false => print_updates(&notices, all),
            }
        }
        FollowAction::Queue(value) => {
            let settings = match value {
                Some(on) => service
                    .update_settings(|s| s.queue_new_chapters = on)
                    .map_err(failure)?,
                None => service.settings().map_err(failure)?,
            };
            let on = settings.queue_new_chapters;
            match json {
                true => print_json(json!({ "queue_new_chapters": on }))?,
                false if on => println!(
                    "新章节自动入队：开启。检查发现新章节时入兵部驿站，交工部（只入队不执行）。"
                ),
                false => println!("新章节自动入队：关闭。开启：ziyuan follow queue on"),
            }
        }
        FollowAction::Read(id) => {
            let found = service.read(&id).map_err(failure)?;
            match json {
                true => print_json(json!({ "id": id, "found": found }))?,
                false if found => println!("已标为已读：{id}"),
                false => {}
            }
            if !found {
                return Err(format!("没有这条提醒：{id}（见 ziyuan follow updates --all）").into());
            }
        }
    }
    Ok(())
}

fn failure(error: SubscriptionError) -> Box<dyn Error> {
    match error {
        SubscriptionError::Storage(e) => {
            format!("关注列表 .ziyuan/subscriptions/state.json：{e}").into()
        }
        SubscriptionError::InvalidInput => "作品链接或代码无效".into(),
        SubscriptionError::Random => "无法从系统取得随机数，未生成 ID".into(),
    }
}

fn print_json(value: Value) -> Result<(), Box<dyn Error>> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn url(code: &str) -> String {
    format!("https://ncode.syosetu.com/{code}/")
}

fn when(at: u64) -> String {
    format_time(at, tz_offset())
}

pub(crate) fn source_message(error: &SourceError) -> &'static str {
    match error {
        SourceError::NotFound => "未找到作品（可能已删除、为 R18 作品或被搜索排除）",
        SourceError::Timeout => "查询超时",
        SourceError::Malformed => "返回的数据无法识别",
        SourceError::RateLimited => "请求过于频繁，被暂时限制",
        SourceError::Unavailable => "服务暂时不可用或网络不通",
        SourceError::Suspicious => "章节数或更新时间比上次记录的少，疑为异常，保留上次的记录",
    }
}

fn error_code(error: &SourceError) -> Value {
    serde_json::to_value(error).unwrap_or(Value::Null)
}

fn work_view(work: &ObservedWork) -> Value {
    json!({
        "title": work.title,
        "author": work.author,
        "chapter_count": work.chapter_count,
        "last_published": work.last_published,
        "work_updated": work.work_updated,
    })
}

fn view(item: &Subscription) -> Value {
    json!({
        "id": item.id,
        "code": item.code,
        "url": url(&item.code),
        "added_at": item.added_at,
        "work": item.baseline.as_ref().map(work_view),
        "last_attempt": item.last_attempt,
        "last_error": item.last_error.as_ref().map(error_code),
        "last_error_message": item.last_error.as_ref().map(source_message),
        "consecutive_failures": item.consecutive_failures,
        "next_check": item.next_check,
    })
}

fn notice_view(notice: &Notice) -> Value {
    let (kind, increase) = match notice.kind {
        NoticeKind::NewChapters { observed_increase } => ("new_chapters", Some(observed_increase)),
        NoticeKind::InformationChanged => ("information_changed", None),
    };
    json!({
        "id": notice.id,
        "subscription_id": notice.subscription_id,
        "code": notice.code,
        "url": url(&notice.code),
        "observed_at": notice.observed_at,
        "kind": kind,
        "observed_increase": increase,
        "work": work_view(&notice.observation),
        "read": notice.read,
    })
}

fn print_list(items: &[Subscription], unread: usize) {
    if items.is_empty() {
        println!("尚未关注任何作品。用 `ziyuan follow add <作品链接|作品代码>` 添加。");
        return;
    }
    println!("关注的作品（{}）：", items.len());
    for item in items {
        match &item.baseline {
            Some(work) => println!(
                "  {}  《{}》 {} · 共 {} 章 · 最近发布 {}",
                item.code,
                work.title,
                work.author,
                work.chapter_count,
                when(work.last_published)
            ),
            None => println!(
                "  {}  尚未检查（下次 `ziyuan follow check` 时记录基线）",
                item.code
            ),
        }
        println!("      {}", url(&item.code));
        if let Some(error) = &item.last_error {
            println!(
                "      上次检查失败：{}（连续 {} 次）",
                source_message(error),
                item.consecutive_failures
            );
        }
    }
    if unread > 0 {
        println!("未读更新 {unread} 条：ziyuan follow updates");
    }
}

fn print_updates(notices: &[Notice], all: bool) {
    if notices.is_empty() {
        match all {
            true => println!("还没有任何更新提醒。"),
            false => println!("没有未读的更新提醒。"),
        }
        return;
    }
    match all {
        true => println!("全部更新提醒（{}）：", notices.len()),
        false => println!("未读更新提醒（{}）：", notices.len()),
    }
    for notice in notices {
        let work = &notice.observation;
        let change = match notice.kind {
            NoticeKind::NewChapters { observed_increase } => format!(
                "比上次多 {observed_increase} 章（现共 {} 章）",
                work.chapter_count
            ),
            NoticeKind::InformationChanged => {
                format!("作品信息有变化（现共 {} 章）", work.chapter_count)
            }
        };
        let state = if notice.read { "已读" } else { "未读" };
        println!(
            "  [{state}] {}  《{}》 {} · {change} · 发现于 {}",
            notice.code,
            work.title,
            work.author,
            when(notice.observed_at)
        );
        println!("      提醒 ID {} · {}", notice.id, url(&notice.code));
    }
    if !all {
        println!("标为已读：ziyuan follow read <提醒ID>");
    }
}

fn print_courier(courier: &FollowCourier) {
    match courier.status {
        FollowCourierStatus::Disabled => return,
        FollowCourierStatus::NotFounded => {
            println!("驿站未入队：朝廷尚未建立（ziyuan init）；提醒保留，建立后下次检查再入队。");
            return;
        }
        FollowCourierStatus::Error => {
            println!(
                "驿站未入队：{}；提醒保留，下次检查再入队。",
                courier.message.as_deref().unwrap_or("未知错误")
            );
            return;
        }
        FollowCourierStatus::Done => {}
    }
    if !courier.queued.is_empty() {
        let ids: Vec<_> = courier
            .queued
            .iter()
            .map(|q| format!("#{}", q.task))
            .collect();
        println!(
            "驿站入队 {} 件：{}（交工部，只入队不执行）",
            courier.queued.len(),
            ids.join("、")
        );
    }
    if courier.already_queued > 0 {
        println!("已入队过 {} 件，不再重复入队。", courier.already_queued);
    }
    for error in &courier.errors {
        println!(
            "  - {} 未入队：{}（下次检查再试）",
            error.station, error.message
        );
    }
}

/// 一次手动检查的结果，附上服务摘要里没有分开的各类原因。
pub(crate) struct CheckReport {
    pub(crate) summary: CheckSummary,
    pub(crate) baselines: usize,
    pub(crate) cooldown: Vec<String>,
    /// 未进入本次检查、也不在冷却中的作品：超出单次上限，或正由另一次检查处理。
    pub(crate) deferred: Vec<String>,
    pub(crate) errors: Vec<(String, SourceError)>,
}

impl CheckReport {
    fn unchanged(&self) -> usize {
        let s = &self.summary;
        s.checked
            .saturating_sub(s.failed)
            .saturating_sub(s.notices)
            .saturating_sub(self.baselines)
    }

    /// 本次已领满上限，仍有作品没轮到：须再次运行。
    pub(crate) fn incomplete(&self) -> bool {
        self.summary.checked + self.summary.discarded >= BATCH && !self.deferred.is_empty()
    }

    fn to_json(&self) -> Value {
        let s = &self.summary;
        json!({
            "status": s.status,
            "checked": s.checked,
            "skipped": s.skipped,
            "failed": s.failed,
            "notices": s.notices,
            "discarded": s.discarded,
            "baselines": self.baselines,
            "unchanged": self.unchanged(),
            "cooldown": self.cooldown,
            "deferred": self.deferred,
            "incomplete": self.incomplete(),
            "errors": self.errors.iter().map(|(code, e)| json!({
                "code": code,
                "error": error_code(e),
                "message": source_message(e),
            })).collect::<Vec<_>>(),
        })
    }

    fn print(&self) {
        let s = &self.summary;
        match s.status {
            CheckStatus::Empty => {
                println!(
                    "关注列表为空，没有可检查的作品。用 `ziyuan follow add <作品链接|作品代码>` 添加。"
                );
                return;
            }
            CheckStatus::Disabled => {
                println!("定时检查未开启，本次没有检查。");
                return;
            }
            CheckStatus::NotDue => println!("本次没有检查任何作品（未联网）："),
            CheckStatus::Complete => {
                println!(
                    "已检查 {} 部作品：新提醒 {} 条，首次记录基线 {} 部，无变化 {} 部，失败 {} 部。",
                    s.checked,
                    s.notices,
                    self.baselines,
                    self.unchanged(),
                    s.failed
                );
                if self.baselines > 0 {
                    println!(
                        "  首次检查只记录当前章节数作为基线，不产生提醒；之后的变化才会提醒。"
                    );
                }
                for (code, error) in &self.errors {
                    println!(
                        "  - {code} 检查失败：{}（保留上次的记录与未读提醒）",
                        source_message(error)
                    );
                }
                if s.discarded > 0 {
                    println!(
                        "  - {} 部作品在检查期间被取消关注或重新添加，结果已丢弃",
                        s.discarded
                    );
                }
            }
        }
        if !self.cooldown.is_empty() {
            println!(
                "  - {} 部刚检查过，处于冷却中（同一作品两次检查至少间隔 {COOLDOWN} 秒）：{}",
                self.cooldown.len(),
                self.cooldown.join("、")
            );
        }
        if self.incomplete() {
            println!(
                "  - 每次最多检查 {BATCH} 部；还有 {} 部未检查：{}。请再次运行 `ziyuan follow check`。",
                self.deferred.len(),
                self.deferred.join("、")
            );
        } else if !self.deferred.is_empty() {
            println!(
                "  - {} 部正由另一次检查处理，已跳过：{}",
                self.deferred.len(),
                self.deferred.join("、")
            );
        }
        if s.notices > 0 {
            println!("查看提醒：ziyuan follow updates");
        }
    }
}

pub(crate) fn check(
    service: &Subscriptions,
    source: &impl WorkSource,
    now: u64,
) -> Result<CheckReport, SubscriptionError> {
    let before = service.list()?;
    let summary = service.check(source, now, CheckMode::Manual)?;
    let mut report = CheckReport {
        summary,
        baselines: 0,
        cooldown: Vec::new(),
        deferred: Vec::new(),
        errors: Vec::new(),
    };
    if report.summary.status != CheckStatus::Complete
        && report.summary.status != CheckStatus::NotDue
    {
        return Ok(report);
    }
    for item in service.list()? {
        let old = before.iter().find(|b| b.id == item.id);
        let old_attempt = old.and_then(|b| b.last_attempt);
        // 本次领取的作品，last_attempt 被改为 now；之前已是 now 的说明处于冷却中。
        let claimed = item.last_attempt == Some(now) && old_attempt != Some(now);
        if claimed {
            if let Some(error) = &item.last_error {
                report.errors.push((item.code.clone(), error.clone()));
            } else if old.is_some_and(|b| b.baseline.is_none()) && item.baseline.is_some() {
                report.baselines += 1;
            }
        } else if old_attempt.is_some_and(|at| now < at.saturating_add(COOLDOWN)) {
            report.cooldown.push(item.code);
        } else {
            report.deferred.push(item.code);
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new(name: &str) -> Self {
            let path = env::temp_dir().join(format!(
                "ziyuan-follow-unit-{name}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct Fake(Result<Vec<ObservedWork>, SourceError>);
    impl WorkSource for Fake {
        fn fetch(&self, codes: &[String]) -> Result<Vec<ObservedWork>, SourceError> {
            self.0.clone().map(|works| {
                works
                    .into_iter()
                    .filter(|w| codes.contains(&w.code))
                    .collect()
            })
        }
    }

    fn work(code: &str, chapters: u64) -> ObservedWork {
        ObservedWork {
            code: code.into(),
            title: "題名".into(),
            author: "作者".into(),
            chapter_count: chapters,
            last_published: 1_700_000_000,
            work_updated: 1_700_000_000,
        }
    }

    fn code(i: usize) -> String {
        format!("n{i:04}aa")
    }

    #[test]
    fn first_baseline_new_chapters_unchanged_and_errors_are_separate() {
        let temp = Temp::new("kinds");
        let service = Subscriptions::new(&temp.0);
        for c in ["n0001aa", "n0002aa", "n0003aa"] {
            service.add(c, 100).unwrap();
        }
        let first = Fake(Ok(vec![work("n0001aa", 1), work("n0002aa", 1)]));
        let report = check(&service, &first, 1000).unwrap();
        assert_eq!(report.baselines, 2);
        assert_eq!(report.summary.failed, 1);
        assert_eq!(
            report.errors,
            vec![("n0003aa".to_owned(), SourceError::NotFound)]
        );
        assert_eq!(report.unchanged(), 0);
        let value = report.to_json();
        assert_eq!(value["errors"][0]["error"], "not_found");
        assert!(
            value["errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains("未找到")
        );

        let second = Fake(Ok(vec![
            work("n0001aa", 3),
            work("n0002aa", 1),
            work("n0003aa", 4),
        ]));
        let report = check(&service, &second, 2000).unwrap();
        assert_eq!(report.summary.notices, 1);
        assert_eq!(report.baselines, 1);
        assert_eq!(report.unchanged(), 1);
        assert!(report.errors.is_empty() && !report.incomplete());

        // 冷却中的作品与「无变化」分开报告。
        let report = check(&service, &second, 2010).unwrap();
        assert_eq!(report.summary.status, CheckStatus::NotDue);
        assert_eq!(report.cooldown.len(), 3);
        assert_eq!(report.unchanged(), 0);
    }

    #[test]
    fn whole_batch_failure_is_reported_per_work() {
        let temp = Temp::new("timeout");
        let service = Subscriptions::new(&temp.0);
        service.add("n0001aa", 100).unwrap();
        let report = check(&service, &Fake(Err(SourceError::Timeout)), 1000).unwrap();
        assert_eq!(report.summary.failed, 1);
        assert_eq!(report.to_json()["errors"][0]["message"], "查询超时");
    }

    #[test]
    fn more_than_one_batch_lists_the_remaining_works() {
        let temp = Temp::new("batch");
        let service = Subscriptions::new(&temp.0);
        for i in 0..52 {
            service.add(&code(i), 100).unwrap();
        }
        let all = Fake(Ok((0..52).map(|i| work(&code(i), 1)).collect()));
        let report = check(&service, &all, 1000).unwrap();
        assert_eq!(report.summary.checked, 50);
        assert!(report.incomplete());
        assert_eq!(report.deferred, vec![code(50), code(51)]);
        assert_eq!(report.to_json()["incomplete"], true);

        // 立即再运行：前 50 部在冷却中，余下 2 部这次检查。
        let report = check(&service, &all, 1001).unwrap();
        assert_eq!(report.summary.checked, 2);
        assert_eq!(report.cooldown.len(), 50);
        assert!(!report.incomplete() && report.deferred.is_empty());
    }

    #[test]
    fn source_errors_have_chinese_messages() {
        for error in [
            SourceError::NotFound,
            SourceError::Timeout,
            SourceError::Malformed,
            SourceError::RateLimited,
            SourceError::Unavailable,
            SourceError::Suspicious,
        ] {
            let message = source_message(&error);
            assert!(!message.is_ascii(), "{error:?}: {message}");
        }
    }
}
