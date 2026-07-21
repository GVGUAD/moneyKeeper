use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::subscription::{
    BillingPeriod, Subscription, SubscriptionListFilter, SubscriptionOverrides,
    SubscriptionProvider, SubscriptionRepository, SubscriptionStatus, SubscriptionUpsertResult,
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
    use crate::infrastructure::test_db;
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
}
