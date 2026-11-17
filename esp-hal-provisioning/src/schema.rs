//! Description of the values a device needs to be provisioned with.
//!
//! A [`Schema`] is a static list of [`Field`]s. Every transport renders the
//! same schema in its own way: the CLI prompts for each field, a captive
//! portal generates an HTML form, BLE exposes the schema so a generic web page
//! can build its UI, and so on.

use alloc::string::String;
use core::fmt;

/// Maximum length (in bytes) of a single stored value.
pub const MAX_VALUE_LEN: u16 = 256;

/// Custom validation hook for a field. Return a short human readable reason
/// on failure; it is shown to the user by the transport.
pub type Validator = fn(&str) -> Result<(), &'static str>;

/// A set of fields that are provisioned, verified and stored together.
#[derive(Debug, Clone, Copy)]
pub struct Schema {
    /// Storage namespace; keys are stored as `"{namespace}/{field.key}"`.
    ///
    /// Must be unique per schema within one [`Store`](crate::Store).
    pub namespace: &'static str,
    /// Human readable title shown by transports (e.g. `"Wi-Fi"`).
    pub title: &'static str,
    /// The fields of this schema, in the order they are presented.
    pub fields: &'static [Field],
}

impl Schema {
    /// Create a new schema.
    pub const fn new(
        namespace: &'static str,
        title: &'static str,
        fields: &'static [Field],
    ) -> Self {
        Self {
            namespace,
            title,
            fields,
        }
    }

    /// Look up a field by key.
    pub fn field(&self, key: &str) -> Option<&'static Field> {
        self.fields.iter().find(|f| f.key == key)
    }
}

/// The type of a field, which drives validation and how transports render it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// Free-form text.
    Text {
        /// Minimum length in bytes (only checked for non-empty values of optional fields).
        min_len: u16,
        /// Maximum length in bytes.
        max_len: u16,
    },
    /// Like [`FieldKind::Text`], but never echoed or logged.
    Secret {
        /// Minimum length in bytes (only checked for non-empty values of optional fields).
        min_len: u16,
        /// Maximum length in bytes.
        max_len: u16,
    },
    /// A signed integer within an inclusive range.
    Integer {
        /// Inclusive minimum.
        min: i64,
        /// Inclusive maximum.
        max: i64,
    },
    /// A yes/no value. Stored as `"true"` / `"false"`.
    Bool,
    /// One value out of a list supplied at runtime (see
    /// [`Request::choices`](crate::Request::choices)), e.g. scanned Wi-Fi networks.
    Choice {
        /// Whether a value not in the list may be entered (e.g. a hidden SSID).
        allow_custom: bool,
        /// Minimum length in bytes.
        min_len: u16,
        /// Maximum length in bytes.
        max_len: u16,
    },
}

/// A single provisionable value.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    /// Storage key, unique within the schema.
    pub key: &'static str,
    /// Short human readable name.
    pub name: &'static str,
    /// Longer description shown to the user.
    pub description: &'static str,
    /// Type of the value.
    pub kind: FieldKind,
    /// Whether a value must be provided.
    pub required: bool,
    /// Optional additional validation.
    pub validator: Option<Validator>,
}

impl Field {
    const fn base(
        key: &'static str,
        name: &'static str,
        description: &'static str,
        kind: FieldKind,
    ) -> Self {
        Self {
            key,
            name,
            description,
            kind,
            required: true,
            validator: None,
        }
    }

    /// A required text field (1..=64 bytes).
    pub const fn text(key: &'static str, name: &'static str, description: &'static str) -> Self {
        Self::base(
            key,
            name,
            description,
            FieldKind::Text {
                min_len: 1,
                max_len: 64,
            },
        )
    }

    /// A required secret field (1..=64 bytes). Secrets are masked on input
    /// and never logged.
    pub const fn secret(key: &'static str, name: &'static str, description: &'static str) -> Self {
        Self::base(
            key,
            name,
            description,
            FieldKind::Secret {
                min_len: 1,
                max_len: 64,
            },
        )
    }

