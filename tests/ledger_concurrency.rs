use std::time::Duration;

use chrono::Utc;
use moneykeeper::contexts::ledger::public::{
    AccountKind, AccountNature, OpenAccount, TransferFunds,
};
use moneykeeper::shared_kernel::{
    CorrelationId, CurrencyCode, IdempotencyKey, Money, UserId,
};
use rust_decimal::Decimal;
use tokio::time::timeout;

#[path = "v2_test_support.rs"]
mod v2_test_support;

#[tokio::test]
async fn opposing_transfers_complete_without_deadlock_or_projection_drift() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let ledger = moneykeeper::contexts::ledger::build(&verified);
    let user = UserId::generate();
    let currency = CurrencyCode::new("UAH").unwrap();
    let open = |name: &str, key: &str| OpenAccount {
        user_id: user, name: name.to_owned(), currency: currency.clone(),
        kind: AccountKind::Cash, nature: AccountNature::Asset,
        opening_balance: Money::new(Decimal::new(10000, 2), currency.clone(), 2).unwrap(),
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        correlation_id: CorrelationId::generate(), causation_id: None, occurred_at: Utc::now(),
    };
    let a = ledger.open_account(open("A", "open-a")).await.unwrap();
    let b = ledger.open_account(open("B", "open-b")).await.unwrap();
    let transfer = |source, target, key: &str| TransferFunds {
        user_id: user, source_account_id: source, target_account_id: target,
        source_amount: Money::new(Decimal::new(1000, 2), currency.clone(), 2).unwrap(),
        target_amount: Money::new(Decimal::new(1000, 2), currency.clone(), 2).unwrap(),
        fee: None, implied_rate: None, description: key.to_owned(),
        idempotency_key: IdempotencyKey::new(key).unwrap(), correlation_id: CorrelationId::generate(),
        causation_id: None, occurred_at: Utc::now(),
    };
    let first = ledger.clone();
    let second = ledger.clone();
    let a_to_b = transfer(a.account.id, b.account.id, "a-to-b");
    let b_to_a = transfer(b.account.id, a.account.id, "b-to-a");
    timeout(Duration::from_secs(10), async move {
        let (left, right) = tokio::join!(first.transfer(a_to_b), second.transfer(b_to_a));
        left.unwrap();
        right.unwrap();
    }).await.expect("opposing transfers deadlocked");

    let drift: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ledger.account_balances b WHERE b.user_id = $1 \
         AND b.signed_balance <> COALESCE((SELECT SUM(p.signed_amount) FROM ledger.postings p \
         WHERE p.user_id = b.user_id AND p.account_id = b.account_id), 0)",
    ).bind(user.into_uuid()).fetch_one(&pool).await.unwrap();
    assert_eq!(drift, 0);
}
