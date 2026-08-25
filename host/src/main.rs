use std::{
    io::Read,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use clap::Parser;
use s3_display_host::{EventHandler, SystemdShutdown, write_host_message};
use s3_display_protocol::{
    FrameDecoder, HostMessage, MAX_FRAME_LEN, Sequence, UnixSeconds, decode_device,
};

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
    let mut port = serialport::new(args.device.to_string_lossy(), 115_200)
        .timeout(Duration::from_millis(100))
        .open()
        .with_context(|| format!("opening {}", args.device.display()))?;

    let interval = Duration::from_secs(args.interval_seconds.max(1));
    let mut next_update = Instant::now();
    let mut sequence = Sequence::ZERO;
    let mut decoder = FrameDecoder::<MAX_FRAME_LEN>::new();
    let mut handler = EventHandler::new(SystemdShutdown, args.allow_shutdown);
    let mut input = [0_u8; 64];

    loop {
        if Instant::now() >= next_update {
            let unix_seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock is before the Unix epoch")?
                .as_secs();
            write_host_message(
                HostMessage::Update {
                    sequence,
                    unix_seconds: UnixSeconds::new(unix_seconds),
                },
                &mut port,
            )?;
            sequence = sequence.wrapping_next();
            next_update = Instant::now() + interval;
        }

        match port.read(&mut input) {
            Ok(count) => {
                for &byte in &input[..count] {
                    let Some(frame) = decoder.push(byte) else {
                        continue;
                    };
                    match frame.and_then(decode_device) {
                        Ok(message) if handler.handle(message, &mut port)? => return Ok(()),
                        Ok(_) => {}
                        Err(error) => eprintln!("discarding malformed device message: {error}"),
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error).context("reading USB display"),
        }
    }
}
