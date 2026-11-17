use alloc::{string::String, vec, vec::Vec};
use core::ops::Range;

use embedded_storage_async::nor_flash::MultiwriteNorFlash;
use sequential_storage::{
    cache::Cache,
    map::{MapConfig, MapConfigError, MapStorage},
};

use super::Store;
use crate::schema::MAX_VALUE_LEN;

/// Maximum length of a full storage key (`"{namespace}/{field}"`).
const MAX_KEY_LEN: usize = 64;

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
    buffer: Vec<u8>,
}

impl<F: MultiwriteNorFlash> FlashStore<F> {
    /// Create a store operating on the absolute flash address range `range`.
    ///
    /// The range must be aligned to the erase size and span at least two
    /// erase pages.
    pub fn new(flash: F, range: Range<u32>) -> Result<Self, MapConfigError> {
        let config = MapConfig::try_new(range)?;
        // Key + value + headers, rounded up generously to the word size.
        let len =
            (MAX_KEY_LEN + MAX_VALUE_LEN as usize + 2 + 16).next_multiple_of(F::WRITE_SIZE.max(4));
        Ok(Self {
            map: MapStorage::new(flash, config, Cache::new_uncached()),
            buffer: vec![0; len],
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
        let value = self.map.fetch_item::<&[u8]>(&mut self.buffer, &key).await?;
        Ok(value.and_then(|bytes| core::str::from_utf8(bytes).ok().map(String::from)))
    }

    async fn write(&mut self, key: &str, value: &str) -> Result<(), Self::Error> {
        let key = String::from(key);
        let result = self
            .map
            .store_item(&mut self.buffer, &key, &value.as_bytes())
            .await;
        // Don't leave secrets in the scratch buffer.
        self.buffer.fill(0);
        result
    }

    async fn remove(&mut self, key: &str) -> Result<(), Self::Error> {
        let key = String::from(key);
        self.map.remove_item(&mut self.buffer, &key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn rejects_misaligned_range() {
        assert!(FlashStore::new(RamFlash(vec![0xff; 4096]), 100..4096).is_err());
    }
}
