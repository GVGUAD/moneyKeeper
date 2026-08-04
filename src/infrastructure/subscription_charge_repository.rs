use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::subscription_charge::{
    ChargeMatchStatus, ReceiptKind, SubscriptionCharge, SubscriptionChargeRepository,
};

pub struct PgSubscriptionChargeRepository {
    pool: PgPool,
}

impl PgSubscriptionChargeRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    subscription_id: Uuid,
    user_id: Uuid,
    amount: Decimal,
    currency: String,
    charged_at: i64,
    email_message_id: String,
    kind: String,
    transaction_id: Option<Uuid>,
    match_status: String,
    created_at: i64,
}

fn row_to_charge(r: Row) -> anyhow::Result<SubscriptionCharge> {
    Ok(SubscriptionCharge {
        id: r.id,
        subscription_id: r.subscription_id,
        user_id: r.user_id,
        amount: r.amount,
        currency: r.currency,
        charged_at: DateTime::from_timestamp(r.charged_at, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid charged_at"))?,
        email_message_id: r.email_message_id,
        kind: ReceiptKind::from_str(&r.kind)?,
        transaction_id: r.transaction_id,
        match_status: ChargeMatchStatus::from_str(&r.match_status)?,
        created_at: DateTime::from_timestamp(r.created_at, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid created_at"))?,
    })
}

#[async_trait::async_trait]
impl SubscriptionChargeRepository for PgSubscriptionChargeRepository {
    async fn create_idempotent(
        &self,
        charge: &SubscriptionCharge,
    ) -> anyhow::Result<(Uuid, bool)> {
        let row = sqlx::query_as::<_, (Uuid,)>(
            "INSERT INTO subscription_charges \
             (id, subscription_id, user_id, amount, currency, charged_at, email_message_id, \
              kind, transaction_id, match_status, created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) \
             ON CONFLICT (email_message_id) DO NOTHING \
             RETURNING id",
        )
        .bind(charge.id)
        .bind(charge.subscription_id)
        .bind(charge.user_id)
        .bind(charge.amount)
        .bind(&charge.currency)
        .bind(charge.charged_at.timestamp())
        .bind(&charge.email_message_id)
        .bind(charge.kind.as_str())
        .bind(charge.transaction_id)
        .bind(charge.match_status.as_str())
        .bind(charge.created_at.timestamp())
        .fetch_optional(&self.pool)
        .await?;

        if let Some((id,)) = row {
            return Ok((id, true));
        }

        // Conflict — fetch existing id
        let (existing_id,) = sqlx::query_as::<_, (Uuid,)>(
            "SELECT id FROM subscription_charges WHERE email_message_id=$1",
        )
        .bind(&charge.email_message_id)
        .fetch_one(&self.pool)
        .await?;

        Ok((existing_id, false))
    }

    async fn find_by_id(
        &self,
        id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Option<SubscriptionCharge>> {
        let row = sqlx::query_as::<_, Row>(
            "SELECT * FROM subscription_charges WHERE id=$1 AND user_id=$2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_charge).transpose()
    }

    async fn list_pending_for_user(
        &self,
        user_id: Uuid,
    ) -> anyhow::Result<Vec<SubscriptionCharge>> {
        let rows = sqlx::query_as::<_, Row>(
            "SELECT * FROM subscription_charges \
             WHERE user_id=$1 AND match_status='Pending' \
             ORDER BY charged_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_charge).collect()
    }

    async fn list_for_subscription(
        &self,
        subscription_id: Uuid,
        user_id: Uuid,
    ) -> anyhow::Result<Vec<SubscriptionCharge>> {
        let rows = sqlx::query_as::<_, Row>(
            "SELECT * FROM subscription_charges \
             WHERE subscription_id=$1 AND user_id=$2 \
             ORDER BY charged_at DESC",
        )
        .bind(subscription_id)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_charge).collect()
    }

    async fn update_match(
        &self,
        id: Uuid,
        transaction_id: Option<Uuid>,
        match_status: ChargeMatchStatus,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE subscription_charges SET transaction_id=$1, match_status=$2 WHERE id=$3",
        )
        .bind(transaction_id)
        .bind(match_status.as_str())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn mark_pending_older_than_unmatched(
        &self,
        threshold: DateTime<Utc>,
    ) -> anyhow::Result<u64> {
        let result = sqlx::query(
            "UPDATE subscription_charges \
             SET match_status='Unmatched' \
             WHERE match_status='Pending' AND charged_at < $1",
        )
        .bind(threshold.timestamp())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::subscription::{
        BillingPeriod, Subscription, SubscriptionProvider, SubscriptionRepository,
        SubscriptionStatus,
    };
    use crate::infrastructure::subscription_repository::PgSubscriptionRepository;
    use crate::infrastructure::test_db;
    use rust_decimal_macros::dec;

    async fn make_subscription(pool: &sqlx::PgPool, user_id: Uuid) -> Uuid {
        let sub = Subscription {
            id: Uuid::new_v4(),
            user_id,
            provider: SubscriptionProvider::Netflix,
            product_name: "Netflix".to_string(),
            merchant_key: "netflix.com:premium".to_string(),
            amount: dec!(15.99),
            currency: "USD".to_string(),
            billing_period: BillingPeriod::Monthly,
            status: SubscriptionStatus::Active,
            started_at: Utc::now(),
            last_charged_at: None,
            next_expected_at: None,
            category_id: None,
            created_at: Utc::now(),
        };
        let id = sub.id;
        PgSubscriptionRepository::new(pool.clone())
            .upsert_by_merchant_key(&sub)
            .await
            .unwrap();
        id
    }

    fn charge(user_id: Uuid, sub_id: Uuid, msg: &str) -> SubscriptionCharge {
        SubscriptionCharge {
            id: Uuid::new_v4(),
            subscription_id: sub_id,
            user_id,
            amount: dec!(15.99),
            currency: "USD".to_string(),
            charged_at: Utc::now(),
            email_message_id: msg.to_string(),
            kind: ReceiptKind::Renewal,
            transaction_id: None,
            match_status: ChargeMatchStatus::Pending,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn create_idempotent_second_call_returns_false() {
        let pool = test_db::fresh_pool().await;
        let user_id = Uuid::new_v4();
        let sub_id = make_subscription(&pool, user_id).await;
        let repo = PgSubscriptionChargeRepository::new(pool);
        let c1 = charge(user_id, sub_id, "msg-1");
        let (id1, inserted1) = repo.create_idempotent(&c1).await.unwrap();
        assert!(inserted1);
        let c2 = charge(user_id, sub_id, "msg-1");
        let (id2, inserted2) = repo.create_idempotent(&c2).await.unwrap();
        assert!(!inserted2);
        assert_eq!(id1, id2);
    }

    #[tokio::test]
    async fn list_pending_for_user_filters_by_status() {
        let pool = test_db::fresh_pool().await;
        let user_id = Uuid::new_v4();
        let sub_id = make_subscription(&pool, user_id).await;
        let repo = PgSubscriptionChargeRepository::new(pool);
        let c1 = charge(user_id, sub_id, "msg-1");
        let c2 = charge(user_id, sub_id, "msg-2");
        repo.create_idempotent(&c1).await.unwrap();
        repo.create_idempotent(&c2).await.unwrap();
        repo.update_match(c2.id, None, ChargeMatchStatus::Unmatched)
            .await
            .unwrap();
        let pending = repo.list_pending_for_user(user_id).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, c1.id);
    }
}
