use alloc::{format, string::String, vec::Vec};
use core::fmt::Debug;

use crate::{
    Choice, Request, Schema, Store, Transport, Values, Verify,
    store::{load, save},
};

/// Options for [`provision`].
#[derive(Debug, Clone, Default)]
pub struct Options<'a> {
    /// Provision even if valid values are already stored (re-provisioning).
    pub force: bool,
    /// Runtime options for choice fields, as `(field key, options)`.
    pub choices: &'a [(&'static str, Vec<Choice>)],
    /// Give up after this many rejected attempts. `None` retries forever.
    pub max_attempts: Option<u32>,
}

/// Provisioning error.
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error<S, T> {
    /// The store failed.
    Store(S),
    /// The transport failed.
    Transport(T),
    /// [`Options::max_attempts`] was reached.
    TooManyAttempts,
}

impl<S: Debug, T: Debug> core::fmt::Display for Error<S, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Store(e) => write!(f, "storage error: {e:?}"),
            Self::Transport(e) => write!(f, "transport error: {e:?}"),
            Self::TooManyAttempts => f.write_str("too many failed provisioning attempts"),
        }
    }
}

impl<S: Debug, T: Debug> core::error::Error for Error<S, T> {}

/// Make sure `store` holds valid values for `schema`, provisioning them over
/// `transport` if necessary.
///
/// 1. Unless [`Options::force`] is set, stored values are returned if they
///    are complete and valid.
/// 2. Otherwise values are collected over `transport`, validated against the
///    schema and checked by `verifier`. On failure the user is asked again,
///    together with the reason.
/// 3. Accepted values are saved and returned.
pub async fn provision<S, T, V>(
    schema: &Schema,
    store: &mut S,
    transport: &mut T,
    verifier: &mut V,
    options: &Options<'_>,
) -> Result<Values, Error<S::Error, T::Error>>
where
    S: Store,
    T: Transport,
    V: Verify,
{
    let current = load(store, schema).await.map_err(Error::Store)?;
    if !options.force
        && let Some(values) = current
    {
        debug!("'{}' already provisioned", schema.namespace);
        return Ok(values);
    }

    info!("provisioning '{}'", schema.namespace);
    let mut last_error: Option<String> = None;
    let mut attempt = 0;

    loop {
        attempt += 1;
        if options.max_attempts.is_some_and(|max| attempt > max) {
            return Err(Error::TooManyAttempts);
        }

        let request = Request {
            schema,
            choices: options.choices,
            current: current.as_ref(),
            attempt,
            last_error: last_error.as_deref(),
        };
        let values = transport
            .collect(&request)
            .await
            .map_err(Error::Transport)?;

        if let Err(e) = validate_all(schema, &values) {
            warn!("provisioned values rejected");
            last_error = Some(e);
            continue;
        }

        if let Err(e) = transport.while_verifying(verifier.verify(&values)).await {
            warn!("verification failed");
            last_error = Some(e.0);
            continue;
        }

        save(store, schema, &values).await.map_err(Error::Store)?;
        transport.finished(schema).await.map_err(Error::Transport)?;
        info!("'{}' provisioned", schema.namespace);
        return Ok(values);
    }
}

