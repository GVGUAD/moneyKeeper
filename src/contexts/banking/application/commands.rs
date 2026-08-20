//! Task-oriented Banking commands.

use chrono::{DateTime, Utc};

use crate::shared_kernel::{CorrelationId, IdempotencyKey, UserId};

use super::super::domain::{ConnectionVersion, ExternalResourceId, ProviderConnectionId};
use crate::contexts::ledger::public::LedgerAccountId;
use super::super::domain::ProviderTransactionState;
use crate::shared_kernel::Money;
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

pub struct BindExistingResource {
    pub user_id: UserId,
    pub resource_id: ExternalResourceId,
    pub ledger_account_id: LedgerAccountId,
    pub expected_resource_version: i64,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub requested_at: DateTime<Utc>,
}

pub struct CreateAndMapResource {
    pub user_id: UserId,
    pub resource_id: ExternalResourceId,
    pub account_name: String,
    pub expected_resource_version: i64,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub requested_at: DateTime<Utc>,
}

pub struct DeactivateResourceMapping {
    pub user_id: UserId,
    pub resource_id: ExternalResourceId,
    pub expected_resource_version: i64,
    pub reason: String,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
    pub requested_at: DateTime<Utc>,
}

pub struct IntakeProviderEvent {
    pub user_id: UserId,
    pub connection_id: ProviderConnectionId,
    pub resource_id: ExternalResourceId,
    pub external_event_id: String,
    pub revision: i64,
    pub state: ProviderTransactionState,
    pub operation_money: Money,
    pub description: String,
    pub effective_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    pub correlation_id: CorrelationId,
}
