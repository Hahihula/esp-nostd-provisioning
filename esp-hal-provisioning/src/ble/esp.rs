use esp_hal::{
    efuse::{InterfaceMacAddress, interface_mac_address},
    peripherals::BT,
};
use esp_radio::ble::controller::{BleConnector, BleInitError};
use trouble_host::prelude::ExternalController;

/// BLE controller on the chip's radio, ready for [`run`](super::run).
pub type EspBleController<'d> = ExternalController<BleConnector<'d>, 20>;

/// Initialise the BLE radio and wrap it as a controller for [`run`](super::run).
pub fn controller(bt: BT<'_>) -> Result<EspBleController<'_>, BleInitError> {
    Ok(ExternalController::new(BleConnector::new(
        bt,
        Default::default(),
    )?))
}

/// Bluetooth MAC address of the chip, little endian.
pub(super) fn mac_address() -> [u8; 6] {
    let mac = interface_mac_address(InterfaceMacAddress::Bluetooth);
    let mut address = [0u8; 6];
    for (dst, src) in address.iter_mut().zip(mac.as_bytes().iter().rev()) {
        *dst = *src;
    }
    address
}
