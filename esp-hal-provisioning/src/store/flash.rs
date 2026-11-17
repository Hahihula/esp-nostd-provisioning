use alloc::{boxed::Box, string::String};
use core::ops::Range;

use embedded_storage_async::nor_flash::MultiwriteNorFlash;
use sequential_storage::{
    cache::Cache,
    map::{MapConfig, MapConfigError, MapStorage},
};

use super::Store;
use crate::schema::{MAX_KEY_LEN, MAX_VALUE_LEN};

/// Scratch space for one item: the key (plus its 2 byte length prefix), the
/// value and sequential-storage's item header (checked in [`FlashStore::new`]),
/// with headroom for word padding.
const BUFFER_LEN: usize = (MAX_KEY_LEN + 2 + MAX_VALUE_LEN as usize + 64).next_multiple_of(32);

/// The scratch buffer is word aligned: `esp-storage` falls back to a 4 KiB
/// stack buffer for every access through an unaligned slice.
#[repr(C, align(4))]
struct Buffer([u8; BUFFER_LEN]);

type Uncached = Cache<
    sequential_storage::cache::Uncached,
    sequential_storage::cache::Uncached,
    sequential_storage::cache::Uncached,
    String,
>;

/// A [`Store`] persisting values in a flash region via `sequential-storage`.
///
/// `flash` must implement the async `MultiwriteNorFlash` trait. For the
/// internal flash of ESP chips wrap `esp_storage::FlashStorage` in
/// `embassy_embedded_hal::adapter::BlockingAsync`.
///
/// The region is usually taken from the partition table, e.g. the `nvs`
/// partition (`esp_bootloader_esp_idf::partitions`). Note that the data
/// format is **not** compatible with ESP-IDF NVS.
pub struct FlashStore<F: MultiwriteNorFlash> {
    map: MapStorage<String, F, Uncached>,
    buffer: Box<Buffer>,
}

impl<F: MultiwriteNorFlash> FlashStore<F> {
    /// Create a store operating on the absolute flash address range `range`.
    ///
    /// The range must be aligned to the erase size and span at least two
    /// erase pages.
    pub fn new(flash: F, range: Range<u32>) -> Result<Self, MapConfigError> {
        let config = MapConfig::try_new(range)?;
        let overhead = MapStorage::<String, F, Uncached>::item_overhead_size() as usize;
        assert!(
            overhead + (MAX_KEY_LEN + 2 + MAX_VALUE_LEN as usize).next_multiple_of(F::WRITE_SIZE)
                <= BUFFER_LEN,
            "FlashStore scratch buffer too small for this flash"
        );
        Ok(Self {
            map: MapStorage::new(flash, config, Cache::new_uncached()),
            buffer: Box::new(Buffer([0; BUFFER_LEN])),
        })
    }

    /// Erase the whole region, removing all stored values.
    pub async fn erase_all(&mut self) -> Result<(), sequential_storage::Error<F::Error>> {
        self.map.erase_all().await
    }

    /// Release the underlying flash.
    pub fn into_inner(self) -> F {
        self.map.destroy().0
    }
}

impl<F: MultiwriteNorFlash> Store for FlashStore<F> {
    type Error = sequential_storage::Error<F::Error>;

    async fn read(&mut self, key: &str) -> Result<Option<String>, Self::Error> {
        let key = String::from(key);
        let result = self
            .map
            .fetch_item::<&[u8]>(&mut self.buffer.0, &key)
            .await
            .map(|value| {
                value.and_then(|bytes| match core::str::from_utf8(bytes) {
                    Ok(text) => Some(String::from(text)),
                    Err(_) => {
                        warn!(
                            "stored value for '{}' is not valid UTF-8, ignoring it",
                            key.as_str()
                        );
                        None
                    }
                })
            });
        // Don't leave secrets in the scratch buffer.
        self.buffer.0.fill(0);
        result
    }

