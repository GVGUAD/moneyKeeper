//! Validated opaque idempotency keys.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use std::{fmt, str::FromStr};

/// Maximum accepted idempotency-key length in UTF-8 bytes.
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 200;

/// A caller-supplied opaque key used to make a scoped command retry-safe.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Validates and preserves an idempotency key without normalization.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty key, surrounding whitespace, control
    /// characters, or more than 200 UTF-8 bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, IdempotencyKeyError> {
        let value = value.into();
        if value.is_empty() {
            return Err(IdempotencyKeyError::Empty);
        }
        if value.trim() != value {
            return Err(IdempotencyKeyError::SurroundingWhitespace);
        }
        if value.chars().any(char::is_control) {
            return Err(IdempotencyKeyError::ControlCharacter);
        }
        if value.len() > MAX_IDEMPOTENCY_KEY_BYTES {
            return Err(IdempotencyKeyError::TooLong {
                actual_bytes: value.len(),
                max_bytes: MAX_IDEMPOTENCY_KEY_BYTES,
            });
        }
        Ok(Self(value))
    }

    /// Returns the exact validated bytes as UTF-8 text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the key length in UTF-8 bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns whether the key contains no bytes.
    ///
    /// A constructed key is never empty; this method complements [`Self::len`].
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for IdempotencyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IdempotencyKey([REDACTED])")
    }
}

impl FromStr for IdempotencyKey {
    type Err = IdempotencyKeyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for IdempotencyKey {
    type Error = IdempotencyKeyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for IdempotencyKey {
    type Error = IdempotencyKeyError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Serialize for IdempotencyKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for IdempotencyKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

/// Explains why an idempotency key was rejected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdempotencyKeyError {
    /// The key contained no bytes.
    #[error("idempotency key cannot be empty")]
    Empty,
    /// The exact key began or ended with whitespace.
    #[error("idempotency key cannot contain surrounding whitespace")]
    SurroundingWhitespace,
    /// The key contained a control character.
    #[error("idempotency key cannot contain control characters")]
    ControlCharacter,
    /// The UTF-8 representation exceeded the storage bound.
    #[error("idempotency key is {actual_bytes} bytes; maximum is {max_bytes}")]
    TooLong {
        /// Actual length of the rejected key in UTF-8 bytes.
        actual_bytes: usize,
        /// Maximum accepted UTF-8 byte length.
        max_bytes: usize,
    },
}
