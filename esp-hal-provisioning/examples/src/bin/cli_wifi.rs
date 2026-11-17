//! Wi-Fi provisioning over the serial console.
//!
//! On first boot (or when the stored credentials stop working) the device
//! scans for networks and lets you pick one and enter the password in the
//! serial monitor. Credentials are verified by connecting before they are
//! saved to the `nvs` flash partition.
//!
//! Hold the BOOT button for 3 s during start-up to force re-provisioning.
//!
//! Run with e.g.:
//!   cargo esp32c3 --bin cli_wifi --features wifi
//!   cargo esp32c6 --bin cli_wifi --features wifi
//!   cargo +esp esp32s3 --bin cli_wifi --features wifi
#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    gpio::{Input, InputConfig, Pull},
    timer::timg::TimerGroup,
};
use esp_hal_provisioning::{
    cli::Cli,
    store, trigger,
    wifi::{self, WifiConfig},
};
use esp_hal_provisioning_examples::{boot_button, console, wifi_heap};
use esp_radio::wifi::WifiController;
use log::info;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    wifi_heap!();

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let mut boot = Input::new(
        boot_button!(peripherals),
        InputConfig::default().with_pull(Pull::Up),
    );
    let force = trigger::button_held(&mut boot, true, Duration::from_secs(3)).await;

    let mut controller = WifiController::new(peripherals.WIFI, Default::default()).unwrap();
    let mut store = store::nvs_store(peripherals.FLASH).unwrap();

    // ---- the provisioning part ----
    let config = WifiConfig {
        force,
        ..Default::default()
    };
    let creds = wifi::provision(
        &mut controller,
        &mut store,
        &mut Cli::new(console!(peripherals)),
        &config,
    )
    .await
    .unwrap();
    // -------------------------------

    info!("Connected to '{}'", creds.ssid);
    // From here on, set up `embassy-net` on `esp_radio::wifi::Interface::station()` as usual.

    loop {
        if let Ok(rssi) = controller.rssi() {
            info!("RSSI: {rssi} dBm");
        }
        Timer::after(Duration::from_secs(10)).await;
    }
}
