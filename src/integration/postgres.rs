//! PostgreSQL adapters for the Finance V2 integration runtime.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{Acquire, PgConnection, PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::infrastructure::v2_db::VerifiedV2Pool;
use crate::shared_kernel::{CausationId, CorrelationId, EventEnvelope, EventId, UserId};

use super::{
    IntegrationEvent,
    inbox::{ConsumerName, InboxAction, InboxError, InboxExecutor, InboxOutcome},
    outbox::{FailureOutcome, OutboxClaim, OutboxError, OutboxMessageId, OutboxWriter},
    process_manager::{
        FencedLease, ProcessError, ProcessKey, ProcessManagerStore, ProcessState, ProcessStatus,
    },
};

impl From<sqlx::Error> for OutboxError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database {
            source: Box::new(error),
        }
    }
}

impl From<sqlx::Error> for InboxError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database {
            source: Box::new(error),
        }
    }
}

impl From<sqlx::Error> for ProcessError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database {
            source: Box::new(error),
        }
    }
}

/// Transaction-bound outbox writer used by a context unit of work.
pub struct PgOutboxWriter<'a> {
    connection: &'a mut PgConnection,
}

impl<'a> PgOutboxWriter<'a> {
    /// Borrows the PostgreSQL transaction that owns the aggregate change.
    pub fn from_transaction(transaction: &'a mut Transaction<'_, Postgres>) -> Self {
        Self {
            connection: &mut **transaction,
        }
    }
}

#[async_trait]
impl OutboxWriter for PgOutboxWriter<'_> {
    async fn append(&mut self, event: &IntegrationEvent) -> Result<(), OutboxError> {
        let aggregate_version = i64::try_from(event.envelope.aggregate_version())
            .map_err(|_| OutboxError::InvalidEvent("aggregate version exceeds BIGINT".into()))?;
        let schema_version = i32::try_from(event.envelope.schema_version())
            .map_err(|_| OutboxError::InvalidEvent("schema version exceeds INTEGER".into()))?;
        let message_id = OutboxMessageId::generate();

        sqlx::query(
            "INSERT INTO integration.outbox_messages (
                message_id, event_id, message_schema_version, context_name,
                aggregate_id, aggregate_version, event_type, user_id,
                occurred_at, correlation_id, causation_id, payload
             ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12
             )",
        )
        .bind(message_id.into_uuid())
        .bind(event.envelope.event_id().into_uuid())
        .bind(schema_version)
        .bind(event.envelope.context())
        .bind(event.envelope.aggregate_id())
        .bind(aggregate_version)
        .bind(event.envelope.event_type())
        .bind(event.envelope.user_id().into_uuid())
        .bind(event.envelope.occurred_at())
        .bind(event.envelope.correlation_id().into_uuid())
        .bind(event.envelope.causation_id().map(CausationId::into_uuid))
        .bind(&event.payload)
        .execute(&mut *self.connection)
        .await?;

        Ok(())
    }
}

/// Pool-backed store used by the dispatcher between short transactions.
#[derive(Clone)]
pub struct PgOutboxStore {
    pool: PgPool,
}

impl PgOutboxStore {
    /// Creates a dispatcher store. This type does not publish events itself.
    pub fn new(pool: &VerifiedV2Pool) -> Self {
        Self {
            pool: pool.pool().clone(),
        }
    }

    pub(crate) fn from_verified_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Claims a bounded batch and commits the claim before returning events.
    pub async fn claim_batch(
        &self,
        holder: &str,
        batch_size: u32,
        claim_ttl: Duration,
    ) -> Result<Vec<OutboxClaim>, OutboxError> {
        let ttl_millis = duration_millis(claim_ttl, "claim_ttl")?;
        let mut transaction = self.pool.begin().await?;
        let rows = sqlx::query(
            "WITH candidates AS (
                SELECT sequence
                FROM integration.outbox_messages
                WHERE published_at IS NULL
                  AND dead_lettered_at IS NULL
                  AND available_at <= clock_timestamp()
                  AND (claim_expires_at IS NULL OR claim_expires_at <= clock_timestamp())
                ORDER BY sequence, message_id
                FOR UPDATE SKIP LOCKED
                LIMIT $2
             )
             UPDATE integration.outbox_messages AS message
             SET claim_holder = $1,
                 claim_token = message.claim_token + 1,
                 claim_expires_at = clock_timestamp() + ($3::bigint * interval '1 millisecond'),
                 attempts = message.attempts + 1
             FROM candidates
             WHERE message.sequence = candidates.sequence
             RETURNING message.message_id, message.event_id,
                       message.message_schema_version, message.context_name,
                       message.aggregate_id, message.aggregate_version,
                       message.event_type, message.user_id, message.occurred_at,
                       message.correlation_id, message.causation_id,
                       message.payload, message.claim_holder,
                       message.claim_token, message.attempts,
                       message.claim_expires_at",
        )
        .bind(holder)
        .bind(i64::from(batch_size))
        .bind(ttl_millis)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;

