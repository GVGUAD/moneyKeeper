//! Typed Ledger failures.

use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LedgerErrorKind {
    InvalidName,
    InvalidAccountKind,
    InvalidVersion,
    VersionConflict,
    AccountArchived,
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
