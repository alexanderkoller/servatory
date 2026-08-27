use std::io::{self, Write};

use s3_display_host::Shutdown;
use s3_display_protocol::{
    BackupJobStatus, FilesystemUsage, GuestKind, GuestSnapshot, GuestStatus, GuestSummary,
    HealthSnapshot, InternetStatus,
};

pub struct ConsoleShutdown<W> {
    output: W,
}

impl<W> ConsoleShutdown<W> {
    pub const fn new(output: W) -> Self {
        Self { output }
    }

    pub fn into_inner(self) -> W {
        self.output
    }
}

impl<W: Write> Shutdown for ConsoleShutdown<W> {
    fn poweroff(&mut self) -> io::Result<()> {
        writeln!(
            self.output,
            "mock shutdown requested; no system shutdown was performed"
        )
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
    )
    .expect("built-in mock host name fits the protocol")
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
        assert_eq!(health.guests.guests().len(), 5);
    }

    #[test]
    fn shutdown_only_writes_a_message() {
        let mut shutdown = ConsoleShutdown::new(Vec::new());
        shutdown.poweroff().unwrap();
        let output = String::from_utf8(shutdown.into_inner()).unwrap();
        assert!(output.contains("no system shutdown"));
    }
}
