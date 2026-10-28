use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use smallvec::{SmallVec, smallvec};

use crate::app::{App, Focus, Mode};
use crate::terminal::{SearchDirection, VisualMode};
use crate::ui::{UiLayout, pane_inner};
use libghostty_vt::selection::Adjustment;

/// Lines scrolled per mouse wheel step.
const WHEEL_LINES: usize = 3;

/// Result of handling a key event.
pub enum KeyAction {
    /// Send bytes to serial.
    Send(SmallVec<[u8; 5]>),
    /// Quit the application.
    Quit,
    /// Layout changed, need to recalculate sizes.
    LayoutChanged,
    /// Reconnect to serial port.
    Reconnect,
    /// Search pattern changed, need to update matches.
    SearchChanged,
    /// Copy text to the clipboard.
    Copy(String),
    /// No action needed.
    None,
}

/// Handle a key event, updating app state and returning any action needed.
pub fn handle_key(app: &mut App, key: KeyEvent) -> KeyAction {
    match app.mode {
        Mode::Insert => handle_insert_mode(app, key),
        Mode::Normal => handle_normal_mode(app, key),
        Mode::Search => handle_search_mode(app, key),
        Mode::Visual => handle_visual_mode(app, key),
    }
}

/// Handle a paste event. Only sends to serial in Insert mode.
pub fn handle_paste(app: &mut App, text: &str) -> KeyAction {
    match app.mode {
        Mode::Insert => KeyAction::Send(text.as_bytes().into()),
        Mode::Normal | Mode::Search | Mode::Visual => {
            app.show_info("Paste ignored (not in Insert mode)");
            KeyAction::None
        }
    }
}

/// Handle a mouse event: click to focus, drag to select, wheel to scroll.
pub fn handle_mouse(app: &mut App, mouse: MouseEvent, layout: &UiLayout) -> KeyAction {
    let (x, y) = (mouse.column, mouse.row);

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let Some(pane) = layout.pane_at(x, y) else {
                return KeyAction::None;
            };
            app.focus = pane;
            if app.mode == Mode::Visual {
                app.mode = Mode::Normal;
            }
            for other in [Focus::App, Focus::Kernel] {
                app.terminal_mut(other).clear_selection();
            }
            let inner = pane_inner(layout.pane(pane));
            if inner.contains((x, y).into()) {
                app.dragging = Some(pane);
                let time = app.elapsed();
                app.terminal_mut(pane)
                    .mouse_press(x - inner.x, y - inner.y, time);
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(pane) = app.dragging {
                let inner = pane_inner(layout.pane(pane));
                app.terminal_mut(pane)
                    .mouse_drag(x as i32 - inner.x as i32, y as i32 - inner.y as i32);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some(pane) = app.dragging.take()
                && let Some(text) = app.terminal_mut(pane).mouse_release()
            {
                return KeyAction::Copy(text);
            }
        }
        MouseEventKind::ScrollUp => {
            if let Some(pane) = layout.pane_at(x, y) {
                app.terminal_mut(pane).scroll_up(WHEEL_LINES);
            }
        }
        MouseEventKind::ScrollDown => {
            if let Some(pane) = layout.pane_at(x, y) {
                app.terminal_mut(pane).scroll_down(WHEEL_LINES);
            }
        }
        _ => {}
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
    match key.code {
        // Mode switching - only if connected
        KeyCode::Char('i') => {
            if app.connection.is_connected() {
                app.mode = Mode::Insert;
                app.focus = Focus::App;
            } else {
                app.show_error("Cannot enter Insert mode: not connected");
            }
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
            let name = match app.layout {
                crate::app::Layout::Horizontal => "horizontal",
                crate::app::Layout::Vertical => "vertical",
            };
            app.show_info(format!("Layout: {}", name));
            KeyAction::LayoutChanged
        }

        // Reconnect to serial port (Ctrl+R)
        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => KeyAction::Reconnect,

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
            let pane_name = match app.focus {
                Focus::App => "App",
                Focus::Kernel => "Kernel",
            };
            focused_terminal(app).clear();
            app.show_info(format!("Cleared {} pane", pane_name));
            KeyAction::None
        }

        // Enter forward search mode (/)
        KeyCode::Char('/') => {
            app.mode = Mode::Search;
            let search = &mut focused_terminal(app).search;
            search.clear();
            search.direction = SearchDirection::Forward;
            KeyAction::None
        }

        // Enter backward search mode (?)
        KeyCode::Char('?') => {
            app.mode = Mode::Search;
            let search = &mut focused_terminal(app).search;
            search.clear();
            search.direction = SearchDirection::Backward;
            KeyAction::None
        }

        // Next match (n) - follows search direction
        KeyCode::Char('n') => {
            focused_terminal(app).next_match();
            KeyAction::None
        }

        // Previous match (N) - opposite of search direction
        KeyCode::Char('N') => {
            focused_terminal(app).prev_match();
            KeyAction::None
        }

        // Start keyboard selection
        KeyCode::Char('v') => {
            focused_terminal(app).visual_start(VisualMode::Char);
            app.mode = Mode::Visual;
            KeyAction::None
        }
        KeyCode::Char('V') => {
            focused_terminal(app).visual_start(VisualMode::Line);
            app.mode = Mode::Visual;
            KeyAction::None
        }

        // Clear search highlights and selection
        KeyCode::Esc => {
            let terminal = focused_terminal(app);
            terminal.search.clear();
            terminal.clear_selection();
            KeyAction::None
        }

        // Unknown command - ignore
        _ => KeyAction::None,
    }
}