        rows.into_iter().map(row_to_claim).collect()
    }

    /// Acknowledges a publication only while this exact claim is current.
    pub async fn acknowledge(&self, claim: &OutboxClaim) -> Result<bool, OutboxError> {
        let result = sqlx::query(
            "UPDATE integration.outbox_messages
             SET published_at = clock_timestamp(),
                 claim_holder = NULL,
                 claim_expires_at = NULL,
                 last_error = NULL
             WHERE message_id = $1
               AND claim_holder = $2
               AND claim_token = $3
               AND claim_expires_at > clock_timestamp()
               AND published_at IS NULL
               AND dead_lettered_at IS NULL",
        )
        .bind(claim.message_id.into_uuid())
        .bind(&claim.holder)
        .bind(claim.claim_token)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Records a safe failure summary and either retries or dead-letters.
    pub async fn record_failure(
        &self,
        claim: &OutboxClaim,
        maximum_attempts: u32,
        retry_delay: Duration,
    ) -> Result<FailureOutcome, OutboxError> {
        let retry_millis = duration_millis(retry_delay, "retry_delay")?;
        let maximum_attempts = i32::try_from(maximum_attempts).map_err(|_| {
            OutboxError::InvalidConfiguration("maximum_attempts exceeds INTEGER".to_owned())
        })?;
        let row = sqlx::query(
            "UPDATE integration.outbox_messages
             SET available_at = CASE
                     WHEN attempts >= $4 THEN available_at
                     ELSE clock_timestamp() + ($5::bigint * interval '1 millisecond')
                 END,
                 dead_lettered_at = CASE
                     WHEN attempts >= $4 THEN clock_timestamp()
                     ELSE NULL
                 END,
                 claim_holder = NULL,
                 claim_expires_at = NULL,
                 last_error = 'event publication failed; publisher details redacted'
             WHERE message_id = $1
               AND claim_holder = $2
               AND claim_token = $3
               AND claim_expires_at > clock_timestamp()
               AND published_at IS NULL
               AND dead_lettered_at IS NULL
             RETURNING available_at, dead_lettered_at",
        )
        .bind(claim.message_id.into_uuid())
        .bind(&claim.holder)
        .bind(claim.claim_token)
        .bind(maximum_attempts)
        .bind(retry_millis)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(FailureOutcome::Fenced);
        };
        let dead_lettered_at: Option<DateTime<Utc>> = row.try_get("dead_lettered_at")?;
        if dead_lettered_at.is_some() {
            Ok(FailureOutcome::DeadLettered)
        } else {
            Ok(FailureOutcome::RetryScheduled {
                available_at: row.try_get("available_at")?,
            })
        }
    }
}

fn duration_millis(duration: Duration, field: &'static str) -> Result<i64, OutboxError> {
    let millis = i64::try_from(duration.as_millis())
        .map_err(|_| OutboxError::InvalidConfiguration(format!("{field} is too large")))?;
    if millis == 0 {
        return Err(OutboxError::InvalidConfiguration(format!(
            "{field} must be at least one millisecond"
        )));
    }
    Ok(millis)
}

