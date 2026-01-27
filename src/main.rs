use clap::Parser;
use color_eyre::Result;

use consolate::app::App;
use consolate::input::{handle_key, KeyAction};
use consolate::serial::{self, SerialConfig};
use consolate::tui::{Event, TerminalEvent, Tui, TuiConfig};
use consolate::ui;

/// A TUI serial console that separates kernel and application messages.
#[derive(Parser, Debug)]
#[command(name = "consolate", version, about)]
struct Args {
    /// Serial port device path
    #[arg(short, long, default_value = "/dev/ttyUSB0")]
    port: String,

    /// Baud rate
    #[arg(short, long, default_value_t = 115200)]
    baud: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    consolate::setup()?;

    let args = Args::parse();

    // Open serial port
    let config = SerialConfig::new(&args.port, args.baud);
    let (serial_rx, serial_writer) = serial::connect(&config)?;

    // Setup TUI
    let mut tui = Tui::new(TuiConfig::default())?;
    let size = tui.size()?;

    // Track frame size for layout changes
    let mut frame_size = ratatui::layout::Rect::new(0, 0, size.width, size.height);

    // Initialize app with terminal size (accounting for UI chrome)
    let (rows, cols) = ui::pane_inner_size(frame_size, Default::default());
    let mut app = App::new(rows, cols);

    tui.enter(serial_rx)?;

    // Main event loop
    while let Some(event) = tui.next().await {
        match event {
            Event::Init => {}
            Event::AppData(data) => {
                app.app_terminal.process(&data);
            }
            Event::KernelData(data) => {
                app.kernel_terminal.process(&data);
            }
            Event::SerialError(e) => {
                tracing::error!("Serial error: {}", e);
                let msg = format!("[ERROR] Serial: {}\r\n", e);
                app.app_terminal.process(msg.as_bytes());
            }
            Event::Terminal(term_event) => match term_event {
                TerminalEvent::Key(key) => match handle_key(&mut app, key) {
                    KeyAction::Send(bytes) => {
                        if let Err(e) = serial_writer.send_key(bytes).await {
                            tracing::error!("Failed to send: {}", e);
                        }
                    }
                    KeyAction::Quit => break,
                    KeyAction::LayoutChanged => {
                        let (rows, cols) = ui::pane_inner_size(frame_size, app.layout);
                        app.resize(rows, cols);
                    }
                    KeyAction::None => {}
                },
                TerminalEvent::Resize(width, height) => {
                    frame_size = ratatui::layout::Rect::new(0, 0, width, height);
                    let (rows, cols) = ui::pane_inner_size(frame_size, app.layout);
                    app.resize(rows, cols);
                }
                TerminalEvent::FocusGained
                | TerminalEvent::FocusLost
                | TerminalEvent::Paste(_)
                | TerminalEvent::Mouse(_)
                | TerminalEvent::Error(_) => {}
            },
        }

        tui.draw(|frame| ui::render(frame, &app))?;
    }

    Ok(())
}
