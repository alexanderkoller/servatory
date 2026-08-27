use std::{
    io::Read,
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use clap::Parser;
use s3_display_host::{EventHandler, SystemdShutdown, write_host_message};
use s3_display_protocol::{
    FrameDecoder, HostMessage, MAX_FRAME_LEN, Sequence, UnixSeconds, decode_device,
};

mod health;

use health::HealthCollector;

#[derive(Debug, Parser)]
#[command(about = "Update an M5Stick S3 display over USB")]
struct Args {
    /// Serial device created by the ESP32-S3 USB Serial/JTAG interface.
    #[arg(long, default_value = "/dev/m5stick-s3")]
    device: PathBuf,

    /// Interval between host updates.
    #[arg(long, default_value_t = 5)]
    interval_seconds: u64,

    /// Permit a long press on the device to invoke `systemctl poweroff`.
    #[arg(long)]
    allow_shutdown: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let interval = Duration::from_secs(args.interval_seconds.max(1));
    let reconnect_delay = Duration::from_secs(1);
    let mut next_update = Instant::now();
    let mut sequence = Sequence::ZERO;
    let mut decoder = FrameDecoder::<MAX_FRAME_LEN>::new();
    let mut handler = EventHandler::new(SystemdShutdown, args.allow_shutdown);
    let mut input = [0_u8; 64];
    let mut health = HealthCollector::default();
    let mut port = None;
    let mut waiting_logged = false;

    loop {
        if port.is_none() {
            match serialport::new(args.device.to_string_lossy(), 115_200)
                .timeout(Duration::from_millis(100))
                .open()
            {
                Ok(opened_port) => {
                    eprintln!("opened {}", args.device.display());
                    port = Some(opened_port);
                    decoder = FrameDecoder::new();
                    handler = EventHandler::new(SystemdShutdown, args.allow_shutdown);
                    next_update = Instant::now();
                    waiting_logged = false;
                }
                Err(error) => {
                    if !waiting_logged {
                        eprintln!("waiting for display at {}: {error}", args.device.display());
                        waiting_logged = true;
                    }
                    thread::sleep(reconnect_delay);
                    continue;
                }
            }
        }

        let connected_port = port.as_mut().expect("port was opened above");
        let mut connection_error = None;

        if Instant::now() >= next_update {
            let unix_seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock is before the Unix epoch")?
                .as_secs();
            if let Err(error) = write_host_message(
                HostMessage::HealthSnapshot {
                    sequence,
                    unix_seconds: UnixSeconds::new(unix_seconds),
                    snapshot: health.collect(),
                },
                connected_port,
            ) {
                connection_error = Some(error.context("writing USB display"));
            } else {
                sequence = sequence.wrapping_next();
                next_update = Instant::now() + interval;
            }
        }

        if connection_error.is_none() {
            match connected_port.read(&mut input) {
                Ok(count) => {
                    for &byte in &input[..count] {
                        let Some(frame) = decoder.push(byte) else {
                            continue;
                        };
                        match frame.and_then(decode_device) {
                            Ok(message) => match handler.handle(message, connected_port) {
                                Ok(true) => return Ok(()),
                                Ok(false) => {}
                                Err(error) => {
                                    connection_error =
                                        Some(error.context("handling USB display message"));
                                    break;
                                }
                            },
                            Err(error) => {
                                eprintln!("discarding malformed device message: {error}");
                            }
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
                Err(error) => {
                    connection_error =
                        Some(anyhow::Error::new(error).context("reading USB display"));
                }
            }
        }

        if let Some(error) = connection_error {
            eprintln!(
                "display disconnected from {}: {error:#}; reconnecting",
                args.device.display()
            );
            port = None;
            decoder = FrameDecoder::new();
            waiting_logged = true;
            thread::sleep(reconnect_delay);
        }
    }
}
