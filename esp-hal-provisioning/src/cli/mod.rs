//! Interactive provisioning over a serial console.
//!
//! [`Cli`] works with anything implementing `embedded_io_async::{Read, Write}`
//! — an async `Uart`, `UsbSerialJtag`, or a TCP socket — and renders any
//! [`Schema`]: each field is shown with its name and
//! description, choice fields list their options as a numbered menu, and
//! secrets are masked while typing.
//!
//! Press Ctrl-C at any prompt to start over.

use alloc::{format, string::String, vec::Vec};

use embedded_io_async::{Read, Write};

use crate::{Field, FieldKind, Request, Schema, Transport, Values, schema::MAX_VALUE_LEN};

mod line;
pub use line::Echo;
use line::{Line, LineReader};

/// Configuration of [`Cli`].
#[derive(Debug, Clone, Copy)]
pub struct CliConfig {
    /// How secret input is echoed. Default: `Echo::Mask('*')`.
    pub secret_echo: Echo,
    /// Ask for confirmation before submitting. Default: `true`.
    pub confirm: bool,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            secret_echo: Echo::Mask('*'),
            confirm: true,
        }
    }
}

/// Serial console provisioning transport.
pub struct Cli<IO> {
    io: IO,
    config: CliConfig,
    reader: LineReader,
}

enum Prompt {
    Value(String),
    Restart,
}

impl<IO: Read + Write> Cli<IO> {
    /// Create a CLI transport with the default configuration.
    pub fn new(io: IO) -> Self {
        Self::with_config(io, CliConfig::default())
    }

    /// Create a CLI transport with a custom configuration.
    pub fn with_config(io: IO, config: CliConfig) -> Self {
        Self {
            io,
            config,
            reader: LineReader::default(),
        }
    }

    /// Release the underlying IO.
    pub fn into_inner(self) -> IO {
        self.io
    }

    async fn print(&mut self, s: &str) -> Result<(), IO::Error> {
        self.io.write_all(s.as_bytes()).await?;
        self.io.flush().await
    }

    async fn read(&mut self, max: usize, echo: Echo) -> Result<Option<String>, IO::Error> {
        let mut buf = Vec::new();
        let line = self
            .reader
            .read_line(&mut self.io, &mut buf, max, echo)
            .await;
        let result = match line? {
            Line::Done => Some(String::from_utf8(core::mem::take(&mut buf)).unwrap_or_default()),
            Line::Cancelled => None,
        };
        // Wipe a potential secret from the scratch buffer.
        buf.iter_mut().for_each(|b| *b = 0);
        Ok(result)
    }

    async fn prompt_field(
        &mut self,
        field: &Field,
        req: &Request<'_>,
    ) -> Result<Prompt, IO::Error> {
        let choices = req.choices_for(field.key);
        let current = req.current.and_then(|c| c.get(field.key));

        let mut header = format!("\r\n{}", field.name);
        if !field.required {
            header.push_str(" (optional)");
        }
        header.push_str("\r\n");
        if !field.description.is_empty() {
            header.push_str(&format!("  {}\r\n", field.description));
        }
        match field.kind {
            FieldKind::Integer { min, max } => {
                header.push_str(&format!("  Range: {min}..={max}\r\n"))
            }
            FieldKind::Bool => header.push_str("  Answer y/n\r\n"),
            FieldKind::Choice { allow_custom, .. } => {
                if choices.is_empty() {
                    if allow_custom {
                        header.push_str("  (no options found, type a value)\r\n");
                    }
                } else {
                    for (i, choice) in choices.iter().enumerate() {
                        header.push_str(&format!("  {:>2}) {}\r\n", i + 1, choice.label));
                    }
                    header.push_str(&format!("  Enter 1-{}", choices.len()));
                    if allow_custom {
                        header.push_str(" or type a value");
                    }
                    header.push_str("\r\n");
                }
            }
            _ => {}
        }
        if let Some(current) = current {
            if field.is_secret() {
                header.push_str("  Press Enter to keep the current value\r\n");
            } else {
                header.push_str(&format!("  Press Enter to keep '{current}'\r\n"));
            }
        }
        self.print(&header).await?;

        let echo = if field.is_secret() {
            self.config.secret_echo
        } else {
            Echo::Plain
        };
        loop {
            self.print("> ").await?;
            let Some(input) = self.read(MAX_VALUE_LEN as usize, echo).await? else {
                return Ok(Prompt::Restart);
            };

            let value = if input.trim().is_empty() && current.is_some() {
                String::from(current.unwrap_or_default())
            } else if let FieldKind::Choice { allow_custom, .. } = field.kind {
                match input.trim().parse::<usize>() {
                    Ok(n) if (1..=choices.len()).contains(&n) => choices[n - 1].value.clone(),
                    _ if !allow_custom && !choices.is_empty() => {
                        self.print(&format!(
                            "  ! pick a number between 1 and {}\r\n",
                            choices.len()
                        ))
                        .await?;
                        continue;
                    }
                    _ => field.normalize(&input),
                }
            } else {
                field.normalize(&input)
            };

            match field.validate(&value) {
                Ok(()) => return Ok(Prompt::Value(value)),
                Err(e) => self.print(&format!("  ! {e}\r\n")).await?,
            }
        }
    }

