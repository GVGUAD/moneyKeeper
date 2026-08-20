//! Provider-neutral credential and remote-provider boundaries.

use std::fmt;

use async_trait::async_trait;
use zeroize::Zeroize;

use crate::shared_kernel::UserId;

use super::super::domain::{BankingError, CredentialEnvelope};

#[derive(Clone)]
pub struct ProviderCredential(String);

impl ProviderCredential {
    pub fn new(value: impl Into<String>) -> Result<Self, BankingError> {
        let value = value.into();
        if value.is_empty() || value.len() > 500 || value.chars().any(char::is_control) {
            return Err(BankingError::InvalidValue("invalid provider credential"));
        }
        Ok(Self(value))
    }

    pub fn expose(&self) -> &str { &self.0 }
}

impl fmt::Debug for ProviderCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderCredential([REDACTED])")
    }
}

impl Drop for ProviderCredential {
    fn drop(&mut self) { self.0.zeroize(); }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialBinding {
    user_id: UserId,
    connection_id: uuid::Uuid,
    provider: String,
    generation: i64,
    slot: String,
}

impl CredentialBinding {
    pub fn new(user_id: UserId, connection_id: uuid::Uuid, provider: impl Into<String>, generation: i64, slot: impl Into<String>) -> Result<Self, BankingError> {
        let provider = provider.into();
        let slot = slot.into();
        if provider.is_empty() || provider.len() > 100 || generation < 1 || !matches!(slot.as_str(), "active" | "pending" | "webhook" | "provenance") {
            return Err(BankingError::InvalidValue("invalid credential binding"));
        }
        Ok(Self { user_id, connection_id, provider, generation, slot })
    }

    pub(crate) fn associated_data(&self) -> Vec<u8> {
        format!("banking|{}|{}|{}|{}|{}", self.user_id, self.connection_id, self.provider, self.generation, self.slot).into_bytes()
    }
}

pub trait CredentialCipher: Send + Sync {
    fn encrypt(&self, credential: &ProviderCredential, binding: &CredentialBinding) -> Result<CredentialEnvelope, BankingError>;
    fn decrypt(&self, envelope: &CredentialEnvelope, binding: &CredentialBinding) -> Result<ProviderCredential, BankingError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderFailureClass { RateLimited, Transient, NeedsReauth, Terminal }

#[derive(Debug, thiserror::Error)]
pub enum ProviderFailure {
    #[error("provider request failed ({class:?}); sensitive response omitted")]
    Classified { class: ProviderFailureClass },
    #[error("provider response could not be normalized")]
    InvalidResponse,
}

#[async_trait]
pub trait ProviderClient: Send + Sync {
    async fn client_info(&self, credential: &ProviderCredential) -> Result<String, ProviderFailure>;

    async fn register_webhook(&self, _credential: &ProviderCredential, _callback_url: &str) -> Result<(), ProviderFailure> {
        Err(ProviderFailure::Classified { class: ProviderFailureClass::Terminal })
    }
}
