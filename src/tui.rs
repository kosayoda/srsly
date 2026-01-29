use std::{
    ops::{Deref, DerefMut},
    time::Duration,
};

use color_eyre::eyre::{ensure, Result};

use crossterm::{
    cursor,
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event as CrosstermEvent, KeyEvent, KeyEventKind, MouseEvent,
    },
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::{FutureExt, StreamExt};
use ratatui::backend::CrosstermBackend as Backend;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::filter::KernelFilter;

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
    terminal: ratatui::Terminal<Backend<std::io::Stderr>>,

    task: JoinHandle<()>,
    cancellation_token: CancellationToken,

    event_rx: mpsc::Receiver<Event>,
    event_tx: mpsc::Sender<Event>,

    config: TuiConfig,
}

impl Tui {
    pub fn new(config: TuiConfig) -> Result<Self> {
        let terminal = ratatui::Terminal::new(Backend::new(std::io::stderr()))?;
        let (event_tx, event_rx) = mpsc::channel(1024);
        let cancellation_token = CancellationToken::new();
        let task = tokio::spawn(async {});
        Ok(Self {
            terminal,
            task,
            cancellation_token,
            event_rx,
            event_tx,
            config,
        })
    }

    fn start(&mut self, serial_rx: mpsc::Receiver<crate::serial::SerialEvent>) {
        // Cancel existing task
        self.cancel();
        self.cancellation_token = CancellationToken::new();

        // Get task dependencies
        let _event_tx = self.event_tx.clone();
        let _cancellation_token = self.cancellation_token.clone();

        self.task = tokio::spawn(async move {
            let mut reader = crossterm::event::EventStream::new();
            let mut serial = tokio_stream::wrappers::ReceiverStream::new(serial_rx);
            let mut filter = KernelFilter::new();

            macro_rules! try_send {
                ($event_tx:ident, $event:expr) => {
                    if $event_tx.send($event).await.is_err() {
                        break;
                    }
                };
            }

            if _event_tx.send(Event::Init).await.is_err() {
                return;
            }

            loop {
                let crossterm_event = reader.next().fuse();
                let serial_event = serial.next().fuse();

                // Dynamic timeout based on pending kernel line
                let flush_deadline = filter.next_deadline();
                let flush_timeout = async {
                    match flush_deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                };

                tokio::select! {
                    _ = _cancellation_token.cancelled() => {
                        tracing::debug!("Received cancellation notice, exiting");
                        break;
                    }
                    maybe_serial = serial_event => {
                        use crate::serial::SerialEvent;
                        let Some(event) = maybe_serial else {
                            continue;
                        };
                        match event {
                            SerialEvent::Data(data) => {
                                let (app_data, kernel_data) = filter.process(&data);
                                if !app_data.is_empty() {
                                    try_send!(_event_tx, Event::AppData(app_data));
                                }
                                if !kernel_data.is_empty() {
                                    try_send!(_event_tx, Event::KernelData(kernel_data));
                                }
                            }
                            SerialEvent::Error(e) => {
                                try_send!(_event_tx, Event::SerialError(e));
                            }
                        }
                    }
                    maybe_event = crossterm_event => {
                        let Some(event) = maybe_event else {
                            continue;
                        };
                        match event {
                            Ok(evt) => match evt {
                                CrosstermEvent::Key(key) => {
                                    if key.kind == KeyEventKind::Press {
                                        try_send!(_event_tx, TerminalEvent::Key(key).into())
                                    }
                                },
                                CrosstermEvent::Mouse(mouse) => {
                                    try_send!(_event_tx, TerminalEvent::Mouse(mouse).into())
                                },
                                CrosstermEvent::Resize(x, y) => {
                                    try_send!(_event_tx, TerminalEvent::Resize(x, y).into())
                                },
                                CrosstermEvent::FocusLost => {
                                    try_send!(_event_tx, TerminalEvent::FocusLost.into())
                                },
                                CrosstermEvent::FocusGained => {
                                    try_send!(_event_tx, TerminalEvent::FocusGained.into())
                                },
                                CrosstermEvent::Paste(s) => {
                                    try_send!(_event_tx, TerminalEvent::Paste(s).into())
                                },
                            },
                            Err(e) => {
                                try_send!(_event_tx, TerminalEvent::Error(e.kind()).into())
                            }
                        }
                    },
                    _ = flush_timeout => {
                        // Pending line timed out - flush to app terminal
                        let data = filter.flush_pending();
                        if !data.is_empty() {
                            try_send!(_event_tx, Event::AppData(data));
                        }
                    },
                }
            }
        });
    }

    pub fn stop(&self) -> Result<()> {
        self.cancel();
        std::thread::sleep(Duration::from_millis(50));

        if self.task.is_finished() {
            return Ok(());
        }

        self.task.abort();
        std::thread::sleep(Duration::from_millis(50));

        ensure!(
            self.task.is_finished(),
            "Failed to abort task in 100 milliseconds"
        );

        Ok(())
    }

    pub fn enter(&mut self, serial_rx: mpsc::Receiver<crate::serial::SerialEvent>) -> Result<()> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(std::io::stderr(), EnterAlternateScreen, cursor::Hide)?;

        if self.config.enable_mouse {
            crossterm::execute!(std::io::stderr(), EnableMouseCapture)?;
        }

        if self.config.enable_paste {
            crossterm::execute!(std::io::stderr(), EnableBracketedPaste)?;
        }

        self.start(serial_rx);
        Ok(())
    }

    pub fn exit(&mut self) -> Result<()> {
        self.stop()?;

        if crossterm::terminal::is_raw_mode_enabled()? {
            self.flush()?;

            if self.config.enable_paste {
                crossterm::execute!(std::io::stderr(), DisableBracketedPaste)?;
            }

            if self.config.enable_mouse {
                crossterm::execute!(std::io::stderr(), DisableMouseCapture)?;
            }

            crossterm::execute!(std::io::stderr(), LeaveAlternateScreen, cursor::Show)?;
            crossterm::terminal::disable_raw_mode()?;
        }
        Ok(())
    }

    pub fn cancel(&self) {
        self.cancellation_token.cancel();
    }

    /// Restart the event loop with a new serial connection.
    /// This cancels the current task and starts a new one.
    pub fn restart(&mut self, serial_rx: mpsc::Receiver<crate::serial::SerialEvent>) {
        self.start(serial_rx);
    }

    pub async fn next(&mut self) -> Option<Event> {
        self.event_rx.recv().await
    }
}

impl Deref for Tui {
    type Target = ratatui::Terminal<Backend<std::io::Stderr>>;

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
