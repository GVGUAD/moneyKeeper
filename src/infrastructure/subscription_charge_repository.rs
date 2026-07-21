use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::subscription_charge::{
    ChargeLinkOutcome, ChargeMatchSource, ChargeMatchStatus, ChargeSource, ReceiptKind,
    SubscriptionCharge, SubscriptionChargeRepository,
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
    rfc_message_id: Option<String>,
    source: String,
    source_key: String,
    source_connection_id: Option<Uuid>,
    provider_message_id: Option<String>,
    kind: String,
    transaction_id: Option<Uuid>,
    match_status: String,
    match_started_at: i64,
    match_source: Option<String>,
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
        rfc_message_id: r.rfc_message_id,
        source: ChargeSource::from_str(&r.source)?,
        source_key: r.source_key,
        source_connection_id: r.source_connection_id,
        provider_message_id: r.provider_message_id,
        kind: ReceiptKind::from_str(&r.kind)?,
        transaction_id: r.transaction_id,
        match_status: ChargeMatchStatus::from_str(&r.match_status)?,
        match_started_at: DateTime::from_timestamp(r.match_started_at, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid match_started_at"))?,
        match_source: r
            .match_source
            .as_deref()
            .map(ChargeMatchSource::from_str)
            .transpose()?,
        created_at: DateTime::from_timestamp(r.created_at, 0)
            .ok_or_else(|| anyhow::anyhow!("invalid created_at"))?,
    })
}

#[async_trait::async_trait]
impl SubscriptionChargeRepository for PgSubscriptionChargeRepository {
    async fn create_idempotent(&self, charge: &SubscriptionCharge) -> anyhow::Result<(Uuid, bool)> {
        let mut tx = self.pool.begin().await?;

        // Existing rows used RFC Message-ID as their only identity. Attach the
        // durable Gmail provider key on the first replay after migration.
        if let Some(rfc_message_id) = &charge.rfc_message_id {
            let attached = sqlx::query_scalar::<_, Uuid>(
                "UPDATE subscription_charges SET \
                   source=$1, source_key=$2, email_message_id=$2, source_connection_id=$3, \
                   provider_message_id=$4 \
                 WHERE user_id=$5 AND rfc_message_id=$6 \
                   AND source_key LIKE 'legacy:%' \
                 RETURNING id",
            )
            .bind(charge.source.as_str())
            .bind(&charge.source_key)
            .bind(charge.source_connection_id)
            .bind(&charge.provider_message_id)
            .bind(charge.user_id)
            .bind(rfc_message_id)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(id) = attached {
                tx.commit().await?;
                return Ok((id, false));
            }
        }

        let row = sqlx::query_as::<_, (Uuid,)>(
            "INSERT INTO subscription_charges \
             (id, subscription_id, user_id, amount, currency, charged_at, email_message_id, \
              rfc_message_id, source, source_key, source_connection_id, provider_message_id, kind, \
              transaction_id, match_status, match_started_at, match_source, created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18) \
             ON CONFLICT DO NOTHING \
             RETURNING id",
        )
        .bind(charge.id)
        .bind(charge.subscription_id)
        .bind(charge.user_id)
        .bind(charge.amount)
        .bind(&charge.currency)
        .bind(charge.charged_at.timestamp())
        .bind(&charge.email_message_id)
        .bind(&charge.rfc_message_id)
        .bind(charge.source.as_str())
        .bind(&charge.source_key)
        .bind(charge.source_connection_id)
        .bind(&charge.provider_message_id)
        .bind(charge.kind.as_str())
        .bind(charge.transaction_id)
        .bind(charge.match_status.as_str())
        .bind(charge.match_started_at.timestamp())
        .bind(charge.match_source.map(|source| source.as_str()))
        .bind(charge.created_at.timestamp())
        .fetch_optional(&mut *tx)
        .await?;

        if let Some((id,)) = row {
            tx.commit().await?;
            return Ok((id, true));
        }

        // A durable source-key conflict. Legacy RFC Message-ID bridging was
        // handled above and is deliberately not general idempotency: the same
        // RFC message may legitimately appear in two Gmail mailboxes.
        let existing_id = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM subscription_charges WHERE source_key=$1",
        )
        .bind(&charge.source_key)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
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

