//! Provisioning from a text file (SD card, …).
//!
//! [`FileTransport`] waits for a provisioning file (by default `SETUP.TXT` in
//! the root directory, see [`format`](mod@format) for the syntax) and reads the section
//! belonging to the requested [`Schema`]:
//!
//! - **No file yet:** a commented-out template is written (unless disabled),
//!   so the user only has to fill in the values. The card is polled until
//!   a filled-in section appears; it can be removed, edited on a PC and
//!   re-inserted while the device waits.
//! - **Rejected values** (bad syntax, failed validation or verification,
//!   e.g. wrong Wi-Fi password): the reason is written to `SETUPERR.TXT` and
//!   the device waits until the file changes.
//! - **Success:** the section is removed from the file (the file is deleted
//!   when nothing else is left), so secrets do not stay on the card. See
//!   [`AfterUse`].
//!
//! The transport is storage agnostic: it works on any [`FileSystem`]. With the
//! `sdcard` feature, [`sd::SdFileSystem`] provides one for FAT formatted SD
//! cards (SPI or SDMMC).

use alloc::{format, vec::Vec};
use core::{convert::Infallible, fmt::Debug};

use embassy_time::{Duration, Timer};
use zeroize::Zeroizing;

use crate::{Request, Schema, Transport, Values};

pub mod format;
#[cfg(feature = "sdcard")]
pub mod sd;

/// Minimal async file access used by [`FileTransport`].
///
/// File names are relative to the root directory. Implementations backed by
/// FAT should use 8.3 names (e.g. `SETUP.TXT`).
pub trait FileSystem {
    /// Error of the underlying storage.
    type Error: Debug;

    /// Read the whole file. Returns `Ok(None)` if it does not exist.
    async fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Create the file, or replace its contents, with `data`.
    ///
    /// Implementations should overwrite the old contents in place where
    /// possible, since they may contain secrets.
    async fn write(&mut self, name: &str, data: &[u8]) -> Result<(), Self::Error>;

    /// Delete the file (overwriting its contents first where possible).
    /// Deleting a missing file is not an error.
    async fn remove(&mut self, name: &str) -> Result<(), Self::Error>;
}

impl<F: FileSystem + ?Sized> FileSystem for &mut F {
    type Error = F::Error;

    async fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>, Self::Error> {
        (**self).read(name).await
    }

    async fn write(&mut self, name: &str, data: &[u8]) -> Result<(), Self::Error> {
        (**self).write(name, data).await
    }

    async fn remove(&mut self, name: &str) -> Result<(), Self::Error> {
        (**self).remove(name).await
    }
}

/// What happens to the provisioning file once its values were accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterUse {
    /// Remove the applied section; delete the file if nothing else is left. (Default)
    RemoveSection,
    /// Delete the whole file.
    DeleteFile,
    /// Leave the file untouched (secrets stay on the card!).
    Keep,
}

/// Configuration of [`FileTransport`].
#[derive(Debug, Clone, Copy)]
pub struct FileConfig {
    /// Provisioning file name. Default: `SETUP.TXT`.
    pub file_name: &'static str,
    /// Where rejection reasons are written, `None` to disable. Default: `SETUPERR.TXT`.
    pub error_file: Option<&'static str>,
    /// How often to check for the file. Default: 2 s.
    pub poll_interval: Duration,
    /// Write a commented-out template if the file or section is missing. Default: `true`.
    pub write_template: bool,
    /// What to do with the file after success. Default: [`AfterUse::RemoveSection`].
    pub after_use: AfterUse,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            file_name: "SETUP.TXT",
            error_file: Some("SETUPERR.TXT"),
            poll_interval: Duration::from_secs(2),
            write_template: true,
            after_use: AfterUse::RemoveSection,
        }
    }
}

/// Provisioning transport reading values from a file. See the [module docs](self).
pub struct FileTransport<F> {
    fs: F,
    config: FileConfig,
    /// Hash of the content last handed out by `collect`.
    returned: Option<u64>,
    /// Hash of content that was rejected; ignored until the file changes.
    rejected: Option<u64>,
    template_written: bool,
}

impl<F: FileSystem> FileTransport<F> {
    /// Create a transport with the default configuration.
    pub fn new(fs: F) -> Self {
        Self::with_config(fs, FileConfig::default())
    }

    /// Create a transport with a custom configuration.
    pub fn with_config(fs: F, config: FileConfig) -> Self {
        Self {
            fs,
            config,
            returned: None,
            rejected: None,
            template_written: false,
        }
    }

    /// Release the file system.
    pub fn into_inner(self) -> F {
        self.fs
    }

