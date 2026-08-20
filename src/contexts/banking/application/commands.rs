//! Task-oriented Banking commands.

use chrono::{DateTime, Utc};

use crate::shared_kernel::{CorrelationId, IdempotencyKey, UserId};

use super::super::domain::{ConnectionVersion, ProviderConnectionId};
use super::ports::ProviderCredential;

pub struct ConnectProvider {
    pub user_id: UserId,
    pub provider: String,
    pub credential: ProviderCredential,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub requested_at: DateTime<Utc>,
}

pub struct ReplaceProviderCredential {
    pub user_id: UserId,
    pub connection_id: ProviderConnectionId,
    pub credential: ProviderCredential,
    pub expected_version: ConnectionVersion,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub requested_at: DateTime<Utc>,
}
