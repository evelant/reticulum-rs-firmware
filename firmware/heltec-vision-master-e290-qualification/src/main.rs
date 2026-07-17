//! RF-inert identity, flash-header, and PSRAM qualification for the E290.
//!
//! Physical flash identity is deliberately collected by `espflash board-info`
//! before this image is flashed. `esp-storage` can report only the capacity
//! encoded in the boot image header, so this binary treats that value as a
//! consistency observation rather than independent flash detection.

#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "forgetting esp-hal peripherals can leave hardware active"
)]
#![deny(clippy::large_stack_frames)]
#![deny(unsafe_op_in_unsafe_fn)]

use allocator_api2::vec::Vec;
use embedded_storage::nor_flash::ReadNorFlash;
use esp_alloc::{ExternalMemory, InternalMemory, MemoryCapability};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    gpio::{Level, Output, OutputConfig},
    psram::{FlashFreq, Psram, PsramConfig, PsramMode, PsramSize, SpiRamFreq},
};
use esp_storage::FlashStorage;
use log::info;

#[cfg(debug_assertions)]
compile_error!("the E290 PSRAM qualification image must be built with --release");

const MINIMUM_PSRAM_BYTES: usize = 8 * 1024 * 1024;
const MAXIMUM_PSRAM_BYTES: usize = 32 * 1024 * 1024;
const FLASH_BYTES_8_MIB: usize = 8 * 1024 * 1024;
const FLASH_BYTES_16_MIB: usize = 16 * 1024 * 1024;
const INTERNAL_HEAP_BYTES: usize = 64 * 1024;
const INTERNAL_ALLOCATION_PROBE_BYTES: usize = 4 * 1024;
const EXTERNAL_ALLOCATION_PROBE_BYTES: usize = 256 * 1024;
const SERIAL_CAPTURE_GRACE_MS: u32 = 5_000;
const HEARTBEAT_PERIOD_MS: u32 = 30_000;

esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "the one-shot qualification entry point owns target resources"
)]
#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // The E290 module owns DIO2 RF switching and DIO3 TCXO control internally.
    // Holding reset low and NSS high contains the complete radio without
    // borrowing Tracker-specific FEM assumptions.
    let _sx1262_reset = Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default());
    let _sx1262_nss = Output::new(peripherals.GPIO8, Level::High, OutputConfig::default());

    // `espflash monitor` resets the target before opening its serial reader.
    // Keep every peripheral inert and delay the one-shot qualification report
    // long enough for that reader to attach, so identity, security, flash, and
    // PSRAM evidence are captured in one complete boot record.
    let delay = Delay::new();
    delay.delay_millis(SERIAL_CAPTURE_GRACE_MS);
    esp_println::logger::init_logger_from_env();

    let base_mac = esp_hal::efuse::base_mac_address();
    let revision = esp_hal::efuse::chip_revision();
    let reset_reason = esp_hal::system::reset_reason();
    let flash_encryption = esp_hal::efuse::flash_encryption();
    let secure_boot = esp_hal::efuse::read_field_le::<u8>(esp_hal::efuse::SECURE_BOOT_EN) != 0;
    let efuse_flash_type = esp_hal::efuse::read_field_le::<u8>(esp_hal::efuse::FLASH_TYPE);
    let efuse_flash_capacity = esp_hal::efuse::read_field_le::<u8>(esp_hal::efuse::FLASH_CAP);
    let efuse_psram_capacity_low = esp_hal::efuse::read_field_le::<u8>(esp_hal::efuse::PSRAM_CAP);
    let efuse_psram_capacity_bit_3 =
        esp_hal::efuse::read_field_le::<u8>(esp_hal::efuse::PSRAM_CAP_3);
    if flash_encryption || secure_boot {
        panic!(
            "e290-qualification stage=security status=FAIL flash_encryption={} secure_boot={}",
            flash_encryption, secure_boot
        );
    }
    info!("e290-qualification stage=security status=PASS flash_encryption=false secure_boot=false");
    info!(
        "e290-qualification stage=boot status=PASS base_mac={} chip_revision={}.{} reset_reason={reset_reason:?} rf_inert=true",
        base_mac, revision.major, revision.minor
    );
    info!(
        "e290-qualification stage=efuse status=OBSERVED flash_encryption=false secure_boot=false flash_type_raw={} flash_capacity_raw={} psram_capacity_low_raw={} psram_capacity_bit_3_raw={}",
        efuse_flash_type,
        efuse_flash_capacity,
        efuse_psram_capacity_low,
        efuse_psram_capacity_bit_3
    );
    info!("e290-qualification stage=rf-interlock status=PASS sx1262_reset=low sx1262_nss=high");

    let flash = FlashStorage::new(peripherals.FLASH);
    let image_header_flash_bytes = ReadNorFlash::capacity(&flash);
    if !matches!(
        image_header_flash_bytes,
        FLASH_BYTES_8_MIB | FLASH_BYTES_16_MIB
    ) {
        panic!(
            "e290-qualification stage=flash-header status=FAIL configured_capacity_bytes={} expected=8388608|16777216",
            image_header_flash_bytes
        );
    }
    info!(
        "e290-qualification stage=flash-header status=PASS configured_capacity_bytes={} independent_physical_detection=false flash_mutation=false",
        image_header_flash_bytes
    );

    let psram_config = PsramConfig {
        mode: PsramMode::Auto,
        size: PsramSize::AutoDetect,
        core_clock: None,
        flash_frequency: FlashFreq::FlashFreq40m,
        ram_frequency: SpiRamFreq::Freq40m,
    };
    let psram = Psram::new(peripherals.PSRAM, psram_config);
    let (psram_start, psram_bytes) = psram.raw_parts();
    let psram_start_address = psram_start as usize;
    let psram_end_address = psram_start_address
        .checked_add(psram_bytes)
        .unwrap_or_else(|| panic!("e290-qualification PSRAM mapped-range overflow"));
    info!(
        "e290-qualification stage=psram-map status=OBSERVED mode_request=auto flash_frequency_mhz=40 ram_frequency_mhz=40 start=0x{:08x} end=0x{:08x} bytes={}",
        psram_start_address, psram_end_address, psram_bytes
    );
    if !(MINIMUM_PSRAM_BYTES..=MAXIMUM_PSRAM_BYTES).contains(&psram_bytes) {
        panic!(
            "e290-qualification stage=psram-map status=FAIL expected_bytes={}..={} actual_bytes={}",
            MINIMUM_PSRAM_BYTES, MAXIMUM_PSRAM_BYTES, psram_bytes
        );
    }
    if !psram_start_address.is_multiple_of(core::mem::align_of::<u32>())
        || !psram_bytes.is_multiple_of(core::mem::size_of::<u32>())
    {
        panic!(
            "e290-qualification stage=psram-map status=FAIL reason=unaligned start=0x{:08x} bytes={}",
            psram_start_address, psram_bytes
        );
    }

    // SAFETY: `Psram::raw_parts` identifies the mapped, exclusively owned
    // PSRAM region. No allocator has received this region yet, the size and
    // alignment were checked above, and this function does not escape a
    // reference or pointer into it.
    unsafe { qualify_entire_psram(psram_start.cast::<u32>(), psram_bytes / 4) };
    info!(
        "e290-qualification stage=psram-pattern status=PASS bytes={} passes=address,inverted,zeroed",
        psram_bytes
    );

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: INTERNAL_HEAP_BYTES);
    esp_alloc::psram_allocator!(&psram);
    log_heap("registered");

    let mut internal = Vec::with_capacity_in(INTERNAL_ALLOCATION_PROBE_BYTES, InternalMemory);
    internal.resize(INTERNAL_ALLOCATION_PROBE_BYTES, 0x3c_u8);
    let internal_start = internal.as_ptr() as usize;
    let internal_end = internal_start
        .checked_add(internal.len())
        .unwrap_or_else(|| panic!("e290-qualification internal allocation range overflow"));
    if ranges_overlap(
        internal_start,
        internal_end,
        psram_start_address,
        psram_end_address,
    ) {
        panic!(
            "e290-qualification stage=allocator status=FAIL reason=internal-in-psram start=0x{:08x} end=0x{:08x}",
            internal_start, internal_end
        );
    }

    let mut external = Vec::with_capacity_in(EXTERNAL_ALLOCATION_PROBE_BYTES, ExternalMemory);
    external.resize(EXTERNAL_ALLOCATION_PROBE_BYTES, 0xa5_u8);
    let external_start = external.as_ptr() as usize;
    let external_end = external_start
        .checked_add(external.len())
        .unwrap_or_else(|| panic!("e290-qualification external allocation range overflow"));
    if external_start < psram_start_address || external_end > psram_end_address {
        panic!(
            "e290-qualification stage=allocator status=FAIL reason=external-outside-psram start=0x{:08x} end=0x{:08x} psram_start=0x{:08x} psram_end=0x{:08x}",
            external_start, external_end, psram_start_address, psram_end_address
        );
    }
    if internal.iter().any(|byte| *byte != 0x3c) || external.iter().any(|byte| *byte != 0xa5) {
        panic!("e290-qualification stage=allocator status=FAIL reason=payload-corrupt");
    }
    info!(
        "e290-qualification stage=allocator status=PASS internal_start=0x{:08x} internal_bytes={} external_start=0x{:08x} external_bytes={}",
        internal_start,
        internal.len(),
        external_start,
        external.len()
    );
    log_heap("allocated");
    drop(external);
    drop(internal);
    log_heap("released");

    info!(
        "e290-qualification stage=complete status=PASS flash_mutation=false radio_initialized=false display_initialized=false"
    );
    loop {
        delay.delay_millis(HEARTBEAT_PERIOD_MS);
        info!("e290-qualification stage=heartbeat status=PASS rf_inert=true");
    }
}