    async fn report(&mut self, schema: &Schema, reason: &str) {
        warn!("provisioning file rejected: {}", reason);
        let Some(error_file) = self.config.error_file else {
            return;
        };
        let text = format!(
            "Provisioning of [{}] ({}) failed:\n{}\n\nFix {} and the device will pick it up automatically.\n",
            schema.namespace, schema.title, reason, self.config.file_name
        );
        if self.fs.write(error_file, text.as_bytes()).await.is_err() {
            warn!("could not write {}", error_file);
        }
    }

    async fn write_template(&mut self, existing: Option<&str>, request: &Request<'_>) {
        if !self.config.write_template || self.template_written {
            return;
        }
        self.template_written = true;
        let template = format::template(request);
        let text = match existing {
            Some(text) if format::has_section(text, request.schema) => return,
            Some(text) if !text.trim().is_empty() => format!("{}\n\n{template}", text.trim_end()),
            _ => format!("# Device provisioning file.\n\n{template}"),
        };
        match self.fs.write(self.config.file_name, text.as_bytes()).await {
            Ok(()) => info!("wrote provisioning template to {}", self.config.file_name),
            Err(_) => warn!("could not write provisioning template"),
        }
    }

    /// One poll: returns values if the file holds new, well-formed values.
    async fn poll(&mut self, request: &Request<'_>) -> Option<Values> {
        let bytes = match self.fs.read(self.config.file_name).await {
            Ok(Some(bytes)) => Zeroizing::new(bytes),
            Ok(None) => {
                self.write_template(None, request).await;
                return None;
            }
            Err(_) => {
                debug!("provisioning file not readable (no card?)");
                return None;
            }
        };

        let hash = fnv1a(&bytes);
        if Some(hash) == self.rejected {
            return None;
        }

        let Ok(text) = core::str::from_utf8(&bytes) else {
            self.rejected = Some(hash);
            self.report(request.schema, "the file is not valid UTF-8 text")
                .await;
            return None;
        };

        match format::parse(text, request.schema) {
            Ok(Some(values)) => {
                info!(
                    "read [{}] from {}",
                    request.schema.namespace, self.config.file_name
                );
                self.returned = Some(hash);
                Some(values)
            }
            Ok(None) => {
                self.write_template(Some(text), request).await;
                None
            }
            Err(e) => {
                self.rejected = Some(hash);
                self.report(request.schema, &format!("{}: {e}", self.config.file_name))
                    .await;
                None
            }
        }
    }
}

impl<F: FileSystem> Transport for FileTransport<F> {
    type Error = Infallible;

    async fn collect(&mut self, request: &Request<'_>) -> Result<Values, Self::Error> {
        if let Some(reason) = request.last_error {
            // The values we handed out were rejected: wait for the file to change.
            if let Some(hash) = self.returned.take() {
                self.rejected = Some(hash);
                self.report(request.schema, reason).await;
            }
        }
        loop {
            if let Some(values) = self.poll(request).await {
                return Ok(values);
            }
            Timer::after(self.config.poll_interval).await;
        }
    }

    async fn finished(&mut self, schema: &Schema) -> Result<(), Self::Error> {
        let name = self.config.file_name;
        let result = match self.config.after_use {
            AfterUse::Keep => Ok(()),
            AfterUse::DeleteFile => self.fs.remove(name).await,
            AfterUse::RemoveSection => match self.fs.read(name).await {
                Ok(Some(bytes)) => {
                    let bytes = Zeroizing::new(bytes);
                    match core::str::from_utf8(&bytes)
                        .ok()
                        .and_then(|t| format::remove_section(t, schema))
                    {
                        Some(rest) => self.fs.write(name, rest.as_bytes()).await,
                        None => self.fs.remove(name).await,
                    }
                }
                Ok(None) => Ok(()),
                Err(e) => Err(e),
            },
        };
        if result.is_err() {
            warn!("could not clean up {}", name);
        }
        if let Some(error_file) = self.config.error_file {
            let _ = self.fs.remove(error_file).await;
        }
        self.returned = None;
        self.rejected = None;
        Ok(())
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Field, NoVerify, Options, Verify, VerifyError, provision, store::MemoryStore};
    use alloc::{collections::BTreeMap, string::String};
    use core::cell::RefCell;
    use futures::executor::block_on;
    use std::rc::Rc;

    static WIFI: Schema = Schema::new(
        "wifi",
        "Wi-Fi",
        &[
            Field::text("ssid", "Network", "SSID"),
            Field::secret("password", "Password", "")
                .optional()
                .with_len(8, 64),
        ],
    );

