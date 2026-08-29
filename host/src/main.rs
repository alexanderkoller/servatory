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
    FrameDecoder, HealthStatus, HostMessage, MAX_FRAME_LEN, MAX_SMART_DEVICES, MAX_SMART_LABEL_LEN,
    Sequence, UnixSeconds, decode_device,
};

mod health;

use health::{HealthCollector, SmartDeviceConfig};

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

    /// Read-only NUT endpoint to query with upsc (for example eaton@localhost).
    #[arg(long)]
    ups: Option<String>,

    /// SMART row in LABEL=/dev/path form; may be repeated up to five times.
    #[arg(long = "smart-device", value_parser = parse_smart_device)]
    smart_devices: Vec<SmartDeviceConfig>,
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
    let mut health = configured_health(args.ups, args.smart_devices)?;
    let mut last_health_status = None;
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
            let unix_seconds = current_unix_seconds()?;
            let snapshot = health.collect();
            let status = snapshot.health_status();
            log_health_status_change(&mut last_health_status, status);
            if let Err(error) = write_host_message(
                HostMessage::HealthSnapshot {
                    sequence,
                    unix_seconds: UnixSeconds::new(unix_seconds),
                    snapshot,
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

fn configured_health(
    ups: Option<String>,
    smart_devices: Vec<SmartDeviceConfig>,
) -> Result<HealthCollector> {
    if smart_devices.len() > MAX_SMART_DEVICES {
        anyhow::bail!("at most {MAX_SMART_DEVICES} --smart-device values are supported");
    }
    Ok(HealthCollector::new(ups, smart_devices))
}

fn parse_smart_device(value: &str) -> Result<SmartDeviceConfig, String> {
    let (label, path) = value
        .split_once('=')
        .ok_or_else(|| "expected LABEL=/dev/path".to_owned())?;
    if label.is_empty() || label.len() > MAX_SMART_LABEL_LEN || !label.is_ascii() {
        return Err(format!(
            "SMART label must be 1-{MAX_SMART_LABEL_LEN} ASCII characters"
        ));
    }
    if !path.starts_with("/dev/") {
        return Err("SMART device path must start with /dev/".to_owned());
    }
    Ok(SmartDeviceConfig {
        label: label.to_ascii_uppercase(),
        path: path.to_owned(),
    })
}

fn current_unix_seconds() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
}

fn log_health_status_change(previous: &mut Option<HealthStatus>, current: HealthStatus) {
    if changed_health_status(*previous, current).is_none() {
        return;
    }
    match current {
        HealthStatus::Healthy if previous.is_some() => {
            eprintln!("health status: HEALTHY (recovered)");
        }
        HealthStatus::Healthy => {}
        HealthStatus::Warning(_) | HealthStatus::Critical(_) => {
            eprintln!("health status: {current}");
        }
    }
    *previous = Some(current);
}

fn changed_health_status(
    previous: Option<HealthStatus>,
    current: HealthStatus,
) -> Option<HealthStatus> {
    previous
        .is_none_or(|previous| !current.same_condition(previous))
        .then_some(current)
}

#[cfg(test)]
mod tests {
    use s3_display_protocol::{CriticalCause, WarningCause};

    use super::*;

    #[test]
    fn logs_initial_problems_changes_and_recovery_once() {
        let warning = HealthStatus::Warning(WarningCause::Cpu(85));
        assert_eq!(changed_health_status(None, warning), Some(warning));
        assert_eq!(
            changed_health_status(Some(warning), HealthStatus::Warning(WarningCause::Cpu(99))),
            None
        );

        let critical = HealthStatus::Critical(CriticalCause::PingFailed);
        assert_eq!(
            changed_health_status(Some(warning), critical),
            Some(critical)
        );
        assert_eq!(
            changed_health_status(Some(critical), HealthStatus::Healthy),
            Some(HealthStatus::Healthy)
        );
        assert_eq!(
            changed_health_status(Some(HealthStatus::Healthy), HealthStatus::Healthy),
            None
        );
    }

    #[test]
    fn parses_named_smart_devices() {
        let device = parse_smart_device("backup=/dev/sdc").unwrap();
        assert_eq!(device.label, "BACKUP");
        assert_eq!(device.path, "/dev/sdc");
        assert!(parse_smart_device("TOO-LONG=/dev/sda").is_err());
        assert!(parse_smart_device("ROOT=sda").is_err());
    }
}
