# esp-hal-provisioning

Async (Embassy), `no_std`, schema-driven device provisioning for [`esp-hal`].

Wi-Fi credentials are the built-in default, but any application settings can
be provisioned the same way: describe them once as a `Schema`, and every
provisioning path (serial console, SD card, BLE, …) renders, validates and
stores them for you.

[`esp-hal`]: https://github.com/esp-rs/esp-hal

## Wi-Fi in one call

```rust,ignore
use esp_hal_provisioning::{cli::Cli, store, wifi::{self, WifiConfig}};

let mut controller = WifiController::new(peripherals.WIFI, Default::default())?;
let mut store = store::nvs_store(peripherals.FLASH)?;

let creds = wifi::provision(
    &mut controller,
    &mut store,
    &mut Cli::new(uart),          // or a USB-Serial-JTAG console
    &WifiConfig::default(),
).await?;
// `controller` is now connected.
```

On first boot this scans for networks, shows the strongest ones (10 by default) in
the serial monitor, asks for the password (masked), **verifies it by connecting**, and
only then saves it. On subsequent boots the stored credentials are used; if
they stop working, provisioning starts again.

```text
=== Wi-Fi provisioning ===

Network
  Wi-Fi network (SSID) to connect to
   1) HomeNetwork                       -42 dBm [####] WPA2
   2) Office                            -61 dBm [### ] WPA3
   3) Guest                             -77 dBm [##  ] open
  Enter 1-3 or type a value
> 1

Password (optional)
  Leave empty for open networks
> ************

Summary:
  Network              HomeNetwork
  Password             ********
Save? [Y/n]
Applying...
Wi-Fi settings saved.
```

## Custom settings

```rust,ignore
use esp_hal_provisioning::{Field, NoVerify, Options, Schema, provision};

static MQTT: Schema = Schema::new("mqtt", "MQTT broker", &[
    Field::text("host", "Broker host", "Hostname or IP address of the MQTT broker"),
    Field::integer("port", "Broker port", "TCP port of the broker", 1, 65535),
    Field::boolean("tls", "Use TLS", "Connect using TLS"),
    Field::secret("token", "Access token", "Token issued by the broker").optional(),
]);

let settings = provision(&MQTT, &mut store, &mut cli, &mut NoVerify, &Options::default()).await?;
let host = settings.get("host").unwrap();
```