    /// A required integer field within `min..=max`.
    pub const fn integer(
        key: &'static str,
        name: &'static str,
        description: &'static str,
        min: i64,
        max: i64,
    ) -> Self {
        Self::base(key, name, description, FieldKind::Integer { min, max })
    }

    /// A required yes/no field.
    pub const fn boolean(key: &'static str, name: &'static str, description: &'static str) -> Self {
        Self::base(key, name, description, FieldKind::Bool)
    }

    /// A required choice field. The options are supplied at runtime.
    pub const fn choice(
        key: &'static str,
        name: &'static str,
        description: &'static str,
        allow_custom: bool,
    ) -> Self {
        Self::base(
            key,
            name,
            description,
            FieldKind::Choice {
                allow_custom,
                min_len: 1,
                max_len: 64,
            },
        )
    }

    /// Mark the field as optional. An empty value is accepted and stored as absent.
    pub const fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    /// Set the length limits of a text, secret or choice field.
    ///
    /// Has no effect on integer and boolean fields.
    pub const fn with_len(mut self, min: u16, max: u16) -> Self {
        let max = if max > MAX_VALUE_LEN {
            MAX_VALUE_LEN
        } else {
            max
        };
        self.kind = match self.kind {
            FieldKind::Text { .. } => FieldKind::Text {
                min_len: min,
                max_len: max,
            },
            FieldKind::Secret { .. } => FieldKind::Secret {
                min_len: min,
                max_len: max,
            },
            FieldKind::Choice { allow_custom, .. } => FieldKind::Choice {
                allow_custom,
                min_len: min,
                max_len: max,
            },
            other => other,
        };
        self
    }

    /// Attach a custom validator, run after the built-in checks.
    pub const fn with_validator(mut self, validator: Validator) -> Self {
        self.validator = Some(validator);
        self
    }

    /// Whether this field holds a secret that must not be echoed or logged.
    pub fn is_secret(&self) -> bool {
        matches!(self.kind, FieldKind::Secret { .. })
    }

    /// Normalize user input into the canonical stored form.
    ///
    /// Trims surrounding whitespace (except for secrets, where whitespace may
    /// be significant) and maps boolean spellings (`y`, `yes`, `1`, `on`, …)
    /// to `"true"` / `"false"`.
    pub fn normalize(&self, input: &str) -> String {
        match self.kind {
            FieldKind::Secret { .. } => String::from(input),
            FieldKind::Bool => match parse_bool(input.trim()) {
                Some(true) => String::from("true"),
                Some(false) => String::from("false"),
                None => String::from(input.trim()),
            },
            _ => String::from(input.trim()),
        }
    }

    /// Validate a (normalized) value against this field.
    pub fn validate(&self, value: &str) -> Result<(), ValidationError> {
        if value.is_empty() {
            return if self.required {
                Err(ValidationError::Required)
            } else {
                Ok(())
            };
        }

        match self.kind {
            FieldKind::Text { min_len, max_len }
            | FieldKind::Secret { min_len, max_len }
            | FieldKind::Choice {
                min_len, max_len, ..
            } => {
                let len = value.len();
                if len < min_len as usize {
                    return Err(ValidationError::TooShort { min: min_len });
                }
                if len > max_len as usize {
                    return Err(ValidationError::TooLong { max: max_len });
                }
            }
            FieldKind::Integer { min, max } => {
                let n: i64 = value.parse().map_err(|_| ValidationError::NotANumber)?;
                if n < min || n > max {
                    return Err(ValidationError::OutOfRange { min, max });
                }
            }
            FieldKind::Bool => {
                if parse_bool(value).is_none() {
                    return Err(ValidationError::NotABool);
                }
            }
        }

        if let Some(validator) = self.validator {
            validator(value).map_err(ValidationError::Custom)?;
        }
        Ok(())
    }
}

