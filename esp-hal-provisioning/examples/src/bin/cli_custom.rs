//! Provisioning application specific settings over the serial console.
//!
//! Defines its own schema (an MQTT broker configuration). Works on every
//! chip, including those without Wi-Fi (esp32h2, esp32p4).
//!
//! Hold the BOOT button for 3 s during start-up to re-provision.
//!
//! Run with e.g.:
//!   cargo esp32h2 --bin cli_custom
//!   cargo esp32c3 --bin cli_custom
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
use esp_hal_provisioning::{Field, NoVerify, Options, Schema, cli::Cli, provision, store, trigger};
use esp_hal_provisioning_examples::{boot_button, console};
use log::info;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

/// Everything the application needs to know about its MQTT broker.
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
    esp_alloc::heap_allocator!(size: 32 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let mut boot = Input::new(
        boot_button!(peripherals),
        InputConfig::default().with_pull(Pull::Up),
    );
    let force = trigger::button_held(&mut boot, true, Duration::from_secs(3)).await;

    let mut store = store::nvs_store(peripherals.FLASH).unwrap();
    let mut cli = Cli::new(console!(peripherals));
    let options = Options {
        force,
        ..Default::default()
    };

    let settings = provision(&MQTT, &mut store, &mut cli, &mut NoVerify, &options)
        .await
        .unwrap();

    info!(
        "MQTT broker: {}:{} (tls: {}, client id: {}, token set: {})",
        settings.get("host").unwrap_or_default(),
        settings.get("port").unwrap_or_default(),
        settings.get("tls").unwrap_or_default(),
        settings.get("client_id").unwrap_or_default(),
        settings.get("token").is_some(),
    );

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