    async fn list_users_with_pending(&self) -> anyhow::Result<Vec<Uuid>> {
        Ok(sqlx::query_scalar(
            "SELECT DISTINCT user_id FROM subscription_charges \
             WHERE match_status='Pending' ORDER BY user_id",
        )
        .fetch_all(&self.pool)
        .await?)
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

    async fn link_transaction(
        &self,
        id: Uuid,
        user_id: Uuid,
        transaction_id: Uuid,
        source: ChargeMatchSource,
    ) -> anyhow::Result<ChargeLinkOutcome> {
        let mut db_tx = self.pool.begin().await?;

        let charge = sqlx::query_as::<_, (Uuid, String, Option<Uuid>)>(
            "SELECT subscription_id, match_status, transaction_id \
             FROM subscription_charges \
             WHERE id=$1 AND user_id=$2 \
             FOR UPDATE",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&mut *db_tx)
        .await?;
        let Some((subscription_id, match_status, current_transaction_id)) = charge else {
            db_tx.rollback().await?;
            return Ok(ChargeLinkOutcome::ChargeNotFound);
        };
        if source == ChargeMatchSource::Automatic && match_status != "Pending" {
            db_tx.rollback().await?;
            return Ok(ChargeLinkOutcome::ChargeNotPending);
        }
        if current_transaction_id.is_some() || match_status == "Matched" {
            db_tx.rollback().await?;
            return Ok(ChargeLinkOutcome::ChargeAlreadyLinked);
        }

        let transaction_kind = sqlx::query_scalar::<_, String>(
            "SELECT kind FROM transactions WHERE id=$1 AND user_id=$2 FOR UPDATE",
        )
        .bind(transaction_id)
        .bind(user_id)
        .fetch_optional(&mut *db_tx)
        .await?;
        let Some(transaction_kind) = transaction_kind else {
            db_tx.rollback().await?;
            return Ok(ChargeLinkOutcome::TransactionNotFound);
        };
        if transaction_kind != "Expense" {
            db_tx.rollback().await?;
            return Ok(ChargeLinkOutcome::TransactionNotExpense);
        }

        let already_linked = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS( \
                 SELECT 1 FROM subscription_charges \
                 WHERE transaction_id=$1 AND id<>$2 \
             )",
        )
        .bind(transaction_id)
        .bind(id)
        .fetch_one(&mut *db_tx)
        .await?;
        if already_linked {
            db_tx.rollback().await?;
            return Ok(ChargeLinkOutcome::TransactionAlreadyLinked);
        }

        let subscription = sqlx::query_as::<_, (Option<Uuid>,)>(
            "SELECT category_id \
             FROM subscriptions \
             WHERE id=$1 AND user_id=$2 \
             FOR UPDATE",
        )
        .bind(subscription_id)
        .bind(user_id)
        .fetch_optional(&mut *db_tx)
        .await?;
        let Some((category_id,)) = subscription else {
            db_tx.rollback().await?;
            return Ok(ChargeLinkOutcome::ChargeNotFound);
        };

        let update_result = sqlx::query(
            "UPDATE subscription_charges \
             SET transaction_id=$1, match_status='Matched', match_source=$2 \
             WHERE id=$3 AND user_id=$4",
        )
        .bind(transaction_id)
        .bind(source.as_str())
        .bind(id)
        .bind(user_id)
        .execute(&mut *db_tx)
        .await;
        if let Err(error) = update_result {
            let transaction_link_conflict = error.as_database_error().is_some_and(|db| {
                db.code().as_deref() == Some("23505")
                    && db.constraint() == Some("subscription_charges_transaction_unique")
            });
            if transaction_link_conflict {
                db_tx.rollback().await?;
                return Ok(ChargeLinkOutcome::TransactionAlreadyLinked);
            }
            return Err(error.into());
        }

        if let Some(category_id) = category_id {
            sqlx::query(
                "UPDATE transactions t \
                 SET category_id=$1 \
                 WHERE t.id=$2 AND t.user_id=$3 AND t.category_id IS NULL \
                   AND EXISTS ( \
                       SELECT 1 FROM categories c WHERE c.id=$1 AND c.user_id=$3 \
                   )",
            )
            .bind(category_id)
            .bind(transaction_id)
            .bind(user_id)
            .execute(&mut *db_tx)
            .await?;
        }

        db_tx.commit().await?;
        Ok(ChargeLinkOutcome::Linked)
    }

