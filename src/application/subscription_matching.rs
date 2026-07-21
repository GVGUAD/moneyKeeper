use std::sync::Arc;
use std::{cmp::Ordering, collections::HashMap};

use chrono::{Duration, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::domain::account::AccountRepository;
use crate::domain::fx_rate::FxRateRepository;
use crate::domain::subscription::SubscriptionRepository;
use crate::domain::subscription_charge::{
    ChargeLinkOutcome, ChargeMatchSource, ReceiptKind, SubscriptionCharge,
    SubscriptionChargeRepository,
};
use crate::domain::transaction::TransactionRepository;

const TIME_WINDOW_DAYS: i64 = 3;
const UNMATCHED_AFTER_DAYS: i64 = 7;

pub struct MatchChargesUseCase {
    charges: Arc<dyn SubscriptionChargeRepository>,
    #[allow(dead_code)]
    subscriptions: Arc<dyn SubscriptionRepository>,
    transactions: Arc<dyn TransactionRepository>,
    #[allow(dead_code)]
    accounts: Arc<dyn AccountRepository>,
    fx: Arc<dyn FxRateRepository>,
}

impl MatchChargesUseCase {
    pub fn new(
        charges: Arc<dyn SubscriptionChargeRepository>,
        subscriptions: Arc<dyn SubscriptionRepository>,
        transactions: Arc<dyn TransactionRepository>,
        accounts: Arc<dyn AccountRepository>,
        fx: Arc<dyn FxRateRepository>,
    ) -> Self {
        Self {
            charges,
            subscriptions,
            transactions,
            accounts,
            fx,
        }
    }

    pub async fn run_for_user(&self, user_id: Uuid) -> anyhow::Result<()> {
        let pending = self.charges.list_pending_for_user(user_id).await?;
        let mut rate_cache: HashMap<(chrono::NaiveDate, String, String), Option<Decimal>> =
            HashMap::new();
        for charge in pending {
            self.try_match_one(&charge, &mut rate_cache).await?;
        }
        let threshold = Utc::now() - Duration::days(UNMATCHED_AFTER_DAYS);
        self.charges
            .mark_pending_older_than_unmatched(user_id, threshold)
            .await?;
        Ok(())
    }

    async fn try_match_one(
        &self,
        charge: &SubscriptionCharge,
        rate_cache: &mut HashMap<(chrono::NaiveDate, String, String), Option<Decimal>>,
    ) -> anyhow::Result<()> {
        if matches!(&charge.kind, ReceiptKind::Refund) || charge.amount <= Decimal::ZERO {
            return Ok(());
        }

        let from = charge.charged_at - Duration::days(TIME_WINDOW_DAYS);
        let to = charge.charged_at + Duration::days(TIME_WINDOW_DAYS);
        let candidates = self
            .transactions
            .list_unlinked_expense_candidates(charge.id, charge.user_id, from, to)
            .await?;

        let expected_source_amount = charge.amount.abs();
        let mut scored = Vec::new();
        for candidate in &candidates {
            let source = charge.currency.to_uppercase();
            let destination = candidate.currency.to_uppercase();
            let rate_date = charge.charged_at.date_naive();
            let cache_key = (rate_date, source, destination);
            let rate = if let Some(rate) = rate_cache.get(&cache_key) {
                *rate
            } else {
                let rate = self
                    .fx
                    .rate_as_of(rate_date, &charge.currency, &candidate.currency)
                    .await?;
                rate_cache.insert(cache_key, rate);
                rate
            };
            let Some(rate) = rate else {
                continue;
            };
            let expected = expected_source_amount * rate;
            if expected <= Decimal::ZERO {
                continue;
            }
            if let Some(score) = candidate_score(
                expected,
                candidate.amount,
                charge.charged_at,
                candidate.transacted_at,
            ) {
                scored.push((candidate, score));
            }
        }

        scored.sort_by(|(a_tx, a_score), (b_tx, b_score)| {
            a_score
                .partial_cmp(b_score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a_tx.id.cmp(&b_tx.id))
        });

        let Some((best, best_score)) = scored.first() else {
            return Ok(());
        };
        if !has_confident_margin(*best_score, scored.get(1).map(|(_, score)| *score)) {
            return Ok(());
        }

        let outcome = self
            .charges
            .link_transaction(
                charge.id,
                charge.user_id,
                best.id,
                ChargeMatchSource::Automatic,
            )
            .await?;
        match outcome {
            ChargeLinkOutcome::Linked
            | ChargeLinkOutcome::ChargeNotFound
            | ChargeLinkOutcome::ChargeNotPending
            | ChargeLinkOutcome::ChargeAlreadyLinked
            | ChargeLinkOutcome::TransactionNotFound
            | ChargeLinkOutcome::TransactionNotExpense
            | ChargeLinkOutcome::TransactionAlreadyLinked => {}
        }
        Ok(())
    }
}

fn candidate_score(
    expected: Decimal,
    actual: Decimal,
    charged_at: chrono::DateTime<Utc>,
    transacted_at: chrono::DateTime<Utc>,
) -> Option<Decimal> {
    if expected <= Decimal::ZERO {
        return None;
    }
    let amount_error = (actual.abs() - expected).abs() / expected;
    if amount_error > Decimal::new(5, 2) {
        return None;
    }
    let seconds = (transacted_at - charged_at).num_seconds().unsigned_abs();
    let time_error = Decimal::from(seconds) / Decimal::from(86_400_u64);
    Some(amount_error + time_error)
}

fn has_confident_margin(best: Decimal, second: Option<Decimal>) -> bool {
    second.is_none_or(|second| second - best >= Decimal::new(10, 2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::account::{Account, AccountDetails, AccountRepository, AccountType};
    use crate::domain::subscription::{
        BillingPeriod, Subscription, SubscriptionProvider, SubscriptionStatus,
    };
    use crate::domain::subscription_charge::{ChargeMatchStatus, ChargeSource, ReceiptKind};
    use crate::domain::transaction::{Transaction, TransactionDetails, TransactionKind};
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
            overrides: Default::default(),
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn matches_within_amount_and_time_window() {
        let (_pool, user_id, account_id, uc) = setup().await;
        let s = sub(user_id);
        uc.subscriptions.upsert_by_merchant_key(&s).await.unwrap();

        let charge_time = Utc::now();
        let source_key = format!("gmail:test:{user_id}:msg-1");
        let charge = SubscriptionCharge {
            id: Uuid::new_v4(),
            subscription_id: s.id,
            user_id,
            amount: dec!(15.99),
            currency: "USD".into(),
            charged_at: charge_time,
            email_message_id: source_key.clone(),
            rfc_message_id: Some("<msg-1@example.test>".into()),
            source: ChargeSource::Gmail,
            source_key,
            source_connection_id: None,
            provider_message_id: Some("msg-1".into()),
            kind: ReceiptKind::Renewal,
            transaction_id: None,
            match_status: ChargeMatchStatus::Pending,
            match_started_at: Utc::now(),
            match_source: None,
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

        let source_key = format!("gmail:test:{user_id}:msg-2");
        let charge = SubscriptionCharge {
            id: Uuid::new_v4(),
            subscription_id: s.id,
            user_id,
            amount: dec!(15.99),
            currency: "USD".into(),
            charged_at: Utc::now(),
            email_message_id: source_key.clone(),
            rfc_message_id: Some("<msg-2@example.test>".into()),
            source: ChargeSource::Gmail,
            source_key,
            source_connection_id: None,
            provider_message_id: Some("msg-2".into()),
            kind: ReceiptKind::Renewal,
            transaction_id: None,
            match_status: ChargeMatchStatus::Pending,
            match_started_at: Utc::now(),
            match_source: None,
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

    #[test]
    fn matching_boundaries_are_inclusive() {
        let charged_at = Utc::now();
        assert_eq!(
            candidate_score(
                dec!(100),
                dec!(105),
                charged_at,
                charged_at + Duration::days(3)
            ),
            Some(dec!(3.05))
        );
        assert!(candidate_score(dec!(100), dec!(105.0001), charged_at, charged_at).is_none());
    }

    #[test]
    fn confidence_margin_accepts_exact_point_one_only() {
        assert!(!has_confident_margin(dec!(0), Some(dec!(0.0999))));
        assert!(has_confident_margin(dec!(0), Some(dec!(0.10))));
        assert!(has_confident_margin(dec!(0), None));
    }
}
