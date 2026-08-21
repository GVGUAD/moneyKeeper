//! Gmail connection aggregate with credential-generation fencing.

use super::MailError;
use crate::shared_kernel::UserId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

crate::define_uuid_id!(#[doc = "Identifies a Gmail connection."] pub GmailConnectionId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConnectionVersion(u64);
impl ConnectionVersion {
    pub const INITIAL: Self = Self(1);
    pub fn new(value: u64) -> Result<Self, MailError> {
        (value > 0)
            .then_some(Self(value))
            .ok_or(MailError::InvalidValue("version must be positive"))
    }
    pub const fn get(self) -> u64 {
        self.0
    }
    fn next(self) -> Result<Self, MailError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(MailError::GenerationOverflow)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Pending,
    Active,
    NeedsReauth,
    Disconnected,
}

#[derive(Clone, PartialEq, Eq)]
pub struct EncryptedSecret {
    key_id: String,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}
impl EncryptedSecret {
    pub fn new(
        key_id: impl Into<String>,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
    ) -> Result<Self, MailError> {
        let key_id = key_id.into();
        if key_id.is_empty() || nonce.len() < 12 || ciphertext.is_empty() {
            return Err(MailError::InvalidValue("invalid encrypted secret"));
        }
        Ok(Self {
            key_id,
            nonce,
            ciphertext,
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
}
impl fmt::Debug for EncryptedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EncryptedSecret([REDACTED])")
    }
}

#[derive(Clone, Debug)]
pub struct GmailConnection {
    id: GmailConnectionId,
    user_id: UserId,
    state: ConnectionState,
    credential: Option<EncryptedSecret>,
    credential_generation: u64,
    sync_generation: u64,
    version: ConnectionVersion,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
impl GmailConnection {
    pub fn connect(user_id: UserId, credential: EncryptedSecret, now: DateTime<Utc>) -> Self {
        Self {
            id: GmailConnectionId::generate(),
            user_id,
            state: ConnectionState::Active,
            credential: Some(credential),
            credential_generation: 1,
            sync_generation: 1,
            version: ConnectionVersion::INITIAL,
            created_at: now,
            updated_at: now,
        }
    }
    pub fn disconnect(
        &mut self,
        expected: ConnectionVersion,
        now: DateTime<Utc>,
    ) -> Result<(), MailError> {
        self.require(expected)?;
        if self.state == ConnectionState::Disconnected {
            return Err(MailError::InvalidState);
        }
        self.credential = None;
        self.state = ConnectionState::Disconnected;
        self.fence(now)
    }
    pub fn replace_credential(
        &mut self,
        credential: EncryptedSecret,
        expected: ConnectionVersion,
        now: DateTime<Utc>,
    ) -> Result<(), MailError> {
        self.require(expected)?;
        self.credential = Some(credential);
        self.state = ConnectionState::Active;
        self.credential_generation = self
            .credential_generation
            .checked_add(1)
            .ok_or(MailError::GenerationOverflow)?;
        self.fence(now)
    }
    pub fn request_resync(
        &mut self,
        expected: ConnectionVersion,
        now: DateTime<Utc>,
    ) -> Result<u64, MailError> {
        self.require(expected)?;
        if self.state != ConnectionState::Active {
            return Err(MailError::InvalidState);
        }
        self.sync_generation = self
            .sync_generation
            .checked_add(1)
            .ok_or(MailError::GenerationOverflow)?;
        self.version = self.version.next()?;
        self.updated_at = now;
        Ok(self.sync_generation)
    }
    pub fn mark_needs_reauth(
        &mut self,
        expected: ConnectionVersion,
        now: DateTime<Utc>,
    ) -> Result<(), MailError> {
        self.require(expected)?;
        if self.state != ConnectionState::Active {
            return Err(MailError::InvalidState);
        }
        self.state = ConnectionState::NeedsReauth;
        self.version = self.version.next()?;
        self.updated_at = now;
        Ok(())
    }
    fn require(&self, v: ConnectionVersion) -> Result<(), MailError> {
        if self.version == v {
            Ok(())
        } else {
            Err(MailError::VersionConflict)
        }
    }
    fn fence(&mut self, now: DateTime<Utc>) -> Result<(), MailError> {
        self.credential_generation = self
            .credential_generation
            .checked_add(1)
            .ok_or(MailError::GenerationOverflow)?;
        self.sync_generation = self
            .sync_generation
            .checked_add(1)
            .ok_or(MailError::GenerationOverflow)?;
        self.version = self.version.next()?;
        self.updated_at = now;
        Ok(())
    }
    pub const fn id(&self) -> GmailConnectionId {
        self.id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub const fn state(&self) -> ConnectionState {
        self.state
    }
    pub const fn version(&self) -> ConnectionVersion {
        self.version
    }
    pub const fn credential_generation(&self) -> u64 {
        self.credential_generation
    }
    pub const fn sync_generation(&self) -> u64 {
        self.sync_generation
    }
    pub const fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    pub const fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
    pub fn credential(&self) -> Option<&EncryptedSecret> {
        self.credential.as_ref()
    }
}
