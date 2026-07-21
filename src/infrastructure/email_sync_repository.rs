use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::email_sync::{
    EmailMessageFailure, EmailSyncRepository, MANUAL_RESYNC_IGNORED_REASONS,
    MessageIngestionOutcome, RecurringReceiptIngestion, RetryableEmailMessage, SyncLeaseClaim,
};
use crate::domain::subscription_charge::ReceiptKind;

pub struct PgEmailSyncRepository {
    pool: PgPool,
}

impl PgEmailSyncRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl EmailSyncRepository for PgEmailSyncRepository {
    async fn list_due_connection_ids(
        &self,
        now: DateTime<Utc>,
        limit: i64,
    ) -> anyhow::Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar(
            "SELECT connection.id FROM email_connections connection \
             WHERE connection.status='connected' \
               AND (connection.next_sync_at <= $1 OR EXISTS ( \
                   SELECT 1 FROM email_message_ingestions message \
                   WHERE message.connection_id=connection.id \
                     AND message.outcome='failed' AND message.next_retry_at <= $1 \
               )) \
               AND (sync_lease_expires_at IS NULL OR sync_lease_expires_at <= $1) \
             ORDER BY next_sync_at, created_at LIMIT $2",
        )
        .bind(now.timestamp())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn claim_connection(
        &self,
        connection_id: Uuid,
        owner: Uuid,
        now: DateTime<Utc>,
        lease_until: DateTime<Utc>,
    ) -> anyhow::Result<SyncLeaseClaim> {
        let acquired = sqlx::query_scalar::<_, Uuid>(
            "UPDATE email_connections SET sync_lease_owner=$1, sync_lease_expires_at=$2 \
             WHERE id=$3 AND status='connected' \
               AND (sync_lease_expires_at IS NULL OR sync_lease_expires_at <= $4) \
             RETURNING id",
        )
        .bind(owner)
        .bind(lease_until.timestamp())
        .bind(connection_id)
        .bind(now.timestamp())
        .fetch_optional(&self.pool)
        .await?;
        if acquired.is_some() {
            return Ok(SyncLeaseClaim::Acquired);
        }

        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM email_connections WHERE id=$1)",
        )
        .bind(connection_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(if exists {
            SyncLeaseClaim::Busy
        } else {
            SyncLeaseClaim::NotFound
        })
    }

    async fn complete_connection(
        &self,
        connection_id: Uuid,
        owner: Uuid,
        expected_credential_version: i64,
        synced_at: DateTime<Utc>,
        history_id: Option<String>,
        next_sync_at: DateTime<Utc>,
    ) -> anyhow::Result<bool> {
        let completed = sqlx::query_scalar::<_, bool>(
            "UPDATE email_connections SET \
               last_synced_at=CASE WHEN credential_version=$1 THEN $2 ELSE last_synced_at END, \
               last_history_id=CASE WHEN credential_version=$1 THEN $3 ELSE last_history_id END, \
               next_sync_at=CASE WHEN credential_version=$1 THEN $4 ELSE next_sync_at END, \
               sync_attempts=CASE WHEN credential_version=$1 THEN 0 ELSE sync_attempts END, \
               sync_last_error_kind=CASE WHEN credential_version=$1 THEN NULL ELSE sync_last_error_kind END, \
               sync_lease_owner=NULL, sync_lease_expires_at=NULL \
             WHERE id=$5 AND sync_lease_owner=$6 \
             RETURNING credential_version=$1",
        )
        .bind(expected_credential_version)
        .bind(synced_at.timestamp())
        .bind(history_id)
        .bind(next_sync_at.timestamp())
        .bind(connection_id)
        .bind(owner)
        .fetch_optional(&self.pool)
        .await?;
        Ok(completed.unwrap_or(false))
    }

    async fn fail_connection(
        &self,
        connection_id: Uuid,
        owner: Uuid,
        expected_credential_version: i64,
        error_kind: &str,
        now: DateTime<Utc>,
        reconnect_required: bool,
    ) -> anyhow::Result<bool> {
        let failed = sqlx::query_scalar::<_, bool>(
            "UPDATE email_connections SET \
               status=CASE WHEN credential_version=$1 AND $2 THEN 'reconnect_required' ELSE status END, \
               next_sync_at=CASE WHEN credential_version=$1 \
                   THEN $3 + LEAST(21600, (300 * power(2, LEAST(sync_attempts, 7)))::BIGINT) \
                   ELSE next_sync_at END, \
               sync_attempts=CASE WHEN credential_version=$1 THEN sync_attempts + 1 ELSE sync_attempts END, \
               sync_last_error_kind=CASE WHEN credential_version=$1 THEN $4 ELSE sync_last_error_kind END, \
               sync_lease_owner=NULL, sync_lease_expires_at=NULL \
             WHERE id=$5 AND sync_lease_owner=$6 \
             RETURNING credential_version=$1",
        )
        .bind(expected_credential_version)
        .bind(reconnect_required)
        .bind(now.timestamp())
        .bind(error_kind)
        .bind(connection_id)
        .bind(owner)
        .fetch_optional(&self.pool)
        .await?;
        Ok(failed.unwrap_or(false))
    }

    async fn record_ignored(
        &self,
        connection_id: Uuid,
        user_id: Uuid,
        provider_message_id: &str,
        rfc_message_id: Option<&str>,
        received_at: DateTime<Utc>,
        reason: &str,
    ) -> anyhow::Result<()> {
        let now = Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO email_message_ingestions \
             (id,connection_id,user_id,provider_message_id,rfc_message_id,outcome,attempts,\
              error_kind,next_retry_at,received_at,processed_at,created_at,updated_at) \
             VALUES ($1,$2,$3,$4,$5,'ignored',0,$6,NULL,$7,$8,$8,$8) \
             ON CONFLICT (connection_id,provider_message_id) DO UPDATE SET \
               rfc_message_id=COALESCE(EXCLUDED.rfc_message_id,email_message_ingestions.rfc_message_id), \
               received_at=CASE WHEN email_message_ingestions.outcome='processed' \
                                THEN email_message_ingestions.received_at ELSE EXCLUDED.received_at END, \
               outcome=CASE WHEN email_message_ingestions.outcome='processed' \
                            THEN 'processed' ELSE 'ignored' END, \
               error_kind=CASE WHEN email_message_ingestions.outcome='processed' \
                               THEN email_message_ingestions.error_kind ELSE EXCLUDED.error_kind END, \
               next_retry_at=NULL, processed_at=EXCLUDED.processed_at, updated_at=EXCLUDED.updated_at",
        )
        .bind(Uuid::new_v4())
        .bind(connection_id)
        .bind(user_id)
        .bind(provider_message_id)
        .bind(rfc_message_id)
        .bind(reason)
        .bind(received_at.timestamp())
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn record_failure(&self, failure: &EmailMessageFailure) -> anyhow::Result<()> {
        let next_retry = failure.recorded_at + Duration::minutes(5);
        sqlx::query(
            "INSERT INTO email_message_ingestions \
             (id,connection_id,user_id,provider_message_id,rfc_message_id,outcome,attempts,\
              error_kind,next_retry_at,received_at,processed_at,created_at,updated_at) \
             VALUES ($1,$2,$3,$4,$5,'failed',1,$6,$7,$8,NULL,$9,$9) \
             ON CONFLICT (connection_id,provider_message_id) DO UPDATE SET \
               rfc_message_id=COALESCE(EXCLUDED.rfc_message_id,email_message_ingestions.rfc_message_id), \
               attempts=email_message_ingestions.attempts + 1, \
               outcome=CASE WHEN email_message_ingestions.attempts + 1 >= 6 \
                            THEN 'dead_letter' ELSE 'failed' END, \
               error_kind=EXCLUDED.error_kind, \
               next_retry_at=CASE email_message_ingestions.attempts + 1 \
                 WHEN 1 THEN $9 + 300 WHEN 2 THEN $9 + 1800 WHEN 3 THEN $9 + 7200 \
                 WHEN 4 THEN $9 + 43200 WHEN 5 THEN $9 + 86400 \
                 ELSE NULL END, \
               processed_at=NULL, updated_at=$9 \
             WHERE email_message_ingestions.outcome <> 'processed'",
        )
        .bind(Uuid::new_v4())
        .bind(failure.connection_id)
        .bind(failure.user_id)
        .bind(&failure.provider_message_id)
        .bind(failure.rfc_message_id.as_deref())
        .bind(&failure.error_kind)
        .bind(next_retry.timestamp())
        .bind(failure.received_at.timestamp())
        .bind(failure.recorded_at.timestamp())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_retryable_messages(
        &self,
        connection_id: Uuid,
        now: DateTime<Utc>,
        limit: i64,
    ) -> anyhow::Result<Vec<RetryableEmailMessage>> {
        let rows = sqlx::query_as::<_, (String, i32)>(
            "SELECT provider_message_id, attempts FROM email_message_ingestions \
             WHERE connection_id=$1 AND outcome='failed' AND next_retry_at <= $2 \
             ORDER BY next_retry_at, created_at LIMIT $3",
        )
        .bind(connection_id)
        .bind(now.timestamp())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(provider_message_id, attempts)| RetryableEmailMessage {
                provider_message_id,
                attempts,
            })
            .collect())
    }

    async fn requeue_for_manual_resync(
        &self,
        connection_id: Uuid,
        user_id: Uuid,
        now: DateTime<Utc>,
    ) -> anyhow::Result<u64> {
        let result = sqlx::query(
            "UPDATE email_message_ingestions SET outcome='failed', attempts=0, \
               next_retry_at=$1, error_kind='manual_requeue', updated_at=$1 \
             WHERE connection_id=$2 AND user_id=$3 \
               AND (outcome='dead_letter' \
                    OR (outcome='ignored' AND error_kind = ANY($4)))",
        )
        .bind(now.timestamp())
        .bind(connection_id)
        .bind(user_id)
        .bind(MANUAL_RESYNC_IGNORED_REASONS.as_slice())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    async fn ingest_recurring(
        &self,
        ingestion: &RecurringReceiptIngestion,
    ) -> anyhow::Result<MessageIngestionOutcome> {
        let receipt = &ingestion.receipt;
        if receipt.amount <= rust_decimal::Decimal::ZERO || receipt.currency.trim().is_empty() {
            anyhow::bail!("recurring receipt amount and currency must be positive and non-empty");
        }
        let mut transaction = self.pool.begin().await?;
        let source_key = format!(
            "gmail:{}:{}",
            ingestion.connection_id, ingestion.provider_message_id
        );
        let merchant_key = format!("gmail:{}:{}", ingestion.connection_id, receipt.merchant_key);
        let lock_key = format!(
            "{}:{}:{}",
            ingestion.user_id,
            receipt.provider.as_str(),
            merchant_key
        );
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(lock_key)
            .execute(&mut *transaction)
            .await?;

        let durable_outcome = sqlx::query_scalar::<_, String>(
            "SELECT outcome FROM email_message_ingestions \
             WHERE connection_id=$1 AND provider_message_id=$2 FOR UPDATE",
        )
        .bind(ingestion.connection_id)
        .bind(&ingestion.provider_message_id)
        .fetch_optional(&mut *transaction)
        .await?;
        if durable_outcome.as_deref() == Some("processed") {
            let charge_id = sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM subscription_charges WHERE source='gmail' AND source_key=$1",
            )
            .bind(&source_key)
            .fetch_optional(&mut *transaction)
            .await?;
            transaction.commit().await?;
            return Ok(MessageIngestionOutcome::AlreadyProcessed(charge_id));
        }

        if let Some(charge_id) = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM subscription_charges WHERE source='gmail' AND source_key=$1 FOR UPDATE",
        )
        .bind(&source_key)
        .fetch_optional(&mut *transaction)
        .await?
        {
            mark_processed(&mut transaction, ingestion, Utc::now().timestamp()).await?;
            transaction.commit().await?;
            return Ok(MessageIngestionOutcome::AlreadyProcessed(Some(charge_id)));
        }

        if let Some(rfc_message_id) = ingestion.rfc_message_id.as_deref()
            && let Some((charge_id, subscription_id, source_connection_id)) =
                sqlx::query_as::<_, (Uuid, Uuid, Option<Uuid>)>(
                    "SELECT id, subscription_id, source_connection_id FROM subscription_charges \
                 WHERE user_id=$1 AND rfc_message_id=$2 \
                   AND source_key LIKE 'legacy:%' \
                 ORDER BY created_at LIMIT 1 FOR UPDATE",
                )
                .bind(ingestion.user_id)
                .bind(rfc_message_id)
                .fetch_optional(&mut *transaction)
                .await?
        {
            if source_connection_id.is_none() {
                sqlx::query(
                    "UPDATE subscription_charges SET source='gmail', source_key=$1, \
                       source_connection_id=$2, provider_message_id=$3, email_message_id=$1 \
                     WHERE id=$4",
                )
                .bind(&source_key)
                .bind(ingestion.connection_id)
                .bind(&ingestion.provider_message_id)
                .bind(charge_id)
                .execute(&mut *transaction)
                .await?;
            }
            sqlx::query(
                "UPDATE subscriptions legacy SET merchant_key=$1 \
                 WHERE legacy.id=$2 AND legacy.merchant_key=$3 \
                   AND NOT EXISTS (SELECT 1 FROM subscriptions current \
                                   WHERE current.user_id=legacy.user_id \
                                     AND current.merchant_key=$1)",
            )
            .bind(&merchant_key)
            .bind(subscription_id)
            .bind(&receipt.merchant_key)
            .execute(&mut *transaction)
            .await?;
            mark_processed(&mut transaction, ingestion, Utc::now().timestamp()).await?;
            transaction.commit().await?;
            return Ok(MessageIngestionOutcome::AlreadyProcessed(Some(charge_id)));
        }

        // First post-migration receipt adopts an unnamespaced legacy aggregate
        // into this mailbox. New aggregates are always mailbox-scoped so two
        // Gmail accounts owned by one user cannot collapse into each other.
        sqlx::query(
            "UPDATE subscriptions legacy SET merchant_key=$1 \
             WHERE legacy.user_id=$2 AND legacy.provider=$3 AND legacy.merchant_key=$4 \
               AND NOT EXISTS (SELECT 1 FROM subscriptions current \
                               WHERE current.user_id=$2 AND current.merchant_key=$1)",
        )
        .bind(&merchant_key)
        .bind(ingestion.user_id)
        .bind(receipt.provider.as_str())
        .bind(&receipt.merchant_key)
        .execute(&mut *transaction)
        .await?;

        let tombstoned = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM subscription_tombstones \
             WHERE user_id=$1 AND provider=$2 AND merchant_key=$3)",
        )
        .bind(ingestion.user_id)
        .bind(receipt.provider.as_str())
        .bind(&merchant_key)
        .fetch_one(&mut *transaction)
        .await?;
        if tombstoned {
            // A replica running the pre-tombstone code may have recreated the
            // legacy, unnamespaced aggregate during a rolling deploy. The
            // adoption above moves it into this mailbox namespace; purge it
            // again before recording the durable ignored outcome so deletion
            // remains permanent even across mixed application versions.
            sqlx::query(
                "DELETE FROM subscriptions \
                 WHERE user_id=$1 AND provider=$2 AND merchant_key=$3",
            )
            .bind(ingestion.user_id)
            .bind(receipt.provider.as_str())
            .bind(&merchant_key)
            .execute(&mut *transaction)
            .await?;
            mark_ignored_in_transaction(
                &mut transaction,
                ingestion,
                "subscription_tombstoned",
                Utc::now().timestamp(),
            )
            .await?;
            transaction.commit().await?;
            return Ok(MessageIngestionOutcome::Tombstoned);
        }

        let now = Utc::now();
        let next_expected_at = ingestion.billing_period.next_after(receipt.charged_at);
        let subscription_insert = sqlx::query(
            "INSERT INTO subscriptions \
             (id,user_id,provider,product_name,merchant_key,amount,currency,billing_period,status,\
              started_at,last_charged_at,next_expected_at,category_id,created_at,last_receipt_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'active',$9,$9,$10,NULL,$11,$9) \
             ON CONFLICT (user_id,merchant_key) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(ingestion.user_id)
        .bind(receipt.provider.as_str())
        .bind(&receipt.product_name)
        .bind(&merchant_key)
        .bind(receipt.amount)
        .bind(receipt.currency.trim().to_ascii_uppercase())
        .bind(ingestion.billing_period.as_str())
        .bind(receipt.charged_at.timestamp())
        .bind(next_expected_at.timestamp())
        .bind(now.timestamp())
        .execute(&mut *transaction)
        .await?;
        let subscription_was_created = subscription_insert.rows_affected() == 1;

        let subscription_id = sqlx::query_scalar::<_, Uuid>(
            "UPDATE subscriptions SET \
               started_at=LEAST(started_at,$1), \
               amount=CASE WHEN $1 > last_receipt_at THEN $2 ELSE amount END, \
               currency=CASE WHEN $1 > last_receipt_at THEN $3 ELSE currency END, \
               product_name=CASE WHEN $1 > last_receipt_at THEN $4 ELSE product_name END, \
               billing_period=CASE WHEN $1 > last_receipt_at THEN $5 ELSE billing_period END, \
               last_charged_at=CASE WHEN $1 > last_receipt_at THEN $1 ELSE last_charged_at END, \
               next_expected_at=CASE WHEN $1 > last_receipt_at THEN $6 ELSE next_expected_at END, \
               status=CASE WHEN $1 > last_receipt_at THEN 'active' ELSE status END, \
               last_receipt_at=GREATEST(last_receipt_at,$1) \
             WHERE user_id=$7 AND merchant_key=$8 RETURNING id",
        )
        .bind(receipt.charged_at.timestamp())
        .bind(receipt.amount)
        .bind(receipt.currency.trim().to_ascii_uppercase())
        .bind(&receipt.product_name)
        .bind(ingestion.billing_period.as_str())
        .bind(next_expected_at.timestamp())
        .bind(ingestion.user_id)
        .bind(&merchant_key)
        .fetch_one(&mut *transaction)
        .await?;

        let preserve_new_subscription_kind = subscription_was_created
            || sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM subscription_charges \
                 WHERE subscription_id=$1 AND kind='new_subscription')",
            )
            .bind(subscription_id)
            .fetch_one(&mut *transaction)
            .await?;

        let charge_id = Uuid::new_v4();
        let charge_insert = sqlx::query(
            "INSERT INTO subscription_charges \
             (id,subscription_id,user_id,amount,currency,charged_at,email_message_id,rfc_message_id,kind,\
              transaction_id,match_status,created_at,source,source_key,source_connection_id,\
              provider_message_id,match_started_at,match_source) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,NULL,'Pending',$10,'gmail',$7,$11,$12,$10,NULL) \
             ON CONFLICT (source_key) DO NOTHING",
        )
        .bind(charge_id)
        .bind(subscription_id)
        .bind(ingestion.user_id)
        .bind(receipt.amount)
        .bind(receipt.currency.trim().to_ascii_uppercase())
        .bind(receipt.charged_at.timestamp())
        .bind(&source_key)
        .bind(&ingestion.rfc_message_id)
        .bind(if subscription_was_created {
            ReceiptKind::NewSubscription.as_str()
        } else {
            ReceiptKind::Renewal.as_str()
        })
        .bind(now.timestamp())
        .bind(ingestion.connection_id)
        .bind(&ingestion.provider_message_id)
        .execute(&mut *transaction)
        .await?;

        if charge_insert.rows_affected() == 1 && preserve_new_subscription_kind {
            // Arrival order is not chronology: a newer receipt can win the
            // merchant lock before an older one. Reclassify the aggregate so
            // the earliest recurring charge is always NewSubscription and all
            // later recurring charges are renewals.
            sqlx::query(
                "UPDATE subscription_charges recurring SET kind = \
                   CASE WHEN recurring.id = ( \
                     SELECT earliest.id FROM subscription_charges earliest \
                     WHERE earliest.subscription_id=$1 \
                       AND earliest.kind IN ('new_subscription','renewal') \
                     ORDER BY earliest.charged_at, earliest.id LIMIT 1 \
                   ) THEN 'new_subscription' ELSE 'renewal' END \
                 WHERE recurring.subscription_id=$1 \
                   AND recurring.kind IN ('new_subscription','renewal')",
            )
            .bind(subscription_id)
            .execute(&mut *transaction)
            .await?;
        }

        mark_processed(&mut transaction, ingestion, now.timestamp()).await?;
        transaction.commit().await?;
        if charge_insert.rows_affected() == 1 {
            Ok(MessageIngestionOutcome::ChargeCreated(charge_id))
        } else {
            Ok(MessageIngestionOutcome::AlreadyProcessed(
                sqlx::query_scalar(
                    "SELECT id FROM subscription_charges WHERE source='gmail' AND source_key=$1",
                )
                .bind(source_key)
                .fetch_optional(&self.pool)
                .await?,
            ))
        }
    }
}

