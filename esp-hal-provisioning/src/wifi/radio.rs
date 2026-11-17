use alloc::{format, vec, vec::Vec};

use embassy_time::{Duration, Timer, with_timeout};
use esp_radio::wifi::{
    AuthenticationMethod, AuthenticationMethodConfig, Config, WifiController, scan::ScanConfig,
    sta::StationConfig,
};

use super::{Network, SCHEMA, Security, WifiCredentials, best_networks, network_choices};
use crate::{
    Error, Options, Store, Transport, Values, Verify, VerifyError, store, transport::Choices,
};

/// Configuration of [`provision`].
#[derive(Debug, Clone)]
pub struct WifiConfig {
    /// How many of the strongest networks to offer. Default: 10.
    pub max_networks: usize,
    /// Timeout of a single connection attempt. Default: 15 s.
    pub connect_timeout: Duration,
    /// Connection attempts with stored credentials before falling back to
    /// provisioning. Default: 3.
    pub connect_attempts: u32,
    /// Pause after a failed attempt with stored credentials, multiplied by
    /// the attempt number (2 s, 4 s, …). Default: 2 s.
    pub retry_delay: Duration,
    /// Ignore stored credentials and provision right away (re-provisioning).
    pub force: bool,
    /// Give up after this many rejected provisioning attempts. Default: retry forever.
    pub max_attempts: Option<u32>,
}

impl Default for WifiConfig {
    fn default() -> Self {
        Self {
            max_networks: 10,
            connect_timeout: Duration::from_secs(15),
            connect_attempts: 3,
            retry_delay: Duration::from_secs(2),
            force: false,
            max_attempts: None,
        }
    }
}

/// Ensure the device has working Wi-Fi credentials and is connected.
///
/// See the [module documentation](super) for the flow. On success the
/// controller is connected in station mode; set up your network stack on
/// `esp_radio::wifi::Interface::station()` as usual.
pub async fn provision<S, T>(
    controller: &mut WifiController<'_>,
    store: &mut S,
    transport: &mut T,
    config: &WifiConfig,
) -> Result<WifiCredentials, Error<S::Error, T::Error>>
where
    S: Store,
    T: Transport,
{
    let stored = if config.force {
        None
    } else {
        store::load(store, &SCHEMA).await.map_err(Error::Store)?
    };
    if let Some(creds) = stored.as_ref().and_then(WifiCredentials::from_values) {
        for attempt in 1..=config.connect_attempts {
            match connect(controller, &creds, config.connect_timeout).await {
                Ok(()) => return Ok(creds),
                Err(e) => warn!("connect attempt {} failed: {}", attempt, e.0.as_str()),
            }
            if attempt < config.connect_attempts {
                Timer::after(config.retry_delay * attempt).await;
            }
        }
        warn!("stored Wi-Fi credentials do not work, starting provisioning");
    }

    let networks = match scan(controller, config.max_networks).await {
        Ok(networks) => networks,
        Err(e) => {
            warn!("Wi-Fi scan failed: {:?}", e);
            Vec::new()
        }
    };
    let choices = vec![(super::SSID, network_choices(&networks))];

    let mut verifier = ConnectVerifier {
        controller,
        timeout: config.connect_timeout,
        max_networks: config.max_networks,
    };
    let options = Options {
        force: true,
        choices: &choices,
        max_attempts: config.max_attempts,
    };
    let values = crate::provision(&SCHEMA, store, transport, &mut verifier, &options).await?;
    Ok(WifiCredentials::from_values(&values).expect("validated by schema"))
}

/// Scan and return the strongest `max` networks (deduplicated by SSID).
pub async fn scan(
    controller: &mut WifiController<'_>,
    max: usize,
) -> Result<Vec<Network>, esp_radio::wifi::WifiError> {
    // Scanning needs station mode. This crate only ever configures station
    // mode, so a connected controller already is in it, and reconfiguring
    // would drop the connection. Otherwise (re)apply a plain station config.
    if !controller.is_connected() {
        controller.set_config(&Config::Station(StationConfig::default()))?;
    }
    let results = controller.scan_async(&ScanConfig::default()).await?;
    let networks = results.into_iter().map(|ap| Network {
        ssid: ap.ssid.as_str().into(),
        rssi: ap.signal_strength,
        security: security(ap.auth_method),
    });
    Ok(best_networks(networks, max))
}

