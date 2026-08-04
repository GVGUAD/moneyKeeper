use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::subscription::{
    BillingPeriod, Subscription, SubscriptionListFilter, SubscriptionProvider,
    SubscriptionRepository, SubscriptionStatus,
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
    merchant_key: String,
    amount: Decimal,
    currency: String,
    billing_period: String,
    status: String,
    started_at: i64,
    last_charged_at: Option<i64>,
    next_expected_at: Option<i64>,
    category_id: Option<Uuid>,
    created_at: i64,
}

fn row_to_sub(r: Row) -> anyhow::Result<Subscription> {
    Ok(Subscription {
        id: r.id,
        user_id: r.user_id,
        provider: SubscriptionProvider::from_str(&r.provider)?,
        product_name: r.product_name,
        merchant_key: r.merchant_key,
        amount: r.amount,
        currency: r.currency,
        billing_period: BillingPeriod::from_str(&r.billing_period)?,
        status: SubscriptionStatus::from_str(&r.status)?,
        started_at: DateTime::from_timestamp(r.started_at, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid started_at"))?,
        last_charged_at: r.last_charged_at.and_then(|t| DateTime::from_timestamp(t, 0)),
        next_expected_at: r.next_expected_at.and_then(|t| DateTime::from_timestamp(t, 0)),
        category_id: r.category_id,
        created_at: DateTime::from_timestamp(r.created_at, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid created_at"))?,
    })
}

#[async_trait::async_trait]
impl SubscriptionRepository for PgSubscriptionRepository {
    async fn upsert_by_merchant_key(&self, sub: &Subscription) -> anyhow::Result<Subscription> {
        let row = sqlx::query_as::<_, Row>(
            "INSERT INTO subscriptions \
             (id, user_id, provider, product_name, merchant_key, amount, currency, billing_period, \
              status, started_at, last_charged_at, next_expected_at, category_id, created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) \
             ON CONFLICT (user_id, merchant_key) DO UPDATE SET \
               amount = EXCLUDED.amount, \
               currency = EXCLUDED.currency, \
               billing_period = EXCLUDED.billing_period, \
               product_name = EXCLUDED.product_name \
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
        .fetch_one(&self.pool)
        .await?;
        row_to_sub(row)
    }

    async fn find_by_id(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<Option<Subscription>> {
        let row = sqlx::query_as::<_, Row>(
            "SELECT * FROM subscriptions WHERE id=$1 AND user_id=$2",
        )
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
                "SELECT * FROM subscriptions WHERE user_id=$1 AND status=$2 ORDER BY created_at",
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
            "UPDATE subscriptions SET last_charged_at=$1, next_expected_at=$2, status=$3 WHERE id=$4",
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
        product_name: Option<String>,
        category_id: Option<Option<Uuid>>,
        billing_period: Option<BillingPeriod>,
        status: Option<SubscriptionStatus>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE subscriptions SET \
               product_name   = COALESCE($1, product_name), \
               billing_period = COALESCE($2, billing_period), \
               status         = COALESCE($3, status), \
               category_id    = CASE WHEN $4 THEN $5 ELSE category_id END \
             WHERE id=$6 AND user_id=$7",
        )
        .bind(product_name)
        .bind(billing_period.map(|b| b.as_str()))
        .bind(status.map(|s| s.as_str()))
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
             WHERE status = 'active' \
               AND next_expected_at IS NOT NULL \
               AND next_expected_at < $1",
        )
        .bind(before.timestamp())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_sub).collect()
    }

    async fn delete(&self, id: Uuid, user_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM subscriptions WHERE id=$1 AND user_id=$2")
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
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
}
