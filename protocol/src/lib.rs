#![no_std]

extern crate alloc;

use alloc::{string::String, vec, vec::Vec};
use core::fmt;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const PROTOCOL_VERSION: u8 = 10;
const FRAME_MAGIC: [u8; 2] = [0xa5, 0x5a];
/// Maximum host-to-device COBS frame, including its trailing zero delimiter.
pub const MAX_HOST_FRAME_LEN: usize = 4 * 1024;
/// Maximum device-to-host COBS frame, including its trailing zero delimiter.
pub const MAX_DEVICE_FRAME_LEN: usize = 128;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum HealthLevel {
    Healthy,
    Warning,
    Critical,
}

impl HealthLevel {
    #[must_use]
    pub const fn priority(self) -> u8 {
        match self {
            Self::Healthy => 0,
            Self::Warning => 1,
            Self::Critical => 2,
        }
    }
}

pub const MAX_RULE_ID_LEN: usize = 48;

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct RuleId(String);

impl RuleId {
    /// Creates a validated identifier for a configured health rule.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is empty, too long, or contains
    /// characters other than ASCII letters, digits, `_`, and `-`.
    pub fn new(value: &str) -> Result<Self, RuleIdError> {
        if value.is_empty()
            || value.len() > MAX_RULE_ID_LEN
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err(RuleIdError);
        }
        Ok(Self(String::from(value)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuleIdError;

impl fmt::Display for RuleIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "rule ID must contain 1 to {MAX_RULE_ID_LEN} ASCII letters, digits, '_' or '-'"
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum StickIncident {
    HostOffline,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum IncidentId {
    Rule(RuleId),
    Stick(StickIncident),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Incident {
    pub id: IncidentId,
    pub level: HealthLevel,
    message: String,
}

impl Incident {
    #[must_use]
    pub fn new(id: IncidentId, level: HealthLevel, message: &str) -> Self {
        Self {
            id,
            level,
            message: String::from(message),
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SoftwareVersion(String);

impl SoftwareVersion {
    #[must_use]
    pub fn new(value: &str) -> Self {
        Self(String::from(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HealthReport {
    pub level: HealthLevel,
    message: String,
}

impl HealthReport {
    #[must_use]
    pub fn healthy() -> Self {
        Self::new(HealthLevel::Healthy, "HEALTHY")
    }

    #[must_use]
    pub fn new(level: HealthLevel, message: &str) -> Self {
        Self {
            level,
            message: String::from(message),
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DisplayPage {
    Overview,
    Resources,
    Storage {
        filesystems_left: bool,
        filesystem_indices: Vec<u32>,
        smart_indices: Vec<u32>,
    },
    PowerNetwork {
        ups_left: bool,
    },
    Guests {
        offset: u32,
        limit: u32,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DisplayLabel {
    value: String,
}

impl DisplayLabel {
    #[must_use]
    pub fn new(value: &str) -> Self {
        Self {
            value: String::from(value),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DisplayView {
    pub title: DisplayLabel,
    pub page: DisplayPage,
}

impl DisplayView {
    #[must_use]
    pub fn new(title: DisplayLabel, page: DisplayPage) -> Self {
        Self { title, page }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DisplayConfig {
    pub shutdown_hold_ms: u16,
    pub shutdown_animation_delay_ms: u16,
    pub daemon_version: SoftwareVersion,
    pub filesystem_labels: Vec<DisplayLabel>,
    pages: Vec<DisplayView>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HttpConfig {
    pub enabled: bool,
    pub port: u16,
    hostname: String,
    pages: Vec<DisplayView>,
}

impl HttpConfig {
    #[must_use]
    pub fn new(enabled: bool, hostname: &str, port: u16, pages: Vec<DisplayView>) -> Self {
        Self {
            enabled,
            port,
            hostname: String::from(hostname),
            pages,
        }
    }

    #[must_use]
    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    #[must_use]
    pub fn pages(&self) -> &[DisplayView] {
        &self.pages
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum NotificationPriority {
    Default,
    High,
    Urgent,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NtfyConfig {
    pub enabled: bool,
    pub severities: NotificationSeverities,
    pub notify_recovery: bool,
    pub warning_priority: NotificationPriority,
    pub critical_priority: NotificationPriority,
    pub recovery_priority: NotificationPriority,
    pub repeat_critical_seconds: Option<u32>,
    server: String,
    click_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NotificationSeverities {
    pub warning: bool,
    pub critical: bool,
}

impl NtfyConfig {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        enabled: bool,
        server: &str,
        severities: NotificationSeverities,
        notify_recovery: bool,
        warning_priority: NotificationPriority,
        critical_priority: NotificationPriority,
        recovery_priority: NotificationPriority,
        repeat_critical_seconds: Option<u32>,
        click_url: Option<&str>,
    ) -> Self {
        Self {
            enabled,
            severities,
            notify_recovery,
            warning_priority,
            critical_priority,
            recovery_priority,
            repeat_critical_seconds,
            server: String::from(server),
            click_url: click_url.map(String::from),
        }
    }

    #[must_use]
    pub fn server(&self) -> &str {
        &self.server
    }

    #[must_use]
    pub fn click_url(&self) -> Option<&str> {
        self.click_url.as_deref()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NetworkConfig {
    pub http: HttpConfig,
    pub ntfy: NtfyConfig,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            shutdown_hold_ms: 3_000,
            shutdown_animation_delay_ms: 200,
            daemon_version: SoftwareVersion::new("unknown"),
            filesystem_labels: Vec::new(),
            pages: vec![DisplayView::new(
                DisplayLabel::new("OVERVIEW"),
                DisplayPage::Overview,
            )],
        }
    }
}

impl DisplayConfig {
    /// Creates a validated device display manifest.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty or oversized page list, a zero hold time,
    /// or an animation delay that does not leave time for the animation.
    pub fn new(
        shutdown_hold_ms: u16,
        shutdown_animation_delay_ms: u16,
        filesystem_labels: Vec<DisplayLabel>,
        pages: Vec<DisplayView>,
    ) -> Result<Self, DisplayConfigError> {
        if pages.is_empty()
            || shutdown_hold_ms == 0
            || shutdown_animation_delay_ms >= shutdown_hold_ms
        {
            return Err(DisplayConfigError);
        }
        Ok(Self {
            shutdown_hold_ms,
            shutdown_animation_delay_ms,
            daemon_version: SoftwareVersion::new("unknown"),
            filesystem_labels,
            pages,
        })
    }

    #[must_use]
    pub fn pages(&self) -> &[DisplayView] {
        &self.pages
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisplayConfigError;

impl fmt::Display for DisplayConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("display must have pages and animation delay must precede hold time")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[repr(transparent)]
pub struct HandshakeNonce(u64);

impl HandshakeNonce {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GuestKind {
    VirtualMachine,
    Container,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum GuestStatus {
    Running,
    Stopped,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum InternetStatus {
    Checking,
    Reachable,
    Missed,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum BackupJobStatus {
    Unknown,
    NoJob,
    Healthy,
    Running,
    Failed,
    Stale,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum UpsStatus {
    NotConfigured,
    Unknown,
    Online,
    OnBattery,
    LowBattery,
    Charging,
    Bypass,
    OutputOff,
    ReplaceBattery,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UpsSnapshot {
    pub status: UpsStatus,
    pub battery_percent: Option<u8>,
    pub load_percent: Option<u8>,
    pub runtime_seconds: Option<u32>,
    pub estimated_watts: Option<u16>,
    pub stale: bool,
}

impl UpsSnapshot {
    pub const NOT_CONFIGURED: Self = Self {
        status: UpsStatus::NotConfigured,
        battery_percent: None,
        load_percent: None,
        runtime_seconds: None,
        estimated_watts: None,
        stale: false,
    };
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmartStatus {
    Healthy,
    Warning,
    Failed,
    Sleeping,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmartDeviceSummary {
    pub status: SmartStatus,
    pub temperature_celsius: Option<i8>,
    label: String,
}

impl SmartDeviceSummary {
    #[must_use]
    pub fn new(label: &str, status: SmartStatus, temperature_celsius: Option<i8>) -> Self {
        Self {
            status,
            temperature_celsius,
            label: String::from(label),
        }
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmartSnapshot {
    devices: Vec<SmartDeviceSummary>,
}

impl SmartSnapshot {
    #[must_use]
    pub fn new(devices: Vec<SmartDeviceSummary>) -> Self {
        Self { devices }
    }

    #[must_use]
    pub fn devices(&self) -> &[SmartDeviceSummary] {
        &self.devices
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ShutdownPhase {
    PreparingGuests,
    StoppingGuests,
    GuestsStopped,
    PoweringOff,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ShutdownFailure {
    HostPoweroff,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FilesystemUsage {
    pub used_percent: u8,
    pub available_mib: u32,
    pub mounted: bool,
}

impl FilesystemUsage {
    pub const MISSING: Self = Self {
        used_percent: 0,
        available_mib: 0,
        mounted: false,
    };

    #[must_use]
    pub const fn new(used_percent: u8, available_mib: u32) -> Self {
        Self {
            used_percent: if used_percent > 100 {
                100
            } else {
                used_percent
            },
            available_mib,
            mounted: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GuestSummary {
    pub vmid: u32,
    pub kind: GuestKind,
    pub status: GuestStatus,
    pub cpu_percent: u8,
    pub memory_used_mib: u32,
    pub memory_total_mib: u32,
    name: String,
}

impl GuestSummary {
    #[must_use]
    pub fn new(
        vmid: u32,
        name: &str,
        kind: GuestKind,
        status: GuestStatus,
        cpu_percent: u8,
        memory_used_mib: u32,
        memory_total_mib: u32,
    ) -> Self {
        Self {
            vmid,
            kind,
            status,
            cpu_percent: cpu_percent.min(100),
            memory_used_mib,
            memory_total_mib,
            name: String::from(name),
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct GuestSnapshot {
    guests: Vec<GuestSummary>,
}

impl GuestSnapshot {
    #[must_use]
    pub fn new(guests: Vec<GuestSummary>) -> Self {
        Self { guests }
    }

    #[must_use]
    pub fn guests(&self) -> &[GuestSummary] {
        &self.guests
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HealthSnapshot {
    pub health: HealthReport,
    active_incidents: Vec<Incident>,
    pub uptime_seconds: u64,
    pub cpu_percent: u8,
    pub memory_used_mib: u32,
    pub memory_total_mib: u32,
    pub io_pressure_percent: u8,
    pub load_average_x100: u16,
    pub filesystems: Vec<FilesystemUsage>,
    pub backup_job_status: BackupJobStatus,
    pub last_successful_backup_age_seconds: Option<u32>,
    pub network_up: bool,
    pub network_mbps: u16,
    pub internet_status: InternetStatus,
    pub last_internet_success_age_seconds: Option<u32>,
    pub ipv4: [u8; 4],
    pub guests: GuestSnapshot,
    pub ups: UpsSnapshot,
    pub smart: SmartSnapshot,
    host_name: String,
    network_interface: String,
}

impl HealthSnapshot {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        host_name: &str,
        uptime_seconds: u64,
        cpu_percent: u8,
        memory_used_mib: u32,
        memory_total_mib: u32,
        io_pressure_percent: u8,
        load_average_x100: u16,
        filesystems: Vec<FilesystemUsage>,
        backup_job_status: BackupJobStatus,
        last_successful_backup_age_seconds: Option<u32>,
        network_up: bool,
        network_mbps: u16,
        network_interface: &str,
        internet_status: InternetStatus,
        last_internet_success_age_seconds: Option<u32>,
        ipv4: [u8; 4],
        guests: GuestSnapshot,
        ups: UpsSnapshot,
        smart: SmartSnapshot,
    ) -> Self {
        Self {
            health: HealthReport::healthy(),
            active_incidents: Vec::new(),
            uptime_seconds,
            cpu_percent: cpu_percent.min(100),
            memory_used_mib,
            memory_total_mib,
            io_pressure_percent: io_pressure_percent.min(100),
            load_average_x100,
            filesystems,
            backup_job_status,
            last_successful_backup_age_seconds,
            network_up,
            network_mbps,
            internet_status,
            last_internet_success_age_seconds,
            ipv4,
            guests,
            ups,
            smart,
            host_name: String::from(host_name),
            network_interface: String::from(network_interface),
        }
    }

    pub fn set_incidents(&mut self, health: HealthReport, active_incidents: Vec<Incident>) {
        self.health = health;
        self.active_incidents = active_incidents;
    }

    #[must_use]
    pub fn active_incidents(&self) -> &[Incident] {
        &self.active_incidents
    }

    #[must_use]
    pub fn host_name(&self) -> &str {
        &self.host_name
    }

    #[must_use]
    pub fn network_interface(&self) -> &str {
        &self.network_interface
    }
}

/// Host-to-device messages. Variants may only be appended within a protocol version.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum HostMessage {
    ShutdownAccepted,
    HealthSnapshot(HealthSnapshot),
    ShutdownProgress {
        phase: ShutdownPhase,
        guests_total: u16,
        guests_remaining: u16,
    },
    ShutdownFailed {
        reason: ShutdownFailure,
        guests_remaining: u16,
    },
    DisplayConfig(DisplayConfig),
    Hello {
        daemon_version: SoftwareVersion,
        session: HandshakeNonce,
    },
    NetworkConfig(NetworkConfig),
}

/// Device-to-host messages. Variants may only be appended within a protocol version.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DeviceMessage {
    ShutdownRequested,
    Hello {
        firmware_version: SoftwareVersion,
        acknowledged_session: Option<HandshakeNonce>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    EmptyFrame,
    FrameTooLong,
    Serialize,
    Deserialize,
    UnsupportedVersion { received: u8 },
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Serializes and COBS-frames one host-to-device message.
///
/// # Errors
///
/// Returns [`ProtocolError::Serialize`] if `output` is too small.
pub fn encode_host(message: HostMessage, output: &mut [u8]) -> Result<&mut [u8], ProtocolError> {
    encode(message, output)
}

/// Serializes and COBS-frames one device-to-host message.
///
/// # Errors
///
/// Returns [`ProtocolError::Serialize`] if `output` is too small.
pub fn encode_device(
    message: DeviceMessage,
    output: &mut [u8],
) -> Result<&mut [u8], ProtocolError> {
    encode(message, output)
}

/// Decodes one mutable COBS frame into a typed host-to-device message.
///
/// # Errors
///
/// Returns an error for malformed data or an unsupported protocol version.
pub fn decode_host(frame: &mut [u8]) -> Result<HostMessage, ProtocolError> {
    decode(frame)
}

/// Decodes one mutable COBS frame into a typed device-to-host message.
///
/// # Errors
///
/// Returns an error for malformed data or an unsupported protocol version.
pub fn decode_device(frame: &mut [u8]) -> Result<DeviceMessage, ProtocolError> {
    decode(frame)
}

fn encode<T: Serialize>(message: T, output: &mut [u8]) -> Result<&mut [u8], ProtocolError> {
    if output.len() < 3 {
        return Err(ProtocolError::Serialize);
    }
    output[..2].copy_from_slice(&FRAME_MAGIC);
    output[2] = PROTOCOL_VERSION;
    let payload = postcard::to_slice_cobs(&message, &mut output[3..])
        .map_err(|_| ProtocolError::Serialize)?;
    let len = payload.len() + 3;
    Ok(&mut output[..len])
}

fn decode<T: DeserializeOwned>(frame: &mut [u8]) -> Result<T, ProtocolError> {
    if frame.starts_with(&FRAME_MAGIC) {
        let (version, payload) = frame[2..]
            .split_first_mut()
            .ok_or(ProtocolError::EmptyFrame)?;
        if *version != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion { received: *version });
        }
        return postcard::from_bytes_cobs(payload).map_err(|_| ProtocolError::Deserialize);
    }

    // Protocols through v5 encoded the version inside the COBS body. Recover
    // only that leading byte for a useful upgrade diagnostic; the old message
    // schema is deliberately not decoded.
    let decoded_len = cobs::decode_in_place(frame).map_err(|_| ProtocolError::Deserialize)?;
    let received = *frame
        .get(..decoded_len)
        .and_then(|decoded| decoded.first())
        .ok_or(ProtocolError::Deserialize)?;
    Err(ProtocolError::UnsupportedVersion { received })
}

/// Extracts zero-delimited COBS frames from an arbitrary byte stream.
pub struct FrameDecoder<const N: usize> {
    bytes: [u8; N],
    len: usize,
    discarding: bool,
}

impl<const N: usize> FrameDecoder<N> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
            discarding: false,
        }
    }

    /// Adds one stream byte and returns a complete mutable frame at a delimiter.
    ///
    /// The returned slice borrows the decoder because postcard performs COBS
    /// decoding in place. Consume it before pushing another byte.
    pub fn push(&mut self, byte: u8) -> Option<Result<&mut [u8], ProtocolError>> {
        if byte == 0 {
            if self.discarding {
                self.discarding = false;
                self.len = 0;
                return Some(Err(ProtocolError::FrameTooLong));
            }
            if self.len == 0 {
                return Some(Err(ProtocolError::EmptyFrame));
            }
            self.bytes[self.len] = 0;
            let frame_len = self.len + 1;
            self.len = 0;
            return Some(Ok(&mut self.bytes[..frame_len]));
        }

        if self.discarding {
            return None;
        }
        // Reserve one byte so the delimiter can be included in the returned frame.
        if N == 0 || self.len >= N - 1 {
            self.discarding = true;
            self.len = 0;
            return None;
        }
        self.bytes[self.len] = byte;
        self.len += 1;
        None
    }
}

impl<const N: usize> Default for FrameDecoder<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    fn guest(name: &str) -> GuestSummary {
        GuestSummary::new(
            100,
            name,
            GuestKind::VirtualMachine,
            GuestStatus::Running,
            23,
            3_104,
            8_192,
        )
    }

    fn snapshot(guests: Vec<GuestSummary>) -> HealthSnapshot {
        HealthSnapshot::new(
            "pve-01",
            86_400,
            23,
            18_688,
            32_768,
            4,
            82,
            vec![
                FilesystemUsage::new(6, 85 * 1_024),
                FilesystemUsage::new(33, 6_186_598),
                FilesystemUsage::new(60, 3_670_016),
            ],
            BackupJobStatus::Healthy,
            Some(21_600),
            true,
            2_500,
            "enp3s0",
            InternetStatus::Reachable,
            Some(0),
            [10, 0, 0, 12],
            GuestSnapshot::new(guests),
            UpsSnapshot::NOT_CONFIGURED,
            SmartSnapshot::default(),
        )
    }

    #[test]
    fn messages_round_trip_as_typed_values() {
        let guest = guest("atlas");
        let host_messages = [
            HostMessage::ShutdownAccepted,
            HostMessage::HealthSnapshot(snapshot(vec![guest])),
            HostMessage::ShutdownProgress {
                phase: ShutdownPhase::StoppingGuests,
                guests_total: 5,
                guests_remaining: 3,
            },
            HostMessage::ShutdownFailed {
                reason: ShutdownFailure::HostPoweroff,
                guests_remaining: 2,
            },
            HostMessage::Hello {
                daemon_version: SoftwareVersion::new("0.1.0"),
                session: HandshakeNonce::new(0x1234),
            },
        ];
        for expected in host_messages {
            let mut storage = [0; MAX_HOST_FRAME_LEN];
            let frame = encode_host(expected.clone(), &mut storage).unwrap();
            assert_eq!(decode_host(frame), Ok(expected));
        }

        let device_messages = [
            DeviceMessage::ShutdownRequested,
            DeviceMessage::Hello {
                firmware_version: SoftwareVersion::new("0.1.0"),
                acknowledged_session: Some(HandshakeNonce::new(0x1234)),
            },
        ];
        for expected in device_messages {
            let mut storage = [0; 32];
            let frame = encode_device(expected.clone(), &mut storage).unwrap();
            assert_eq!(decode_device(frame), Ok(expected));
        }
    }

    #[test]
    fn rule_ids_are_validated_and_namespaced_from_stick_incidents() {
        let rule = RuleId::new("backup_failed").unwrap();
        assert_eq!(rule.as_str(), "backup_failed");
        assert!(RuleId::new("").is_err());
        assert!(RuleId::new("not a rule").is_err());
        assert_ne!(
            IncidentId::Rule(RuleId::new("host_offline").unwrap()),
            IncidentId::Stick(StickIncident::HostOffline)
        );
    }

    #[test]
    fn network_manifest_round_trips() {
        let manifest = NetworkConfig {
            http: HttpConfig::new(
                true,
                "servatory",
                80,
                DisplayConfig::default().pages().to_vec(),
            ),
            ntfy: NtfyConfig::new(
                true,
                "https://ntfy.sh",
                NotificationSeverities {
                    warning: true,
                    critical: true,
                },
                true,
                NotificationPriority::High,
                NotificationPriority::Urgent,
                NotificationPriority::Default,
                Some(3_600),
                Some("http://servatory.local/"),
            ),
        };
        let expected = HostMessage::NetworkConfig(manifest);
        let mut storage = [0; MAX_HOST_FRAME_LEN];
        let frame = encode_host(expected.clone(), &mut storage).unwrap();
        assert_eq!(decode_host(frame), Ok(expected));
    }

    #[test]
    fn frame_decoder_handles_fragmentation() {
        let expected = DeviceMessage::Hello {
            firmware_version: SoftwareVersion::new("0.2.0"),
            acknowledged_session: Some(HandshakeNonce::new(81)),
        };
        let mut encoded = [0; MAX_DEVICE_FRAME_LEN];
        let frame = encode_device(expected.clone(), &mut encoded).unwrap();
        let mut decoder = FrameDecoder::<32>::new();
        let mut received = None;
        for &byte in frame.iter() {
            if let Some(result) = decoder.push(byte) {
                received = Some(decode_device(result.unwrap()).unwrap());
            }
        }
        assert_eq!(received, Some(expected));
    }

    #[test]
    fn decoder_recovers_after_oversized_frame() {
        let mut decoder = FrameDecoder::<4>::new();
        for byte in [1, 1, 1, 1] {
            assert!(decoder.push(byte).is_none());
        }
        assert_eq!(decoder.push(0), Some(Err(ProtocolError::FrameTooLong)));

        assert!(decoder.push(1).is_none());
        assert!(decoder.push(1).is_none());
        assert!(decoder.push(0).is_some());
    }

    #[test]
    fn incompatible_version_is_rejected() {
        let mut storage = [0; 32];
        // Deliberately use an unrelated payload type. Version inspection must
        // happen before decoding the version-specific message schema.
        storage[..6].copy_from_slice(&[
            FRAME_MAGIC[0],
            FRAME_MAGIC[1],
            PROTOCOL_VERSION + 1,
            0xfe,
            0xed,
            0,
        ]);
        let frame = &mut storage[..6];
        assert_eq!(
            decode_device(frame),
            Err(ProtocolError::UnsupportedVersion {
                received: PROTOCOL_VERSION + 1
            })
        );
    }

    #[test]
    fn legacy_envelope_reports_its_actual_version() {
        #[derive(Serialize)]
        struct LegacyEnvelope {
            version: u8,
            message: u16,
        }

        let mut storage = [0; 32];
        let frame = postcard::to_slice_cobs(
            &LegacyEnvelope {
                version: 5,
                message: 0xfeed,
            },
            &mut storage,
        )
        .unwrap();
        assert_eq!(
            decode_device(frame),
            Err(ProtocolError::UnsupportedVersion { received: 5 })
        );
    }

    #[test]
    fn device_messages_fit_the_device_frame_budget() {
        let mut storage = [0; MAX_DEVICE_FRAME_LEN];
        let frame = encode_device(
            DeviceMessage::Hello {
                firmware_version: SoftwareVersion::new("0.2.0+g1234567890.dirty"),
                acknowledged_session: Some(HandshakeNonce::new(u64::MAX)),
            },
            &mut storage,
        )
        .unwrap();
        assert!(frame.len() <= MAX_DEVICE_FRAME_LEN);
    }

    #[test]
    fn snapshots_use_dynamic_names_and_counts() {
        let long_name = "guest-name-that-is-not-a-firmware-sized-array";
        let guests = vec![guest(long_name); 40];
        let value = snapshot(guests);
        assert_eq!(value.guests.guests().len(), 40);
        assert_eq!(value.guests.guests()[0].name(), long_name);

        let mut storage = [0; MAX_HOST_FRAME_LEN];
        let frame = encode_host(HostMessage::HealthSnapshot(value), &mut storage).unwrap();
        assert!(frame.len() <= MAX_HOST_FRAME_LEN);
    }

    #[test]
    fn frame_budget_is_the_only_collection_limit() {
        let mut storage = [0; 128];
        let oversized_for_this_transport =
            HostMessage::HealthSnapshot(snapshot(vec![guest("long-enough-name"); 40]));
        assert_eq!(
            encode_host(oversized_for_this_transport, &mut storage),
            Err(ProtocolError::Serialize)
        );
    }
}
