//! Immutable encrypted source-message revisions.
use super::{GmailConnectionId, MailError};
use crate::shared_kernel::UserId;
use chrono::{DateTime, Utc};
crate::define_uuid_id!(#[doc = "Identifies an immutable source-message revision."] pub SourceMessageId);
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceMessage {
    id: SourceMessageId,
    user_id: UserId,
    connection_id: GmailConnectionId,
    provider_message_id: String,
    revision: u64,
    payload_digest: [u8; 32],
    received_at: DateTime<Utc>,
    recorded_at: DateTime<Utc>,
}
impl SourceMessage {
    pub fn record(
        user_id: UserId,
        connection_id: GmailConnectionId,
        provider_message_id: impl Into<String>,
        revision: u64,
        payload_digest: [u8; 32],
        received_at: DateTime<Utc>,
        recorded_at: DateTime<Utc>,
    ) -> Result<Self, MailError> {
        let provider_message_id = provider_message_id.into();
        if provider_message_id.trim() != provider_message_id
            || provider_message_id.is_empty()
            || revision == 0
            || received_at > recorded_at
        {
            return Err(MailError::InvalidValue("invalid source message"));
        }
        Ok(Self {
            id: SourceMessageId::generate(),
            user_id,
            connection_id,
            provider_message_id,
            revision,
            payload_digest,
            received_at,
            recorded_at,
        })
    }
    pub const fn id(&self) -> SourceMessageId {
        self.id
    }
    pub const fn user_id(&self) -> UserId {
        self.user_id
    }
    pub const fn connection_id(&self) -> GmailConnectionId {
        self.connection_id
    }
    pub fn provider_message_id(&self) -> &str {
        &self.provider_message_id
    }
    pub const fn revision(&self) -> u64 {
        self.revision
    }
    pub const fn payload_digest(&self) -> [u8; 32] {
        self.payload_digest
    }
    pub const fn received_at(&self) -> DateTime<Utc> {
        self.received_at
    }
    pub const fn recorded_at(&self) -> DateTime<Utc> {
        self.recorded_at
    }
}
