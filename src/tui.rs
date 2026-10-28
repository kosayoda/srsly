use std::{
    ops::{Deref, DerefMut},
    time::Duration,
};

use color_eyre::eyre::{Result, ensure};

use crossterm::{
    cursor,
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event as CrosstermEvent, KeyEvent, KeyEventKind, MouseEvent,
    },
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend as Backend;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::filter::KernelFilter;

/// Time between autoscroll steps while dragging past a pane edge.
const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub enum Event {
    Init,
    Terminal(TerminalEvent),
    /// Filtered data for the app terminal (kernel messages removed).
    AppData(Vec<u8>),
    /// Kernel messages only.
    KernelData(Vec<u8>),
    /// Serial error.
    SerialError(std::io::Error),
    /// "No response" timeout reached - trigger UI refresh.
    NoResponseTimeout,
    /// A mouse drag is past a pane edge; scroll and extend the selection.
    AutoscrollTick,
}

impl From<TerminalEvent> for Event {
    fn from(value: TerminalEvent) -> Self {
        Self::Terminal(value)
    }
}

#[derive(Clone, Debug)]
pub enum TerminalEvent {
    FocusGained,
    FocusLost,
    Paste(String),
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    Error(std::io::ErrorKind),
}

pub struct TuiConfig {
    enable_mouse: bool,
    enable_paste: bool,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            enable_mouse: true,
            enable_paste: true,
        }
    }
}

pub struct Tui {
    terminal: ratatui::Terminal<Backend<std::io::Stdout>>,

    /// Reads serial data and drives timers. Runs until reconnect or exit.
    serial_task: JoinHandle<()>,
    serial_token: CancellationToken,

    /// Reads terminal input. Paused while an external program has the terminal.
    input_task: JoinHandle<()>,
    input_token: CancellationToken,

    event_rx: mpsc::Receiver<Event>,
    event_tx: mpsc::Sender<Event>,

    /// Channel to send "no response" deadline updates to the event loop.
    no_response_tx: mpsc::Sender<Option<tokio::time::Instant>>,

    /// Channel to turn autoscroll ticks on and off.
    autoscroll_tx: mpsc::Sender<bool>,

    config: TuiConfig,
}

impl Tui {
    pub fn new(config: TuiConfig) -> Result<Self> {
        let terminal = ratatui::Terminal::new(Backend::new(std::io::stdout()))?;
        let (event_tx, event_rx) = mpsc::channel(1024);
        let (no_response_tx, _no_response_rx) = mpsc::channel(16);
        let (autoscroll_tx, _autoscroll_rx) = mpsc::channel(16);
        Ok(Self {
            terminal,
            serial_task: tokio::spawn(async {}),
            serial_token: CancellationToken::new(),
            input_task: tokio::spawn(async {}),
            input_token: CancellationToken::new(),
            event_rx,
            event_tx,
            no_response_tx,
            autoscroll_tx,
            config,
        })
    }

    /// Set the deadline for "no response" timeout. Pass None to cancel.
    pub fn set_no_response_deadline(&self, deadline: Option<tokio::time::Instant>) {
        let _ = self.no_response_tx.try_send(deadline);
    }

    /// Enable or disable periodic [`Event::AutoscrollTick`] events.
    pub fn set_autoscroll(&self, enabled: bool) {
        let _ = self.autoscroll_tx.try_send(enabled);
    }

    fn start_serial(&mut self, serial_rx: mpsc::Receiver<crate::serial::SerialEvent>) {
        self.serial_token.cancel();
        self.serial_token = CancellationToken::new();

        let (no_response_tx, no_response_rx) = mpsc::channel(16);
        self.no_response_tx = no_response_tx;
        let (autoscroll_tx, autoscroll_rx) = mpsc::channel(16);
        self.autoscroll_tx = autoscroll_tx;

        let event_tx = self.event_tx.clone();
        let token = self.serial_token.clone();

        self.serial_task = tokio::spawn(async move {
            let mut serial = tokio_stream::wrappers::ReceiverStream::new(serial_rx);
            let mut no_response_updates =
                tokio_stream::wrappers::ReceiverStream::new(no_response_rx);
            let mut autoscroll_updates = tokio_stream::wrappers::ReceiverStream::new(autoscroll_rx);
            let mut filter = KernelFilter::new();
            let mut no_response_deadline: Option<tokio::time::Instant> = None;
            let mut autoscroll = false;

            macro_rules! try_send {
                ($event:expr) => {
                    if event_tx.send($event).await.is_err() {
                        break;
                    }
                };
            }

            if event_tx.send(Event::Init).await.is_err() {
                return;
            }

            loop {
                // Dynamic timeout based on pending kernel line
                let flush_deadline = filter.next_deadline();
                let flush_timeout = async {
                    match flush_deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                };

                // "No response" timeout
                let no_response_timeout = async {
                    match no_response_deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                };

                let autoscroll_tick = async {
                    if autoscroll {
                        tokio::time::sleep(AUTOSCROLL_INTERVAL).await
                    } else {
                        std::future::pending().await
                    }
                };

                tokio::select! {
                    _ = token.cancelled() => {
                        tracing::debug!("Received cancellation notice, exiting");
                        break;
                    }
                    Some(deadline) = no_response_updates.next() => {
                        no_response_deadline = deadline;
                    }
                    Some(enabled) = autoscroll_updates.next() => {
                        autoscroll = enabled;
                    }
                    _ = autoscroll_tick => {
                        try_send!(Event::AutoscrollTick);
                    }
                    _ = no_response_timeout => {
                        no_response_deadline = None;
                        try_send!(Event::NoResponseTimeout);
                    }
                    // Disabled once the serial stream ends after a disconnect
                    Some(event) = serial.next() => {
                        use crate::serial::SerialEvent;
                        match event {
                            SerialEvent::Data(data) => {
                                let (app_data, kernel_data) = filter.process(&data);
                                if !app_data.is_empty() {
                                    try_send!(Event::AppData(app_data));
                                }
                                if !kernel_data.is_empty() {
                                    try_send!(Event::KernelData(kernel_data));
                                }
                            }
                            SerialEvent::Error(e) => {
                                try_send!(Event::SerialError(e));
                            }
                        }
                    }
                    _ = flush_timeout => {
                        // Pending line timed out - flush to app terminal
                        let data = filter.flush_pending();
                        if !data.is_empty() {
                            try_send!(Event::AppData(data));
                        }
                    }
                }
            }
        });
    }

