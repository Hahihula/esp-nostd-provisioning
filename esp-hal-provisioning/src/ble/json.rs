//! Minimal JSON support for the BLE protocol: a small parser for client
//! requests and an escaping helper for building responses.

use alloc::{string::String, vec::Vec};
use core::fmt::Write;

/// A parsed JSON value. Numbers are kept as their source text.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    /// `null`
    Null,
    /// `true` / `false`
    Bool(bool),
    /// A number, as written.
    Number(String),
    /// A string.
    String(String),
    /// An array.
    Array(Vec<Json>),
    /// An object, in source order.
    Object(Vec<(String, Json)>),
}

impl Json {
    /// Look up `key` in an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The string value, if this is a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }
}

const MAX_DEPTH: usize = 8;
/// Maximum number of entries in one object or array.
const MAX_ENTRIES: usize = 64;
/// Maximum length of a number literal.
const MAX_NUMBER_LEN: usize = 32;

/// Parse a JSON document.
pub fn parse(input: &str) -> Result<Json, &'static str> {
    let mut p = Parser {
        s: input.as_bytes(),
        i: 0,
    };
    let value = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err("trailing characters after JSON value");
    }
    Ok(value)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while matches!(self.s.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.s.get(self.i) == Some(&b) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn literal(&mut self, lit: &str, value: Json) -> Result<Json, &'static str> {
        if self.s[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            Ok(value)
        } else {
            Err("invalid literal")
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, &'static str> {
        if depth > MAX_DEPTH {
            return Err("JSON nested too deeply");
        }
        self.ws();
        match self.s.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut entries = Vec::new();
                self.ws();
                if self.eat(b'}') {
                    return Ok(Json::Object(entries));
                }
                loop {
                    self.ws();
                    let key = self.string()?;
                    self.ws();
                    if !self.eat(b':') {
                        return Err("expected ':' in object");
                    }
                    let value = self.value(depth + 1)?;
                    if entries.len() == MAX_ENTRIES {
                        return Err("too many entries in object");
                    }
                    entries.push((key, value));
                    self.ws();
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b'}') {
                        return Ok(Json::Object(entries));
                    }
                    return Err("expected ',' or '}' in object");
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.eat(b']') {
                    return Ok(Json::Array(items));
                }
                loop {
                    if items.len() == MAX_ENTRIES {
                        return Err("too many entries in array");
                    }
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b']') {
                        return Ok(Json::Array(items));
                    }
                    return Err("expected ',' or ']' in array");
                }
            }
            Some(b'"') => Ok(Json::String(self.string()?)),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err("unexpected character"),
        }
    }

    /// `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?`
    fn number(&mut self) -> Result<Json, &'static str> {
        let start = self.i;
        self.eat(b'-');
        match self.s.get(self.i) {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err("invalid number"),
        }
        if self.eat(b'.') {
            if !matches!(self.s.get(self.i), Some(b'0'..=b'9')) {
                return Err("invalid number");
            }
            self.digits();
        }
        if self.eat(b'e') || self.eat(b'E') {
            if !self.eat(b'+') {
                self.eat(b'-');
            }
            if !matches!(self.s.get(self.i), Some(b'0'..=b'9')) {
                return Err("invalid number");
            }
            self.digits();
        }
        if self.i - start > MAX_NUMBER_LEN {
            return Err("number too long");
        }
        // Only ASCII digits and signs were consumed.
        let text = core::str::from_utf8(&self.s[start..self.i]).map_err(|_| "invalid number")?;
        Ok(Json::Number(String::from(text)))
    }

    fn digits(&mut self) {
        while matches!(self.s.get(self.i), Some(b'0'..=b'9')) {
            self.i += 1;
        }
    }

    fn hex4(&mut self) -> Result<u32, &'static str> {
        let digits = self
            .s
            .get(self.i..self.i + 4)
            .ok_or("truncated \\u escape")?;
        let text = core::str::from_utf8(digits).map_err(|_| "invalid \\u escape")?;
        self.i += 4;
        u32::from_str_radix(text, 16).map_err(|_| "invalid \\u escape")
    }

    fn string(&mut self) -> Result<String, &'static str> {
        if !self.eat(b'"') {
            return Err("expected string");
        }
        let mut out: Vec<u8> = Vec::new();
        loop {
            let b = *self.s.get(self.i).ok_or("unterminated string")?;
            self.i += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let e = *self.s.get(self.i).ok_or("unterminated string")?;
                    self.i += 1;
                    let c = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xd800..0xdc00).contains(&hi) {
                                if !(self.eat(b'\\') && self.eat(b'u')) {
                                    return Err("unpaired surrogate");
                                }
                                let lo = self.hex4()?;
                                if !(0xdc00..0xe000).contains(&lo) {
                                    return Err("unpaired surrogate");
                                }
                                0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00)
                            } else {
                                hi
                            };
                            char::from_u32(code).ok_or("invalid \\u escape")?
                        }
                        _ => return Err("invalid escape"),
                    };
                    let mut tmp = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
                }
                0x00..=0x1f => return Err("control character in string"),
                b => out.push(b),
            }
        }
        String::from_utf8(out).map_err(|_| "invalid UTF-8 in string")
    }
}

/// Append `s` to `out` as a quoted JSON string.
pub fn push_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn parses_nested_documents() {
        let j = parse(
            r#" {"op":"set","values":{"ssid":"Home","n":1883,"ok":true,"x":null},"a":[1,"b",[]]} "#,
        )
        .unwrap();
        assert_eq!(j.get("op").and_then(Json::as_str), Some("set"));
        let values = j.get("values").unwrap();
        assert_eq!(values.get("n"), Some(&Json::Number("1883".into())));
        assert_eq!(values.get("ok"), Some(&Json::Bool(true)));
        assert_eq!(values.get("x"), Some(&Json::Null));
        assert_eq!(
            j.get("a"),
            Some(&Json::Array(vec![
                Json::Number("1".into()),
                Json::String("b".into()),
                Json::Array(vec![])
            ]))
        );
    }

    #[test]
    fn string_escapes_and_unicode() {
        let j = parse(r#""a\"b\\c\/\né😀""#).unwrap();
        assert_eq!(j, Json::String("a\"b\\c/\né😀".into()));
        assert_eq!(parse(r#""ünï""#).unwrap(), Json::String("ünï".into()));
    }

    #[test]
    fn rejects_malformed_input() {
        for bad in [
            r#"{"a":}"#,
            r#"{"a" 1}"#,
            r#"[1,]"#,
            r#""unterminated"#,
            "tru",
            r#"{"a":1} x"#,
            "\"\u{1}\"",
            r#""\ud800""#,
            "1-2+3e",
            "01",
            "1.",
            "1e",
            "-",
            "123456789012345678901234567890123",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let deep = "[".repeat(20) + &"]".repeat(20);
        assert!(parse(&deep).is_err());
        let wide = alloc::format!("[{}0]", "0,".repeat(MAX_ENTRIES));
        assert!(parse(&wide).is_err());
    }

    #[test]
    fn valid_numbers() {
        for good in ["0", "-1", "1883", "1.5", "-0.25e+3", "2E-2"] {
            assert_eq!(parse(good), Ok(Json::Number(good.into())), "{good}");
        }
    }

    #[test]
    fn escaping_roundtrips() {
        let original = "quote\" back\\ nl\n tab\t ctl\u{1} é";
        let mut out = String::new();
        push_str(&mut out, original);
        assert_eq!(parse(&out).unwrap(), Json::String(original.into()));
    }
}
