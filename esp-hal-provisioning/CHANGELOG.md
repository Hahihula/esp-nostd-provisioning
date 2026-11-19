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
- `Verify::refresh_choices`: the Wi-Fi verifier rescans networks after a failed attempt.
- `wifi::Security` in scan results (shown as open/WEP/WPA2/WPA3); enterprise, OWE and other networks that a password cannot unlock are no longer offered.
- `WifiConfig::retry_delay`: growing pause between connection attempts with stored credentials.
- `store::load_partial()`; still valid stored values are offered as defaults when the schema changed.
- `FileSystem::error_reason` (user-fixable read errors, such as a file that is too large, are reported to `SETUPERR.TXT`) and `SdFileSystem::on_success`.
- Compile-time checks in `Schema::new` (storage key length, duplicate keys) and `Field::with_len` (min ≤ max).

### Changed

- A tuple transport keeps running when one of its transports fails; it only fails when both do.
- After a rejected attempt (failed validation or verification) the submission's valid fields are offered as defaults on the next attempt.
- `Cli`'s error type is now `cli::CliError` (`Io` or `Eof`).
- WEP networks are no longer offered (the station configuration cannot connect to them).
- Choice fields without `allow_custom` are checked against the offered options in the core, for every transport.
- `FlashStore` uses a word-aligned scratch buffer (avoids a 4 KiB stack fallback in `esp-storage`) sized from `sequential-storage`'s item overhead.
- The file transport retries rejected content and rewrites the template after the card was removed and re-inserted.
- The BLE JSON parser enforces the JSON number grammar and limits entries per object/array; `set` messages with more values than the schema has fields are rejected.

### Fixed

- The CLI no longer turns input with invalid UTF-8 into an empty value (an open Wi-Fi network); it asks again. Length limits no longer split a character.
- `wifi::connect` disconnects after every failed attempt, not only after timeouts.
- BLE device names are truncated on character boundaries (non-ASCII names could panic).
- The CLI no longer spins when the console reports end of stream (a 0 byte read).
- The web page no longer accumulates duplicate `<datalist>` elements on re-render.
- Partition end address overflow check in `store::partition_store()`/`nvs_store()`.
