//! High-entropy callback credentials and keyed lookup digests.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::contexts::banking::domain::BankingError;

pub struct WebhookCredential(String);

impl WebhookCredential {
    pub fn new(value: impl Into<String>) -> Result<Self, BankingError> {
        let value = value.into();
        if value.len() < 43
            || value.len() > 100
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(BankingError::InvalidValue("invalid webhook credential"));
        }
        Ok(Self(value))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for WebhookCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("WebhookCredential([REDACTED])")
    }
}

impl Drop for WebhookCredential {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone)]
pub struct WebhookSecretManager {
    lookup_key: [u8; 32],
}

impl WebhookSecretManager {
    pub const fn new(lookup_key: [u8; 32]) -> Self {
        Self { lookup_key }
    }
    pub fn generate(&self) -> WebhookCredential {
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        WebhookCredential(URL_SAFE_NO_PAD.encode(bytes))
    }
    pub fn digest(&self, credential: &WebhookCredential) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(self.lookup_key);
        hash.update(credential.expose().as_bytes());
        hash.finalize().into()
    }
    pub fn verify(&self, credential: &WebhookCredential, expected: &[u8]) -> bool {
        constant_time_eq(&self.digest(credential), expected)
    }
    pub fn verify_digest(&self, actual: &[u8], expected: &[u8]) -> bool {
        constant_time_eq(actual, expected)
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}
