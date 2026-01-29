use clap::Parser;
use color_eyre::eyre::bail;
use color_eyre::Result;

use consolate::app::App;
use consolate::input::{handle_key, handle_mouse, handle_paste, KeyAction};
use consolate::serial::{self, SerialConfig, SerialWriter};
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

    // Serial configuration (kept for reconnection)
    let serial_config = SerialConfig::new(&args.port, args.baud);

    // Open serial port
    let (serial_rx, serial_writer) = serial::connect(&serial_config)?;
    let mut serial_writer: Option<SerialWriter> = Some(serial_writer);

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
                app.set_disconnected(e.to_string());
                serial_writer = None;
            }
            Event::Terminal(term_event) => match term_event {
                TerminalEvent::Key(key) => {
                    // Clear any transient message on key press
                    app.clear_message();

                    match handle_key(&mut app, key) {
                        KeyAction::Send(bytes) => {
                            if let Some(ref writer) = serial_writer {
                                if let Err(e) = writer.send_key(bytes).await {
                                    tracing::error!("Failed to send: {}", e);
                                    app.set_disconnected(e.to_string());
                                    serial_writer = None;
                                }
                            }
                        }
                        KeyAction::Quit => break,
                        KeyAction::LayoutChanged => {
                            let (rows, cols) = ui::pane_inner_size(frame_size, app.layout);
                            app.resize(rows, cols);
                        }
                        KeyAction::Reconnect => match serial::connect(&serial_config) {
                            Ok((new_rx, new_writer)) => {
                                tui.restart(new_rx);
                                serial_writer = Some(new_writer);
                                app.set_connected();
                            }
                            Err(e) => {
                                tracing::error!("Reconnect failed: {}", e);
                                app.show_error(format!("Reconnect failed: {}", e));
                            }
                        },
                        KeyAction::None => {}
                    }
                }
                TerminalEvent::Resize(width, height) => {
                    frame_size = ratatui::layout::Rect::new(0, 0, width, height);
                    let (rows, cols) = ui::pane_inner_size(frame_size, app.layout);
                    app.resize(rows, cols);
                }
                TerminalEvent::Paste(text) => {
                    if let KeyAction::Send(bytes) = handle_paste(&mut app, &text) {
                        if let Some(ref writer) = serial_writer {
                            if let Err(e) = writer.send_key(bytes).await {
                                tracing::error!("Failed to send paste: {}", e);
                                app.set_disconnected(e.to_string());
                                serial_writer = None;
                            }
                        }
                    }
                }
                TerminalEvent::Mouse(mouse) => {
                    let layout = ui::UiLayout::compute(frame_size, app.layout);
                    handle_mouse(&mut app, mouse, layout.pane_areas());
                }
                TerminalEvent::FocusGained | TerminalEvent::FocusLost => {}
                TerminalEvent::Error(err) => {
                    tui.exit()?;
                    bail!(err);
                }
            },
        }

        tui.draw(|frame| ui::render(frame, &app))?;
    }

    Ok(())
}