    async fn confirm(
        &mut self,
        schema: &Schema,
        values: &Values,
    ) -> Result<Option<bool>, IO::Error> {
        let mut summary = String::from("\r\nSummary:\r\n");
        for field in schema.fields {
            let shown = match values.get(field.key) {
                None | Some("") => "<empty>",
                Some(_) if field.is_secret() => "********",
                Some(v) => v,
            };
            summary.push_str(&format!("  {:<20} {}\r\n", field.name, shown));
        }
        summary.push_str("Save? [Y/n] ");
        self.print(&summary).await?;
        loop {
            let Some(answer) = self.read(8, Echo::Plain).await? else {
                return Ok(None);
            };
            match answer.trim() {
                "" => return Ok(Some(true)),
                a => match crate::schema::parse_bool(a) {
                    Some(b) => return Ok(Some(b)),
                    None => self.print("Save? [Y/n] ").await?,
                },
            }
        }
    }
}

impl<IO: Read + Write> Transport for Cli<IO> {
    type Error = IO::Error;

    async fn collect(&mut self, req: &Request<'_>) -> Result<Values, Self::Error> {
        'restart: loop {
            self.print(&format!(
                "\r\n=== {} provisioning ===\r\n",
                req.schema.title
            ))
            .await?;
            if let Some(err) = req.last_error {
                self.print(&format!("! Previous attempt failed: {err}\r\n"))
                    .await?;
            }

            let mut values = Values::new();
            for field in req.schema.fields {
                match self.prompt_field(field, req).await? {
                    Prompt::Value(v) if v.is_empty() => {}
                    Prompt::Value(v) => values.set(field.key, v),
                    Prompt::Restart => continue 'restart,
                }
            }

            if self.config.confirm {
                match self.confirm(req.schema, &values).await? {
                    Some(true) => {}
                    Some(false) | None => continue 'restart,
                }
            }

            self.print("Applying...\r\n").await?;
            return Ok(values);
        }
    }

    async fn finished(&mut self, schema: &Schema) -> Result<(), Self::Error> {
        self.print(&format!("{} settings saved.\r\n", schema.title))
            .await
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{Choice, NoVerify, Options, provision, store::MemoryStore};
    use alloc::{string::ToString, vec};
    use core::convert::Infallible;
    use futures::executor::block_on;

    /// In-memory serial port. Reading past the scripted input fails the test
    /// instead of blocking forever.
    pub(crate) struct MockIo {
        input: Vec<u8>,
        pos: usize,
        out: Vec<u8>,
    }

    impl MockIo {
        pub(crate) fn new(input: &[u8]) -> Self {
            Self {
                input: input.to_vec(),
                pos: 0,
                out: Vec::new(),
            }
        }
        pub(crate) fn input_exhausted(&self) -> bool {
            self.pos >= self.input.len()
        }
        pub(crate) fn output(&self) -> String {
            String::from_utf8_lossy(&self.out).to_string()
        }
    }

    impl embedded_io_async::ErrorType for MockIo {
        type Error = Infallible;
    }

    impl Read for MockIo {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
            assert!(
                !self.input_exhausted(),
                "CLI asked for more input than scripted:\n{}",
                self.output()
            );
            // Deliver in small chunks to exercise chunk handling.
            let n = buf.len().min(3).min(self.input.len() - self.pos);
            buf[..n].copy_from_slice(&self.input[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    impl Write for MockIo {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            self.out.extend_from_slice(buf);
            Ok(buf.len())
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    static SCHEMA: Schema = Schema::new(
        "dev",
        "Device",
        &[
            Field::choice("net", "Network", "Pick a network", true),
            Field::secret("pw", "Password", "Network password")
                .optional()
                .with_len(8, 64),
            Field::integer("port", "Port", "", 1, 65535),
            Field::boolean("tls", "TLS", "Use TLS"),
        ],
    );

    fn run(input: &str, current: Option<Values>) -> (Values, String) {
        block_on(async {
            let mut store = MemoryStore::new();
            if let Some(c) = &current {
                crate::store::save(&mut store, &SCHEMA, c).await.unwrap();
            }
            let mut cli = Cli::new(MockIo::new(input.as_bytes()));
            let choices = vec![(
                "net",
                vec![
                    Choice {
                        value: "home".into(),
                        label: "home  -40 dBm".into(),
                    },
                    Choice {
                        value: "office".into(),
                        label: "office  -70 dBm".into(),
                    },
                ],
            )];
            let opts = Options {
                force: true,
                choices: &choices,
                ..Default::default()
            };
            let v = provision(&SCHEMA, &mut store, &mut cli, &mut NoVerify, &opts)
                .await
                .unwrap();
            let io = cli.into_inner();
            assert!(io.input_exhausted(), "unused input");
            (v, io.output())
        })
    }

    #[test]
    fn full_session_with_choice_secret_and_retries() {
        let (v, out) = run("2\rshort\rlongpassword\r99999\r1883\rmaybe\ry\r\r", None);
        assert_eq!(v.get("net"), Some("office"));
        assert_eq!(v.get("pw"), Some("longpassword"));
        assert_eq!(v.get("port"), Some("1883"));
        assert_eq!(v.get("tls"), Some("true"));
        assert!(out.contains(" 1) home  -40 dBm"));
        assert!(out.contains("must be at least 8 bytes"));
        assert!(out.contains("must be between 1 and 65535"));
        assert!(out.contains("answer yes or no"));
        assert!(!out.contains("longpassword"), "secret leaked to console");
        assert!(!out.contains("short\r"), "secret leaked to console");
        assert!(out.contains("Device settings saved."));
    }

    #[test]
    fn custom_choice_and_optional_secret() {
        let (v, _) = run("hidden-net\r\r80\rn\ry\r", None);
        assert_eq!(v.get("net"), Some("hidden-net"));
        assert_eq!(v.get("pw"), None);
        assert_eq!(v.get("tls"), Some("false"));
    }

    #[test]
    fn ctrl_c_and_declined_confirmation_restart() {
        let input = concat!(
            "1\r\x03",         // Ctrl-C at the password prompt
            "2\r\r80\ry\rn\r", // decline confirmation
            "1\r\r81\rn\r\r",  // accept
        );
        let (v, out) = run(input, None);
        assert_eq!(v.get("net"), Some("home"));
        assert_eq!(v.get("port"), Some("81"));
        assert_eq!(out.matches("=== Device provisioning ===").count(), 3);
    }

    #[test]
    fn enter_keeps_current_values_without_showing_secret() {
        let mut current = Values::new();
        current.set("net", "home");
        current.set("pw", "oldsecret1");
        current.set("port", "1");
        current.set("tls", "true");
        let (v, out) = run("\r\r\r\r\r", Some(current));
        assert_eq!(v.get("pw"), Some("oldsecret1"));
        assert_eq!(v.get("port"), Some("1"));
        assert!(!out.contains("oldsecret1"));
        assert!(out.contains("Press Enter to keep 'home'"));
    }
}