    async fn write(&mut self, key: &str, value: &str) -> Result<(), Self::Error> {
        let key = String::from(key);
        let result = self
            .map
            .store_item(&mut self.buffer.0, &key, &value.as_bytes())
            .await;
        self.buffer.0.fill(0);
        result
    }

    async fn remove(&mut self, key: &str) -> Result<(), Self::Error> {
        let key = String::from(key);
        self.map.remove_item(&mut self.buffer.0, &key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{vec, vec::Vec};
    use core::convert::Infallible;
    use embedded_storage_async::nor_flash::{ErrorType, NorFlash, ReadNorFlash};
    use futures::executor::block_on;

    /// RAM-backed NOR flash: erase sets bytes to 0xff, writes can only clear bits.
    struct RamFlash(Vec<u8>);

    impl ErrorType for RamFlash {
        type Error = Infallible;
    }

    impl ReadNorFlash for RamFlash {
        const READ_SIZE: usize = 1;
        async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            let o = offset as usize;
            bytes.copy_from_slice(&self.0[o..o + bytes.len()]);
            Ok(())
        }
        fn capacity(&self) -> usize {
            self.0.len()
        }
    }

    impl NorFlash for RamFlash {
        const WRITE_SIZE: usize = 4;
        const ERASE_SIZE: usize = 1024;
        async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            self.0[from as usize..to as usize].fill(0xff);
            Ok(())
        }
        async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            for (i, b) in bytes.iter().enumerate() {
                self.0[offset as usize + i] &= *b;
            }
            Ok(())
        }
    }

    impl MultiwriteNorFlash for RamFlash {}

    #[test]
    fn flash_roundtrip() {
        block_on(async {
            let flash = RamFlash(vec![0xff; 4 * 1024]);
            let mut store = FlashStore::new(flash, 0..4 * 1024).unwrap();
            assert_eq!(store.read("wifi/ssid").await.unwrap(), None);
            store.write("wifi/ssid", "home").await.unwrap();
            store.write("wifi/ssid", "office").await.unwrap();
            store.write("wifi/password", "secretpw").await.unwrap();
            assert_eq!(
                store.read("wifi/ssid").await.unwrap().as_deref(),
                Some("office")
            );
            store.remove("wifi/ssid").await.unwrap();
            assert_eq!(store.read("wifi/ssid").await.unwrap(), None);
            assert_eq!(
                store.read("wifi/password").await.unwrap().as_deref(),
                Some("secretpw")
            );
        });
    }

    #[test]
    fn survives_many_rewrites() {
        block_on(async {
            let flash = RamFlash(vec![0xff; 4 * 1024]);
            let mut store = FlashStore::new(flash, 0..4 * 1024).unwrap();
            for i in 0..200 {
                store
                    .write("k", &alloc::format!("value-{i}"))
                    .await
                    .unwrap();
            }
            assert_eq!(store.read("k").await.unwrap().as_deref(), Some("value-199"));
        });
    }

    #[test]
    fn longest_key_and_value_fit() {
        block_on(async {
            let flash = RamFlash(vec![0xff; 8 * 1024]);
            let mut store = FlashStore::new(flash, 0..8 * 1024).unwrap();
            let key = "k".repeat(MAX_KEY_LEN);
            let value = "v".repeat(MAX_VALUE_LEN as usize);
            store.write(&key, &value).await.unwrap();
            assert_eq!(store.read(&key).await.unwrap(), Some(value));
        });
    }

    #[test]
    fn scratch_buffer_is_word_aligned() {
        let store = FlashStore::new(RamFlash(vec![0xff; 4096]), 0..4096).unwrap();
        assert_eq!(store.buffer.0.as_ptr() as usize % 4, 0);
    }

    #[test]
    fn rejects_misaligned_range() {
        assert!(FlashStore::new(RamFlash(vec![0xff; 4096]), 100..4096).is_err());
    }
}
