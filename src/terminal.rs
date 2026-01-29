use vt100::Parser;

/// A scrollable terminal backed by a vt100 parser.
pub struct Terminal {
    /// The vt100 parser/screen state
    parser: Parser,
    /// Scrollback offset (0 = at bottom, showing live output)
    scroll_offset: usize,
    /// Cached total scrollback length (updated on scroll operations)
    scrollback_len: usize,
    /// Search state for this terminal
    pub search: SearchState,
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
    /// Matches found: (row, start_col, end_col). Row is absolute (0 = oldest scrollback line).
    pub matches: Vec<SearchMatch>,
    /// Current match index (for n/N navigation).
    pub current_match: usize,
    /// Search direction (/ = forward, ? = backward).
    pub direction: SearchDirection,
}

/// A single search match location.
#[derive(Debug, Clone, Copy)]
pub struct SearchMatch {
    /// Scroll offset when this row is at position `view_row`.
    pub scroll_offset: usize,
    /// Row index within the visible screen (0 = top of visible area).
    pub view_row: usize,
    /// Start column (0-based, byte offset in row text).
    pub start_col: usize,
    /// End column (exclusive).
    pub end_col: usize,
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

    /// `n` key - move in the search direction.
    /// Forward search (`/`): n goes down (newer), N goes up (older)
    /// Backward search (`?`): n goes up (older), N goes down (newer)
    pub fn next_match(&mut self) -> Option<SearchMatch> {
        match self.direction {
            SearchDirection::Forward => self.move_forward(),
            SearchDirection::Backward => self.move_backward(),
        }
    }

    /// `N` key - move opposite to search direction.
    pub fn prev_match(&mut self) -> Option<SearchMatch> {
        match self.direction {
            SearchDirection::Forward => self.move_backward(),
            SearchDirection::Backward => self.move_forward(),
        }
    }

    /// Get current match.
    pub fn current(&self) -> Option<SearchMatch> {
        self.matches.get(self.current_match).copied()
    }
}

impl Terminal {
    /// Create a new terminal with the given dimensions.
    pub fn new(rows: u16, cols: u16, scrollback_lines: usize) -> Self {
        Self {
            parser: Parser::new(rows, cols, scrollback_lines),
            scroll_offset: 0,
            scrollback_len: 0,
            search: SearchState::default(),
        }
    }

    /// Process incoming bytes.
    pub fn process(&mut self, data: &[u8]) {
        self.parser.process(data);
        self.update_scrollback_len();
    }

    /// Update the cached scrollback length by querying vt100.
    fn update_scrollback_len(&mut self) {
        let original = self.scroll_offset;
        self.parser.screen_mut().set_scrollback(usize::MAX);
        self.scrollback_len = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(original);
    }

    /// Get the underlying screen for rendering.
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Resize the terminal.
    pub fn set_size(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
    }

    /// Get the current size (rows, cols).
    #[allow(dead_code)]
    pub fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    /// Scroll up by n lines (clamped to scrollback bounds).
    pub fn scroll_up(&mut self, n: usize) {
        let new_offset = self.scroll_offset.saturating_add(n);
        self.parser.screen_mut().set_scrollback(new_offset);
        // Read back the clamped value
        self.scroll_offset = self.parser.screen().scrollback();
    }

