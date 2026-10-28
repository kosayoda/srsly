use std::io::Write;

use base64::{Engine, engine::general_purpose::STANDARD};

/// Copy text to the system clipboard with OSC 52. Inside tmux this needs
/// `set -g set-clipboard on`.
pub fn copy(text: &str) -> std::io::Result<()> {
    let mut stdout = std::io::stdout();
    write!(stdout, "\x1b]52;c;{}\x07", STANDARD.encode(text))?;
    stdout.flush()
}