async fn mark_processed(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ingestion: &RecurringReceiptIngestion,
    now: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO email_message_ingestions \
         (id,connection_id,user_id,provider_message_id,rfc_message_id,outcome,attempts,\
          error_kind,next_retry_at,received_at,processed_at,created_at,updated_at) \
         VALUES ($1,$2,$3,$4,$5,'processed',0,NULL,NULL,$6,$7,$7,$7) \
         ON CONFLICT (connection_id,provider_message_id) DO UPDATE SET \
           rfc_message_id=COALESCE(EXCLUDED.rfc_message_id,email_message_ingestions.rfc_message_id), \
           received_at=EXCLUDED.received_at, outcome='processed', error_kind=NULL, next_retry_at=NULL, \
           processed_at=EXCLUDED.processed_at, updated_at=EXCLUDED.updated_at",
    )
    .bind(Uuid::new_v4())
    .bind(ingestion.connection_id)
    .bind(ingestion.user_id)
    .bind(&ingestion.provider_message_id)
    .bind(&ingestion.rfc_message_id)
    .bind(ingestion.received_at.timestamp())
    .bind(now)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn mark_ignored_in_transaction(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ingestion: &RecurringReceiptIngestion,
    reason: &str,
    now: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO email_message_ingestions \
         (id,connection_id,user_id,provider_message_id,rfc_message_id,outcome,attempts,\
          error_kind,next_retry_at,received_at,processed_at,created_at,updated_at) \
         VALUES ($1,$2,$3,$4,$5,'ignored',0,$6,NULL,$7,$8,$8,$8) \
         ON CONFLICT (connection_id,provider_message_id) DO UPDATE SET \
           received_at=EXCLUDED.received_at, outcome='ignored', \
           error_kind=EXCLUDED.error_kind, next_retry_at=NULL, \
           processed_at=EXCLUDED.processed_at, updated_at=EXCLUDED.updated_at",
    )
    .bind(Uuid::new_v4())
    .bind(ingestion.connection_id)
    .bind(ingestion.user_id)
    .bind(&ingestion.provider_message_id)
    .bind(&ingestion.rfc_message_id)
    .bind(reason)
    .bind(ingestion.received_at.timestamp())
    .bind(now)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::receipt_parser::ParsedReceipt;
    use crate::domain::subscription::{BillingPeriod, SubscriptionProvider};
    use crate::infrastructure::test_db;
    use rust_decimal::Decimal;

    fn timestamp(value: &str) -> DateTime<Utc> {
        value.parse().expect("valid test timestamp")
    }

    async fn insert_connection(pool: &PgPool, user_id: Uuid, created_at: DateTime<Utc>) -> Uuid {
        insert_connection_at_address(
            pool,
            user_id,
            created_at,
            &format!("{user_id}@example.test"),
        )
        .await
    }

    async fn insert_connection_at_address(
        pool: &PgPool,
        user_id: Uuid,
        created_at: DateTime<Utc>,
        email_address: &str,
    ) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO email_connections \
             (id,user_id,provider,email_address,oauth_access_token,oauth_refresh_token,\
              access_token_expires_at,status,last_synced_at,last_history_id,created_at) \
             VALUES ($1,$2,'gmail',$3,'access-token','refresh-token',$4,'connected',NULL,NULL,$5)",
        )
        .bind(id)
        .bind(user_id)
        .bind(email_address)
        .bind((created_at + Duration::days(1)).timestamp())
        .bind(created_at.timestamp())
        .execute(pool)
        .await
        .unwrap();
        id
    }

    #[allow(clippy::too_many_arguments)]
    fn recurring_ingestion(
        connection_id: Uuid,
        user_id: Uuid,
        provider_message_id: &str,
        rfc_message_id: &str,
        charged_at: DateTime<Utc>,
        product_name: &str,
        amount: Decimal,
        currency: &str,
        billing_period: BillingPeriod,
    ) -> RecurringReceiptIngestion {
        RecurringReceiptIngestion {
            connection_id,
            user_id,
            provider_message_id: provider_message_id.to_string(),
            rfc_message_id: Some(rfc_message_id.to_string()),
            received_at: charged_at + Duration::minutes(5),
            receipt: ParsedReceipt {
                provider: SubscriptionProvider::Netflix,
                product_name: product_name.to_string(),
                merchant_key: "netflix.com:premium".to_string(),
                amount,
                currency: currency.to_string(),
                charged_at,
                billing_period_hint: Some(billing_period),
            },
            billing_period,
        }
    }

    async fn message_state(
        pool: &PgPool,
        connection_id: Uuid,
        provider_message_id: &str,
    ) -> (String, i32, Option<i64>) {
        sqlx::query_as(
            "SELECT outcome,attempts,next_retry_at FROM email_message_ingestions \
             WHERE connection_id=$1 AND provider_message_id=$2",
        )
        .bind(connection_id)
        .bind(provider_message_id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn lease_reclaim_fences_stale_owner_completion() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailSyncRepository::new(pool.clone());
        let user_id = Uuid::new_v4();
        let now = timestamp("2026-06-01T10:00:00Z");
        let connection_id = insert_connection(&pool, user_id, now).await;
        let first_owner = Uuid::new_v4();
        let second_owner = Uuid::new_v4();
        let first_expiry = now + Duration::minutes(10);

        assert_eq!(
            repo.claim_connection(connection_id, first_owner, now, first_expiry)
                .await
                .unwrap(),
            SyncLeaseClaim::Acquired
        );
        assert_eq!(
            repo.claim_connection(
                connection_id,
                second_owner,
                now + Duration::minutes(1),
                now + Duration::minutes(11),
            )
            .await
            .unwrap(),
            SyncLeaseClaim::Busy
        );
        assert!(
            !repo
                .complete_connection(
                    connection_id,
                    second_owner,
                    0,
                    now + Duration::minutes(1),
                    Some("wrong-owner-cursor".to_string()),
                    now + Duration::hours(1),
                )
                .await
                .unwrap()
        );

        assert_eq!(
            repo.claim_connection(
                connection_id,
                second_owner,
                first_expiry,
                first_expiry + Duration::minutes(10),
            )
            .await
            .unwrap(),
            SyncLeaseClaim::Acquired
        );
        assert!(
            !repo
                .complete_connection(
                    connection_id,
                    first_owner,
                    0,
                    first_expiry,
                    Some("stale-cursor".to_string()),
                    first_expiry + Duration::hours(1),
                )
                .await
                .unwrap()
        );
        let current_owner: Option<Uuid> =
            sqlx::query_scalar("SELECT sync_lease_owner FROM email_connections WHERE id=$1")
                .bind(connection_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(current_owner, Some(second_owner));

        let completed_at = first_expiry + Duration::minutes(1);
        let next_sync_at = completed_at + Duration::hours(1);
        assert!(
            repo.complete_connection(
                connection_id,
                second_owner,
                0,
                completed_at,
                Some("fresh-cursor".to_string()),
                next_sync_at,
            )
            .await
            .unwrap()
        );
        let row: (Option<Uuid>, Option<i64>, Option<String>, i64) = sqlx::query_as(
            "SELECT sync_lease_owner,last_synced_at,last_history_id,next_sync_at \
             FROM email_connections WHERE id=$1",
        )
        .bind(connection_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, None);
        assert_eq!(row.1, Some(completed_at.timestamp()));
        assert_eq!(row.2.as_deref(), Some("fresh-cursor"));
        assert_eq!(row.3, next_sync_at.timestamp());
    }

    #[tokio::test]
    async fn credential_reconnect_fences_stale_cursor_and_failure_updates() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailSyncRepository::new(pool.clone());
        let user_id = Uuid::new_v4();
        let now = timestamp("2026-06-01T10:00:00Z");
        let connection_id = insert_connection(&pool, user_id, now).await;
        let owner = Uuid::new_v4();
        assert_eq!(
            repo.claim_connection(connection_id, owner, now, now + Duration::minutes(10),)
                .await
                .unwrap(),
            SyncLeaseClaim::Acquired
        );

        // A browser reconnect replaces the credentials while this worker is
        // still running, and intentionally schedules an immediate fresh sync.
        sqlx::query(
            "UPDATE email_connections SET credential_version=1, next_sync_at=0, \
             sync_attempts=0, sync_last_error_kind=NULL WHERE id=$1",
        )
        .bind(connection_id)
        .execute(&pool)
        .await
        .unwrap();

        assert!(
            !repo
                .complete_connection(
                    connection_id,
                    owner,
                    0,
                    now + Duration::minutes(1),
                    Some("stale-cursor".to_string()),
                    now + Duration::hours(1),
                )
                .await
                .unwrap()
        );
        let after_completion: (Option<Uuid>, Option<i64>, Option<String>, i64, i32) =
            sqlx::query_as(
                "SELECT sync_lease_owner,last_synced_at,last_history_id,next_sync_at,sync_attempts \
                 FROM email_connections WHERE id=$1",
            )
            .bind(connection_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(after_completion.0, None);
        assert_eq!(after_completion.1, None);
        assert_eq!(after_completion.2, None);
        assert_eq!(after_completion.3, 0);
        assert_eq!(after_completion.4, 0);

        let failure_owner = Uuid::new_v4();
        assert_eq!(
            repo.claim_connection(
                connection_id,
                failure_owner,
                now + Duration::minutes(2),
                now + Duration::minutes(12),
            )
            .await
            .unwrap(),
            SyncLeaseClaim::Acquired
        );
        sqlx::query(
            "UPDATE email_connections SET credential_version=2, next_sync_at=0 \
             WHERE id=$1",
        )
        .bind(connection_id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            !repo
                .fail_connection(
                    connection_id,
                    failure_owner,
                    1,
                    "invalid_credentials",
                    now + Duration::minutes(3),
                    true,
                )
                .await
                .unwrap()
        );
        let after_failure: (String, Option<Uuid>, i64, i32, Option<String>) = sqlx::query_as(
            "SELECT status,sync_lease_owner,next_sync_at,sync_attempts,sync_last_error_kind \
             FROM email_connections WHERE id=$1",
        )
        .bind(connection_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(after_failure.0, "connected");
        assert_eq!(after_failure.1, None);
        assert_eq!(after_failure.2, 0);
        assert_eq!(after_failure.3, 0);
        assert_eq!(after_failure.4, None);
    }

    #[tokio::test]
    async fn retry_schedule_dead_letters_and_manual_requeue_restart_backoff() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailSyncRepository::new(pool.clone());
        let user_id = Uuid::new_v4();
        let start = timestamp("2026-06-02T10:00:00Z");
        let connection_id = insert_connection(&pool, user_id, start).await;
        let message_id = "gmail-retry-1";
        sqlx::query("UPDATE email_connections SET next_sync_at=$1 WHERE id=$2")
            .bind((start + Duration::days(7)).timestamp())
            .bind(connection_id)
            .execute(&pool)
            .await
            .unwrap();

        let delays = [300_i64, 1_800, 7_200, 43_200, 86_400];
        let mut attempt_at = start;
        for (index, delay) in delays.into_iter().enumerate() {
            repo.record_failure(&EmailMessageFailure {
                connection_id,
                user_id,
                provider_message_id: message_id.to_string(),
                rfc_message_id: Some("<retry@example.test>".to_string()),
                received_at: start,
                error_kind: "message_fetch_failed".to_string(),
                recorded_at: attempt_at,
            })
            .await
            .unwrap();
            let expected_attempts = index as i32 + 1;
            let expected_retry = attempt_at + Duration::seconds(delay);
            assert_eq!(
                message_state(&pool, connection_id, message_id).await,
                (
                    "failed".to_string(),
                    expected_attempts,
                    Some(expected_retry.timestamp()),
                )
            );
            assert!(
                repo.list_retryable_messages(
                    connection_id,
                    expected_retry - Duration::seconds(1),
                    10,
                )
                .await
                .unwrap()
                .is_empty()
            );
            let retryable = repo
                .list_retryable_messages(connection_id, expected_retry, 10)
                .await
                .unwrap();
            assert_eq!(retryable.len(), 1);
            assert_eq!(retryable[0].provider_message_id, message_id);
            assert_eq!(retryable[0].attempts, expected_attempts);
            if index == 0 {
                assert!(
                    repo.list_due_connection_ids(expected_retry - Duration::seconds(1), 10)
                        .await
                        .unwrap()
                        .is_empty()
                );
                assert_eq!(
                    repo.list_due_connection_ids(expected_retry, 10)
                        .await
                        .unwrap(),
                    vec![connection_id]
                );
            }
            attempt_at = expected_retry;
        }

        repo.record_failure(&EmailMessageFailure {
            connection_id,
            user_id,
            provider_message_id: message_id.to_string(),
            rfc_message_id: None,
            received_at: start,
            error_kind: "message_fetch_failed".to_string(),
            recorded_at: attempt_at,
        })
        .await
        .unwrap();
        assert_eq!(
            message_state(&pool, connection_id, message_id).await,
            ("dead_letter".to_string(), 6, None)
        );
        assert!(
            repo.list_retryable_messages(connection_id, attempt_at + Duration::days(1), 10)
                .await
                .unwrap()
                .is_empty()
        );

        let requeued_at = attempt_at + Duration::hours(1);
        assert_eq!(
            repo.requeue_for_manual_resync(connection_id, user_id, requeued_at)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            message_state(&pool, connection_id, message_id).await,
            ("failed".to_string(), 0, Some(requeued_at.timestamp()))
        );
        assert_eq!(
            repo.list_retryable_messages(connection_id, requeued_at, 10)
                .await
                .unwrap()[0]
                .attempts,
            0
        );

        repo.record_failure(&EmailMessageFailure {
            connection_id,
            user_id,
            provider_message_id: message_id.to_string(),
            rfc_message_id: None,
            received_at: start,
            error_kind: "message_fetch_failed".to_string(),
            recorded_at: requeued_at,
        })
        .await
        .unwrap();
        let restarted_retry = requeued_at + Duration::minutes(5);
        assert_eq!(
            message_state(&pool, connection_id, message_id).await,
            ("failed".to_string(), 1, Some(restarted_retry.timestamp()),)
        );
        assert!(
            repo.list_retryable_messages(
                connection_id,
                restarted_retry - Duration::seconds(1),
                10,
            )
            .await
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            repo.list_retryable_messages(connection_id, restarted_retry, 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn manual_resync_requeues_only_scoped_upgrade_sensitive_outcomes() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailSyncRepository::new(pool.clone());
        let user_id = Uuid::new_v4();
        let other_user_id = Uuid::new_v4();
        let recorded_at = timestamp("2026-06-02T10:00:00Z");
        let requeued_at = recorded_at + Duration::hours(1);
        let connection_id = insert_connection(&pool, user_id, recorded_at).await;
        let other_connection_id = insert_connection(&pool, other_user_id, recorded_at).await;

        for reason in MANUAL_RESYNC_IGNORED_REASONS {
            repo.record_ignored(
                connection_id,
                user_id,
                &format!("candidate-{reason}"),
                None,
                recorded_at,
                reason,
            )
            .await
            .unwrap();
        }
        for reason in [
            "invalid_recurring_amount_or_currency",
            "subscription_tombstoned",
        ] {
            repo.record_ignored(
                connection_id,
                user_id,
                &format!("permanent-{reason}"),
                None,
                recorded_at,
                reason,
            )
            .await
            .unwrap();
        }
        repo.record_ignored(
            other_connection_id,
            other_user_id,
            "other-mailbox-candidate",
            None,
            recorded_at,
            "not_recurring",
        )
        .await
        .unwrap();
        repo.record_ignored(
            connection_id,
            user_id,
            "dead-letter-candidate",
            None,
            recorded_at,
            "parser_failure",
        )
        .await
        .unwrap();
        sqlx::query(
            "UPDATE email_message_ingestions SET outcome='dead_letter', attempts=6 \
             WHERE connection_id=$1 AND provider_message_id='dead-letter-candidate'",
        )
        .bind(connection_id)
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(
            repo.requeue_for_manual_resync(connection_id, user_id, requeued_at)
                .await
                .unwrap(),
            5
        );

        let retryable = repo
            .list_retryable_messages(connection_id, requeued_at, 20)
            .await
            .unwrap();
        assert_eq!(retryable.len(), 5);
        assert!(retryable.iter().all(|message| message.attempts == 0));
        let permanent: Vec<(String, String, Option<i64>)> = sqlx::query_as(
            "SELECT provider_message_id,outcome,next_retry_at FROM email_message_ingestions \
             WHERE connection_id=$1 AND provider_message_id LIKE 'permanent-%' \
             ORDER BY provider_message_id",
        )
        .bind(connection_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(permanent.len(), 2);
        assert!(
            permanent
                .iter()
                .all(|(_, outcome, next_retry_at)| outcome == "ignored" && next_retry_at.is_none())
        );
        assert_eq!(
            message_state(&pool, other_connection_id, "other-mailbox-candidate").await,
            ("ignored".to_string(), 0, None)
        );
    }

    #[tokio::test]
    async fn successful_manual_reprocess_replaces_placeholder_received_at() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailSyncRepository::new(pool.clone());
        let user_id = Uuid::new_v4();
        let placeholder_at = timestamp("2026-07-21T08:00:00Z");
        let actual_received_at = timestamp("2026-06-26T11:58:48Z");
        let charged_at = timestamp("2026-06-26T11:58:00Z");
        let connection_id = insert_connection(&pool, user_id, placeholder_at).await;
        let provider_message_id = "previously-untrusted";

        repo.record_ignored(
            connection_id,
            user_id,
            provider_message_id,
            None,
            placeholder_at,
            "untrusted_sender_authentication",
        )
        .await
        .unwrap();
        assert_eq!(
            repo.requeue_for_manual_resync(
                connection_id,
                user_id,
                placeholder_at + Duration::minutes(1),
            )
            .await
            .unwrap(),
            1
        );

        let mut ingestion = recurring_ingestion(
            connection_id,
            user_id,
            provider_message_id,
            "<google-play-receipt@example.test>",
            charged_at,
            "Culinara",
            Decimal::new(18_900, 2),
            "UAH",
            BillingPeriod::Monthly,
        );
        ingestion.received_at = actual_received_at;
        assert!(matches!(
            repo.ingest_recurring(&ingestion).await.unwrap(),
            MessageIngestionOutcome::ChargeCreated(_)
        ));

        let state: (String, Option<String>, i64, Option<i64>) = sqlx::query_as(
            "SELECT outcome,error_kind,received_at,next_retry_at \
             FROM email_message_ingestions \
             WHERE connection_id=$1 AND provider_message_id=$2",
        )
        .bind(connection_id)
        .bind(provider_message_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state.0, "processed");
        assert_eq!(state.1, None);
        assert_eq!(state.2, actual_received_at.timestamp());
        assert_eq!(state.3, None);
    }

    #[tokio::test]
    async fn concurrent_recurring_replay_creates_one_atomic_charge() {
        let pool = test_db::fresh_pool().await;
        let user_id = Uuid::new_v4();
        let charged_at = timestamp("2026-06-03T10:00:00Z");
        let connection_id = insert_connection(&pool, user_id, charged_at).await;
        let ingestion = recurring_ingestion(
            connection_id,
            user_id,
            "gmail-atomic-1",
            "<atomic@example.test>",
            charged_at,
            "Netflix Premium",
            Decimal::new(1_999, 2),
            "usd",
            BillingPeriod::Monthly,
        );
        let first_repo = PgEmailSyncRepository::new(pool.clone());
        let second_repo = PgEmailSyncRepository::new(pool.clone());
        let first_ingestion = ingestion.clone();
        let second_ingestion = ingestion.clone();

        let (first, second) = tokio::join!(
            first_repo.ingest_recurring(&first_ingestion),
            second_repo.ingest_recurring(&second_ingestion),
        );
        match (first.unwrap(), second.unwrap()) {
            (
                MessageIngestionOutcome::ChargeCreated(created),
                MessageIngestionOutcome::AlreadyProcessed(Some(existing)),
            )
            | (
                MessageIngestionOutcome::AlreadyProcessed(Some(existing)),
                MessageIngestionOutcome::ChargeCreated(created),
            ) => assert_eq!(created, existing),
            outcomes => panic!("unexpected concurrent ingestion outcomes: {outcomes:?}"),
        }

        let subscription_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM subscriptions WHERE user_id=$1")
                .bind(user_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let charge_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM subscription_charges WHERE user_id=$1")
                .bind(user_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(subscription_count, 1);
        assert_eq!(charge_count, 1);
        assert_eq!(
            message_state(&pool, connection_id, "gmail-atomic-1")
                .await
                .0,
            "processed"
        );
    }

    #[tokio::test]
    async fn concurrent_distinct_receipts_keep_chronological_kinds() {
        let pool = test_db::fresh_pool().await;
        let user_id = Uuid::new_v4();
        let older_at = timestamp("2026-05-03T10:00:00Z");
        let newer_at = timestamp("2026-06-03T10:00:00Z");
        let connection_id = insert_connection(&pool, user_id, older_at).await;
        let older = recurring_ingestion(
            connection_id,
            user_id,
            "gmail-concurrent-older",
            "<concurrent-older@example.test>",
            older_at,
            "Netflix Premium",
            Decimal::new(1_999, 2),
            "usd",
            BillingPeriod::Monthly,
        );
        let newer = recurring_ingestion(
            connection_id,
            user_id,
            "gmail-concurrent-newer",
            "<concurrent-newer@example.test>",
            newer_at,
            "Netflix Premium",
            Decimal::new(1_999, 2),
            "usd",
            BillingPeriod::Monthly,
        );
        let first_repo = PgEmailSyncRepository::new(pool.clone());
        let second_repo = PgEmailSyncRepository::new(pool.clone());

        let (first, second) = tokio::join!(
            first_repo.ingest_recurring(&newer),
            second_repo.ingest_recurring(&older),
        );
        assert!(matches!(
            first.unwrap(),
            MessageIngestionOutcome::ChargeCreated(_)
        ));
        assert!(matches!(
            second.unwrap(),
            MessageIngestionOutcome::ChargeCreated(_)
        ));

        let kinds: Vec<(String, String)> = sqlx::query_as(
            "SELECT provider_message_id, kind FROM subscription_charges \
             WHERE user_id=$1 ORDER BY charged_at, id",
        )
        .bind(user_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            kinds,
            vec![
                (
                    "gmail-concurrent-older".to_string(),
                    "new_subscription".to_string(),
                ),
                ("gmail-concurrent-newer".to_string(), "renewal".to_string(),),
            ]
        );
    }

    #[tokio::test]
    async fn older_receipt_only_moves_started_at_and_never_regresses_latest_state() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailSyncRepository::new(pool.clone());
        let user_id = Uuid::new_v4();
        let newer_at = timestamp("2026-06-15T10:00:00Z");
        let older_at = timestamp("2026-05-15T10:00:00Z");
        let connection_id = insert_connection(&pool, user_id, older_at).await;
        let newer = recurring_ingestion(
            connection_id,
            user_id,
            "gmail-newer",
            "<newer@example.test>",
            newer_at,
            "Netflix Premium",
            Decimal::new(2_999, 2),
            "usd",
            BillingPeriod::Yearly,
        );
        let older = recurring_ingestion(
            connection_id,
            user_id,
            "gmail-older",
            "<older@example.test>",
            older_at,
            "Netflix Basic",
            Decimal::new(999, 2),
            "eur",
            BillingPeriod::Monthly,
        );

        assert!(matches!(
            repo.ingest_recurring(&newer).await.unwrap(),
            MessageIngestionOutcome::ChargeCreated(_)
        ));
        assert!(matches!(
            repo.ingest_recurring(&older).await.unwrap(),
            MessageIngestionOutcome::ChargeCreated(_)
        ));

        let state: (String, Decimal, String, String, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT product_name,amount,currency,billing_period,started_at,last_charged_at,\
                    next_expected_at,last_receipt_at \
             FROM subscriptions WHERE user_id=$1 AND merchant_key=$2",
        )
        .bind(user_id)
        .bind(format!("gmail:{connection_id}:netflix.com:premium"))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state.0, "Netflix Premium");
        assert_eq!(state.1, Decimal::new(2_999, 2));
        assert_eq!(state.2, "USD");
        assert_eq!(state.3, "yearly");
        assert_eq!(state.4, older_at.timestamp());
        assert_eq!(state.5, newer_at.timestamp());
        assert_eq!(
            state.6,
            BillingPeriod::Yearly.next_after(newer_at).timestamp()
        );
        assert_eq!(state.7, newer_at.timestamp());
        let charge_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM subscription_charges WHERE user_id=$1")
                .bind(user_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(charge_count, 2);
        let chronological_kinds: Vec<(String, String)> = sqlx::query_as(
            "SELECT provider_message_id, kind FROM subscription_charges \
             WHERE user_id=$1 ORDER BY charged_at, id",
        )
        .bind(user_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            chronological_kinds,
            vec![
                ("gmail-older".to_string(), "new_subscription".to_string()),
                ("gmail-newer".to_string(), "renewal".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn tombstone_suppresses_subscription_and_charge_atomically() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailSyncRepository::new(pool.clone());
        let user_id = Uuid::new_v4();
        let charged_at = timestamp("2026-06-04T10:00:00Z");
        let connection_id = insert_connection(&pool, user_id, charged_at).await;
        sqlx::query(
            "INSERT INTO subscription_tombstones (user_id,provider,merchant_key,deleted_at) \
             VALUES ($1,'netflix',$2,$3)",
        )
        .bind(user_id)
        .bind(format!("gmail:{connection_id}:netflix.com:premium"))
        .bind((charged_at - Duration::days(1)).timestamp())
        .execute(&pool)
        .await
        .unwrap();
        // Simulate an old replica recreating the legacy aggregate after the
        // tombstone was written but before it learned about tombstones.
        sqlx::query(
            "INSERT INTO subscriptions \
             (id,user_id,provider,product_name,merchant_key,amount,currency,billing_period,status,\
              started_at,last_charged_at,next_expected_at,category_id,created_at,last_receipt_at) \
             VALUES ($1,$2,'netflix','Netflix Premium','netflix.com:premium',19.99,'USD',\
                     'monthly','active',$3,$3,$4,NULL,$3,$3)",
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .bind(charged_at.timestamp())
        .bind((charged_at + Duration::days(30)).timestamp())
        .execute(&pool)
        .await
        .unwrap();
        let ingestion = recurring_ingestion(
            connection_id,
            user_id,
            "gmail-tombstoned",
            "<tombstoned@example.test>",
            charged_at,
            "Netflix Premium",
            Decimal::new(1_999, 2),
            "usd",
            BillingPeriod::Monthly,
        );

        assert_eq!(
            repo.ingest_recurring(&ingestion).await.unwrap(),
            MessageIngestionOutcome::Tombstoned
        );
        let aggregate_counts: (i64, i64) = sqlx::query_as(
            "SELECT \
               (SELECT count(*) FROM subscriptions WHERE user_id=$1), \
               (SELECT count(*) FROM subscription_charges WHERE user_id=$1)",
        )
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(aggregate_counts, (0, 0));
        let ingestion_state: (String, Option<String>) = sqlx::query_as(
            "SELECT outcome,error_kind FROM email_message_ingestions \
             WHERE connection_id=$1 AND provider_message_id='gmail-tombstoned'",
        )
        .bind(connection_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(ingestion_state.0, "ignored");
        assert_eq!(
            ingestion_state.1.as_deref(),
            Some("subscription_tombstoned")
        );
    }

    #[tokio::test]
    async fn provider_message_id_is_scoped_by_connection_across_users() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailSyncRepository::new(pool.clone());
        let first_user = Uuid::new_v4();
        let second_user = Uuid::new_v4();
        let charged_at = timestamp("2026-06-05T10:00:00Z");
        let first_connection = insert_connection(&pool, first_user, charged_at).await;
        let second_connection = insert_connection(&pool, second_user, charged_at).await;
        let first = recurring_ingestion(
            first_connection,
            first_user,
            "shared-provider-id",
            "<shared@example.test>",
            charged_at,
            "Netflix Premium",
            Decimal::new(1_999, 2),
            "usd",
            BillingPeriod::Monthly,
        );
        let second = recurring_ingestion(
            second_connection,
            second_user,
            "shared-provider-id",
            "<shared@example.test>",
            charged_at,
            "Netflix Premium",
            Decimal::new(1_999, 2),
            "usd",
            BillingPeriod::Monthly,
        );

        assert!(matches!(
            repo.ingest_recurring(&first).await.unwrap(),
            MessageIngestionOutcome::ChargeCreated(_)
        ));
        assert!(matches!(
            repo.ingest_recurring(&second).await.unwrap(),
            MessageIngestionOutcome::ChargeCreated(_)
        ));
        let ingestion_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM email_message_ingestions \
             WHERE provider_message_id='shared-provider-id'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let charge_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM subscription_charges \
             WHERE user_id IN ($1,$2)",
        )
        .bind(first_user)
        .bind(second_user)
        .fetch_one(&pool)
        .await
        .unwrap();
        let distinct_users: i64 = sqlx::query_scalar(
            "SELECT count(DISTINCT user_id) FROM subscription_charges \
             WHERE user_id IN ($1,$2)",
        )
        .bind(first_user)
        .bind(second_user)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(ingestion_count, 2);
        assert_eq!(charge_count, 2);
        assert_eq!(distinct_users, 2);
    }

    #[tokio::test]
    async fn identical_products_in_two_mailboxes_remain_distinct_aggregates() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailSyncRepository::new(pool.clone());
        let user_id = Uuid::new_v4();
        let charged_at = timestamp("2026-06-05T10:00:00Z");
        let first_connection = insert_connection(&pool, user_id, charged_at).await;
        let second_connection =
            insert_connection_at_address(&pool, user_id, charged_at, "second-mailbox@example.test")
                .await;
        let first = recurring_ingestion(
            first_connection,
            user_id,
            "first-mailbox-message",
            "<first-mailbox@example.test>",
            charged_at,
            "Netflix Premium",
            Decimal::new(1_999, 2),
            "usd",
            BillingPeriod::Monthly,
        );
        let second = recurring_ingestion(
            second_connection,
            user_id,
            "second-mailbox-message",
            "<second-mailbox@example.test>",
            charged_at,
            "Netflix Premium",
            Decimal::new(1_999, 2),
            "usd",
            BillingPeriod::Monthly,
        );

        repo.ingest_recurring(&first).await.unwrap();
        repo.ingest_recurring(&second).await.unwrap();

        let merchant_keys: Vec<String> = sqlx::query_scalar(
            "SELECT merchant_key FROM subscriptions WHERE user_id=$1 ORDER BY merchant_key",
        )
        .bind(user_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(merchant_keys.len(), 2);
        assert!(
            merchant_keys
                .iter()
                .any(|key| key.contains(&first_connection.to_string()))
        );
        assert!(
            merchant_keys
                .iter()
                .any(|key| key.contains(&second_connection.to_string()))
        );
    }

    #[tokio::test]
    async fn first_charge_for_preexisting_aggregate_is_a_renewal() {
        let pool = test_db::fresh_pool().await;
        let repo = PgEmailSyncRepository::new(pool.clone());
        let user_id = Uuid::new_v4();
        let charged_at = timestamp("2026-06-05T10:00:00Z");
        let connection_id = insert_connection(&pool, user_id, charged_at).await;
        let merchant_key = format!("gmail:{connection_id}:netflix.com:premium");
        sqlx::query(
            "INSERT INTO subscriptions \
             (id,user_id,provider,product_name,merchant_key,amount,currency,billing_period,status,\
              started_at,last_charged_at,next_expected_at,category_id,created_at,last_receipt_at) \
             VALUES ($1,$2,'netflix','Netflix Premium',$3,19.99,'USD','monthly','active',\
                     $4,$4,$5,NULL,$4,$4)",
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .bind(merchant_key)
        .bind((charged_at - Duration::days(31)).timestamp())
        .bind(charged_at.timestamp())
        .execute(&pool)
        .await
        .unwrap();
        let ingestion = recurring_ingestion(
            connection_id,
            user_id,
            "preexisting-renewal",
            "<preexisting@example.test>",
            charged_at,
            "Netflix Premium",
            Decimal::new(1_999, 2),
            "usd",
            BillingPeriod::Monthly,
        );

        repo.ingest_recurring(&ingestion).await.unwrap();

        let kind: String = sqlx::query_scalar(
            "SELECT kind FROM subscription_charges WHERE provider_message_id='preexisting-renewal'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(kind, "renewal");
    }
}
