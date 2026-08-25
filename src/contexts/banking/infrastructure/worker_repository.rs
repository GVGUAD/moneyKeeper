//! PostgreSQL claims and fenced completions for the durable Banking worker.

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};

use super::PgBankingStore;
use crate::{
    contexts::banking::{
        application::{
            BalanceObservedV1, BankingWorkerRepository, NormalizedProviderEvent,
            NormalizedResource, ProviderEventReadyV1, ProviderFailureClass, StatementWork,
            ValidationWork, WebhookProvisioning, WebhookReceiptWork, WebhookRegistrationWork,
        },
        domain::{
            BalanceObservationId, BankingError, CredentialEnvelope, ExternalResourceId,
            ProviderConnectionId, ProviderEventId, ProviderTransactionState, SyncJobId,
        },
    },
    integration::{IntegrationEvent, outbox::OutboxWriter, postgres::PgOutboxWriter},
    shared_kernel::{CorrelationId, CurrencyCode, EventEnvelope, EventId, Money, UserId},
};

#[async_trait]
impl BankingWorkerRepository for PgBankingStore {
    async fn claim_validation(
        &self,
        holder: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
    ) -> Result<Option<ValidationWork>, BankingError> {
        validate_claim(holder, lease_seconds)?;
        let row = sqlx::query(
            "WITH candidate AS (
                SELECT id,user_id
                FROM banking.provider_connections
                WHERE state IN ('pending','pending_credential_validation')
                  AND validation_state IN ('pending','retry_due','running')
                  AND (validation_next_retry_at IS NULL OR validation_next_retry_at <= $2)
                  AND (validation_lease_expires_at IS NULL OR validation_lease_expires_at <= $2)
                ORDER BY created_at,id
                FOR UPDATE SKIP LOCKED LIMIT 1
             )
             UPDATE banking.provider_connections connection
             SET validation_state='running',validation_attempts=validation_attempts+1,
                 validation_lease_holder=$1,validation_lease_token=validation_lease_token+1,
                 validation_lease_expires_at=$2+($3::bigint*interval '1 second'),updated_at=$2
             FROM candidate
             WHERE connection.id=candidate.id AND connection.user_id=candidate.user_id
             RETURNING connection.id,connection.user_id,connection.provider,connection.state,
                 connection.credential_generation,connection.webhook_lookup_digest,
                 connection.active_credential_ciphertext,connection.active_credential_nonce,
                 connection.active_credential_key_id,connection.active_credential_envelope_version,
                 connection.pending_credential_ciphertext,connection.pending_credential_nonce,
                 connection.pending_credential_key_id,connection.pending_credential_envelope_version,
                 connection.validation_lease_holder,connection.validation_lease_token,
                 connection.validation_attempts",
        )
        .bind(holder)
        .bind(now)
        .bind(lease_seconds)
        .fetch_optional(&self.uow.pool)
        .await
        .map_err(database)?;
        row.map(|row| {
            let replacement = row.get::<String, _>("state") == "pending_credential_validation";
            let generation = row.get::<i64, _>("credential_generation") + i64::from(replacement);
            let prefix = if replacement { "pending" } else { "active" };
            Ok(ValidationWork {
                user_id: UserId::new(row.get("user_id")),
                connection_id: ProviderConnectionId::new(row.get("id")),
                provider: row.get("provider"),
                generation,
                replacement,
                webhook_configured: row
                    .get::<Option<Vec<u8>>, _>("webhook_lookup_digest")
                    .is_some(),
                envelope: envelope(&row, prefix)?,
                holder: row.get("validation_lease_holder"),
                fencing_token: row.get("validation_lease_token"),
                attempts: row.get("validation_attempts"),
            })
        })
        .transpose()
    }

    async fn complete_validation_success(
        &self,
        work: &ValidationWork,
        active_envelope: Option<&CredentialEnvelope>,
        webhook: Option<&WebhookProvisioning>,
        resources: &[NormalizedResource],
        now: DateTime<Utc>,
    ) -> Result<bool, BankingError> {
        let mut tx = self.uow.pool.begin().await.map_err(database)?;
        let row = validation_fence(&mut tx, work, now).await?;
        let Some(row) = row else {
            tx.rollback().await.map_err(database)?;
            return Ok(false);
        };
        let current_generation: i64 = row.get("credential_generation");
        if current_generation + i64::from(work.replacement) != work.generation {
            tx.rollback().await.map_err(database)?;
            return Ok(false);
        }
        for resource in resources {
            let resource_id: uuid::Uuid = sqlx::query_scalar(
                "INSERT INTO banking.external_resources
                 (id,user_id,connection_id,external_resource_id,kind,funding_model,currency,
                  masked_label,credit_limit,discovery_state)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
                 ON CONFLICT (connection_id,external_resource_id) DO UPDATE
                 SET kind=EXCLUDED.kind,funding_model=EXCLUDED.funding_model,
                     currency=EXCLUDED.currency,masked_label=EXCLUDED.masked_label,
                     credit_limit=EXCLUDED.credit_limit,discovery_state=EXCLUDED.discovery_state,
                     version=banking.external_resources.version+1,updated_at=$11
                 RETURNING id",
            )
            .bind(uuid::Uuid::new_v4())
            .bind(work.user_id.into_uuid())
            .bind(work.connection_id.into_uuid())
            .bind(&resource.external_resource_id)
            .bind(kind(resource.kind))
            .bind(funding(resource.funding_model))
            .bind(resource.currency.as_str())
            .bind(&resource.masked_label)
            .bind(resource.credit_limit.as_ref().map(Money::amount))
            .bind(
                if matches!(
                    resource.kind,
                    crate::contexts::banking::domain::ResourceKind::Unsupported
                ) {
                    "unsupported"
                } else {
                    "active"
                },
            )
            .bind(now)
            .fetch_one(&mut *tx)
            .await
            .map_err(database)?;
            insert_observation(
                &mut tx,
                work.user_id,
                work.connection_id,
                ExternalResourceId::new(resource_id),
                &resource.provider_balance,
                matches!(
                    resource.funding_model,
                    crate::contexts::banking::domain::FundingModel::OwnFunds
                ),
                now,
            )
            .await?;
        }
        let updated = if work.replacement {
            let Some(active) = active_envelope else {
                return Err(BankingError::CredentialUnavailable);
            };
            sqlx::query(
                "UPDATE banking.provider_connections SET
                    active_credential_ciphertext=$6,active_credential_nonce=$7,
                    active_credential_key_id=$8,active_credential_envelope_version=$9,
                    pending_credential_ciphertext=NULL,pending_credential_nonce=NULL,
                    pending_credential_key_id=NULL,pending_credential_envelope_version=NULL,
                    credential_generation=$5,state='active',validation_state='succeeded',
                    validation_next_retry_at=NULL,validation_last_error=NULL,
                    validation_lease_holder=NULL,validation_lease_expires_at=NULL,
                    webhook_registration_state=CASE WHEN webhook_lookup_digest IS NULL THEN webhook_registration_state ELSE 'pending' END,
                    webhook_registration_attempts=CASE WHEN webhook_lookup_digest IS NULL THEN webhook_registration_attempts ELSE 0 END,
                    webhook_next_retry_at=NULL,webhook_last_error=NULL,
                    version=version+1,updated_at=$4
                 WHERE id=$1 AND user_id=$2 AND validation_lease_holder=$3
                   AND validation_lease_token=$10",
            )
            .bind(work.connection_id.into_uuid())
            .bind(work.user_id.into_uuid())
            .bind(&work.holder)
            .bind(now)
            .bind(work.generation)
            .bind(active.ciphertext())
            .bind(active.nonce())
            .bind(active.key_id())
            .bind(i16::try_from(active.envelope_version()).unwrap_or(1))
            .bind(work.fencing_token)
            .execute(&mut *tx)
            .await
            .map_err(database)?
        } else {
            sqlx::query(
                "UPDATE banking.provider_connections SET state='active',validation_state='succeeded',
                    validation_next_retry_at=NULL,validation_last_error=NULL,
                    validation_lease_holder=NULL,validation_lease_expires_at=NULL,
                    version=version+1,updated_at=$4
                 WHERE id=$1 AND user_id=$2 AND validation_lease_holder=$3
                   AND validation_lease_token=$5",
            )
            .bind(work.connection_id.into_uuid())
            .bind(work.user_id.into_uuid())
            .bind(&work.holder)
            .bind(now)
            .bind(work.fencing_token)
            .execute(&mut *tx)
            .await
            .map_err(database)?
        };
        if updated.rows_affected() != 1 {
            tx.rollback().await.map_err(database)?;
            return Ok(false);
        }
        if let Some(webhook) = webhook {
            sqlx::query(
                "UPDATE banking.provider_connections SET
                    webhook_credential_ciphertext=$3,webhook_credential_nonce=$4,
                    webhook_credential_key_id=$5,webhook_credential_envelope_version=$6,
                    webhook_lookup_digest=$7,webhook_desired_version=$8,
                    webhook_registration_state='pending',webhook_registration_attempts=0,
                    webhook_next_retry_at=NULL,webhook_last_error=NULL,updated_at=$9
                 WHERE id=$1 AND user_id=$2 AND webhook_lookup_digest IS NULL",
            )
            .bind(work.connection_id.into_uuid())
            .bind(work.user_id.into_uuid())
            .bind(webhook.envelope.ciphertext())
            .bind(webhook.envelope.nonce())
            .bind(webhook.envelope.key_id())
            .bind(i16::try_from(webhook.envelope.envelope_version()).unwrap_or(1))
            .bind(webhook.digest.as_slice())
            .bind(webhook.version)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(database)?;
        }
        tx.commit().await.map_err(database)?;
        Ok(true)
    }

    async fn complete_validation_failure(
        &self,
        work: &ValidationWork,
        class: ProviderFailureClass,
        next_retry_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<bool, BankingError> {
        let error = error_class(class);
        let retry = next_retry_at.is_some();
        let terminal_state = if work.replacement {
            "active"
        } else {
            "needs_reauth"
        };
        let mut tx = self.uow.pool.begin().await.map_err(database)?;
        let result = sqlx::query(
            "UPDATE banking.provider_connections SET
                state=CASE WHEN $6 THEN state ELSE $7 END,
                validation_state=CASE WHEN $6 THEN 'retry_due' ELSE 'failed' END,
                validation_next_retry_at=$5,validation_last_error=$8,
                validation_lease_holder=NULL,validation_lease_expires_at=NULL,
                active_credential_ciphertext=CASE WHEN NOT $6 AND NOT $9 THEN NULL ELSE active_credential_ciphertext END,
                active_credential_nonce=CASE WHEN NOT $6 AND NOT $9 THEN NULL ELSE active_credential_nonce END,
                active_credential_key_id=CASE WHEN NOT $6 AND NOT $9 THEN NULL ELSE active_credential_key_id END,
                active_credential_envelope_version=CASE WHEN NOT $6 AND NOT $9 THEN NULL ELSE active_credential_envelope_version END,
                pending_credential_ciphertext=CASE WHEN NOT $6 THEN NULL ELSE pending_credential_ciphertext END,
                pending_credential_nonce=CASE WHEN NOT $6 THEN NULL ELSE pending_credential_nonce END,
                pending_credential_key_id=CASE WHEN NOT $6 THEN NULL ELSE pending_credential_key_id END,
                pending_credential_envelope_version=CASE WHEN NOT $6 THEN NULL ELSE pending_credential_envelope_version END,
                version=version+CASE WHEN $6 THEN 0 ELSE 1 END,updated_at=$4
             WHERE id=$1 AND user_id=$2 AND validation_lease_holder=$3
               AND validation_lease_token=$10 AND validation_state='running'
               AND validation_lease_expires_at>$4",
        )
        .bind(work.connection_id.into_uuid())
        .bind(work.user_id.into_uuid())
        .bind(&work.holder)
        .bind(now)
        .bind(next_retry_at)
        .bind(retry)
        .bind(terminal_state)
        .bind(error)
        .bind(work.replacement)
        .bind(work.fencing_token)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
        if result.rows_affected() != 1 {
            tx.rollback().await.map_err(database)?;
            return Ok(false);
        }
        tx.commit().await.map_err(database)?;
        Ok(true)
    }

    async fn claim_webhook_registration(
        &self,
        holder: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
    ) -> Result<Option<WebhookRegistrationWork>, BankingError> {
        validate_claim(holder, lease_seconds)?;
        let row = sqlx::query(
            "WITH candidate AS (
                SELECT id,user_id FROM banking.provider_connections
                WHERE state='active' AND webhook_registration_state IN ('pending','retry_due')
                  AND (webhook_next_retry_at IS NULL OR webhook_next_retry_at <= $2)
                  AND (webhook_lease_expires_at IS NULL OR webhook_lease_expires_at <= $2)
                ORDER BY updated_at,id FOR UPDATE SKIP LOCKED LIMIT 1
             )
             UPDATE banking.provider_connections connection SET
                webhook_lease_holder=$1,webhook_lease_token=webhook_lease_token+1,
                webhook_lease_expires_at=$2+($3::bigint*interval '1 second'),
                webhook_registration_attempts=webhook_registration_attempts+1,updated_at=$2
             FROM candidate WHERE connection.id=candidate.id AND connection.user_id=candidate.user_id
             RETURNING connection.id,connection.user_id,connection.provider,
                connection.credential_generation,connection.webhook_desired_version,
                connection.active_credential_ciphertext,connection.active_credential_nonce,
                connection.active_credential_key_id,connection.active_credential_envelope_version,
                connection.webhook_credential_ciphertext,connection.webhook_credential_nonce,
                connection.webhook_credential_key_id,connection.webhook_credential_envelope_version,
                connection.webhook_lease_holder,connection.webhook_lease_token,
                connection.webhook_registration_attempts",
        )
        .bind(holder)
        .bind(now)
        .bind(lease_seconds)
        .fetch_optional(&self.uow.pool)
        .await
        .map_err(database)?;
        row.map(|row| {
            Ok(WebhookRegistrationWork {
                user_id: UserId::new(row.get("user_id")),
                connection_id: ProviderConnectionId::new(row.get("id")),
                provider: row.get("provider"),
                credential_generation: row.get("credential_generation"),
                webhook_version: row.get("webhook_desired_version"),
                provider_envelope: envelope(&row, "active")?,
                webhook_envelope: envelope(&row, "webhook")?,
                holder: row.get("webhook_lease_holder"),
                fencing_token: row.get("webhook_lease_token"),
                attempts: row.get("webhook_registration_attempts"),
            })
        })
        .transpose()
    }

    async fn complete_claimed_webhook_registration(
        &self,
        work: &WebhookRegistrationWork,
        failure: Option<ProviderFailureClass>,
        next_retry_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<bool, BankingError> {
        let state = if failure.is_none() {
            "registered"
        } else if next_retry_at.is_some() {
            "retry_due"
        } else {
            "failed"
        };
        let mut tx = self.uow.pool.begin().await.map_err(database)?;
        let result = sqlx::query(
            "UPDATE banking.provider_connections SET
                webhook_registration_state=$6,
                webhook_registered_version=CASE WHEN $6='registered' THEN $5 ELSE webhook_registered_version END,
                webhook_next_retry_at=$7,webhook_last_error=$8,
                webhook_lease_holder=NULL,webhook_lease_expires_at=NULL,updated_at=$4
             WHERE id=$1 AND user_id=$2 AND webhook_lease_holder=$3
               AND webhook_lease_token=$9 AND webhook_desired_version=$5
               AND webhook_lease_expires_at>$4 AND state='active'",
        )
        .bind(work.connection_id.into_uuid())
        .bind(work.user_id.into_uuid())
        .bind(&work.holder)
        .bind(now)
        .bind(work.webhook_version)
        .bind(state)
        .bind(next_retry_at)
        .bind(failure.map(error_class))
        .bind(work.fencing_token)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
        if result.rows_affected() != 1 {
            tx.rollback().await.map_err(database)?;
            return Ok(false);
        }
        if failure == Some(ProviderFailureClass::NeedsReauth) {
            sqlx::query(
                "UPDATE banking.provider_connections SET state='needs_reauth',
                 active_credential_ciphertext=NULL,active_credential_nonce=NULL,
                 active_credential_key_id=NULL,active_credential_envelope_version=NULL,
                 validation_state='failed',validation_last_error='needs_reauth',
                 validation_lease_holder=NULL,validation_lease_expires_at=NULL,
                 validation_lease_token=validation_lease_token+1,
                 version=version+1,updated_at=$3 WHERE id=$1 AND user_id=$2",
            )
            .bind(work.connection_id.into_uuid())
            .bind(work.user_id.into_uuid())
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(database)?;
            sqlx::query(
                "UPDATE banking.sync_jobs SET state='cancelled',last_error='needs_reauth',
                 lease_holder=NULL,lease_expires_at=NULL,version=version+1,updated_at=$3
                 WHERE connection_id=$1 AND user_id=$2
                   AND state IN ('requested','running','waiting_for_events','retry_due')",
            )
            .bind(work.connection_id.into_uuid())
            .bind(work.user_id.into_uuid())
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(database)?;
        }
        tx.commit().await.map_err(database)?;
        Ok(true)
    }

    async fn claim_webhook_receipt(
        &self,
        holder: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
    ) -> Result<Option<WebhookReceiptWork>, BankingError> {
        validate_claim(holder, lease_seconds)?;
        sqlx::query(
            "UPDATE banking.webhook_receipts SET state='quarantined',processed_at=$1,
             last_error='payload_unavailable',lease_holder=NULL,lease_expires_at=NULL
             WHERE state='pending' AND provenance_ciphertext IS NULL",
        )
        .bind(now)
        .execute(&self.uow.pool)
        .await
        .map_err(database)?;
        let row = sqlx::query(
            "WITH candidate AS (
                SELECT receipt.id,receipt.user_id
                FROM banking.webhook_receipts receipt
                WHERE receipt.state='pending'
                  AND (receipt.next_retry_at IS NULL OR receipt.next_retry_at <= $2)
                  AND (receipt.lease_expires_at IS NULL OR receipt.lease_expires_at <= $2)
                ORDER BY receipt.received_at,receipt.id FOR UPDATE SKIP LOCKED LIMIT 1
             )
             UPDATE banking.webhook_receipts receipt SET
                lease_holder=$1,lease_token=lease_token+1,
                lease_expires_at=$2+($3::bigint*interval '1 second'),attempts=attempts+1
             FROM candidate WHERE receipt.id=candidate.id AND receipt.user_id=candidate.user_id
             RETURNING receipt.id,receipt.user_id,receipt.connection_id,
                receipt.provenance_ciphertext,receipt.provenance_nonce,
                receipt.provenance_key_id,receipt.provenance_envelope_version,
                receipt.provenance_generation,
                receipt.lease_holder,receipt.lease_token,receipt.attempts",
        )
        .bind(holder)
        .bind(now)
        .bind(lease_seconds)
        .fetch_optional(&self.uow.pool)
        .await
        .map_err(database)?;
        let Some(row) = row else { return Ok(None) };
        let connection = sqlx::query(
            "SELECT provider
             FROM banking.provider_connections WHERE id=$1 AND user_id=$2",
        )
        .bind(row.get::<uuid::Uuid, _>("connection_id"))
        .bind(row.get::<uuid::Uuid, _>("user_id"))
        .fetch_one(&self.uow.pool)
        .await
        .map_err(database)?;
        Ok(Some(WebhookReceiptWork {
            receipt_id: row.get("id"),
            user_id: UserId::new(row.get("user_id")),
            connection_id: ProviderConnectionId::new(row.get("connection_id")),
            provider: connection.get("provider"),
            binding_generation: row.get("provenance_generation"),
            envelope: envelope(&row, "provenance")?,
            holder: row.get("lease_holder"),
            fencing_token: row.get("lease_token"),
        }))
    }

    async fn webhook_resource_currency(
        &self,
        work: &WebhookReceiptWork,
        external_resource_id: &str,
    ) -> Result<(CurrencyCode, bool), BankingError> {
        let row = sqlx::query(
            "SELECT currency,funding_model FROM banking.external_resources
             WHERE user_id=$1 AND connection_id=$2 AND external_resource_id=$3
               AND discovery_state IN ('active','needs_review')",
        )
        .bind(work.user_id.into_uuid())
        .bind(work.connection_id.into_uuid())
        .bind(external_resource_id)
        .fetch_optional(&self.uow.pool)
        .await
        .map_err(database)?
        .ok_or(BankingError::InvalidState)?;
        let currency = CurrencyCode::new(row.get::<String, _>("currency"))
            .map_err(|_| BankingError::InvalidValue("stored resource currency is invalid"))?;
        Ok((
            currency,
            row.get::<String, _>("funding_model") == "own_funds",
        ))
    }

    async fn complete_webhook_receipt(
        &self,
        work: &WebhookReceiptWork,
        event: Result<(String, NormalizedProviderEvent), &'static str>,
        now: DateTime<Utc>,
    ) -> Result<bool, BankingError> {
        let mut tx = self.uow.pool.begin().await.map_err(database)?;
        let fenced = sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT id FROM banking.webhook_receipts
             WHERE id=$1 AND user_id=$2 AND state='pending' AND lease_holder=$3
               AND lease_token=$4 AND lease_expires_at>$5 FOR UPDATE",
        )
        .bind(work.receipt_id)
        .bind(work.user_id.into_uuid())
        .bind(&work.holder)
        .bind(work.fencing_token)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database)?;
        if fenced.is_none() {
            tx.rollback().await.map_err(database)?;
            return Ok(false);
        }
        match event {
            Ok((external_id, event)) => {
                let resource = sqlx::query(
                    "SELECT id,funding_model FROM banking.external_resources
                     WHERE user_id=$1 AND connection_id=$2 AND external_resource_id=$3 FOR UPDATE",
                )
                .bind(work.user_id.into_uuid())
                .bind(work.connection_id.into_uuid())
                .bind(&external_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(database)?
                .ok_or(BankingError::InvalidState)?;
                intake_normalized(
                    &mut tx,
                    work.user_id,
                    work.connection_id,
                    ExternalResourceId::new(resource.get("id")),
                    &event,
                    resource.get::<String, _>("funding_model") == "own_funds",
                    now,
                )
                .await?;
                sqlx::query(
                    "UPDATE banking.webhook_receipts SET state='processed',processed_at=$5,
                     last_error=NULL,lease_holder=NULL,lease_expires_at=NULL
                     WHERE id=$1 AND user_id=$2 AND lease_holder=$3 AND lease_token=$4",
                )
                .bind(work.receipt_id)
                .bind(work.user_id.into_uuid())
                .bind(&work.holder)
                .bind(work.fencing_token)
                .bind(now)
                .execute(&mut *tx)
                .await
                .map_err(database)?;
            }
            Err(reason) => {
                sqlx::query(
                    "UPDATE banking.webhook_receipts SET state='quarantined',processed_at=$5,
                     last_error=$6,lease_holder=NULL,lease_expires_at=NULL
                     WHERE id=$1 AND user_id=$2 AND lease_holder=$3 AND lease_token=$4",
                )
                .bind(work.receipt_id)
                .bind(work.user_id.into_uuid())
                .bind(&work.holder)
                .bind(work.fencing_token)
                .bind(now)
                .bind(reason)
                .execute(&mut *tx)
                .await
                .map_err(database)?;
            }
        }
        tx.commit().await.map_err(database)?;
        Ok(true)
    }

    async fn claim_statement_window(
        &self,
        holder: &str,
        now: DateTime<Utc>,
        lease_seconds: i64,
    ) -> Result<Option<StatementWork>, BankingError> {
        validate_claim(holder, lease_seconds)?;
        sqlx::query(
            "INSERT INTO banking.sync_job_resources
             (sync_job_id,user_id,connection_id,external_resource_id,position,
              snapshot_from,snapshot_to,next_from)
             SELECT job.id,job.user_id,job.connection_id,resource.id,
                    row_number() OVER (PARTITION BY job.id ORDER BY resource.created_at,resource.id)::integer,
                    job.requested_from-(job.overlap_seconds::bigint*interval '1 second'),
                    job.requested_to,
                    job.requested_from-(job.overlap_seconds::bigint*interval '1 second')
             FROM banking.sync_jobs job
             JOIN banking.external_resources resource
               ON resource.user_id=job.user_id AND resource.connection_id=job.connection_id
              AND (job.resource_id IS NULL OR resource.id=job.resource_id)
             WHERE job.state IN ('requested','running','retry_due')
               AND resource.kind IN ('card','current_account','jar')
               AND resource.discovery_state IN ('active','needs_review')
               AND NOT EXISTS (SELECT 1 FROM banking.sync_job_resources snapshot
                   WHERE snapshot.sync_job_id=job.id AND snapshot.user_id=job.user_id)
             ON CONFLICT DO NOTHING",
        )
        .execute(&self.uow.pool)
        .await
        .map_err(database)?;
        let mut tx = self.uow.pool.begin().await.map_err(database)?;
        let row = sqlx::query(
            "SELECT job.id sync_job_id,job.user_id,job.connection_id,job.attempts,
                    resource.external_resource_id resource_id,resource.next_from,
                    resource.snapshot_to,external.external_resource_id,external.currency,
                    external.funding_model,
                    connection.provider,connection.credential_generation,
                    connection.active_credential_ciphertext,connection.active_credential_nonce,
                    connection.active_credential_key_id,connection.active_credential_envelope_version
             FROM banking.sync_jobs job
             JOIN banking.provider_connections connection
               ON connection.id=job.connection_id AND connection.user_id=job.user_id
             JOIN LATERAL (
                SELECT candidate.external_resource_id,candidate.next_from,candidate.snapshot_to
                FROM banking.sync_job_resources candidate
                WHERE candidate.sync_job_id=job.id AND candidate.user_id=job.user_id
                  AND candidate.state='requested'
                ORDER BY candidate.position LIMIT 1
             ) resource ON true
             JOIN banking.external_resources external
               ON external.id=resource.external_resource_id AND external.user_id=job.user_id
             WHERE job.state IN ('requested','retry_due','running')
               AND (job.next_retry_at IS NULL OR job.next_retry_at <= $2)
               AND (job.lease_expires_at IS NULL OR job.lease_expires_at <= $2)
               AND connection.state='active' AND connection.version=job.connection_version
               AND connection.credential_generation=job.credential_generation
               AND (connection.statement_next_request_at IS NULL OR connection.statement_next_request_at <= $2)
               AND NOT EXISTS (SELECT 1 FROM banking.sync_jobs other
                   WHERE other.connection_id=job.connection_id AND other.id<>job.id
                     AND other.lease_expires_at>$2)
             ORDER BY job.created_at,job.id
             FOR UPDATE OF job,connection SKIP LOCKED LIMIT 1",
        )
        .bind(holder)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database)?;
        let Some(row) = row else {
            tx.rollback().await.map_err(database)?;
            return Ok(None);
        };
        let sync_job_id: uuid::Uuid = row.get("sync_job_id");
        let user_id: uuid::Uuid = row.get("user_id");
        let claimed = sqlx::query(
            "UPDATE banking.sync_jobs SET state='running',lease_holder=$3,
             lease_token=lease_token+1,lease_expires_at=$4+($5::bigint*interval '1 second'),
             attempts=attempts+1,updated_at=$4 WHERE id=$1 AND user_id=$2
             RETURNING lease_token,attempts",
        )
        .bind(sync_job_id)
        .bind(user_id)
        .bind(holder)
        .bind(now)
        .bind(lease_seconds)
        .fetch_one(&mut *tx)
        .await
        .map_err(database)?;
        sqlx::query(
            "UPDATE banking.provider_connections
             SET statement_next_request_at=$3+interval '61 seconds'
             WHERE id=$1 AND user_id=$2",
        )
        .bind(row.get::<uuid::Uuid, _>("connection_id"))
        .bind(user_id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
        tx.commit().await.map_err(database)?;
        let from: DateTime<Utc> = row.get("next_from");
        let snapshot_to: DateTime<Utc> = row.get("snapshot_to");
        let to = (from + Duration::days(30)).min(snapshot_to);
        Ok(Some(StatementWork {
            user_id: UserId::new(user_id),
            connection_id: ProviderConnectionId::new(row.get("connection_id")),
            sync_job_id: SyncJobId::new(sync_job_id),
            resource_id: ExternalResourceId::new(row.get("resource_id")),
            external_resource_id: row.get("external_resource_id"),
            resource_currency: CurrencyCode::new(row.get::<String, _>("currency"))
                .map_err(|_| BankingError::InvalidValue("stored resource currency is invalid"))?,
            from,
            to,
            snapshot_to,
            balance_comparable: row.get::<String, _>("funding_model") == "own_funds",
            provider: row.get("provider"),
            credential_generation: row.get("credential_generation"),
            provider_envelope: envelope(&row, "active")?,
            holder: holder.to_owned(),
            fencing_token: claimed.get("lease_token"),
            attempts: claimed.get("attempts"),
        }))
    }

    async fn complete_statement_window(
        &self,
        work: &StatementWork,
        events: &[NormalizedProviderEvent],
        now: DateTime<Utc>,
    ) -> Result<u32, BankingError> {
        let mut tx = self.uow.pool.begin().await.map_err(database)?;
        let fenced = sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT job.id FROM banking.sync_jobs job
             JOIN banking.provider_connections connection
               ON connection.id=job.connection_id AND connection.user_id=job.user_id
             WHERE job.id=$1 AND job.user_id=$2 AND job.lease_holder=$3
               AND job.lease_token=$4 AND job.lease_expires_at>$5
               AND connection.state='active' AND connection.version=job.connection_version
               AND connection.credential_generation=job.credential_generation FOR UPDATE OF job",
        )
        .bind(work.sync_job_id.into_uuid())
        .bind(work.user_id.into_uuid())
        .bind(&work.holder)
        .bind(work.fencing_token)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database)?;
        if fenced.is_none() {
            tx.rollback().await.map_err(database)?;
            return Err(BankingError::LeaseFenced);
        }
        let page_id = uuid::Uuid::new_v4();
        let page_number: i64 = sqlx::query_scalar(
            "SELECT COALESCE(max(page_number),0)+1 FROM banking.sync_pages
             WHERE sync_job_id=$1 AND user_id=$2",
        )
        .bind(work.sync_job_id.into_uuid())
        .bind(work.user_id.into_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(database)?;
        sqlx::query(
            "INSERT INTO banking.sync_pages
             (id,user_id,connection_id,sync_job_id,page_number,provider_cursor,next_cursor,
              expected_events,state,external_resource_id,window_from,window_to)
             VALUES ($1,$2,$3,$4,$5,$6,$7,0,'intaking',$8,$9,$10)",
        )
        .bind(page_id)
        .bind(work.user_id.into_uuid())
        .bind(work.connection_id.into_uuid())
        .bind(work.sync_job_id.into_uuid())
        .bind(page_number)
        .bind(work.from.timestamp().to_string())
        .bind(
            (work.to < work.snapshot_to)
                .then(|| (work.to + Duration::seconds(1)).timestamp().to_string()),
        )
        .bind(work.resource_id.into_uuid())
        .bind(work.from)
        .bind(work.to)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
        for event in events {
            let receipt = intake_normalized(
                &mut tx,
                work.user_id,
                work.connection_id,
                work.resource_id,
                event,
                work.balance_comparable,
                now,
            )
            .await?;
            sqlx::query(
                "INSERT INTO banking.sync_page_events
                 (sync_page_id,sync_job_id,user_id,provider_event_id,intake_outcome)
                 VALUES ($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",
            )
            .bind(page_id)
            .bind(work.sync_job_id.into_uuid())
            .bind(work.user_id.into_uuid())
            .bind(receipt.0.into_uuid())
            .bind(receipt.1)
            .execute(&mut *tx)
            .await
            .map_err(database)?;
        }
        let expected: i32 = sqlx::query_scalar(
            "SELECT count(*)::integer FROM banking.sync_page_events
             WHERE sync_page_id=$1 AND user_id=$2",
        )
        .bind(page_id)
        .bind(work.user_id.into_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(database)?;
        sqlx::query(
            "UPDATE banking.sync_pages SET expected_events=$3,state='waiting_for_events',updated_at=$4
             WHERE id=$1 AND user_id=$2",
        )
        .bind(page_id)
        .bind(work.user_id.into_uuid())
        .bind(expected)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
        sqlx::query(
            "UPDATE banking.sync_jobs SET state='waiting_for_events',attempts=0,
             next_retry_at=NULL,last_error=NULL,lease_holder=NULL,lease_expires_at=NULL,updated_at=$5
             WHERE id=$1 AND user_id=$2 AND lease_holder=$3 AND lease_token=$4",
        )
        .bind(work.sync_job_id.into_uuid())
        .bind(work.user_id.into_uuid())
        .bind(&work.holder)
        .bind(work.fencing_token)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
        tx.commit().await.map_err(database)?;
        Ok(u32::try_from(expected).unwrap_or(0))
    }

    async fn fail_statement_window(
        &self,
        work: &StatementWork,
        class: ProviderFailureClass,
        next_retry_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<bool, BankingError> {
        let mut tx = self.uow.pool.begin().await.map_err(database)?;
        let job_state = if next_retry_at.is_some() {
            "retry_due"
        } else {
            "failed"
        };
        let updated = sqlx::query(
            "UPDATE banking.sync_jobs AS job SET state=$6,next_retry_at=$7,last_error=$8,
             lease_holder=NULL,lease_expires_at=NULL,updated_at=$5
             WHERE id=$1 AND user_id=$2 AND lease_holder=$3 AND lease_token=$4
               AND lease_expires_at>$5
               AND EXISTS (SELECT 1 FROM banking.provider_connections connection
                   WHERE connection.id=job.connection_id
                     AND connection.user_id=job.user_id
                     AND connection.state='active'
                     AND connection.version=job.connection_version
                     AND connection.credential_generation=job.credential_generation)",
        )
        .bind(work.sync_job_id.into_uuid())
        .bind(work.user_id.into_uuid())
        .bind(&work.holder)
        .bind(work.fencing_token)
        .bind(now)
        .bind(job_state)
        .bind(next_retry_at)
        .bind(error_class(class))
        .execute(&mut *tx)
        .await
        .map_err(database)?;
        if updated.rows_affected() != 1 {
            tx.rollback().await.map_err(database)?;
            return Ok(false);
        }
        if class == ProviderFailureClass::NeedsReauth {
            sqlx::query(
                "UPDATE banking.provider_connections SET state='needs_reauth',
                 active_credential_ciphertext=NULL,active_credential_nonce=NULL,
                 active_credential_key_id=NULL,active_credential_envelope_version=NULL,
                 validation_state='failed',validation_last_error='needs_reauth',
                 validation_lease_holder=NULL,validation_lease_expires_at=NULL,
                 validation_lease_token=validation_lease_token+1,
                 webhook_registration_state='failed',webhook_last_error='needs_reauth',
                 webhook_lease_holder=NULL,webhook_lease_expires_at=NULL,
                 webhook_lease_token=webhook_lease_token+1,version=version+1,updated_at=$3
                 WHERE id=$1 AND user_id=$2 AND credential_generation=$4",
            )
            .bind(work.connection_id.into_uuid())
            .bind(work.user_id.into_uuid())
            .bind(now)
            .bind(work.credential_generation)
            .execute(&mut *tx)
            .await
            .map_err(database)?;
            sqlx::query(
                "UPDATE banking.sync_jobs SET state='cancelled',last_error='needs_reauth',
                 lease_holder=NULL,lease_expires_at=NULL,version=version+1,updated_at=$3
                 WHERE connection_id=$1 AND user_id=$2 AND id<>$4
                   AND state IN ('requested','running','waiting_for_events','retry_due')",
            )
            .bind(work.connection_id.into_uuid())
            .bind(work.user_id.into_uuid())
            .bind(now)
            .bind(work.sync_job_id.into_uuid())
            .execute(&mut *tx)
            .await
            .map_err(database)?;
        }
        tx.commit().await.map_err(database)?;
        Ok(true)
    }

    async fn finalize_one_sync_page(&self, now: DateTime<Utc>) -> Result<bool, BankingError> {
        let mut tx = self.uow.pool.begin().await.map_err(database)?;
        let page = sqlx::query(
            "SELECT page.id,page.user_id,page.sync_job_id,page.external_resource_id,
                    page.window_to,resource.snapshot_to
             FROM banking.sync_pages page
             JOIN banking.sync_job_resources resource
               ON resource.sync_job_id=page.sync_job_id AND resource.user_id=page.user_id
              AND resource.external_resource_id=page.external_resource_id
             WHERE page.state='waiting_for_events' AND page.external_resource_id IS NOT NULL
               AND NOT EXISTS (
                 SELECT 1 FROM banking.sync_page_events link
                 LEFT JOIN banking.provider_event_processes process
                   ON process.provider_event_id=link.provider_event_id AND process.user_id=link.user_id
                 WHERE link.sync_page_id=page.id AND link.user_id=page.user_id
                   AND (process.provider_event_id IS NULL
                     OR process.state NOT IN ('posted','no_financial_change','quarantined'))
               )
             ORDER BY page.created_at,page.id FOR UPDATE OF page,resource SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(database)?;
        let Some(page) = page else {
            tx.rollback().await.map_err(database)?;
            return Ok(false);
        };
        let counts = sqlx::query(
            "SELECT count(*) FILTER (WHERE process.state IN ('posted','no_financial_change'))::integer processed,
                    count(*) FILTER (WHERE process.state='quarantined')::integer quarantined
             FROM banking.sync_page_events link
             JOIN banking.provider_event_processes process
               ON process.provider_event_id=link.provider_event_id AND process.user_id=link.user_id
             WHERE link.sync_page_id=$1 AND link.user_id=$2",
        )
        .bind(page.get::<uuid::Uuid, _>("id"))
        .bind(page.get::<uuid::Uuid, _>("user_id"))
        .fetch_one(&mut *tx)
        .await
        .map_err(database)?;
        sqlx::query(
            "UPDATE banking.sync_pages SET processed_events=$3,quarantined_events=$4,
             state='completed',completed_at=$5,updated_at=$5 WHERE id=$1 AND user_id=$2",
        )
        .bind(page.get::<uuid::Uuid, _>("id"))
        .bind(page.get::<uuid::Uuid, _>("user_id"))
        .bind(counts.get::<i32, _>("processed"))
        .bind(counts.get::<i32, _>("quarantined"))
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
        let window_to: DateTime<Utc> = page.get("window_to");
        let snapshot_to: DateTime<Utc> = page.get("snapshot_to");
        sqlx::query(
            "UPDATE banking.sync_job_resources SET
                state=CASE WHEN $5 >= snapshot_to THEN 'completed' ELSE 'requested' END,
                next_from=CASE WHEN $5 >= snapshot_to THEN snapshot_to ELSE LEAST($5+interval '1 second',snapshot_to) END,
                updated_at=$6
             WHERE sync_job_id=$1 AND user_id=$2 AND external_resource_id=$3 AND snapshot_to=$4",
        )
        .bind(page.get::<uuid::Uuid, _>("sync_job_id"))
        .bind(page.get::<uuid::Uuid, _>("user_id"))
        .bind(page.get::<uuid::Uuid, _>("external_resource_id"))
        .bind(snapshot_to)
        .bind(window_to)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
        let next_cursor: Option<String> = sqlx::query_scalar(
            "SELECT external_resource_id::text || ':' || extract(epoch FROM next_from)::bigint::text
             FROM banking.sync_job_resources
             WHERE sync_job_id=$1 AND user_id=$2 AND state='requested'
             ORDER BY position LIMIT 1",
        )
        .bind(page.get::<uuid::Uuid, _>("sync_job_id"))
        .bind(page.get::<uuid::Uuid, _>("user_id"))
        .fetch_one(&mut *tx)
        .await
        .map_err(database)?;
        sqlx::query(
            "UPDATE banking.sync_jobs SET state=$3,cursor=$4,lease_holder=NULL,
             lease_expires_at=NULL,version=version+1,updated_at=$5
             WHERE id=$1 AND user_id=$2 AND state='waiting_for_events'",
        )
        .bind(page.get::<uuid::Uuid, _>("sync_job_id"))
        .bind(page.get::<uuid::Uuid, _>("user_id"))
        .bind(if next_cursor.is_some() {
            "requested"
        } else {
            "completed"
        })
        .bind(&next_cursor)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(database)?;
        tx.commit().await.map_err(database)?;
        Ok(true)
    }
}

async fn validation_fence(
    tx: &mut Transaction<'_, Postgres>,
    work: &ValidationWork,
    now: DateTime<Utc>,
) -> Result<Option<sqlx::postgres::PgRow>, BankingError> {
    sqlx::query(
        "SELECT credential_generation FROM banking.provider_connections
         WHERE id=$1 AND user_id=$2 AND validation_state='running'
           AND validation_lease_holder=$3 AND validation_lease_token=$4
           AND validation_lease_expires_at>$5 FOR UPDATE",
    )
    .bind(work.connection_id.into_uuid())
    .bind(work.user_id.into_uuid())
    .bind(&work.holder)
    .bind(work.fencing_token)
    .bind(now)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database)
}

async fn intake_normalized(
    tx: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    connection_id: ProviderConnectionId,
    resource_id: ExternalResourceId,
    event: &NormalizedProviderEvent,
    balance_comparable: bool,
    now: DateTime<Utc>,
) -> Result<(ProviderEventId, &'static str), BankingError> {
    sqlx::query(
        "SELECT id FROM banking.external_resources
         WHERE id=$1 AND user_id=$2 AND connection_id=$3 FOR UPDATE",
    )
    .bind(resource_id.into_uuid())
    .bind(user_id.into_uuid())
    .bind(connection_id.into_uuid())
    .fetch_one(&mut **tx)
    .await
    .map_err(database)?;
    let digest = normalized_digest(connection_id, resource_id, event)?;
    let latest = sqlx::query(
        "SELECT id,revision,content_digest FROM banking.provider_events
         WHERE connection_id=$1 AND external_resource_id=$2 AND external_event_id=$3
           AND user_id=$4
         ORDER BY revision DESC LIMIT 1",
    )
    .bind(connection_id.into_uuid())
    .bind(resource_id.into_uuid())
    .bind(&event.external_event_id)
    .bind(user_id.into_uuid())
    .fetch_optional(&mut **tx)
    .await
    .map_err(database)?;
    if let Some(latest) = &latest
        && latest.get::<Vec<u8>, _>("content_digest") == digest
    {
        return Ok((ProviderEventId::new(latest.get("id")), "duplicate"));
    }
    let revision = latest
        .as_ref()
        .map(|row| row.get::<i64, _>("revision") + 1)
        .unwrap_or(1);
    let id = ProviderEventId::generate();
    sqlx::query(
        "INSERT INTO banking.provider_events
         (id,user_id,connection_id,external_resource_id,external_event_id,revision,
          transaction_state,original_amount,original_currency,operation_amount,
          operation_currency,description,merchant_mcc,content_digest,effective_at,recorded_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)",
    )
    .bind(id.into_uuid())
    .bind(user_id.into_uuid())
    .bind(connection_id.into_uuid())
    .bind(resource_id.into_uuid())
    .bind(&event.external_event_id)
    .bind(revision)
    .bind(transaction_state(event.state))
    .bind(event.original_money.as_ref().map(Money::amount))
    .bind(
        event
            .original_money
            .as_ref()
            .map(|money| money.currency().as_str()),
    )
    .bind(event.operation_money.amount())
    .bind(event.operation_money.currency().as_str())
    .bind(&event.description)
    .bind(event.merchant_mcc)
    .bind(&digest)
    .bind(event.effective_at.min(now))
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(database)?;
    sqlx::query(
        "INSERT INTO banking.provider_event_processes (provider_event_id,user_id,state)
         VALUES ($1,$2,'ready')",
    )
    .bind(id.into_uuid())
    .bind(user_id.into_uuid())
    .execute(&mut **tx)
    .await
    .map_err(database)?;
    append_provider_event(
        tx,
        ProviderEventPublication {
            user_id,
            connection_id,
            resource_id,
            id,
            event,
            revision,
            now,
        },
    )
    .await?;
    if let Some(balance) = &event.running_balance {
        insert_observation(
            tx,
            user_id,
            connection_id,
            resource_id,
            balance,
            balance_comparable,
            now,
        )
        .await?;
    }
    Ok((id, "new"))
}

struct ProviderEventPublication<'a> {
    user_id: UserId,
    connection_id: ProviderConnectionId,
    resource_id: ExternalResourceId,
    id: ProviderEventId,
    event: &'a NormalizedProviderEvent,
    revision: i64,
    now: DateTime<Utc>,
}

async fn append_provider_event(
    tx: &mut Transaction<'_, Postgres>,
    publication: ProviderEventPublication<'_>,
) -> Result<(), BankingError> {
    let payload = ProviderEventReadyV1 {
        provider_event_id: publication.id,
        connection_id: publication.connection_id,
        resource_id: publication.resource_id,
        external_event_id: publication.event.external_event_id.clone(),
        revision: publication.revision,
    };
    let envelope = EventEnvelope::new(
        EventId::generate(),
        "banking",
        publication.id.to_string(),
        1,
        "banking.provider-event-ready.v1",
        1,
        publication.user_id,
        publication.now,
        CorrelationId::generate(),
        None,
    )
    .map_err(|_| BankingError::InvalidValue("cannot create provider event envelope"))?;
    PgOutboxWriter::from_transaction(tx)
        .append(&IntegrationEvent::new(
            envelope,
            serde_json::to_value(payload)
                .map_err(|_| BankingError::InvalidValue("cannot serialize provider event"))?,
        ))
        .await
        .map_err(|_| BankingError::InvalidValue("cannot append provider event outbox"))
}

async fn insert_observation(
    tx: &mut Transaction<'_, Postgres>,
    user_id: UserId,
    connection_id: ProviderConnectionId,
    resource_id: ExternalResourceId,
    balance: &Money,
    comparable: bool,
    now: DateTime<Utc>,
) -> Result<(), BankingError> {
    let sequence: i64 = sqlx::query_scalar(
        "SELECT COALESCE(max(source_sequence),0)+1 FROM banking.balance_observations
         WHERE external_resource_id=$1 AND user_id=$2",
    )
    .bind(resource_id.into_uuid())
    .bind(user_id.into_uuid())
    .fetch_one(&mut **tx)
    .await
    .map_err(database)?;
    let id = BalanceObservationId::generate();
    sqlx::query(
        "INSERT INTO banking.balance_observations
         (id,user_id,connection_id,external_resource_id,source_sequence,basis,
          provider_amount,provider_currency,sign_semantics,comparable_amount,
          comparable_currency,non_comparable_reason,observed_at,recorded_at)
         VALUES ($1,$2,$3,$4,$5,'reported',$6,$7,'provider_native',$8,$9,$10,$11,$11)",
    )
    .bind(id.into_uuid())
    .bind(user_id.into_uuid())
    .bind(connection_id.into_uuid())
    .bind(resource_id.into_uuid())
    .bind(sequence)
    .bind(balance.amount())
    .bind(balance.currency().as_str())
    .bind(comparable.then(|| balance.amount()))
    .bind(comparable.then(|| balance.currency().as_str()))
    .bind((!comparable).then_some("provider credit balance semantics require review"))
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(database)?;
    let delivery = if comparable {
        "pending"
    } else {
        "not_comparable"
    };
    sqlx::query(
        "INSERT INTO banking.balance_observation_deliveries (observation_id,user_id,state)
         VALUES ($1,$2,$3)",
    )
    .bind(id.into_uuid())
    .bind(user_id.into_uuid())
    .bind(delivery)
    .execute(&mut **tx)
    .await
    .map_err(database)?;
    let payload = BalanceObservedV1 {
        observation_id: id,
        resource_id,
        source_sequence: sequence,
        basis: crate::contexts::banking::domain::BalanceBasis::Reported,
        comparable,
    };
    let envelope = EventEnvelope::new(
        EventId::generate(),
        "banking",
        id.to_string(),
        1,
        "banking.balance-observed.v1",
        1,
        user_id,
        now,
        CorrelationId::generate(),
        None,
    )
    .map_err(|_| BankingError::InvalidValue("cannot create balance observation envelope"))?;
    PgOutboxWriter::from_transaction(tx)
        .append(&IntegrationEvent::new(
            envelope,
            serde_json::to_value(payload)
                .map_err(|_| BankingError::InvalidValue("cannot serialize balance observation"))?,
        ))
        .await
        .map_err(|_| BankingError::InvalidValue("cannot append balance observation outbox"))
}

fn normalized_digest(
    connection_id: ProviderConnectionId,
    resource_id: ExternalResourceId,
    event: &NormalizedProviderEvent,
) -> Result<Vec<u8>, BankingError> {
    let content = json!({
        "connection_id": connection_id,
        "resource_id": resource_id,
        "external_event_id": event.external_event_id,
        "state": event.state,
        "operation_money": event.operation_money,
        "original_money": event.original_money,
        "description": event.description,
        "merchant_mcc": event.merchant_mcc,
        "effective_at": event.effective_at,
    });
    Ok(Sha256::digest(
        serde_json::to_vec(&content)
            .map_err(|_| BankingError::InvalidValue("cannot canonicalize provider event"))?,
    )
    .to_vec())
}

fn validate_claim(holder: &str, lease_seconds: i64) -> Result<(), BankingError> {
    if holder.trim() != holder
        || holder.is_empty()
        || holder.len() > 200
        || lease_seconds <= 0
        || lease_seconds > 30
    {
        return Err(BankingError::InvalidValue("invalid Banking worker claim"));
    }
    Ok(())
}

fn envelope(row: &sqlx::postgres::PgRow, prefix: &str) -> Result<CredentialEnvelope, BankingError> {
    let infix = if prefix == "provenance" {
        "provenance".to_owned()
    } else {
        format!("{prefix}_credential")
    };
    CredentialEnvelope::new(
        row.try_get::<String, _>(format!("{infix}_key_id").as_str())
            .map_err(|_| BankingError::CredentialUnavailable)?,
        row.try_get::<Vec<u8>, _>(format!("{infix}_nonce").as_str())
            .map_err(|_| BankingError::CredentialUnavailable)?,
        row.try_get::<Vec<u8>, _>(format!("{infix}_ciphertext").as_str())
            .map_err(|_| BankingError::CredentialUnavailable)?,
    )
}

fn error_class(class: ProviderFailureClass) -> &'static str {
    match class {
        ProviderFailureClass::RateLimited => "rate_limited",
        ProviderFailureClass::Transient => "transient",
        ProviderFailureClass::NeedsReauth => "needs_reauth",
        ProviderFailureClass::Terminal => "terminal",
    }
}

fn transaction_state(state: ProviderTransactionState) -> &'static str {
    match state {
        ProviderTransactionState::Pending => "pending",
        ProviderTransactionState::Settled => "settled",
        ProviderTransactionState::Reversed => "reversed",
    }
}

fn kind(value: crate::contexts::banking::domain::ResourceKind) -> &'static str {
    match value {
        crate::contexts::banking::domain::ResourceKind::Card => "card",
        crate::contexts::banking::domain::ResourceKind::CurrentAccount => "current_account",
        crate::contexts::banking::domain::ResourceKind::Jar => "jar",
        crate::contexts::banking::domain::ResourceKind::SecurityPortfolio => "security_portfolio",
        crate::contexts::banking::domain::ResourceKind::Unsupported => "unsupported",
    }
}

fn funding(value: crate::contexts::banking::domain::FundingModel) -> &'static str {
    match value {
        crate::contexts::banking::domain::FundingModel::OwnFunds => "own_funds",
        crate::contexts::banking::domain::FundingModel::RevolvingCredit => "revolving_credit",
        crate::contexts::banking::domain::FundingModel::Unknown => "unknown",
    }
}

fn database(_: sqlx::Error) -> BankingError {
    BankingError::InvalidValue("banking persistence failed")
}
