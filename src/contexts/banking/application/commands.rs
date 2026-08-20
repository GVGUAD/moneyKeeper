//! Task-oriented Banking commands.

use chrono::{DateTime, Utc};

use crate::shared_kernel::{CorrelationId, IdempotencyKey, UserId};

use super::super::domain::ProviderTransactionState;
use super::super::domain::{ConnectionVersion, ExternalResourceId, ProviderConnectionId};
use super::ports::ProviderCredential;
use crate::contexts::ledger::public::LedgerAccountId;
use crate::shared_kernel::Money;

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

pub struct RequestSyncJob {
    pub user_id: UserId,
    pub connection_id: ProviderConnectionId,
    pub requested_from: DateTime<Utc>,
    pub requested_to: DateTime<Utc>,
    pub overlap_seconds: i32,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: CorrelationId,
}

pub struct BeginSyncPage {
    pub user_id: UserId,
    pub sync_job_id: super::super::domain::SyncJobId,
    pub holder: String,
    pub fencing_token: i64,
    pub provider_cursor: Option<String>,
    pub next_cursor: Option<String>,
    pub expected_events: i32,
    pub now: DateTime<Utc>,
}

pub struct CompleteSyncPage {
    pub user_id: UserId,
    pub sync_job_id: super::super::domain::SyncJobId,
    pub sync_page_id: uuid::Uuid,
    pub holder: String,
    pub fencing_token: i64,
    pub processed_events: i32,
    pub quarantined_events: i32,
    pub now: DateTime<Utc>,
}

pub struct RecordBalanceObservation {
    pub user_id: UserId,
    pub connection_id: ProviderConnectionId,
    pub resource_id: ExternalResourceId,
    pub basis: super::super::domain::BalanceBasis,
    pub provider_money: Money,
    pub sign_semantics: String,
    pub comparability: super::super::domain::BalanceComparability,
    pub observed_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    pub correlation_id: CorrelationId,
}

pub struct RotateWebhookCredential {
    pub user_id: UserId,
    pub connection_id: ProviderConnectionId,
    pub expected_version: ConnectionVersion,
    pub requested_at: DateTime<Utc>,
}
