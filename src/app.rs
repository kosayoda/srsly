use std::time::{Duration, Instant};

use crate::terminal::Terminal;

/// Default scrollback lines for terminals.
const SCROLLBACK_LINES: usize = 10_000;

/// How long to wait after sending before showing "no response" warning.
const NO_RESPONSE_THRESHOLD: Duration = Duration::from_secs(2);

/// Input mode (vim-style).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Keys go to serial (except Esc).
    Insert,
    /// Keys are commands (navigation, quit, etc.).
    #[default]
    Normal,
    /// Searching within panes.
    Search,
}

/// Serial connection state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnectionState {
    #[default]
    Connected,
    Disconnected {
        error: String,
    },
}

impl ConnectionState {
    pub fn is_connected(&self) -> bool {
        matches!(self, ConnectionState::Connected)
    }
}

/// Which pane currently has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// App output pane (input goes here)
    #[default]
    App,
    /// Kernel message pane (for scrolling)
    Kernel,
}

/// Layout direction for the two panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    /// Top-bottom
    #[default]
    Horizontal,
    /// Side-by-side
    Vertical,
}

impl Layout {
    pub fn toggle(self) -> Self {
        match self {
            Layout::Vertical => Layout::Horizontal,
            Layout::Horizontal => Layout::Vertical,
        }
    }
}

/// Application state.
pub struct App {
    /// Terminal emulator for app pane (kernel messages filtered out).
    pub app_terminal: Terminal,
    /// Terminal for kernel messages only.
    pub kernel_terminal: Terminal,
    /// Current input mode.
    pub mode: Mode,
    /// Which pane has focus.
    pub focus: Focus,
    /// Current layout (vertical or horizontal split).
    pub layout: Layout,
    /// Whether the app should quit.
    pub should_quit: bool,
    /// Serial connection state.
    pub connection: ConnectionState,
    /// Transient message to display in the message bar.
    pub message: Option<Message>,
    /// Last time data was sent to serial (None if never sent).
    last_send: Option<Instant>,
    /// Last time data was received from serial (None if never received).
    last_receive: Option<Instant>,
}

/// A transient message to display in the UI.
#[derive(Debug, Clone)]
pub struct Message {
    pub text: String,
    pub level: MessageLevel,
}

/// Message severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageLevel {
    Info,
    Error,
}

impl App {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            app_terminal: Terminal::new(rows, cols, SCROLLBACK_LINES),
            kernel_terminal: Terminal::new(rows, cols, SCROLLBACK_LINES),
            mode: Mode::default(),
            focus: Focus::App,
            layout: Layout::default(),
            should_quit: false,
            connection: ConnectionState::default(),
            message: None,
            last_send: None,
            last_receive: None,
        }
    }

    /// Get a reference to the focused terminal.
    pub fn focused_terminal(&self) -> &Terminal {
        match self.focus {
            Focus::App => &self.app_terminal,
            Focus::Kernel => &self.kernel_terminal,
        }
    }

    /// Get a mutable reference to the focused terminal.
    pub fn focused_terminal_mut(&mut self) -> &mut Terminal {
        match self.focus {
            Focus::App => &mut self.app_terminal,
            Focus::Kernel => &mut self.kernel_terminal,
        }
    }

    /// Record that data was sent to serial.
    /// Only updates the timestamp if we're not already waiting for a response,
    /// so the "no response" timer shows total wait time.
    pub fn record_send(&mut self) {
        if self.last_send.is_none() {
            self.last_send = Some(Instant::now());
        }
    }

    /// Record that data was received from serial.
    pub fn record_receive(&mut self) {
        self.last_receive = Some(Instant::now());
        // Clear any "no response" state when we get data
        self.last_send = None;
    }

    /// Check if we're waiting for a response (sent data but no response yet).
    /// Returns the duration we've been waiting, if applicable.
    pub fn waiting_for_response(&self) -> Option<Duration> {
        let send_time = self.last_send?;
        let elapsed = send_time.elapsed();

        // Only report if we've been waiting longer than threshold
        // and haven't received anything since sending
        if elapsed >= NO_RESPONSE_THRESHOLD {
            match self.last_receive {
                Some(recv_time) if recv_time > send_time => None,
                _ => Some(elapsed),
            }
        } else {
            None
        }
    }

    /// Get the deadline for when "no response" warning should appear.
    /// Used by the TUI to schedule a refresh.
    pub fn no_response_deadline(&self) -> Option<Instant> {
        let send_time = self.last_send?;
        // If we've already passed the threshold, no need for deadline
        if send_time.elapsed() >= NO_RESPONSE_THRESHOLD {
            None
        } else {
            Some(send_time + NO_RESPONSE_THRESHOLD)
        }
    }

    /// Mark connection as disconnected with an error message.
    pub fn set_disconnected(&mut self, error: impl Into<String>) {
        let error = error.into();
        self.show_error(format!("Serial disconnected: {}", error));
        self.connection = ConnectionState::Disconnected { error };
        // Switch to Normal mode so user can press Ctrl+R to reconnect
        self.mode = Mode::Normal;
    }

    /// Mark connection as connected.
    pub fn set_connected(&mut self) {
        self.connection = ConnectionState::Connected;
        self.show_info("Serial reconnected");
    }

    /// Show an info message.
    pub fn show_info(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            level: MessageLevel::Info,
        });
    }

    /// Show an error message.
    pub fn show_error(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            level: MessageLevel::Error,
        });
    }

    /// Clear the current message.
    pub fn clear_message(&mut self) {
        self.message = None;
    }

    /// Resize both terminal panes.
    /// Resize both terminal panes.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.app_terminal.set_size(rows, cols);
        self.kernel_terminal.set_size(rows, cols);
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new(24, 80)
    }
}
