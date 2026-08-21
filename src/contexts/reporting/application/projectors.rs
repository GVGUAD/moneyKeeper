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
