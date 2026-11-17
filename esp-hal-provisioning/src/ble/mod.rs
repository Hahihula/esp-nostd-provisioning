//! Provisioning over Bluetooth LE.
//!
//! The device advertises a GATT service (see [`protocol`]) that a web page
//! can talk to through Web Bluetooth. The page in this repository's `web/`
//! directory (served via GitHub Pages) reads the device's schema and renders
//! a form for it, so it works for Wi-Fi as well as for custom settings.
//!
//! The transport lives inside [`run`], which owns the BLE host and keeps it
//! running while your provisioning code executes:
//!
//! ```rust,ignore
//! let controller = esp_hal_provisioning::ble::controller(peripherals.BT)?;
//! let creds = ble::run(controller, &BleConfig::default(), async |ble| {
//!     wifi::provision(&mut wifi, &mut store, ble, &WifiConfig::default()).await
//! })
//! .await?;
//! ```
//!
//! Combine it with other transports as usual, e.g. `&mut (Cli::new(uart), ble)`.
//!
//! **Using BLE and Wi-Fi together** (Wi-Fi verification while connected over
//! BLE) requires the `coex` feature.
//!
//! **Security:** protocol version 1 sends values unencrypted. Anyone in radio
//! range can observe them while provisioning is in progress.
//!
//! **Browsers:** Web Bluetooth is available in Chrome, Edge and Opera on
//! desktop and Android. On iOS use the [Bluefy](https://apps.apple.com/app/bluefy-web-ble-browser/id1492822055)
//! browser.

use alloc::vec::Vec;
use core::convert::Infallible;

use embassy_futures::select::{Either, select};
use embassy_time::Timer;
use trouble_host::prelude::*;

use crate::{Request, Schema, Transport, Values};

pub mod json;
pub mod protocol;

#[cfg(feature = "esp-ble")]
mod esp;
#[cfg(feature = "esp-ble")]
pub use esp::{EspBleController, controller};

use protocol::{Assembler, ClientMessage, State};

const SERVICE: u128 = 0x4b8f0001_7a7c_4e1d_9f3b_5c2e6d1a0b9e;
const RX: u128 = 0x4b8f0002_7a7c_4e1d_9f3b_5c2e6d1a0b9e;
const TX: u128 = 0x4b8f0003_7a7c_4e1d_9f3b_5c2e6d1a0b9e;
/// GAP (5) + GATT (1) + provisioning service (1 + RX 2 + TX 3) attributes, with headroom.
const ATTRIBUTES: usize = 16;
const VALUE_CAPACITY: usize = 512;

type Value = heapless::Vec<u8, VALUE_CAPACITY>;
type Mutex = embassy_sync::blocking_mutex::raw::NoopRawMutex;
type Server<'v> = AttributeServer<'v, Mutex, Pool, ATTRIBUTES, CONNECTIONS>;

/// Configuration of [`run`].
#[derive(Debug, Clone, Copy)]
pub struct BleConfig {
    /// Advertised device name (shown in the browser's device picker).
    /// Default: `"ESP-Provision"`.
    pub name: &'static str,
    /// Random static address (little endian). Default: derived from the
    /// chip's Bluetooth MAC with `esp-ble`, a fixed address otherwise.
    pub address: Option<[u8; 6]>,
}

impl Default for BleConfig {
    fn default() -> Self {
        Self {
            name: "ESP-Provision",
            address: None,
        }
    }
}

const CONNECTIONS: usize = 1;
const L2CAP_CHANNELS: usize = 2;

type Pool = DefaultPacketPool;

