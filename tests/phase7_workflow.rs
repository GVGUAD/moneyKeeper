mod v2_test_support;

use chrono::{Duration, Utc};
use moneykeeper::{
    bootstrap::v2,
    contexts::portfolio::public::*,
    shared_kernel::{CorrelationId, CurrencyCode, IdempotencyKey, UserId},
};
use rust_decimal_macros::dec;

#[tokio::test]
async fn ovdp_lifecycle_rebuildable_scenario_is_exact() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let contexts = v2::supporting_contexts(&verified);
    let portfolio = contexts.portfolio.clone();
    let user = UserId::generate();
    let now = Utc::now();
    let instrument = portfolio
        .create_manual_ovdp(CreateManualOvdpInstrument {
            user_id: user,
            identifier: InstrumentIdentifier::new(IdentifierKind::Isin, "UA4000227096").unwrap(),
            display_name: "ОВДП lifecycle".into(),
            currency: CurrencyCode::new("UAH").unwrap(),
            face_value: dec!(1000),
            issue_date: now.date_naive(),
            maturity_date: now.date_naive() + chrono::Days::new(365),
            coupon_terms: CouponTerms::Fixed {
                annual_rate: dec!(16.5),
            },
            idempotency_key: IdempotencyKey::new("e2e-instrument").unwrap(),
            correlation_id: CorrelationId::generate(),
            occurred_at: now,
        })
        .await
        .unwrap();
    let account = portfolio
        .open_account(OpenPortfolioAccount {
            user_id: user,
            name: "E2E Treasury".into(),
            idempotency_key: IdempotencyKey::new("e2e-account").unwrap(),
            correlation_id: CorrelationId::generate(),
            occurred_at: now,
        })
        .await
        .unwrap();
    let account = PortfolioAccountId::new(account.aggregate_id);
    let instrument = InstrumentId::new(instrument.aggregate_id);
    let command = |version: u64, key: &str, activity| RecordPortfolioTransaction {
        user_id: user,
        account_id: account,
        instrument_id: instrument,
        expected_account_version: 1,
        expected_position_version: version,
        activity,
        cash_settlement: None,
        actor_id: PortfolioActorId::generate(),
        idempotency_key: IdempotencyKey::new(key).unwrap(),
        correlation_id: CorrelationId::generate(),
        recorded_at: now,
    };
    portfolio
        .record_transaction(command(
            0,
            "e2e-buy-1",
            PortfolioActivityCommand::Buy {
                quantity: dec!(2),
                total_acquisition_cost: dec!(1900),
                fee: None,
                accrued_interest: None,
                trade_at: now,
            },
        ))
        .await
        .unwrap();
    portfolio
        .record_transaction(command(
            1,
            "e2e-buy-2",
            PortfolioActivityCommand::Buy {
                quantity: dec!(3),
                total_acquisition_cost: dec!(3000),
                fee: None,
                accrued_interest: None,
                trade_at: now + Duration::days(1),
            },
        ))
        .await
        .unwrap();
    portfolio
        .record_transaction(command(
            2,
            "e2e-sell",
            PortfolioActivityCommand::Sell {
                quantity: dec!(2),
                proceeds: dec!(2100),
                fee: Some(dec!(10)),
                trade_at: now + Duration::days(2),
                lot_allocations: None,
            },
        ))
        .await
        .unwrap();
    portfolio
        .record_transaction(command(
            3,
            "e2e-coupon",
            PortfolioActivityCommand::Coupon {
                amount: dec!(80),
                ex_date: Some(now.date_naive()),
                payment_date: now.date_naive() + chrono::Days::new(3),
            },
        ))
        .await
        .unwrap();
    portfolio
        .record_valuation(RecordValuationSnapshot {
            user_id: user,
            account_id: account,
            instrument_id: instrument,
            price_per_instrument: dec!(1010),
            accrued_interest_per_instrument: dec!(15),
            currency: CurrencyCode::new("UAH").unwrap(),
            source: "Manual close".into(),
            quoted_at: now + Duration::days(3),
            idempotency_key: IdempotencyKey::new("e2e-value").unwrap(),
            correlation_id: CorrelationId::generate(),
            recorded_at: now,
        })
        .await
        .unwrap();
    let redemption = portfolio
        .record_transaction(command(
            4,
            "e2e-redemption",
            PortfolioActivityCommand::Redemption {
                quantity: dec!(1),
                proceeds: dec!(1000),
                maturity_date: now.date_naive() + chrono::Days::new(365),
                reference: "Maturity".into(),
                lot_allocations: None,
            },
        ))
        .await
        .unwrap();
    portfolio
        .reverse_transaction(ReversePortfolioTransaction {
            user_id: user,
            transaction_id: redemption.transaction_id.unwrap(),
            expected_account_version: 1,
            expected_position_version: 5,
            reason: "Redemption recorded early".into(),
            actor_id: PortfolioActorId::generate(),
            idempotency_key: IdempotencyKey::new("e2e-redemption-reversal").unwrap(),
            correlation_id: CorrelationId::generate(),
            recorded_at: now,
        })
        .await
        .unwrap();
    let position = portfolio
        .positions(user, account)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(position.quantity, dec!(3));
    assert_eq!(position.remaining_known_cost, dec!(3000));
    assert_eq!(position.latest_market_value, Some(dec!(3075)));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM portfolio.transactions WHERE user_id=$1"
        )
        .bind(user.into_uuid())
        .fetch_one(&pool)
        .await
        .unwrap(),
        6
    );
    let workers = v2::phase4_workers(&verified);
    for _ in 0..32 {
        if !workers.route_event_once().await.unwrap().claimed {
            break;
        }
    }
    let summary = contexts.reporting.portfolio_summary(user).await.unwrap();
    assert_eq!(summary[0].quantity, dec!(3));
    assert_eq!(summary[0].market_value, Some(dec!(3075)));
}
