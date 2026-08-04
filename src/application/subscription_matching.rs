use std::sync::Arc;

use chrono::{Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use uuid::Uuid;

use crate::domain::account::AccountRepository;
use crate::domain::fx_rate::FxRateRepository;
use crate::domain::subscription::{SubscriptionRepository, SubscriptionStatus};
use crate::domain::subscription_charge::{
    ChargeMatchStatus, SubscriptionCharge, SubscriptionChargeRepository,
};
use crate::domain::transaction::{Transaction, TransactionRepository};

const TIME_WINDOW_DAYS: i64 = 3;
const UNMATCHED_AFTER_DAYS: i64 = 7;

pub struct MatchChargesUseCase {
    pub charges: Arc<dyn SubscriptionChargeRepository>,
    pub subscriptions: Arc<dyn SubscriptionRepository>,
    pub transactions: Arc<dyn TransactionRepository>,
    #[allow(dead_code)]
    pub accounts: Arc<dyn AccountRepository>,
    #[allow(dead_code)]
    pub fx: Arc<dyn FxRateRepository>,
}

impl MatchChargesUseCase {
    pub async fn run_for_user(&self, user_id: Uuid) -> anyhow::Result<()> {
        let pending = self.charges.list_pending_for_user(user_id).await?;
        for charge in pending {
            self.try_match_one(&charge).await?;
        }
        let threshold = Utc::now() - Duration::days(UNMATCHED_AFTER_DAYS);
        self.charges
            .mark_pending_older_than_unmatched(threshold)
            .await?;
        Ok(())
    }

    async fn try_match_one(&self, charge: &SubscriptionCharge) -> anyhow::Result<()> {
        let from = charge.charged_at - Duration::days(TIME_WINDOW_DAYS);
        let to = charge.charged_at + Duration::days(TIME_WINDOW_DAYS);

        let bounds = amount_bounds(charge.amount);
        let candidates = self
            .transactions
            .list_match_candidates(
                charge.user_id,
                from,
                to,
                bounds.0,
                bounds.1,
                &charge.currency,
            )
            .await?;

        let Some(best) = pick_best(charge, &candidates) else {
            return Ok(());
        };

        self.charges
            .update_match(charge.id, Some(best.id), ChargeMatchStatus::Matched)
            .await?;

        if let Some(sub) = self
            .subscriptions
            .find_by_id(charge.subscription_id, charge.user_id)
            .await?
        {
            let next_expected = sub.billing_period.next_after(charge.charged_at);
            self.subscriptions
                .update_after_charge(
                    sub.id,
                    charge.charged_at,
                    next_expected,
                    SubscriptionStatus::Active,
                )
                .await?;

            if let Some(cat) = sub.category_id
                && best.category_id.is_none()
            {
                let mut updated = best.clone();
                updated.category_id = Some(cat);
                self.transactions
                    .update(
                        &updated,
                        &crate::domain::transaction::TransactionDetails::None,
                    )
                    .await?;
            }
        }
        Ok(())
    }
}

fn amount_bounds(amount: Decimal) -> (Decimal, Decimal) {
    let tol = amount * Decimal::new(5, 2); // 0.05
    (amount - tol, amount + tol)
}

fn pick_best<'a>(
    charge: &SubscriptionCharge,
    candidates: &'a [Transaction],
) -> Option<&'a Transaction> {
    candidates.iter().min_by(|a, b| {
        let sa = score(charge, a);
        let sb = score(charge, b);
        sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn score(charge: &SubscriptionCharge, tx: &Transaction) -> f64 {
    let amount_delta_pct = if charge.amount.is_zero() {
        0.0
    } else {
        ((charge.amount - tx.amount) / charge.amount)
            .abs()
            .to_f64()
            .unwrap_or(1.0)
    };
    let time_delta_h = (charge.charged_at - tx.transacted_at).num_hours().abs() as f64;
    amount_delta_pct + time_delta_h / 24.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::account::{Account, AccountDetails, AccountRepository, AccountType};
    use crate::domain::subscription::{
        BillingPeriod, Subscription, SubscriptionProvider, SubscriptionStatus,
    };
    use crate::domain::subscription_charge::ReceiptKind;
    use crate::domain::transaction::{TransactionDetails, TransactionKind};
    use crate::infrastructure::account_repository::SqliteAccountRepository;
    use crate::infrastructure::fx_rate_repository::PgFxRateRepository;
    use crate::infrastructure::subscription_charge_repository::PgSubscriptionChargeRepository;
    use crate::infrastructure::subscription_repository::PgSubscriptionRepository;
    use crate::infrastructure::test_db;
    use crate::infrastructure::transaction_repository::SqliteTransactionRepository;
    use rust_decimal_macros::dec;

    async fn setup() -> (sqlx::PgPool, Uuid, Uuid, MatchChargesUseCase) {
        let pool = test_db::fresh_pool().await;
        let user_id = Uuid::new_v4();

        let accounts: Arc<dyn AccountRepository> =
            Arc::new(SqliteAccountRepository::new(pool.clone()));
        let acc = Account::new(user_id, "Card".into(), AccountType::Cash, "USD".into());
        let account_id = acc.id;
        accounts.create(&acc, &AccountDetails::None).await.unwrap();

        let txs: Arc<dyn TransactionRepository> =
            Arc::new(SqliteTransactionRepository::new(pool.clone()));
        let subs: Arc<dyn SubscriptionRepository> =
            Arc::new(PgSubscriptionRepository::new(pool.clone()));
        let charges: Arc<dyn SubscriptionChargeRepository> =
            Arc::new(PgSubscriptionChargeRepository::new(pool.clone()));
        let fx: Arc<dyn FxRateRepository> = Arc::new(PgFxRateRepository::new(pool.clone()));

        let uc = MatchChargesUseCase {
            charges,
            subscriptions: subs,
            transactions: txs,
            accounts,
            fx,
        };
        (pool, user_id, account_id, uc)
    }

    fn sub(user_id: Uuid) -> Subscription {
        Subscription {
            id: Uuid::new_v4(),
            user_id,
            provider: SubscriptionProvider::Netflix,
            product_name: "Netflix Premium".into(),
            merchant_key: "netflix.com:premium".into(),
            amount: dec!(15.99),
            currency: "USD".into(),
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
    async fn matches_within_amount_and_time_window() {
        let (_pool, user_id, account_id, uc) = setup().await;
        let s = sub(user_id);
        uc.subscriptions.upsert_by_merchant_key(&s).await.unwrap();

        let charge_time = Utc::now();
        let charge = SubscriptionCharge {
            id: Uuid::new_v4(),
            subscription_id: s.id,
            user_id,
            amount: dec!(15.99),
            currency: "USD".into(),
            charged_at: charge_time,
            email_message_id: "msg-1".into(),
            kind: ReceiptKind::Renewal,
            transaction_id: None,
            match_status: ChargeMatchStatus::Pending,
            created_at: Utc::now(),
        };
        uc.charges.create_idempotent(&charge).await.unwrap();

        let tx = Transaction::new(
            account_id,
            user_id,
            dec!(16.00),
            "USD".into(),
            TransactionKind::Expense,
            None,
            None,
            charge_time + Duration::hours(2),
        );
        uc.transactions
            .create(&tx, &TransactionDetails::None)
            .await
            .unwrap();

        uc.run_for_user(user_id).await.unwrap();

        let found = uc
            .charges
            .find_by_id(charge.id, user_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.match_status, ChargeMatchStatus::Matched);
        assert_eq!(found.transaction_id, Some(tx.id));
    }

    #[tokio::test]
    async fn no_match_when_outside_tolerance() {
        let (_pool, user_id, account_id, uc) = setup().await;
        let s = sub(user_id);
        uc.subscriptions.upsert_by_merchant_key(&s).await.unwrap();

        let charge = SubscriptionCharge {
            id: Uuid::new_v4(),
            subscription_id: s.id,
            user_id,
            amount: dec!(15.99),
            currency: "USD".into(),
            charged_at: Utc::now(),
            email_message_id: "msg-2".into(),
            kind: ReceiptKind::Renewal,
            transaction_id: None,
            match_status: ChargeMatchStatus::Pending,
            created_at: Utc::now(),
        };
        uc.charges.create_idempotent(&charge).await.unwrap();

        let tx = Transaction::new(
            account_id,
            user_id,
            dec!(20.00),
            "USD".into(),
            TransactionKind::Expense,
            None,
            None,
            Utc::now(),
        );
        uc.transactions
            .create(&tx, &TransactionDetails::None)
            .await
            .unwrap();

        uc.run_for_user(user_id).await.unwrap();
        let still_pending = uc
            .charges
            .find_by_id(charge.id, user_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(still_pending.match_status, ChargeMatchStatus::Pending);
    }
}
