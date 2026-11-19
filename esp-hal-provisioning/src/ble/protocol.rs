//! The BLE provisioning protocol (version 1).
//!
//! The device exposes one GATT service with two characteristics, similar to a
//! serial link:
//!
//! | Characteristic | UUID | Properties |
//! |---|---|---|
//! | Service | [`SERVICE_UUID`] | |
//! | RX (client → device) | [`RX_UUID`] | write, write without response |
//! | TX (device → client) | [`TX_UUID`] | notify |
//!
//! ## Framing
//!
//! Messages are UTF-8 JSON documents split into chunks that fit the ATT MTU.
//! Every chunk starts with one header byte:
//!
//! - bit 7: last chunk of the message,
//! - bit 6: first chunk of the message,
//! - bits 0–5: sequence number, 0 for the first chunk, wrapping at 64.
//!
//! A message is at most [`MAX_MESSAGE_LEN`] bytes.
//!
//! ## Messages
//!
//! Client → device:
//!
//! - `{"op":"get_schema"}` — the device answers with a `schema` message.
//! - `{"op":"set","values":{"ssid":"Home","password":"secret"}}` — submit
//!   values. String, number and boolean values are accepted.
//!
//! Device → client:
//!
//! - `{"op":"schema","proto":1,"namespace":…,"title":…,"attempt":…,
//!   "error":…,"fields":[…]}` — see [`schema_message`].
//! - `{"op":"status","state":S,"error":…}` where `S` is one of
//!   `waiting` (ready for values), `verifying`, `error` (values rejected,
//!   `error` holds the reason; send new values), `done` or `busy`.

use alloc::{format, string::String, vec::Vec};

use super::json::{self, Json};
use crate::{FieldKind, Request, Schema, Values};

/// Protocol version announced in the schema message.
pub const PROTOCOL_VERSION: u32 = 1;
/// UUID of the provisioning GATT service.
pub const SERVICE_UUID: &str = "4b8f0001-7a7c-4e1d-9f3b-5c2e6d1a0b9e";
/// UUID of the RX characteristic (client writes).
pub const RX_UUID: &str = "4b8f0002-7a7c-4e1d-9f3b-5c2e6d1a0b9e";
/// UUID of the TX characteristic (device notifies).
pub const TX_UUID: &str = "4b8f0003-7a7c-4e1d-9f3b-5c2e6d1a0b9e";
/// Largest message accepted or sent.
pub const MAX_MESSAGE_LEN: usize = 8 * 1024;

const FINAL: u8 = 0x80;
const FIRST: u8 = 0x40;
const SEQ_MASK: u8 = 0x3f;

/// Framing error while reassembling a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FrameError {
    /// A chunk was lost or duplicated.
    OutOfSequence,
    /// The message exceeds [`MAX_MESSAGE_LEN`].
    TooLong,
    /// An empty write.
    Empty,
}

/// Reassembles chunks into messages.
#[derive(Debug, Default)]
pub struct Assembler {
    buffer: Vec<u8>,
    next_seq: u8,
    active: bool,
}

impl Assembler {
    /// Feed one chunk. Returns the message once its last chunk arrived.
    ///
    /// On error the partial message is discarded; the next chunk marked as
    /// first starts a new message.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Option<Vec<u8>>, FrameError> {
        let (&header, payload) = chunk.split_first().ok_or(FrameError::Empty)?;
        let seq = header & SEQ_MASK;
        if header & FIRST != 0 {
            self.reset();
            self.active = true;
        }
        if !self.active || seq != self.next_seq {
            self.reset();
            return Err(FrameError::OutOfSequence);
        }
        if self.buffer.len() + payload.len() > MAX_MESSAGE_LEN {
            self.reset();
            return Err(FrameError::TooLong);
        }
        self.buffer.extend_from_slice(payload);
        self.next_seq = (self.next_seq + 1) & SEQ_MASK;
        if header & FINAL != 0 {
            self.next_seq = 0;
            self.active = false;
            return Ok(Some(core::mem::take(&mut self.buffer)));
        }
        Ok(None)
    }

    fn reset(&mut self) {
        self.buffer.iter_mut().for_each(|b| *b = 0);
        self.buffer.clear();
        self.next_seq = 0;
        self.active = false;
    }
}

