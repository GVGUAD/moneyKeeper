use chrono::Utc;
use moneykeeper::{
    contexts::{ledger::public::*, portfolio::public::*},
    shared_kernel::{CorrelationId, CurrencyCode, UserId},
};
use rust_decimal_macros::dec;
mod test_support;

#[tokio::test]
async fn durable_worker_posts_and_reverses_one_correlated_ledger_effect() {
    let (verified, pool) = test_support::fresh_runtime().await;
    let contexts = moneykeeper::bootstrap::build_contexts(&verified);
    let user = UserId::generate();
    let now = Utc::now();
    let currency = CurrencyCode::new("UAH").unwrap();
    let cash = contexts
        .ledger
        .open_account(OpenAccount {
            user_id: user,
            name: "Cash".into(),
            currency: currency.clone(),
            kind: AccountKind::Cash,
            nature: AccountNature::Asset,
            opening_balance: moneykeeper::shared_kernel::Money::new(
                dec!(5000),
                currency.clone(),
                2,
            )
            .unwrap(),
            idempotency_key: moneykeeper::shared_kernel::IdempotencyKey::new(
                "portfolio-cash-account",
            )
            .unwrap(),
            correlation_id: CorrelationId::generate(),
            causation_id: None,
            occurred_at: now,
        })
        .await
        .unwrap();
    let instrument = contexts
        .portfolio
        .create_manual_ovdp(CreateManualOvdpInstrument {
            user_id: user,
            identifier: InstrumentIdentifier::new(IdentifierKind::Manual, "CASH-OVDP").unwrap(),
            display_name: "Cash ОВДП".into(),
            currency: currency.clone(),
            face_value: dec!(1000),
            issue_date: now.date_naive(),
            maturity_date: now.date_naive() + chrono::Days::new(365),
            coupon_terms: CouponTerms::ZeroCoupon,
            idempotency_key: moneykeeper::shared_kernel::IdempotencyKey::new("cash-instrument")
                .unwrap(),
            correlation_id: CorrelationId::generate(),
            occurred_at: now,
        })
        .await
        .unwrap();
    let account = contexts
        .portfolio
        .open_account(OpenPortfolioAccount {
            user_id: user,
            name: "Treasury".into(),
            idempotency_key: moneykeeper::shared_kernel::IdempotencyKey::new(
                "cash-portfolio-account",
            )
            .unwrap(),
            correlation_id: CorrelationId::generate(),
            occurred_at: now,
        })
        .await
        .unwrap();
    let account_id = PortfolioAccountId::new(account.aggregate_id);
    let instrument_id = InstrumentId::new(instrument.aggregate_id);
    let purchase = contexts
        .portfolio
        .record_transaction(RecordPortfolioTransaction {
            user_id: user,
            account_id,
            instrument_id,
            expected_account_version: 1,
            expected_position_version: 0,
            activity: PortfolioActivityCommand::Buy {
                quantity: dec!(1),
                total_acquisition_cost: dec!(1000),
                fee: None,
                accrued_interest: None,
                trade_at: now,
            },
            cash_settlement: Some(OptionalCashSettlement {
                cash_account_id: cash.account.id,
                amount: dec!(1000),
            }),
            actor_id: PortfolioActorId::generate(),
            idempotency_key: moneykeeper::shared_kernel::IdempotencyKey::new("cash-buy").unwrap(),
            correlation_id: CorrelationId::generate(),
            recorded_at: now,
        })
        .await
        .unwrap();
    let workers = moneykeeper::bootstrap::portfolio_settlement_runner(&verified);
    assert!(workers.run_once().await.unwrap().records == 1);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM portfolio.cash_settlement_processes WHERE transaction_id=$1"
        )
        .bind(purchase.transaction_id.unwrap().into_uuid())
        .fetch_one(&pool)
        .await
        .unwrap(),
        "posted"
    );
    contexts
        .portfolio
        .reverse_transaction(ReversePortfolioTransaction {
            user_id: user,
            transaction_id: purchase.transaction_id.unwrap(),
            expected_account_version: 1,
            expected_position_version: 1,
            reason: "Cancelled purchase".into(),
            actor_id: PortfolioActorId::generate(),
            idempotency_key: moneykeeper::shared_kernel::IdempotencyKey::new("cash-buy-reversal")
                .unwrap(),
            correlation_id: CorrelationId::generate(),
            recorded_at: now,
        })
        .await
        .unwrap();
    workers.run_once().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM portfolio.cash_settlement_processes WHERE transaction_id=$1"
        )
        .bind(purchase.transaction_id.unwrap().into_uuid())
        .fetch_one(&pool)
        .await
        .unwrap(),
        "reversed"
    );
}
