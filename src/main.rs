use clap::{Parser, Subcommand};
use color_eyre::Result;
use color_eyre::eyre::bail;
use dialoguer::Select;
use dialoguer::theme::ColorfulTheme;
use owo_colors::OwoColorize;

use srsly::app::{App, Focus};
use srsly::clipboard;
use srsly::input::{KeyAction, handle_key, handle_mouse, handle_paste};
use srsly::serial::{self, SerialConfig, SerialWriter};
use srsly::tui::{Event, TerminalEvent, Tui, TuiConfig};
use srsly::ui;

/// A TUI serial console that separates kernel and "application" messages.
#[derive(Parser, Debug)]
#[command(name = "srsly", version, about)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// List available serial ports
    List,
    /// Connect to a serial port
    Connect {
        /// Serial port device path (interactive selection if not provided)
        #[arg(short, long)]
        port: Option<String>,

        /// Baud rate
        #[arg(short, long, default_value_t = 115200)]
        baud: u32,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    srsly::setup()?;

    let Args { command } = Args::parse();

    match command {
        Some(Command::List) => {
            list_ports();
        }
        Some(Command::Connect { port, baud }) => {
            let port = match port {
                Some(p) => p,
                None => select_port()?,
            };
            run_connect(&port, baud).await?;
        }
        None => {
            let port = select_port()?;
            run_connect(&port, 115200).await?;
        }
    }

    Ok(())
}

fn select_port() -> Result<String> {
    let ports = tokio_serial::available_ports()?;

    if ports.is_empty() {
        bail!("No serial ports found");
    }

    let items: Vec<String> = ports
        .iter()
        .map(|p| {
            let name = &p.port_name;
            match &p.port_type {
                tokio_serial::SerialPortType::UsbPort(info) => {
                    let product = info.product.as_deref().unwrap_or("Unknown");
                    let manufacturer = info.manufacturer.as_deref().unwrap_or("Unknown");
                    format!("{} - {} [{}]", name, product, manufacturer)
                }
                tokio_serial::SerialPortType::BluetoothPort => {
                    format!("{} - Bluetooth", name)
                }
                tokio_serial::SerialPortType::PciPort => {
                    format!("{} - PCI", name)
                }
                tokio_serial::SerialPortType::Unknown => name.clone(),
            }
        })
        .collect();

    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select a serial port")
        .items(&items)
        .default(0)
        .interact()?;

    Ok(ports[selection].port_name.clone())
}

