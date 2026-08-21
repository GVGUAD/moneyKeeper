//! Mail aggregates and immutable facts.

mod attempt;
mod connection;
mod error;
mod message;

pub use attempt::{AttemptId, AttemptKind, AttemptOutcome, ProcessingAttempt};
pub use connection::{
    ConnectionState, ConnectionVersion, EncryptedSecret, GmailConnection, GmailConnectionId,
};
pub use error::MailError;
pub use message::{SourceMessage, SourceMessageId};
