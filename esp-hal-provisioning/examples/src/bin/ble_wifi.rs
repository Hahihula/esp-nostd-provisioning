//! Wi-Fi provisioning over Bluetooth LE *or* the serial console.
//!
//! Open the provisioning web page (`web/index.html`, deployed to GitHub
//! Pages) in Chrome/Edge (or Bluefy on iOS), connect to "ESP-Provision",
//! pick a network and enter the password. The page shows whether the device
//! could connect. The serial console works at the same time.
//!
//! Run with e.g.:
//!   cargo esp32c3 --bin ble_wifi --features coex
//!   cargo +esp esp32s3 --bin ble_wifi --features coex
#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::{clock::CpuClock, timer::timg::TimerGroup};
use esp_hal_provisioning::{
    ble::{self, BleConfig},
    cli::Cli,
    store,
    wifi::{self, WifiConfig},
};
use esp_hal_provisioning_examples::{console, wifi_heap};
use esp_radio::wifi::WifiController;
use log::info;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    wifi_heap!();
    // Extra heap for the BLE controller.
    esp_alloc::heap_allocator!(size: 32 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let mut controller = WifiController::new(peripherals.WIFI, Default::default()).unwrap();
    let mut store = store::nvs_store(peripherals.FLASH).unwrap();
    let mut cli = Cli::new(console!(peripherals));

    // ---- the provisioning part ----
    let creds = ble::run(
        ble::controller(peripherals.BT).unwrap(),
        &BleConfig::default(),
        async |ble| {
            let mut transports = (&mut cli, ble);
            wifi::provision(
                &mut controller,
                &mut store,
                &mut transports,
                &WifiConfig::default(),
            )
            .await
        },
    )
    .await
    .unwrap();
    // -------------------------------

    info!("Connected to '{}'", creds.ssid);

    loop {
        Timer::after(Duration::from_secs(10)).await;
    }
}