/// Parse common boolean spellings.
pub fn parse_bool(s: &str) -> Option<bool> {
    const TRUE: &[&str] = &["true", "yes", "y", "1", "on"];
    const FALSE: &[&str] = &["false", "no", "n", "0", "off"];
    if TRUE.iter().any(|t| s.eq_ignore_ascii_case(t)) {
        Some(true)
    } else if FALSE.iter().any(|f| s.eq_ignore_ascii_case(f)) {
        Some(false)
    } else {
        None
    }
}

/// Why a value was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ValidationError {
    /// The field is required but the value is empty.
    Required,
    /// The value is shorter than allowed.
    TooShort {
        /// Minimum length in bytes.
        min: u16,
    },
    /// The value is longer than allowed.
    TooLong {
        /// Maximum length in bytes.
        max: u16,
    },
    /// The value is not a valid integer.
    NotANumber,
    /// The integer is outside the allowed range.
    OutOfRange {
        /// Inclusive minimum.
        min: i64,
        /// Inclusive maximum.
        max: i64,
    },
    /// The value is not a recognized boolean.
    NotABool,
    /// Rejected by a custom [`Validator`].
    Custom(&'static str),
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Required => f.write_str("a value is required"),
            Self::TooShort { min } => write!(f, "must be at least {min} bytes long"),
            Self::TooLong { max } => write!(f, "must be at most {max} bytes long"),
            Self::NotANumber => f.write_str("must be a whole number"),
            Self::OutOfRange { min, max } => write!(f, "must be between {min} and {max}"),
            Self::NotABool => f.write_str("answer yes or no"),
            Self::Custom(reason) => f.write_str(reason),
        }
    }
}

impl core::error::Error for ValidationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_text() {
        let f = Field::text("k", "K", "");
        assert_eq!(f.validate(""), Err(ValidationError::Required));
        assert_eq!(f.validate("x"), Ok(()));
        assert_eq!(
            f.validate(&"x".repeat(65)),
            Err(ValidationError::TooLong { max: 64 })
        );
    }

    #[test]
    fn optional_skips_length_check_when_empty() {
        let f = Field::secret("p", "P", "").optional().with_len(8, 64);
        assert_eq!(f.validate(""), Ok(()));
        assert_eq!(
            f.validate("short"),
            Err(ValidationError::TooShort { min: 8 })
        );
        assert_eq!(f.validate("longenough"), Ok(()));
    }

    #[test]
    fn integer_range() {
        let f = Field::integer("port", "Port", "", 1, 65535);
        assert_eq!(
            f.validate("0"),
            Err(ValidationError::OutOfRange { min: 1, max: 65535 })
        );
        assert_eq!(f.validate("abc"), Err(ValidationError::NotANumber));
        assert_eq!(f.validate("1883"), Ok(()));
    }

    #[test]
    fn bool_normalization() {
        let f = Field::boolean("b", "B", "");
        assert_eq!(f.normalize(" Yes "), "true");
        assert_eq!(f.normalize("off"), "false");
        assert_eq!(f.validate("maybe"), Err(ValidationError::NotABool));
        assert_eq!(f.validate("true"), Ok(()));
    }

    #[test]
    fn secrets_are_not_trimmed() {
        let f = Field::secret("p", "P", "");
        assert_eq!(f.normalize(" pass "), " pass ");
        let t = Field::text("t", "T", "");
        assert_eq!(t.normalize(" text "), "text");
    }

    #[test]
    fn custom_validator() {
        fn no_spaces(v: &str) -> Result<(), &'static str> {
            if v.contains(' ') {
                Err("must not contain spaces")
            } else {
                Ok(())
            }
        }
        let f = Field::text("t", "T", "").with_validator(no_spaces);
        assert_eq!(
            f.validate("a b"),
            Err(ValidationError::Custom("must not contain spaces"))
        );
        assert_eq!(f.validate("ab"), Ok(()));
    }

    #[test]
    fn with_len_clamps_to_max_value_len() {
        let f = Field::text("t", "T", "").with_len(0, 1000);
        assert_eq!(
            f.kind,
            FieldKind::Text {
                min_len: 0,
                max_len: MAX_VALUE_LEN
            }
        );
    }
}
