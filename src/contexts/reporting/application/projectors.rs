//! Versioned event dispatch for exactly-once Reporting consumers.
use crate::contexts::ledger::public::{LedgerEventFactV1, LedgerEventV1};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionAction {
    AccountLifecycle,
    JournalPosted,
    JournalReversed,
    JournalReplaced,
    Annotation,
    Balance,
    Reconciliation,
    AccountingProcess,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortfolioProjectionAction {
    Instrument,
    Account,
    Transaction,
    Position,
    Valuation,
    CashWorkflow,
}
pub fn classify_portfolio(
    event: &crate::contexts::portfolio::public::PortfolioEventV1,
) -> Result<PortfolioProjectionAction, &'static str> {
    use crate::contexts::portfolio::public::PortfolioEventFactV1 as F;
    if event.metadata.schema_version != 1 {
        return Err("unknown portfolio event major version");
    }
    Ok(match event.fact {
        F::InstrumentCreated { .. } => PortfolioProjectionAction::Instrument,
        F::AccountChanged { .. } => PortfolioProjectionAction::Account,
        F::TransactionPosted { .. } | F::TransactionReversed { .. } => {
            PortfolioProjectionAction::Transaction
        }
        F::PositionChanged { .. } => PortfolioProjectionAction::Position,
        F::ValuationRecorded { .. } => PortfolioProjectionAction::Valuation,
        F::CashSettlementPosted { .. }
        | F::CashSettlementReversed { .. }
        | F::CashSettlementCancelledWithoutEffect { .. } => PortfolioProjectionAction::CashWorkflow,
    })
}
pub fn classify(event: &LedgerEventV1) -> Result<ProjectionAction, &'static str> {
    if event.metadata.schema_version != 1 {
        return Err("unknown ledger event major version");
    }
    Ok(match event.fact {
        LedgerEventFactV1::AccountLifecycleChanged { .. } => ProjectionAction::AccountLifecycle,
        LedgerEventFactV1::EntryPosted { .. } => ProjectionAction::JournalPosted,
        LedgerEventFactV1::EntryReversed { .. } => ProjectionAction::JournalReversed,
        LedgerEventFactV1::EntryReplaced { .. } => ProjectionAction::JournalReplaced,
        LedgerEventFactV1::AnnotationChanged { .. } => ProjectionAction::Annotation,
        LedgerEventFactV1::BalanceChanged { .. } => ProjectionAction::Balance,
        LedgerEventFactV1::ReconciliationObserved { .. }
        | LedgerEventFactV1::ReconciliationMatched { .. }
        | LedgerEventFactV1::ReconciliationSuperseded { .. }
        | LedgerEventFactV1::ReconciliationIgnoredOlder { .. }
        | LedgerEventFactV1::ReconciliationApproved { .. }
        | LedgerEventFactV1::ReconciliationDismissed { .. }
        | LedgerEventFactV1::ReconciliationStale { .. } => ProjectionAction::Reconciliation,
        LedgerEventFactV1::InternalAccountingCommandPosted { .. }
        | LedgerEventFactV1::InternalAccountingCommandFailed { .. } => {
            ProjectionAction::AccountingProcess
        }
    })
}
