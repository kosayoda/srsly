use libghostty_vt::{
    Terminal as Vt, TerminalOptions,
    error::Error,
    fmt::{Format, Formatter, FormatterOptions},
    render::{CellIterator, RenderState, RowIterator},
    screen::{CellContentTag, CellWide, TrackedGridRef},
    style::{Style, StyleColor},
    terminal::{Point, PointCoordinate, PointSpace, ScrollViewport},
};

/// A scrollable terminal backed by libghostty-vt.
pub struct Terminal {
    vt: Vt<'static, 'static>,
    render: RenderState<'static>,
    row_iter: RowIterator<'static>,
    cell_iter: CellIterator<'static>,
    /// Search state for this terminal
    pub search: SearchState,
    /// Tracks the screen row of the newest match so match rows can be
    /// shifted when scrollback is evicted.
    search_anchor: Option<(TrackedGridRef, u32)>,
    /// New output arrived since matches were computed.
    search_stale: bool,
}

/// Search direction (vim-style).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchDirection {
    /// `/` search - forward (toward bottom/newer)
    #[default]
    Forward,
    /// `?` search - backward (toward top/older)
    Backward,
}

/// State for incremental search within a terminal.
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    /// Current search pattern being typed.
    pub pattern: String,
    /// Compiled regex (None if pattern is empty or invalid).
    pub regex: Option<regex::Regex>,
    /// Whether the pattern is invalid.
    pub error: bool,
    /// Matches in screen order.
    pub matches: Vec<SearchMatch>,
    /// Current match index (for n/N navigation).
    pub current_match: usize,
    /// Search direction (/ = forward, ? = backward).
    pub direction: SearchDirection,
    /// Screen rows visible when the search started; matches are picked
    /// relative to this so typing doesn't drift the view.
    origin: Option<(u32, u32)>,
}

/// A search match in screen coordinates (row 0 = oldest scrollback row).
/// Matches can span soft-wrapped rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    /// (row, col) of the first cell.
    pub start: (u32, u16),
    /// (row, col) one past the last cell.
    pub end: (u32, u16),
}

impl SearchMatch {
    pub fn contains(&self, row: u32, col: u16) -> bool {
        (row, col) >= self.start && (row, col) < self.end
    }
}

impl SearchState {
    /// Update the search pattern and recompile regex.
    pub fn set_pattern(&mut self, pattern: String) {
        self.pattern = pattern;
        self.compile_regex();
    }

    /// Add a character to the pattern.
    pub fn push_char(&mut self, c: char) {
        self.pattern.push(c);
        self.compile_regex();
    }

    /// Remove the last character from the pattern.
    pub fn pop_char(&mut self) {
        self.pattern.pop();
        self.compile_regex();
    }

    /// Compile the regex from the current pattern.
    fn compile_regex(&mut self) {
        if self.pattern.is_empty() {
            self.regex = None;
            self.error = false;
        } else {
            // Case-insensitive search by default
            match regex::RegexBuilder::new(&self.pattern)
                .case_insensitive(true)
                .build()
            {
                Ok(re) => {
                    self.regex = Some(re);
                    self.error = false;
                }
                Err(_) => {
                    self.regex = None;
                    self.error = true;
                }
            }
        }
        // Reset matches when pattern changes (will be repopulated by find_matches)
        self.matches.clear();
        self.current_match = 0;
    }

    /// Clear the search state entirely.
    pub fn clear(&mut self) {
        self.pattern.clear();
        self.regex = None;
        self.error = false;
        self.matches.clear();
        self.current_match = 0;
        self.origin = None;
        // Note: direction is preserved so repeated searches use same direction
    }

    /// Move forward in matches (toward higher indices = newer content).
    fn move_forward(&mut self) -> Option<SearchMatch> {
        if self.matches.is_empty() {
            return None;
        }
        self.current_match = (self.current_match + 1) % self.matches.len();
        self.matches.get(self.current_match).copied()
    }

    /// Move backward in matches (toward lower indices = older content).
    fn move_backward(&mut self) -> Option<SearchMatch> {
        if self.matches.is_empty() {
            return None;
        }
        self.current_match = self
            .current_match
            .checked_sub(1)
            .unwrap_or(self.matches.len() - 1);
        self.matches.get(self.current_match).copied()
    }

    /// Get current match.
    pub fn current(&self) -> Option<SearchMatch> {
        self.matches.get(self.current_match).copied()
    }

