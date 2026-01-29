use clap::{Parser, Subcommand};
use color_eyre::Result;
use color_eyre::eyre::bail;
use dialoguer::Select;
use dialoguer::theme::ColorfulTheme;
use owo_colors::OwoColorize;

use srsly::app::App;
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
    let mut app = App::new(rows, cols);

    tui.enter(serial_rx)?;

    // Helper to update the TUI's no-response deadline from app state
    let update_no_response_deadline = |tui: &Tui, app: &App| {
        if let Some(deadline) = app.no_response_deadline() {
            tui.set_no_response_deadline(Some(tokio::time::Instant::from_std(deadline)));
        } else {
            tui.set_no_response_deadline(None);
        }
    };

    // Main event loop
    while let Some(event) = tui.next().await {
        match event {
            Event::Init | Event::NoResponseTimeout => {
                // NoResponseTimeout just triggers a redraw (handled below)
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