/// Run the BLE host and call `f` with the provisioning transport.
///
/// The host (advertising, connection handling) keeps running for as long
/// as `f` runs, including while values are verified and saved. When `f`
/// returns, the BLE host is shut down.
pub async fn run<C, R, F>(controller: C, config: &BleConfig, f: F) -> R
where
    C: Controller,
    F: for<'t, 'p, 's, 'v> AsyncFnOnce(&'t mut BleTransport<'p, 's, 'v, C>) -> R,
{
    let address = Address::random(random_static(
        config.address.unwrap_or_else(default_address),
    ));
    let mut resources: HostResources<C, Pool, CONNECTIONS, L2CAP_CHANNELS> = HostResources::new();
    let stack = trouble_host::new(controller, &mut resources)
        .set_random_address(address)
        .build();
    let mut runner = stack.runner();
    // The attribute table is built by hand (instead of with trouble's macros,
    // which use `static` storage) so `run` can be called more than once.
    let mut rx_store = [0u8; VALUE_CAPACITY];
    let mut tx_store = [0u8; VALUE_CAPACITY];
    let name = &config.name[..config.name.len().min(22)];
    let mut table: AttributeTable<'_, Mutex, ATTRIBUTES> = AttributeTable::new();
    {
        let mut gap = table.add_service(Service::new(service::GAP));
        gap.add_characteristic_ro(characteristic::DEVICE_NAME, name);
        gap.add_characteristic_ro(characteristic::APPEARANCE, &appearance::UNKNOWN);
        gap.build();
    }
    table.add_service(Service::new(service::GATT)).build();
    let (rx, tx) = {
        let mut provisioning = table.add_service(Service::new(SERVICE));
        let rx = provisioning
            .add_characteristic(
                RX,
                [
                    CharacteristicProp::Write,
                    CharacteristicProp::WriteWithoutResponse,
                ],
                Value::new(),
                &mut rx_store,
            )
            .build();
        let tx = provisioning
            .add_characteristic(
                TX,
                [CharacteristicProp::Notify],
                Value::new(),
                &mut tx_store,
            )
            .build();
        provisioning.build();
        (rx, tx)
    };
    let server: Server<'_> = AttributeServer::new(table);

    let mut transport = BleTransport {
        stack: &stack,
        peripheral: stack.peripheral(),
        server: &server,
        rx,
        tx,
        connection: None,
        assembler: Assembler::default(),
        name: config.name,
    };

    let host = async {
        loop {
            if runner.run().await.is_err() {
                warn!("BLE host error, restarting");
                Timer::after_millis(100).await;
            }
        }
    };

    match select(host, f(&mut transport)).await {
        Either::First(never) => never,
        Either::Second(result) => result,
    }
}

/// Make `address` a valid random static address (two most significant bits set).
fn random_static(mut address: [u8; 6]) -> [u8; 6] {
    address[5] |= 0xc0;
    address
}

#[cfg(feature = "esp-ble")]
fn default_address() -> [u8; 6] {
    esp::mac_address()
}

#[cfg(not(feature = "esp-ble"))]
fn default_address() -> [u8; 6] {
    [0x01, 0x50, 0x52, 0x4f, 0x56, 0xc0]
}

/// Parse a UUID string into little-endian bytes (as used in advertising data).
fn uuid_le(uuid: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    let hex: Vec<u8> = uuid.bytes().filter(|b| *b != b'-').collect();
    for (i, pair) in hex.chunks(2).enumerate().take(16) {
        let text = core::str::from_utf8(pair).unwrap_or("00");
        out[15 - i] = u8::from_str_radix(text, 16).unwrap_or(0);
    }
    out
}

/// The BLE provisioning transport. Created by [`run`].
pub struct BleTransport<'p, 's, 'v, C: Controller> {
    stack: &'p Stack<'s, C, Pool>,
    peripheral: Peripheral<'p, C, Pool>,
    server: &'p Server<'v>,
    rx: Characteristic<Value>,
    tx: Characteristic<Value>,
    connection: Option<GattConnection<'p, 'p, Pool>>,
    assembler: Assembler,
    name: &'static str,
}

impl<'p, 's, 'v, C: Controller> BleTransport<'p, 's, 'v, C> {
    /// Whether a client is connected.
    pub fn is_connected(&self) -> bool {
        self.connection.is_some()
    }

    /// Advertise until a client connects.
    async fn connect(&mut self) {
        let service = [uuid_le(protocol::SERVICE_UUID)];
        let name = &self.name.as_bytes()[..self.name.len().min(29)];
        let mut adv_data = [0u8; 31];
        let mut scan_data = [0u8; 31];
        let adv_len = AdStructure::encode_slice(
            &[
                AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
                AdStructure::CompleteServiceUuids128(&service),
            ],
            &mut adv_data,
        )
        .unwrap_or(0);
        let scan_len =
            AdStructure::encode_slice(&[AdStructure::CompleteLocalName(name)], &mut scan_data)
                .unwrap_or(0);

        loop {
            let advertisement = Advertisement::ConnectableScannableUndirected {
                adv_data: &adv_data[..adv_len],
                scan_data: &scan_data[..scan_len],
            };
            let result = async {
                let advertiser = self
                    .peripheral
                    .advertise(&Default::default(), advertisement)
                    .await?;
                info!("BLE: advertising as '{}'", self.name);
                let connection = advertiser.accept().await?;
                connection
                    .with_attribute_server(self.server)
                    .map_err(BleHostError::BleHost)
            }
            .await;
            match result {
                Ok(connection) => {
                    info!("BLE: client connected");
                    self.assembler = Assembler::default();
                    self.connection = Some(connection);
                    return;
                }
                Err(_) => {
                    warn!("BLE: advertising failed, retrying");
                    Timer::after_secs(1).await;
                }
            }
        }
    }

