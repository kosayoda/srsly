use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph},
    Frame,
};

use crate::app::{App, Focus, Layout as AppLayout, Mode};

/// Render the application UI.
pub fn render(frame: &mut Frame, app: &App) {
    let size = frame.area();

    // Split into main content area and keybinds bar at bottom
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(size);

    let content_area = main_chunks[0];
    let keybinds_area = main_chunks[1];

    // Split content area based on current layout
    let direction = match app.layout {
        AppLayout::Vertical => Direction::Horizontal,
        AppLayout::Horizontal => Direction::Vertical,
    };

    let pane_chunks = Layout::default()
        .direction(direction)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(content_area);

    // Render kernel pane
    render_terminal_pane(
        frame,
        pane_chunks[0],
        "Kernel",
        app.kernel_terminal.screen(),
        app.kernel_terminal.scroll_offset(),
        app.kernel_terminal.scrollback_len(),
        app.focus == Focus::Kernel,
    );

    // Render app pane
    render_terminal_pane(
        frame,
        pane_chunks[1],
        "App",
        app.app_terminal.screen(),
        app.app_terminal.scroll_offset(),
        app.app_terminal.scrollback_len(),
        app.focus == Focus::App,
    );

    // Render keybinds bar
    render_keybinds(frame, keybinds_area, app);
}

/// Render a vt100 terminal pane.
fn render_terminal_pane(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    screen: &vt100::Screen,
    scroll_offset: usize,
    scrollback_len: usize,
    focused: bool,
) {
    let border_style = if focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let title_suffix = if scroll_offset == 0 {
        format!("-/{}", scrollback_len)
    } else {
        format!("{}/{}", scroll_offset, scrollback_len)
    };

    let block = Block::default()
        .padding(Padding::horizontal(1))
        .title(format!(" {} ({}) ", title, title_suffix))
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
        let mut spans: Vec<Span> = Vec::new();
        let mut current_text = String::new();
        let mut current_style = Style::default();

        for col in 0..cols.min(inner.width) {
            if let Some(cell) = screen.cell(row, col) {
                let cell_style = cell_to_style(cell);

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
    if focused && !screen.hide_cursor() {
        let (cursor_row, cursor_col) = screen.cursor_position();
        let cursor_x = inner.x + cursor_col.min(inner.width.saturating_sub(1));
        let cursor_y = inner.y + cursor_row.min(inner.height.saturating_sub(1));
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

/// Render the status bar.
fn render_keybinds(frame: &mut Frame, area: Rect, app: &App) {
    let (rows, cols) = app.app_terminal.size();

    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .horizontal_margin(1)
        .constraints([Constraint::Min(0), Constraint::Percentage(30)])
        .split(area);

    // Left side: mode indicator and keybinds
    let (mode_str, mode_style, keybinds) = match app.mode {
        Mode::Insert => (
            " INSERT ",
            Style::default().fg(Color::Black).bg(Color::Green),
            vec![Span::styled("Esc", key_style), Span::raw(": Normal Mode")],
        ),
        Mode::Normal => (
            " NORMAL ",
            Style::default().fg(Color::Black).bg(Color::Blue),
            vec![
                Span::styled("i", key_style),
                Span::raw(": Insert Mode  "),
                Span::styled("q", key_style),
                Span::raw(": Quit  "),
                Span::styled("s", key_style),
                Span::raw(": Layout  "),
                Span::styled("r", key_style),
                Span::raw(": Resize  "),
                Span::styled("w", key_style),
                Span::raw(": Switch pane  "),
                Span::styled("j/k", key_style),
                Span::raw(": Scroll  "),
                Span::styled("c", key_style),
                Span::raw(": Clear"),
            ],
        ),
    };

    let mut left_spans = vec![Span::styled(mode_str, mode_style), Span::raw(" ")];
    left_spans.extend(keybinds);
    let left = Paragraph::new(Line::from(left_spans));

    // Right side: terminal size
    let right = Paragraph::new(Line::from(vec![Span::styled(
        format!("{}x{}", cols, rows),
        Style::default().fg(Color::Yellow),
    )]))
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
    // Account for keybinds bar (1 row)
    let content_height = frame_size.height.saturating_sub(1);

    let (pane_height, pane_width) = match layout {
        AppLayout::Vertical => (content_height, frame_size.width / 2),
        AppLayout::Horizontal => (content_height / 2, frame_size.width),
    };

    // Account for border (2) and horizontal padding (2)
    let inner_rows = pane_height.saturating_sub(2);
    let inner_cols = pane_width.saturating_sub(4);

    (inner_rows, inner_cols)
}
