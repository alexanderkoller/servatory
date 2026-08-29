use std::{io::Write, thread, time::Duration};

use s3_display_host::Shutdown;
use s3_display_protocol::{
    BackupJobStatus, FilesystemUsage, GuestKind, GuestSnapshot, GuestStatus, GuestSummary,
    HealthSnapshot, InternetStatus, SmartDeviceSummary, SmartSnapshot, SmartStatus, UpsSnapshot,
    UpsStatus,
};

pub struct ConsoleShutdown<W> {
    output: W,
    progress_delay: Duration,
}

impl<W> ConsoleShutdown<W> {
    pub const fn new(output: W) -> Self {
        Self {
            output,
            progress_delay: Duration::from_millis(500),
        }
    }

    pub fn into_inner(self) -> W {
        self.output
    }
}

impl<W: Write> Shutdown for ConsoleShutdown<W> {
    fn poweroff(&mut self, output: &mut impl Write) -> anyhow::Result<()> {
        use s3_display_host::write_host_message;
        use s3_display_protocol::{HostMessage, ShutdownPhase};

        for (phase, remaining) in [
            (ShutdownPhase::PreparingGuests, 4),
            (ShutdownPhase::StoppingGuests, 4),
            (ShutdownPhase::StoppingGuests, 3),
            (ShutdownPhase::StoppingGuests, 2),
            (ShutdownPhase::StoppingGuests, 1),
            (ShutdownPhase::GuestsStopped, 0),
            (ShutdownPhase::PoweringOff, 0),
        ] {
            write_host_message(
                HostMessage::ShutdownProgress {
                    phase,
                    guests_total: 4,
                    guests_remaining: remaining,
                },
                output,
            )?;
            output.flush()?;
            thread::sleep(self.progress_delay);
        }
        writeln!(
            self.output,
            "mock shutdown requested; no system shutdown was performed"
        )?;
        Ok(())
    }
}

/// Returns deterministic fake Proxmox data for display and protocol testing.
#[must_use]
pub fn made_up_guests() -> GuestSnapshot {
    let guests = [
        guest(
            100,
            "atlas",
            GuestKind::VirtualMachine,
            GuestStatus::Running,
            23,
            3_104,
            8_192,
        ),
        guest(
            101,
            "db-primary",
            GuestKind::VirtualMachine,
            GuestStatus::Running,
            61,
            12_480,
            16_384,
        ),
        guest(
            102,
            "paperless",
            GuestKind::Container,
            GuestStatus::Running,
            8,
            768,
            2_048,
        ),
        guest(
            103,
            "old-backup",
            GuestKind::VirtualMachine,
            GuestStatus::Stopped,
            0,
            0,
            4_096,
        ),
        guest(
            104,
            "home-assistant",
            GuestKind::Container,
            GuestStatus::Running,
            17,
            1_536,
            4_096,
        ),
    ];
    GuestSnapshot::from_slice(&guests)
}

/// Returns deterministic host data matching the landscape Split View mockup.
///
/// # Panics
///
/// Panics only if the built-in host name exceeds the protocol's fixed limit.
#[must_use]
pub fn made_up_health() -> HealthSnapshot {
    HealthSnapshot::new(
        "pve-01",
        18 * 24 * 60 * 60 + 4 * 60 * 60,
        23,
        18_688,
        32_768,
        4,
        82,
        FilesystemUsage::new(6, 85 * 1_024),
        FilesystemUsage::new(33, 6_186_598),
        FilesystemUsage::new(60, 3_670_016),
        BackupJobStatus::Healthy,
        Some(6 * 60 * 60),
        true,
        2_500,
        "enp3s0",
        InternetStatus::Reachable,
        Some(0),
        [10, 0, 0, 12],
        made_up_guests(),
        UpsSnapshot {
            status: UpsStatus::Online,
            battery_percent: Some(100),
            load_percent: Some(15),
            runtime_seconds: Some(2_160),
            estimated_watts: Some(102),
            stale: false,
        },
        SmartSnapshot::from_slice(&[
            smart("ROOT", SmartStatus::Healthy, Some(38)),
            smart("HDD", SmartStatus::Healthy, Some(31)),
            smart("BACKUP", SmartStatus::Healthy, Some(30)),
            smart("SDD", SmartStatus::Healthy, Some(34)),
            smart("SDE", SmartStatus::Sleeping, None),
        ]),
    )
    .expect("built-in mock host name fits the protocol")
}

fn smart(label: &str, status: SmartStatus, temperature_celsius: Option<i8>) -> SmartDeviceSummary {
    SmartDeviceSummary::new(label, status, temperature_celsius)
        .expect("built-in SMART labels fit the protocol")
}

fn guest(
    vmid: u32,
    name: &str,
    kind: GuestKind,
    status: GuestStatus,
    cpu_percent: u8,
    memory_used_mib: u32,
    memory_total_mib: u32,
) -> GuestSummary {
    GuestSummary::new(
        vmid,
        name,
        kind,
        status,
        cpu_percent,
        memory_used_mib,
        memory_total_mib,
    )
    .expect("built-in mock guest names fit the protocol")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_data_contains_running_and_stopped_guests() {
        let snapshot = made_up_guests();
        assert!(snapshot.guests().len() > 1);
        assert!(
            snapshot
                .guests()
                .iter()
                .any(|guest| guest.status == GuestStatus::Running)
        );
        assert!(
            snapshot
                .guests()
                .iter()
                .any(|guest| guest.status == GuestStatus::Stopped)
        );
    }

    #[test]
    fn mock_health_matches_the_ui_draft() {
        let health = made_up_health();
        assert_eq!(health.host_name(), "pve-01");
        assert_eq!(health.cpu_percent, 23);
        assert!(health.network_up);
        assert_eq!(health.network_mbps, 2_500);
        assert_eq!(health.network_interface(), "enp3s0");
        assert_eq!(health.internet_status, InternetStatus::Reachable);
        assert_eq!(health.backup_job_status, BackupJobStatus::Healthy);
        assert_eq!(health.last_successful_backup_age_seconds, Some(21_600));
        assert_eq!(health.ups.estimated_watts, Some(102));
        assert_eq!(health.smart.devices().len(), 5);
        assert_eq!(health.guests.guests().len(), 5);
    }

    #[test]
    fn shutdown_only_writes_a_message() {
        let mut shutdown = ConsoleShutdown::new(Vec::new());
        shutdown.progress_delay = Duration::ZERO;
        shutdown.poweroff(&mut Vec::new()).unwrap();
        let output = String::from_utf8(shutdown.into_inner()).unwrap();
        assert!(output.contains("no system shutdown"));
    }
}