    /// Matches that intersect screen rows `top..top + rows`.
    pub fn visible(&self, top: u32, rows: u16) -> &[SearchMatch] {
        let bottom = top + rows as u32;
        let lo = self.matches.partition_point(|m| m.end.0 < top);
        let hi = self.matches.partition_point(|m| m.start.0 < bottom);
        &self.matches[lo..hi.max(lo)]
    }
}

/// One rendered cell. Spacer cells after wide characters are omitted.
pub struct ViewCell {
    pub col: u16,
    pub text: String,
    pub style: Style,
}

pub struct ViewRow {
    pub cells: Vec<ViewCell>,
    /// Selected column range, inclusive.
    pub selection: Option<(u16, u16)>,
}

/// A snapshot of the visible rows.
#[derive(Default)]
pub struct Viewport {
    pub rows: Vec<ViewRow>,
    /// Screen row of the first viewport row.
    pub top: u32,
    pub cursor: Option<(u16, u16)>,
}

impl Terminal {
    /// Create a new terminal with the given dimensions.
    pub fn new(rows: u16, cols: u16, scrollback_bytes: usize) -> Result<Self, Error> {
        Ok(Self {
            vt: Vt::new(TerminalOptions {
                cols: cols.max(1),
                rows: rows.max(1),
                max_scrollback: scrollback_bytes,
            })?,
            render: RenderState::new()?,
            row_iter: RowIterator::new()?,
            cell_iter: CellIterator::new()?,
            search: SearchState::default(),
            search_anchor: None,
            search_stale: false,
        })
    }

    /// Process incoming bytes.
    pub fn process(&mut self, data: &[u8]) {
        self.vt.vt_write(data);
        if self.search.regex.is_some() {
            self.search_stale = true;
            self.shift_matches();
        }
    }

    /// Resize the terminal.
    pub fn set_size(&mut self, rows: u16, cols: u16) {
        if let Err(e) = self.vt.resize(cols.max(1), rows.max(1), 1, 1) {
            tracing::error!("Resize failed: {e}");
        }
        if self.search.regex.is_some() {
            self.update_search_matches();
        }
    }

    /// Get the current size (rows, cols).
    pub fn size(&self) -> (u16, u16) {
        (
            self.vt.rows().unwrap_or_default(),
            self.vt.cols().unwrap_or_default(),
        )
    }

    /// Scroll up by n lines.
    pub fn scroll_up(&mut self, n: usize) {
        self.vt
            .scroll_viewport(ScrollViewport::Delta(-(n as isize)));
    }

    /// Scroll down by n lines.
    pub fn scroll_down(&mut self, n: usize) {
        self.vt.scroll_viewport(ScrollViewport::Delta(n as isize));
    }

    /// Scroll to top of scrollback.
    pub fn scroll_to_top(&mut self) {
        self.vt.scroll_viewport(ScrollViewport::Top);
    }

    /// Scroll to bottom (live view).
    pub fn scroll_to_bottom(&mut self) {
        self.vt.scroll_viewport(ScrollViewport::Bottom);
    }

    /// Lines scrolled up from the bottom (0 = live view).
    pub fn scroll_offset(&self) -> usize {
        self.vt
            .scrollbar()
            .map(|s| (s.total - s.len - s.offset) as usize)
            .unwrap_or_default()
    }

    /// Lines available to scroll back.
    pub fn scrollback_len(&self) -> usize {
        self.vt.scrollback_rows().unwrap_or_default()
    }

    /// Clear the screen and scrollback.
    pub fn clear(&mut self) {
        self.vt.vt_write(b"\x1b[H\x1b[2J\x1b[3J");
        self.search.matches.clear();
        self.search.current_match = 0;
        self.search_anchor = None;
    }

    /// Snapshot the visible rows for rendering.
    pub fn viewport(&mut self) -> Viewport {
        self.try_viewport().unwrap_or_else(|e| {
            tracing::error!("Failed to read viewport: {e}");
            Viewport::default()
        })
    }

