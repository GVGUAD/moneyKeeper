use chrono::{TimeZone, Utc};
use moneykeeper::{
    contexts::recurring::domain::{
        Allocation, ChargeEvidenceId, ChargeMatching, DecisionSource, MatchingState,
        MatchingVersion, RecurringError,
    },
    shared_kernel::{CurrencyCode, Money},
};
use rust_decimal_macros::dec;
fn money(amount: rust_decimal::Decimal) -> Money {
    Money::new(amount, CurrencyCode::new("UAH").unwrap(), 2).unwrap()
}
#[test]
fn matching_is_allocated_versioned_and_append_only() {
    let now = Utc.with_ymd_and_hms(2026, 8, 20, 12, 0, 0).unwrap();
    let mut matching =
        ChargeMatching::new(ChargeEvidenceId::generate(), money(dec!(100.00))).unwrap();
    let first = Allocation::new(uuid::Uuid::new_v4(), money(dec!(40.00))).unwrap();
    let match_id = matching
        .allocate(
            MatchingVersion::INITIAL,
            vec![first],
            DecisionSource::Manual,
            now,
        )
        .unwrap();
    assert_eq!(matching.state(), MatchingState::PartiallyMatched);
    let stale = matching.allocate(
        MatchingVersion::INITIAL,
        vec![Allocation::new(uuid::Uuid::new_v4(), money(dec!(60.00))).unwrap()],
        DecisionSource::Manual,
        now,
    );
    assert_eq!(stale, Err(RecurringError::VersionConflict));
    matching.unmatch(matching.version(), match_id, now).unwrap();
    assert_eq!(matching.state(), MatchingState::Undecided);
    assert_eq!(matching.pull_events().len(), 2)
}
#[test]
fn matching_rejects_overcommit() {
    let now = Utc::now();
    let mut matching =
        ChargeMatching::new(ChargeEvidenceId::generate(), money(dec!(10.00))).unwrap();
    let result = matching.allocate(
        MatchingVersion::INITIAL,
        vec![Allocation::new(uuid::Uuid::new_v4(), money(dec!(10.01))).unwrap()],
        DecisionSource::Manual,
        now,
    );
    assert_eq!(result, Err(RecurringError::AllocationOvercommit));
}
