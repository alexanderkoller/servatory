use std::{
    io::{Read, stderr},
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use health_stick_host::{EventHandler, write_host_message};
use health_stick_mock_host::{ConsoleShutdown, made_up_health};
use health_stick_protocol::{
    FrameDecoder, HostMessage, MAX_FRAME_LEN, Sequence, UnixSeconds, decode_device,
};
use serialport::{SerialPortInfo, SerialPortType};

const ESPRESSIF_VID: u16 = 0x303a;
const USB_SERIAL_JTAG_PID: u16 = 0x1001;

#[derive(Debug, Parser)]
#[command(about = "Run portable made-up Proxmox data against an M5Stick S3 display")]
struct Args {
    /// Serial device. If omitted, a single connected ESP32-S3 is detected automatically.
    #[arg(long)]
    device: Option<PathBuf>,

    /// Interval between mock health snapshots.
    #[arg(long, default_value_t = 5)]
    interval_seconds: u64,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let device = resolve_device(args.device)?;
    eprintln!("using {}", device.display());
    let mut port = serialport::new(device.to_string_lossy(), 115_200)
        .timeout(Duration::from_millis(100))
        .open()
        .with_context(|| format!("opening {}", device.display()))?;

    let interval = Duration::from_secs(args.interval_seconds.max(1));
    let mut next_update = Instant::now();
    let mut sequence = Sequence::ZERO;
    let mut decoder = FrameDecoder::<MAX_FRAME_LEN>::new();
    let mut handler = EventHandler::new(ConsoleShutdown::new(stderr()), true);
    let mut input = [0_u8; 64];

    loop {
        if Instant::now() >= next_update {
            let unix_seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock is before the Unix epoch")?
                .as_secs();
            write_host_message(
                HostMessage::HealthSnapshot {
                    sequence,
                    unix_seconds: UnixSeconds::new(unix_seconds),
                    snapshot: made_up_health(),
                },
                &mut port,
            )?;
            eprintln!("sent mock health snapshot #{sequence}");
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
                        Ok(message) => {
                            if handler.handle(&message, &mut port)? {
                                return Ok(());
                            }
                        }
                        Err(error) => eprintln!("discarding malformed device message: {error}"),
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error).context("reading USB display"),
        }
    }
}

fn resolve_device(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(device) = explicit {
        return Ok(device);
    }

    let ports = serialport::available_ports().context("enumerating serial devices")?;
    let matches: Vec<_> = ports.iter().filter(|port| is_stick(port)).collect();
    match matches.as_slice() {
        [port] => Ok(PathBuf::from(&port.port_name)),
        [] => bail!("no ESP32-S3 USB Serial/JTAG device found; pass --device <PATH>"),
        _ => Err(anyhow!(
            "multiple ESP32-S3 devices found ({}); pass --device <PATH>",
            matches
                .iter()
                .map(|port| port.port_name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn is_stick(port: &SerialPortInfo) -> bool {
    matches!(
        &port.port_type,
        SerialPortType::UsbPort(info)
            if info.vid == ESPRESSIF_VID && info.pid == USB_SERIAL_JTAG_PID
    )
}
