//! Helpers for deciding when to (re-)provision.
//!
//! Provisioning starts automatically when no valid values are stored. On top
//! of that, re-provisioning can be triggered by:
//!
//! - **API:** set [`Options::force`](crate::Options::force) (or
//!   [`WifiConfig::force`](crate::wifi::WifiConfig::force)), or erase values
//!   with [`store::clear`](crate::store::clear).
//! - **Button:** [`button_held`] at boot.
//! - **Connection failures:** [`wifi::provision`](crate::wifi::provision)
//!   falls back to provisioning after
//!   [`WifiConfig::connect_attempts`](crate::wifi::WifiConfig::connect_attempts)
//!   failed connection attempts with the stored credentials.

use embassy_time::{Duration, Instant, Timer};
use embedded_hal::digital::InputPin;

/// Returns `true` if `pin` stays in its active state for `hold`.
///
/// Returns `false` as soon as the pin is released (or reading it fails).
/// Typically called right after boot to let the user force re-provisioning by
/// holding the BOOT button:
///
/// ```rust,ignore
/// let mut boot = Input::new(peripherals.GPIO9, InputConfig::default().with_pull(Pull::Up));
/// let force = trigger::button_held(&mut boot, true, Duration::from_secs(3)).await;
/// ```
pub async fn button_held<P: InputPin>(pin: &mut P, active_low: bool, hold: Duration) -> bool {
    let is_active = |pin: &mut P| pin.is_low().map(|low| low == active_low).unwrap_or(false);

    let start = Instant::now();
    while start.elapsed() < hold {
        if !is_active(pin) {
            return false;
        }
        Timer::after_millis(20).await;
    }
    is_active(pin)
}
