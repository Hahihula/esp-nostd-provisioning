//! FAT formatted SD cards, via [`embedded-sdmmc`](embedded_sdmmc).
//!
//! [`SdFileSystem`] implements [`FileSystem`] on top of any
//! [`embedded_sdmmc::BlockDevice`]:
//!
//! - **SPI** (every chip): `embedded_sdmmc::SdCard` over an `esp-hal` SPI
//!   device.
//! - **SDMMC host** (esp32, esp32s3, esp32p4, esp32s31): `esp-hal`'s async
//!   `sdmmc::Slot` with the [`sdio`](https://docs.rs/sdio) crate's
//!   `BlockDevice`, wrapped in [`BlockOn`].
//!
//! Only the first partition (or an unpartitioned card) is used, and file
//! names must be 8.3 (e.g. `SETUP.TXT`; lower case on the PC is fine).
//!
//! `embedded-sdmmc` is a blocking library, so every file access blocks the
//! executor while it runs: opening the volume and reading a small file takes
//! a handful of block reads, i.e. tens of milliseconds at the 400 kHz SPI
//! clock needed for card initialisation. Raise the clock once the card is
//! initialised (see [`SdFileSystem::on_success`]).
//!
//! Wiping files (before they are rewritten or deleted) overwrites their data
//! clusters in place. This is **best effort**: the card's controller may
//! remap blocks internally, so old contents can survive in blocks the
//! file system no longer sees.

use alloc::{vec, vec::Vec};
use core::{cell::RefCell, fmt};

use aligned::Aligned;
use embedded_sdmmc::{
    Block, BlockCount, BlockDevice, BlockIdx, Mode, TimeSource, Timestamp, VolumeIdx, VolumeManager,
};

use super::FileSystem;

/// Largest provisioning file that is read.
pub const MAX_FILE_LEN: usize = 16 * 1024;

/// Error of [`SdFileSystem`].
#[derive(Debug)]
#[non_exhaustive]
pub enum SdError<E: core::error::Error> {
    /// File system or card error.
    Fs(embedded_sdmmc::Error<E>),
    /// The file is larger than [`MAX_FILE_LEN`].
    TooLarge,
}

impl<E: core::error::Error> From<embedded_sdmmc::Error<E>> for SdError<E> {
    fn from(e: embedded_sdmmc::Error<E>) -> Self {
        Self::Fs(e)
    }
}

/// A [`TimeSource`] returning a fixed timestamp (2026-01-01 00:00), for
/// devices without a real-time clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct FixedTime;

