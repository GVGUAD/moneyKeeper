//! Transactional outbox contracts and the at-least-once dispatcher.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::infrastructure::v2_db::VerifiedV2Pool;

use super::{IntegrationEvent, postgres::PgOutboxStore};

crate::define_uuid_id!(
    /// Identifies one durable outbox record independently from event identity.
    pub OutboxMessageId
);

/// Failure while appending or updating a durable outbox record.
#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    /// PostgreSQL rejected or could not execute the operation.
    #[error("outbox persistence failed")]
    Database {
        /// Preserved adapter error without exposing a database type in the port.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Persisted event metadata was malformed or unsupported.
    #[error("outbox contains an invalid event: {0}")]
    InvalidEvent(String),
    /// Dispatcher configuration is outside its safe bounds.
    #[error("invalid outbox dispatcher configuration: {0}")]
    InvalidConfiguration(String),
}

/// Appends an integration event using the caller's unit of work.
#[async_trait]
pub trait OutboxWriter {
    /// Appends `event`; rollback of the caller's transaction removes it too.
    async fn append(&mut self, event: &IntegrationEvent) -> Result<(), OutboxError>;
}

/// Publishes an integration event to an external or in-process transport.
///
/// Implementations must tolerate duplicate calls because a process can crash
/// after publication but before the outbox acknowledgment is persisted.
#[async_trait]
pub trait EventPublisher: Send + Sync {
    /// Publisher-specific error type. Its text is never persisted verbatim.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Publishes one event after its database claim has committed.
    async fn publish(&self, event: &IntegrationEvent) -> Result<(), Self::Error>;
}

/// Retry and lease policy for a bounded dispatcher invocation.
#[derive(Clone, Debug)]
pub struct DispatcherConfig {
    /// Maximum records claimed per invocation.
    pub batch_size: u32,
    /// Duration for which a committed claim excludes another dispatcher.
    pub claim_ttl: Duration,
    /// First retry delay after a failed publication.
    pub initial_retry_delay: Duration,
    /// Upper bound for exponential retry delay.
    pub maximum_retry_delay: Duration,
    /// Publication attempts after which a record is dead-lettered.
    pub maximum_attempts: u32,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            batch_size: 100,
            claim_ttl: Duration::from_secs(30),
            initial_retry_delay: Duration::from_secs(1),
            maximum_retry_delay: Duration::from_secs(60 * 60),
            maximum_attempts: 10,
        }
    }
}