fn row_to_claim(row: sqlx::postgres::PgRow) -> Result<OutboxClaim, OutboxError> {
    let event_id: Uuid = row.try_get("event_id")?;
    let aggregate_version: i64 = row.try_get("aggregate_version")?;
    let schema_version: i32 = row.try_get("message_schema_version")?;
    let user_id: Uuid = row.try_get("user_id")?;
    let correlation_id: Uuid = row.try_get("correlation_id")?;
    let causation_id: Option<Uuid> = row.try_get("causation_id")?;
    let envelope = EventEnvelope::new(
        EventId::new(event_id),
        row.try_get::<String, _>("context_name")?,
        row.try_get::<String, _>("aggregate_id")?,
        u64::try_from(aggregate_version)
            .map_err(|_| OutboxError::InvalidEvent("negative aggregate version".into()))?,
        row.try_get::<String, _>("event_type")?,
        u32::try_from(schema_version)
            .map_err(|_| OutboxError::InvalidEvent("negative schema version".into()))?,
        UserId::new(user_id),
        row.try_get("occurred_at")?,
        CorrelationId::new(correlation_id),
        causation_id.map(CausationId::new),
    )
    .map_err(|error| OutboxError::InvalidEvent(error.to_string()))?;
    let attempts: i32 = row.try_get("attempts")?;

    Ok(OutboxClaim {
        message_id: OutboxMessageId::new(row.try_get("message_id")?),
        event: IntegrationEvent::new(envelope, row.try_get("payload")?),
        holder: row.try_get("claim_holder")?,
        claim_token: row.try_get("claim_token")?,
        attempts: u32::try_from(attempts)
            .map_err(|_| OutboxError::InvalidEvent("negative attempts".into()))?,
        claim_expires_at: row.try_get("claim_expires_at")?,
    })
}

/// Transaction-bound exactly-once inbox executor.
pub struct PgInboxExecutor<'a> {
    connection: &'a mut PgConnection,
}

impl<'a> PgInboxExecutor<'a> {
    /// Borrows the transaction that also owns the consumer's local effects.
    pub fn from_transaction(transaction: &'a mut Transaction<'_, Postgres>) -> Self {
        Self {
            connection: &mut **transaction,
        }
    }
}

#[async_trait]
impl InboxExecutor for PgInboxExecutor<'_> {
    type UnitOfWork = PgConnection;

    async fn execute_once<T>(
        &mut self,
        consumer: &ConsumerName,
        message: &IntegrationEvent,
        action: T,
    ) -> Result<InboxOutcome, InboxError>
    where
        T: for<'a> FnOnce(&'a mut Self::UnitOfWork) -> InboxAction<'a> + Send,
    {
        // A nested transaction is a savepoint when `connection` belongs to a
        // caller transaction. It guarantees an action error cannot leave a
        // committed receipt or partial local effects behind.
        let mut savepoint = self.connection.begin().await?;
        let result = sqlx::query(
            "INSERT INTO integration.inbox_receipts (
                consumer_name, message_id, event_type
             ) VALUES ($1, $2, $3)
             ON CONFLICT (consumer_name, message_id) DO NOTHING",
        )
        .bind(consumer.as_str())
        .bind(message.envelope.event_id().into_uuid())
        .bind(message.envelope.event_type())
        .execute(&mut *savepoint)
        .await?;

        if result.rows_affected() == 0 {
            savepoint.commit().await?;
            return Ok(InboxOutcome::Duplicate);
        }

        match action(&mut savepoint).await {
            Ok(()) => {
                sqlx::query(
                    "UPDATE integration.inbox_receipts
                     SET processed_at = clock_timestamp()
                     WHERE consumer_name = $1 AND message_id = $2",
                )
                .bind(consumer.as_str())
                .bind(message.envelope.event_id().into_uuid())
                .execute(&mut *savepoint)
                .await?;
                savepoint.commit().await?;
                Ok(InboxOutcome::Applied)
            }
            Err(error) => {
                savepoint.rollback().await?;
                Err(error)
            }
        }
    }
}

/// Transaction-bound process-manager state and lease adapter.
pub struct PgProcessManagerStore<'a> {
    connection: &'a mut PgConnection,
}

impl<'a> PgProcessManagerStore<'a> {
    /// Borrows the transaction that owns process state and related local work.
    pub fn from_transaction(transaction: &'a mut Transaction<'_, Postgres>) -> Self {
        Self {
            connection: &mut **transaction,
        }
    }

    /// Loads the current process state, if any.
    pub async fn load(&mut self, key: &ProcessKey) -> Result<Option<ProcessState>, ProcessError> {
        let row = sqlx::query(
            "SELECT state, status, version, next_wake_at
             FROM integration.process_instances
             WHERE process_name = $1 AND instance_key = $2",
        )
        .bind(key.process_name())
        .bind(key.instance_key())
        .fetch_optional(&mut *self.connection)
        .await?;

        row.map(|row| {
            let status = ProcessStatus::new(row.try_get::<String, _>("status")?)?;
            let version: i64 = row.try_get("version")?;
            let version = u64::try_from(version).map_err(|_| ProcessError::VersionConflict)?;
            Ok(ProcessState::rehydrate(
                key.clone(),
                row.try_get("state")?,
                status,
                version,
                row.try_get("next_wake_at")?,
            ))
        })
        .transpose()
    }
}

