use crate::terminal::Terminal;

/// Default scrollback lines for terminals.
const SCROLLBACK_LINES: usize = 10_000;

/// Input mode (vim-style).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Keys go to serial (except Esc).
    Insert,
    /// Keys are commands (navigation, quit, etc.).
    #[default]
    Normal,
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