    async fn unlink_transaction(
        &self,
        id: Uuid,
        user_id: Uuid,
        reject_transaction: bool,
    ) -> anyhow::Result<bool> {
        let mut db_tx = self.pool.begin().await?;
        let current = sqlx::query_scalar::<_, Option<Uuid>>(
            "SELECT transaction_id FROM subscription_charges \
             WHERE id=$1 AND user_id=$2 FOR UPDATE",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&mut *db_tx)
        .await?;
        let Some(current_transaction_id) = current else {
            db_tx.rollback().await?;
            return Ok(false);
        };
        let Some(transaction_id) = current_transaction_id else {
            db_tx.rollback().await?;
            return Ok(false);
        };

        if reject_transaction {
            sqlx::query(
                "INSERT INTO subscription_charge_match_rejections \
                 (id, charge_id, transaction_id, user_id, reason, created_at) \
                 VALUES ($1,$2,$3,$4,'manual_unlink',$5) \
                 ON CONFLICT (charge_id, transaction_id) DO UPDATE SET \
                   reason=EXCLUDED.reason, created_at=EXCLUDED.created_at",
            )
            .bind(Uuid::new_v4())
            .bind(id)
            .bind(transaction_id)
            .bind(user_id)
            .bind(Utc::now().timestamp())
            .execute(&mut *db_tx)
            .await?;
        }

        sqlx::query(
            "UPDATE subscription_charges SET \
               transaction_id=NULL, match_status='Pending', match_source=NULL, \
               match_started_at=$1 \
             WHERE id=$2 AND user_id=$3",
        )
        .bind(Utc::now().timestamp())
        .bind(id)
        .bind(user_id)
        .execute(&mut *db_tx)
        .await?;

        db_tx.commit().await?;
        Ok(true)
    }