/// Split `message` into chunks of at most `max_chunk` bytes (header included).
pub fn chunks(message: &[u8], max_chunk: usize) -> impl Iterator<Item = Vec<u8>> + '_ {
    let payload = max_chunk.max(2) - 1;
    let count = message.len().div_ceil(payload).max(1);
    (0..count).map(move |i| {
        let part =
            &message[(i * payload).min(message.len())..((i + 1) * payload).min(message.len())];
        let mut header = (i as u8) & SEQ_MASK;
        if i == 0 {
            header |= FIRST;
        }
        if i + 1 == count {
            header |= FINAL;
        }
        let mut chunk = Vec::with_capacity(part.len() + 1);
        chunk.push(header);
        chunk.extend_from_slice(part);
        chunk
    })
}

/// A request from the client.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClientMessage {
    /// Send the schema.
    GetSchema,
    /// Submitted values as `(key, value)` pairs.
    Set(Vec<(String, String)>),
}

/// Parse a client message.
pub fn parse_client(message: &[u8]) -> Result<ClientMessage, String> {
    let text = core::str::from_utf8(message).map_err(|_| String::from("message is not UTF-8"))?;
    let doc = json::parse(text).map_err(String::from)?;
    match doc.get("op").and_then(Json::as_str) {
        Some("get_schema") => Ok(ClientMessage::GetSchema),
        Some("set") => {
            let Some(Json::Object(entries)) = doc.get("values") else {
                return Err(String::from("'set' needs a 'values' object"));
            };
            let mut pairs = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                let value = match value {
                    Json::String(s) => s.clone(),
                    Json::Number(n) => n.clone(),
                    Json::Bool(b) => String::from(if *b { "true" } else { "false" }),
                    Json::Null => String::new(),
                    _ => {
                        return Err(format!(
                            "value of '{key}' must be a string, number or boolean"
                        ));
                    }
                };
                pairs.push((key.clone(), value));
            }
            Ok(ClientMessage::Set(pairs))
        }
        Some(op) => Err(format!("unknown op '{op}'")),
        None => Err(String::from("missing 'op'")),
    }
}

/// Map submitted pairs onto `schema` (normalizing values). Empty values are
/// treated as "not set". Secret fields that are *missing* from the
/// submission keep their `current` value, so a client can update other
/// fields without knowing the stored secret. Field validation is left to the
/// provisioning core.
pub fn values_from(
    schema: &Schema,
    pairs: &[(String, String)],
    current: Option<&Values>,
) -> Result<Values, String> {
    if pairs.len() > schema.fields.len() {
        return Err(String::from("more values than the schema has fields"));
    }
    let mut values = Values::new();
    for field in schema.fields.iter().filter(|f| f.is_secret()) {
        if !pairs.iter().any(|(k, _)| k == field.key)
            && let Some(secret) = current.and_then(|c| c.get(field.key))
        {
            values.set(field.key, secret);
        }
    }
    for (key, value) in pairs {
        let field = schema
            .field(key)
            .ok_or_else(|| format!("unknown field '{key}'"))?;
        let value = field.normalize(value);
        if !value.is_empty() {
            values.set(field.key, value);
        }
    }
    Ok(values)
}

/// Status reported to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum State {
    /// Ready to receive values.
    Waiting,
    /// Values received, being verified.
    Verifying,
    /// Values rejected; see the error.
    Error,
    /// Values saved, provisioning finished.
    Done,
    /// A request arrived while the device was busy verifying.
    Busy,
}

/// Build a `status` message.
pub fn status_message(state: State, error: Option<&str>) -> String {
    let state = match state {
        State::Waiting => "waiting",
        State::Verifying => "verifying",
        State::Error => "error",
        State::Done => "done",
        State::Busy => "busy",
    };
    let mut out = format!("{{\"op\":\"status\",\"state\":\"{state}\",\"error\":");
    match error {
        Some(e) => json::push_str(&mut out, e),
        None => out.push_str("null"),
    }
    out.push('}');
    out
}

