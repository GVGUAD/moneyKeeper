//! ISO-shaped currency-code value object.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use std::{fmt, str::FromStr};

/// A canonical three-letter uppercase ASCII currency code.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CurrencyCode(String);

impl CurrencyCode {
    /// Creates a canonical ISO-shaped currency code.
    ///
    /// Recognition and enablement belong to Reference Data; this constructor
    /// validates only the universal representation invariant.
    ///
    /// # Errors
    ///
    /// Returns [`CurrencyCodeError::InvalidFormat`] unless the input is
    /// exactly three uppercase ASCII letters.
    pub fn new(value: impl AsRef<str>) -> Result<Self, CurrencyCodeError> {
        let value = value.as_ref();
        if value.len() != 3 || !value.bytes().all(|byte| byte.is_ascii_uppercase()) {
            return Err(CurrencyCodeError::InvalidFormat);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the canonical three-letter representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CurrencyCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for CurrencyCode {
    type Err = CurrencyCodeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for CurrencyCode {
    type Error = CurrencyCodeError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for CurrencyCode {
    type Error = CurrencyCodeError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Serialize for CurrencyCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CurrencyCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

/// Explains why a currency code was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CurrencyCodeError {
    /// The value was not exactly three uppercase ASCII letters.
    #[error("currency code must be exactly three uppercase ASCII letters")]
    InvalidFormat,
}