    async fn mark_pending_older_than_unmatched(
        &self,
        user_id: Uuid,
        threshold: DateTime<Utc>,
    ) -> anyhow::Result<u64> {
        let result = sqlx::query(
            "UPDATE subscription_charges \
             SET match_status='Unmatched' \
             WHERE user_id=$1 AND match_status='Pending' AND match_started_at < $2",
        )
        .bind(user_id)
        .bind(threshold.timestamp())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::account::{Account, AccountDetails, AccountRepository, AccountType};
    use crate::domain::subscription::{
        BillingPeriod, Subscription, SubscriptionProvider, SubscriptionRepository,
        SubscriptionStatus,
    };
    use crate::domain::transaction::{
        Transaction, TransactionDetails, TransactionKind, TransactionRepository,
    };
    use crate::infrastructure::account_repository::SqliteAccountRepository;
    use crate::infrastructure::subscription_repository::PgSubscriptionRepository;
    use crate::infrastructure::test_db;
    use crate::infrastructure::transaction_repository::SqliteTransactionRepository;
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
            overrides: Default::default(),
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
        let source_key = format!("gmail:test:{user_id}:{msg}");
        SubscriptionCharge {
            id: Uuid::new_v4(),
            subscription_id: sub_id,
            user_id,
            amount: dec!(15.99),
            currency: "USD".to_string(),
            charged_at: Utc::now(),
            email_message_id: source_key.clone(),
            rfc_message_id: Some(format!("<{msg}@example.test>")),
            source: ChargeSource::Gmail,
            source_key,
            source_connection_id: None,
            provider_message_id: Some(msg.to_string()),
            kind: ReceiptKind::Renewal,
            transaction_id: None,
            match_status: ChargeMatchStatus::Pending,
            match_started_at: Utc::now(),
            match_source: None,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn create_idempotent_second_call_returns_false() {
        let pool = test_db::fresh_pool().await;
        let user_id = Uuid::new_v4();
        let sub_id = make_subscription(&pool, user_id).await;
        let repo = PgSubscriptionChargeRepository::new(pool.clone());
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
        let repo = PgSubscriptionChargeRepository::new(pool.clone());
        let c1 = charge(user_id, sub_id, "msg-1");
        let c2 = charge(user_id, sub_id, "msg-2");
        repo.create_idempotent(&c1).await.unwrap();
        repo.create_idempotent(&c2).await.unwrap();
        sqlx::query("UPDATE subscription_charges SET match_status='Unmatched' WHERE id=$1")
            .bind(c2.id)
            .execute(&pool)
            .await
            .unwrap();
        let pending = repo.list_pending_for_user(user_id).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, c1.id);
    }

    #[tokio::test]
    async fn concurrent_charges_cannot_reserve_the_same_transaction() {
        let pool = test_db::fresh_pool().await;
        let user_id = Uuid::new_v4();
        let subscription_id = make_subscription(&pool, user_id).await;
        let first = charge(user_id, subscription_id, "concurrent-1");
        let second = charge(user_id, subscription_id, "concurrent-2");
        let repo = PgSubscriptionChargeRepository::new(pool.clone());
        repo.create_idempotent(&first).await.unwrap();
        repo.create_idempotent(&second).await.unwrap();

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
            dec!(15.99),
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
        let transaction_id = transaction.id;
        let first_charge_id = first.id;
        let second_charge_id = second.id;

        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));
        let first_task = {
            let pool = pool.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                PgSubscriptionChargeRepository::new(pool)
                    .link_transaction(
                        first_charge_id,
                        user_id,
                        transaction_id,
                        ChargeMatchSource::Manual,
                    )
                    .await
                    .unwrap()
            })
        };
        let second_task = {
            let pool = pool.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                PgSubscriptionChargeRepository::new(pool)
                    .link_transaction(
                        second_charge_id,
                        user_id,
                        transaction_id,
                        ChargeMatchSource::Manual,
                    )
                    .await
                    .unwrap()
            })
        };
        barrier.wait().await;
        let outcomes = [first_task.await.unwrap(), second_task.await.unwrap()];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == ChargeLinkOutcome::Linked)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == ChargeLinkOutcome::TransactionAlreadyLinked)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn pending_charge_aging_is_scoped_to_one_user() {
        let pool = test_db::fresh_pool().await;
        let first_user = Uuid::new_v4();
        let second_user = Uuid::new_v4();
        let first_subscription = make_subscription(&pool, first_user).await;
        let second_subscription = make_subscription(&pool, second_user).await;
        let first = charge(first_user, first_subscription, "aging-first");
        let second = charge(second_user, second_subscription, "aging-second");
        let repo = PgSubscriptionChargeRepository::new(pool.clone());
        repo.create_idempotent(&first).await.unwrap();
        repo.create_idempotent(&second).await.unwrap();
        let old = (Utc::now() - chrono::Duration::days(8)).timestamp();
        sqlx::query("UPDATE subscription_charges SET match_started_at=$1 WHERE id=$2 OR id=$3")
            .bind(old)
            .bind(first.id)
            .bind(second.id)
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            repo.mark_pending_older_than_unmatched(
                first_user,
                Utc::now() - chrono::Duration::days(7),
            )
            .await
            .unwrap(),
            1
        );
        assert_eq!(
            repo.find_by_id(first.id, first_user)
                .await
                .unwrap()
                .unwrap()
                .match_status,
            ChargeMatchStatus::Unmatched
        );
        assert_eq!(
            repo.find_by_id(second.id, second_user)
                .await
                .unwrap()
                .unwrap()
                .match_status,
            ChargeMatchStatus::Pending
        );
    }
}
