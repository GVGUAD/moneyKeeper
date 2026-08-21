//! Stable Mail domain failures.

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MailError {
    #[error("mail value is invalid: {0}")]
    InvalidValue(&'static str),
    #[error("mail connection is in an invalid state")]
    InvalidState,
    #[error("mail connection version conflict")]
    VersionConflict,
    #[error("mail generation overflowed")]
    GenerationOverflow,
    #[error("source message conflicts with an existing provider revision")]
    MessageConflict,
}
