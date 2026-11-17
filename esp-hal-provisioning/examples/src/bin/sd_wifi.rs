//! Wi-Fi provisioning from an SD card (SPI) *or* the serial console.
//!
//! Both transports run concurrently; whichever delivers credentials first
//! wins. If the card has no `SETUP.TXT`, a template is written to it: put the
//! card in a PC, fill in
//!
//! ```text
//! [wifi]
//! ssid = MyNetwork
//! password = my password
//! ```
//!
//! and re-insert it. If the credentials do not work, the reason is written
//! to `SETUPERR.TXT`. After success the `[wifi]` section is removed from the
//! card.
//!
//! Wiring: see `sd_spi_pins!` in `src/lib.rs`.
//!
//! Run with e.g.:
//!   cargo esp32c3 --bin sd_wifi --features wifi,sdcard
#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use embedded_sdmmc::SdCard;
use esp_backtrace as _;
use esp_hal::{
    Blocking,
    clock::CpuClock,
    delay::Delay,
    gpio::{Level, Output, OutputConfig},
    spi::master::{Config as SpiConfig, Spi},
    time::Rate,
    timer::timg::TimerGroup,
};
use esp_hal_provisioning::{
    cli::Cli,
    file::{FileTransport, sd::SdFileSystem},
    store,
    wifi::{self, WifiConfig},
};
use esp_hal_provisioning_examples::{console, sd_spi_pins, wifi_heap};
use esp_radio::wifi::WifiController;
use log::info;

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

type Card = SdCard<ExclusiveDevice<Spi<'static, Blocking>, Output<'static>, Delay>, Delay>;

fn spi_config(khz: u32) -> SpiConfig {
    SpiConfig::default().with_frequency(Rate::from_khz(khz))
}

/// After an error (e.g. the card was pulled out): re-initialise the card on
/// the next access, at the ≤ 400 kHz that initialisation requires.
fn reset_card(card: &Card) {
    card.mark_card_uninit();
    card.spi(|dev| dev.bus_mut().apply_config(&spi_config(400)).ok());
}

/// After a successful access the card is initialised: use a fast clock so
/// reading the file blocks the executor as briefly as possible.
fn fast_clock(card: &Card) {
    card.spi(|dev| dev.bus_mut().apply_config(&spi_config(20_000)).ok());
}

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    wifi_heap!();

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // SD card on SPI. Starts at 400 kHz for card initialisation; `fast_clock`
    // raises it afterwards.
    let (sck, mosi, miso, cs) = sd_spi_pins!(peripherals);
    let spi = Spi::new(peripherals.SPI2, spi_config(400))
        .unwrap()
        .with_sck(sck)
        .with_mosi(mosi)
        .with_miso(miso);
    let cs = Output::new(cs, Level::High, OutputConfig::default());
    let card = SdCard::new(
        ExclusiveDevice::new(spi, cs, Delay::new()).unwrap(),
        Delay::new(),
    );
    let sd = FileTransport::new(
        SdFileSystem::new(card)
            .on_error(reset_card)
            .on_success(fast_clock),
    );

    let mut controller = WifiController::new(peripherals.WIFI, Default::default()).unwrap();
    let mut store = store::nvs_store(peripherals.FLASH).unwrap();

    // Console and SD card at the same time: first one to deliver wins.
    let mut transports = (Cli::new(console!(peripherals)), sd);
    let creds = wifi::provision(
        &mut controller,
        &mut store,
        &mut transports,
        &WifiConfig::default(),
    )
    .await
    .unwrap();

    info!("Connected to '{}'", creds.ssid);

    loop {
        Timer::after(Duration::from_secs(10)).await;
    }
}
