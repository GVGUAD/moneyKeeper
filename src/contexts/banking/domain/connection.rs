//! Provider connection aggregate and encrypted credential generations.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::shared_kernel::UserId;

use super::{BankingError, ProviderConnectionId};

/// Optimistic provider-connection version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConnectionVersion(i64);

impl ConnectionVersion {
    pub const INITIAL: Self = Self(1);

    pub fn new(value: i64) -> Result<Self, BankingError> {
        (value > 0)
            .then_some(Self(value))
            .ok_or(BankingError::InvalidValue(
                "connection version must be positive",
            ))
    }

    pub const fn get(self) -> i64 {
        self.0
    }

    fn next(self) -> Result<Self, BankingError> {
        self.0
            .checked_add(1)
            .ok_or(BankingError::InvalidValue("connection version overflow"))
            .and_then(Self::new)
    }
}

/// Opaque authenticated encryption envelope. Its debug form is always redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct CredentialEnvelope {
    key_id: String,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
    envelope_version: u16,
}

impl CredentialEnvelope {
    pub fn new(
        key_id: impl Into<String>,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
    ) -> Result<Self, BankingError> {
        let key_id = key_id.into();
        if key_id.is_empty() || key_id.len() > 100 || nonce.is_empty() || ciphertext.is_empty() {
            return Err(BankingError::InvalidValue("invalid credential envelope"));
        }
        Ok(Self {
            key_id,
            nonce,
            ciphertext,
            envelope_version: 1,
        })
    }

    pub(crate) fn key_id(&self) -> &str {
        &self.key_id
    }

    pub(crate) fn nonce(&self) -> &[u8] {
        &self.nonce
    }

    pub(crate) fn ciphertext(&self) -> &[u8] {
        &self.ciphertext
    }

    pub(crate) const fn envelope_version(&self) -> u16 {
        self.envelope_version
    }
}

impl fmt::Debug for CredentialEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialEnvelope")
            .field("key_id", &"[REDACTED]")
            .field("nonce", &"[REDACTED]")
            .field("ciphertext", &"[REDACTED]")
            .field("envelope_version", &self.envelope_version)
            .finish()
    }
}

/// Provider connection lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Pending,
    Active,
    PendingCredentialValidation,
    NeedsReauth,
    Revoked,
}

/// One provider identity with at most one active and one pending credential.
#[derive(Clone, Debug)]
pub struct ProviderConnection {
    id: ProviderConnectionId,
    user_id: UserId,
    provider: String,
    state: ConnectionState,
    active_credential: Option<CredentialEnvelope>,
    pending_credential: Option<CredentialEnvelope>,
    credential_generation: i64,
    version: ConnectionVersion,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ProviderConnection {
    pub fn request(
        user_id: UserId,
        provider: impl Into<String>,
        credential: CredentialEnvelope,
        now: DateTime<Utc>,
    ) -> Result<Self, BankingError> {
        let provider = bounded(provider.into(), "provider")?;
        Ok(Self {
            id: ProviderConnectionId::generate(),
            user_id,
            provider,
            state: ConnectionState::Pending,
            active_credential: Some(credential),
            pending_credential: None,
            credential_generation: 1,
            version: ConnectionVersion::INITIAL,
            created_at: now,
            updated_at: now,
        })
    }

    pub fn activate(
        &mut self,
        expected_version: ConnectionVersion,
        now: DateTime<Utc>,
    ) -> Result<(), BankingError> {
        self.require_version(expected_version)?;
        if self.state != ConnectionState::Pending || self.active_credential.is_none() {
            return Err(BankingError::InvalidState);
        }
        self.state = ConnectionState::Active;
        self.bump(now)
    }

    pub fn request_credential_replacement(
        &mut self,
        candidate: CredentialEnvelope,
        expected_version: ConnectionVersion,
        now: DateTime<Utc>,
    ) -> Result<(), BankingError> {
        self.require_version(expected_version)?;
        if self.state == ConnectionState::Revoked || self.pending_credential.is_some() {
            return Err(BankingError::InvalidState);
        }
        self.pending_credential = Some(candidate);
        self.state = ConnectionState::PendingCredentialValidation;
        self.bump(now)
    }

    pub fn activate_candidate(
        &mut self,
        expected_version: ConnectionVersion,
        now: DateTime<Utc>,
    ) -> Result<(), BankingError> {
        self.require_version(expected_version)?;
        if self.state != ConnectionState::PendingCredentialValidation {
            return Err(BankingError::InvalidState);
        }
        self.active_credential = self.pending_credential.take();
        if self.active_credential.is_none() {
            return Err(BankingError::CredentialUnavailable);
        }
        self.credential_generation = self
            .credential_generation
            .checked_add(1)
            .ok_or(BankingError::InvalidValue("credential generation overflow"))?;
        self.state = ConnectionState::Active;
        self.bump(now)
    }

    pub fn reject_candidate(
        &mut self,
        expected_version: ConnectionVersion,
        now: DateTime<Utc>,
    ) -> Result<(), BankingError> {
        self.require_version(expected_version)?;
        if self.pending_credential.take().is_none() {
            return Err(BankingError::InvalidState);
        }
        self.state = if self.active_credential.is_some() {
            ConnectionState::Active
        } else {
            ConnectionState::NeedsReauth
        };
        self.bump(now)
    }

    pub fn disconnect(
        &mut self,
        expected_version: ConnectionVersion,
        now: DateTime<Utc>,
    ) -> Result<(), BankingError> {
        self.require_version(expected_version)?;
        self.active_credential = None;
        self.pending_credential = None;
        self.state = ConnectionState::Revoked;
        self.bump(now)
    }

    fn require_version(&self, expected: ConnectionVersion) -> Result<(), BankingError> {
        (self.version == expected)
            .then_some(())
            .ok_or(BankingError::VersionConflict)
    }

    fn bump(&mut self, now: DateTime<Utc>) -> Result<(), BankingError> {
        self.version = self.version.next()?;
        self.updated_at = now;
        Ok(())
    }

    pub const fn id(&self) -> ProviderConnectionId {
        self.id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub fn provider(&self) -> &str {
        &self.provider
    }
    pub const fn state(&self) -> ConnectionState {
        self.state
    }
    pub const fn version(&self) -> ConnectionVersion {
        self.version
    }
    pub const fn credential_generation(&self) -> i64 {
        self.credential_generation
    }
    pub fn has_usable_credential(&self) -> bool {
        self.active_credential.is_some() && self.state == ConnectionState::Active
    }
    pub const fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub const fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
}

fn bounded(value: String, _name: &'static str) -> Result<String, BankingError> {
    if value.is_empty()
        || value.len() > 100
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        Err(BankingError::InvalidValue(
            "provider must be bounded printable text",
        ))
    } else {
        Ok(value)
    }
}
