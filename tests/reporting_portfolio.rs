mod v2_test_support;
use chrono::Utc;
use moneykeeper::{
    bootstrap::v2,
    contexts::portfolio::public::*,
    shared_kernel::{CorrelationId, EventId, UserId},
};
use rust_decimal_macros::dec;
fn event(user: UserId, sequence: u64, fact: PortfolioEventFactV1) -> PortfolioEventV1 {
    let now = Utc::now();
    PortfolioEventV1 {
        metadata: PortfolioEventMetadataV1 {
            schema_version: 1,
            event_id: EventId::generate(),
            user_id: user,
            sequence,
            correlation_id: CorrelationId::generate(),
            occurred_at: now,
            recorded_at: now,
        },
        fact,
    }
}
#[tokio::test]
async fn reporting_consumes_portfolio_value_once_and_rebuilds_exactly() {
    let (verified, _) = v2_test_support::fresh_v2_runtime().await;
    let reporting = v2::supporting_contexts(&verified).reporting;
    let user = UserId::generate();
    let account = PortfolioAccountId::generate();
    let instrument = InstrumentId::generate();
    let position = event(
        user,
        1,
        PortfolioEventFactV1::PositionChanged {
            account_id: account,
            instrument_id: instrument,
            quantity: dec!(2),
            known_cost_quantity: dec!(2),
            unknown_cost_quantity: dec!(0),
            remaining_known_cost: dec!(1900),
            realized_gain_loss: Some(dec!(0)),
            currency: "UAH".into(),
            position_version: 1,
        },
    );
    let valuation = event(
        user,
        2,
        PortfolioEventFactV1::ValuationRecorded {
            snapshot_id: ValuationSnapshotId::generate(),
            account_id: account,
            instrument_id: instrument,
            quantity: dec!(2),
            price_per_instrument: dec!(1010),
            accrued_interest_per_instrument: dec!(15),
            market_value: dec!(2050),
            currency: "UAH".into(),
            quoted_at: Utc::now(),
            source: "manual".into(),
        },
    );
    assert!(
        reporting
            .apply_portfolio_event(position.clone())
            .await
            .unwrap()
            .applied
    );
    assert!(
        reporting
            .apply_portfolio_event(valuation.clone())
            .await
            .unwrap()
            .applied
    );
    assert!(
        !reporting
            .apply_portfolio_event(valuation.clone())
            .await
            .unwrap()
            .applied
    );
    let live = reporting.portfolio_summary(user).await.unwrap();
    assert_eq!(live[0].market_value, Some(dec!(2050)));
    reporting
        .rebuild_portfolio(vec![position, valuation])
        .await
        .unwrap();
    assert_eq!(reporting.portfolio_summary(user).await.unwrap(), live);
}
#[tokio::test]
async fn missing_cost_or_price_is_explicitly_incomplete() {
    let (verified, _) = v2_test_support::fresh_v2_runtime().await;
    let reporting = v2::supporting_contexts(&verified).reporting;
    let user = UserId::generate();
    reporting
        .apply_portfolio_event(event(
            user,
            1,
            PortfolioEventFactV1::PositionChanged {
                account_id: PortfolioAccountId::generate(),
                instrument_id: InstrumentId::generate(),
                quantity: dec!(1),
                known_cost_quantity: dec!(0),
                unknown_cost_quantity: dec!(1),
                remaining_known_cost: dec!(0),
                realized_gain_loss: None,
                currency: "UAH".into(),
                position_version: 1,
            },
        ))
        .await
        .unwrap();
    let row = reporting
        .portfolio_summary(user)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(row.incomplete);
    assert!(row.market_value.is_none());
}
