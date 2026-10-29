//! git-delta 渲染：把改动按段交给 [git-delta](https://github.com/dandavison/delta) 着色，
//! 再把它的终端色码转成 HTML（Web）或带样式的片段（TUI）。段的划分、段号与审批仍以
//! [`FileChange::hunks`] 为准，delta 只管呈现。
//!
//! 可执行文件取 `ZIYUAN_DELTA`，未设则找 PATH 上的 `delta`。同名的程序不止一个（如 Zed 的
//! Delta CLI，`--version` 也是 `delta 0.x`），故不看版本号，而是用 git-delta 的参数实跑一小段
//! 差异：退出码为零且输出含原文的才算可用。找不到或实跑不过时，调用方回退到内置预览。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use ziyuan_core::FileChange;

/// 一个实跑验证过的 git-delta。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    path: PathBuf,
}

/// 渲染方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeltaStyle {
    pub side_by_side: bool,
    /// 输出宽度（列）；左右并排时两栏各占一半。
    pub width: u16,
    /// 浅色底（Web 的宣纸主题）。
    pub light: bool,
}

const PROBE: &str = "diff --git a/probe.rs b/probe.rs\n--- a/probe.rs\n+++ b/probe.rs\n@@ -1 +1 @@\n-ziyuan_probe_old\n+ziyuan_probe_new\n";
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const RENDER_TIMEOUT: Duration = Duration::from_secs(20);

/// 本进程内找到的 git-delta（只探测一次）。
pub fn delta() -> Option<&'static Delta> {
    static FOUND: OnceLock<Option<Delta>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let configured = std::env::var_os("ZIYUAN_DELTA").filter(|v| !v.is_empty());
            let path = match configured {
                Some(p) => PathBuf::from(p),
                None => find_on_path("delta")?,
            };
            Delta::probe(&path)
        })
        .as_ref()
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let exe = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(&exe))
        .find(|p| p.is_file())
}

impl Delta {
    /// 实跑一小段差异，确认 `path` 是 git-delta。
    pub fn probe(path: &Path) -> Option<Self> {
        let delta = Self {
            path: path.to_owned(),
        };
        let style = DeltaStyle {
            side_by_side: false,
            width: 80,
            light: false,
        };
        let out = delta.run(PROBE, style, PROBE_TIMEOUT).ok()?;
        out.contains("ziyuan_probe_new").then_some(delta)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 渲染一段标准 unified diff，返回带终端色码的文本。
    pub fn render(&self, diff: &str, style: DeltaStyle) -> Result<String, String> {
        self.run(diff, style, RENDER_TIMEOUT)
    }

    /// 直接显示到终端（继承标准输出，由 delta 自己量宽、辨深浅底色），用于 `ziyuan diff`。
    /// 这里照用户的 git 配置，好与其 `git diff` 一致。
    pub fn show(&self, diff: &str, side_by_side: bool) -> Result<(), String> {
        let mut command = Command::new(&self.path);
        command.args(["--paging=never", "--line-numbers"]);
        if side_by_side {
            command.arg("--side-by-side");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut stdin = child.stdin.take().expect("已设管道");
        let written = stdin.write_all(diff.as_bytes());
        drop(stdin);
        let status = child.wait().map_err(|e| e.to_string())?;
        written.map_err(|e| e.to_string())?;
        status
            .success()
            .then_some(())
            .ok_or_else(|| format!("delta 退出码 {status}"))
    }

    fn run(&self, input: &str, style: DeltaStyle, timeout: Duration) -> Result<String, String> {
        let mut command = Command::new(&self.path);
        command
            // 不读用户的 git 配置与 DELTA_FEATURES，同一份改动处处一样。
            .args(["--no-gitconfig", "--paging=never", "--true-color=always"])
            .args([
                "--line-numbers",
                "--file-style=omit",
                "--hunk-header-style=omit",
            ])
            .arg(format!("--width={}", style.width.max(20)))
            .env_remove("DELTA_FEATURES")
            .env_remove("DELTA_PAGER")
            .env_remove("BAT_THEME")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if style.side_by_side {
            command.arg("--side-by-side");
        }
        command.arg(if style.light { "--light" } else { "--dark" });
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let mut stdin = child.stdin.take().expect("已设管道");
        let input = input.to_owned();
        let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
        let mut stdout = child.stdout.take().expect("已设管道");
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            stdout.read_to_end(&mut buf).map(|_| buf)
        });
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(status) => break status,
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("delta 超时".into());
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        let _ = writer.join();
        let out = reader
            .join()
            .map_err(|_| "delta 输出读取失败".to_owned())?
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("delta 退出码 {status}"));
        }
        String::from_utf8(out).map_err(|e| e.to_string())
    }
}