impl TimeSource for FixedTime {
    fn get_timestamp(&self) -> Timestamp {
        Timestamp {
            year_since_1970: 56,
            zero_indexed_month: 0,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

/// [`FileSystem`] on the root directory of the first FAT volume of a card.
pub struct SdFileSystem<D: BlockDevice, T: TimeSource = FixedTime> {
    parts: Option<(D, T)>,
    on_error: Option<fn(&D)>,
    on_success: Option<fn(&D)>,
}

type Dir<'a, D, T> = embedded_sdmmc::Directory<'a, D, T, 4, 4, 1>;

impl<D: BlockDevice> SdFileSystem<D, FixedTime> {
    /// Create a file system on `device`, using [`FixedTime`] for timestamps.
    pub fn new(device: D) -> Self {
        Self::with_time_source(device, FixedTime)
    }
}

impl<D: BlockDevice, T: TimeSource> SdFileSystem<D, T> {
    /// Create a file system on `device` with a custom time source.
    pub fn with_time_source(device: D, time_source: T) -> Self {
        Self {
            parts: Some((device, time_source)),
            on_error: None,
            on_success: None,
        }
    }

    /// Called with the device after a failed access, e.g. to force card
    /// re-initialisation so a removed and re-inserted card works:
    ///
    /// ```rust,ignore
    /// let fs = SdFileSystem::new(SdCard::new(spi, delay)).on_error(SdCard::mark_card_uninit);
    /// ```
    pub fn on_error(mut self, hook: fn(&D)) -> Self {
        self.on_error = Some(hook);
        self
    }

    /// Called with the device after a successful access, e.g. to raise the
    /// SPI clock once the card is initialised (and lower it again in
    /// [`on_error`](Self::on_error), since re-initialisation needs ≤ 400 kHz):
    ///
    /// ```rust,ignore
    /// fn fast(card: &Card) {
    ///     card.spi(|dev| dev.bus_mut().apply_config(&spi_config(25_000)).ok());
    /// }
    /// let fs = SdFileSystem::new(card).on_error(slow_and_uninit).on_success(fast);
    /// ```
    pub fn on_success(mut self, hook: fn(&D)) -> Self {
        self.on_success = Some(hook);
        self
    }

    /// Release the block device and time source.
    pub fn into_inner(mut self) -> (D, T) {
        self.parts
            .take()
            .expect("parts are always present between calls")
    }

    /// Run `f` on the root directory. A fresh `VolumeManager` is used for
    /// every access so no stale cache survives a card swap.
    fn with_root<R>(
        &mut self,
        f: impl FnOnce(&Dir<'_, D, T>) -> Result<R, SdError<D::Error>>,
    ) -> Result<R, SdError<D::Error>> {
        let (device, time) = self
            .parts
            .take()
            .expect("parts are always present between calls");
        let manager: VolumeManager<D, T, 4, 4, 1> = VolumeManager::new(device, time);
        let result = (|| {
            let volume = manager.open_volume(VolumeIdx(0))?;
            let root = volume.open_root_dir()?;
            f(&root)
        })();
        let (device, time) = manager.free();
        if let Some(hook) = if result.is_ok() {
            self.on_success
        } else {
            self.on_error
        } {
            hook(&device);
        }
        self.parts = Some((device, time));
        result
    }
}

/// Overwrite the contents of an existing file with zeros, in place.
fn wipe<D: BlockDevice, T: TimeSource>(
    root: &Dir<'_, D, T>,
    name: &str,
) -> Result<bool, SdError<D::Error>> {
    let file = match root.open_file_in_dir(name, Mode::ReadWriteAppend) {
        Ok(file) => file,
        Err(embedded_sdmmc::Error::NotFound) => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let len = file.length() as usize;
    file.seek_from_start(0)?;
    let zeros = [0u8; 512];
    let mut left = len;
    while left > 0 {
        let n = left.min(zeros.len());
        file.write(&zeros[..n])?;
        left -= n;
    }
    file.close()?;
    Ok(true)
}

impl<D: BlockDevice, T: TimeSource> FileSystem for SdFileSystem<D, T> {
    type Error = SdError<D::Error>;

    async fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>, Self::Error> {
        self.with_root(|root| {
            let file = match root.open_file_in_dir(name, Mode::ReadOnly) {
                Ok(file) => file,
                Err(embedded_sdmmc::Error::NotFound) => return Ok(None),
                Err(e) => return Err(e.into()),
            };
            let len = file.length() as usize;
            if len > MAX_FILE_LEN {
                return Err(SdError::TooLarge);
            }
            let mut data = vec![0u8; len];
            let mut filled = 0;
            while filled < len {
                let n = file.read(&mut data[filled..])?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            data.truncate(filled);
            file.close()?;
            Ok(Some(data))
        })
    }

    async fn write(&mut self, name: &str, data: &[u8]) -> Result<(), Self::Error> {
        self.with_root(|root| {
            wipe(root, name)?;
            let file = root.open_file_in_dir(name, Mode::ReadWriteCreateOrTruncate)?;
            file.write(data)?;
            file.close()?;
            Ok(())
        })
    }

    async fn remove(&mut self, name: &str) -> Result<(), Self::Error> {
        self.with_root(|root| {
            if wipe(root, name)? {
                root.delete_entry_in_dir(name)?;
            }
            Ok(())
        })
    }

    fn error_reason(error: &Self::Error) -> Option<&'static str> {
        match error {
            SdError::TooLarge => Some("the file is larger than 16 KiB"),
            SdError::Fs(_) => None,
        }
    }
}

/// Error of a device wrapped in [`BlockOn`].
#[derive(Debug)]
pub struct BlockOnError<E>(pub E);

impl<E: fmt::Debug> fmt::Display for BlockOnError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "block device error: {:?}", self.0)
    }
}

impl<E: fmt::Debug> core::error::Error for BlockOnError<E> {}

/// Use an async [`block_device_driver::BlockDevice`] (such as the `sdio`
/// crate's SD card driver on `esp-hal`'s SDMMC host) as an
/// [`embedded_sdmmc::BlockDevice`].
///
/// Every block access is driven to completion with
/// [`embassy_futures::block_on`], which busy-polls the future on the current
/// task. That works for drivers woken from interrupts (like `esp-hal`'s
/// SDMMC host), but would dead-lock with a driver that needs another task of
/// the same executor to make progress.
pub struct BlockOn<D>(RefCell<D>);

impl<D> BlockOn<D> {
    /// Wrap an async block device.
    pub fn new(device: D) -> Self {
        Self(RefCell::new(device))
    }

    /// Access the wrapped device, e.g. from an [`SdFileSystem::on_error`]
    /// hook to re-acquire the card.
    pub fn with_inner<R>(&self, f: impl FnOnce(&mut D) -> R) -> R {
        f(&mut self.0.borrow_mut())
    }

    /// Release the wrapped device.
    pub fn into_inner(self) -> D {
        self.0.into_inner()
    }
}

impl<D> BlockDevice for BlockOn<D>
where
    D: block_device_driver::BlockDevice<512>,
    D::Error: 'static,
{
    type Error = BlockOnError<D::Error>;

    fn read(&self, blocks: &mut [Block], start_block_idx: BlockIdx) -> Result<(), Self::Error> {
        let mut device = self.0.borrow_mut();
        let mut buffer = [Aligned::<D::Align, _>([0u8; 512])];
        for (i, block) in blocks.iter_mut().enumerate() {
            embassy_futures::block_on(device.read(start_block_idx.0 + i as u32, &mut buffer))
                .map_err(BlockOnError)?;
            block.contents.copy_from_slice(&buffer[0][..]);
        }
        Ok(())
    }

    fn write(&self, blocks: &[Block], start_block_idx: BlockIdx) -> Result<(), Self::Error> {
        let mut device = self.0.borrow_mut();
        let mut buffer = [Aligned::<D::Align, _>([0u8; 512])];
        for (i, block) in blocks.iter().enumerate() {
            buffer[0].copy_from_slice(&block.contents);
            embassy_futures::block_on(device.write(start_block_idx.0 + i as u32, &buffer))
                .map_err(BlockOnError)?;
        }
        Ok(())
    }

    fn num_blocks(&self) -> Result<BlockCount, Self::Error> {
        let bytes = embassy_futures::block_on(self.0.borrow_mut().size()).map_err(BlockOnError)?;
        Ok(BlockCount((bytes / 512) as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Field, NoVerify, Options, Schema,
        file::{FileConfig, FileTransport},
        provision,
        store::MemoryStore,
    };
    use embassy_time::Duration;
    use futures::executor::block_on;

    /// RAM disk holding an MBR with a single FAT16 partition.
    struct RamDisk(RefCell<Vec<u8>>);

    #[derive(Debug)]
    struct RamDiskError;
    impl fmt::Display for RamDiskError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("ram disk error")
        }
    }
    impl core::error::Error for RamDiskError {}

    const PART_START: u32 = 1;
    const PART_SECTORS: u32 = 8192;

    impl RamDisk {
        fn fat16() -> Self {
            let mut d = vec![0u8; (PART_START + PART_SECTORS) as usize * 512];
            // MBR: one FAT16 (LBA) partition.
            let e = 446;
            d[e + 4] = 0x0e;
            d[e + 8..e + 12].copy_from_slice(&PART_START.to_le_bytes());
            d[e + 12..e + 16].copy_from_slice(&PART_SECTORS.to_le_bytes());
            d[510] = 0x55;
            d[511] = 0xaa;
            // Boot sector / BPB.
            let b = PART_START as usize * 512;
            d[b..b + 3].copy_from_slice(&[0xeb, 0x3c, 0x90]);
            d[b + 3..b + 11].copy_from_slice(b"MSWIN4.1");
            d[b + 11..b + 13].copy_from_slice(&512u16.to_le_bytes()); // bytes per sector
            d[b + 13] = 1; // sectors per cluster
            d[b + 14..b + 16].copy_from_slice(&1u16.to_le_bytes()); // reserved sectors
            d[b + 16] = 2; // FATs
            d[b + 17..b + 19].copy_from_slice(&512u16.to_le_bytes()); // root entries
            d[b + 21] = 0xf8; // media
            d[b + 22..b + 24].copy_from_slice(&32u16.to_le_bytes()); // sectors per FAT
            d[b + 32..b + 36].copy_from_slice(&PART_SECTORS.to_le_bytes());
            d[b + 38] = 0x29;
            d[b + 43..b + 54].copy_from_slice(b"PROVISION  ");
            d[b + 54..b + 62].copy_from_slice(b"FAT16   ");
            d[b + 510] = 0x55;
            d[b + 511] = 0xaa;
            // Both FATs: reserved entries 0 and 1.
            for fat in 0..2 {
                let f = b + 512 * (1 + 32 * fat);
                d[f..f + 4].copy_from_slice(&[0xf8, 0xff, 0xff, 0xff]);
            }
            Self(RefCell::new(d))
        }
    }

    impl BlockDevice for RamDisk {
        type Error = RamDiskError;
        fn read(&self, blocks: &mut [Block], start: BlockIdx) -> Result<(), Self::Error> {
            let d = self.0.borrow();
            for (i, block) in blocks.iter_mut().enumerate() {
                let o = (start.0 as usize + i) * 512;
                block
                    .contents
                    .copy_from_slice(d.get(o..o + 512).ok_or(RamDiskError)?);
            }
            Ok(())
        }
        fn write(&self, blocks: &[Block], start: BlockIdx) -> Result<(), Self::Error> {
            let mut d = self.0.borrow_mut();
            for (i, block) in blocks.iter().enumerate() {
                let o = (start.0 as usize + i) * 512;
                d.get_mut(o..o + 512)
                    .ok_or(RamDiskError)?
                    .copy_from_slice(&block.contents);
            }
            Ok(())
        }
        fn num_blocks(&self) -> Result<BlockCount, Self::Error> {
            Ok(BlockCount((self.0.borrow().len() / 512) as u32))
        }
    }

    fn contains(disk: &[u8], needle: &[u8]) -> bool {
        disk.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn read_write_remove_on_fat16() {
        block_on(async {
            let mut fs = SdFileSystem::new(RamDisk::fat16());
            assert_eq!(fs.read("SETUP.TXT").await.unwrap(), None);
            fs.write("SETUP.TXT", b"[wifi]\nssid = home\npassword = topsecret1\n")
                .await
                .unwrap();
            assert_eq!(
                fs.read("setup.txt").await.unwrap().as_deref(),
                Some(&b"[wifi]\nssid = home\npassword = topsecret1\n"[..])
            );
            fs.write("SETUP.TXT", b"short").await.unwrap();
            assert_eq!(
                fs.read("SETUP.TXT").await.unwrap().as_deref(),
                Some(&b"short"[..])
            );
            fs.remove("SETUP.TXT").await.unwrap();
            fs.remove("SETUP.TXT").await.unwrap(); // missing file is fine
            assert_eq!(fs.read("SETUP.TXT").await.unwrap(), None);

            let (disk, _) = fs.into_inner();
            assert!(
                !contains(&disk.0.borrow(), b"topsecret1"),
                "secret left on the card"
            );
        });
    }

    #[test]
    fn rejects_huge_files() {
        block_on(async {
            let mut fs = SdFileSystem::new(RamDisk::fat16());
            fs.write("BIG.TXT", &vec![b'x'; MAX_FILE_LEN + 1])
                .await
                .unwrap();
            assert!(matches!(fs.read("BIG.TXT").await, Err(SdError::TooLarge)));
        });
    }

    #[test]
    fn unformatted_card_is_an_error_and_triggers_hook() {
        static CALLED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
        block_on(async {
            let disk = RamDisk(RefCell::new(vec![0; 64 * 512]));
            let mut fs = SdFileSystem::new(disk)
                .on_error(|_| CALLED.store(true, core::sync::atomic::Ordering::SeqCst));
            assert!(fs.read("SETUP.TXT").await.is_err());
            assert!(CALLED.load(core::sync::atomic::Ordering::SeqCst));
        });
    }

    static WIFI: Schema = Schema::new(
        "wifi",
        "Wi-Fi",
        &[
            Field::text("ssid", "Network", ""),
            Field::secret("password", "Password", "").optional(),
        ],
    );

    #[test]
    fn provision_from_card_end_to_end() {
        block_on(async {
            let mut fs = SdFileSystem::new(RamDisk::fat16());
            fs.write(
                "SETUP.TXT",
                b"[wifi]\r\nssid = home\r\npassword = topsecret1\r\n",
            )
            .await
            .unwrap();
            let config = FileConfig {
                poll_interval: Duration::from_millis(1),
                ..Default::default()
            };
            let mut transport = FileTransport::with_config(fs, config);
            let mut store = MemoryStore::new();
            let v = provision(
                &WIFI,
                &mut store,
                &mut transport,
                &mut NoVerify,
                &Options::default(),
            )
            .await
            .unwrap();
            assert_eq!(v.get("password"), Some("topsecret1"));

            let mut fs = transport.into_inner();
            assert_eq!(
                fs.read("SETUP.TXT").await.unwrap(),
                None,
                "file should be consumed"
            );
            let (disk, _) = fs.into_inner();
            assert!(
                !contains(&disk.0.borrow(), b"topsecret1"),
                "secret left on the card"
            );
        });
    }

    /// Async RAM block device with 4-byte alignment, like `sdio`'s.
    struct AsyncRam(Vec<u8>);

    impl block_device_driver::BlockDevice<512> for AsyncRam {
        type Error = ();
        type Align = aligned::A4;
        async fn read(
            &mut self,
            addr: u32,
            data: &mut [Aligned<aligned::A4, [u8; 512]>],
        ) -> Result<(), ()> {
            for (i, b) in data.iter_mut().enumerate() {
                let o = (addr as usize + i) * 512;
                b.copy_from_slice(self.0.get(o..o + 512).ok_or(())?);
            }
            Ok(())
        }
        async fn write(
            &mut self,
            addr: u32,
            data: &[Aligned<aligned::A4, [u8; 512]>],
        ) -> Result<(), ()> {
            for (i, b) in data.iter().enumerate() {
                let o = (addr as usize + i) * 512;
                self.0
                    .get_mut(o..o + 512)
                    .ok_or(())?
                    .copy_from_slice(&b[..]);
            }
            Ok(())
        }
        async fn size(&mut self) -> Result<u64, ()> {
            Ok(self.0.len() as u64)
        }
    }

    #[test]
    fn block_on_adapter_works_with_async_devices() {
        block_on(async {
            let image = RamDisk::fat16().0.into_inner();
            let mut fs = SdFileSystem::new(BlockOn::new(AsyncRam(image)));
            fs.write("SETUP.TXT", b"hello").await.unwrap();
            assert_eq!(
                fs.read("SETUP.TXT").await.unwrap().as_deref(),
                Some(&b"hello"[..])
            );
            let (dev, _) = fs.into_inner();
            assert_eq!(
                dev.with_inner(|d| d.0.len()),
                (PART_START + PART_SECTORS) as usize * 512
            );
        });
    }
}
