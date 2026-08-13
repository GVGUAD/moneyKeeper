//! Idempotent inbox contracts for transaction-local consumer effects.

use std::{future::Future, pin::Pin};

use async_trait::async_trait;

use super::IntegrationEvent;

/// Validated durable name of one independently idempotent consumer.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConsumerName(String);

impl ConsumerName {
    /// Creates a stable consumer name.
    ///
    /// # Errors
    ///
    /// Rejects empty, overly long, or control-character-containing names.
    pub fn new(value: impl Into<String>) -> Result<Self, InboxError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 200
            || value.trim() != value
            || value.chars().any(char::is_control)
        {
            return Err(InboxError::InvalidConsumerName);
        }
        Ok(Self(value))
    }

    /// Returns the persisted consumer name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ConsumerName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Boxed transaction-local consumer work accepted by [`InboxExecutor`].
pub type InboxAction<'a> = Pin<Box<dyn Future<Output = Result<(), InboxError>> + Send + 'a>>;

/// Durable result of attempting one inbox delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InboxOutcome {
    /// The receipt and local side effects were applied together.
    Applied,
    /// This consumer had already committed this event identity.
    Duplicate,
}

/// Failure while validating or executing inbox work.
#[derive(Debug, thiserror::Error)]
pub enum InboxError {
    /// Consumer names must be bounded printable strings.
    #[error("consumer name must contain 1 to 200 printable characters")]
    InvalidConsumerName,
    /// PostgreSQL rejected or could not execute the operation.
    #[error("inbox persistence failed")]
    Database {
        /// Preserved adapter error without exposing a database type in the port.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The local consumer effect failed; its receipt and partial work rolled back.
    #[error("inbox action failed: {0}")]
    Action(String),
}

/// Executes a consumer effect once in the same unit of work as its receipt.
#[async_trait]
pub trait InboxExecutor {
    /// Persistence unit exposed only to the supplied local action.
    type UnitOfWork: Send;

    /// Inserts the receipt and runs `action`, or skips both for a duplicate.
    async fn execute_once<T>(
        &mut self,
        consumer: &ConsumerName,
        message: &IntegrationEvent,
        action: T,
    ) -> Result<InboxOutcome, InboxError>
    where
        T: for<'a> FnOnce(&'a mut Self::UnitOfWork) -> InboxAction<'a> + Send;
}
