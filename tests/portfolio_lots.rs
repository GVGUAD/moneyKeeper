use chrono::{TimeZone, Utc};
use moneykeeper::{
    contexts::portfolio::public::*,
    shared_kernel::{CurrencyCode, UserId},
};
use rust_decimal_macros::dec;

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap()
}
fn lot(q: rust_decimal::Decimal, cost: LotCost, sequence: u64) -> PositionLot {
    PositionLot::new(
        UserId::from_uuid(uuid::Uuid::nil()),
        PortfolioAccountId::from_uuid(uuid::Uuid::nil()),
        InstrumentId::from_uuid(uuid::Uuid::nil()),
        PortfolioTransactionId::generate(),
        q,
        cost,
        CurrencyCode::new("UAH").unwrap(),
        now() + chrono::Duration::days(sequence as i64),
        sequence,
    )
    .unwrap()
}

#[test]
fn fifo_is_deterministic_and_conserves_known_cost() {
    let lots = vec![
        lot(dec!(2), LotCost::Known(dec!(1900)), 1),
        lot(dec!(3), LotCost::Known(dec!(3000)), 2),
    ];
    let result = allocate_fifo(&lots, dec!(4)).unwrap();
    assert_eq!(result.allocations[0].lot_id, lots[0].id);
    assert_eq!(result.allocated_known_cost, dec!(3900));
    assert_eq!(
        result
            .lots
            .iter()
            .map(|l| match l.remaining_cost {
                LotCost::Known(v) => v,
                LotCost::Unknown => dec!(0),
            })
            .sum::<rust_decimal::Decimal>(),
        dec!(1000)
    );
}

#[test]
fn unknown_cost_never_fabricates_realized_gain() {
    let result = allocate_fifo(&[lot(dec!(1), LotCost::Unknown, 1)], dec!(1)).unwrap();
    assert_eq!(result.realized_gain_loss(dec!(1000), dec!(5)), None);
}

#[test]
fn reversal_restores_exact_consumed_state() {
    let original = lot(dec!(3), LotCost::Known(dec!(1000)), 1);
    let result = allocate_fifo(std::slice::from_ref(&original), dec!(1)).unwrap();
    let restored = restore_allocations(&result.lots, &result.allocations).unwrap();
    assert_eq!(restored[0].remaining_quantity, original.remaining_quantity);
    assert_eq!(restored[0].remaining_cost, original.remaining_cost);
}
