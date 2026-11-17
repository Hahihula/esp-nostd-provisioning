//! Internal logging macros forwarding to `log` or `defmt` (or nothing).
#![allow(unused_macros)]

macro_rules! debug {
    ($($arg:tt)*) => {{
        #[cfg(feature = "log-04")]
        ::log::debug!($($arg)*);
        #[cfg(feature = "defmt")]
        ::defmt::debug!($($arg)*);
        #[cfg(not(any(feature = "log-04", feature = "defmt")))]
        { let _ = ::core::format_args!($($arg)*); }
    }};
}

macro_rules! info {
    ($($arg:tt)*) => {{
        #[cfg(feature = "log-04")]
        ::log::info!($($arg)*);
        #[cfg(feature = "defmt")]
        ::defmt::info!($($arg)*);
        #[cfg(not(any(feature = "log-04", feature = "defmt")))]
        { let _ = ::core::format_args!($($arg)*); }
    }};
}

macro_rules! warn {
    ($($arg:tt)*) => {{
        #[cfg(feature = "log-04")]
        ::log::warn!($($arg)*);
        #[cfg(feature = "defmt")]
        ::defmt::warn!($($arg)*);
        #[cfg(not(any(feature = "log-04", feature = "defmt")))]
        { let _ = ::core::format_args!($($arg)*); }
    }};
}
