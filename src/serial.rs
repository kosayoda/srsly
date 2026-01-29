use color_eyre::Result;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_serial::{SerialPortBuilderExt, SerialStream};

/// Events from the serial port.
#[derive(Debug)]
pub enum SerialEvent {
    Data(Vec<u8>),
    Error(std::io::Error),
}

/// Serial port configuration.
#[derive(Debug, Clone)]
pub struct SerialConfig {
    pub port: String,
    pub baud_rate: u32,
}

impl SerialConfig {
    pub fn new(port: impl Into<String>, baud_rate: u32) -> Self {
        Self {
            port: port.into(),
            baud_rate,
        }
    }
}

impl Default for SerialConfig {
    fn default() -> Self {
        Self {
            port: "/dev/ttyUSB0".into(),
            baud_rate: 115200,
        }
    }
}

/// A keypress "entered" over a serial port
pub type SerialKey = smallvec::SmallVec<[u8; 5]>;

/// Handle for writing to the serial port.
#[derive(Debug)]
pub struct SerialWriter {
    tx: mpsc::Sender<SerialKey>,
}

impl SerialWriter {
    /// Send some bytes to the serial port.
    pub async fn send_key(&self, data: SerialKey) -> Result<(), mpsc::error::SendError<SerialKey>> {
        self.tx.send(data).await
    }
}

/// Open a serial port and return channels for reading/writing.
///
/// Returns:
/// - A receiver for incoming data/errors
/// - A writer handle for sending data
///
/// The serial port is managed by background tasks that handle the actual I/O.
pub fn connect(config: &SerialConfig) -> Result<(mpsc::Receiver<SerialEvent>, SerialWriter)> {
    let port = tokio_serial::new(&config.port, config.baud_rate).open_native_async()?;

    let (event_tx, event_rx) = mpsc::channel(1024);
    let (write_tx, write_rx) = mpsc::channel(256);

    let (reader, writer) = tokio::io::split(port);

    tokio::spawn(read_task(reader, event_tx));
    tokio::spawn(write_task(writer, write_rx));

    Ok((event_rx, SerialWriter { tx: write_tx }))
}

/// Background task that reads raw data from the serial port.
async fn read_task(mut reader: tokio::io::ReadHalf<SerialStream>, tx: mpsc::Sender<SerialEvent>) {
    let mut buf = [0u8; 4096];

    loop {
        match reader.read(&mut buf).await {
            // EOF - port closed
            Ok(0) => {
                break;
            }
            Ok(n) => {
                if tx
                    .send(SerialEvent::Data(buf[0..n].to_vec()))
                    .await
                    .is_err()
                {
                    // Receiver dropped
                    break;
                }
            }
            Err(e) => {
                let _ = tx.send(SerialEvent::Error(e)).await;
                break;
            }
        }
    }
}

/// Background task that writes to the serial port.
async fn write_task(
    mut writer: tokio::io::WriteHalf<SerialStream>,
    mut rx: mpsc::Receiver<SerialKey>,
) {
    while let Some(data) = rx.recv().await {
        if let Err(e) = writer.write_all(&data).await {
            tracing::error!("Serial write error: {}", e);
            break;
        }

        if let Err(e) = writer.flush().await {
            tracing::error!("Serial flush error: {}", e);
            break;
        }
    }
}