/// Search mode: typing updates pattern, Enter/Esc exits.
fn handle_search_mode(app: &mut App, key: KeyEvent) -> KeyAction {
    match key.code {
        // Exit search mode, keep matches
        KeyCode::Enter => {
            app.mode = Mode::Normal;
            let search = &focused_terminal(app).search;
            if search.matches.is_empty() && !search.pattern.is_empty() {
                app.show_info("No matches found");
            }
            KeyAction::None
        }

        // Cancel search, clear matches
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            focused_terminal(app).search.clear();
            KeyAction::None
        }

        // Delete last character
        KeyCode::Backspace => {
            focused_terminal(app).search.pop_char();
            KeyAction::SearchChanged
        }

        // Clear entire pattern
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            focused_terminal(app).search.set_pattern(String::new());
            KeyAction::SearchChanged
        }

        // Next match
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            focused_terminal(app).next_match();
            KeyAction::None
        }

        // Previous match
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            focused_terminal(app).prev_match();
            KeyAction::None
        }

        // Add character to pattern
        KeyCode::Char(c) => {
            focused_terminal(app).search.push_char(c);
            KeyAction::SearchChanged
        }

        _ => KeyAction::None,
    }
}

/// Visual mode: keys move the selection head, `y` copies.
fn handle_visual_mode(app: &mut App, key: KeyEvent) -> KeyAction {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let terminal = focused_terminal(app);

    let adjustment = match key.code {
        KeyCode::Char('d') if ctrl => Some(Adjustment::PageDown),
        KeyCode::Char('u') if ctrl => Some(Adjustment::PageUp),
        KeyCode::Char('h') | KeyCode::Left => Some(Adjustment::Left),
        KeyCode::Char('j') | KeyCode::Down => Some(Adjustment::Down),
        KeyCode::Char('k') | KeyCode::Up => Some(Adjustment::Up),
        KeyCode::Char('l') | KeyCode::Right => Some(Adjustment::Right),
        KeyCode::Char('0') | KeyCode::Home => Some(Adjustment::BeginningOfLine),
        KeyCode::Char('$') | KeyCode::End => Some(Adjustment::EndOfLine),
        KeyCode::Char('g') => Some(Adjustment::Home),
        KeyCode::Char('G') => Some(Adjustment::End),
        _ => None,
    };
    if let Some(adjustment) = adjustment {
        terminal.visual_move(adjustment);
        return KeyAction::None;
    }

    let action = match key.code {
        KeyCode::Char('o') => {
            terminal.visual_swap();
            return KeyAction::None;
        }
        KeyCode::Char('n') => {
            terminal.visual_jump(true);
            return KeyAction::None;
        }
        KeyCode::Char('N') => {
            terminal.visual_jump(false);
            return KeyAction::None;
        }
        // v/V switch granularity, or leave visual mode if already in it
        KeyCode::Char('v') if terminal.visual() != Some(VisualMode::Char) => {
            terminal.set_visual_mode(VisualMode::Char);
            return KeyAction::None;
        }
        KeyCode::Char('V') if terminal.visual() != Some(VisualMode::Line) => {
            terminal.set_visual_mode(VisualMode::Line);
            return KeyAction::None;
        }
        KeyCode::Char('v' | 'V') | KeyCode::Esc => KeyAction::None,
        KeyCode::Char('y') => match terminal.selection_text() {
            Ok(Some(text)) => KeyAction::Copy(text),
            _ => KeyAction::None,
        },
        _ => return KeyAction::None,
    };

    terminal.clear_selection();
    app.mode = Mode::Normal;
    action
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
