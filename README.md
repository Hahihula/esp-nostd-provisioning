# ESP no_std provisioning

Get Wi-Fi credentials, or any other settings, onto an ESP32 device without
recompiling. The crate is async (Embassy), `no_std` and bare-metal, built on
[`esp-hal`], and works on every chip `esp-hal` supports.

The goal is for this crate to become part of [esp-hal-community], so it
follows esp-rs conventions: chip features rather than per-chip branches,
optional features for each provisioning path, and CI for every chip.

[`esp-hal`]: https://github.com/esp-rs/esp-hal
[esp-hal-community]: https://github.com/esp-rs/esp-hal-community

## What it does

When the device has no valid settings stored, it asks for them over one or
more *provisioning paths*. It checks the values (for Wi-Fi, by actually
connecting) and only then saves them to flash. On later boots the stored values
are used. A button, repeated connection failures or your own code can start
provisioning again.

Wi-Fi is the built-in default. Any other settings (an MQTT broker, an API
token, a device name, …) work the same way: describe them once as a schema with
names and descriptions, and every path renders, validates and stores them.

| Path | Feature | Status |
|---|---|---|
| Serial console (UART or USB-Serial-JTAG): pick from the strongest scanned networks, masked password | `cli` | ✅ done, tested on hardware |
| Text file on an SD card (SPI or SDMMC) | `sdcard` | ✅ done, host-tested only |
| Bluetooth LE and a web page (Web Bluetooth, hosted on GitHub Pages) | `ble`, `esp-ble` | ✅ done, tested on hardware |
| ESP-IDF unified provisioning (Espressif's official apps) | `idf-prov` | planned |
| SoftAP / captive portal, and HTTP over Ethernet (ESP32-P4) | `softap` | planned |

Several paths can run at the same time; whichever one delivers valid values
first wins.

## Repository layout

```text
esp-hal-provisioning/        the crate
  src/                       core, storage, transports (cli, file, ble), wifi
  examples/                  per-chip example firmware (its own Cargo workspace)
  README.md                  full crate documentation: API, features, formats
web/index.html               the BLE provisioning web page (single static file)
.github/workflows/ci.yml     host tests, clippy, build matrix for all chips
.github/workflows/pages.yml  publishes web/ to GitHub Pages
```

## Using the crate

The crate is not on crates.io yet. Add it as a git dependency (replace the URL
with this repository's):

```toml
[dependencies]
esp-hal-provisioning = { git = "https://github.com/Hahihula/esp-nostd-provisioning", features = ["esp32c3"] }
```

The default features are `wifi`, `cli`, `flash-store` and `esp-flash`. Add `sdcard`,
`esp-ble` or `coex` (Wi-Fi verification while connected over BLE) as needed.
Chips without Wi-Fi (esp32h2, esp32p4) use `default-features = false`.

Wi-Fi provisioning over the serial console in one call:

```rust,ignore
use esp_hal_provisioning::{cli::Cli, store, wifi::{self, WifiConfig}};

let mut store = store::nvs_store(peripherals.FLASH)?;
let creds = wifi::provision(&mut wifi_controller, &mut store, &mut Cli::new(console), &WifiConfig::default()).await?;
// The controller is connected; `creds` holds the SSID and password.
```

Over BLE and the console at the same time:

```rust,ignore
let creds = ble::run(ble::controller(peripherals.BT)?, &BleConfig::default(), async |ble| {
    wifi::provision(&mut wifi_controller, &mut store, &mut (&mut cli, ble), &WifiConfig::default()).await
}).await?;
```

Custom settings, re-provisioning triggers, the SD card file format, BLE
hosting on your own domain, storage details, and the feature and chip tables
are all in **[esp-hal-provisioning/README.md](esp-hal-provisioning/README.md)**.

## Supported chips

| Chip | Wi-Fi | BLE | SD (SPI) | SD (SDMMC) |
|---|---|---|---|---|
| esp32 | ✅ | ⚠️ fails to link in esp-radio 1.0.0-beta.1 | ✅ | ✅ |
| esp32c2, esp32c3, esp32c5, esp32c6, esp32c61 | ✅ | ✅ | ✅ | |
| esp32s2 | ✅ | | ✅ | |
| esp32s3 | ✅ | ✅ | ✅ | ✅ |
| esp32s31 | ✅ | not yet in esp-radio | ✅ | ✅ |
| esp32h2 | | ✅ | ✅ | |
| esp32p4 | | | ✅ | ✅ |

The serial console and custom settings work on every chip.

## Development

### Toolchains

- **RISC-V chips** (c2, c3, c5, c6, c61, h2, p4, s31): stable Rust 1.88 or newer,
  plus the target, e.g. `rustup target add riscv32imc-unknown-none-elf`.
- **Xtensa chips** (esp32, s2, s3): the `esp` toolchain from [espup]. Use
  version **1.98.1**; esp 1.99 currently fails to link esp32s3 images.

  ```bash
  espup install --toolchain-version 1.98.1.0
  ```

- **Flashing:** [espflash]

  ```bash
  cargo install espflash
  ```

[espup]: https://github.com/esp-rs/espup
[espflash]: https://github.com/esp-rs/espflash

### Host tests

The schema, storage, provisioning loop, console, SD file format, FAT
filesystem and BLE protocol are tested on the host, no hardware needed:

```bash
cargo test -p esp-hal-provisioning --no-default-features --features cli,flash-store,sdcard,ble
```

```bash
cargo test -p esp-hal-provisioning --no-default-features --features file
```

Lint and formatting, as CI runs them:

```bash
cargo fmt --all -- --check
```

```bash
cargo clippy -p esp-hal-provisioning --no-default-features --features cli,flash-store,sdcard,ble --all-targets -- -D warnings
```

### Building the examples

The examples live in their own Cargo workspace with one alias per chip, which
builds, flashes and opens the monitor:

```bash
cd esp-hal-provisioning/examples
```

| Example | What it shows | Command (C3 as an example) |
|---|---|---|
| `cli_custom` | Custom settings (MQTT) over the console, no radio | `cargo esp32c3 --bin cli_custom` |
| `cli_wifi` | Wi-Fi over the console | `cargo esp32c3 --bin cli_wifi --features wifi` |
| `sd_wifi` | Wi-Fi from an SD card (SPI) or the console | `cargo esp32c3 --bin sd_wifi --features wifi,sdcard` |
| `sdmmc_custom` | Custom settings from an SD card on the SDMMC host | `cargo esp32s3 --bin sdmmc_custom --features sdmmc` |
| `ble_wifi` | Wi-Fi over BLE (web page) or the console | `cargo esp32c3 --bin ble_wifi --features coex` |
| `ble_custom` | Custom settings over BLE | `cargo esp32h2 --bin ble_custom --features ble` |

Swap the alias for your chip (`cargo esp32c6 …`, `cargo esp32h2 …`, …). On
Xtensa chips add the toolchain: `cargo +esp esp32s3 …`.

The console follows the same rule as `esp-println`: on chips with a built-in
USB port (USB-Serial-JTAG), it is used when a computer is connected to that
port; otherwise UART0 on the board's usual console pins. The first log line
says which one was picked (`console: …`). The pins for each chip (UART, BOOT
button, SD card SPI) are in
[examples/src/lib.rs](esp-hal-provisioning/examples/src/lib.rs); adjust them
to your board.

## Testing on real hardware

Go in this order: each step relies on what the previous one proved. Start
from an erased flash so nothing old is stored:

```bash
espflash erase-flash
```

### 1. Console with custom settings (storage only, no radio)

```bash
cargo esp32c3 --bin cli_custom
```

- [ ] Every field shows its name and description; a secret shows `*` as you type.
- [ ] Invalid input (bad number, too long) is rejected and asked again.
- [ ] After a reset, the values load from flash without asking.
- [ ] Holding BOOT for 3 s during reset asks again, with the current values shown.

### 2. Console with Wi-Fi (scan, verify, save)

```bash
cargo esp32c3 --bin cli_wifi --features wifi
```

- [ ] Up to 10 networks appear, strongest first; you can pick a number or type a hidden SSID.
- [ ] A wrong password is reported, nothing is saved, and you are asked again.
- [ ] The right password connects and is saved; the next boot connects without asking.
- [ ] With the router off or its password changed, the device returns to provisioning after the configured number of failed connects.
- [ ] On an esp32c5, 5 GHz networks appear too.

### 3. SD card

Use a FAT16/FAT32 card. Wire it to the SPI pins from `sd_spi_pins!` (C3: SCK 4,
MOSI 6, MISO 5, CS 7).

```bash
cargo esp32c3 --bin sd_wifi --features wifi,sdcard
```

- [ ] On first boot the device writes `SETUP.TXT` containing a template.
- [ ] Fill it in on a PC and re-insert the card: the device picks it up within ~2 s without a reset, connects, and removes the section.
- [ ] Check on the PC that the password is no longer anywhere in the file.
- [ ] A wrong password produces `SETUPERR.TXT` explaining why.

For the SDMMC host (esp32, s3, p4, s31) use `sdmmc_custom --features sdmmc`.
On P4 and S31 boards the card is often powered from an on-chip LDO, which the
example does not enable yet.

### 4. Bluetooth LE

```bash
cargo esp32c3 --bin ble_wifi --features coex
```

Web Bluetooth needs HTTPS or `localhost`. While developing, serve the page
locally and open it in Chrome or Edge on your computer:

```bash
cd web && python3 -m http.server 8000
```

Then open <http://localhost:8000>. To test from a phone, use the GitHub Pages
deployment (Android: Chrome; iPhone/iPad: the [Bluefy] browser).

- [ ] The device appears as `ESP-Provision` in the browser's device picker; the serial console keeps working at the same time.
- [ ] Scanned networks are offered for the SSID; Show/Hide toggles the password.
- [ ] A wrong password shows "Checking…", then the error, and the connection stays up for another try.
- [ ] The right password shows "Saved" and the device continues with Wi-Fi.
- [ ] Re-provisioning (BOOT held): the password field offers to keep the current value, and leaving it empty keeps it.
- [ ] Finishing over the console while the browser is connected does not hang the device.

[Bluefy]: https://apps.apple.com/app/bluefy-web-ble-browser/id1492822055

### Reporting a problem

Include the full `espflash monitor` output, the chip and board name, and the
exact command. Things most likely to break first:
- heap exhaustion (allocation panics), especially with `coex` on C2 and C3;
- Wi-Fi connect timeouts while a phone is connected over BLE;
- strapping or console pins that differ on your board;
- SD cards that fail to initialise at 400 kHz.

### Hardware test status

| Example | Board | Result |
|---|---|---|
| `cli_custom` | ESP32-C3 (built-in USB) | ✅ |
| `cli_wifi` | ESP32-C3 (built-in USB) | ✅ |
| `ble_wifi` | ESP32-C3, desktop Chrome | ✅ |
| `sd_wifi`, `sdmmc_custom` | | not tested yet |
| other chips | | not tested yet |

## Continuous integration

`.github/workflows/ci.yml` runs on pushes to `main`/`master` and on pull requests:

- **host:** formatting, clippy and the host tests above;
- **lib:** clippy for the library on all 11 chips, with both `log` and `defmt`;
- **examples:** builds the examples for every chip with its supported features.

`.github/workflows/pages.yml` publishes `web/` to GitHub Pages on pushes to
`main` or `master`. Enable Pages with "GitHub Actions" as the source in the repository
settings.

## Known limitations

- BLE protocol v1 has no pairing or encryption: anyone in range can read the
  values and also write their own. Provision in a trusted environment. The
  planned ESP-IDF provisioning path adds encryption (security1/security2).
- `store::nvs_store()` erases existing ESP-IDF NVS data in the `nvs`
  partition (the formats are incompatible); use `partition_store()` with a
  dedicated partition if that matters.
- Secret handling (RAM wiping, SD card wiping) is best effort; see the crate
  README.
- Classic esp32: BLE fails to link with esp-radio 1.0.0-beta.1 (unresolved
  symbols in the controller library). The esp32 examples are built without BLE.
- esp32s31: esp-radio does not support BLE yet; the BOOT button pin in the
  examples is unverified.
- The on-flash format (`sequential-storage`) is not compatible with ESP-IDF NVS.

## Roadmap

1. ESP-IDF unified provisioning protocol (`idf-prov`), so Espressif's official
   apps work, including on iOS without Bluefy.
2. Encryption for the BLE path.
3. SoftAP / captive portal, and HTTP provisioning over Ethernet for the ESP32-P4.
4. Move into esp-hal-community.

## License

Licensed under either of [Apache License, Version 2.0](esp-hal-provisioning/LICENSE-APACHE)
or [MIT license](esp-hal-provisioning/LICENSE-MIT) at your option.