/// 各处改动逐段的标准 unified diff（带 `diff --git` 与 `@@ -a,b +c,d @@` 头，供 delta 认出语言
/// 与行号），与 [`FileChange::hunks`] 同序；外层按文件、内层按段。
pub fn unified_hunks(files: &[FileChange]) -> Vec<Vec<String>> {
    files
        .iter()
        .map(|change| {
            let old = match change.before {
                Some(_) => format!("a/{}", change.path),
                None => "/dev/null".to_owned(),
            };
            let head = format!(
                "diff --git a/{0} b/{0}\n--- {old}\n+++ b/{0}\n",
                change.path
            );
            let mut shift: isize = 0;
            change
                .hunks()
                .into_iter()
                .map(|hunk| {
                    let old_count = hunk.lines.iter().filter(|l| l.0 != '+').count();
                    let new_count = hunk.lines.iter().filter(|l| l.0 != '-').count();
                    let new_start = if change.before.is_none() {
                        1
                    } else {
                        (hunk.start as isize + shift) as usize
                    };
                    shift += new_count as isize - old_count as isize;
                    // 行数为零时，起始行号按惯例取前一行。
                    let at = |start: usize, count: usize| {
                        if count == 0 {
                            start.saturating_sub(1)
                        } else {
                            start
                        }
                    };
                    let mut out = format!(
                        "{head}@@ -{},{old_count} +{},{new_count} @@\n",
                        at(hunk.start, old_count),
                        at(new_start, new_count),
                    );
                    for (mark, text) in &hunk.lines {
                        out.push(*mark);
                        out.push_str(text);
                        out.push('\n');
                    }
                    out
                })
                .collect()
        })
        .collect()
}

/// 整份改动的标准 unified diff，段头后附「第 n 段」（段号同逐段准奏），给终端分页显示。
pub fn unified_diff(files: &[FileChange]) -> String {
    let mut number = 0;
    let mut out = String::new();
    for hunks in unified_hunks(files) {
        for (i, hunk) in hunks.into_iter().enumerate() {
            number += 1;
            let body = if i == 0 {
                hunk.as_str()
            } else {
                // 同一文件的后续段去掉重复的文件头。
                &hunk[hunk.find("\n@@ ").map_or(0, |p| p + 1)..]
            };
            let (header, rest) = body.split_at(body.find("@@ -").unwrap_or(0));
            let line_end = rest.find('\n').unwrap_or(rest.len());
            out.push_str(header);
            out.push_str(&rest[..line_end]);
            out.push_str(&format!(" 第 {number} 段"));
            out.push_str(&rest[line_end..]);
        }
    }
    out
}

/// 一段终端文字的样式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AnsiStyle {
    pub fg: Option<(u8, u8, u8)>,
    pub bg: Option<(u8, u8, u8)>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
}

