use moneykeeper::{
    contexts::sharing::public::*,
    integration::process_managers::sharing_settlement::*,
    shared_kernel::{CorrelationId, CurrencyCode, Money, UserId},
};
use rust_decimal::Decimal;
fn money(value: i64) -> Money {
    Money::new(Decimal::new(value, 2), CurrencyCode::new("UAH").unwrap(), 2).unwrap()
}

#[test]
fn partial_settlement_rejects_overpayment_and_is_once_only() {
    let bill = BillSplitId::generate();
    let user = UserId::generate();
    let debtor = Participant::Contact(ContactId::generate());
    let remaining = money(1000);
    assert!(matches!(
        Settlement::create(
            SettlementId::generate(),
            bill,
            user,
            debtor,
            Participant::CurrentUser,
            money(1001),
            &remaining,
            SettlementEvidence::External,
            chrono::Utc::now()
        ),
        Err(SharingError::OverSettlement)
    ));
    let mut settlement = Settlement::create(
        SettlementId::generate(),
        bill,
        user,
        debtor,
        Participant::CurrentUser,
        money(500),
        &remaining,
        SettlementEvidence::External,
        chrono::Utc::now(),
    )
    .unwrap();
    settlement.mark_posted(SettlementVersion(1)).unwrap();
    settlement.reverse("mistake", SettlementVersion(2)).unwrap();
    assert!(matches!(
        settlement.reverse("again", SettlementVersion(3)),
        Err(SharingError::AlreadyReversed)
    ));
}

#[test]
fn settlement_process_keys_are_stable() {
    let process =
        SettlementAccountingProcess::start(SettlementId::generate(), CorrelationId::generate());
    assert!(
        process
            .posting_key()
            .unwrap()
            .as_str()
            .starts_with("sharing-settlement:")
    );
    assert!(
        process
            .reversal_key()
            .unwrap()
            .as_str()
            .starts_with("sharing-settlement-reversal:")
    );
}
