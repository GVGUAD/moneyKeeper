//! Stable Sharing domain failures.

/// Explains why a Sharing command was rejected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SharingError {
    #[error("{0} cannot be empty")]
    Empty(&'static str),
    #[error("{0} is too long")]
    TooLong(&'static str),
    #[error("money must use the bill currency")]
    CurrencyMismatch,
    #[error("bill total must be positive")]
    InvalidTotal,
    #[error("contribution must be positive")]
    InvalidContribution,
    #[error("share cannot be negative")]
    InvalidShare,
    #[error("at least one share must be positive")]
    AllZeroShares,
    #[error("participant appears more than once")]
    DuplicateParticipant,
    #[error("contributions must equal the bill total")]
    ContributionTotalMismatch,
    #[error("shares must equal the bill total")]
    ShareTotalMismatch,
    #[error("equal allocation requires at least one participant")]
    EmptyEqualAllocation,
    #[error("minor-unit arithmetic overflowed")]
    ArithmeticOverflow,
    #[error("expected version {expected}, current version {actual}")]
    VersionConflict { expected: u64, actual: u64 },
    #[error("contact is archived")]
    ContactArchived,
    #[error("invalid lifecycle transition")]
    InvalidTransition,
    #[error("active settlements must be reversed first")]
    ActiveSettlements,
    #[error("accounting is still pending")]
    AccountingPending,
    #[error("settlement amount exceeds the remaining obligation")]
    OverSettlement,
    #[error("settlement must be positive")]
    InvalidSettlement,
    #[error("settlement has already been reversed")]
    AlreadyReversed,
    #[error("obligation cannot have the same debtor and creditor")]
    SelfObligation,
    #[error("money error: {0}")]
    Money(String),
    #[error("Sharing aggregate was not found")]
    NotFound,
    #[error("idempotency key was reused with different command content")]
    IdempotencyConflict,
    #[error("Sharing persistence failed: {0}")]
    Persistence(String),
}

impl From<crate::shared_kernel::MoneyError> for SharingError {
    fn from(value: crate::shared_kernel::MoneyError) -> Self {
        Self::Money(value.to_string())
    }
}
