use chrono::{TimeZone, Utc};
use moneykeeper::{
    contexts::portfolio::public::*,
    shared_kernel::{CorrelationId, CurrencyCode, Money, UserId},
};
use rust_decimal_macros::dec;

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap()
}
fn uah() -> CurrencyCode {
    CurrencyCode::new("UAH").unwrap()
}
fn money(value: rust_decimal::Decimal) -> Money {
    Money::new(value, uah(), 2).unwrap()
}

#[test]
fn manual_ovdp_preserves_explicit_terms() {
    let instrument = Instrument::manual_ovdp(
        UserId::generate(),
        InstrumentIdentifier::new(IdentifierKind::Isin, "UA4000227096").unwrap(),
        "ОВДП 2028",
        uah(),
        dec!(1000),
        now().date_naive(),
        now().date_naive() + chrono::Days::new(730),
        CouponTerms::Unknown,
        now(),
    )
    .unwrap();
    assert_eq!(instrument.instrument_type(), InstrumentType::Ovdp);
    assert_eq!(instrument.version(), 1);
}

#[test]
fn account_lifecycle_is_version_fenced_and_reversal_friendly() {
    let mut account = PortfolioAccount::open(UserId::generate(), "Treasury", now()).unwrap();
    account.archive(1, now()).unwrap();
    assert_eq!(
        account.rename("Nope", 1, now()),
        Err(PortfolioError::VersionConflict)
    );
    assert_eq!(
        account.require_activity_allowed(false),
        Err(PortfolioError::AccountArchived)
    );
    assert!(account.require_activity_allowed(true).is_ok());
}

#[test]
fn ovdp_transactions_require_whole_units_and_reversal_is_append_only() {
    let user = UserId::generate();
    let account = PortfolioAccountId::generate();
    let instrument = InstrumentId::generate();
    assert_eq!(
        PortfolioTransaction::buy(
            user,
            account,
            instrument,
            dec!(1.5),
            money(dec!(990)),
            None,
            None,
            uah(),
            PortfolioActorId::generate(),
            CorrelationId::generate(),
            now(),
            now()
        ),
        Err(PortfolioError::FractionalOvdpQuantity)
    );
    let mut buy = PortfolioTransaction::buy(
        user,
        account,
        instrument,
        dec!(2),
        money(dec!(1980)),
        Some(money(dec!(10))),
        None,
        uah(),
        PortfolioActorId::generate(),
        CorrelationId::generate(),
        now(),
        now(),
    )
    .unwrap();
    let reversal = PortfolioTransaction::reversal_of(
        &mut buy,
        PortfolioActorId::generate(),
        "Mistake",
        CorrelationId::generate(),
        now(),
    )
    .unwrap();
    assert_eq!(reversal.quantity(), dec!(-2));
    assert_eq!(reversal.reversal_of_id(), Some(buy.id()));
    assert_eq!(
        PortfolioTransaction::reversal_of(
            &mut buy,
            PortfolioActorId::generate(),
            "Again",
            CorrelationId::generate(),
            now()
        ),
        Err(PortfolioError::AlreadyReversed)
    );
}
