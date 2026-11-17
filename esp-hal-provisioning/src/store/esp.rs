use embassy_embedded_hal::adapter::BlockingAsync;
use esp_bootloader_esp_idf::partitions::{
    self, DataPartitionSubType, PARTITION_TABLE_MAX_LEN, PartitionType,
};
use esp_hal::peripherals::FLASH;
use esp_storage::FlashStorage;
use sequential_storage::map::MapConfigError;

use super::FlashStore;

/// [`FlashStore`] on the chip's internal flash.
pub type EspFlashStore<'d> = FlashStore<BlockingAsync<FlashStorage<'d>>>;

/// Error creating an [`EspFlashStore`].
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum EspFlashStoreError {
    /// The partition table could not be read.
    PartitionTable(partitions::Error),
    /// No matching partition exists.
    PartitionNotFound,
    /// The partition is not usable (misaligned or too small, needs ≥ 2 sectors).
    InvalidPartition(MapConfigError),
    /// The partition table entry is invalid (it extends past the address space).
    PartitionOutOfRange,
}

/// Create a store on the `nvs` data partition of the internal flash.
///
/// The default ESP-IDF partition tables (and the ones used by `espflash`)
/// contain a 24 KiB `nvs` partition, so this works without a custom
/// partition table.
///
/// ⚠️ The data format is **not** compatible with ESP-IDF NVS: existing NVS
/// content (from an ESP-IDF or Arduino firmware, or calibration data kept
/// there) is **erased** on first use. If the device also runs ESP-IDF code or
/// keeps data in NVS, add a dedicated partition to the partition table and use
/// [`partition_store`] instead.
pub fn nvs_store(flash: FLASH<'_>) -> Result<EspFlashStore<'_>, EspFlashStoreError> {
    open(flash, |p| {
        p.partition_type() == PartitionType::Data(DataPartitionSubType::Nvs)
    })
}

/// Create a store on the partition labelled `label` (from a custom partition table).
pub fn partition_store<'d>(
    flash: FLASH<'d>,
    label: &str,
) -> Result<EspFlashStore<'d>, EspFlashStoreError> {
    open(flash, |p| p.label_as_str() == label)
}

fn open(
    flash: FLASH<'_>,
    matches: impl Fn(&partitions::PartitionEntry) -> bool,
) -> Result<EspFlashStore<'_>, EspFlashStoreError> {
    let mut storage = FlashStorage::new(flash);
    let range = {
        let mut buffer = [0u8; PARTITION_TABLE_MAX_LEN];
        let table = partitions::read_partition_table(&mut storage, &mut buffer)
            .map_err(EspFlashStoreError::PartitionTable)?;
        let partition = table
            .iter()
            .find(|p| matches(p))
            .ok_or(EspFlashStoreError::PartitionNotFound)?;
        let end = partition
            .offset()
            .checked_add(partition.len())
            .ok_or(EspFlashStoreError::PartitionOutOfRange)?;
        partition.offset()..end
    };
    FlashStore::new(BlockingAsync::new(storage), range)
        .map_err(EspFlashStoreError::InvalidPartition)
}
