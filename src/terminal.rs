use vt100::Parser;

/// A scrollable terminal backed by a vt100 parser.
pub struct Terminal {
    /// The vt100 parser/screen state
    parser: Parser,
    /// Scrollback offset (0 = at bottom, showing live output)
    scroll_offset: usize,
    /// Cached total scrollback length (updated on scroll operations)
    scrollback_len: usize,
}

impl Terminal {
    /// Create a new terminal with the given dimensions.
    pub fn new(rows: u16, cols: u16, scrollback_lines: usize) -> Self {
        Self {
            parser: Parser::new(rows, cols, scrollback_lines),
            scroll_offset: 0,
            scrollback_len: 0,
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
}
