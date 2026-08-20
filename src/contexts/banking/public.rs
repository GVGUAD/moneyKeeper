//! Stable public Banking contracts.

pub use super::application::*;
pub use super::domain::*;
pub use super::infrastructure::{
    Aes256CredentialCipher, MonobankAdapter, MonobankClient, NormalizedResource,
    NormalizedSnapshot, WebhookCredential, WebhookSecretManager,
};

/// Identifies the Banking bounded context before its contracts are introduced.
pub const CONTEXT_NAME: &str = "banking";