    fn try_viewport(&mut self) -> Result<Viewport, Error> {
        let top = self.vt.scrollbar()?.offset as u32;
        let snapshot = self.render.update(&self.vt)?;
        let cursor = snapshot
            .cursor_viewport()?
            .filter(|_| snapshot.cursor_visible().unwrap_or(true))
            .map(|c| (c.x, c.y));

        let mut rows = Vec::new();
        let mut row_iter = self.row_iter.update(&snapshot)?;
        while let Some(row) = row_iter.next() {
            let mut cells = Vec::new();
            let mut cell_iter = self.cell_iter.update(row)?;
            let mut col = 0;
            while let Some(cell) = cell_iter.next() {
                let raw = cell.raw_cell()?;
                if raw.wide()? != CellWide::SpacerTail {
                    let mut text = String::new();
                    cell.graphemes_utf8(&mut text)?;
                    let mut style = if cell.has_styling()? {
                        cell.style()?
                    } else {
                        Style::default()
                    };
                    match raw.content_tag()? {
                        CellContentTag::BgColorPalette => {
                            style.bg_color = StyleColor::Palette(raw.bg_color_palette()?)
                        }
                        CellContentTag::BgColorRgb => {
                            style.bg_color = StyleColor::Rgb(raw.bg_color_rgb()?)
                        }
                        _ => {}
                    }
                    cells.push(ViewCell { col, text, style });
                }
                col += 1;
            }
            let selection = row.selection()?.map(|s| (s.start_x, s.end_x));
            rows.push(ViewRow { cells, selection });
        }

        Ok(Viewport { rows, top, cursor })
    }

    /// Search the whole buffer for the current regex.
    pub fn update_search_matches(&mut self) {
        let previous = self.search.current().map(|m| m.start);
        self.search.matches.clear();
        self.search.current_match = 0;
        self.search_anchor = None;
        self.search_stale = false;

        let Some(regex) = self.search.regex.clone() else {
            return;
        };
        match self.find_matches(&regex) {
            Ok(matches) => self.search.matches = matches,
            Err(e) => {
                tracing::error!("Search failed: {e}");
                return;
            }
        }
        let Some(last) = self.search.matches.last() else {
            return;
        };
        let row = last.start.0;
        self.search_anchor = self
            .vt
            .track_grid_ref(Point::Screen(PointCoordinate { x: 0, y: row }))
            .ok()
            .map(|r| (r, row));

        if let Some(i) =
            previous.and_then(|p| self.search.matches.iter().position(|m| m.start == p))
        {
            self.search.current_match = i;
            return;
        }

        let (top, bottom) = *self.search.origin.get_or_insert_with(|| {
            let top = self.vt.scrollbar().map(|s| s.offset as u32).unwrap_or(0);
            (top, top + self.vt.rows().unwrap_or_default() as u32)
        });
        let matches = &self.search.matches;
        self.search.current_match = match self.search.direction {
            SearchDirection::Forward => matches.iter().position(|m| m.start.0 >= top).unwrap_or(0),
            SearchDirection::Backward => matches
                .iter()
                .rposition(|m| m.start.0 < bottom)
                .unwrap_or(matches.len() - 1),
        };
        if let Some(m) = self.search.current() {
            self.scroll_to_match(m);
        }
    }

    fn find_matches(&self, regex: &regex::Regex) -> Result<Vec<SearchMatch>, Error> {
        // One line per screen row; soft-wrapped rows are joined below.
        let opts = FormatterOptions::new()
            .with_format(Format::Plain)
            .with_unwrap(false)
            .with_trim(false);
        let bytes = Formatter::new(&self.vt, opts)?.format_alloc(None)?;
        let text = String::from_utf8_lossy(&bytes);
        let rows: Vec<&str> = text.split('\n').collect();

        let mut matches = Vec::new();
        let mut line = String::new();
        // (screen row, byte offset in `line`) for each row in the line
        let mut segments: Vec<(u32, usize)> = Vec::new();

        for (y, row_text) in rows.iter().enumerate() {
            let y = y as u32;
            segments.push((y, line.len()));
            line.push_str(row_text);
            if self.is_wrapped(y) && (y as usize) < rows.len() - 1 {
                continue;
            }

            for m in regex.find_iter(&line).filter(|m| !m.is_empty()) {
                let start = self.byte_to_cell(&rows, &segments, m.start())?;
                let (row, col) = self.byte_to_cell(&rows, &segments, m.end() - 1)?;
                matches.push(SearchMatch {
                    start,
                    end: (row, col + 1),
                });
            }
            line.clear();
            segments.clear();
        }

        Ok(matches)
    }

    fn is_wrapped(&self, y: u32) -> bool {
        self.vt
            .grid_ref(Point::Screen(PointCoordinate { x: 0, y }))
            .and_then(|r| r.row())
            .and_then(|r| r.is_wrapped())
            .unwrap_or(false)
    }

