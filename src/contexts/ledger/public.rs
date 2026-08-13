//! Stable Ledger contracts exposed to HTTP adapters and collaborating contexts.

pub use super::domain::{
    AccountAuthority, AccountKind, AccountLifecycle, AccountNature, AccountVersion,
    AccountVisibility, Actor, AnnotationChanged, AnnotationChanges, AnnotationId,
    AnnotationVersion, BalanceObservation, BalanceVersion, BudgetVisibility, CategoryReference,
    JournalEntry, JournalEntryId, JournalRelations, JournalSource, LedgerAccount, LedgerAccountId,
    LedgerError, NormalizedTags, ObservationId, Posting, PostingId, PostingPurpose,
    ReconciliationCase, ReconciliationCaseId, ReconciliationEvent, ReconciliationStatus,
    ReconciliationVersion, SourceReference, SystemAccountRole, TransactionAnnotation,
};

/// Stable bounded-context name used in integration envelopes.
pub const CONTEXT_NAME: &str = "ledger";
