//! Typed Ledger failures.

use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LedgerErrorKind {
    InvalidName,
    InvalidAccountKind,
    InvalidVersion,
    VersionConflict,
    AccountArchived,
    TooFewPostings,
    ZeroPosting,
    UnbalancedJournal,
    TenantMismatch,
    CurrencyMismatch,
    InvalidRelation,
    InvalidAnnotation,
    InvalidTags,
    InvalidSourceReference,
    InvalidObservation,
    InvalidState,
    StaleObservedBalance,
    IdempotencyConflict,
    InvalidMoney,
    NotFound,
    Persistence,
}

/// Reports a rejected Ledger operation without leaking persistence details.
#[derive(Debug)]
pub struct LedgerError {
    pub(crate) kind: LedgerErrorKind,
    message: String,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl LedgerError {
    pub(crate) fn invalid_name() -> Self {
        Self::new(
            LedgerErrorKind::InvalidName,
            "account name must contain 1 to 100 characters",
        )
    }

    pub(crate) fn invalid_account_kind() -> Self {
        Self::new(
            LedgerErrorKind::InvalidAccountKind,
            "account kind is incompatible with its nature or authority",
        )
    }

    pub(crate) fn invalid_version() -> Self {
        Self::new(
            LedgerErrorKind::InvalidVersion,
            "account version must be positive",
        )
    }

    pub(crate) fn version_conflict() -> Self {
        Self::new(LedgerErrorKind::VersionConflict, "ledger version conflict")
    }

    pub(crate) fn account_archived() -> Self {
        Self::new(
            LedgerErrorKind::AccountArchived,
            "archived account does not accept ordinary activity",
        )
    }

    pub(crate) fn too_few_postings() -> Self {
        Self::new(LedgerErrorKind::TooFewPostings, "journal requires at least two postings")
    }

    pub(crate) fn zero_posting() -> Self {
        Self::new(LedgerErrorKind::ZeroPosting, "posting amount cannot be zero")
    }

    pub(crate) fn unbalanced_journal() -> Self {
        Self::new(
            LedgerErrorKind::UnbalancedJournal,
            "journal must balance to zero independently in every currency",
        )
    }

    pub(crate) fn tenant_mismatch() -> Self {
        Self::new(LedgerErrorKind::TenantMismatch, "ledger tenant mismatch")
    }

    pub(crate) fn currency_mismatch() -> Self {
        Self::new(LedgerErrorKind::CurrencyMismatch, "ledger currency mismatch")
    }

    pub(crate) fn invalid_relation() -> Self {
        Self::new(LedgerErrorKind::InvalidRelation, "journal relation is invalid")
    }

    pub(crate) fn invalid_annotation(message: impl Into<String>) -> Self {
        Self::new(LedgerErrorKind::InvalidAnnotation, message)
    }

    pub(crate) fn invalid_tags() -> Self {
        Self::new(LedgerErrorKind::InvalidTags, "transaction tags are invalid")
    }

    pub(crate) fn invalid_source_reference() -> Self {
        Self::new(
            LedgerErrorKind::InvalidSourceReference,
            "source reference fields must contain bounded printable text",
        )
    }

    pub(crate) fn invalid_observation(message: impl Into<String>) -> Self {
        Self::new(LedgerErrorKind::InvalidObservation, message)
    }

    pub(crate) fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(LedgerErrorKind::InvalidState, message)
    }

    pub(crate) fn stale_observed_balance() -> Self {
        Self::new(
            LedgerErrorKind::StaleObservedBalance,
            "ledger balance changed after the observation",
        )
    }

    pub(crate) fn idempotency_conflict() -> Self {
        Self::new(
            LedgerErrorKind::IdempotencyConflict,
            "idempotency key was already used with a different request",
        )
    }

    pub(crate) fn invalid_money(message: impl Into<String>) -> Self {
        Self::new(LedgerErrorKind::InvalidMoney, message)
    }

    pub(crate) fn not_found() -> Self {
        Self::new(LedgerErrorKind::NotFound, "ledger resource was not found")
    }

    pub(crate) fn database(source: sqlx::Error) -> Self {
        Self::new(LedgerErrorKind::Persistence, "ledger storage is unavailable")
            .with_source(source)
    }

    pub(crate) fn persistence(message: impl Into<String>) -> Self {
        Self::new(LedgerErrorKind::Persistence, message)
    }

    fn new(kind: LedgerErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }

    fn with_source(mut self, source: impl Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Returns whether an account name violated its invariant.
    pub fn is_invalid_name(&self) -> bool {
        self.kind == LedgerErrorKind::InvalidName
    }

    /// Returns whether an account kind/nature/authority combination was invalid.
    pub fn is_invalid_account_kind(&self) -> bool {
        self.kind == LedgerErrorKind::InvalidAccountKind
    }

    /// Returns whether a stored or requested version was invalid.
    pub fn is_invalid_version(&self) -> bool {
        self.kind == LedgerErrorKind::InvalidVersion
    }

    /// Returns whether optimistic concurrency rejected a mutation.
    pub fn is_version_conflict(&self) -> bool {
        self.kind == LedgerErrorKind::VersionConflict
    }

    /// Returns whether ordinary activity targeted an archived account.
    pub fn is_account_archived(&self) -> bool {
        self.kind == LedgerErrorKind::AccountArchived
    }

    pub fn is_too_few_postings(&self) -> bool {
        self.kind == LedgerErrorKind::TooFewPostings
    }

    pub fn is_zero_posting(&self) -> bool {
        self.kind == LedgerErrorKind::ZeroPosting
    }

    pub fn is_unbalanced_journal(&self) -> bool {
        self.kind == LedgerErrorKind::UnbalancedJournal
    }

    pub fn is_tenant_mismatch(&self) -> bool {
        self.kind == LedgerErrorKind::TenantMismatch
    }

    pub fn is_currency_mismatch(&self) -> bool {
        self.kind == LedgerErrorKind::CurrencyMismatch
    }

    pub fn is_invalid_relation(&self) -> bool {
        self.kind == LedgerErrorKind::InvalidRelation
    }

    pub fn is_invalid_annotation(&self) -> bool {
        self.kind == LedgerErrorKind::InvalidAnnotation
    }

    pub fn is_invalid_tags(&self) -> bool {
        self.kind == LedgerErrorKind::InvalidTags
    }

    pub fn is_invalid_source_reference(&self) -> bool {
        self.kind == LedgerErrorKind::InvalidSourceReference
    }

    pub fn is_invalid_observation(&self) -> bool {
        self.kind == LedgerErrorKind::InvalidObservation
    }

    pub fn is_invalid_state(&self) -> bool {
        self.kind == LedgerErrorKind::InvalidState
    }

    pub fn is_stale_observed_balance(&self) -> bool {
        self.kind == LedgerErrorKind::StaleObservedBalance
    }

    pub fn is_idempotency_conflict(&self) -> bool {
        self.kind == LedgerErrorKind::IdempotencyConflict
    }

    pub fn is_invalid_money(&self) -> bool {
        self.kind == LedgerErrorKind::InvalidMoney
    }

    /// Returns whether a tenant-scoped Ledger resource was absent.
    pub fn is_not_found(&self) -> bool {
        self.kind == LedgerErrorKind::NotFound
    }

    /// Returns whether an infrastructure operation failed.
    pub fn is_persistence(&self) -> bool {
        self.kind == LedgerErrorKind::Persistence
    }
}

impl fmt::Display for LedgerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for LedgerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}