fn security(auth: Option<AuthenticationMethod>) -> Security {
    match auth {
        None | Some(AuthenticationMethod::None) => Security::Open,
        Some(AuthenticationMethod::Wep) => Security::Wep,
        Some(
            AuthenticationMethod::Wpa
            | AuthenticationMethod::Wpa2Personal
            | AuthenticationMethod::WpaWpa2Personal,
        ) => Security::Wpa2,
        Some(
            AuthenticationMethod::Wpa3Personal
            | AuthenticationMethod::Wpa2Wpa3Personal
            | AuthenticationMethod::Wpa3ExtPsk
            | AuthenticationMethod::Wpa3ExtPskMixed,
        ) => Security::Wpa3,
        Some(_) => Security::Unsupported,
    }
}

/// Connect to the network described by `creds`, waiting at most `timeout`.
pub async fn connect(
    controller: &mut WifiController<'_>,
    creds: &WifiCredentials,
    timeout: Duration,
) -> Result<(), VerifyError> {
    let station = station_config(creds)?;
    controller
        .set_config(&Config::Station(station))
        .map_err(|e| VerifyError(format!("invalid configuration: {e:?}")))?;

    match with_timeout(timeout, controller.connect_async()).await {
        Ok(Ok(_)) => {
            info!("connected to '{}'", creds.ssid.as_str());
            Ok(())
        }
        Ok(Err(e)) => {
            let _ = controller.disconnect_async().await;
            Err(VerifyError(format!(
                "could not connect to '{}': {e}",
                creds.ssid
            )))
        }
        Err(_) => {
            let _ = controller.disconnect_async().await;
            Err(VerifyError(format!(
                "timed out connecting to '{}'",
                creds.ssid
            )))
        }
    }
}

fn station_config(creds: &WifiCredentials) -> Result<StationConfig, VerifyError> {
    let ssid = creds
        .ssid
        .as_str()
        .try_into()
        .map_err(|_| VerifyError("invalid SSID".into()))?;
    // The configured method is the *minimum* the driver accepts
    // (`threshold.authmode`), with PMF enabled. WPA/WPA2 as the minimum
    // therefore also connects to WPA2/WPA3 transition and WPA3 networks, and
    // works for stored credentials without knowing the network's security.
    let auth = if creds.password.is_empty() {
        AuthenticationMethodConfig::Open
    } else {
        AuthenticationMethodConfig::WpaWpa2Personal(
            creds
                .password
                .as_str()
                .try_into()
                .map_err(|_| VerifyError("invalid password".into()))?,
        )
    };
    Ok(StationConfig::default()
        .with_ssid(ssid)
        .with_authentication(auth))
}

/// [`Verify`] implementation that connects to the provisioned network.
///
/// On success the controller stays connected. After a failed attempt the
/// network list offered to the user is refreshed by scanning again.
pub struct ConnectVerifier<'a, 'd> {
    /// The Wi-Fi controller to connect with.
    pub controller: &'a mut WifiController<'d>,
    /// Connection timeout.
    pub timeout: Duration,
    /// How many networks to offer after rescanning.
    pub max_networks: usize,
}

impl Verify for ConnectVerifier<'_, '_> {
    async fn verify(&mut self, values: &Values) -> Result<(), VerifyError> {
        let creds = WifiCredentials::from_values(values)
            .ok_or_else(|| VerifyError("missing SSID".into()))?;
        connect(self.controller, &creds, self.timeout).await
    }

    async fn refresh_choices(&mut self, choices: &mut Choices) {
        match scan(self.controller, self.max_networks).await {
            Ok(networks) => {
                choices.retain(|(key, _)| *key != super::SSID);
                choices.push((super::SSID, network_choices(&networks)));
            }
            Err(e) => warn!("Wi-Fi rescan failed: {:?}", e),
        }
    }
}