unsafe fn qualify_entire_psram(start: *mut u32, words: usize) {
    for index in 0..words {
        let expected = address_pattern(index);
        // SAFETY: the caller establishes a valid exclusive region of `words`
        // aligned u32 values; `index` stays within that region.
        unsafe { start.add(index).write_volatile(expected) };
    }
    verify_psram_pass(start, words, false);

    for index in 0..words {
        let expected = !address_pattern(index);
        // SAFETY: same region and bounds proof as the first write pass.
        unsafe { start.add(index).write_volatile(expected) };
    }
    verify_psram_pass(start, words, true);

    for index in 0..words {
        // SAFETY: same region and bounds proof; zeroing leaves no test payload
        // behind before the memory is registered with the allocator.
        unsafe { start.add(index).write_volatile(0) };
    }
}

fn verify_psram_pass(start: *mut u32, words: usize, inverted: bool) {
    for index in 0..words {
        let pattern = address_pattern(index);
        let expected = if inverted { !pattern } else { pattern };
        // SAFETY: this function is called only from `qualify_entire_psram`
        // while its exclusive mapped-region proof is active and with identical
        // start/length arguments.
        let actual = unsafe { start.add(index).read_volatile() };
        if actual != expected {
            panic!(
                "e290-qualification stage=psram-pattern status=FAIL word={} address=0x{:08x} expected=0x{:08x} actual=0x{:08x} inverted={}",
                index,
                start.wrapping_add(index) as usize,
                expected,
                actual,
                inverted
            );
        }
    }
}

const fn address_pattern(index: usize) -> u32 {
    (index as u32).wrapping_mul(0x9e37_79b9).rotate_left(11) ^ 0xa5c3_6f19
}

const fn ranges_overlap(
    first_start: usize,
    first_end: usize,
    second_start: usize,
    second_end: usize,
) -> bool {
    first_start < second_end && second_start < first_end
}

fn log_heap(stage: &str) {
    let stats = esp_alloc::HEAP.stats();
    let internal_free = esp_alloc::HEAP.free_caps(MemoryCapability::Internal.into());
    let external_free = esp_alloc::HEAP.free_caps(MemoryCapability::External.into());
    info!(
        "e290-qualification stage=heap status=OBSERVED point={} total_bytes={} current_usage_bytes={} max_usage_bytes={} internal_free_bytes={} external_free_bytes={}",
        stage, stats.size, stats.current_usage, stats.max_usage, internal_free, external_free
    );
}
