mod v2_test_support;
use chrono::{Duration, Utc};
use moneykeeper::{
    bootstrap::v2,
    contexts::portfolio::public::*,
    shared_kernel::{CorrelationId, CurrencyCode, IdempotencyKey, UserId},
};
use rust_decimal_macros::dec;

#[tokio::test]
async fn schema_installs_tenant_safe_immutable_portfolio_storage() {
    let (_verified, pool) = v2_test_support::fresh_v2_runtime().await;
    for table in [
        "instruments",
        "accounts",
        "command_receipts",
        "transactions",
        "transaction_components",
        "position_lots",
        "lot_allocations",
        "position_projection",
        "valuation_snapshots",
        "latest_valuation_projection",
        "cash_settlement_processes",
        "audit_log",
    ] {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(format!("portfolio.{table}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(exists, "missing portfolio.{table}");
    }
    let user = uuid::Uuid::new_v4();
    let other = uuid::Uuid::new_v4();
    let now = Utc::now();
    let account = uuid::Uuid::new_v4();
    let instrument = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO portfolio.accounts(id,user_id,name,lifecycle,version,created_at,updated_at) VALUES($1,$2,'Main','active',1,$3,$3)").bind(account).bind(user).bind(now).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO portfolio.instruments(id,user_id,identifier_kind,identifier,instrument_type,issuer_type,display_name,currency,face_value,issue_date,maturity_date,coupon_kind,source,version,created_at,updated_at) VALUES($1,$2,'manual','bond-1','ovdp','sovereign_bond','Bond','UAH',1000,CURRENT_DATE,CURRENT_DATE+1,'unknown','manual',1,$3,$3)").bind(instrument).bind(user).bind(now).execute(&pool).await.unwrap();
    assert!(sqlx::query("INSERT INTO portfolio.transactions(id,user_id,account_id,instrument_id,sequence,position_version,kind,status,quantity,currency,source,actor_id,correlation_id,effective_at,recorded_at) VALUES($1,$2,$3,$4,1,1,'buy','posted',1,'UAH','manual',$5,$6,$7,$7)").bind(uuid::Uuid::new_v4()).bind(other).bind(account).bind(instrument).bind(other).bind(uuid::Uuid::new_v4()).bind(now).execute(&pool).await.is_err());
    let transaction = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO portfolio.transactions(id,user_id,account_id,instrument_id,sequence,position_version,kind,status,quantity,currency,source,actor_id,correlation_id,effective_at,recorded_at) VALUES($1,$2,$3,$4,1,1,'buy','posted',1,'UAH','manual',$5,$6,$7,$7)").bind(transaction).bind(user).bind(account).bind(instrument).bind(user).bind(uuid::Uuid::new_v4()).bind(now).execute(&pool).await.unwrap();
    assert!(
        sqlx::query("UPDATE portfolio.transactions SET quantity=2 WHERE id=$1")
            .bind(transaction)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM portfolio.transactions WHERE id=$1")
            .bind(transaction)
            .execute(&pool)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn command_is_atomic_idempotent_and_fifo_projection_is_exact() {
    let (verified, _pool) = v2_test_support::fresh_v2_runtime().await;
    let contexts = v2::supporting_contexts(&verified);
    let portfolio = contexts.portfolio;
    let user = UserId::generate();
    let now = Utc::now();
    let instrument = portfolio
        .create_manual_ovdp(CreateManualOvdpInstrument {
            user_id: user,
            identifier: InstrumentIdentifier::new(IdentifierKind::Manual, "UAH-2028-A").unwrap(),
            display_name: "ОВДП 2028".into(),
            currency: CurrencyCode::new("UAH").unwrap(),
            face_value: dec!(1000),
            issue_date: now.date_naive(),
            maturity_date: now.date_naive() + chrono::Days::new(730),
            coupon_terms: CouponTerms::Unknown,
            idempotency_key: IdempotencyKey::new("instrument-1").unwrap(),
            correlation_id: CorrelationId::generate(),
            occurred_at: now,
        })
        .await
        .unwrap();
    let account = portfolio
        .open_account(OpenPortfolioAccount {
            user_id: user,
            name: "Treasury".into(),
            idempotency_key: IdempotencyKey::new("account-1").unwrap(),
            correlation_id: CorrelationId::generate(),
            occurred_at: now,
        })
        .await
        .unwrap();
    let account_id = PortfolioAccountId::new(account.aggregate_id);
    let instrument_id = InstrumentId::new(instrument.aggregate_id);
    for (index, (quantity, cost)) in [(dec!(2), dec!(1900)), (dec!(3), dec!(3000))]
        .into_iter()
        .enumerate()
    {
        let command = RecordPortfolioTransaction {
            user_id: user,
            account_id,
            instrument_id,
            expected_account_version: 1,
            expected_position_version: index as u64,
            activity: PortfolioActivityCommand::Buy {
                quantity,
                total_acquisition_cost: cost,
                fee: None,
                accrued_interest: None,
                trade_at: now + Duration::days(index as i64),
            },
            cash_settlement: None,
            actor_id: PortfolioActorId::generate(),
            idempotency_key: IdempotencyKey::new(format!("buy-{index}")).unwrap(),
            correlation_id: CorrelationId::generate(),
            recorded_at: now,
        };
        let first = portfolio.record_transaction(command.clone()).await.unwrap();
        let replay = portfolio.record_transaction(command).await.unwrap();
        assert_eq!(first.transaction_id, replay.transaction_id);
        assert!(replay.replayed);
    }
    let sale = portfolio
        .record_transaction(RecordPortfolioTransaction {
            user_id: user,
            account_id,
            instrument_id,
            expected_account_version: 1,
            expected_position_version: 2,
            activity: PortfolioActivityCommand::Sell {
                quantity: dec!(4),
                proceeds: dec!(4200),
                fee: Some(dec!(20)),
                trade_at: now + Duration::days(3),
                lot_allocations: None,
            },
            cash_settlement: None,
            actor_id: PortfolioActorId::generate(),
            idempotency_key: IdempotencyKey::new("sell-1").unwrap(),
            correlation_id: CorrelationId::generate(),
            recorded_at: now,
        })
        .await
        .unwrap();
    let position = portfolio
        .positions(user, account_id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(position.quantity, dec!(1));
    assert_eq!(position.remaining_known_cost, dec!(1000));
    assert_eq!(position.realized_gain_loss, Some(dec!(280)));
    portfolio
        .reverse_transaction(ReversePortfolioTransaction {
            user_id: user,
            transaction_id: sale.transaction_id.unwrap(),
            expected_account_version: 1,
            expected_position_version: 3,
            reason: "Sale entered twice".into(),
            actor_id: PortfolioActorId::generate(),
            idempotency_key: IdempotencyKey::new("reverse-sale-1").unwrap(),
            correlation_id: CorrelationId::generate(),
            recorded_at: now,
        })
        .await
        .unwrap();
    let restored = portfolio
        .positions(user, account_id)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(restored.quantity, dec!(5));
    assert_eq!(restored.remaining_known_cost, dec!(4900));
    assert_eq!(restored.realized_gain_loss, Some(dec!(0)));
}

#[tokio::test]
async fn valuation_is_append_only_and_never_creates_cash_process() {
    let (verified, pool) = v2_test_support::fresh_v2_runtime().await;
    let portfolio = v2::supporting_contexts(&verified).portfolio;
    let user = UserId::generate();
    let now = Utc::now();
    let instrument = portfolio
        .create_manual_ovdp(CreateManualOvdpInstrument {
            user_id: user,
            identifier: InstrumentIdentifier::new(IdentifierKind::Manual, "VAL-1").unwrap(),
            display_name: "Valued".into(),
            currency: CurrencyCode::new("UAH").unwrap(),
            face_value: dec!(1000),
            issue_date: now.date_naive(),
            maturity_date: now.date_naive() + chrono::Days::new(1),
            coupon_terms: CouponTerms::ZeroCoupon,
            idempotency_key: IdempotencyKey::new("vi").unwrap(),
            correlation_id: CorrelationId::generate(),
            occurred_at: now,
        })
        .await
        .unwrap();
    let account = portfolio
        .open_account(OpenPortfolioAccount {
            user_id: user,
            name: "A".into(),
            idempotency_key: IdempotencyKey::new("va").unwrap(),
            correlation_id: CorrelationId::generate(),
            occurred_at: now,
        })
        .await
        .unwrap();
    let account = PortfolioAccountId::new(account.aggregate_id);
    let instrument = InstrumentId::new(instrument.aggregate_id);
    portfolio
        .record_transaction(RecordPortfolioTransaction {
            user_id: user,
            account_id: account,
            instrument_id: instrument,
            expected_account_version: 1,
            expected_position_version: 0,
            activity: PortfolioActivityCommand::OpeningPosition {
                quantity: dec!(2),
                acquisition_cost: None,
                acquisition_date: now.date_naive(),
                reason: "Opening".into(),
            },
            cash_settlement: None,
            actor_id: PortfolioActorId::generate(),
            idempotency_key: IdempotencyKey::new("vp").unwrap(),
            correlation_id: CorrelationId::generate(),
            recorded_at: now,
        })
        .await
        .unwrap();
    let result = portfolio
        .record_valuation(RecordValuationSnapshot {
            user_id: user,
            account_id: account,
            instrument_id: instrument,
            price_per_instrument: dec!(1010),
            accrued_interest_per_instrument: dec!(15),
            currency: CurrencyCode::new("UAH").unwrap(),
            source: "Manual quote".into(),
            quoted_at: now,
            idempotency_key: IdempotencyKey::new("vv").unwrap(),
            correlation_id: CorrelationId::generate(),
            recorded_at: now,
        })
        .await
        .unwrap();
    assert_eq!(
        portfolio.positions(user, account).await.unwrap()[0].latest_market_value,
        Some(dec!(2050))
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM portfolio.cash_settlement_processes")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert!(
        sqlx::query("DELETE FROM portfolio.valuation_snapshots WHERE id=$1")
            .bind(result.aggregate_id)
            .execute(&pool)
            .await
            .is_err()
    );
}