/// 把带 SGR 色码的文本切成行、每行若干（文字, 样式）；其余控制序列（光标、OSC 链接）丢弃。
pub fn ansi_lines(text: &str) -> Vec<Vec<(String, AnsiStyle)>> {
    let mut lines = Vec::new();
    let mut line: Vec<(String, AnsiStyle)> = Vec::new();
    let mut style = AnsiStyle::default();
    let mut chars = text.chars().peekable();
    let push =
        |line: &mut Vec<(String, AnsiStyle)>, c: char, style: AnsiStyle| match line.last_mut() {
            Some((t, s)) if *s == style => t.push(c),
            _ => line.push((c.to_string(), style)),
        };
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                Some('[') => {
                    let mut params = String::new();
                    let mut end = None;
                    for c in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            end = Some(c);
                            break;
                        }
                        params.push(c);
                    }
                    if end == Some('m') {
                        apply_sgr(&mut style, &params);
                    }
                }
                Some(']') => {
                    // OSC：到 BEL 或 ESC \ 为止。
                    while let Some(c) = chars.next() {
                        if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\n' => lines.push(std::mem::take(&mut line)),
            '\r' => {}
            c if c.is_control() && c != '\t' => {}
            c => push(&mut line, c, style),
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    // delta 在省去文件头与段头后仍留空行。
    let blank = |l: &Vec<(String, AnsiStyle)>| l.iter().all(|(t, _)| t.trim().is_empty());
    while lines.last().is_some_and(blank) {
        lines.pop();
    }
    let lead = lines.iter().take_while(|l| blank(l)).count();
    lines.drain(..lead);
    lines
}

fn apply_sgr(style: &mut AnsiStyle, params: &str) {
    let codes: Vec<u16> = if params.is_empty() {
        vec![0]
    } else {
        params
            .split([';', ':'])
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    let mut i = 0;
    while i < codes.len() {
        let code = codes[i];
        let mut extended = |target: &mut Option<(u8, u8, u8)>| match codes.get(i + 1) {
            Some(2) if i + 4 < codes.len() => {
                let c = |k: usize| codes[i + k].min(255) as u8;
                *target = Some((c(2), c(3), c(4)));
                i += 4;
            }
            Some(5) if i + 2 < codes.len() => {
                *target = Some(palette(codes[i + 2].min(255) as u8));
                i += 2;
            }
            _ => {}
        };
        match code {
            0 => *style = AnsiStyle::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            3 => style.italic = true,
            4 => style.underline = true,
            22 => (style.bold, style.dim) = (false, false),
            23 => style.italic = false,
            24 => style.underline = false,
            30..=37 => style.fg = Some(palette((code - 30) as u8)),
            90..=97 => style.fg = Some(palette((code - 90 + 8) as u8)),
            40..=47 => style.bg = Some(palette((code - 40) as u8)),
            100..=107 => style.bg = Some(palette((code - 100 + 8) as u8)),
            38 => extended(&mut style.fg),
            48 => extended(&mut style.bg),
            39 => style.fg = None,
            49 => style.bg = None,
            _ => {}
        }
        i += 1;
    }
}

/// xterm 256 色。
fn palette(n: u8) -> (u8, u8, u8) {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 49, 49),
        (13, 188, 121),
        (229, 229, 16),
        (36, 114, 200),
        (188, 63, 188),
        (17, 168, 205),
        (229, 229, 229),
        (102, 102, 102),
        (241, 76, 76),
        (35, 209, 139),
        (245, 245, 67),
        (59, 142, 234),
        (214, 112, 214),
        (41, 184, 219),
        (255, 255, 255),
    ];
    match n {
        0..=15 => BASE[n as usize],
        16..=231 => {
            let n = n - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            (level(n / 36), level(n / 6 % 6), level(n % 6))
        }
        _ => {
            let v = 8 + (n - 232) * 10;
            (v, v, v)
        }
    }
}

/// [`ansi_lines`] 转成 HTML，一行一个字符串；文字已转义。
pub fn ansi_html(text: &str) -> Vec<String> {
    ansi_lines(text)
        .into_iter()
        .map(|line| {
            line.into_iter()
                .map(|(t, s)| {
                    let mut css = Vec::new();
                    if let Some((r, g, b)) = s.fg {
                        css.push(format!("color:#{r:02x}{g:02x}{b:02x}"));
                    }
                    if let Some((r, g, b)) = s.bg {
                        css.push(format!("background:#{r:02x}{g:02x}{b:02x}"));
                    }
                    if s.bold {
                        css.push("font-weight:600".into());
                    }
                    if s.dim {
                        css.push("opacity:.6".into());
                    }
                    if s.italic {
                        css.push("font-style:italic".into());
                    }
                    if s.underline {
                        css.push("text-decoration:underline".into());
                    }
                    let t = escape(&t);
                    if css.is_empty() {
                        t
                    } else {
                        format!("<span style=\"{}\">{t}</span>", css.join(";"))
                    }
                })
                .collect()
        })
        .collect()
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}