    /// Convert a byte offset in a joined line to the screen cell holding it.
    fn byte_to_cell(
        &self,
        rows: &[&str],
        segments: &[(u32, usize)],
        byte: usize,
    ) -> Result<(u32, u16), Error> {
        let i = segments.partition_point(|&(_, start)| start <= byte) - 1;
        let (y, start) = segments[i];
        let target = byte - start;
        let row_text = rows[y as usize];

        // Walk the row's cells, matching each one's text to the formatter output.
        let cols = self.vt.cols()?;
        let mut offset = 0;
        let mut buf = ['\0'; 16];
        for x in 0..cols {
            let grid = self.vt.grid_ref(Point::Screen(PointCoordinate { x, y }))?;
            let cell = grid.cell()?;
            if matches!(cell.wide()?, CellWide::SpacerTail | CellWide::SpacerHead) {
                continue;
            }
            let len = if cell.has_text()? {
                let n = grid.graphemes(&mut buf)?;
                buf[..n].iter().map(|c| c.len_utf8()).sum()
            } else {
                1
            };
            if target < offset + len {
                return Ok((y, x));
            }
            offset += len;
            if offset >= row_text.len() {
                break;
            }
        }
        Ok((y, cols.saturating_sub(1)))
    }

    /// Adjust match rows for scrollback evicted since they were computed.
    fn shift_matches(&mut self) {
        let Some((anchor, row)) = &mut self.search_anchor else {
            return;
        };
        let Some(now) = anchor.point(PointSpace::Screen).ok().flatten() else {
            self.search.matches.clear();
            self.search.current_match = 0;
            self.search_anchor = None;
            return;
        };
        let shift = row.saturating_sub(now.y);
        if shift == 0 {
            return;
        }
        *row = now.y;

        if let Some((top, bottom)) = &mut self.search.origin {
            *top = top.saturating_sub(shift);
            *bottom = bottom.saturating_sub(shift);
        }
        let evicted = self.search.matches.partition_point(|m| m.start.0 < shift);
        self.search.matches.drain(..evicted);
        for m in &mut self.search.matches {
            m.start.0 -= shift;
            m.end.0 -= shift;
        }
        self.search.current_match = self.search.current_match.saturating_sub(evicted);
    }

    /// `n` key: move in the search direction.
    /// Forward search (`/`): n goes down (newer), N goes up (older)
    /// Backward search (`?`): n goes up (older), N goes down (newer)
    pub fn next_match(&mut self) {
        let forward = self.search.direction == SearchDirection::Forward;
        self.step_match(forward);
    }

    /// `N` key: move opposite to the search direction.
    pub fn prev_match(&mut self) {
        let forward = self.search.direction == SearchDirection::Backward;
        self.step_match(forward);
    }

    fn step_match(&mut self, forward: bool) {
        if self.search_stale {
            self.update_search_matches();
        }
        let m = if forward {
            self.search.move_forward()
        } else {
            self.search.move_backward()
        };
        if let Some(m) = m {
            self.scroll_to_match(m);
        }
    }

    /// Scroll so the match is near the middle of the viewport.
    pub fn scroll_to_match(&mut self, m: SearchMatch) {
        let half = self.size().0 as u32 / 2;
        let row = m.start.0.saturating_sub(half);
        self.vt.scroll_viewport(ScrollViewport::Row(row as usize));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(t: &mut Terminal, pattern: &str) -> Vec<SearchMatch> {
        t.search.set_pattern(pattern.into());
        t.update_search_matches();
        t.search.matches.clone()
    }

    #[test]
    fn search_spans_soft_wraps_and_wide_chars() {
        let mut t = Terminal::new(4, 10, 1_000_000).unwrap();
        t.process("xx日本abcdefgh\r\n".as_bytes());
        // Row 0: "xx日本abcd" (10 cells), row 1: "efgh"
        assert_eq!(
            search(&mut t, "本ab"),
            [SearchMatch {
                start: (0, 4),
                end: (0, 8)
            }]
        );
        assert_eq!(
            search(&mut t, "cdef"),
            [SearchMatch {
                start: (0, 8),
                end: (1, 2)
            }]
        );
    }

    #[test]
    fn matches_follow_eviction() {
        let mut t = Terminal::new(4, 120, 10_000).unwrap();
        for i in 0..2000 {
            t.process(format!("line {i}\r\n").as_bytes());
        }
        let pattern = r"line \d*7\b";
        let first = search(&mut t, pattern)[0];
        for i in 0..20 {
            for j in 0..100 {
                t.process(format!("more {i} {j}\r\n").as_bytes());
            }
            let shifted = t.search.matches.clone();
            assert_eq!(shifted, search(&mut t, pattern));
        }
        assert_ne!(t.search.matches.first(), Some(&first));
    }
}
