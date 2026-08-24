use crate::contexts::reporting::public::{
    LoanSummary, PortfolioSummary, ProjectionApplyResult, ReportRange, ReportResponse,
    ReportingError,
};
use crate::shared_kernel::UserId;
use async_trait::async_trait;

#[async_trait]
pub(crate) trait ProjectionWriter: Send + Sync {
    async fn apply_ledger_event(
        &self,
        event: crate::contexts::ledger::public::LedgerEventV1,
    ) -> Result<ProjectionApplyResult, ReportingError>;
    async fn apply_fx_event(
        &self,
        event: crate::contexts::reference_data::public::FxObservedV1,
        source_sequence: u64,
    ) -> Result<ProjectionApplyResult, ReportingError>;
    async fn apply_journal_export(
        &self,
        event_id: crate::shared_kernel::EventId,
        source_sequence: u64,
        journal: crate::contexts::ledger::public::JournalView,
    ) -> Result<ProjectionApplyResult, ReportingError>;
    async fn apply_recurring_charge(
        &self,
        event_id: crate::shared_kernel::EventId,
        source_sequence: u64,
        event: crate::contexts::recurring::public::ChargeEvidenceRecordedV1,
    ) -> Result<ProjectionApplyResult, ReportingError>;
    async fn apply_loan_event(
        &self,
        event: crate::contexts::loans::public::LoanEventV1,
    ) -> Result<ProjectionApplyResult, ReportingError>;
    async fn apply_portfolio_event(
        &self,
        event: crate::contexts::portfolio::public::PortfolioEventV1,
    ) -> Result<ProjectionApplyResult, ReportingError>;
    async fn apply_sharing_event(
        &self,
        event: crate::contexts::sharing::public::SharingEventV1,
    ) -> Result<ProjectionApplyResult, ReportingError>;
}

#[async_trait]
pub(crate) trait ReportQuery: Send + Sync {
    async fn read(
        &self,
        user: UserId,
        range: ReportRange,
        kind: &'static str,
    ) -> Result<ReportResponse, ReportingError>;
    async fn portfolio_summary(
        &self,
        user: UserId,
    ) -> Result<Vec<PortfolioSummary>, ReportingError>;
    async fn loan_summary(
        &self,
        user: UserId,
        id: crate::contexts::loans::public::LoanAgreementId,
    ) -> Result<Option<LoanSummary>, ReportingError>;
}

#[async_trait]
pub(crate) trait ProjectionRebuild: Send + Sync {
    async fn rebuild_portfolio(
        &self,
        events: Vec<crate::contexts::portfolio::public::PortfolioEventV1>,
    ) -> Result<(), ReportingError>;
    async fn rebuild_sharing(
        &self,
        events: Vec<crate::contexts::sharing::public::SharingEventV1>,
    ) -> Result<(), ReportingError>;
    async fn rebuild_journals(
        &self,
        journals: Vec<(
            crate::shared_kernel::EventId,
            u64,
            crate::contexts::ledger::public::JournalView,
        )>,
    ) -> Result<(), ReportingError>;
}
