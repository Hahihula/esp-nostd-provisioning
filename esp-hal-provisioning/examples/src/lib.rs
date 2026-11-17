//! Board helpers shared by the examples: the default console and BOOT button
//! pins differ between chips.
#![no_std]

/// Create the async console used for provisioning.
///
/// On chips with a USB-Serial-JTAG peripheral the console follows the same rule
/// as `esp-println`: if a USB host is attached to the chip's own USB port it is
/// used, otherwise UART0 on the default console pins. The `usb-serial-jtag`
/// feature skips the detection and always uses USB-Serial-JTAG.
#[macro_export]
macro_rules! console {
    ($p:ident) => {{
        let (tx, rx) = $crate::uart0_pins!($p);
        let uart = esp_hal::uart::Uart::new($p.UART0, esp_hal::uart::Config::default())
            .unwrap()
            .with_tx(tx)
            .with_rx(rx)
            .into_async();
        #[cfg(any(
            feature = "esp32c3",
            feature = "esp32c5",
            feature = "esp32c6",
            feature = "esp32c61",
            feature = "esp32h2",
            feature = "esp32p4",
            feature = "esp32s3",
            feature = "esp32s31"
        ))]
        let console = $crate::Console::auto(uart, $p.USB_DEVICE);
        #[cfg(not(any(
            feature = "esp32c3",
            feature = "esp32c5",
            feature = "esp32c6",
            feature = "esp32c61",
            feature = "esp32h2",
            feature = "esp32p4",
            feature = "esp32s3",
            feature = "esp32s31"
        )))]
        let console = $crate::Console::Uart(uart);
        console
    }};
}

/// Console of the examples: UART0 or the chip's USB-Serial-JTAG port.
pub enum Console<'d> {
    /// UART0 on the default console pins.
    Uart(esp_hal::uart::Uart<'d, esp_hal::Async>),
    /// The chip's built-in USB-Serial-JTAG port.
    #[cfg(has_usb_serial_jtag)]
    UsbSerialJtag(esp_hal::usb::usb_serial_jtag::UsbSerialJtag<'d, esp_hal::Async>),
}

#[cfg(has_usb_serial_jtag)]
impl<'d> Console<'d> {
    /// Use USB-Serial-JTAG if a USB host is attached to it, UART0 otherwise.
    pub fn auto(
        uart: esp_hal::uart::Uart<'d, esp_hal::Async>,
        usb: esp_hal::peripherals::USB_DEVICE<'d>,
    ) -> Self {
        if cfg!(feature = "usb-serial-jtag") || usb_host_attached() {
            log::info!("console: USB-Serial-JTAG");
            drop(uart);
            Console::UsbSerialJtag(
                esp_hal::usb::usb_serial_jtag::UsbSerialJtag::new(usb).into_async(),
            )
        } else {
            log::info!("console: UART0");
            Console::Uart(uart)
        }
    }
}

/// A USB host sends a start-of-frame packet every millisecond; the peripheral
/// latches it in its raw interrupt status (the check `esp-println` uses).
#[cfg(has_usb_serial_jtag)]
fn usb_host_attached() -> bool {
    let regs = esp_hal::peripherals::USB_DEVICE::regs();
    let delay = esp_hal::delay::Delay::new();
    for _ in 0..20 {
        if regs.int_raw().read().sof().bit_is_set() {
            return true;
        }
        delay.delay_millis(1);
    }
    false
}

/// Error of [`Console`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsoleError(embedded_io_async::ErrorKind);

impl core::fmt::Display for ConsoleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "console error: {:?}", self.0)
    }
}

impl core::error::Error for ConsoleError {}

impl embedded_io_async::Error for ConsoleError {
    fn kind(&self) -> embedded_io_async::ErrorKind {
        self.0
    }
}

fn console_error(e: impl embedded_io_async::Error) -> ConsoleError {
    ConsoleError(e.kind())
}

impl embedded_io_async::ErrorType for Console<'_> {
    type Error = ConsoleError;
}

impl embedded_io_async::Read for Console<'_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, ConsoleError> {
        match self {
            Console::Uart(io) => embedded_io_async::Read::read(io, buf)
                .await
                .map_err(console_error),
            #[cfg(has_usb_serial_jtag)]
            Console::UsbSerialJtag(io) => embedded_io_async::Read::read(io, buf)
                .await
                .map_err(console_error),
        }
    }
}

impl embedded_io_async::Write for Console<'_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, ConsoleError> {
        match self {
            Console::Uart(io) => embedded_io_async::Write::write(io, buf)
                .await
                .map_err(console_error),
            #[cfg(has_usb_serial_jtag)]
            Console::UsbSerialJtag(io) => embedded_io_async::Write::write(io, buf)
                .await
                .map_err(console_error),
        }
    }

    async fn flush(&mut self) -> Result<(), ConsoleError> {
        match self {
            Console::Uart(io) => embedded_io_async::Write::flush(io)
                .await
                .map_err(console_error),
            #[cfg(has_usb_serial_jtag)]
            Console::UsbSerialJtag(io) => embedded_io_async::Write::flush(io)
                .await
                .map_err(console_error),
        }
    }
}

