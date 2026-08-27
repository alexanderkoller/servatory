#![no_std]

use core::fmt;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const PROTOCOL_VERSION: u8 = 3;
/// Maximum COBS-encoded frame size, including its trailing zero delimiter.
pub const MAX_FRAME_LEN: usize = 512;
pub const MAX_GUESTS: usize = 8;
pub const MAX_GUEST_NAME_LEN: usize = 20;
pub const MAX_HOST_NAME_LEN: usize = 20;
pub const MAX_NETWORK_INTERFACE_LEN: usize = 15;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[repr(transparent)]
pub struct Sequence(u32);

impl Sequence {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn wrapping_next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

impl fmt::Display for Sequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[repr(transparent)]
pub struct UnixSeconds(u64);

impl UnixSeconds {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
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
pub enum ShutdownPhase {
    PreparingGuests,
    StoppingGuests,
    GuestsStopped,
    PoweringOff,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ShutdownFailure {
    GuestQuery,
    GuestShutdown,
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GuestSummary {
    pub vmid: u32,
    pub kind: GuestKind,
    pub status: GuestStatus,
    pub cpu_percent: u8,
    pub memory_used_mib: u32,
    pub memory_total_mib: u32,
    name_len: u8,
    name: [u8; MAX_GUEST_NAME_LEN],
}

impl GuestSummary {
    pub const EMPTY: Self = Self {
        vmid: 0,
        kind: GuestKind::VirtualMachine,
        status: GuestStatus::Stopped,
        cpu_percent: 0,
        memory_used_mib: 0,
        memory_total_mib: 0,
        name_len: 0,
        name: [0; MAX_GUEST_NAME_LEN],
    };

    /// Creates a bounded, allocation-free guest summary.
    ///
    /// # Errors
    ///
    /// Returns [`GuestNameError::TooLong`] when the UTF-8 name exceeds the wire limit.
    pub fn new(
        vmid: u32,
        name: &str,
        kind: GuestKind,
        status: GuestStatus,
        cpu_percent: u8,
        memory_used_mib: u32,
        memory_total_mib: u32,
    ) -> Result<Self, GuestNameError> {
        if name.len() > MAX_GUEST_NAME_LEN {
            return Err(GuestNameError::TooLong);
        }
        let mut bytes = [0; MAX_GUEST_NAME_LEN];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        Ok(Self {
            vmid,
            kind,
            status,
            cpu_percent: cpu_percent.min(100),
            memory_used_mib,
            memory_total_mib,
            name_len: u8::try_from(name.len()).unwrap_or(0),
            name: bytes,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        let len = usize::from(self.name_len).min(MAX_GUEST_NAME_LEN);
        core::str::from_utf8(&self.name[..len]).unwrap_or("?")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestNameError {
    TooLong,
}

impl fmt::Display for GuestNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "guest name exceeds {MAX_GUEST_NAME_LEN} bytes")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GuestSnapshot {
    len: u8,
    guests: [GuestSummary; MAX_GUESTS],
}

impl GuestSnapshot {
    pub const EMPTY: Self = Self {
        len: 0,
        guests: [GuestSummary::EMPTY; MAX_GUESTS],
    };

    /// Copies as many guests as fit in one protocol snapshot.
    #[must_use]
    pub fn from_slice(guests: &[GuestSummary]) -> Self {
        let len = guests.len().min(MAX_GUESTS);
        let mut snapshot = Self::EMPTY;
        snapshot.guests[..len].copy_from_slice(&guests[..len]);
        snapshot.len = u8::try_from(len).unwrap_or(0);
        snapshot
    }

    #[must_use]
    pub fn guests(&self) -> &[GuestSummary] {
        &self.guests[..usize::from(self.len).min(MAX_GUESTS)]
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HealthSnapshot {
    pub uptime_seconds: u64,
    pub cpu_percent: u8,
    pub memory_used_mib: u32,
    pub memory_total_mib: u32,
    pub io_pressure_percent: u8,
    pub load_average_x100: u16,
    pub root_storage: FilesystemUsage,
    pub hdd_storage: FilesystemUsage,
    pub backup_storage: FilesystemUsage,
    pub backup_job_status: BackupJobStatus,
    pub last_successful_backup_age_seconds: Option<u32>,
    pub network_up: bool,
    pub network_mbps: u16,
    pub internet_status: InternetStatus,
    pub last_internet_success_age_seconds: Option<u32>,
    pub ipv4: [u8; 4],
    pub guests: GuestSnapshot,
    host_name_len: u8,
    host_name: [u8; MAX_HOST_NAME_LEN],
    network_interface_len: u8,
    network_interface: [u8; MAX_NETWORK_INTERFACE_LEN],
}

impl HealthSnapshot {
    /// Creates a bounded, allocation-free host-health snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`HealthSnapshotError`] when either name exceeds its wire limit.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        host_name: &str,
        uptime_seconds: u64,
        cpu_percent: u8,
        memory_used_mib: u32,
        memory_total_mib: u32,
        io_pressure_percent: u8,
        load_average_x100: u16,
        root_storage: FilesystemUsage,
        hdd_storage: FilesystemUsage,
        backup_storage: FilesystemUsage,
        backup_job_status: BackupJobStatus,
        last_successful_backup_age_seconds: Option<u32>,
        network_up: bool,
        network_mbps: u16,
        network_interface: &str,
        internet_status: InternetStatus,
        last_internet_success_age_seconds: Option<u32>,
        ipv4: [u8; 4],
        guests: GuestSnapshot,
    ) -> Result<Self, HealthSnapshotError> {
        if host_name.len() > MAX_HOST_NAME_LEN {
            return Err(HealthSnapshotError::HostNameTooLong);
        }
        if network_interface.len() > MAX_NETWORK_INTERFACE_LEN {
            return Err(HealthSnapshotError::NetworkInterfaceTooLong);
        }
        let mut bytes = [0; MAX_HOST_NAME_LEN];
        bytes[..host_name.len()].copy_from_slice(host_name.as_bytes());
        let mut interface_bytes = [0; MAX_NETWORK_INTERFACE_LEN];
        interface_bytes[..network_interface.len()].copy_from_slice(network_interface.as_bytes());
        Ok(Self {
            uptime_seconds,
            cpu_percent: cpu_percent.min(100),
            memory_used_mib,
            memory_total_mib,
            io_pressure_percent: io_pressure_percent.min(100),
            load_average_x100,
            root_storage,
            hdd_storage,
            backup_storage,
            backup_job_status,
            last_successful_backup_age_seconds,
            network_up,
            network_mbps,
            internet_status,
            last_internet_success_age_seconds,
            ipv4,
            guests,
            host_name_len: u8::try_from(host_name.len()).unwrap_or(0),
            host_name: bytes,
            network_interface_len: u8::try_from(network_interface.len()).unwrap_or(0),
            network_interface: interface_bytes,
        })
    }

    #[must_use]
    pub fn host_name(&self) -> &str {
        let len = usize::from(self.host_name_len).min(MAX_HOST_NAME_LEN);
        core::str::from_utf8(&self.host_name[..len]).unwrap_or("?")
    }

    #[must_use]
    pub fn network_interface(&self) -> &str {
        let len = usize::from(self.network_interface_len).min(MAX_NETWORK_INTERFACE_LEN);
        core::str::from_utf8(&self.network_interface[..len]).unwrap_or("?")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HealthSnapshotError {
    HostNameTooLong,
    NetworkInterfaceTooLong,
}

impl fmt::Display for HealthSnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HostNameTooLong => write!(f, "host name exceeds {MAX_HOST_NAME_LEN} bytes"),
            Self::NetworkInterfaceTooLong => write!(
                f,
                "network interface name exceeds {MAX_NETWORK_INTERFACE_LEN} bytes"
            ),
        }
    }
}

/// Host-to-device messages. Variants may only be appended within a protocol version.
// The no_std firmware deliberately uses a fixed-size inline snapshot instead of allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum HostMessage {
    /// A content-neutral heartbeat. Screen data will be added after the UI is designed.
    Update {
        sequence: Sequence,
        unix_seconds: UnixSeconds,
    },
    ShutdownAccepted,
    GuestSnapshot {
        sequence: Sequence,
        unix_seconds: UnixSeconds,
        snapshot: GuestSnapshot,
    },
    HealthSnapshot {
        sequence: Sequence,
        unix_seconds: UnixSeconds,
        snapshot: HealthSnapshot,
    },
    ShutdownProgress {
        phase: ShutdownPhase,
        guests_total: u16,
        guests_remaining: u16,
    },
    ShutdownFailed {
        reason: ShutdownFailure,
        guests_remaining: u16,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ButtonAction {
    NextScreen,
    ShutdownRequested,
}

/// Device-to-host messages. Variants may only be appended within a protocol version.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DeviceMessage {
    Ready,
    Ack { sequence: Sequence },
    Button(ButtonAction),
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

#[derive(Deserialize, Serialize)]
struct Envelope<T> {
    version: u8,
    message: T,
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
    postcard::to_slice_cobs(
        &Envelope {
            version: PROTOCOL_VERSION,
            message,
        },
        output,
    )
    .map_err(|_| ProtocolError::Serialize)
}

fn decode<T: DeserializeOwned>(frame: &mut [u8]) -> Result<T, ProtocolError> {
    let envelope: Envelope<T> =
        postcard::from_bytes_cobs(frame).map_err(|_| ProtocolError::Deserialize)?;
    if envelope.version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion {
            received: envelope.version,
        });
    }
    Ok(envelope.message)
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

    #[test]
    fn messages_round_trip_as_typed_values() {
        let guest = GuestSummary::new(
            100,
            "atlas",
            GuestKind::VirtualMachine,
            GuestStatus::Running,
            23,
            3_104,
            8_192,
        )
        .unwrap();
        let host_messages = [
            HostMessage::Update {
                sequence: Sequence::new(42),
                unix_seconds: UnixSeconds::new(1_700_000_000),
            },
            HostMessage::ShutdownAccepted,
            HostMessage::GuestSnapshot {
                sequence: Sequence::new(43),
                unix_seconds: UnixSeconds::new(1_700_000_005),
                snapshot: GuestSnapshot::from_slice(&[guest]),
            },
            HostMessage::HealthSnapshot {
                sequence: Sequence::new(44),
                unix_seconds: UnixSeconds::new(1_700_000_010),
                snapshot: HealthSnapshot::new(
                    "pve-01",
                    86_400,
                    23,
                    18_688,
                    32_768,
                    4,
                    82,
                    FilesystemUsage::new(6, 85 * 1_024),
                    FilesystemUsage::new(33, 6_186_598),
                    FilesystemUsage::new(60, 3_670_016),
                    BackupJobStatus::Healthy,
                    Some(21_600),
                    true,
                    2_500,
                    "enp3s0",
                    InternetStatus::Reachable,
                    Some(0),
                    [10, 0, 0, 12],
                    GuestSnapshot::from_slice(&[guest]),
                )
                .unwrap(),
            },
            HostMessage::ShutdownProgress {
                phase: ShutdownPhase::StoppingGuests,
                guests_total: 5,
                guests_remaining: 3,
            },
            HostMessage::ShutdownFailed {
                reason: ShutdownFailure::GuestShutdown,
                guests_remaining: 2,
            },
        ];
        for expected in host_messages {
            let mut storage = [0; MAX_FRAME_LEN];
            let frame = encode_host(expected, &mut storage).unwrap();
            assert_eq!(decode_host(frame), Ok(expected));
        }

        let device_messages = [
            DeviceMessage::Ready,
            DeviceMessage::Ack {
                sequence: Sequence::new(42),
            },
            DeviceMessage::Button(ButtonAction::NextScreen),
            DeviceMessage::Button(ButtonAction::ShutdownRequested),
        ];
        for expected in device_messages {
            let mut storage = [0; 32];
            let frame = encode_device(expected, &mut storage).unwrap();
            assert_eq!(decode_device(frame), Ok(expected));
        }
    }

    #[test]
    fn frame_decoder_handles_fragmentation() {
        let expected = DeviceMessage::Ack {
            sequence: Sequence::new(81),
        };
        let mut encoded = [0; 32];
        let frame = encode_device(expected, &mut encoded).unwrap();
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
        let frame = postcard::to_slice_cobs(
            &Envelope {
                version: PROTOCOL_VERSION + 1,
                message: DeviceMessage::Ready,
            },
            &mut storage,
        )
        .unwrap();
        assert_eq!(
            decode_device(frame),
            Err(ProtocolError::UnsupportedVersion {
                received: PROTOCOL_VERSION + 1
            })
        );
    }

    #[test]
    fn current_messages_fit_one_usb_packet() {
        let mut storage = [0; 64];
        let frame = encode_host(
            HostMessage::Update {
                sequence: Sequence::new(u32::MAX),
                unix_seconds: UnixSeconds::new(u64::MAX),
            },
            &mut storage,
        )
        .unwrap();
        assert!(frame.len() <= 64);
    }

    #[test]
    fn guest_names_and_snapshot_counts_are_bounded() {
        assert_eq!(
            GuestSummary::new(
                100,
                "a-name-that-is-longer-than-twenty-bytes",
                GuestKind::Container,
                GuestStatus::Running,
                150,
                1,
                2,
            ),
            Err(GuestNameError::TooLong)
        );

        let guests = [GuestSummary::EMPTY; MAX_GUESTS + 1];
        assert_eq!(
            GuestSnapshot::from_slice(&guests).guests().len(),
            MAX_GUESTS
        );
    }

    #[test]
    fn network_interface_names_are_bounded() {
        assert_eq!(
            HealthSnapshot::new(
                "pve-01",
                0,
                0,
                0,
                0,
                0,
                0,
                FilesystemUsage::MISSING,
                FilesystemUsage::MISSING,
                FilesystemUsage::MISSING,
                BackupJobStatus::Unknown,
                None,
                true,
                2_500,
                "interface-name-too-long",
                InternetStatus::Checking,
                None,
                [0; 4],
                GuestSnapshot::EMPTY,
            ),
            Err(HealthSnapshotError::NetworkInterfaceTooLong)
        );
    }

    #[test]
    fn largest_guest_snapshot_fits_the_frame_buffer() {
        let guest = GuestSummary::new(
            u32::MAX,
            "12345678901234567890",
            GuestKind::VirtualMachine,
            GuestStatus::Running,
            100,
            u32::MAX,
            u32::MAX,
        )
        .unwrap();
        let mut storage = [0; MAX_FRAME_LEN];
        let frame = encode_host(
            HostMessage::GuestSnapshot {
                sequence: Sequence::new(u32::MAX),
                unix_seconds: UnixSeconds::new(u64::MAX),
                snapshot: GuestSnapshot::from_slice(&[guest; MAX_GUESTS]),
            },
            &mut storage,
        )
        .unwrap();
        assert!(frame.len() <= MAX_FRAME_LEN);
    }

    #[test]
    fn largest_health_snapshot_fits_the_frame_buffer() {
        let guest = GuestSummary::new(
            u32::MAX,
            "12345678901234567890",
            GuestKind::VirtualMachine,
            GuestStatus::Running,
            100,
            u32::MAX,
            u32::MAX,
        )
        .unwrap();
        let snapshot = HealthSnapshot::new(
            "12345678901234567890",
            u64::MAX,
            100,
            u32::MAX,
            u32::MAX,
            100,
            u16::MAX,
            FilesystemUsage::new(100, u32::MAX),
            FilesystemUsage::new(100, u32::MAX),
            FilesystemUsage::new(100, u32::MAX),
            BackupJobStatus::Failed,
            Some(u32::MAX),
            true,
            u16::MAX,
            "123456789012345",
            InternetStatus::Failed,
            Some(u32::MAX),
            [255; 4],
            GuestSnapshot::from_slice(&[guest; MAX_GUESTS]),
        )
        .unwrap();
        let mut storage = [0; MAX_FRAME_LEN];
        let frame = encode_host(
            HostMessage::HealthSnapshot {
                sequence: Sequence::new(u32::MAX),
                unix_seconds: UnixSeconds::new(u64::MAX),
                snapshot,
            },
            &mut storage,
        )
        .unwrap();
        assert!(frame.len() <= MAX_FRAME_LEN);
    }
}
