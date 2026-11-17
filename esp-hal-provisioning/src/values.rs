//! A set of provisioned values.

use alloc::{string::String, vec::Vec};
use core::fmt;

use zeroize::Zeroizing;

/// Values collected for a [`Schema`](crate::Schema), keyed by field key.
///
/// Values are zeroed in memory when dropped, and the [`Debug`] implementation
/// only prints keys so secrets do not end up in logs.
#[derive(Default, Clone, PartialEq, Eq)]
pub struct Values {
    entries: Vec<(&'static str, Zeroizing<String>)>,
}

impl Values {
    /// Create an empty set of values.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Get the value for `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Set the value for `key`, replacing any previous value.
    pub fn set(&mut self, key: &'static str, value: impl Into<String>) {
        let value = Zeroizing::new(value.into());
        match self.entries.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 = value,
            None => self.entries.push((key, value)),
        }
    }

    /// Remove the value for `key`.
    pub fn remove(&mut self, key: &str) {
        self.entries.retain(|(k, _)| *k != key);
    }

    /// Iterate over `(key, value)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &str)> {
        self.entries.iter().map(|(k, v)| (*k, v.as_str()))
    }

    /// Number of stored values.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no values are stored.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl fmt::Debug for Values {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set()
            .entries(self.entries.iter().map(|(k, _)| k))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn set_get_replace_remove() {
        let mut v = Values::new();
        v.set("a", "1");
        v.set("b", "2");
        v.set("a", "3");
        assert_eq!(v.get("a"), Some("3"));
        assert_eq!(v.len(), 2);
        v.remove("a");
        assert_eq!(v.get("a"), None);
    }

    #[test]
    fn debug_hides_values() {
        let mut v = Values::new();
        v.set("password", "hunter2");
        let s = format!("{v:?}");
        assert!(s.contains("password"));
        assert!(!s.contains("hunter2"));
    }
}