    /// Wait for the next complete client message. `None` on disconnect (or
    /// when not connected).
    async fn next_message(&mut self) -> Option<Vec<u8>> {
        loop {
            let event = self.connection.as_ref()?.next().await;
            match event {
                GattConnectionEvent::Disconnected { .. } => {
                    info!("BLE: client disconnected");
                    self.connection = None;
                    self.assembler = Assembler::default();
                    return None;
                }
                GattConnectionEvent::RequestConnectionParams(request) => {
                    let _ = request.accept(None, self.stack).await;
                }
                GattConnectionEvent::Gatt { event } => {
                    let mut message = None;
                    if let GattEvent::Write(write) = &event
                        && write.handle() == self.rx.handle
                    {
                        match write.with_data(|_, data| self.assembler.push(data)) {
                            Ok(complete) => message = complete,
                            Err(_) => warn!("BLE: dropped malformed chunk"),
                        }
                    }
                    if let Ok(reply) = event.accept() {
                        reply.send().await;
                    }
                    if message.is_some() {
                        return message;
                    }
                }
                _ => {}
            }
        }
    }

    /// Send a message to the connected client (if any).
    async fn send(&mut self, message: &str) {
        let Some(connection) = self.connection.as_ref() else {
            return;
        };
        // ATT notification header is 3 bytes; the characteristic holds at most 512.
        let max_chunk = (connection.raw().att_mtu() as usize)
            .saturating_sub(3)
            .clamp(20, VALUE_CAPACITY);
        for chunk in protocol::chunks(message.as_bytes(), max_chunk) {
            if self.tx.notify_raw(connection, &chunk, false).await.is_err() {
                warn!("BLE: notification failed");
                return;
            }
        }
    }

    async fn send_status(&mut self, state: State, error: Option<&str>) {
        self.send(&protocol::status_message(state, error)).await;
    }
}

impl<C: Controller> Transport for BleTransport<'_, '_, '_, C> {
    type Error = Infallible;

    async fn collect(&mut self, request: &Request<'_>) -> Result<Values, Self::Error> {
        let mut error_reported = false;
        loop {
            if self.connection.is_none() {
                self.connect().await;
                error_reported = false;
            }
            if let Some(error) = request.last_error
                && !error_reported
            {
                self.send_status(State::Error, Some(error)).await;
                error_reported = true;
            }

            let Some(mut message) = self.next_message().await else {
                continue;
            };
            let parsed = protocol::parse_client(&message);
            message.iter_mut().for_each(|b| *b = 0);

            match parsed {
                Ok(ClientMessage::GetSchema) => self.send(&protocol::schema_message(request)).await,
                Ok(ClientMessage::Set(pairs)) => {
                    match protocol::values_from(request.schema, &pairs, request.current) {
                        Ok(values) => {
                            info!("BLE: received values for [{}]", request.schema.namespace);
                            self.send_status(State::Verifying, None).await;
                            return Ok(values);
                        }
                        Err(e) => self.send_status(State::Error, Some(&e)).await,
                    }
                }
                Err(e) => self.send_status(State::Error, Some(&e)).await,
            }
        }
    }

    async fn finished(&mut self, _schema: &Schema) -> Result<(), Self::Error> {
        self.send_status(State::Done, None).await;
        Ok(())
    }

    async fn while_verifying<F: Future>(&mut self, verification: F) -> F::Output {
        let serve = async {
            loop {
                match self.next_message().await {
                    Some(_) => self.send_status(State::Busy, None).await,
                    None => core::future::pending::<()>().await,
                }
            }
        };
        match select(verification, serve).await {
            Either::First(output) => output,
            Either::Second(never) => never,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_bytes_are_little_endian() {
        let b = uuid_le(protocol::SERVICE_UUID);
        assert_eq!(b[15], 0x4b);
        assert_eq!(b[14], 0x8f);
        assert_eq!(b[0], 0x9e);
    }

    #[test]
    fn uuid_constants_match_protocol_strings() {
        assert_eq!(uuid_le(protocol::SERVICE_UUID), SERVICE.to_le_bytes());
        assert_eq!(uuid_le(protocol::RX_UUID), RX.to_le_bytes());
        assert_eq!(uuid_le(protocol::TX_UUID), TX.to_le_bytes());
    }

    #[test]
    fn random_static_sets_top_bits() {
        assert_eq!(random_static([0; 6])[5] & 0xc0, 0xc0);
    }
}
