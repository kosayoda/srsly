use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph},
};

use crate::app::{App, ConnectionState, Focus, Layout as AppLayout, MessageLevel, Mode};
use crate::terminal::{SearchDirection, Terminal};

/// Computed layout areas for the UI.
pub struct UiLayout {
    pub kernel_pane: Rect,
    pub app_pane: Rect,
    pub status_bar: Rect,
}

/// Parameters for rendering a terminal pane.
struct PaneParams<'a> {
    title: &'a str,
    terminal: &'a Terminal,
    focused: bool,
    no_response: Option<std::time::Duration>,
}

impl UiLayout {
    /// Compute layout areas from frame size and layout mode.
    pub fn compute(frame_size: Rect, layout: AppLayout) -> Self {
        // Bottom area: status bar with top border (2 rows total)
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(2)])
            .split(frame_size);

        let content_area = main_chunks[0];
        let status_bar = main_chunks[1];

        let direction = match layout {
            AppLayout::Vertical => Direction::Horizontal,
            AppLayout::Horizontal => Direction::Vertical,
        };

        let pane_chunks = Layout::default()
            .direction(direction)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(content_area);

        Self {
            kernel_pane: pane_chunks[0],
            app_pane: pane_chunks[1],
            status_bar,
        }
    }

    /// Get pane areas as tuple (kernel, app) for hit testing.
    pub fn pane_areas(&self) -> (Rect, Rect) {
        (self.kernel_pane, self.app_pane)
    }
}

/// Render the application UI.
pub fn render(frame: &mut Frame, app: &App) {
    let layout = UiLayout::compute(frame.area(), app.layout);

    // Get "no response" warning for focused pane
    let no_response = app.waiting_for_response();

    // Render kernel pane
    render_terminal_pane(
        frame,
        layout.kernel_pane,
        PaneParams {
            title: "Kernel",
            terminal: &app.kernel_terminal,
            focused: app.focus == Focus::Kernel,
            no_response: if app.focus == Focus::Kernel {
                no_response
            } else {
                None
            },
        },
    );

    // Render app pane
    render_terminal_pane(
        frame,
        layout.app_pane,
        PaneParams {
            title: "App",
            terminal: &app.app_terminal,
            focused: app.focus == Focus::App,
            no_response: if app.focus == Focus::App {
                no_response
            } else {
                None
            },
        },
    );

    // Render status bar (with message in frame title if present)
    render_status_bar(frame, layout.status_bar, app);
}

