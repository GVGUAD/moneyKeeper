use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::subscription::{
    BillingPeriod, MarkTransactionSubscription, MarkTransactionSubscriptionOutcome, Subscription,
    SubscriptionListFilter, SubscriptionOverrides, SubscriptionProvider, SubscriptionRepository,
    SubscriptionStatus, SubscriptionUpsertResult, TransactionSubscriptionTarget,
};

pub struct PgSubscriptionRepository {
    pool: PgPool,
}

impl PgSubscriptionRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    user_id: Uuid,
    provider: String,
    product_name: String,
    product_name_override: Option<String>,
    merchant_key: String,
    amount: Decimal,
    currency: String,
    billing_period: String,
    billing_period_override: Option<String>,
    status: String,
    status_override: Option<String>,
    started_at: i64,
    last_charged_at: Option<i64>,
    next_expected_at: Option<i64>,
    category_id: Option<Uuid>,
    created_at: i64,
}

fn row_to_sub(r: Row) -> anyhow::Result<Subscription> {
    let product_name_override = r.product_name_override;
    let billing_period_override = r
        .billing_period_override
        .as_deref()
        .map(BillingPeriod::from_str)
        .transpose()?;
    let status_override = r
        .status_override
        .as_deref()
        .map(SubscriptionStatus::from_str)
        .transpose()?;
    let product_name = product_name_override.clone().unwrap_or(r.product_name);
    let billing_period =
        billing_period_override.unwrap_or(BillingPeriod::from_str(&r.billing_period)?);
    let status = status_override
        .clone()
        .unwrap_or(SubscriptionStatus::from_str(&r.status)?);
    Ok(Subscription {
        id: r.id,
        user_id: r.user_id,
        provider: SubscriptionProvider::from_str(&r.provider)?,
        product_name,
        merchant_key: r.merchant_key,
        amount: r.amount,
        currency: r.currency,
        billing_period,
        status,
        started_at: DateTime::from_timestamp(r.started_at, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid started_at"))?,
        last_charged_at: r
            .last_charged_at
            .and_then(|t| DateTime::from_timestamp(t, 0)),
        next_expected_at: r
            .next_expected_at
            .and_then(|t| DateTime::from_timestamp(t, 0)),
        category_id: r.category_id,
        overrides: SubscriptionOverrides {
            product_name: product_name_override,
            billing_period: billing_period_override,
            status: status_override,
        },
        created_at: DateTime::from_timestamp(r.created_at, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid created_at"))?,
    })
}

#[async_trait::async_trait]
impl SubscriptionRepository for PgSubscriptionRepository {
    async fn mark_transaction_as_subscription(
        &self,
        command: &MarkTransactionSubscription,
    ) -> anyhow::Result<MarkTransactionSubscriptionOutcome> {
        let mut db_tx = self.pool.begin().await?;
        let transaction =
            sqlx::query_as::<_, (Decimal, String, String, Option<Uuid>, DateTime<Utc>)>(
                "SELECT amount, currency, kind, category_id, transacted_at \
             FROM transactions WHERE id=$1 AND user_id=$2 FOR UPDATE",
            )
            .bind(command.transaction_id)
            .bind(command.user_id)
            .fetch_optional(&mut *db_tx)
            .await?;
        let Some((amount, currency, kind, transaction_category_id, transacted_at)) = transaction
        else {
            db_tx.rollback().await?;
            return Ok(MarkTransactionSubscriptionOutcome::TransactionNotFound);
        };
        if kind != "Expense" {
            db_tx.rollback().await?;
            return Ok(MarkTransactionSubscriptionOutcome::TransactionNotExpense);
        }
        let currency = currency.trim().to_ascii_uppercase();
        if amount <= Decimal::ZERO || currency.is_empty() {
            db_tx.rollback().await?;
            return Ok(MarkTransactionSubscriptionOutcome::TransactionInvalid);
        }

        let existing_link = sqlx::query_as::<_, (Uuid, Uuid)>(
            "SELECT subscription_id, id FROM subscription_charges \
             WHERE transaction_id=$1 FOR UPDATE",
        )
        .bind(command.transaction_id)
        .fetch_optional(&mut *db_tx)
        .await?;
        if let Some((subscription_id, charge_id)) = existing_link {
            let outcome = match &command.target {
                TransactionSubscriptionTarget::Create { .. } => {
                    MarkTransactionSubscriptionOutcome::AlreadyLinked {
                        subscription_id,
                        charge_id,
                    }
                }
                TransactionSubscriptionTarget::Attach {
                    subscription_id: requested,
                } if *requested == subscription_id => {
                    MarkTransactionSubscriptionOutcome::AlreadyLinked {
                        subscription_id,
                        charge_id,
                    }
                }
                TransactionSubscriptionTarget::Attach { .. } => {
                    MarkTransactionSubscriptionOutcome::TransactionAlreadyLinked {
                        subscription_id,
                        charge_id,
                    }
                }
            };
            db_tx.commit().await?;
            return Ok(outcome);
        }

        let now = command.requested_at.timestamp();
        let charged_at = transacted_at.timestamp();
        let source_key = format!("manual:transaction:{}", command.transaction_id);
        let (
            subscription_id,
            subscription_created,
            effective_period,
            subscription_category_id,
            manual_subscription,
            last_charged_at,
        ) = match &command.target {
            TransactionSubscriptionTarget::Create {
                subscription_id,
                product_name,
                billing_period,
            } => {
                let merchant_key = format!("manual:{subscription_id}");
                let next_expected_at = billing_period.next_after(transacted_at).timestamp();
                sqlx::query(
                        "INSERT INTO subscriptions \
                         (id,user_id,provider,product_name,merchant_key,amount,currency,billing_period,\
                          status,started_at,last_charged_at,next_expected_at,category_id,created_at,\
                          last_receipt_at) \
                         VALUES ($1,$2,'other',$3,$4,$5,$6,$7,'active',$8,$8,$9,$10,$11,$8)",
                    )
                    .bind(subscription_id)
                    .bind(command.user_id)
                    .bind(product_name)
                    .bind(merchant_key)
                    .bind(amount)
                    .bind(&currency)
                    .bind(billing_period.as_str())
                    .bind(charged_at)
                    .bind(next_expected_at)
                    .bind(transaction_category_id)
                    .bind(now)
                    .execute(&mut *db_tx)
                    .await?;
                (
                    *subscription_id,
                    true,
                    *billing_period,
                    transaction_category_id,
                    true,
                    None,
                )
            }
            TransactionSubscriptionTarget::Attach { subscription_id } => {
                let subscription = sqlx::query_as::<
                    _,
                    (String, Option<String>, String, Option<Uuid>, Option<i64>),
                >(
                    "SELECT billing_period,billing_period_override,merchant_key,category_id,\
                                last_charged_at \
                         FROM subscriptions WHERE id=$1 AND user_id=$2 FOR UPDATE",
                )
                .bind(subscription_id)
                .bind(command.user_id)
                .fetch_optional(&mut *db_tx)
                .await?;
                let Some((period, period_override, merchant_key, category_id, last_charged_at)) =
                    subscription
                else {
                    db_tx.rollback().await?;
                    return Ok(MarkTransactionSubscriptionOutcome::SubscriptionNotFound);
                };
                let effective_period =
                    BillingPeriod::from_str(period_override.as_deref().unwrap_or(period.as_str()))?;
                (
                    *subscription_id,
                    false,
                    effective_period,
                    category_id,
                    merchant_key.starts_with("manual:"),
                    last_charged_at,
                )
            }
        };

        let charge_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO subscription_charges \
             (id,subscription_id,user_id,amount,currency,charged_at,email_message_id,rfc_message_id,\
              kind,transaction_id,match_status,created_at,source,source_key,source_connection_id,\
              provider_message_id,match_started_at,match_source) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,NULL,$8,$9,'Matched',$10,'manual',$7,NULL,NULL,$10,'manual')",
        )
        .bind(charge_id)
        .bind(subscription_id)
        .bind(command.user_id)
        .bind(amount)
        .bind(&currency)
        .bind(charged_at)
        .bind(&source_key)
        .bind(if subscription_created {
            "new_subscription"
        } else {
            "renewal"
        })
        .bind(command.transaction_id)
        .bind(now)
        .execute(&mut *db_tx)
        .await?;

        if !subscription_created {
            sqlx::query("UPDATE subscriptions SET started_at=LEAST(started_at,$1) WHERE id=$2")
                .bind(charged_at)
                .bind(subscription_id)
                .execute(&mut *db_tx)
                .await?;
            let newest_charge = last_charged_at.is_none_or(|last| charged_at >= last);
            if newest_charge {
                let next_expected_at = effective_period.next_after(transacted_at).timestamp();
                sqlx::query(
                    "UPDATE subscriptions SET \
                       last_charged_at=$1,next_expected_at=$2,status='active',\
                       amount=CASE WHEN $3 THEN $4 ELSE amount END,\
                       currency=CASE WHEN $3 THEN $5 ELSE currency END \
                     WHERE id=$6",
                )
                .bind(charged_at)
                .bind(next_expected_at)
                .bind(manual_subscription)
                .bind(amount)
                .bind(&currency)
                .bind(subscription_id)
                .execute(&mut *db_tx)
                .await?;
            }
            if let Some(category_id) = subscription_category_id {
                sqlx::query(
                    "UPDATE transactions SET category_id=$1 \
                     WHERE id=$2 AND user_id=$3 AND category_id IS NULL",
                )
                .bind(category_id)
                .bind(command.transaction_id)
                .bind(command.user_id)
                .execute(&mut *db_tx)
                .await?;
            }
        }

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
        .execute(&mut *db_tx)
        .await?;

        db_tx.commit().await?;
        Ok(MarkTransactionSubscriptionOutcome::Created {
            subscription_id,
            charge_id,
            subscription_created,
        })
    }

    async fn upsert_by_merchant_key(&self, sub: &Subscription) -> anyhow::Result<Subscription> {
        self.upsert_receipt_if_not_tombstoned(sub)
            .await?
            .map(|result| result.subscription)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "subscription {}/{} was previously deleted",
                    sub.provider.as_str(),
                    sub.merchant_key
                )
            })
    }

    async fn upsert_receipt_if_not_tombstoned(
        &self,
        sub: &Subscription,
    ) -> anyhow::Result<Option<SubscriptionUpsertResult>> {
        let mut tx = self.pool.begin().await?;

        // Serialize receipt ingestion and deletion for exactly one merchant.
        // Delete uses the same key expression before inserting its tombstone.
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended(\
                $1::uuid::text || ':' || $2 || ':' || $3, 0\
             ))",
        )
        .bind(sub.user_id)
        .bind(sub.provider.as_str())
        .bind(&sub.merchant_key)
        .execute(&mut *tx)
        .await?;

        let tombstoned = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(\
                SELECT 1 FROM subscription_tombstones \
                WHERE user_id=$1 AND provider=$2 AND merchant_key=$3\
             )",
        )
        .bind(sub.user_id)
        .bind(sub.provider.as_str())
        .bind(&sub.merchant_key)
        .fetch_one(&mut *tx)
        .await?;
        if tombstoned {
            tx.rollback().await?;
            return Ok(None);
        }

        let inserted = !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(\
                SELECT 1 FROM subscriptions WHERE user_id=$1 AND merchant_key=$2\
             )",
        )
        .bind(sub.user_id)
        .bind(&sub.merchant_key)
        .fetch_one(&mut *tx)
        .await?;

        let receipt_at = sub.last_charged_at.unwrap_or(sub.started_at).timestamp();
        let row = sqlx::query_as::<_, Row>(
            "INSERT INTO subscriptions \
             (id, user_id, provider, product_name, merchant_key, amount, currency, billing_period, \
              status, started_at, last_charged_at, next_expected_at, category_id, created_at, \
              last_receipt_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15) \
             ON CONFLICT (user_id, merchant_key) DO UPDATE SET \
               started_at = LEAST(EXCLUDED.started_at, subscriptions.started_at), \
               amount = CASE WHEN EXCLUDED.last_receipt_at > subscriptions.last_receipt_at \
                             THEN EXCLUDED.amount ELSE subscriptions.amount END, \
               currency = CASE WHEN EXCLUDED.last_receipt_at > subscriptions.last_receipt_at \
                               THEN EXCLUDED.currency ELSE subscriptions.currency END, \
               billing_period = CASE WHEN EXCLUDED.last_receipt_at > subscriptions.last_receipt_at \
                                     THEN EXCLUDED.billing_period ELSE subscriptions.billing_period END, \
               product_name = CASE WHEN EXCLUDED.last_receipt_at > subscriptions.last_receipt_at \
                                   THEN EXCLUDED.product_name ELSE subscriptions.product_name END, \
               last_charged_at = CASE WHEN EXCLUDED.last_receipt_at > subscriptions.last_receipt_at \
                                      THEN COALESCE(EXCLUDED.last_charged_at, subscriptions.last_charged_at) \
                                      ELSE subscriptions.last_charged_at END, \
               next_expected_at = CASE WHEN EXCLUDED.last_receipt_at > subscriptions.last_receipt_at \
                                       THEN COALESCE(EXCLUDED.next_expected_at, subscriptions.next_expected_at) \
                                       ELSE subscriptions.next_expected_at END, \
               status = CASE WHEN EXCLUDED.last_receipt_at > subscriptions.last_receipt_at \
                             THEN EXCLUDED.status ELSE subscriptions.status END, \
               last_receipt_at = GREATEST(EXCLUDED.last_receipt_at, subscriptions.last_receipt_at) \
             RETURNING *",
        )
        .bind(sub.id)
        .bind(sub.user_id)
        .bind(sub.provider.as_str())
        .bind(&sub.product_name)
        .bind(&sub.merchant_key)
        .bind(sub.amount)
        .bind(&sub.currency)
        .bind(sub.billing_period.as_str())
        .bind(sub.status.as_str())
        .bind(sub.started_at.timestamp())
        .bind(sub.last_charged_at.map(|d| d.timestamp()))
        .bind(sub.next_expected_at.map(|d| d.timestamp()))
        .bind(sub.category_id)
        .bind(sub.created_at.timestamp())
        .bind(receipt_at)
        .fetch_one(&mut *tx)
        .await?;
        let subscription = row_to_sub(row)?;
        tx.commit().await?;
        Ok(Some(SubscriptionUpsertResult {
            subscription,
            inserted,
        }))
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<Option<Subscription>> {
        let row =
            sqlx::query_as::<_, Row>("SELECT * FROM subscriptions WHERE id=$1 AND user_id=$2")
                .bind(id)
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await?;
        row.map(row_to_sub).transpose()
    }

    async fn list_by_user(
        &self,
        user_id: Uuid,
        filter: &SubscriptionListFilter,
    ) -> anyhow::Result<Vec<Subscription>> {
        let rows = if let Some(status) = &filter.status {
            sqlx::query_as::<_, Row>(
                "SELECT * FROM subscriptions \
                 WHERE user_id=$1 AND COALESCE(status_override, status)=$2 \
                 ORDER BY created_at",
            )
            .bind(user_id)
            .bind(status.as_str())
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, Row>(
                "SELECT * FROM subscriptions WHERE user_id=$1 ORDER BY created_at",
            )
            .bind(user_id)
            .fetch_all(&self.pool)
            .await?
        };
        rows.into_iter().map(row_to_sub).collect()
    }

    async fn update_after_charge(
        &self,
        id: Uuid,
        last_charged_at: DateTime<Utc>,
        next_expected_at: DateTime<Utc>,
        status: SubscriptionStatus,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE subscriptions SET \
               last_charged_at = CASE \
                   WHEN last_charged_at IS NULL OR last_charged_at <= $1 THEN $1 \
                   ELSE last_charged_at END, \
               next_expected_at = CASE \
                   WHEN last_charged_at IS NULL OR last_charged_at <= $1 THEN $2 \
                   ELSE next_expected_at END, \
               status=$3 \
             WHERE id=$4",
        )
        .bind(last_charged_at.timestamp())
        .bind(next_expected_at.timestamp())
        .bind(status.as_str())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_editable_fields(
        &self,
        id: Uuid,
        user_id: Uuid,
        product_name: Option<Option<String>>,
        category_id: Option<Option<Uuid>>,
        billing_period: Option<Option<BillingPeriod>>,
        status: Option<Option<SubscriptionStatus>>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE subscriptions SET \
               product_name_override   = CASE WHEN $1 THEN $2 ELSE product_name_override END, \
               billing_period_override = CASE WHEN $3 THEN $4 ELSE billing_period_override END, \
               status_override         = CASE WHEN $5 THEN $6 ELSE status_override END, \
               category_id             = CASE WHEN $7 THEN $8 ELSE category_id END \
             WHERE id=$9 AND user_id=$10",
        )
        .bind(product_name.is_some())
        .bind(product_name.flatten())
        .bind(billing_period.is_some())
        .bind(billing_period.flatten().map(|value| value.as_str()))
        .bind(status.is_some())
        .bind(status.flatten().map(|value| value.as_str()))
        .bind(category_id.is_some())
        .bind(category_id.flatten())
        .bind(id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_lapsed(&self, before: DateTime<Utc>) -> anyhow::Result<Vec<Subscription>> {
        let rows = sqlx::query_as::<_, Row>(
            "SELECT * FROM subscriptions \
             WHERE COALESCE(status_override, status) = 'active' \
               AND (next_expected_at IS NOT NULL OR billing_period_override IS NOT NULL)",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(row_to_sub)
            .collect::<anyhow::Result<Vec<_>>>()
            .map(|subscriptions| {
                subscriptions
                    .into_iter()
                    .filter(|subscription| {
                        let next_expected = if subscription.overrides.billing_period.is_some() {
                            Some(
                                subscription.billing_period.next_after(
                                    subscription
                                        .last_charged_at
                                        .unwrap_or(subscription.started_at),
                                ),
                            )
                        } else {
                            subscription.next_expected_at
                        };
                        next_expected.is_some_and(|next_expected| next_expected < before)
                    })
                    .collect()
            })
    }

    async fn mark_lapsed(&self, before: DateTime<Utc>) -> anyhow::Result<u64> {
        let result = sqlx::query(
            "UPDATE subscriptions SET status='inactive' \
             WHERE status_override IS NULL AND status='active' \
               AND ( \
                 (billing_period_override IS NULL \
                   AND next_expected_at IS NOT NULL AND next_expected_at < $1) \
                 OR \
                 (billing_period_override IS NOT NULL AND \
                   EXTRACT(EPOCH FROM ( \
                     (to_timestamp(COALESCE(last_charged_at, started_at)) AT TIME ZONE 'UTC') + \
                     CASE billing_period_override \
                       WHEN 'weekly' THEN INTERVAL '1 week' \
                       WHEN 'monthly' THEN INTERVAL '1 month' \
                       WHEN 'yearly' THEN INTERVAL '1 year' \
                     END \
                   ))::BIGINT < $1) \
               )",
        )
        .bind(before.timestamp())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        let merchant = sqlx::query_as::<_, (String, String)>(
            "SELECT provider, merchant_key FROM subscriptions WHERE id=$1 AND user_id=$2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((provider, merchant_key)) = merchant else {
            tx.rollback().await?;
            return Ok(());
        };

        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended(\
                $1::uuid::text || ':' || $2 || ':' || $3, 0\
             ))",
        )
        .bind(user_id)
        .bind(&provider)
        .bind(&merchant_key)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "WITH removed AS (\
                DELETE FROM subscriptions \
                WHERE id=$1 AND user_id=$2 \
                RETURNING user_id, provider, merchant_key\
             ) \
             INSERT INTO subscription_tombstones \
                (user_id, provider, merchant_key, deleted_at) \
             SELECT user_id, provider, merchant_key, $3 FROM removed \
             ON CONFLICT (user_id, provider, merchant_key) DO UPDATE SET \
                deleted_at=EXCLUDED.deleted_at",
        )
        .bind(id)
        .bind(user_id)
        .bind(Utc::now().timestamp())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::account::{Account, AccountDetails, AccountRepository, AccountType};
    use crate::domain::transaction::{
        Transaction, TransactionDetails, TransactionKind, TransactionRepository,
    };
    use crate::infrastructure::account_repository::SqliteAccountRepository;
    use crate::infrastructure::test_db;
    use crate::infrastructure::transaction_repository::SqliteTransactionRepository;
    use rust_decimal_macros::dec;

    fn sample(user_id: Uuid, key: &str) -> Subscription {
        Subscription {
            id: Uuid::new_v4(),
            user_id,
            provider: SubscriptionProvider::Netflix,
            product_name: "Netflix Premium".to_string(),
            merchant_key: key.to_string(),
            amount: dec!(15.99),
            currency: "USD".to_string(),
            billing_period: BillingPeriod::Monthly,
            status: SubscriptionStatus::Active,
            started_at: Utc::now(),
            last_charged_at: None,
            next_expected_at: None,
            category_id: None,
            overrides: Default::default(),
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn upsert_inserts_then_updates() {
        let pool = test_db::fresh_pool().await;
        let repo = PgSubscriptionRepository::new(pool);
        let user_id = Uuid::new_v4();
        let s1 = sample(user_id, "netflix.com:premium");
        let inserted = repo.upsert_by_merchant_key(&s1).await.unwrap();
        assert_eq!(inserted.id, s1.id);

        let mut s2 = sample(user_id, "netflix.com:premium");
        s2.amount = dec!(17.99);
        s2.started_at = s1.started_at + chrono::Duration::seconds(1);
        let upserted = repo.upsert_by_merchant_key(&s2).await.unwrap();
        assert_eq!(upserted.id, s1.id, "same id reused");
        assert_eq!(upserted.amount, dec!(17.99));
    }

    #[tokio::test]
    async fn list_lapsed_returns_active_past_threshold() {
        let pool = test_db::fresh_pool().await;
        let repo = PgSubscriptionRepository::new(pool);
        let user_id = Uuid::new_v4();
        let mut s = sample(user_id, "netflix.com:premium");
        s.next_expected_at = Some(Utc::now() - chrono::Duration::days(10));
        repo.upsert_by_merchant_key(&s).await.unwrap();
        let lapsed = repo
            .list_lapsed(Utc::now() - chrono::Duration::days(7))
            .await
            .unwrap();
        assert_eq!(lapsed.len(), 1);
    }

    #[tokio::test]
    async fn list_lapsed_uses_effective_billing_period_override() {
        let pool = test_db::fresh_pool().await;
        let repo = PgSubscriptionRepository::new(pool);
        let user_id = Uuid::new_v4();
        let mut subscription = sample(user_id, "netflix.com:override-period");
        subscription.started_at = Utc::now() - chrono::Duration::days(60);
        subscription.last_charged_at = Some(Utc::now() - chrono::Duration::days(40));
        subscription.next_expected_at = Some(Utc::now() - chrono::Duration::days(10));
        repo.upsert_by_merchant_key(&subscription).await.unwrap();
        repo.update_editable_fields(
            subscription.id,
            user_id,
            None,
            None,
            Some(Some(BillingPeriod::Yearly)),
            None,
        )
        .await
        .unwrap();

        let lapsed = repo
            .list_lapsed(Utc::now() - chrono::Duration::days(7))
            .await
            .unwrap();
        assert!(lapsed.is_empty());
    }

    #[tokio::test]
    async fn concurrent_manual_creation_reserves_transaction_once() {
        let pool = test_db::fresh_pool().await;
        let user_id = Uuid::new_v4();
        let accounts = SqliteAccountRepository::new(pool.clone());
        let account = Account::new(user_id, "Card".into(), AccountType::Cash, "USD".into());
        accounts
            .create(&account, &AccountDetails::None)
            .await
            .unwrap();
        let transactions = SqliteTransactionRepository::new(pool.clone());
        let transaction = Transaction::new(
            account.id,
            user_id,
            dec!(9.99),
            "USD".into(),
            TransactionKind::Expense,
            None,
            None,
            Utc::now(),
        );
        transactions
            .create(&transaction, &TransactionDetails::None)
            .await
            .unwrap();

        let repo = PgSubscriptionRepository::new(pool.clone());
        let command = |subscription_id| MarkTransactionSubscription {
            user_id,
            transaction_id: transaction.id,
            target: TransactionSubscriptionTarget::Create {
                subscription_id,
                product_name: "Music".into(),
                billing_period: BillingPeriod::Monthly,
            },
            requested_at: Utc::now(),
        };
        let first_command = command(Uuid::new_v4());
        let second_command = command(Uuid::new_v4());
        let (first, second) = tokio::join!(
            repo.mark_transaction_as_subscription(&first_command),
            repo.mark_transaction_as_subscription(&second_command)
        );
        let outcomes = [first.unwrap(), second.unwrap()];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    MarkTransactionSubscriptionOutcome::Created { .. }
                ))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    MarkTransactionSubscriptionOutcome::AlreadyLinked { .. }
                ))
                .count(),
            1
        );
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
    }
}
