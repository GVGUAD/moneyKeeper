#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RecurringError {
    #[error("recurring value is invalid: {0}")]
    InvalidValue(&'static str),
    #[error("recurring aggregate version conflict")]
    VersionConflict,
    #[error("recurring aggregate is in an invalid state")]
    InvalidState,
    #[error("allocation currency differs from evidence currency")]
    CurrencyMismatch,
    #[error("allocations exceed charge evidence amount")]
    AllocationOvercommit,
    #[error("match was already unmatched")]
    AlreadyUnmatched,
    #[error("categorization is pending")]
    CategorizationPending,
    #[error("arithmetic overflow")]
    ArithmeticOverflow,
}