Field types: `text`, `secret` (masked, never logged, zeroed on drop — see
[Secrets](#secrets)), `integer`
(range checked), `boolean`, and `choice` (options supplied at runtime, e.g.
scan results). Each field can be `.optional()`, have `.with_len(min, max)` and
a custom `.with_validator(fn)`.

Implement `Verify` to check values before they are saved (as the Wi-Fi flow
does by connecting). Rejected values are shown to the user with the reason,
and the user is asked again.

## Provisioning paths

| Path | Feature | Status |
|---|---|---|
| Serial console (UART / USB-Serial-JTAG) | `cli` | ✅ |
| Text file on an SD card (SPI or SDMMC) | `file` + `sdcard` | ✅ |
| BLE GATT + web page (GitHub Pages, Web Bluetooth) | `ble` + `esp-ble` | ✅ |
| ESP-IDF unified provisioning protocol (Espressif apps) | `idf-prov` | planned |
| SoftAP + captive portal (and HTTP over Ethernet, e.g. ESP32-P4) | `softap` | planned |

Several transports can run concurrently: the first one to deliver values wins.

```rust,ignore
let mut transports = (Cli::new(uart), FileTransport::new(SdFileSystem::new(sd_card)));
wifi::provision(&mut controller, &mut store, &mut transports, &WifiConfig::default()).await?;
```

### SD card

Put a `SETUP.TXT` in the root of a FAT formatted card. One file can hold
sections for several schemas:

```ini
# Lines starting with '#' or ';' are comments.
[wifi]
ssid = My Network
password = "  quotes keep surrounding spaces  "

[mqtt]
host = broker.local
port = 1883
```

- If the file or section is missing, the device writes a commented-out
  **template** to the card and waits. Remove the card, fill in the values on a
  PC, and re-insert it.
- If the values are rejected (syntax error, failed validation, or wrong Wi-Fi
  password), the reason is written to **`SETUPERR.TXT`** and the device waits
  for the file to change.
- After success the section is **removed** from the card (the file is deleted
  when nothing else is left) and its old contents are overwritten in place
  (best effort, see [Secrets](#secrets)).
  See `AfterUse` for other policies.

```rust,ignore
use esp_hal_provisioning::file::{FileTransport, sd::SdFileSystem};

// SPI (any chip)
let card = embedded_sdmmc::SdCard::new(spi_device, delay);
let mut sd = FileTransport::new(SdFileSystem::new(card).on_error(SdCard::mark_card_uninit));

// SDMMC host (esp32, esp32s3, esp32p4, esp32s31) through the `sdio` crate
let card: sdio::DefaultBlockDevice<sdio::sd::Card, _, _> =
    sdio::BlockDevice::new(slot.into_async(), embassy_time::Delay, 25_000_000).await?;
let mut sd = FileTransport::new(SdFileSystem::new(BlockOn::new(card)));
```

The transport works on any `file::FileSystem`, so other file sources can
be added. FAT support comes from `embedded-sdmmc`, which is blocking: each
poll blocks the executor for a few milliseconds while reading the small file.
File names must be 8.3 (`setup.txt` written on a PC is fine).

### Bluetooth LE

```rust,ignore
use esp_hal_provisioning::ble::{self, BleConfig};

let creds = ble::run(ble::controller(peripherals.BT)?, &BleConfig::default(), async |ble| {
    // BLE and the serial console at the same time:
    wifi::provision(&mut wifi, &mut store, &mut (&mut cli, ble), &WifiConfig::default()).await
})
.await?;
```

The device advertises as `ESP-Provision` (configurable) with a custom GATT
service. Open the provisioning page from [`web/`](../web) (deployed to GitHub
Pages by `.github/workflows/pages.yml`), connect, and fill in the form. The
page reads the device's **schema**, so the same page provisions Wi-Fi as well as
any custom settings. It shows the verification result (e.g. a wrong Wi-Fi
password) live, and the device keeps the connection open while it verifies.

- **Browsers:** Web Bluetooth works in Chrome, Edge and Opera on desktop and
  Android. On iOS/iPadOS use the [Bluefy] browser. The page must be served over
  HTTPS (GitHub Pages is), or from `localhost` while developing.
- **Hosting on your own domain:** copy `web/index.html` to any static HTTPS
  host. Optional URL parameters: `?name=Prefix` filters devices by name;
  `?service=<uuid>` changes the service UUID (the RX/TX UUIDs follow the same
  pattern, `…0002…` / `…0003…`).
- **Your own client:** the protocol (chunked JSON over one write and one
  notify characteristic) is documented in `ble::protocol`.
- **Wi-Fi verification over BLE** needs the `coex` feature (Wi-Fi/BLE
  coexistence).
- **`ble::run` owns the BLE stack** for as long as your closure runs. It can be
  called again later (e.g. for re-provisioning from your API).
- **Security:** protocol v1 has no pairing and no encryption. Anyone in radio
  range can read the values while provisioning runs, and can also **write**
  values (i.e. provision the device before you do). Only run BLE provisioning
  when needed (e.g. after a button press) and in a trusted environment.
- **Chips:** esp32c2, esp32c3, esp32c5, esp32c6, esp32c61, esp32h2, esp32s3.
  The classic esp32 currently fails to link BLE with esp-radio 1.0.0-beta.1, and
  esp-radio does not support BLE on the esp32s31 yet.

### Notes on upcoming paths

- **ESP-IDF unified provisioning** is a separate feature next to the
  custom BLE GATT service, so Espressif's official provisioning apps work.
- **Captive portal** is the least preferred path for Wi-Fi (an open AP);
  it is mainly intended for devices provisioned over Ethernet.

[Bluefy]: https://apps.apple.com/app/bluefy-web-ble-browser/id1492822055

## Storage

`store::nvs_store(peripherals.FLASH)` opens a `FlashStore` on the `nvs` data
partition of the default partition table (`sequential-storage` key/value map
with wear levelling). Use `store::partition_store(flash, "label")` for a custom
partition, or implement the `Store` trait yourself.

> ⚠️ The on-flash format is **not** compatible with ESP-IDF NVS. `nvs_store`
> **erases** whatever ESP-IDF/Arduino firmware stored in the `nvs` partition.
> If the device also runs ESP-IDF code or keeps data there, add a dedicated
> partition to your partition table and use `partition_store` instead.

Do not cancel `provision()` (e.g. with `with_timeout`) while it saves values;
use `max_attempts` to bound a session instead.

## Secrets

Secret handling is **best effort**:

- Secrets are masked on input, never logged, and kept in `Zeroizing`
  buffers that are wiped on drop. Copies made while a value is processed
  (e.g. when a `String` grows) can still leave fragments in freed heap memory.
- The flash store wipes its scratch buffer after each access; values are
  stored in plain text in flash (use flash encryption to protect them).
- The SD card transport overwrites the file in place before rewriting or
  deleting it, but the card's controller may keep old copies in blocks it
  remapped internally.
- `MemoryStore` (for tests and demos) keeps plain `String`s.

## Re-provisioning

Provisioning runs automatically when nothing valid is stored. To re-provision:

- **API:** `Options { force: true, .. }` / `WifiConfig { force: true, .. }`, or `store::clear(&mut store, &SCHEMA)`.
- **Button:** `trigger::button_held(&mut boot_button, true, Duration::from_secs(3)).await`.
- **Failures:** `wifi::provision` falls back to provisioning after `connect_attempts` failed connections.

## Supported chips

| Chip | Wi-Fi | BLE | SD over SPI | SD over SDMMC |
|---|---|---|---|---|
| esp32 | ✅ | ⚠️ esp-radio link issue | ✅ | ✅ |
| esp32c2, esp32c3, esp32c5, esp32c6, esp32c61 | ✅ | ✅ | ✅ | |
| esp32s2 | ✅ | | ✅ | |
| esp32s3 | ✅ | ✅ | ✅ | ✅ |
| esp32s31 | ✅ | not yet in esp-radio | ✅ | ✅ |
| esp32h2 | | ✅ | ✅ | |
| esp32p4 | | | ✅ | ✅ |

All chips support the serial console and custom schemas.

## Feature flags

| Feature | Default | Description |
|---|---|---|
| `cli` | ✅ | Serial console transport |
| `file` | | Text file transport on any `file::FileSystem` |
| `sdcard` | | FAT SD cards (SPI or SDMMC) via `embedded-sdmmc`; implies `file` |
| `ble` | | BLE GATT transport, generic over a `trouble-host` controller |
| `esp-ble` | | `ble::controller()` on the chip's radio (requires a chip) |
| `coex` | | Wi-Fi + BLE coexistence (verify Wi-Fi while connected over BLE) |
| `wifi` | ✅ | Wi-Fi scan/verify via `esp-radio` (requires a chip feature) |
| `flash-store` | ✅ | `FlashStore` on any async `MultiwriteNorFlash` |
| `esp-flash` | ✅ | `store::nvs_store()` on the internal flash (requires a chip feature) |
| `log-04` / `defmt` | | Logging |
| `esp32`, `esp32c2`, `esp32c3`, `esp32c5`, `esp32c6`, `esp32c61`, `esp32h2`, `esp32p4`, `esp32s2`, `esp32s3`, `esp32s31` | | Target chip |

Chips without Wi-Fi (`esp32h2`, `esp32p4`) use
`default-features = false, features = ["cli", "esp-flash", "<chip>"]` and
provision custom schemas.

`wifi` uses `esp-radio`, which needs `esp-hal`'s `unstable` feature.

## Examples

See [`examples/`](examples). Examples are built per chip:

```bash
cd esp-hal-provisioning/examples
cargo esp32c3 --bin cli_wifi --features wifi
cargo esp32c6 --bin cli_wifi --features wifi
cargo esp32h2 --bin cli_custom
cargo esp32c3 --bin sd_wifi --features wifi,sdcard        # SD card (SPI) or console
cargo esp32p4 --bin sdmmc_custom --features sdmmc         # SD card on the SDMMC host
cargo esp32s31 --bin cli_wifi --features wifi
cargo esp32c3 --bin ble_wifi --features coex             # Wi-Fi over BLE (web page) or console
cargo esp32h2 --bin ble_custom --features ble            # custom settings over BLE
cargo +esp esp32s3 --bin cli_wifi --features wifi   # Xtensa: espup toolchain
```

The console is picked the way `esp-println` picks its output: on chips with a
USB-Serial-JTAG port (c3, c5, c6, c61, h2, p4, s3, s31) it is used when a USB
host is attached to the chip's own USB port, otherwise UART0 on the default
console pins. The first log line says which one (`console: …`). The
`usb-serial-jtag` feature forces USB-Serial-JTAG.

## Minimum supported versions

- `esp-hal` 1.2, `esp-radio` 1.0.0-beta.1, `esp-rtos` 0.4
- Rust 1.88 (stable for RISC-V, `esp` toolchain for Xtensa)

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
