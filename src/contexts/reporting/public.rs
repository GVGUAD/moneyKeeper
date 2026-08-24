//! Stable read-only Reporting contracts.
use super::application::ports::{ProjectionRebuild, ProjectionWriter, ReportQuery};
pub use super::application::projectors::{
    PortfolioProjectionAction, ProjectionAction, classify, classify_portfolio,
};
use crate::shared_kernel::CurrencyCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
pub const CONTEXT_NAME: &str = "reporting";
#[derive(Clone)]
pub struct ReportingFacade {
    writer: Arc<dyn ProjectionWriter>,
    queries: Arc<dyn ReportQuery>,
    rebuilds: Arc<dyn ProjectionRebuild>,
}
impl ReportingFacade {
    pub(crate) fn new(
        writer: Arc<dyn ProjectionWriter>,
        queries: Arc<dyn ReportQuery>,
        rebuilds: Arc<dyn ProjectionRebuild>,
    ) -> Self {
        Self {
            writer,
            queries,
            rebuilds,
        }
    }
    pub async fn apply_ledger_event(
        &self,
        event: crate::contexts::ledger::public::LedgerEventV1,
    ) -> Result<ProjectionApplyResult, ReportingError> {
        self.writer.apply_ledger_event(event).await
    }
    pub async fn apply_fx_event(
        &self,
        event: crate::contexts::reference_data::public::FxObservedV1,
        source_sequence: u64,
    ) -> Result<ProjectionApplyResult, ReportingError> {
        self.writer.apply_fx_event(event, source_sequence).await
    }
    pub async fn apply_journal_export(
        &self,
        event_id: crate::shared_kernel::EventId,
        source_sequence: u64,
        journal: crate::contexts::ledger::public::JournalView,
    ) -> Result<ProjectionApplyResult, ReportingError> {
        self.writer
            .apply_journal_export(event_id, source_sequence, journal)
            .await
    }
    pub async fn apply_recurring_charge(
        &self,
        event_id: crate::shared_kernel::EventId,
        source_sequence: u64,
        event: crate::contexts::recurring::public::ChargeEvidenceRecordedV1,
    ) -> Result<ProjectionApplyResult, ReportingError> {
        self.writer
            .apply_recurring_charge(event_id, source_sequence, event)
            .await
    }
    pub async fn apply_loan_event(
        &self,
        event: crate::contexts::loans::public::LoanEventV1,
    ) -> Result<ProjectionApplyResult, ReportingError> {
        self.writer.apply_loan_event(event).await
    }
    pub async fn apply_portfolio_event(
        &self,
        event: crate::contexts::portfolio::public::PortfolioEventV1,
    ) -> Result<ProjectionApplyResult, ReportingError> {
        self.writer.apply_portfolio_event(event).await
    }
    pub async fn portfolio_summary(
        &self,
        user: crate::shared_kernel::UserId,
    ) -> Result<Vec<PortfolioSummary>, ReportingError> {
        self.queries.portfolio_summary(user).await
    }
    pub async fn rebuild_portfolio(
        &self,
        events: Vec<crate::contexts::portfolio::public::PortfolioEventV1>,
    ) -> Result<(), ReportingError> {
        self.rebuilds.rebuild_portfolio(events).await
    }
    pub async fn loan_summary(
        &self,
        user: crate::shared_kernel::UserId,
        id: crate::contexts::loans::public::LoanAgreementId,
    ) -> Result<Option<LoanSummary>, ReportingError> {
        self.queries.loan_summary(user, id).await
    }

    pub async fn apply_sharing_event(
        &self,
        event: crate::contexts::sharing::public::SharingEventV1,
    ) -> Result<ProjectionApplyResult, ReportingError> {
        self.writer.apply_sharing_event(event).await
    }

    pub async fn rebuild_sharing(
        &self,
        events: Vec<crate::contexts::sharing::public::SharingEventV1>,
    ) -> Result<(), ReportingError> {
        self.rebuilds.rebuild_sharing(events).await
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
    ) -> Result<(), ReportingError> {
        self.rebuilds.rebuild_journals(journals).await
    }

    pub async fn read(
        &self,
        user: crate::shared_kernel::UserId,
        range: ReportRange,
        kind: &'static str,
    ) -> Result<ReportResponse, ReportingError> {
        self.queries.read(user, range, kind).await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReportingErrorKind {
    Invalid,
    Persistence,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ReportingError {
    kind: ReportingErrorKind,
    message: &'static str,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ReportingError {
    pub(crate) fn invalid(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::with_source(
            ReportingErrorKind::Invalid,
            "published reporting fact is invalid",
            source,
        )
    }
    pub(crate) fn persistence(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::with_source(
            ReportingErrorKind::Persistence,
            "reporting persistence failed",
            source,
        )
    }
    fn with_source(
        kind: ReportingErrorKind,
        message: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            message,
            source: Some(Box::new(source)),
        }
    }
    pub fn is_invalid(&self) -> bool {
        self.kind == ReportingErrorKind::Invalid
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LoanSummary {
    pub agreement_id: crate::contexts::loans::public::LoanAgreementId,
    pub currency: CurrencyCode,
    pub direction: Option<crate::contexts::loans::public::LoanDirection>,
    #[serde(with = "rust_decimal::serde::str")]
    pub principal: rust_decimal::Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub interest: rust_decimal::Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub fees: rust_decimal::Decimal,
    pub status: String,
    pub source_sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PortfolioSummary {
    pub account_id: crate::contexts::portfolio::public::PortfolioAccountId,
    pub instrument_id: crate::contexts::portfolio::public::InstrumentId,
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: rust_decimal::Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub remaining_known_cost: rust_decimal::Decimal,
    pub realized_gain_loss: Option<rust_decimal::Decimal>,
    pub market_value: Option<rust_decimal::Decimal>,
    pub currency: CurrencyCode,
    pub valuation_as_of: Option<DateTime<Utc>>,
    pub incomplete: bool,
    pub source_sequence: u64,
}
