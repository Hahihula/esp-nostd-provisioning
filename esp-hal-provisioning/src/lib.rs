//! Async, schema-driven provisioning for `esp-hal` based firmware.
//!
//! The crate collects a set of values described by a [`Schema`] over one or
//! more *transports* (serial console, SD card, BLE, …), optionally verifies
//! them (e.g. by actually connecting to Wi-Fi), and persists them in a
//! [`Store`].
//!
//! Wi-Fi credentials are the built-in default payload (see [`wifi`]), but any
//! application specific settings can be provisioned the same way by defining
//! your own [`Schema`].
//!
//! ## Wi-Fi in one call
//!
//! ```rust,ignore
//! let creds = esp_hal_provisioning::wifi::provision(
//!     &mut wifi_controller,
//!     &mut store,
//!     &mut Cli::new(usb_serial),
//!     &WifiConfig::default(),
//! ).await?;
//! // `wifi_controller` is now connected using `creds`.
//! ```
//!
//! ## Custom settings
//!
//! ```rust,ignore
//! static MQTT: Schema = Schema::new("mqtt", "MQTT broker", &[
//!     Field::text("host", "Broker host", "Hostname or IP of the MQTT broker"),
//!     Field::integer("port", "Broker port", "TCP port", 1, 65535),
//!     Field::secret("token", "Access token", "Token issued by the broker").optional(),
//! ]);
//!
//! let values = esp_hal_provisioning::provision(
//!     &MQTT, &mut store, &mut Cli::new(uart), &mut NoVerify, &Options::default(),
//! ).await?;
//! let host = values.get("host").unwrap();
//! ```
//!
//! ## Feature flags
//!
//! | Feature       | Default | Description                                               |
//! |---------------|---------|-----------------------------------------------------------|
//! | `cli`         | ✅      | Interactive provisioning over a serial console            |
//! | `file`        |         | Provisioning from a text file ([`file::FileTransport`])   |
//! | `sdcard`      |         | FAT SD cards (SPI / SDMMC) for the `file` transport       |
//! | `ble`         |         | BLE GATT transport + Web Bluetooth page ([`ble`])         |
//! | `esp-ble`     |         | `ble::controller()` on the chip's radio (needs a chip)     |
//! | `coex`        |         | Wi-Fi + BLE coexistence (verify Wi-Fi over BLE)           |
//! | `wifi`        | ✅      | Wi-Fi scanning / verification via `esp-radio` (needs chip) |
//! | `flash-store` | ✅      | [`store::FlashStore`] backed by `sequential-storage`      |
//! | `log-04`      |         | Logging through `log`                                     |
//! | `defmt`       |         | Logging through `defmt`                                   |
//! | `esp32…`      |         | Target chip, required together with `wifi`                |
#![no_std]
// The traits use `async fn` without `Send` bounds on purpose: Embassy
// executors on these chips are single threaded, so futures never cross
// threads and implementors should not be forced to be `Send`.
#![allow(async_fn_in_trait)]
#![deny(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(all(feature = "wifi", any(feature = "esp32h2", feature = "esp32p4")))]
compile_error!(
    "The `wifi` feature is not available on chips without a Wi-Fi radio (esp32h2, esp32p4)."
);

#[macro_use]
mod fmt;

mod provision;
pub mod schema;
pub mod store;
pub mod transport;
pub mod trigger;
pub mod values;
pub mod wifi;

#[cfg(feature = "cli")]
pub mod cli;

#[cfg(feature = "file")]
pub mod file;

#[cfg(feature = "ble")]
pub mod ble;

pub use provision::{Error, Options, provision};
pub use schema::{Field, FieldKind, Schema, ValidationError};
pub use store::Store;
pub use transport::{Choice, NoVerify, Request, Transport, Verify, VerifyError};
pub use values::Values;
