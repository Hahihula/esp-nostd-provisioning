//! The provisioning file format.
//!
//! A simple INI-like text file. Each [`Schema`] has its own section named
//! after its namespace, so one file can provision several schemas:
//!
//! ```text
//! # Lines starting with '#' or ';' are comments.
//! [wifi]
//! ssid = My Network
//! password = "  spaces are kept inside quotes  "
//!
//! [mqtt]
//! host = broker.local
//! port = 1883
//! ```
//!
//! - Section names are matched case-insensitively against the namespace.
//! - Values are trimmed; wrap them in `"…"` to keep surrounding whitespace.
//!   Inside quotes `\"` and `\\` are escapes.
//! - `#` only starts a comment at the beginning of a line, so values (e.g.
//!   passwords) may contain `#`.
//! - An empty value (`password =`) means "not set".
//! - A section without any `key = value` line counts as *not filled in*.

use alloc::{format, string::String, vec::Vec};
use core::fmt;

use crate::{Request, Schema, Values};

/// Why a provisioning file was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based line number.
    pub line: usize,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

enum Line<'a> {
    Blank,
    Section(&'a str),
    Entry(&'a str, &'a str),
    Invalid,
}

fn classify(line: &str) -> Line<'_> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
        Line::Blank
    } else if let Some(name) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        Line::Section(name.trim())
    } else if let Some((key, value)) = trimmed.split_once('=') {
        Line::Entry(key.trim(), value.trim())
    } else {
        Line::Invalid
    }
}

fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l))
}

fn unquote(value: &str) -> Result<String, &'static str> {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .filter(|_| value.len() >= 2)
    else {
        return Ok(String::from(value));
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(e @ ('"' | '\\')) => out.push(e),
                _ => return Err("invalid escape in quoted value (use \\\" or \\\\)"),
            },
            '"' => return Err("unescaped '\"' inside quoted value"),
            c => out.push(c),
        }
    }
    Ok(out)
}

/// Extract the values of `schema` from `text`.
///
/// Returns `Ok(None)` if the file has no (filled in) section for the schema.
/// Field validation is left to [`provision`](crate::provision); this only
/// rejects structural problems such as unknown or duplicate keys.
pub fn parse(text: &str, schema: &Schema) -> Result<Option<Values>, ParseError> {
    let mut in_section = false;
    let mut found_entry = false;
    let mut values = Values::new();
    let mut seen: Vec<&str> = Vec::new();

    for (number, line) in lines(text) {
        let err = |message: String| ParseError {
            line: number,
            message,
        };
        match classify(line) {
            Line::Blank => {}
            Line::Section(name) => in_section = name.eq_ignore_ascii_case(schema.namespace),
            _ if !in_section => {}
            Line::Invalid => return Err(err(String::from("expected 'key = value'"))),
            Line::Entry(key, raw) => {
                let field = schema
                    .fields
                    .iter()
                    .find(|f| f.key.eq_ignore_ascii_case(key))
                    .ok_or_else(|| err(format!("unknown key '{key}' in [{}]", schema.namespace)))?;
                if seen.contains(&field.key) {
                    return Err(err(format!("duplicate key '{}'", field.key)));
                }
                seen.push(field.key);
                found_entry = true;

                let value = unquote(raw).map_err(|m| err(String::from(m)))?;
                let value = field.normalize(&value);
                if !value.is_empty() {
                    values.set(field.key, value);
                }
            }
        }
    }

    Ok(found_entry.then_some(values))
}

/// Whether `text` contains a section for `schema` (filled in or not).
pub fn has_section(text: &str, schema: &Schema) -> bool {
    lines(text).any(|(_, l)| matches!(classify(l), Line::Section(n) if n.eq_ignore_ascii_case(schema.namespace)))
}

/// Remove the section of `schema` from `text` (e.g. after it was applied).
///
/// Returns `None` if nothing but blank lines and the device's own
/// [`FILE_HEADER`] would remain; the user's comments are kept.
pub fn remove_section(text: &str, schema: &Schema) -> Option<String> {
    let mut out = String::new();
    let mut in_section = false;
    let mut meaningful = false;
    for (_, line) in lines(text) {
        if let Line::Section(name) = classify(line) {
            in_section = name.eq_ignore_ascii_case(schema.namespace);
        }
        if in_section {
            continue;
        }
        // Anything but blank lines and the header the device wrote itself
        // (including the user's own comments) keeps the file.
        if !line.trim().is_empty() && line.trim() != FILE_HEADER {
            meaningful = true;
        }
        out.push_str(line);
        out.push('\n');
    }
    meaningful.then_some(out)
}

/// First line of a provisioning file created by the device.
pub const FILE_HEADER: &str = "# Device provisioning file.";