    fn start_input(&mut self) {
        self.input_token.cancel();
        self.input_token = CancellationToken::new();

        let event_tx = self.event_tx.clone();
        let token = self.input_token.clone();

        self.input_task = tokio::spawn(async move {
            let mut reader = crossterm::event::EventStream::new();

            loop {
                let event = tokio::select! {
                    _ = token.cancelled() => break,
                    event = reader.next() => event,
                };
                let event = match event {
                    Some(Ok(CrosstermEvent::Key(key))) if key.kind == KeyEventKind::Press => {
                        TerminalEvent::Key(key)
                    }
                    Some(Ok(CrosstermEvent::Key(_))) => continue,
                    Some(Ok(CrosstermEvent::Mouse(mouse))) => TerminalEvent::Mouse(mouse),
                    Some(Ok(CrosstermEvent::Resize(x, y))) => TerminalEvent::Resize(x, y),
                    Some(Ok(CrosstermEvent::FocusLost)) => TerminalEvent::FocusLost,
                    Some(Ok(CrosstermEvent::FocusGained)) => TerminalEvent::FocusGained,
                    Some(Ok(CrosstermEvent::Paste(s))) => TerminalEvent::Paste(s),
                    Some(Err(e)) => TerminalEvent::Error(e.kind()),
                    None => break,
                };
                if event_tx.send(event.into()).await.is_err() {
                    break;
                }
            }
        });
    }

    pub fn stop(&self) -> Result<()> {
        self.cancel();
        std::thread::sleep(Duration::from_millis(50));

        let finished = |tui: &Self| tui.serial_task.is_finished() && tui.input_task.is_finished();
        if finished(self) {
            return Ok(());
        }

        self.serial_task.abort();
        self.input_task.abort();
        std::thread::sleep(Duration::from_millis(50));

        ensure!(finished(self), "Failed to abort tasks in 100 milliseconds");

        Ok(())
    }

    pub fn enter(&mut self, serial_rx: mpsc::Receiver<crate::serial::SerialEvent>) -> Result<()> {
        self.setup_terminal()?;
        self.start_serial(serial_rx);
        self.start_input();
        Ok(())
    }

    pub fn exit(&mut self) -> Result<()> {
        self.stop()?;
        self.restore_terminal()
    }

    /// Hand the terminal to another program. Serial data keeps flowing.
    pub async fn suspend(&mut self) -> Result<()> {
        self.input_token.cancel();
        (&mut self.input_task).await.ok();
        self.restore_terminal()
    }

    /// Take the terminal back after [`Tui::suspend`].
    pub fn resume(&mut self) -> Result<()> {
        self.setup_terminal()?;
        self.start_input();
        self.terminal.clear()?;
        Ok(())
    }

    fn setup_terminal(&mut self) -> Result<()> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(std::io::stdout(), EnterAlternateScreen, cursor::Hide)?;

        if self.config.enable_mouse {
            crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
        }

        if self.config.enable_paste {
            crossterm::execute!(std::io::stdout(), EnableBracketedPaste)?;
        }
        Ok(())
    }

    fn restore_terminal(&mut self) -> Result<()> {
        if crossterm::terminal::is_raw_mode_enabled()? {
            self.flush()?;

            if self.config.enable_paste {
                crossterm::execute!(std::io::stdout(), DisableBracketedPaste)?;
            }

            if self.config.enable_mouse {
                crossterm::execute!(std::io::stdout(), DisableMouseCapture)?;
            }

            crossterm::execute!(std::io::stdout(), LeaveAlternateScreen, cursor::Show)?;
            crossterm::terminal::disable_raw_mode()?;
        }
        Ok(())
    }

    pub fn cancel(&self) {
        self.serial_token.cancel();
        self.input_token.cancel();
    }

    /// Restart serial reading with a new connection.
    pub fn restart(&mut self, serial_rx: mpsc::Receiver<crate::serial::SerialEvent>) {
        self.start_serial(serial_rx);
    }

    pub async fn next(&mut self) -> Option<Event> {
        self.event_rx.recv().await
    }
}

impl Deref for Tui {
    type Target = ratatui::Terminal<Backend<std::io::Stdout>>;

    fn deref(&self) -> &Self::Target {
        &self.terminal
    }
}

impl DerefMut for Tui {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.terminal
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        if let Err(e) = self.exit() {
            tracing::error!("Failed to gracefully exit TUI: {e}");
        }
    }
}