    type Files = Rc<RefCell<BTreeMap<String, Vec<u8>>>>;
    /// `(after this many reads, new SETUP.TXT content or None to delete it)`
    type Script = Rc<RefCell<Vec<(usize, Option<&'static str>)>>>;

    /// In-memory file system. `script` simulates the user editing the card:
    /// on every read, if the next scripted content is due, it replaces the file.
    #[derive(Clone, Default)]
    struct MemFs {
        files: Files,
        reads: Rc<RefCell<usize>>,
        script: Script,
    }

    impl MemFs {
        fn file(&self, name: &str) -> Option<String> {
            self.files
                .borrow()
                .get(name)
                .map(|b| String::from_utf8(b.clone()).unwrap())
        }
        fn put(&self, name: &str, text: &str) {
            self.files
                .borrow_mut()
                .insert(name.into(), text.as_bytes().to_vec());
        }
        /// After `after_reads` reads, set `SETUP.TXT` to `text`.
        fn edit_after(&self, after_reads: usize, text: Option<&'static str>) {
            self.script.borrow_mut().push((after_reads, text));
        }
    }

    impl FileSystem for MemFs {
        type Error = Infallible;
        async fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>, Self::Error> {
            *self.reads.borrow_mut() += 1;
            let reads = *self.reads.borrow();
            let due: Vec<_> = self
                .script
                .borrow()
                .iter()
                .filter(|(n, _)| *n == reads)
                .cloned()
                .collect();
            for (_, text) in due {
                match text {
                    Some(t) => self.put("SETUP.TXT", t),
                    None => {
                        self.files.borrow_mut().remove("SETUP.TXT");
                    }
                }
            }
            assert!(reads < 100, "transport is polling forever");
            Ok(self.files.borrow().get(name).cloned())
        }
        async fn write(&mut self, name: &str, data: &[u8]) -> Result<(), Self::Error> {
            self.files.borrow_mut().insert(name.into(), data.to_vec());
            Ok(())
        }
        async fn remove(&mut self, name: &str) -> Result<(), Self::Error> {
            self.files.borrow_mut().remove(name);
            Ok(())
        }
    }

    fn fast() -> FileConfig {
        FileConfig {
            poll_interval: Duration::from_millis(1),
            ..Default::default()
        }
    }

    fn run(fs: &MemFs, config: FileConfig, verifier: &mut impl Verify) -> Values {
        block_on(async {
            let mut store = MemoryStore::new();
            let mut transport = FileTransport::with_config(fs.clone(), config);
            provision(
                &WIFI,
                &mut store,
                &mut transport,
                verifier,
                &Options::default(),
            )
            .await
            .unwrap()
        })
    }

    #[test]
    fn reads_values_and_removes_section() {
        let fs = MemFs::default();
        fs.put(
            "SETUP.TXT",
            "[wifi]\nssid = home\npassword = secret123\n\n[mqtt]\nhost = b\n",
        );
        let v = run(&fs, fast(), &mut NoVerify);
        assert_eq!(v.get("ssid"), Some("home"));
        let rest = fs.file("SETUP.TXT").unwrap();
        assert!(!rest.contains("secret123"));
        assert!(rest.contains("[mqtt]"));
    }

    #[test]
    fn deletes_file_when_nothing_is_left() {
        let fs = MemFs::default();
        fs.put("SETUP.TXT", "# my card\n[wifi]\nssid = home\n");
        run(&fs, fast(), &mut NoVerify);
        assert_eq!(fs.file("SETUP.TXT"), None);
    }

    #[test]
    fn writes_template_and_waits_for_user() {
        let fs = MemFs::default();
        // The user fills in the template on the 4th poll.
        fs.edit_after(4, Some("[wifi]\nssid = office\n"));
        let v = run(&fs, fast(), &mut NoVerify);
        assert_eq!(v.get("ssid"), Some("office"));
    }

    #[test]
    fn template_is_appended_once_to_existing_file() {
        let fs = MemFs::default();
        fs.put("SETUP.TXT", "[mqtt]\nhost = b\n");
        fs.edit_after(3, Some("[mqtt]\nhost = b\n[wifi]\nssid = x\n"));
        run(&fs, fast(), &mut NoVerify);
        assert_eq!(fs.file("SETUP.TXT").as_deref(), Some("[mqtt]\nhost = b\n"));
    }

    #[test]
    fn template_contents() {
        let fs = MemFs::default();
        let mut t = FileTransport::with_config(fs.clone(), fast());
        let req = Request {
            schema: &WIFI,
            choices: &[],
            current: None,
            attempt: 1,
            last_error: None,
        };
        assert!(block_on(t.poll(&req)).is_none());
        let text = fs.file("SETUP.TXT").unwrap();
        assert!(text.starts_with("# Device provisioning file."));
        assert!(text.contains("#ssid =") && text.contains("#password ="));
    }

    #[test]
    fn syntax_error_is_reported_until_file_changes() {
        let fs = MemFs::default();
        fs.put("SETUP.TXT", "[wifi]\nsid = home\n");
        fs.edit_after(5, Some("[wifi]\nssid = home\n"));
        let v = run(&fs, fast(), &mut NoVerify);
        assert_eq!(v.get("ssid"), Some("home"));
        // Error file was written, then cleaned up on success.
        assert_eq!(fs.file("SETUPERR.TXT"), None);
    }

    #[test]
    fn error_file_explains_the_problem() {
        let fs = MemFs::default();
        fs.put("SETUP.TXT", "[wifi]\nsid = home\n");
        let mut t = FileTransport::with_config(fs.clone(), fast());
        let req = Request {
            schema: &WIFI,
            choices: &[],
            current: None,
            attempt: 1,
            last_error: None,
        };
        assert!(block_on(t.poll(&req)).is_none());
        let err = fs.file("SETUPERR.TXT").unwrap();
        assert!(
            err.contains("SETUP.TXT: line 2: unknown key 'sid' in [wifi]"),
            "{err}"
        );
        // The same content is not re-parsed (or re-reported) on the next poll.
        fs.files.borrow_mut().remove("SETUPERR.TXT");
        assert!(block_on(t.poll(&req)).is_none());
        assert_eq!(fs.file("SETUPERR.TXT"), None);
    }

    struct WrongPasswordOnce(bool);
    impl Verify for WrongPasswordOnce {
        async fn verify(&mut self, _: &Values) -> Result<(), VerifyError> {
            if core::mem::replace(&mut self.0, false) {
                Err(VerifyError("could not connect to 'home'".into()))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn verification_failure_waits_for_a_changed_file() {
        let fs = MemFs::default();
        fs.put("SETUP.TXT", "[wifi]\nssid = home\npassword = wrongpass\n");
        fs.edit_after(4, Some("[wifi]\nssid = home\npassword = rightpass\n"));
        let v = run(&fs, fast(), &mut WrongPasswordOnce(true));
        assert_eq!(v.get("password"), Some("rightpass"));
    }

    #[test]
    fn validation_failure_is_reported() {
        let fs = MemFs::default();
        fs.put("SETUP.TXT", "[wifi]\nssid = home\npassword = short\n");
        block_on(async {
            let mut store = MemoryStore::new();
            let mut t = FileTransport::with_config(fs.clone(), fast());
            let opts = Options {
                max_attempts: Some(2),
                ..Default::default()
            };
            // Attempt 1 returns the file, validation fails, attempt 2 reports it and waits;
            // the script then provides a fixed file.
            fs.edit_after(3, Some("[wifi]\nssid = home\npassword = longenough\n"));
            let v = provision(&WIFI, &mut store, &mut t, &mut NoVerify, &opts)
                .await
                .unwrap();
            assert_eq!(v.get("password"), Some("longenough"));
        });
    }

    #[test]
    fn keep_and_delete_policies() {
        let fs = MemFs::default();
        fs.put("SETUP.TXT", "[wifi]\nssid = home\n[mqtt]\nhost = b\n");
        run(
            &fs,
            FileConfig {
                after_use: AfterUse::Keep,
                ..fast()
            },
            &mut NoVerify,
        );
        assert!(fs.file("SETUP.TXT").unwrap().contains("ssid = home"));
        run(
            &fs,
            FileConfig {
                after_use: AfterUse::DeleteFile,
                ..fast()
            },
            &mut NoVerify,
        );
        assert_eq!(fs.file("SETUP.TXT"), None);
    }

    #[test]
    fn missing_card_errors_are_retried() {
        struct Flaky(usize, MemFs);
        impl FileSystem for Flaky {
            type Error = &'static str;
            async fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>, Self::Error> {
                self.0 += 1;
                if self.0 < 3 {
                    Err("no card")
                } else {
                    Ok(self.1.read(name).await.unwrap())
                }
            }
            async fn write(&mut self, n: &str, d: &[u8]) -> Result<(), Self::Error> {
                self.1.write(n, d).await.unwrap();
                Ok(())
            }
            async fn remove(&mut self, n: &str) -> Result<(), Self::Error> {
                self.1.remove(n).await.unwrap();
                Ok(())
            }
        }
        let fs = MemFs::default();
        fs.put("SETUP.TXT", "[wifi]\nssid = home\n");
        block_on(async {
            let mut store = MemoryStore::new();
            let mut t = FileTransport::with_config(Flaky(0, fs.clone()), fast());
            provision(
                &WIFI,
                &mut store,
                &mut t,
                &mut NoVerify,
                &Options::default(),
            )
            .await
            .unwrap();
        });
    }
}