/// A commented-out template section for `request.schema`.
///
/// The user removes the leading `#` from each key and fills in the value.
pub fn template(request: &Request<'_>) -> String {
    let schema = request.schema;
    let mut out = format!(
        "[{}]\n# {} settings: remove the leading '#' from each line below and fill in the value.\n",
        schema.namespace, schema.title
    );
    for field in schema.fields {
        out.push_str(&format!("#\n# {}", field.name));
        if !field.required {
            out.push_str(" (optional)");
        }
        if !field.description.is_empty() {
            out.push_str(&format!(": {}", field.description));
        }
        out.push('\n');
        let choices = request.choices_for(field.key);
        if !choices.is_empty() {
            out.push_str("# Available:");
            for (i, choice) in choices.iter().take(10).enumerate() {
                out.push_str(if i == 0 { " " } else { ", " });
                out.push_str(&choice.value);
            }
            out.push('\n');
        }
        out.push_str(&format!("#{} =\n", field.key));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Choice, Field};
    use alloc::vec;

    static WIFI: Schema = Schema::new(
        "wifi",
        "Wi-Fi",
        &[
            Field::text("ssid", "Network", "SSID"),
            Field::secret("password", "Password", "").optional(),
            Field::boolean("hidden", "Hidden", "").optional(),
        ],
    );

    #[test]
    fn parses_section_and_ignores_others() {
        let text = "\u{feff}# comment\r\n[mqtt]\r\nhost = x\r\n[WiFi]\r\nssid = My Net \r\npassword = p#ss=word\r\n\r\n[other]\r\nwhatever\r\n";
        let v = parse(text, &WIFI).unwrap().unwrap();
        assert_eq!(v.get("ssid"), Some("My Net"));
        assert_eq!(v.get("password"), Some("p#ss=word"));
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn quotes_keep_whitespace_and_support_escapes() {
        let v = parse("[wifi]\nssid = n\npassword = \"  a \\\"b\\\\ \"\n", &WIFI)
            .unwrap()
            .unwrap();
        assert_eq!(v.get("password"), Some("  a \"b\\ "));
        assert!(parse("[wifi]\npassword = \"a\\x\"\n", &WIFI).is_err());
    }

    #[test]
    fn empty_value_means_unset_and_bool_is_normalized() {
        let v = parse("[wifi]\nssid = n\npassword =\nhidden = Yes\n", &WIFI)
            .unwrap()
            .unwrap();
        assert_eq!(v.get("password"), None);
        assert_eq!(v.get("hidden"), Some("true"));
    }

    #[test]
    fn missing_or_unfilled_section_is_none() {
        assert_eq!(parse("[mqtt]\nhost = x\n", &WIFI).unwrap(), None);
        assert_eq!(parse("[wifi]\n#ssid =\n", &WIFI).unwrap(), None);
        assert_eq!(parse("", &WIFI).unwrap(), None);
    }

    #[test]
    fn structural_errors_have_line_numbers() {
        let e = parse("[wifi]\nssid = a\nsid = b\n", &WIFI).unwrap_err();
        assert_eq!(e.line, 3);
        assert!(e.message.contains("unknown key 'sid'"));
        let e = parse("[wifi]\nssid = a\nSSID = b\n", &WIFI).unwrap_err();
        assert!(e.message.contains("duplicate"));
        let e = parse("[wifi]\njust text\n", &WIFI).unwrap_err();
        assert_eq!(alloc::format!("{e}"), "line 2: expected 'key = value'");
    }

    #[test]
    fn remove_section_keeps_other_sections() {
        let text = "# header\n[wifi]\nssid = a\n[mqtt]\nhost = b\n";
        assert_eq!(
            remove_section(text, &WIFI).as_deref(),
            Some("# header\n[mqtt]\nhost = b\n")
        );
        // The user's comments keep the file; the device's own header does not.
        assert_eq!(
            remove_section("# c\n[wifi]\nssid = a\n", &WIFI).as_deref(),
            Some("# c\n")
        );
        let generated = alloc::format!("{FILE_HEADER}\n\n[wifi]\nssid = a\n");
        assert_eq!(remove_section(&generated, &WIFI), None);
    }

    #[test]
    fn template_roundtrip() {
        let choices = vec![(
            "ssid",
            vec![Choice {
                value: "Home".into(),
                label: "Home -40".into(),
            }],
        )];
        let req = Request {
            schema: &WIFI,
            choices: &choices,
            current: None,
            attempt: 1,
            last_error: None,
        };
        let t = template(&req);
        assert!(t.contains("# Available: Home\n"));
        assert!(t.contains("#ssid =\n"));
        assert!(has_section(&t, &WIFI));
        // An untouched template is "not filled in".
        assert_eq!(parse(&t, &WIFI).unwrap(), None);
        // Uncommenting the lines makes it parse.
        let filled = t
            .replace("#ssid =", "ssid = Home")
            .replace("#password =", "password = secret123");
        let v = parse(&filled, &WIFI).unwrap().unwrap();
        assert_eq!(v.get("ssid"), Some("Home"));
        assert_eq!(v.get("password"), Some("secret123"));
    }
}