/// Validate values coming from a (possibly remote) transport against the schema.
fn validate_all(schema: &Schema, values: &Values) -> Result<(), String> {
    for (key, _) in values.iter() {
        if schema.field(key).is_none() {
            return Err(format!("unknown field '{key}'"));
        }
    }
    for field in schema.fields {
        field
            .validate(values.get(field.key).unwrap_or(""))
            .map_err(|e| format!("{}: {e}", field.name))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Field, NoVerify, VerifyError, store::MemoryStore};
    use alloc::{collections::VecDeque, string::ToString};
    use futures::executor::block_on;

    static SCHEMA: Schema = Schema::new(
        "t",
        "Test",
        &[
            Field::text("name", "Name", ""),
            Field::integer("n", "Number", "", 1, 9),
        ],
    );

    /// Transport replaying a script of answers and recording the errors it was shown.
    struct Scripted {
        answers: VecDeque<Values>,
        seen_errors: Vec<Option<String>>,
        finished: bool,
    }

    impl Scripted {
        fn new(answers: &[&[(&'static str, &str)]]) -> Self {
            let answers = answers
                .iter()
                .map(|a| {
                    let mut v = Values::new();
                    for (k, val) in a.iter() {
                        v.set(k, *val);
                    }
                    v
                })
                .collect();
            Self {
                answers,
                seen_errors: Vec::new(),
                finished: false,
            }
        }
    }

    impl Transport for Scripted {
        type Error = &'static str;
        async fn collect(&mut self, req: &Request<'_>) -> Result<Values, Self::Error> {
            self.seen_errors.push(req.last_error.map(|s| s.to_string()));
            self.answers.pop_front().ok_or("script exhausted")
        }
        async fn finished(&mut self, _: &Schema) -> Result<(), Self::Error> {
            self.finished = true;
            Ok(())
        }
    }

    struct RejectFirst(bool);
    impl Verify for RejectFirst {
        async fn verify(&mut self, _: &Values) -> Result<(), VerifyError> {
            if core::mem::replace(&mut self.0, false) {
                Err(VerifyError("nope".into()))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn provisions_and_saves() {
        block_on(async {
            let mut store = MemoryStore::new();
            let mut t = Scripted::new(&[&[("name", "dev"), ("n", "3")]]);
            let v = provision(
                &SCHEMA,
                &mut store,
                &mut t,
                &mut NoVerify,
                &Options::default(),
            )
            .await
            .unwrap();
            assert_eq!(v.get("name"), Some("dev"));
            assert!(t.finished);
            assert!(load(&mut store, &SCHEMA).await.unwrap().is_some());
        });
    }

    #[test]
    fn skips_when_already_provisioned() {
        block_on(async {
            let mut store = MemoryStore::new();
            let mut v = Values::new();
            v.set("name", "dev");
            v.set("n", "3");
            save(&mut store, &SCHEMA, &v).await.unwrap();
            let mut t = Scripted::new(&[]);
            provision(
                &SCHEMA,
                &mut store,
                &mut t,
                &mut NoVerify,
                &Options::default(),
            )
            .await
            .unwrap();
            assert!(t.seen_errors.is_empty());
        });
    }

    #[test]
    fn force_reprovisions_and_passes_current() {
        block_on(async {
            let mut store = MemoryStore::new();
            let mut v = Values::new();
            v.set("name", "old");
            v.set("n", "3");
            save(&mut store, &SCHEMA, &v).await.unwrap();
            let mut t = Scripted::new(&[&[("name", "new"), ("n", "4")]]);
            let opts = Options {
                force: true,
                ..Default::default()
            };
            let v = provision(&SCHEMA, &mut store, &mut t, &mut NoVerify, &opts)
                .await
                .unwrap();
            assert_eq!(v.get("name"), Some("new"));
        });
    }

    #[test]
    fn retries_on_invalid_and_on_verify_failure() {
        block_on(async {
            let mut store = MemoryStore::new();
            let mut t = Scripted::new(&[
                &[("name", "dev"), ("n", "42")],
                &[("name", "dev"), ("n", "4")],
                &[("name", "dev"), ("n", "5")],
            ]);
            let v = provision(
                &SCHEMA,
                &mut store,
                &mut t,
                &mut RejectFirst(true),
                &Options::default(),
            )
            .await
            .unwrap();
            assert_eq!(v.get("n"), Some("5"));
            assert_eq!(t.seen_errors.len(), 3);
            assert_eq!(t.seen_errors[0], None);
            assert!(t.seen_errors[1].as_deref().unwrap().contains("Number"));
            assert_eq!(t.seen_errors[2].as_deref(), Some("nope"));
        });
    }

    /// Transport that records whether verification ran inside its hook.
    struct Hooked {
        inner: Scripted,
        hooked: u32,
    }

    impl Transport for Hooked {
        type Error = &'static str;
        async fn collect(&mut self, req: &Request<'_>) -> Result<Values, Self::Error> {
            self.inner.collect(req).await
        }
        async fn while_verifying<F: core::future::Future>(&mut self, f: F) -> F::Output {
            self.hooked += 1;
            f.await
        }
    }

    #[test]
    fn verification_runs_inside_transport_hook() {
        block_on(async {
            let mut store = MemoryStore::new();
            let mut t = Hooked {
                inner: Scripted::new(&[&[("name", "a"), ("n", "1")], &[("name", "a"), ("n", "2")]]),
                hooked: 0,
            };
            provision(
                &SCHEMA,
                &mut store,
                &mut (&mut t, NoopTransport),
                &mut RejectFirst(true),
                &Options::default(),
            )
            .await
            .unwrap();
            assert_eq!(t.hooked, 2, "hook must also run through tuple combinators");
        });
    }

    /// Transport that never delivers values.
    struct NoopTransport;
    impl Transport for NoopTransport {
        type Error = &'static str;
        async fn collect(&mut self, _: &Request<'_>) -> Result<Values, Self::Error> {
            core::future::pending().await
        }
    }

    #[test]
    fn rejects_unknown_fields() {
        let mut v = Values::new();
        v.set("name", "dev");
        v.set("n", "1");
        v.set("evil", "x");
        assert!(validate_all(&SCHEMA, &v).is_err());
    }

    #[test]
    fn max_attempts() {
        block_on(async {
            let mut store = MemoryStore::new();
            let mut t = Scripted::new(&[&[("n", "1")], &[("n", "1")]]);
            let opts = Options {
                max_attempts: Some(2),
                ..Default::default()
            };
            let r = provision(&SCHEMA, &mut store, &mut t, &mut NoVerify, &opts).await;
            assert!(matches!(r, Err(Error::TooManyAttempts)));
        });
    }
}
