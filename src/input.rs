use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use smallvec::{smallvec, SmallVec};

use crate::app::{App, Focus, Mode};

/// Result of handling a key event.
pub enum KeyAction {
    /// Send bytes to serial.
    Send(SmallVec<[u8; 5]>),
    /// Quit the application.
    Quit,
    /// Layout changed, need to recalculate sizes.
    LayoutChanged,
    /// No action needed.
    None,
}

/// Handle a key event, updating app state and returning any action needed.
pub fn handle_key(app: &mut App, key: KeyEvent) -> KeyAction {
    match app.mode {
        Mode::Insert => handle_insert_mode(app, key),
        Mode::Normal => handle_normal_mode(app, key),
    }
}

/// Handle a paste event. Only sends to serial in Insert mode.
pub fn handle_paste(app: &App, text: &str) -> KeyAction {
    match app.mode {
        Mode::Insert => KeyAction::Send(text.as_bytes().into()),
        Mode::Normal => KeyAction::None,
    }
}

/// Handle a mouse event. Click to focus a pane.
/// `pane_areas` should be (kernel_area, app_area).
pub fn handle_mouse(app: &mut App, mouse: MouseEvent, pane_areas: (Rect, Rect)) -> KeyAction {
    let (kernel_area, app_area) = pane_areas;

    if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
        let x = mouse.column;
        let y = mouse.row;

        if kernel_area.contains((x, y).into()) {
            app.focus = Focus::Kernel;
        } else if app_area.contains((x, y).into()) {
            app.focus = Focus::App;
        }
    }

    KeyAction::None
}

/// Insert mode: all keys go to serial, except Esc.
fn handle_insert_mode(app: &mut App, key: KeyEvent) -> KeyAction {
    if key.code == KeyCode::Esc {
        app.mode = Mode::Normal;
        return KeyAction::None;
    }

    match key_to_bytes(key) {
        Some(bytes) => KeyAction::Send(bytes),
        None => KeyAction::None,
    }
}

/// Normal mode: keys are commands.
fn handle_normal_mode(app: &mut App, key: KeyEvent) -> KeyAction {
    // Ignore modifiers for normal mode commands
    match key.code {
        // Mode switching - also focuses app pane
        KeyCode::Char('i') => {
            app.mode = Mode::Insert;
            app.focus = Focus::App;
            KeyAction::None
        }

        // Quit
        KeyCode::Char('q') => {
            app.should_quit = true;
            KeyAction::Quit
        }

        // Layout toggle
        KeyCode::Char('s') => {
            app.layout = app.layout.toggle();
            KeyAction::LayoutChanged
        }

        // Send resize command
        KeyCode::Char('r') => {
            let (rows, cols) = app.app_terminal.size();
            let cmd = format!("stty rows {} cols {}\n", rows, cols);
            KeyAction::Send(cmd.into_bytes().into())
        }

        // Switch focus between panes
        KeyCode::Tab | KeyCode::Char('w') => {
            app.focus = match app.focus {
                Focus::App => Focus::Kernel,
                Focus::Kernel => Focus::App,
            };
            KeyAction::None
        }

        // Scrolling (works on focused pane)
        KeyCode::Char('j') | KeyCode::Down => {
            focused_terminal(app).scroll_down(1);
            KeyAction::None
        }
        KeyCode::Char('k') | KeyCode::Up => {
            focused_terminal(app).scroll_up(1);
            KeyAction::None
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            focused_terminal(app).scroll_down(10);
            KeyAction::None
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            focused_terminal(app).scroll_up(10);
            KeyAction::None
        }
        KeyCode::Char('g') => {
            focused_terminal(app).scroll_to_top();
            KeyAction::None
        }
        KeyCode::Char('G') => {
            focused_terminal(app).scroll_to_bottom();
            KeyAction::None
        }

        // Clear focused pane
        KeyCode::Char('c') => {
            focused_terminal(app).clear();
            KeyAction::None
        }

        // Unknown command - ignore
        _ => KeyAction::None,
    }
}

/// Get mutable reference to the focused terminal.
fn focused_terminal(app: &mut App) -> &mut crate::terminal::Terminal {
    match app.focus {
        Focus::App => &mut app.app_terminal,
        Focus::Kernel => &mut app.kernel_terminal,
    }
}

/// Convert a key event to bytes to send over serial.
fn key_to_bytes(key: KeyEvent) -> Option<SmallVec<[u8; 5]>> {
    let bytes = match key.code {
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                // Ctrl+A = 0x01, Ctrl+B = 0x02, etc.
                let ctrl_char = (c.to_ascii_lowercase() as u8).wrapping_sub(b'a' - 1);
                smallvec![ctrl_char]
            } else {
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                s.as_bytes().into()
            }
        }
        KeyCode::Enter => smallvec![b'\r'],
        KeyCode::Backspace => smallvec![0x7f],
        KeyCode::Tab => smallvec![b'\t'],
        KeyCode::Esc => smallvec![0x1b],
        KeyCode::Up => smallvec![0x1b, b'[', b'A'],
        KeyCode::Down => smallvec![0x1b, b'[', b'B'],
        KeyCode::Right => smallvec![0x1b, b'[', b'C'],
        KeyCode::Left => smallvec![0x1b, b'[', b'D'],
        KeyCode::Home => smallvec![0x1b, b'[', b'H'],
        KeyCode::End => smallvec![0x1b, b'[', b'F'],
        KeyCode::Delete => smallvec![0x1b, b'[', b'3', b'~'],
        KeyCode::Insert => smallvec![0x1b, b'[', b'2', b'~'],
        KeyCode::PageUp => smallvec![0x1b, b'[', b'5', b'~'],
        KeyCode::PageDown => smallvec![0x1b, b'[', b'6', b'~'],
        KeyCode::F(n) => match n {
            1 => smallvec![0x1b, b'O', b'P'],
            2 => smallvec![0x1b, b'O', b'Q'],
            3 => smallvec![0x1b, b'O', b'R'],
            4 => smallvec![0x1b, b'O', b'S'],
            5 => smallvec![0x1b, b'[', b'1', b'5', b'~'],
            6 => smallvec![0x1b, b'[', b'1', b'7', b'~'],
            7 => smallvec![0x1b, b'[', b'1', b'8', b'~'],
            8 => smallvec![0x1b, b'[', b'1', b'9', b'~'],
            9 => smallvec![0x1b, b'[', b'2', b'0', b'~'],
            10 => smallvec![0x1b, b'[', b'2', b'1', b'~'],
            11 => smallvec![0x1b, b'[', b'2', b'3', b'~'],
            12 => smallvec![0x1b, b'[', b'2', b'4', b'~'],
            _ => return None,
        },
        _ => return None,
    };
    Some(bytes)
}
