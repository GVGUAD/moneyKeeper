//! Stable Ledger contracts exposed to HTTP adapters and collaborating contexts.

pub use super::domain::{
    AccountAuthority, AccountKind, AccountLifecycle, AccountNature, AccountVersion,
    AccountVisibility, AnnotationId, JournalEntryId, LedgerAccount, LedgerAccountId, LedgerError,
    ObservationId, PostingId, PostingPurpose, ReconciliationCaseId, SystemAccountRole,
};

/// Stable bounded-context name used in integration envelopes.
pub const CONTEXT_NAME: &str = "ledger";
