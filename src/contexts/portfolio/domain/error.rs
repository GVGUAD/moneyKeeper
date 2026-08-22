//! Stable Portfolio domain failures.

/// Explains why a Portfolio business operation was rejected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PortfolioError {
    #[error("{0} is invalid")]
    InvalidValue(&'static str),
    #[error("currency does not match the instrument currency")]
    CurrencyMismatch,
    #[error("expected version is stale")]
    VersionConflict,
    #[error("portfolio account is archived")]
    AccountArchived,
    #[error("quantity must contain whole instruments")]
    FractionalOvdpQuantity,
    #[error("position does not contain enough quantity")]
    InsufficientQuantity,
    #[error("lot allocation does not belong to this position")]
    ForeignLot,
    #[error("lot allocations do not equal disposal quantity")]
    AllocationMismatch,
    #[error("transaction has already been reversed")]
    AlreadyReversed,
    #[error("a reversal cannot itself be reversed")]
    CannotReverseReversal,
    #[error("decimal arithmetic overflowed")]
    Arithmetic,
}
