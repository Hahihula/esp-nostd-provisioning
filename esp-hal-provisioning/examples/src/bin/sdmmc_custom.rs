//! Provisioning custom settings from an SD card on the SDMMC host
//! (esp32, esp32s3, esp32p4, esp32s31), e.g. for an Ethernet-connected ESP32-P4.
//!
//! Put a `SETUP.TXT` like this on a FAT formatted card:
//!
//! ```text
//! [mqtt]
//! host = broker.local
//! port = 1883
//! tls = no
//! client_id = device-01
//! ```
//!
//! Without the file, a template is written to the card.
//!
//! Pins (4-bit bus):
//! - esp32 (slot 1, fixed): CLK 14, CMD 15, D0 2, D1 4, D2 12, D3 13
//! - esp32s3 (any pins): CLK 14, CMD 15, D0 2, D1 4, D2 12, D3 13
//! - esp32p4 (slot 0, fixed): CLK 43, CMD 44, D0 39, D1 40, D2 41, D3 42
//! - esp32s31 (slot 0, fixed): CLK 24, CMD 25, D0 20, D1 21, D2 22, D3 23
//!
//! On ESP32-P4 / ESP32-S31 boards the card is often powered from an on-chip
//! LDO channel (P4: 4, S31: 1) which must be enabled first; this example does
//! not do that yet.
//!
//! Run with e.g.:
//!   cargo esp32p4 --bin sdmmc_custom --features sdmmc
//!   cargo +esp esp32s3 --bin sdmmc_custom --features sdmmc
#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Delay, Duration, Timer};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    sdmmc::{Config as SdConfig, SdHostController, SlotConfig},
    timer::timg::TimerGroup,
};
use esp_hal_provisioning::{
    Field, NoVerify, Options, Schema,
    file::{
        FileTransport,
        sd::{BlockOn, SdFileSystem},
    },
    provision, store,
};
use log::info;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

static MQTT: Schema = Schema::new(
    "mqtt",
    "MQTT broker",
    &[
        Field::text(
            "host",
            "Broker host",
            "Hostname or IP address of the MQTT broker",
        ),
        Field::integer("port", "Broker port", "TCP port of the broker", 1, 65535),
        Field::boolean("tls", "Use TLS", "Connect using TLS"),
        Field::text("client_id", "Client ID", "Unique ID of this device").with_len(1, 23),
        Field::secret("token", "Access token", "Token issued by the broker").optional(),
    ],
);

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_alloc::heap_allocator!(size: 48 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let host = SdHostController::new(peripherals.SDHOST, SdConfig::default()).unwrap();

    #[cfg(any(feature = "esp32", feature = "esp32s3"))]
    let slot = host
        .slot::<1>(SlotConfig::default())
        .unwrap()
        .with_clk(peripherals.GPIO14)
        .with_cmd(peripherals.GPIO15)
        .with_data0(peripherals.GPIO2)
        .with_data1(peripherals.GPIO4)
        .with_data2(peripherals.GPIO12)
        .with_data3(peripherals.GPIO13);
    #[cfg(feature = "esp32p4")]
    let slot = host
        .slot::<0>(SlotConfig::default())
        .unwrap()
        .with_clk(peripherals.GPIO43)
        .with_cmd(peripherals.GPIO44)
        .with_data0(peripherals.GPIO39)
        .with_data1(peripherals.GPIO40)
        .with_data2(peripherals.GPIO41)
        .with_data3(peripherals.GPIO42);
    #[cfg(feature = "esp32s31")]
    let slot = host
        .slot::<0>(SlotConfig::default())
        .unwrap()
        .with_clk(peripherals.GPIO24)
        .with_cmd(peripherals.GPIO25)
        .with_data0(peripherals.GPIO20)
        .with_data1(peripherals.GPIO21)
        .with_data2(peripherals.GPIO22)
        .with_data3(peripherals.GPIO23);

    let card: sdio::DefaultBlockDevice<sdio::sd::Card, _, _> =
        sdio::BlockDevice::new(slot.into_async(), Delay, 25_000_000)
            .await
            .unwrap();

    let mut sd = FileTransport::new(SdFileSystem::new(BlockOn::new(card)));
    let mut store = store::nvs_store(peripherals.FLASH).unwrap();

    let settings = provision(
        &MQTT,
        &mut store,
        &mut sd,
        &mut NoVerify,
        &Options::default(),
    )
    .await
    .unwrap();

    info!(
        "MQTT broker: {}:{} (tls: {}, client id: {})",
        settings.get("host").unwrap_or_default(),
        settings.get("port").unwrap_or_default(),
        settings.get("tls").unwrap_or_default(),
        settings.get("client_id").unwrap_or_default(),
    );

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
