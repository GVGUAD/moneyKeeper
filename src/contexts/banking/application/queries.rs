//! Tenant-safe Banking read models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::shared_kernel::UserId;

use super::super::domain::{
    BalanceObservationId, ConnectionState, ConnectionVersion, ExternalResourceId,
    ProviderConnectionId, ProviderEventId, ResourceMappingId, SyncJobId,
};
use crate::contexts::ledger::public::JournalEntryId;
use crate::contexts::ledger::public::LedgerAccountId;
use crate::shared_kernel::Money;

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
pub enum ProviderEventIntakeOutcome {
    New,
    Duplicate,
    ConflictingContent,
}

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

#[derive(Clone, Debug)]
pub struct ProviderImportWork {
    pub provider_event_id: ProviderEventId,
    pub user_id: UserId,
    pub connection_id: ProviderConnectionId,
    pub resource_id: ExternalResourceId,
    pub external_event_id: String,
    pub revision: i64,
    pub state: super::super::domain::ProviderTransactionState,
    pub operation_money: Money,
    pub description: String,
    pub effective_at: DateTime<Utc>,
    pub ledger_account_id: LedgerAccountId,
    pub previous_journal_id: Option<JournalEntryId>,
    pub previous_money: Option<Money>,
    pub previous_state: Option<super::super::domain::ProviderTransactionState>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderImportOutcome {
    pub provider_event_id: ProviderEventId,
    pub state: String,
    pub ledger_journal_entry_id: Option<JournalEntryId>,
    pub replayed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BalanceObservationView {
    pub id: BalanceObservationId,
    pub resource_id: ExternalResourceId,
    pub source_sequence: i64,
    pub basis: super::super::domain::BalanceBasis,
    pub provider_money: Money,
    pub comparable_money: Option<Money>,
    pub non_comparable_reason: Option<String>,
    pub observed_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    pub delivery_state: String,
    pub reconciliation_case_id: Option<crate::contexts::ledger::public::ReconciliationCaseId>,
}

#[derive(Clone, Debug)]
pub struct BalanceObservationDeliveryWork {
    pub observation: BalanceObservationView,
    pub user_id: UserId,
    pub ledger_account_id: LedgerAccountId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceObservationDeliveryOutcome {
    pub observation_id: BalanceObservationId,
    pub state: String,
    pub reconciliation_case_id: Option<crate::contexts::ledger::public::ReconciliationCaseId>,
    pub active_case_id: Option<crate::contexts::ledger::public::ReconciliationCaseId>,
    pub replayed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceObservedV1 {
    pub observation_id: BalanceObservationId,
    pub resource_id: ExternalResourceId,
    pub source_sequence: i64,
    pub basis: super::super::domain::BalanceBasis,
    pub comparable: bool,
}

#[derive(Debug)]
pub struct WebhookRotationResult {
    pub connection_id: ProviderConnectionId,
    pub credential: super::super::infrastructure::WebhookCredential,
    pub desired_version: i64,
    pub connection_version: ConnectionVersion,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookReceiptOutcome {
    pub connection_id: ProviderConnectionId,
    pub receipt_id: uuid::Uuid,
    pub duplicate: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalResourceView {
    pub id: ExternalResourceId,
    pub connection_id: ProviderConnectionId,
    pub kind: super::super::domain::ResourceKind,
    pub funding_model: super::super::domain::FundingModel,
    pub currency: crate::shared_kernel::CurrencyCode,
    pub masked_label: String,
    pub discovery_state: String,
    pub version: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProviderEventView {
    pub id: ProviderEventId,
    pub resource_id: ExternalResourceId,
    pub external_event_id: String,
    pub revision: i64,
    pub state: super::super::domain::ProviderTransactionState,
    pub operation_money: Money,
    pub description: String,
    pub effective_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    pub processing_state: String,
    pub attempts: i32,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountingProcessView {
    pub id: uuid::Uuid,
    pub process_name: String,
    pub state: serde_json::Value,
    pub status: String,
    pub version: i64,
    pub next_wake_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProviderAccountSummary {
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub provider_reported: Option<rust_decimal::Decimal>,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub available: Option<rust_decimal::Decimal>,
    pub currency: Option<crate::shared_kernel::CurrencyCode>,
    pub as_of: Option<DateTime<Utc>>,
}
