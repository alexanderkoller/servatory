use std::{
    io::Read,
    path::PathBuf,
    process,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use clap::Parser;
use servatory_host::{EventHandler, SystemdShutdown, write_host_message};
use servatory_protocol::{
    DeviceMessage, FrameDecoder, HandshakeNonce, HealthLevel, HealthReport, HostMessage,
    MAX_DEVICE_FRAME_LEN, PROTOCOL_VERSION, ProtocolError, SoftwareVersion, decode_device,
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
    let mut network_config = config.network_config(0)?;
    let mut configured_guest_pages = guest_page_count(0);
    let mut next_update = Instant::now();
    let mut decoder = FrameDecoder::<MAX_DEVICE_FRAME_LEN>::new();
    let mut handler = EventHandler::new(SystemdShutdown, config.actions.shutdown.enabled);
    let mut input = [0_u8; 64];
    let filesystem_paths: Vec<String> = config
        .sources
        .filesystems
        .iter()
        .map(|item| item.path.clone())
        .collect();
    let health = HealthCollector::new(
        config.sources.ups.endpoint.clone(),
        config.sources.ups.failures_before_unavailable,
        config.sources.smart.devices.clone(),
        filesystem_paths,
        Some(&config.sources.internet),
        config.sources.proxmox.backup.task_history_limit,
    );
    let latest_health = spawn_health_collector(health, interval)?;
    let mut last_health_status = None;
    let mut port = None;
    let mut waiting_logged = false;
    let mut manifest_pending = true;
    let mut handshake_established = false;
    let mut next_hello = Instant::now();
    let mut handshake_started = Instant::now();
    let mut handshake_timeout_logged = false;
    let mut incompatible_version = None;
    let mut input_synchronized = false;
    let mut handshake_nonce = new_handshake_nonce();

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
                    input_synchronized = false;
                    handshake_nonce = new_handshake_nonce();
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
                    session: handshake_nonce,
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
            } else if let Err(error) = write_host_message(
                HostMessage::NetworkConfig(network_config.clone()),
                connected_port,
            ) {
                connection_error = Some(error.context("writing network configuration"));
            } else {
                manifest_pending = false;
            }
        }

        if handshake_established
            && connection_error.is_none()
            && Instant::now() >= next_update
            && let Some(mut snapshot) = take_latest_health(&latest_health)
        {
            let guest_count = snapshot.guests.guests().len();
            let next_guest_pages = guest_page_count(guest_count);
            if next_guest_pages != configured_guest_pages {
                let next_display_config = config.display_config(guest_count)?;
                let next_network_config = config.network_config(guest_count)?;
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
                if connection_error.is_none() && next_network_config != network_config {
                    if let Err(error) = write_host_message(
                        HostMessage::NetworkConfig(next_network_config.clone()),
                        connected_port,
                    ) {
                        connection_error = Some(error.context("updating network configuration"));
                    } else {
                        network_config = next_network_config;
                    }
                }
                if connection_error.is_none() {
                    configured_guest_pages = next_guest_pages;
                }
            }
            let (status, incidents) = config.evaluate_health(&snapshot);
            snapshot.set_incidents(status.clone(), incidents);
            log_health_status_change(&mut last_health_status, status);
            if connection_error.is_none() {
                match write_host_message(HostMessage::HealthSnapshot(snapshot), connected_port) {
                    Ok(()) => next_update = Instant::now() + interval,
                    Err(error) => {
                        connection_error = Some(error.context("writing USB display"));
                    }
                }
            }
        }

        if connection_error.is_none() {
            match connected_port.read(&mut input) {
                Ok(count) => {
                    for &byte in &input[..count] {
                        // The port may open in the middle of a frame emitted
                        // before this process acquired it. Synchronize at the
                        // next delimiter; both peers repeat Hello until the
                        // handshake completes.
                        if !input_synchronized {
                            if byte == 0 {
                                input_synchronized = true;
                                decoder = FrameDecoder::new();
                            }
                            continue;
                        }
                        let Some(frame) = decoder.push(byte) else {
                            continue;
                        };
                        match frame.and_then(decode_device) {
                            Ok(DeviceMessage::Hello {
                                firmware_version,
                                acknowledged_session,
                            }) => {
                                if acknowledged_session == Some(handshake_nonce) {
                                    if handshake_established {
                                        continue;
                                    }
                                    eprintln!(
                                        "display firmware {} protocol v{PROTOCOL_VERSION} handshake established",
                                        firmware_version.as_str()
                                    );
                                    handshake_established = true;
                                    manifest_pending = true;
                                    next_update = Instant::now();
                                } else {
                                    // An unacknowledged Hello is emitted at
                                    // Stick startup. A mismatched nonce can be
                                    // an acknowledgement left over from an old
                                    // daemon process. In both cases, issue the
                                    // current challenge until it is echoed.
                                    if handshake_established {
                                        eprintln!(
                                            "display restarted; establishing a new protocol handshake"
                                        );
                                        handshake_established = false;
                                        handshake_started = Instant::now();
                                        handshake_timeout_logged = false;
                                    }
                                    next_hello = Instant::now();
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

fn spawn_health_collector(
    mut collector: HealthCollector,
    interval: Duration,
) -> std::io::Result<Arc<Mutex<Option<servatory_protocol::HealthSnapshot>>>> {
    let latest = Arc::new(Mutex::new(None));
    let worker_latest = Arc::clone(&latest);
    thread::Builder::new()
        .name("health-collector".into())
        .spawn(move || {
            loop {
                let snapshot = collector.collect();
                let Ok(mut slot) = worker_latest.lock() else {
                    return;
                };
                *slot = Some(snapshot);
                drop(slot);
                thread::sleep(interval);
            }
        })?;
    Ok(latest)
}

fn take_latest_health(
    latest: &Mutex<Option<servatory_protocol::HealthSnapshot>>,
) -> Option<servatory_protocol::HealthSnapshot> {
    latest.lock().ok()?.take()
}

fn guest_page_count(guest_count: usize) -> usize {
    guest_count.max(1).div_ceil(4)
}

fn new_handshake_nonce() -> HandshakeNonce {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let low = u64::try_from(now & u128::from(u64::MAX)).unwrap_or(0);
    let high = u64::try_from(now >> 64).unwrap_or(0);
    let folded_time = low ^ high;
    HandshakeNonce::new(folded_time ^ (u64::from(process::id()) << 32))
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

    #[test]
    fn guest_manifests_change_only_at_page_boundaries() {
        assert_eq!(guest_page_count(0), 1);
        assert_eq!(guest_page_count(1), 1);
        assert_eq!(guest_page_count(4), 1);
        assert_eq!(guest_page_count(5), 2);
        assert_eq!(guest_page_count(8), 2);
        assert_eq!(guest_page_count(9), 3);
    }
}