#[async_trait]
impl ProcessManagerStore for PgProcessManagerStore<'_> {
    async fn acquire_lease(
        &mut self,
        key: &ProcessKey,
        holder: &str,
        ttl: Duration,
    ) -> Result<FencedLease, ProcessError> {
        if holder.is_empty()
            || holder.len() > 200
            || holder.trim() != holder
            || holder.chars().any(char::is_control)
        {
            return Err(ProcessError::InvalidLease(
                "holder must contain 1 to 200 printable characters".to_owned(),
            ));
        }
        let ttl_millis = i64::try_from(ttl.as_millis())
            .map_err(|_| ProcessError::InvalidLease("ttl is too large".to_owned()))?;
        if ttl_millis == 0 {
            return Err(ProcessError::InvalidLease(
                "ttl must be positive".to_owned(),
            ));
        }

        let row = sqlx::query(
            "INSERT INTO integration.process_leases (
                process_name, instance_key, holder, expires_at, fencing_token
             ) VALUES (
                $1, $2, $3,
                clock_timestamp() + ($4::bigint * interval '1 millisecond'),
                1
             )
             ON CONFLICT (process_name, instance_key) DO UPDATE
             SET holder = EXCLUDED.holder,
                 expires_at = EXCLUDED.expires_at,
                 fencing_token = integration.process_leases.fencing_token + 1
             WHERE integration.process_leases.expires_at <= clock_timestamp()
                OR integration.process_leases.holder = EXCLUDED.holder
             RETURNING holder, expires_at, fencing_token",
        )
        .bind(key.process_name())
        .bind(key.instance_key())
        .bind(holder)
        .bind(ttl_millis)
        .fetch_optional(&mut *self.connection)
        .await?;

        let Some(row) = row else {
            return Err(ProcessError::LeaseUnavailable);
        };
        Ok(FencedLease::rehydrate(
            key.clone(),
            row.try_get("holder")?,
            row.try_get("fencing_token")?,
            row.try_get("expires_at")?,
        ))
    }

    async fn save(
        &mut self,
        state: &ProcessState,
        lease: &FencedLease,
    ) -> Result<(), ProcessError> {
        if state.key() != lease.key() {
            return Err(ProcessError::LeaseFenced);
        }
        let expected_version =
            i64::try_from(state.version()).map_err(|_| ProcessError::VersionConflict)?;
        // Lock the exact lease row first. A successor cannot advance the token
        // until the caller's transaction commits, so the following CAS write
        // is protected by the same fencing decision.
        let lease_token = sqlx::query_scalar::<_, i64>(
            "SELECT fencing_token
             FROM integration.process_leases
             WHERE process_name = $1
               AND instance_key = $2
               AND holder = $3
               AND fencing_token = $4
               AND expires_at > clock_timestamp()
             FOR UPDATE",
        )
        .bind(state.key().process_name())
        .bind(state.key().instance_key())
        .bind(lease.holder())
        .bind(lease.fencing_token())
        .fetch_optional(&mut *self.connection)
        .await?;
        if lease_token.is_none() {
            return Err(ProcessError::LeaseFenced);
        }

        let result = if expected_version == 0 {
            sqlx::query(
                "INSERT INTO integration.process_instances (
                    process_name, instance_key, state, status, version,
                    next_wake_at
                 ) VALUES ($1, $2, $3, $4, 1, $5)
                 ON CONFLICT (process_name, instance_key) DO NOTHING",
            )
            .bind(state.key().process_name())
            .bind(state.key().instance_key())
            .bind(state.state())
            .bind(state.status().as_str())
            .bind(state.next_wake_at())
            .execute(&mut *self.connection)
            .await?
        } else {
            sqlx::query(
                "UPDATE integration.process_instances
                 SET state = $4,
                     status = $5,
                     version = version + 1,
                     next_wake_at = $6,
                     updated_at = clock_timestamp()
                 WHERE process_name = $1
                   AND instance_key = $2
                   AND version = $3",
            )
            .bind(state.key().process_name())
            .bind(state.key().instance_key())
            .bind(expected_version)
            .bind(state.state())
            .bind(state.status().as_str())
            .bind(state.next_wake_at())
            .execute(&mut *self.connection)
            .await?
        };

        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(ProcessError::VersionConflict)
        }
    }
}
