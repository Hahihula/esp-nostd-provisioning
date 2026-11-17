//! Provisioning transports and value verification.

use alloc::{string::String, vec::Vec};
use core::fmt::Debug;

use embassy_futures::select::{Either, select};

use crate::{Schema, Values};

/// One option of a [`FieldKind::Choice`](crate::FieldKind::Choice) field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// The value stored when this option is picked.
    pub value: String,
    /// Human readable label (e.g. `"MyNetwork  -48 dBm  WPA2"`).
    pub label: String,
}

/// Everything a transport needs to run one provisioning round.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// The schema to collect values for.
    pub schema: &'a Schema,
    /// Runtime options for choice fields, as `(field key, options)`.
    pub choices: &'a [(&'static str, Vec<Choice>)],
    /// Currently stored values, if re-provisioning. Transports may offer
    /// them as defaults (secrets must never be displayed).
    pub current: Option<&'a Values>,
    /// 1-based attempt number.
    pub attempt: u32,
    /// Why the previous attempt was rejected (validation or verification).
    pub last_error: Option<&'a str>,
}

impl Request<'_> {
    /// Options available for the field `key`.
    pub fn choices_for(&self, key: &str) -> &[Choice] {
        self.choices
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, c)| c.as_slice())
            .unwrap_or(&[])
    }
}

/// A way of obtaining values from the user (serial console, SD card, BLE, …).
pub trait Transport {
    /// Transport specific error.
    type Error: Debug;

    /// Collect values for `request.schema`.
    ///
    /// Called again with [`Request::last_error`] set if the returned values
    /// fail validation or verification.
    async fn collect(&mut self, request: &Request<'_>) -> Result<Values, Self::Error>;

    /// Called once the values were verified and saved.
    async fn finished(&mut self, _schema: &Schema) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Drive `verification` to completion.
    ///
    /// Transports with background work (e.g. a BLE connection or an HTTP
    /// server) override this to keep serving their clients while values are
    /// being verified, which can take several seconds (connecting to Wi-Fi).
    async fn while_verifying<F: Future>(&mut self, verification: F) -> F::Output {
        verification.await
    }
}

impl<T: Transport + ?Sized> Transport for &mut T {
    type Error = T::Error;

    async fn collect(&mut self, request: &Request<'_>) -> Result<Values, Self::Error> {
        (**self).collect(request).await
    }

    async fn finished(&mut self, schema: &Schema) -> Result<(), Self::Error> {
        (**self).finished(schema).await
    }

    async fn while_verifying<F: Future>(&mut self, verification: F) -> F::Output {
        (**self).while_verifying(verification).await
    }
}

/// Error of a combined transport `(A, B)`.
#[derive(Debug)]
pub enum EitherError<A, B> {
    /// Error of the first transport.
    First(A),
    /// Error of the second transport.
    Second(B),
}

/// Run two transports concurrently; the first one to deliver values wins.
///
/// Nest tuples to combine more: `(cli, (sdcard, ble))`.
impl<A: Transport, B: Transport> Transport for (A, B) {
    type Error = EitherError<A::Error, B::Error>;

    async fn collect(&mut self, request: &Request<'_>) -> Result<Values, Self::Error> {
        match select(self.0.collect(request), self.1.collect(request)).await {
            Either::First(r) => r.map_err(EitherError::First),
            Either::Second(r) => r.map_err(EitherError::Second),
        }
    }

    async fn finished(&mut self, schema: &Schema) -> Result<(), Self::Error> {
        self.0.finished(schema).await.map_err(EitherError::First)?;
        self.1.finished(schema).await.map_err(EitherError::Second)
    }

    async fn while_verifying<F: Future>(&mut self, verification: F) -> F::Output {
        let (a, b) = (&mut self.0, &mut self.1);
        a.while_verifying(b.while_verifying(verification)).await
    }
}

/// Reason why [`Verify::verify`] rejected a set of values. Shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyError(pub String);

impl core::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Checks that collected values actually work before they are saved
/// (e.g. by connecting to the Wi-Fi network).
pub trait Verify {
    /// Verify `values`. On error the user is asked again.
    async fn verify(&mut self, values: &Values) -> Result<(), VerifyError>;
}

impl<V: Verify + ?Sized> Verify for &mut V {
    async fn verify(&mut self, values: &Values) -> Result<(), VerifyError> {
        (**self).verify(values).await
    }
}

/// A [`Verify`] implementation accepting everything.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoVerify;

impl Verify for NoVerify {
    async fn verify(&mut self, _values: &Values) -> Result<(), VerifyError> {
        Ok(())
    }
}
