use std::time::Duration;

use regex::bytes::Regex;
use tokio::time::Instant;

/// Timeout for flushing partial kernel messages.
const FLUSH_TIMEOUT: Duration = Duration::from_millis(100);

/// Filters serial data, separating kernel messages from app output.
///
/// Lines starting with `[` are buffered as potential kernel messages.
/// If the line matches the kernel pattern (e.g., `[    0.123456] ...`), it goes
/// to kernel output. Otherwise, it's flushed to app output.
///
/// A timeout ensures partial lines don't get stuck forever.
pub struct KernelFilter {
    kernel_pattern: Regex,
    /// Buffer for a line that might be a kernel message (starts with `[`).
    pending_line: Vec<u8>,
    /// When the pending line started accumulating.
    pending_since: Option<Instant>,
}

impl KernelFilter {
    pub fn new() -> Self {
        Self {
            // Match kernel message: [ followed by timestamp like "    0.123456]"
            kernel_pattern: Regex::new(r"^\[\s*\d+\.\d+\]").expect("invalid kernel regex"),
            pending_line: Vec::new(),
            pending_since: None,
        }
    }

    /// Process incoming data, returning (app_data, kernel_data).
    pub fn process(&mut self, data: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut app_data = Vec::new();
        let mut kernel_data = Vec::new();

        for &byte in data {
            self.process_byte(byte, &mut app_data, &mut kernel_data);
        }

        (app_data, kernel_data)
    }

    fn process_byte(&mut self, byte: u8, app_data: &mut Vec<u8>, kernel_data: &mut Vec<u8>) {
        if !self.pending_line.is_empty() {
            // We're accumulating a potential kernel message
            self.pending_line.push(byte);

            if byte == b'\n' {
                // Line complete - check if it's actually a kernel message
                let line = std::mem::take(&mut self.pending_line);
                self.pending_since = None;

                // Trim trailing \r\n for matching
                let line_content = line
                    .strip_suffix(b"\r\n".as_slice())
                    .or_else(|| line.strip_suffix(b"\n".as_slice()))
                    .unwrap_or(&line);

                if self.kernel_pattern.is_match(line_content) {
                    kernel_data.extend_from_slice(&line);
                } else {
                    app_data.extend_from_slice(&line);
                }
            }
        } else {
            // Not currently buffering
            if byte == b'[' {
                // Start of potential kernel message - begin buffering
                self.pending_line.push(byte);
                self.pending_since = Some(Instant::now());
            } else {
                // Regular byte - pass through immediately
                app_data.push(byte);
            }
        }
    }

    /// Flush pending data. Called when the timeout fires.
    /// Returns data to send to app terminal.
    pub fn flush_pending(&mut self) -> Vec<u8> {
        self.pending_since = None;
        std::mem::take(&mut self.pending_line)
    }

    /// Returns the deadline for the next flush, if there's a pending line.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending_since.map(|started| started + FLUSH_TIMEOUT)
    }
}