/// Render a vt100 terminal pane.
fn render_terminal_pane(frame: &mut Frame, area: Rect, params: PaneParams) {
    let terminal = params.terminal;
    let screen = terminal.screen();

    let border_style = if params.focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let title_suffix = if terminal.scroll_offset() == 0 {
        format!("-/{}", terminal.scrollback_len())
    } else {
        format!("{}/{}", terminal.scroll_offset(), terminal.scrollback_len())
    };

    // Build title with optional "no response" warning
    let title_line: Line = if let Some(duration) = params.no_response {
        Line::from(vec![
            Span::raw(format!(" {} ({}) ", params.title, title_suffix)),
            Span::styled(
                format!("No response ({}s) ", duration.as_secs()),
                Style::default().fg(Color::Yellow),
            ),
        ])
    } else {
        Line::from(format!(" {} ({}) ", params.title, title_suffix))
    };

    let block = Block::default()
        .padding(Padding::horizontal(1))
        .title(title_line)
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Build lines from the vt100 screen
    let (rows, cols) = screen.size();
    let mut lines: Vec<Line> = Vec::with_capacity(rows as usize);

    for row in 0..rows.min(inner.height) {
        let view_row = row as usize;
        let mut spans: Vec<Span> = Vec::new();
        let mut current_text = String::new();
        let mut current_style = Style::default();

        for col in 0..cols.min(inner.width) {
            if let Some(cell) = screen.cell(row, col) {
                // Check if this cell is part of a search match (using view row)
                let in_current_match = terminal.is_current_match(view_row, col as usize);
                let in_any_match = terminal.is_any_match(view_row, col as usize);

                let cell_style = if in_current_match {
                    // Current match: bright yellow background
                    Style::default()
                        .bg(Color::Yellow)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD)
                } else if in_any_match {
                    // Other matches: dimmer highlight
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD)
                } else {
                    cell_to_style(cell)
                };

                if cell_style != current_style {
                    if !current_text.is_empty() {
                        spans.push(Span::styled(current_text.clone(), current_style));
                        current_text.clear();
                    }
                    current_style = cell_style;
                }

                current_text.push_str(cell.contents());
            }
        }

        if !current_text.is_empty() {
            spans.push(Span::styled(current_text, current_style));
        }

        lines.push(Line::from(spans));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);

    // Show cursor if focused
    if params.focused && !screen.hide_cursor() {
        let (cursor_row, cursor_col) = screen.cursor_position();
        let cursor_x = inner.x + cursor_col.min(inner.width.saturating_sub(1));
        let cursor_y = inner.y + cursor_row.min(inner.height.saturating_sub(1));
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

/// Render the status bar (mode, keybinds, connection status).
/// Message is shown in the frame's top border title (right-aligned).
fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let (rows, cols) = app.app_terminal.size();

    // Build the frame with message in title (right-aligned)
    let title: Line = if let Some(message) = &app.message {
        let style = match message.level {
            MessageLevel::Info => Style::default().fg(Color::Green),
            MessageLevel::Error => Style::default().fg(Color::Red),
        };
        Line::from(Span::styled(format!(" {} ", message.text), style))
    } else {
        Line::default()
    };

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(title)
        .title_alignment(ratatui::layout::Alignment::Right);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .horizontal_margin(1)
        .constraints([Constraint::Min(0), Constraint::Percentage(30)])
        .split(inner);

    // Get focused terminal's search state
    let search = &app.focused_terminal().search;

    // Left side: mode indicator and keybinds (or search input)
    let (mode_str, mode_style, keybinds) = match app.mode {
        Mode::Insert => (
            " INSERT ",
            Style::default().fg(Color::Black).bg(Color::Green),
            vec![Span::styled("Esc", key_style), Span::raw(": Normal Mode")],
        ),
        Mode::Normal => {
            let mut binds = vec![
                Span::styled("i", key_style),
                Span::raw(": Insert  "),
                Span::styled("q", key_style),
                Span::raw(": Quit  "),
                Span::styled("s", key_style),
                Span::raw(": Layout  "),
                Span::styled("/", key_style),
                Span::raw(": Search  "),
                Span::styled("j/k", key_style),
                Span::raw(": Scroll"),
            ];

            // Add n/N hint if there are search matches
            if !search.matches.is_empty() {
                binds.push(Span::raw("  "));
                binds.push(Span::styled("n/N", key_style));
                binds.push(Span::raw(format!(
                    ": Match {}/{}",
                    search.current_match + 1,
                    search.matches.len()
                )));
            }

            // Add reconnect hint if disconnected
            if !app.connection.is_connected() {
                binds.push(Span::raw("  "));
                binds.push(Span::styled("^R", key_style));
                binds.push(Span::raw(": Reconnect"));
            }

            (
                " NORMAL ",
                Style::default().fg(Color::Black).bg(Color::Blue),
                binds,
            )
        }
        Mode::Search => {
            // Show search input with pattern
            let pattern_style = if search.error {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::White)
            };

            let match_info = if search.matches.is_empty() {
                if search.pattern.is_empty() {
                    String::new()
                } else if search.error {
                    " (invalid regex)".to_string()
                } else {
                    " (no matches)".to_string()
                }
            } else {
                format!(" ({}/{})", search.current_match + 1, search.matches.len())
            };

            let search_char = match search.direction {
                SearchDirection::Forward => "/",
                SearchDirection::Backward => "?",
            };

            (
                " SEARCH ",
                Style::default().fg(Color::Black).bg(Color::Magenta),
                vec![
                    Span::raw(search_char),
                    Span::styled(&search.pattern, pattern_style),
                    Span::styled(match_info, Style::default().fg(Color::DarkGray)),
                    Span::raw("  "),
                    Span::styled("Enter", key_style),
                    Span::raw(": Confirm  "),
                    Span::styled("Esc", key_style),
                    Span::raw(": Cancel"),
                ],
            )
        }
    };

    let mut left_spans = vec![Span::styled(mode_str, mode_style), Span::raw(" ")];
    left_spans.extend(keybinds);
    let left = Paragraph::new(Line::from(left_spans));

    // Right side: connection indicator + terminal size
    let conn_indicator = match &app.connection {
        ConnectionState::Connected => Span::styled(" ", Style::default().fg(Color::Green)),
        ConnectionState::Disconnected { .. } => Span::styled(" ", Style::default().fg(Color::Red)),
    };

    let size_style = match &app.connection {
        ConnectionState::Connected => Style::default().fg(Color::Yellow),
        ConnectionState::Disconnected { .. } => Style::default().fg(Color::DarkGray),
    };

    let right = Paragraph::new(Line::from(vec![
        conn_indicator,
        Span::raw(" "),
        Span::styled(format!("{}x{}", cols, rows), size_style),
    ]))
    .alignment(ratatui::layout::Alignment::Right);

    frame.render_widget(left, chunks[0]);
    frame.render_widget(right, chunks[1]);
}

/// Convert a vt100 cell to a ratatui Style.
fn cell_to_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default();

    style = style.fg(color_to_ratatui(cell.fgcolor()));
    style = style.bg(color_to_ratatui(cell.bgcolor()));

    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.dim() {
        style = style.add_modifier(Modifier::DIM);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }

    style
}

/// Convert a vt100 color to a ratatui Color.
fn color_to_ratatui(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(idx) => Color::Indexed(idx),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Calculate the inner pane size after accounting for borders and padding.
/// Returns (rows, cols) for terminal emulators.
pub fn pane_inner_size(frame_size: Rect, layout: AppLayout) -> (u16, u16) {
    let ui_layout = UiLayout::compute(frame_size, layout);
    // Use app_pane dimensions (same as kernel_pane with 50/50 split)
    let pane = ui_layout.app_pane;

    // Account for border (2) and horizontal padding (2)
    let inner_rows = pane.height.saturating_sub(2);
    let inner_cols = pane.width.saturating_sub(4);

    (inner_rows, inner_cols)
}
