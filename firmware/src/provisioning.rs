use alloc::{string::String, vec, vec::Vec};

use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use esp_storage::FlashStorage;
use serde::{Deserialize, Serialize};
use servatory_protocol::NetworkConfig;

const MAGIC: [u8; 4] = *b"SVTY";
const STORAGE_VERSION: u8 = 1;
const MAX_RECORD_LEN: usize = 2_048;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Provisioning {
    pub ssid: String,
    pub password: String,
    pub hostname: String,
    pub ntfy_topic: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StoredSettings {
    pub provisioning: Provisioning,
    pub network: Option<NetworkConfig>,
}

impl Provisioning {
    pub fn is_valid(&self) -> bool {
        !self.ssid.is_empty()
            && self.ssid.len() <= 32
            && self.password.len() <= 63
            && !self.hostname.is_empty()
            && self.hostname.len() <= 32
            && self
                .hostname
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && !self.ntfy_topic.is_empty()
            && self.ntfy_topic.len() <= 128
            && self
                .ntfy_topic
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    }
}

pub struct Store<'d> {
    flash: FlashStorage<'d>,
    sector: u32,
}

impl<'d> Store<'d> {
    pub fn new(flash: esp_hal::peripherals::FLASH<'d>) -> Self {
        let flash = FlashStorage::new(flash);
        let sector = u32::try_from(flash.capacity() - FlashStorage::SECTOR_SIZE as usize)
            .expect("flash address fits u32");
        Self { flash, sector }
    }

    pub fn load(&mut self) -> Option<StoredSettings> {
        let mut header = [0_u8; 12];
        self.flash.read(self.sector, &mut header).ok()?;
        if header[..4] != MAGIC || header[4] != STORAGE_VERSION {
            return None;
        }
        let len = usize::from(u16::from_le_bytes([header[6], header[7]]));
        if len == 0 || len > MAX_RECORD_LEN {
            return None;
        }
        let expected_checksum = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
        let padded = len.next_multiple_of(4);
        let mut payload = vec![0_u8; padded];
        self.flash.read(self.sector + 12, &mut payload).ok()?;
        let payload = &payload[..len];
        if checksum(payload) != expected_checksum {
            return None;
        }
        let value: StoredSettings = postcard::from_bytes(payload).ok()?;
        value.provisioning.is_valid().then_some(value)
    }

    pub fn save_provisioning(&mut self, value: Provisioning) -> Result<(), ()> {
        self.save(&StoredSettings {
            provisioning: value,
            network: None,
        })
    }

    pub fn save_network(
        &mut self,
        provisioning: &Provisioning,
        network: NetworkConfig,
    ) -> Result<(), ()> {
        self.save(&StoredSettings {
            provisioning: provisioning.clone(),
            network: Some(network),
        })
    }

    fn save(&mut self, value: &StoredSettings) -> Result<(), ()> {
        if !value.provisioning.is_valid() {
            return Err(());
        }
        let payload: Vec<u8> = postcard::to_allocvec(value).map_err(|_| ())?;
        if payload.len() > MAX_RECORD_LEN {
            return Err(());
        }
        let padded = payload.len().next_multiple_of(4);
        let mut record = vec![0xff_u8; 12 + padded];
        record[..4].copy_from_slice(&MAGIC);
        record[4] = STORAGE_VERSION;
        record[6..8].copy_from_slice(&u16::try_from(payload.len()).map_err(|_| ())?.to_le_bytes());
        record[8..12].copy_from_slice(&checksum(&payload).to_le_bytes());
        record[12..12 + payload.len()].copy_from_slice(&payload);
        self.flash
            .erase(self.sector, self.sector + FlashStorage::SECTOR_SIZE)
            .map_err(|_| ())?;
        self.flash.write(self.sector, &record).map_err(|_| ())
    }
}

fn checksum(bytes: &[u8]) -> u32 {
    bytes.iter().fold(2_166_136_261, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(16_777_619)
    })
}
