use chrono::{TimeZone, Utc};
use moneykeeper::contexts::ledger::public::{JournalEntryId, LedgerAccountId};
use moneykeeper::contexts::loans::public::*;
use moneykeeper::shared_kernel::{CorrelationId, CurrencyCode, Money, UserId};
use rust_decimal_macros::dec;

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap()
}
fn currency() -> CurrencyCode {
    CurrencyCode::new("UAH").unwrap()
}
fn money(value: rust_decimal::Decimal) -> Money {
    Money::new(value, currency(), 2).unwrap()
}
fn terms(principal: rust_decimal::Decimal) -> LoanTerms {
    LoanTerms::new(
        Counterparty::new("Alex").unwrap(),
        money(principal),
        now().date_naive(),
        Some(now().date_naive() + chrono::Days::new(365)),
        Some(AnnualRate::new(dec!(12.5)).unwrap()),
    )
    .unwrap()
}
fn components(principal: rust_decimal::Decimal) -> MovementComponents {
    MovementComponents {
        principal,
        accrued_interest: dec!(0),
        accrued_fee: dec!(0),
        current_interest: dec!(0),
        current_fee: dec!(0),
    }
}

#[test]
fn borrowed_and_lent_agreements_map_to_distinct_contractual_directions() {
    let user = UserId::generate();
    let borrowed =
        LoanAgreement::open(user, LoanDirection::Borrowed, terms(dec!(1000)), now()).unwrap();
    let lent = LoanAgreement::open(user, LoanDirection::Lent, terms(dec!(1000)), now()).unwrap();
    assert_eq!(borrowed.direction(), LoanDirection::Borrowed);
    assert_eq!(lent.direction(), LoanDirection::Lent);
    assert_eq!(borrowed.status(), LoanStatus::PendingAccounting);
    assert_eq!(borrowed.version(), 1);
}

#[test]
fn confirmed_components_change_only_after_ledger_posts() {
    let user = UserId::generate();
    let mut loan =
        LoanAgreement::open(user, LoanDirection::Borrowed, terms(dec!(1000)), now()).unwrap();
    loan.link_principal_account(LedgerAccountId::generate(), 1, now())
        .unwrap();
    let mut movement = LoanMovement::request(
        user,
        MovementKind::Disbursement,
        money(dec!(400)),
        components(dec!(400)),
        Some(LedgerAccountId::generate()),
        None,
        CorrelationId::generate(),
        None,
        now(),
    )
    .unwrap();
    loan.request_movement(&movement, 2, now()).unwrap();
    assert_eq!(loan.balances().principal, dec!(0));
    loan.confirm_posted(&mut movement, JournalEntryId::generate(), 3, now())
        .unwrap();
    assert_eq!(loan.balances().principal, dec!(400));
    assert_eq!(movement.status(), MovementStatus::Posted);
}

#[test]
fn repayment_keeps_principal_interest_and_fee_separate() {
    let balances = ComponentBalances {
        currency: currency(),
        principal: dec!(500),
        accrued_interest: dec!(25),
        accrued_fee: dec!(5),
        version: 1,
    };
    let repayment = MovementComponents {
        principal: dec!(100),
        accrued_interest: dec!(20),
        accrued_fee: dec!(5),
        current_interest: dec!(3),
        current_fee: dec!(2),
    };
    let next = balances
        .apply(MovementKind::Repayment, &repayment, false)
        .unwrap();
    assert_eq!(next.principal, dec!(400));
    assert_eq!(next.accrued_interest, dec!(5));
    assert_eq!(next.accrued_fee, dec!(0));
}

#[test]
fn reversal_restores_exact_confirmed_components_once() {
    let user = UserId::generate();
    let mut loan =
        LoanAgreement::open(user, LoanDirection::Lent, terms(dec!(1000)), now()).unwrap();
    loan.link_principal_account(LedgerAccountId::generate(), 1, now())
        .unwrap();
    let mut movement = LoanMovement::request(
        user,
        MovementKind::Disbursement,
        money(dec!(250)),
        components(dec!(250)),
        Some(LedgerAccountId::generate()),
        None,
        CorrelationId::generate(),
        None,
        now(),
    )
    .unwrap();
    loan.request_movement(&movement, 2, now()).unwrap();
    loan.confirm_posted(&mut movement, JournalEntryId::generate(), 3, now())
        .unwrap();
    loan.confirm_reversed(&mut movement, JournalEntryId::generate(), 4, now())
        .unwrap();
    assert_eq!(loan.balances().principal, dec!(0));
    assert_eq!(movement.status(), MovementStatus::Reversed);
    assert_eq!(
        loan.confirm_reversed(&mut movement, JournalEntryId::generate(), 5, now()),
        Err(LoanError::AlreadyReversed)
    );
}

#[test]
fn closure_requires_zero_balances_and_no_pending_accounting() {
    let user = UserId::generate();
    let mut loan =
        LoanAgreement::open(user, LoanDirection::Borrowed, terms(dec!(100)), now()).unwrap();
    loan.link_principal_account(LedgerAccountId::generate(), 1, now())
        .unwrap();
    let mut disbursement = LoanMovement::request(
        user,
        MovementKind::Disbursement,
        money(dec!(100)),
        components(dec!(100)),
        Some(LedgerAccountId::generate()),
        None,
        CorrelationId::generate(),
        None,
        now(),
    )
    .unwrap();
    loan.request_movement(&disbursement, 2, now()).unwrap();
    loan.confirm_posted(&mut disbursement, JournalEntryId::generate(), 3, now())
        .unwrap();
    assert_eq!(loan.close(4, now()), Err(LoanError::OutstandingBalance));
    let mut repayment = LoanMovement::request(
        user,
        MovementKind::Repayment,
        money(dec!(100)),
        components(dec!(100)),
        Some(LedgerAccountId::generate()),
        None,
        CorrelationId::generate(),
        None,
        now(),
    )
    .unwrap();
    loan.request_movement(&repayment, 4, now()).unwrap();
    loan.confirm_posted(&mut repayment, JournalEntryId::generate(), 5, now())
        .unwrap();
    loan.close(6, now()).unwrap();
    assert_eq!(loan.status(), LoanStatus::Closed);
}
