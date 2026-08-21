//! Stable failures raised by the Loans domain.

/// Explains why a loan command was rejected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LoanError {
    #[error("invalid loan value: {0}")]
    InvalidValue(&'static str),
    #[error("loan command uses a different currency")]
    CurrencyMismatch,
    #[error("loan aggregate version conflict")]
    VersionConflict,
    #[error("loan lifecycle does not allow this command")]
    InvalidState,
    #[error("loan component balance is insufficient")]
    InsufficientOutstanding,
    #[error("loan disbursements exceed contractual principal")]
    ContractualPrincipalExceeded,
    #[error("loan has pending accounting work")]
    AccountingPending,
    #[error("loan still has an outstanding balance")]
    OutstandingBalance,
    #[error("loan movement has already been reversed")]
    AlreadyReversed,
    #[error("money arithmetic failed")]
    Arithmetic,
}
