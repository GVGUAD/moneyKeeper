//! Typed Banking failures.

/// Reports a rejected Banking operation without exposing credentials or provider bodies.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BankingError {
    #[error("banking value is invalid: {0}")]
    InvalidValue(&'static str),
    #[error("banking state transition is not allowed")]
    InvalidState,
    #[error("banking aggregate version conflict")]
    VersionConflict,
    #[error("idempotency key was reused for a different banking request")]
    IdempotencyConflict,
    #[error("provider credential is unavailable")]
    CredentialUnavailable,
    #[error("resource must be routed to Portfolio")]
    RouteToPortfolio,
    #[error("resource cannot be mapped to that Ledger account")]
    IncompatibleMapping,
    #[error("resource already has an active mapping")]
    MappingAlreadyActive,
    #[error("resource has no active mapping")]
    MappingNotActive,
    #[error("provider event revision is invalid")]
    InvalidRevision,
    #[error("sync claim was fenced")]
    LeaseFenced,
    #[error("sync page is incomplete")]
    PageIncomplete,
}