/// Build the `schema` message for `request`.
///
/// Each field has `key`, `name`, `description`, `kind` (`text`, `secret`,
/// `integer`, `bool` or `choice`) and `required`; text-like kinds add
/// `min_len`/`max_len`, integers `min`/`max`, choices `allow_custom` and
/// `options` (`[{"value","label"}]`). Non-secret current values are sent as
/// `current`; for secrets only `has_value` is sent.
pub fn schema_message(request: &Request<'_>) -> String {
    let schema = request.schema;
    let mut out = format!("{{\"op\":\"schema\",\"proto\":{PROTOCOL_VERSION},\"namespace\":");
    json::push_str(&mut out, schema.namespace);
    out.push_str(",\"title\":");
    json::push_str(&mut out, schema.title);
    out.push_str(&format!(",\"attempt\":{},\"error\":", request.attempt));
    match request.last_error {
        Some(e) => json::push_str(&mut out, e),
        None => out.push_str("null"),
    }
    out.push_str(",\"fields\":[");
    for (i, field) in schema.fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"key\":");
        json::push_str(&mut out, field.key);
        out.push_str(",\"name\":");
        json::push_str(&mut out, field.name);
        out.push_str(",\"description\":");
        json::push_str(&mut out, field.description);
        out.push_str(&format!(",\"required\":{}", field.required));
        match field.kind {
            FieldKind::Text { min_len, max_len } => out.push_str(&format!(
                ",\"kind\":\"text\",\"min_len\":{min_len},\"max_len\":{max_len}"
            )),
            FieldKind::Secret { min_len, max_len } => out.push_str(&format!(
                ",\"kind\":\"secret\",\"min_len\":{min_len},\"max_len\":{max_len}"
            )),
            FieldKind::Integer { min, max } => out.push_str(&format!(
                ",\"kind\":\"integer\",\"min\":{min},\"max\":{max}"
            )),
            FieldKind::Bool => out.push_str(",\"kind\":\"bool\""),
            FieldKind::Choice {
                allow_custom,
                min_len,
                max_len,
            } => {
                out.push_str(&format!(
                    ",\"kind\":\"choice\",\"allow_custom\":{allow_custom},\"min_len\":{min_len},\"max_len\":{max_len},\"options\":["
                ));
                for (j, choice) in request.choices_for(field.key).iter().enumerate() {
                    if j > 0 {
                        out.push(',');
                    }
                    out.push_str("{\"value\":");
                    json::push_str(&mut out, &choice.value);
                    out.push_str(",\"label\":");
                    json::push_str(&mut out, &choice.label);
                    out.push('}');
                }
                out.push(']');
            }
        }
        match request.current.and_then(|c| c.get(field.key)) {
            Some(_) if field.is_secret() => out.push_str(",\"has_value\":true"),
            Some(value) => {
                out.push_str(",\"current\":");
                json::push_str(&mut out, value);
            }
            None => {}
        }
        out.push('}');
    }
    out.push_str("]}");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Choice, Field};
    use alloc::vec;

    static SCHEMA: Schema = Schema::new(
        "wifi",
        "Wi-Fi \"home\"",
        &[
            Field::choice("ssid", "Network", "SSID", true).with_len(1, 32),
            Field::secret("password", "Password", "").optional(),
            Field::integer("port", "Port", "", 1, 65535).optional(),
            Field::boolean("tls", "TLS", "").optional(),
        ],
    );

    #[test]
    fn framing_roundtrip_across_mtus() {
        let message: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        // mtu 2 => 1000 chunks, so the 6-bit sequence number wraps many times.
        for mtu in [2, 3, 20, 23, 185, 244, 2000] {
            let mut a = Assembler::default();
            let mut out = None;
            let parts: Vec<_> = chunks(&message, mtu).collect();
            for (i, chunk) in parts.iter().enumerate() {
                assert!(chunk.len() <= mtu.max(2));
                let r = a.push(chunk).unwrap();
                assert_eq!(r.is_some(), i + 1 == parts.len());
                out = out.or(r);
            }
            assert_eq!(out.unwrap(), message, "mtu {mtu}");
        }
    }

    #[test]
    fn empty_message_is_one_final_chunk() {
        let parts: Vec<_> = chunks(&[], 20).collect();
        assert_eq!(parts, vec![vec![FIRST | FINAL]]);
        assert_eq!(Assembler::default().push(&parts[0]).unwrap(), Some(vec![]));
    }

    #[test]
    fn lost_chunk_is_detected_and_recovered() {
        let message = [7u8; 100];
        let parts: Vec<_> = chunks(&message, 21).collect();
        let mut a = Assembler::default();
        a.push(&parts[0]).unwrap();
        assert_eq!(a.push(&parts[2]), Err(FrameError::OutOfSequence));
        // Continuation chunks without a first chunk are rejected.
        assert_eq!(a.push(&parts[1]), Err(FrameError::OutOfSequence));
        // A new message starting with a first chunk works.
        let mut out = None;
        for p in &parts {
            out = out.or(a.push(p).unwrap());
        }
        assert_eq!(out.unwrap(), message);
        assert_eq!(a.push(&[]), Err(FrameError::Empty));
    }

    #[test]
    fn oversized_message_is_rejected() {
        let big = vec![0u8; MAX_MESSAGE_LEN + 1];
        let mut a = Assembler::default();
        let result: Result<Vec<_>, _> = chunks(&big, 244).map(|c| a.push(&c)).collect();
        assert_eq!(result, Err(FrameError::TooLong));
    }

    #[test]
    fn parses_client_messages() {
        assert_eq!(
            parse_client(br#"{"op":"get_schema"}"#),
            Ok(ClientMessage::GetSchema)
        );
        assert_eq!(
            parse_client(
                br#"{"op":"set","values":{"ssid":"Home","port":1883,"tls":false,"password":null}}"#
            ),
            Ok(ClientMessage::Set(vec![
                ("ssid".into(), "Home".into()),
                ("port".into(), "1883".into()),
                ("tls".into(), "false".into()),
                ("password".into(), "".into()),
            ]))
        );
        assert!(parse_client(br#"{"op":"set"}"#).is_err());
        assert!(
            parse_client(br#"{"op":"reboot"}"#)
                .unwrap_err()
                .contains("unknown op")
        );
        assert!(parse_client(br#"{"op":"set","values":{"a":[1]}}"#).is_err());
        assert!(parse_client(b"\xff").is_err());
    }

    #[test]
    fn maps_values_onto_schema() {
        let v = values_from(
            &SCHEMA,
            &[
                ("ssid".into(), " Home ".into()),
                ("password".into(), "".into()),
                ("tls".into(), "yes".into()),
            ],
            None,
        )
        .unwrap();
        assert_eq!(v.get("ssid"), Some("Home"));
        assert_eq!(v.get("password"), None);
        assert_eq!(v.get("tls"), Some("true"));
        assert!(
            values_from(&SCHEMA, &[("evil".into(), "x".into())], None)
                .unwrap_err()
                .contains("unknown field")
        );
    }

    #[test]
    fn missing_secret_keeps_current_but_empty_secret_clears_it() {
        let mut current = Values::new();
        current.set("password", "stored-secret");
        let kept = values_from(&SCHEMA, &[("ssid".into(), "Home".into())], Some(&current)).unwrap();
        assert_eq!(kept.get("password"), Some("stored-secret"));
        let submitted = [
            ("ssid".into(), "Home".into()),
            ("password".into(), "".into()),
        ];
        let cleared = values_from(&SCHEMA, &submitted, Some(&current)).unwrap();
        assert_eq!(cleared.get("password"), None);
    }

    #[test]
    fn schema_message_is_valid_json_without_secrets() {
        let choices = vec![(
            "ssid",
            vec![Choice {
                value: "Ho\"me".into(),
                label: "Ho\"me -40 dBm".into(),
            }],
        )];
        let mut current = Values::new();
        current.set("ssid", "Old");
        current.set("password", "supersecret");
        let req = Request {
            schema: &SCHEMA,
            choices: &choices,
            current: Some(&current),
            attempt: 2,
            last_error: Some("wrong password"),
        };
        let msg = schema_message(&req);
        assert!(!msg.contains("supersecret"));
        let doc = json::parse(&msg).unwrap();
        assert_eq!(doc.get("op").and_then(Json::as_str), Some("schema"));
        assert_eq!(
            doc.get("title").and_then(Json::as_str),
            Some("Wi-Fi \"home\"")
        );
        assert_eq!(
            doc.get("error").and_then(Json::as_str),
            Some("wrong password")
        );
        let Some(Json::Array(fields)) = doc.get("fields") else {
            panic!()
        };
        assert_eq!(fields.len(), 4);
        assert_eq!(fields[0].get("kind").and_then(Json::as_str), Some("choice"));
        assert_eq!(fields[0].get("current").and_then(Json::as_str), Some("Old"));
        let Some(Json::Array(options)) = fields[0].get("options") else {
            panic!()
        };
        assert_eq!(
            options[0].get("value").and_then(Json::as_str),
            Some("Ho\"me")
        );
        assert_eq!(fields[1].get("has_value"), Some(&Json::Bool(true)));
        assert_eq!(fields[2].get("min"), Some(&Json::Number("1".into())));
        assert_eq!(fields[3].get("kind").and_then(Json::as_str), Some("bool"));
    }

    #[test]
    fn status_messages() {
        assert_eq!(
            status_message(State::Verifying, None),
            r#"{"op":"status","state":"verifying","error":null}"#
        );
        let e = json::parse(&status_message(
            State::Error,
            Some("could not connect to \"x\""),
        ))
        .unwrap();
        assert_eq!(
            e.get("error").and_then(Json::as_str),
            Some("could not connect to \"x\"")
        );
    }
}
