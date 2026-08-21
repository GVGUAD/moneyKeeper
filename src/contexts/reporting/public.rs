//! Stable read-only Reporting contracts.
pub use super::application::projectors::{ProjectionAction, classify};
use super::infrastructure::PgReportingStore;
use crate::shared_kernel::CurrencyCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
pub const CONTEXT_NAME: &str = "reporting";
#[derive(Clone)]
pub struct ReportingFacade {
    pub(crate) store: PgReportingStore,
}
impl ReportingFacade {
    pub(crate) fn new(store: PgReportingStore) -> Self {
        Self { store }
    }
    pub async fn apply_ledger_event(
        &self,
        event: crate::contexts::ledger::public::LedgerEventV1,
    ) -> Result<ProjectionApplyResult, sqlx::Error> {
        self.store.apply_ledger_event(event).await
    }
    pub async fn apply_fx_event(
        &self,
        event: crate::contexts::reference_data::public::FxObservedV1,
        source_sequence: u64,
    ) -> Result<ProjectionApplyResult, sqlx::Error> {
        self.store.apply_fx_event(event, source_sequence).await
    }
    pub async fn apply_journal_export(
        &self,
        event_id: crate::shared_kernel::EventId,
        source_sequence: u64,
        journal: crate::contexts::ledger::public::JournalView,
    ) -> Result<ProjectionApplyResult, sqlx::Error> {
        self.store
            .apply_journal_export(event_id, source_sequence, journal)
            .await
    }
    pub async fn apply_recurring_charge(
        &self,
        event_id: crate::shared_kernel::EventId,
        source_sequence: u64,
        event: crate::contexts::recurring::public::ChargeEvidenceRecordedV1,
    ) -> Result<ProjectionApplyResult, sqlx::Error> {
        self.store
            .apply_recurring_charge(event_id, source_sequence, event)
            .await
    }

    /// Clears rebuildable financial projections and replays a complete,
    /// sequence-ordered tenant-safe Ledger export.
    pub async fn rebuild_journals(
        &self,
        journals: Vec<(
            crate::shared_kernel::EventId,
            u64,
            crate::contexts::ledger::public::JournalView,
        )>,
    ) -> Result<(), sqlx::Error> {
        self.store.rebuild_journals(journals).await
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ProjectionApplyResult {
    pub applied: bool,
    pub sequence: u64,
}
#[derive(Clone, Debug, Deserialize)]
pub struct ReportRange {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub timezone: String,
    pub base_currency: Option<CurrencyCode>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReportMetadata {
    pub as_of: DateTime<Utc>,
    pub projection_sequence: u64,
    pub lag_seconds: u64,
    pub source_currency: Option<CurrencyCode>,
    pub base_currency: Option<CurrencyCode>,
    pub conversion_status: ConversionStatus,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversionStatus {
    NotRequested,
    Complete,
    MissingHistoricalRate,
    Partial,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReportResponse {
    pub metadata: ReportMetadata,
    pub rows: Vec<serde_json::Value>,
}
