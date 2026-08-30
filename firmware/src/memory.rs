use allocator_api2::{boxed::Box, vec::Vec};
use esp_alloc::{EspHeap, HeapRegion, MemoryCapability};
use esp_hal::{
    peripherals::PSRAM,
    psram::{FlashFreq, Psram, PsramConfig, PsramMode, PsramSize, SpiRamFreq},
};

pub const RECLAIMED_HEAP_BYTES: usize = 64 * 1024;
pub const PRIMARY_HEAP_BYTES: usize = 48 * 1024;
pub const INTERNAL_HEAP_BYTES: usize = RECLAIMED_HEAP_BYTES + PRIMARY_HEAP_BYTES;

/// A heap containing only external PSRAM. Keeping it separate prevents normal
/// allocations (especially atomics) from silently spilling out of SRAM.
static PSRAM_HEAP: EspHeap = EspHeap::empty();

pub type PsramBox<T> = Box<T, &'static EspHeap>;

#[allow(unsafe_code)]
pub fn initialize_psram(peripheral: PSRAM<'static>) -> Psram {
    let psram = Psram::new(
        peripheral,
        PsramConfig {
            mode: PsramMode::OctalSpi,
            size: PsramSize::Size(8 * 1024 * 1024),
            flash_frequency: FlashFreq::FlashFreq80m,
            ram_frequency: SpiRamFreq::Freq80m,
            ..PsramConfig::default()
        },
    );
    let (start, size) = psram.raw_parts();
    assert_eq!(size, 8 * 1024 * 1024, "StickS3 PSRAM was not mapped");

    // SAFETY: `Psram` maps this exclusive, fixed memory range for the life of
    // the program. This function is called exactly once during startup, before
    // any external allocation is made.
    unsafe {
        PSRAM_HEAP.add_region(HeapRegion::new(
            start,
            size,
            MemoryCapability::External.into(),
        ));
    }
    psram
}

pub fn zeroed_psram(len: usize) -> PsramBox<[u8]> {
    let mut bytes = Vec::with_capacity_in(len, &PSRAM_HEAP);
    bytes.resize(len, 0);
    bytes.into_boxed_slice()
}