/// Default UART0 console pins `(tx, rx)` of the selected chip.
#[macro_export]
macro_rules! uart0_pins {
    ($p:ident) => {{
        #[cfg(feature = "esp32")]
        let pins = ($p.GPIO1, $p.GPIO3);
        #[cfg(feature = "esp32c2")]
        let pins = ($p.GPIO20, $p.GPIO19);
        #[cfg(feature = "esp32c3")]
        let pins = ($p.GPIO21, $p.GPIO20);
        #[cfg(feature = "esp32c5")]
        let pins = ($p.GPIO11, $p.GPIO12);
        #[cfg(feature = "esp32c6")]
        let pins = ($p.GPIO16, $p.GPIO17);
        #[cfg(feature = "esp32c61")]
        let pins = ($p.GPIO11, $p.GPIO10);
        #[cfg(feature = "esp32h2")]
        let pins = ($p.GPIO24, $p.GPIO23);
        #[cfg(feature = "esp32p4")]
        let pins = ($p.GPIO37, $p.GPIO38);
        #[cfg(feature = "esp32s2")]
        let pins = ($p.GPIO43, $p.GPIO44);
        #[cfg(feature = "esp32s3")]
        let pins = ($p.GPIO43, $p.GPIO44);
        #[cfg(feature = "esp32s31")]
        let pins = ($p.GPIO58, $p.GPIO59);
        pins
    }};
}

/// The BOOT button pin of the selected chip (active low).
///
/// The esp32s31 pin is unverified (one of its strapping pins); adjust it to your board.
#[macro_export]
macro_rules! boot_button {
    ($p:ident) => {{
        #[cfg(feature = "esp32")]
        let pin = $p.GPIO0;
        #[cfg(feature = "esp32c2")]
        let pin = $p.GPIO9;
        #[cfg(feature = "esp32c3")]
        let pin = $p.GPIO9;
        #[cfg(feature = "esp32c5")]
        let pin = $p.GPIO28;
        #[cfg(feature = "esp32c6")]
        let pin = $p.GPIO9;
        #[cfg(feature = "esp32c61")]
        let pin = $p.GPIO9;
        #[cfg(feature = "esp32h2")]
        let pin = $p.GPIO9;
        #[cfg(feature = "esp32p4")]
        let pin = $p.GPIO35;
        #[cfg(feature = "esp32s2")]
        let pin = $p.GPIO0;
        #[cfg(feature = "esp32s3")]
        let pin = $p.GPIO0;
        #[cfg(feature = "esp32s31")]
        let pin = $p.GPIO36;
        pin
    }};
}

/// SPI pins `(sck, mosi, miso, cs)` used by the SD card examples.
///
/// Chosen to avoid strapping, flash and USB pins where possible. Adjust to your wiring.
#[macro_export]
macro_rules! sd_spi_pins {
    ($p:ident) => {{
        #[cfg(feature = "esp32")]
        let pins = ($p.GPIO18, $p.GPIO23, $p.GPIO19, $p.GPIO5);
        #[cfg(feature = "esp32c2")]
        let pins = ($p.GPIO4, $p.GPIO6, $p.GPIO5, $p.GPIO7);
        #[cfg(feature = "esp32c3")]
        let pins = ($p.GPIO4, $p.GPIO6, $p.GPIO5, $p.GPIO7);
        #[cfg(feature = "esp32c5")]
        let pins = ($p.GPIO6, $p.GPIO5, $p.GPIO4, $p.GPIO8);
        #[cfg(feature = "esp32c6")]
        let pins = ($p.GPIO6, $p.GPIO7, $p.GPIO2, $p.GPIO3);
        #[cfg(feature = "esp32c61")]
        let pins = ($p.GPIO6, $p.GPIO5, $p.GPIO2, $p.GPIO1);
        #[cfg(feature = "esp32h2")]
        let pins = ($p.GPIO4, $p.GPIO5, $p.GPIO1, $p.GPIO0);
        #[cfg(feature = "esp32p4")]
        let pins = ($p.GPIO43, $p.GPIO44, $p.GPIO39, $p.GPIO42);
        #[cfg(feature = "esp32s2")]
        let pins = ($p.GPIO12, $p.GPIO11, $p.GPIO13, $p.GPIO10);
        #[cfg(feature = "esp32s3")]
        let pins = ($p.GPIO12, $p.GPIO11, $p.GPIO13, $p.GPIO10);
        #[cfg(feature = "esp32s31")]
        let pins = ($p.GPIO4, $p.GPIO5, $p.GPIO6, $p.GPIO7);
        pins
    }};
}

/// Set up the heap for the Wi-Fi examples.
///
/// Uses RAM reclaimed from the bootloader where there is enough of it; on the
/// esp32s31 that region is only ~8 KiB, so a regular heap region is added.
#[macro_export]
macro_rules! wifi_heap {
    () => {{
        #[cfg(not(feature = "esp32s31"))]
        esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
        #[cfg(feature = "esp32s31")]
        {
            esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 8176);
            esp_alloc::heap_allocator!(size: 64 * 1024);
        }
    }};
}
