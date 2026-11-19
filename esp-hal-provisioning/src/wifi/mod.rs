//! Built-in Wi-Fi provisioning.
//!
//! [`SCHEMA`] describes Wi-Fi credentials (SSID + password). With the `wifi`
//! feature, `provision` implements the complete flow on top of
//! `esp-radio`:
//!
//! 1. Try to connect with stored credentials.
//! 2. If there are none, or connecting fails
//!    `WifiConfig::connect_attempts` times, scan and offer the strongest
//!    `WifiConfig::max_networks` networks through the transport.
//! 3. Verify the entered credentials by connecting, and only then save them.
//!
//! When `provision` returns, the controller is connected.

use alloc::{format, string::String, vec::Vec};

use zeroize::Zeroizing;

use crate::{Choice, Field, Schema, Values};

#[cfg(feature = "wifi")]
mod radio;
#[cfg(feature = "wifi")]
pub use radio::*;

/// Field key of the SSID.
pub const SSID: &str = "ssid";
/// Field key of the password.
pub const PASSWORD: &str = "password";

/// Schema of Wi-Fi credentials, stored in the `wifi` namespace.
pub static SCHEMA: Schema = Schema::new(
    "wifi",
    "Wi-Fi",
    &[
        Field::choice(SSID, "Network", "Wi-Fi network (SSID) to connect to", true).with_len(1, 32),
        Field::secret(PASSWORD, "Password", "Leave empty for open networks")
            .optional()
            .with_len(8, 64)
            .with_validator(validate_password),
    ],
);

fn validate_password(password: &str) -> Result<(), &'static str> {
    if password.len() == 64 && !password.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("a 64 character password must be a hex encoded key");
    }
    Ok(())
}

/// Provisioned Wi-Fi credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct WifiCredentials {
    /// Network name.
    pub ssid: String,
    /// Password, empty for open networks. Zeroed on drop.
    pub password: Zeroizing<String>,
}

impl WifiCredentials {
    /// Extract credentials from values provisioned for [`SCHEMA`].
    pub fn from_values(values: &Values) -> Option<Self> {
        Some(Self {
            ssid: String::from(values.get(SSID)?),
            password: Zeroizing::new(String::from(values.get(PASSWORD).unwrap_or(""))),
        })
    }

    /// Convert into [`Values`] for [`SCHEMA`].
    pub fn to_values(&self) -> Values {
        let mut values = Values::new();
        values.set(SSID, self.ssid.as_str());
        if !self.password.is_empty() {
            values.set(PASSWORD, self.password.as_str());
        }
        values
    }
}

impl core::fmt::Debug for WifiCredentials {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WifiCredentials")
            .field("ssid", &self.ssid)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Security of a scanned network, independent of the radio driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Security {
    /// No password.
    Open,
    /// WEP (legacy, broken). Not offered: WEP keys are not supported.
    Wep,
    /// WPA and/or WPA2 Personal.
    Wpa2,
    /// WPA3 Personal, possibly in WPA2/WPA3 transition mode.
    Wpa3,
    /// Not supported by this crate (enterprise, OWE, WAPI, DPP, …).
    Unsupported,
}

impl Security {
    /// Whether a password is needed.
    pub fn needs_password(self) -> bool {
        !matches!(self, Security::Open)
    }

    /// Short label shown to the user.
    pub fn label(self) -> &'static str {
        match self {
            Security::Open => "open",
            Security::Wep => "WEP",
            Security::Wpa2 => "WPA2",
            Security::Wpa3 => "WPA3",
            Security::Unsupported => "unsupported",
        }
    }
}

/// A scanned network, independent of the radio driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    /// Network name.
    pub ssid: String,
    /// Signal strength in dBm.
    pub rssi: i8,
    /// Security of the network.
    pub security: Security,
}

/// Deduplicate networks by SSID (keeping the strongest), drop hidden ones and
/// those the station configuration cannot connect to (WEP, enterprise,
/// OWE, …), sort by signal strength and keep at most `max`.
pub fn best_networks(networks: impl IntoIterator<Item = Network>, max: usize) -> Vec<Network> {
    let mut best: Vec<Network> = Vec::new();
    for network in networks {
        if network.ssid.is_empty()
            || matches!(network.security, Security::Wep | Security::Unsupported)
        {
            continue;
        }
        match best.iter_mut().find(|n| n.ssid == network.ssid) {
            Some(existing) if existing.rssi >= network.rssi => {}
            Some(existing) => *existing = network,
            None => best.push(network),
        }
    }
    best.sort_by_key(|n| core::cmp::Reverse(n.rssi));
    best.truncate(max);
    best
}

/// Turn networks into choices for the [`SSID`] field.
pub fn network_choices(networks: &[Network]) -> Vec<Choice> {
    networks
        .iter()
        .map(|n| Choice {
            value: n.ssid.clone(),
            label: format!(
                "{:<32} {:>4} dBm {} {}",
                n.ssid,
                n.rssi,
                signal_bars(n.rssi),
                n.security.label()
            ),
        })
        .collect()
}

fn signal_bars(rssi: i8) -> &'static str {
    match rssi {
        -55..=0 => "[####]",
        -67..=-56 => "[### ]",
        -78..=-68 => "[##  ]",
        _ => "[#   ]",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    fn net(ssid: &str, rssi: i8) -> Network {
        Network {
            ssid: ssid.to_string(),
            rssi,
            security: Security::Wpa2,
        }
    }

    #[test]
    fn best_networks_dedups_sorts_and_truncates() {
        let nets = [
            net("a", -80),
            net("b", -40),
            net("a", -50),
            net("", -10),
            net("c", -60),
            net("d", -90),
        ];
        let best = best_networks(nets, 3);
        let names: Vec<_> = best.iter().map(|n| (n.ssid.as_str(), n.rssi)).collect();
        assert_eq!(names, [("b", -40), ("a", -50), ("c", -60)]);
    }

    #[test]
    fn unsupported_networks_are_dropped_and_security_is_labelled() {
        let mut corp = net("corp", -30);
        corp.security = Security::Unsupported;
        let mut new = net("new", -50);
        new.security = Security::Wpa3;
        let mut old = net("old", -40);
        old.security = Security::Wep;
        let best = best_networks([corp, new, old], 10);
        assert_eq!(best.len(), 1);
        assert!(network_choices(&best)[0].label.ends_with("WPA3"));
    }

    #[test]
    fn password_rules() {
        let f = SCHEMA.field(PASSWORD).unwrap();
        assert!(f.validate("").is_ok(), "open network");
        assert!(f.validate("1234567").is_err());
        assert!(f.validate("12345678").is_ok());
        assert!(f.validate(&"a".repeat(64)).is_ok());
        assert!(f.validate(&"z".repeat(64)).is_err());
        assert!(f.validate(&"a".repeat(65)).is_err());
    }

    #[test]
    fn ssid_rules() {
        let f = SCHEMA.field(SSID).unwrap();
        assert!(f.validate("").is_err());
        assert!(f.validate(&"s".repeat(32)).is_ok());
        assert!(f.validate(&"s".repeat(33)).is_err());
    }

    #[test]
    fn credentials_roundtrip_and_debug_redacts() {
        let c = WifiCredentials {
            ssid: "home".into(),
            password: Zeroizing::new("secretpw".into()),
        };
        let back = WifiCredentials::from_values(&c.to_values()).unwrap();
        assert_eq!(back, c);
        assert!(!alloc::format!("{c:?}").contains("secretpw"));
    }
}