impl DispatcherConfig {
    pub(crate) fn validate(&self) -> Result<(), OutboxError> {
        if self.batch_size == 0 || self.batch_size > 10_000 {
            return Err(OutboxError::InvalidConfiguration(
                "batch_size must be between 1 and 10000".to_owned(),
            ));
        }
        if self.claim_ttl.is_zero() {
            return Err(OutboxError::InvalidConfiguration(
                "claim_ttl must be positive".to_owned(),
            ));
        }
        if self.initial_retry_delay.is_zero() {
            return Err(OutboxError::InvalidConfiguration(
                "initial_retry_delay must be positive".to_owned(),
            ));
        }
        if self.maximum_retry_delay < self.initial_retry_delay {
            return Err(OutboxError::InvalidConfiguration(
                "maximum_retry_delay cannot be shorter than initial_retry_delay".to_owned(),
            ));
        }
        if self.maximum_attempts == 0 {
            return Err(OutboxError::InvalidConfiguration(
                "maximum_attempts must be positive".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Outcome of one bounded dispatcher invocation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DispatchReport {
    /// Records claimed in the initial short transaction.
    pub claimed: u32,
    /// Records published and acknowledged.
    pub published: u32,
    /// Failed records made available for a later retry.
    pub retry_scheduled: u32,
    /// Failed records moved to a terminal dead-letter state.
    pub dead_lettered: u32,
    /// Acknowledgments rejected because the dispatcher's claim was fenced.
    pub fenced: u32,
}

/// Claims durable records, publishes after commit, and records each outcome.
pub struct OutboxDispatcher<P> {
    pool: PgPool,
    holder: String,
    publisher: P,
    config: DispatcherConfig,
}

impl<P> OutboxDispatcher<P>
where
    P: EventPublisher,
{
    /// Creates a bounded dispatcher with a stable worker-holder name.
    ///
    /// # Errors
    ///
    /// Returns [`OutboxError::InvalidConfiguration`] for an empty holder or an
    /// unsafe retry/claim policy.
    pub fn new(
        pool: &VerifiedV2Pool,
        holder: impl Into<String>,
        publisher: P,
        config: DispatcherConfig,
    ) -> Result<Self, OutboxError> {
        config.validate()?;
        let holder = holder.into();
        if holder.is_empty()
            || holder.len() > 200
            || holder.trim() != holder
            || holder.chars().any(char::is_control)
        {
            return Err(OutboxError::InvalidConfiguration(
                "holder must contain 1 to 200 printable characters".to_owned(),
            ));
        }
        Ok(Self {
            pool: pool.pool().clone(),
            holder,
            publisher,
            config,
        })
    }

    /// Dispatches at most `config.batch_size` events.
    ///
    /// Claiming commits before the first call to [`EventPublisher::publish`].
    /// Publication is therefore at least once and never holds a PostgreSQL
    /// transaction open across transport I/O.
    pub async fn dispatch_batch(&self) -> Result<DispatchReport, OutboxError> {
        let store = PgOutboxStore::from_verified_pool(self.pool.clone());
        let claims: Vec<OutboxClaim> = store
            .claim_batch(&self.holder, self.config.batch_size, self.config.claim_ttl)
            .await?;
        let mut report = DispatchReport {
            claimed: claims.len() as u32,
            ..DispatchReport::default()
        };

        for claim in claims {
            match self.publisher.publish(&claim.event).await {
                Ok(()) => {
                    if store.acknowledge(&claim).await? {
                        report.published += 1;
                    } else {
                        report.fenced += 1;
                    }
                }
                Err(_error) => {
                    let outcome = store
                        .record_failure(
                            &claim,
                            self.config.maximum_attempts,
                            retry_delay(
                                self.config.initial_retry_delay,
                                self.config.maximum_retry_delay,
                                claim.attempts,
                            ),
                        )
                        .await?;
                    match outcome {
                        FailureOutcome::RetryScheduled { .. } => report.retry_scheduled += 1,
                        FailureOutcome::DeadLettered => report.dead_lettered += 1,
                        FailureOutcome::Fenced => report.fenced += 1,
                    }
                }
            }
        }
        Ok(report)
    }
}

fn retry_delay(initial: Duration, maximum: Duration, attempts: u32) -> Duration {
    let exponent = attempts.saturating_sub(1).min(31);
    let multiplier = 1_u32 << exponent;
    initial.saturating_mul(multiplier).min(maximum)
}

/// A committed outbox claim owned by one dispatcher fencing token.
#[derive(Clone, Debug)]
pub struct OutboxClaim {
    /// Durable outbox-record identity.
    pub message_id: OutboxMessageId,
    /// Event supplied to the publisher.
    pub event: IntegrationEvent,
    /// Holder that owns this claim.
    pub holder: String,
    /// Monotonically increasing token fencing older claims.
    pub claim_token: i64,
    /// Number of publication attempts, including this claim.
    pub attempts: u32,
    /// Time at which another dispatcher may reclaim the event.
    pub claim_expires_at: DateTime<Utc>,
}

/// Durable outcome of recording a failed publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureOutcome {
    /// The event remains pending until the supplied instant.
    RetryScheduled {
        /// Earliest time at which another claim may be acquired.
        available_at: DateTime<Utc>,
    },
    /// The configured attempt limit was reached.
    DeadLettered,
    /// A newer claim fenced this dispatcher before it recorded the outcome.
    Fenced,
}

#[cfg(test)]
mod tests {
    use super::retry_delay;
    use std::time::Duration;

    #[test]
    fn retry_delay_is_exponential_and_bounded() {
        let initial = Duration::from_secs(2);
        let maximum = Duration::from_secs(10);
        assert_eq!(retry_delay(initial, maximum, 1), Duration::from_secs(2));
        assert_eq!(retry_delay(initial, maximum, 2), Duration::from_secs(4));
        assert_eq!(retry_delay(initial, maximum, 3), Duration::from_secs(8));
        assert_eq!(retry_delay(initial, maximum, 4), Duration::from_secs(10));
        assert_eq!(retry_delay(initial, maximum, u32::MAX), maximum);
    }
}
