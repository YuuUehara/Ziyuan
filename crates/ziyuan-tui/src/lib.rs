//! 紫禁城终端界面（TUI）：在终端里查看朝廷、下诏、执行、评定与任免。
//!
//! 与 CLI、Web 一样只是 [`Session`] 的一个前端：写入以乐观并发提交，执行诏令前取得执行者锁，
//! 并定时重新加载事件簿，因此能看到其他 ziyuan 进程的变更。
//!
//! [`App`] 不依赖真实终端：按键经 [`App::handle`] 处理，画面由 [`App::draw`] 绘制，
//! 测试用 ratatui 的 `TestBackend` 即可驱动。
mod actions;
mod app;
mod input;
mod render;

pub use app::Message;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self};
use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use ziyuan_app::Session;
use ziyuan_core::TaskId;
use ziyuan_runtime::{CancelToken, Limits, WorkerBackend};
use ziyuan_storage::EventLog;

use crate::app::{Modal, Tab};
pub struct Config {
    /// 朝廷所在目录（含 `.ziyuan/`）。
    pub root: PathBuf,
    pub backend: Arc<dyn WorkerBackend>,
    pub limits: Limits,
}

/// 接管终端运行界面，直到用户退出。
pub async fn run(config: Config) -> Result<(), Box<dyn Error>> {
    let (mut app, mut messages) = App::new(config)?;
    // crossterm 的读取是阻塞的；放在独立线程，退出时不必等它。
    let input = app.sender.clone();
    std::thread::spawn(move || {
        while let Ok(event) = event::read() {
            if input.send(Message::Input(event)).is_err() {
                break;
            }
        }
    });
    let mut terminal = ratatui::init();
    // 不支持备用屏的终端会留着启动前的输出（如「执行后端：…」），差量绘制不会擦掉它们。
    // 直接清屏而不用 `Terminal::clear`：后者要向终端询问光标位置，有的终端不答。
    let cleared = ratatui::crossterm::execute!(
        std::io::stdout(),
        ratatui::crossterm::terminal::Clear(ratatui::crossterm::terminal::ClearType::All)
    );
    let result = match cleared {
        Ok(()) => event_loop(&mut terminal, &mut app, &mut messages).await,
        Err(e) => Err(e.into()),
    };
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    messages: &mut UnboundedReceiver<Message>,
) -> Result<(), Box<dyn Error>> {
    // 定时重新加载，看到其他进程的变更。
    let mut refresh = tokio::time::interval(Duration::from_secs(1));
    while !app.quit {
        terminal.draw(|frame| app.draw(frame))?;
        tokio::select! {
            Some(message) = messages.recv() => app.handle(message),
            _ = refresh.tick() => app.reload(),
        }
    }
    if let Some(cancel) = &app.run {
        cancel.cancel();
    }
    Ok(())
}

pub struct App {
    config: Config,
    /// 承办官员所用的模型；演示后端为 `None`。
    model: Option<String>,
    session: Session<EventLog>,
    sender: UnboundedSender<Message>,
    tab: Tab,
    /// 选中的诏令；列表按新到旧排列。
    selected_task: Option<TaskId>,
    selected_official: usize,
    /// `None` 表示跟随最新事件。
    selected_event: Option<usize>,
    /// 详情面板的滚动行数（执行中的诏令自动跟随末尾）。
    detail_scroll: u16,
    modal: Option<Modal>,
    /// 最近一次操作的结果或错误。
    status: Option<(String, bool)>,
    /// 本进程正在进行的执行。
    run: Option<CancelToken>,
    /// 执行中诏令的流式输出（不入事件簿）。
    live: HashMap<TaskId, String>,
    /// 廷议进行中已收到的发言（议毕后以事件簿为准）。
    debating: HashMap<TaskId, Vec<String>>,
    quit: bool,
}

#[cfg(test)]
mod tests;