fn list_ports() {
    let ports = tokio_serial::available_ports();
    match ports {
        Ok(ports) if ports.is_empty() => {
            println!("{}", "No serial ports found.".yellow());
        }
        Ok(ports) => {
            println!("{}", "Available serial ports:".bold());

            for port in ports {
                println!("  {}", port.port_name.cyan().bold());
                match port.port_type {
                    tokio_serial::SerialPortType::UsbPort(info) => {
                        if let Some(manufacturer) = &info.manufacturer {
                            println!("    {}: {}", "Manufacturer".dimmed(), manufacturer);
                        }
                        if let Some(product) = &info.product {
                            println!("    {}: {}", "Product".dimmed(), product.green());
                        }
                        if let Some(serial) = &info.serial_number {
                            println!("    {}: {}", "Serial".dimmed(), serial);
                        }
                        println!(
                            "    {}: {}",
                            "VID:PID".dimmed(),
                            format!("{:04x}:{:04x}", info.vid, info.pid).yellow()
                        );
                    }
                    tokio_serial::SerialPortType::BluetoothPort => {
                        println!("    {}: {}", "Type".dimmed(), "Bluetooth".blue());
                    }
                    tokio_serial::SerialPortType::PciPort => {
                        println!("    {}: {}", "Type".dimmed(), "PCI".magenta());
                    }
                    tokio_serial::SerialPortType::Unknown => {
                        println!("    {}: {}", "Type".dimmed(), "Unknown".dimmed());
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("{}: {}", "Error listing ports".red().bold(), e);
        }
    }
}

async fn run_connect(port: &str, baud: u32) -> Result<()> {
    // Serial configuration (kept for reconnection)
    let serial_config = SerialConfig::new(port, baud);

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
    let mut app = App::new(rows, cols)?;

    tui.enter(serial_rx)?;

    // Helper to update the TUI's no-response deadline from app state
    let update_no_response_deadline = |tui: &Tui, app: &App| {
        if let Some(deadline) = app.no_response_deadline() {
            tui.set_no_response_deadline(Some(tokio::time::Instant::from_std(deadline)));
        } else {
            tui.set_no_response_deadline(None);
        }
    };

    let mut pager: Option<Pager> = None;

    // Main event loop
    loop {
        let event = tokio::select! {
            event = tui.next() => match event {
                Some(event) => event,
                None => break,
            },
            status = wait_pager(&mut pager) => {
                if let Some(Pager { path, .. }) = pager.take() {
                    let _ = std::fs::remove_file(path);
                }
                tui.resume()?;
                if let Err(e) = status {
                    app.show_error(format!("Pager failed: {e}"));
                }
                // The terminal may have been resized while the pager had it
                let size = tui.size()?;
                frame_size = ratatui::layout::Rect::new(0, 0, size.width, size.height);
                let (rows, cols) = ui::pane_inner_size(frame_size, app.layout);
                app.resize(rows, cols);
                tui.draw(|frame| ui::render(frame, &mut app))?;
                continue;
            }
        };

        match event {
            Event::Init | Event::NoResponseTimeout => {
                // NoResponseTimeout just triggers a redraw (handled below)
            }
            Event::AutoscrollTick => {
                if let Some(pane) = app.dragging {
                    app.terminal_mut(pane).autoscroll_tick();
                }
                tui.set_autoscroll(app.autoscrolling());
            }
            Event::AppData(data) => {
                app.record_receive();
                tui.set_no_response_deadline(None);
                app.app_terminal.process(&data);
            }
            Event::KernelData(data) => {
                app.record_receive();
                tui.set_no_response_deadline(None);
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
                                } else {
                                    app.record_send();
                                    update_no_response_deadline(&tui, &app);
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
                        KeyAction::SearchChanged => {
                            app.focused_terminal_mut().update_search_matches();
                        }
                        KeyAction::Copy(text) => copy(&mut app, &text),
                        KeyAction::OpenPager => match open_pager(&app, &mut tui).await {
                            Ok(p) => pager = Some(p),
                            Err(e) => {
                                tui.resume()?;
                                app.show_error(format!("Pager failed: {e}"));
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
                            } else {
                                app.record_send();
                                update_no_response_deadline(&tui, &app);
                            }
                        }
                    }
                }
                TerminalEvent::Mouse(mouse) => {
                    let layout = ui::UiLayout::compute(frame_size, app.layout);
                    if let KeyAction::Copy(text) = handle_mouse(&mut app, mouse, &layout) {
                        copy(&mut app, &text);
                    }
                    tui.set_autoscroll(app.autoscrolling());
                }
                TerminalEvent::FocusGained | TerminalEvent::FocusLost => {}
                TerminalEvent::Error(err) => {
                    tui.exit()?;
                    bail!(err);
                }
            },
        }

        if pager.is_none() {
            tui.draw(|frame| ui::render(frame, &mut app))?;
        }
    }

    Ok(())
}

/// A pager showing an exported pane, and the temp file it reads.
struct Pager {
    child: tokio::process::Child,
    path: std::path::PathBuf,
}

/// Export the focused pane and open it in `$PAGER` (default `less -R`).
/// The TUI is suspended; the caller must resume it if this fails.
async fn open_pager(app: &App, tui: &mut Tui) -> Result<Pager> {
    let name = match app.focus {
        Focus::App => "app",
        Focus::Kernel => "kernel",
    };
    let path = std::env::temp_dir().join(format!("srsly-{}-{name}.txt", std::process::id()));
    std::fs::write(&path, app.focused_terminal().export()?)?;

    tui.suspend().await?;
    let child = tokio::process::Command::new("sh")
        .args(["-c", r#"${PAGER:-less -R} "$1""#, "sh"])
        .arg(&path)
        .spawn()?;
    Ok(Pager { child, path })
}

/// Wait for the pager to exit. Pends forever when no pager is open.
async fn wait_pager(pager: &mut Option<Pager>) -> std::io::Result<std::process::ExitStatus> {
    let Some(pager) = pager else {
        return std::future::pending().await;
    };
    loop {
        tokio::select! {
            status = pager.child.wait() => return status,
            // The pager handles Ctrl-C itself; don't let it kill srsly
            _ = tokio::signal::ctrl_c() => {}
        }
    }
}

fn copy(app: &mut App, text: &str) {
    match clipboard::copy(text) {
        Ok(()) => app.show_info(format!("Copied {} lines", text.lines().count())),
        Err(e) => app.show_error(format!("Copy failed: {e}")),
    }
}