    /// Scroll down by n lines (clamped to 0).
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        self.parser.screen_mut().set_scrollback(self.scroll_offset);
    }

    /// Scroll to top of scrollback.
    pub fn scroll_to_top(&mut self) {
        self.parser.screen_mut().set_scrollback(usize::MAX);
        // Read back the clamped value (actual max scrollback)
        self.scroll_offset = self.parser.screen().scrollback();
    }

    /// Scroll to bottom (live view).
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.parser.screen_mut().set_scrollback(0);
    }

    /// Check if at the bottom (live view).
    pub fn is_at_bottom(&self) -> bool {
        self.scroll_offset == 0
    }

    /// Get current scroll offset.
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Get total scrollback length (lines available to scroll back).
    pub fn scrollback_len(&self) -> usize {
        self.scrollback_len
    }

    /// Clear the screen and scrollback, reset to bottom.
    pub fn clear(&mut self) {
        // Send clear screen + cursor home sequences to the parser
        // \x1b[2J = clear entire screen
        // \x1b[H = cursor to home position
        // \x1b[3J = clear scrollback (xterm extension)
        self.parser.process(b"\x1b[2J\x1b[H\x1b[3J");
        self.scroll_offset = 0;
        self.scrollback_len = 0;
    }

    /// Search for regex matches in the entire terminal (visible + scrollback).
    /// Updates self.search.matches with results.
    pub fn update_search_matches(&mut self) {
        self.search.matches.clear();

        let regex = match &self.search.regex {
            Some(r) => r.clone(),
            None => return,
        };

        let (visible_rows, cols) = self.parser.screen().size();
        let original_offset = self.scroll_offset;

        // Search all content by scrolling through it
        // We iterate from highest offset (oldest content) to 0 (newest content)
        let max_offset = self.scrollback_len;

        for offset in (0..=max_offset).rev() {
            self.parser.screen_mut().set_scrollback(offset);

            // At each offset, determine which rows to search to avoid duplicates
            // - At max_offset: search all rows (0 to visible_rows-1)
            // - At other offsets: only search the bottom row (it's newly revealed)
            let rows_to_search = if offset == max_offset {
                0..visible_rows as usize
            } else {
                (visible_rows as usize - 1)..visible_rows as usize
            };

            for view_row in rows_to_search {
                // Get row text and track byte-to-column mapping
                let mut row_text = String::new();
                let mut byte_to_col: Vec<usize> = Vec::new();

                for col in 0..cols {
                    if let Some(cell) = self.parser.screen().cell(view_row as u16, col) {
                        let contents = cell.contents();
                        for _ in contents.bytes() {
                            byte_to_col.push(col as usize);
                        }
                        row_text.push_str(contents);
                    }
                }

                // Find regex matches and convert byte positions to column positions
                for m in regex.find_iter(&row_text) {
                    let start_col = byte_to_col.get(m.start()).copied().unwrap_or(0);
                    let end_col = byte_to_col
                        .get(m.end().saturating_sub(1))
                        .copied()
                        .map(|c| c + 1)
                        .unwrap_or(cols as usize);

                    self.search.matches.push(SearchMatch {
                        scroll_offset: offset,
                        view_row,
                        start_col,
                        end_col,
                    });
                }
            }
        }

        // Restore original scroll position
        self.parser.screen_mut().set_scrollback(original_offset);
        self.scroll_offset = original_offset;

        // If we found matches, set current_match based on search direction
        if !self.search.matches.is_empty() {
            self.search.current_match = match self.search.direction {
                SearchDirection::Forward => self.find_first_match_at_or_after_view(),
                SearchDirection::Backward => self.find_last_match_at_or_before_view(),
            };
            if let Some(m) = self.search.current() {
                self.scroll_to_match(m);
            }
        } else {
            self.search.current_match = 0;
        }
    }

    /// Scroll to make the given match visible.
    pub fn scroll_to_match(&mut self, m: SearchMatch) {
        // The match was found at a specific scroll_offset with a specific view_row
        // To center it, we want view_row to be near the middle of the screen
        let (visible_rows, _) = self.parser.screen().size();
        let half_screen = visible_rows as usize / 2;

        // If view_row < half_screen, we need to scroll up (increase offset)
        // If view_row > half_screen, we need to scroll down (decrease offset)
        let adjustment = m.view_row as isize - half_screen as isize;
        let new_offset = (m.scroll_offset as isize - adjustment)
            .max(0)
            .min(self.scrollback_len as isize) as usize;

        self.parser.screen_mut().set_scrollback(new_offset);
        self.scroll_offset = self.parser.screen().scrollback();
    }

    /// Find the index of the first match at or after the current view (for `/` forward search).
    /// If no match exists after current position, wraps to the first match.
    fn find_first_match_at_or_after_view(&self) -> usize {
        if self.search.matches.is_empty() {
            return 0;
        }

        // A match is "at or after" current view if its scroll_offset <= current scroll_offset
        // (lower offset = further down = newer content)
        // Or if same offset, any view_row is fine
        self.search
            .matches
            .iter()
            .enumerate()
            .find(|(_, m)| m.scroll_offset <= self.scroll_offset)
            .map(|(idx, _)| idx)
            .unwrap_or(0) // Wrap to first match
    }

    /// Find the index of the last match at or before the current view (for `?` backward search).
    /// If no match exists before current position, wraps to the last match.
    fn find_last_match_at_or_before_view(&self) -> usize {
        if self.search.matches.is_empty() {
            return 0;
        }

        // A match is "at or before" current view if its scroll_offset >= current scroll_offset
        // (higher offset = further up = older content)
        self.search
            .matches
            .iter()
            .enumerate()
            .rev()
            .find(|(_, m)| m.scroll_offset >= self.scroll_offset)
            .map(|(idx, _)| idx)
            .unwrap_or(self.search.matches.len().saturating_sub(1)) // Wrap to last match
    }

    /// Check if a view row/col at the current scroll position is the current search match.
    pub fn is_current_match(&self, view_row: usize, col: usize) -> bool {
        if let Some(m) = self.search.matches.get(self.search.current_match) {
            self.match_is_visible(m, view_row, col)
        } else {
            false
        }
    }

    /// Check if a view row/col at the current scroll position is any search match.
    pub fn is_any_match(&self, view_row: usize, col: usize) -> bool {
        self.search
            .matches
            .iter()
            .any(|m| self.match_is_visible(m, view_row, col))
    }

    /// Check if a match is visible at the given view_row and col.
    fn match_is_visible(&self, m: &SearchMatch, view_row: usize, col: usize) -> bool {
        // A match at (scroll_offset=S, view_row=V) is visible at current scroll_offset=C
        // When we scroll down (decrease offset), content moves up on screen.
        // offset_diff = S - C (positive means match was found at higher/older scroll position)
        // visible_view_row = V - offset_diff
        // Example: match at offset=10, view_row=5. Current offset=8.
        //   offset_diff = 10 - 8 = 2 (we scrolled down 2)
        //   visible_view_row = 5 - 2 = 3 (content moved up 2 rows)
        let offset_diff = m.scroll_offset as isize - self.scroll_offset as isize;
        let visible_view_row = m.view_row as isize - offset_diff;

        visible_view_row == view_row as isize && col >= m.start_col && col < m.end_col
    }
}
