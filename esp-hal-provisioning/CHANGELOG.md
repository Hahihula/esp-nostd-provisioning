# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Schema-driven provisioning core: `Schema`, `Field`, `Values`, `provision()`.
- `Store` trait with `MemoryStore` and `FlashStore` (`sequential-storage`), plus `store::nvs_store()` for the internal flash `nvs` partition (`esp-flash`).
- `Transport` trait with concurrent combination of transports via tuples.
- `Verify` trait; values are verified before they are saved.
- CLI transport over any `embedded-io-async` serial port (UART, USB-Serial-JTAG) with masked secret input (`cli`).
- Built-in Wi-Fi schema, scanning (top N networks), and connection verification on `esp-radio` (`wifi`).
- File transport (`file`): INI-like `SETUP.TXT` with one section per schema, template generation, error reports in `SETUPERR.TXT`, polling for card changes, and removal of applied secrets.
- FAT SD card backend (`sdcard`) via `embedded-sdmmc`, for SPI cards and (through `BlockOn` + `sdio`) the `esp-hal` SDMMC host.
- BLE transport (`ble`, `esp-ble`): GATT service with a chunked JSON protocol, schema-driven Web Bluetooth page in `web/` (GitHub Pages workflow), live verification status, re-runnable `ble::run`.
- `Transport::while_verifying` hook so transports keep serving clients during verification.
- `coex` feature for Wi-Fi verification over BLE.
- Support for all chips of `esp-hal` 1.2: esp32, esp32c2, esp32c3, esp32c5, esp32c6, esp32c61, esp32h2, esp32p4, esp32s2, esp32s3, esp32s31.
- Re-provisioning triggers: `force` option, `store::clear()`, `trigger::button_held()`, and fallback after failed connection attempts.
