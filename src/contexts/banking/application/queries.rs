//! Tenant-safe Banking read models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::shared_kernel::UserId;

use super::super::domain::{
    ConnectionState, ConnectionVersion, ExternalResourceId, ProviderConnectionId,
    ResourceMappingId,
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
