use std::{
    io::Read,
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use clap::Parser;
use servatory_host::{EventHandler, SystemdShutdown, write_host_message};
use servatory_protocol::{
    DeviceMessage, FrameDecoder, HealthLevel, HealthReport, HostMessage, MAX_FRAME_LEN,
    PROTOCOL_VERSION, ProtocolError, Sequence, SoftwareVersion, UnixSeconds, decode_device,
};

mod config;
mod health;

use config::{Config, DEFAULT_CONFIG_PATH};
use health::HealthCollector;

#[derive(Debug, Parser)]
#[command(about = "Update an M5Stick S3 display over USB")]
struct Args {
    /// YAML configuration file.
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,

    /// Validate the configuration and exit.
    #[arg(long)]
    check_config: bool,
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<()> {
    let args = Args::parse();
    let config = Config::load(&args.config)?;
    if args.check_config {
        println!("{}: configuration is valid", args.config.display());
        return Ok(());
    }
    let interval = config.host.update_interval;
    let reconnect_delay = config.connection.usb_serial.reconnect_interval;
    let device = &config.connection.usb_serial.device;
    let mut display_config = config.display_config(0)?;
    let mut next_update = Instant::now();
    let mut sequence = Sequence::ZERO;
    let mut decoder = FrameDecoder::<MAX_FRAME_LEN>::new();
    let mut handler = EventHandler::new(SystemdShutdown, config.actions.shutdown.enabled);
    let mut input = [0_u8; 64];
    let filesystem_paths: Vec<String> = config
        .sources
        .filesystems
        .iter()
        .map(|item| item.path.clone())
        .collect();
    let mut health = HealthCollector::new(
        config.sources.ups.endpoint.clone(),
        config.sources.ups.failures_before_unavailable,
        config.sources.smart.devices.clone(),
        filesystem_paths,
        Some(&config.sources.internet),
        config.sources.proxmox.backup.task_history_limit,
    );
    let mut last_health_status = None;
    let mut port = None;
    let mut waiting_logged = false;
    let mut manifest_pending = true;
    let mut handshake_established = false;
    let mut next_hello = Instant::now();
    let mut handshake_started = Instant::now();
    let mut handshake_timeout_logged = false;
    let mut incompatible_version = None;

    loop {
        if port.is_none() {
            match serialport::new(device.to_string_lossy(), 115_200)
                .timeout(Duration::from_millis(100))
                .open()
            {
                Ok(opened_port) => {
                    eprintln!("opened {}", device.display());
                    port = Some(opened_port);
                    decoder = FrameDecoder::new();
                    handler = EventHandler::new(SystemdShutdown, config.actions.shutdown.enabled);
                    next_update = Instant::now();
                    manifest_pending = true;
                    handshake_established = false;
                    next_hello = Instant::now();
                    handshake_started = Instant::now();
                    handshake_timeout_logged = false;
                    incompatible_version = None;
                    waiting_logged = false;
                }
                Err(error) => {
                    if !waiting_logged {
                        eprintln!("waiting for display at {}: {error}", device.display());
                        waiting_logged = true;
                    }
                    thread::sleep(reconnect_delay);
                    continue;
                }
            }
        }

        let connected_port = port.as_mut().expect("port was opened above");
        let mut connection_error = None;

        if !handshake_established && Instant::now() >= next_hello {
            if let Err(error) = write_host_message(
                HostMessage::Hello {
                    daemon_version: SoftwareVersion::new(env!("SERVATORY_BUILD_VERSION")),
                },
                connected_port,
            ) {
                connection_error = Some(error.context("writing protocol hello"));
            } else {
                next_hello = Instant::now() + Duration::from_secs(1);
            }
        }
        if !handshake_established
            && !handshake_timeout_logged
            && handshake_started.elapsed() >= Duration::from_secs(5)
        {
            eprintln!(
                "no compatible display handshake after 5s; firmware may use another protocol version"
            );
            handshake_timeout_logged = true;
        }

        if handshake_established && manifest_pending {
            if let Err(error) = write_host_message(
                HostMessage::DisplayConfig(display_config.clone()),
                connected_port,
            ) {
                connection_error = Some(error.context("writing display configuration"));
            } else {
                manifest_pending = false;
            }
        }

        if handshake_established && connection_error.is_none() && Instant::now() >= next_update {
            let unix_seconds = current_unix_seconds()?;
            let mut snapshot = health.collect();
            let next_display_config = config.display_config(snapshot.guests.guests().len())?;
            if next_display_config != display_config {
                if let Err(error) = write_host_message(
                    HostMessage::DisplayConfig(next_display_config.clone()),
                    connected_port,
                ) {
                    connection_error = Some(error.context("updating display configuration"));
                } else {
                    display_config = next_display_config;
                }
            }
            let status = config.evaluate_health(&snapshot);
            snapshot.set_health(status.clone());
            log_health_status_change(&mut last_health_status, status);
            if connection_error.is_none()
                && let Err(error) = write_host_message(
                    HostMessage::HealthSnapshot {
                        sequence,
                        unix_seconds: UnixSeconds::new(unix_seconds),
                        snapshot,
                    },
                    connected_port,
                )
            {
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
                            Ok(DeviceMessage::Hello { firmware_version }) => {
                                if handshake_established {
                                    // A fresh Hello on an open serial port means the stick
                                    // restarted without requiring a USB disconnect.
                                    manifest_pending = true;
                                    next_update = Instant::now();
                                } else {
                                    eprintln!(
                                        "display firmware {} protocol v{PROTOCOL_VERSION} handshake established",
                                        firmware_version.as_str()
                                    );
                                    handshake_established = true;
                                    manifest_pending = true;
                                    next_update = Instant::now();
                                }
                            }
                            Ok(DeviceMessage::Ready) => {
                                if !handshake_established {
                                    handshake_established = true;
                                    manifest_pending = true;
                                    next_update = Instant::now();
                                }
                                if let Err(error) =
                                    handler.handle(&DeviceMessage::Ready, connected_port)
                                {
                                    connection_error =
                                        Some(error.context("handling display ready message"));
                                    break;
                                }
                            }
                            Ok(message) if handshake_established => {
                                match handler.handle(&message, connected_port) {
                                    Ok(true) => return Ok(()),
                                    Ok(false) => {}
                                    Err(error) => {
                                        connection_error =
                                            Some(error.context("handling USB display message"));
                                        break;
                                    }
                                }
                            }
                            Ok(_) => {}
                            Err(ProtocolError::UnsupportedVersion { received }) => {
                                if incompatible_version != Some(received) {
                                    eprintln!(
                                        "incompatible display protocol v{received}; daemon requires v{PROTOCOL_VERSION}"
                                    );
                                    incompatible_version = Some(received);
                                }
                            }
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
                device.display()
            );
            port = None;
            decoder = FrameDecoder::new();
            waiting_logged = true;
            thread::sleep(reconnect_delay);
        }
    }
}

fn current_unix_seconds() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
}

fn log_health_status_change(previous: &mut Option<HealthReport>, current: HealthReport) {
    if changed_health_status(previous.clone(), current.clone()).is_none() {
        return;
    }
    match current.level {
        HealthLevel::Healthy if previous.is_some() => {
            eprintln!("health status: HEALTHY (recovered)");
        }
        HealthLevel::Healthy => {}
        HealthLevel::Warning | HealthLevel::Critical => {
            eprintln!("health status: {:?}: {}", current.level, current.message());
        }
    }
    *previous = Some(current);
}

fn changed_health_status(
    previous: Option<HealthReport>,
    current: HealthReport,
) -> Option<HealthReport> {
    previous
        .is_none_or(|previous| {
            previous.level != current.level || previous.message() != current.message()
        })
        .then_some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_initial_problems_changes_and_recovery_once() {
        let warning = HealthReport::new(HealthLevel::Warning, "CPU 85%");
        assert_eq!(
            changed_health_status(None, warning.clone()),
            Some(warning.clone())
        );
        assert_eq!(
            changed_health_status(
                Some(warning.clone()),
                HealthReport::new(HealthLevel::Warning, "CPU 99%")
            ),
            Some(HealthReport::new(HealthLevel::Warning, "CPU 99%"))
        );

        let critical = HealthReport::new(HealthLevel::Critical, "PING FAILED");
        assert_eq!(
            changed_health_status(Some(warning), critical.clone()),
            Some(critical.clone())
        );
        assert_eq!(
            changed_health_status(Some(critical), HealthReport::healthy()),
            Some(HealthReport::healthy())
        );
        assert_eq!(
            changed_health_status(Some(HealthReport::healthy()), HealthReport::healthy()),
            None
        );
    }
}
