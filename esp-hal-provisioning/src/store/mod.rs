//! Persistent storage of provisioned values.
//!
//! [`Store`] is a minimal async key/value interface. The crate ships
//! [`MemoryStore`] (RAM only, for tests and demos) and, with the
//! `flash-store` feature, [`FlashStore`] which persists values in a flash
//! partition using `sequential-storage`. With `esp-flash`, `nvs_store()`
//! opens a [`FlashStore`] on the internal flash `nvs` partition in one call.

use alloc::{collections::BTreeMap, format, string::String};
use core::fmt::Debug;

use crate::{Schema, Values};

#[cfg(feature = "flash-store")]
mod flash;
#[cfg(feature = "flash-store")]
pub use flash::{FlashStore, FlashStoreError};

#[cfg(feature = "esp-flash")]
mod esp;
#[cfg(feature = "esp-flash")]
pub use esp::{EspFlashStore, EspFlashStoreError, nvs_store, partition_store};

/// Async key/value storage for provisioned values.
///
/// Keys are `"{namespace}/{field}"` strings, values are UTF-8 strings of at
/// most [`MAX_VALUE_LEN`](crate::schema::MAX_VALUE_LEN) bytes.
pub trait Store {
    /// Error type of the underlying storage.
    type Error: Debug;

    /// Read the value stored under `key`.
    async fn read(&mut self, key: &str) -> Result<Option<String>, Self::Error>;

    /// Store `value` under `key`, replacing any previous value.
    async fn write(&mut self, key: &str, value: &str) -> Result<(), Self::Error>;

    /// Remove the value stored under `key`. Removing a missing key is not an error.
    async fn remove(&mut self, key: &str) -> Result<(), Self::Error>;
}

impl<T: Store + ?Sized> Store for &mut T {
    type Error = T::Error;

    async fn read(&mut self, key: &str) -> Result<Option<String>, Self::Error> {
        (**self).read(key).await
    }

    async fn write(&mut self, key: &str, value: &str) -> Result<(), Self::Error> {
        (**self).write(key, value).await
    }

    async fn remove(&mut self, key: &str) -> Result<(), Self::Error> {
        (**self).remove(key).await
    }
}

/// A RAM-only [`Store`]. Values are lost on reset.
///
/// For tests and demos: values (including secrets) are kept as plain
/// `String`s and are not zeroed when replaced or removed.
#[derive(Debug, Default)]
pub struct MemoryStore {
    map: BTreeMap<String, String>,
}

impl MemoryStore {
    /// Create an empty store.
    pub const fn new() -> Self {
        Self {
            map: BTreeMap::new(),
        }
    }
}

impl Store for MemoryStore {
    type Error = core::convert::Infallible;

    async fn read(&mut self, key: &str) -> Result<Option<String>, Self::Error> {
        Ok(self.map.get(key).cloned())
    }

    async fn write(&mut self, key: &str, value: &str) -> Result<(), Self::Error> {
        self.map.insert(String::from(key), String::from(value));
        Ok(())
    }

    async fn remove(&mut self, key: &str) -> Result<(), Self::Error> {
        self.map.remove(key);
        Ok(())
    }
}

fn storage_key(schema: &Schema, key: &str) -> String {
    format!("{}/{}", schema.namespace, key)
}

/// Load the values of `schema` from `store`.
///
/// Returns `None` if a required field is missing or any stored value no
/// longer passes validation (e.g. because the schema changed), meaning the
/// device needs to be (re-)provisioned.
pub async fn load<S: Store>(store: &mut S, schema: &Schema) -> Result<Option<Values>, S::Error> {
    let (values, complete) = load_partial(store, schema).await?;
    Ok(complete.then_some(values))
}

/// Load whatever valid values of `schema` are stored, and whether they are
/// complete (all required fields present and every stored value valid).
///
/// Used to offer the still valid values as defaults when re-provisioning,
/// e.g. after a field was added to the schema.
pub async fn load_partial<S: Store>(
    store: &mut S,
    schema: &Schema,
) -> Result<(Values, bool), S::Error> {
    let mut values = Values::new();
    let mut complete = true;
    for field in schema.fields {
        match store.read(&storage_key(schema, field.key)).await? {
            Some(value) if field.validate(&value).is_ok() => values.set(field.key, value),
            Some(_) => {
                warn!("stored value for '{}' is invalid", field.key);
                complete = false;
            }
            None if field.required => complete = false,
            None => {}
        }
    }
    Ok((values, complete))
}

/// Persist `values` for `schema`. Optional fields without a value (or with an
/// empty value) are removed from the store.
pub async fn save<S: Store>(
    store: &mut S,
    schema: &Schema,
    values: &Values,
) -> Result<(), S::Error> {
    for field in schema.fields {
        let key = storage_key(schema, field.key);
        match values.get(field.key) {
            Some(value) if !value.is_empty() => store.write(&key, value).await?,
            _ => store.remove(&key).await?,
        }
    }
    Ok(())
}

/// Remove all values of `schema`, forcing re-provisioning on next boot.
pub async fn clear<S: Store>(store: &mut S, schema: &Schema) -> Result<(), S::Error> {
    for field in schema.fields {
        store.remove(&storage_key(schema, field.key)).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Field;
    use futures::executor::block_on;

    static SCHEMA: Schema = Schema::new(
        "test",
        "Test",
        &[
            Field::text("name", "Name", ""),
            Field::integer("port", "Port", "", 1, 10),
            Field::secret("token", "Token", "").optional(),
        ],
    );

    #[test]
    fn save_load_roundtrip() {
        block_on(async {
            let mut store = MemoryStore::new();
            assert!(load(&mut store, &SCHEMA).await.unwrap().is_none());

            let mut v = Values::new();
            v.set("name", "dev");
            v.set("port", "5");
            save(&mut store, &SCHEMA, &v).await.unwrap();

            let loaded = load(&mut store, &SCHEMA).await.unwrap().unwrap();
            assert_eq!(loaded.get("name"), Some("dev"));
            assert_eq!(loaded.get("port"), Some("5"));
            assert_eq!(loaded.get("token"), None);
            assert_eq!(
                store.read("test/name").await.unwrap().as_deref(),
                Some("dev")
            );
        });
    }

    #[test]
    fn invalid_stored_value_requires_reprovisioning() {
        block_on(async {
            let mut store = MemoryStore::new();
            store.write("test/name", "dev").await.unwrap();
            store.write("test/port", "99").await.unwrap();
            assert!(load(&mut store, &SCHEMA).await.unwrap().is_none());
        });
    }

    #[test]
    fn partial_load_keeps_valid_values() {
        block_on(async {
            let mut store = MemoryStore::new();
            store.write("test/name", "dev").await.unwrap();
            store.write("test/port", "99").await.unwrap();
            let (values, complete) = load_partial(&mut store, &SCHEMA).await.unwrap();
            assert!(!complete);
            assert_eq!(values.get("name"), Some("dev"));
            assert_eq!(values.get("port"), None);
        });
    }

    #[test]
    fn clear_removes_everything() {
        block_on(async {
            let mut store = MemoryStore::new();
            let mut v = Values::new();
            v.set("name", "dev");
            v.set("port", "5");
            v.set("token", "t");
            save(&mut store, &SCHEMA, &v).await.unwrap();
            clear(&mut store, &SCHEMA).await.unwrap();
            assert!(store.read("test/token").await.unwrap().is_none());
            assert!(load(&mut store, &SCHEMA).await.unwrap().is_none());
        });
    }
}
