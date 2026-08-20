//! Tenant-safe Banking read models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::shared_kernel::UserId;

use super::super::domain::{
    ConnectionState, ConnectionVersion, ExternalResourceId, ProviderConnectionId,
    ResourceMappingId,
    ProviderEventId,
    SyncJobId,
};
use crate::contexts::ledger::public::LedgerAccountId;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConnectionView {
    pub id: ProviderConnectionId,
    pub user_id: UserId,
    pub provider: String,
    pub state: ConnectionState,
    pub credential_generation: i64,
    pub version: ConnectionVersion,
    pub webhook_configured: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionResult {
    pub connection: ProviderConnectionView,
    pub replayed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceMappingView {
    pub id: ResourceMappingId,
    pub resource_id: ExternalResourceId,
    pub ledger_account_id: Option<LedgerAccountId>,
    pub mapping_version: i64,
    pub state: String,
    pub effective_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceMappingResult {
    pub mapping: ResourceMappingView,
    pub replayed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderEventIntakeOutcome { New, Duplicate, ConflictingContent }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEventReceipt {
    pub provider_event_id: ProviderEventId,
    pub outcome: ProviderEventIntakeOutcome,
    pub processing_state: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEventReadyV1 {
    pub provider_event_id: ProviderEventId,
    pub connection_id: ProviderConnectionId,
    pub resource_id: ExternalResourceId,
    pub external_event_id: String,
    pub revision: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncJobView {
    pub id: SyncJobId,
    pub user_id: UserId,
    pub connection_id: ProviderConnectionId,
    pub state: String,
    pub cursor: Option<String>,
    pub attempts: i32,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub fencing_token: i64,
    pub lease_holder: Option<String>,
    pub lease_expires_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPageView {
    pub id: uuid::Uuid,
    pub sync_job_id: SyncJobId,
    pub page_number: i64,
    pub provider_cursor: Option<String>,
    pub next_cursor: Option<String>,
    pub expected_events: i32,
    pub processed_events: i32,
    pub quarantined_events: i32,
    pub state: String,
}
